//! H.264 through FFmpeg (the `ffmpeg` feature): hardware encoders (NVENC, AMF, Quick Sync,
//! VideoToolbox), tried in order until one opens, and FFmpeg's fast H.264 decoder. The stream
//! on the wire is the same Annex-B H.264 the OpenH264 path makes, so either end can use either.

use anyhow::{Result, anyhow, bail};
use ffmpeg_next as ff;
use ff::format::Pixel;
use ff::software::scaling;

/// Hardware H.264 encoders to try, best first, with their low-latency settings.
const ENCODERS: &[(&str, &str, &[(&str, &str)])] = &[
    ("h264_nvenc", "NVENC", &[("preset", "p2"), ("tune", "ll"), ("rc", "cbr"), ("zerolatency", "1"), ("delay", "0"), ("forced-idr", "1")]),
    ("h264_amf", "AMF", &[("usage", "ultralowlatency"), ("latency", "1"), ("async_depth", "1"), ("rc", "cbr"), ("quality", "speed"), ("forced_idr", "1"), ("header_spacing", "1")]),
    ("h264_qsv", "Quick Sync", &[("preset", "veryfast"), ("async_depth", "1"), ("look_ahead", "0")]),
    ("h264_videotoolbox", "VideoToolbox", &[("realtime", "1"), ("allow_sw", "0")]),
    // AMD and Intel on Linux: frames are uploaded to GPU surfaces first (kall_hwaccel).
    ("h264_vaapi", "VAAPI", &[("rc_mode", "CBR")]),
];

pub fn init() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let ok = ff::init().is_ok();
        ff::util::log::set_level(ff::util::log::Level::Error);
        ok
    })
}

/// A hardware encoder, opened for one frame size.
pub struct HwEncoder {
    enc: ff::encoder::video::Encoder,
    /// VAAPI's GPU surfaces (only for h264_vaapi).
    vaapi: Option<kall_hwaccel::VaapiFrames>,
    scaler: scaling::Context,
    bgra: ff::frame::Video,
    nv12: ff::frame::Video,
    pts: i64,
    /// The last SPS and PPS seen, put back in front of keyframes that lack them.
    headers: Vec<u8>,
    pub name: &'static str,
    pub width: usize,
    pub height: usize,
}

impl HwEncoder {
    /// The first hardware encoder that opens at this size, or an error naming what was tried.
    pub fn open(width: usize, height: usize, fps: u32, bitrate: u32) -> Result<HwEncoder> {
        if !init() {
            bail!("FFmpeg didn't start");
        }
        let mut tried = Vec::new();
        for &(codec_name, label, opts) in ENCODERS {
            let Some(codec) = ff::encoder::find_by_name(codec_name) else { continue };
            match Self::open_one(codec, label, opts, width, height, fps, bitrate) {
                Ok(e) => {
                    if !tried.is_empty() {
                        eprintln!("kall video: skipped {}", tried.join("; "));
                    }
                    return Ok(e);
                }
                Err(e) => tried.push(format!("{label}: {e}")),
            }
        }
        bail!("no hardware H.264 encoder ({})", if tried.is_empty() { "none built in".into() } else { tried.join("; ") })
    }

    fn open_one(codec: ff::Codec, label: &'static str, opts: &[(&str, &str)], width: usize, height: usize, fps: u32, bitrate: u32) -> Result<HwEncoder> {
        let mut ctx = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
        ctx.set_width(width as u32);
        ctx.set_height(height as u32);
        // VAAPI encodes from GPU surfaces; the others take NV12 frames directly.
        let vaapi = if label == "VAAPI" {
            let frames = kall_hwaccel::VaapiFrames::new(width as u32, height as u32).ok_or_else(|| anyhow!("no VAAPI device"))?;
            ctx.set_format(Pixel::VAAPI);
            frames.attach_encoder(&mut ctx);
            Some(frames)
        } else {
            ctx.set_format(Pixel::NV12);
            None
        };
        ctx.set_time_base(ff::Rational::new(1, fps as i32));
        ctx.set_frame_rate(Some(ff::Rational::new(fps as i32, 1)));
        ctx.set_bit_rate(bitrate as usize);
        ctx.set_max_bit_rate(bitrate as usize);
        // A keyframe every 4 s bounds how long a viewer who lost one waits (and on request).
        ctx.set_gop(fps * 4);
        ctx.set_max_b_frames(0);
        let mut dict = ff::Dictionary::new();
        for (k, v) in opts {
            dict.set(k, v);
        }
        let enc = ctx.open_as_with(codec, dict)?;
        let scaler = scaling::Context::get(Pixel::BGRA, width as u32, height as u32, Pixel::NV12, width as u32, height as u32, scaling::Flags::FAST_BILINEAR)?;
        Ok(HwEncoder {
            enc,
            vaapi,
            scaler,
            bgra: ff::frame::Video::new(Pixel::BGRA, width as u32, height as u32),
            nv12: ff::frame::Video::new(Pixel::NV12, width as u32, height as u32),
            pts: 0,
            headers: Vec::new(),
            name: label,
            width,
            height,
        })
    }

    /// Encode one packed BGRA frame. Returns (convert seconds, encode seconds) and the Annex-B
    /// output with whether it's a keyframe (None while the encoder buffers).
    pub fn encode(&mut self, bgra: &[u8], key: bool) -> Result<((f64, f64), Option<(Vec<u8>, bool)>)> {
        let t0 = std::time::Instant::now();
        let stride = self.bgra.stride(0);
        let row = self.width * 4;
        let dst = self.bgra.data_mut(0);
        for y in 0..self.height {
            dst[y * stride..y * stride + row].copy_from_slice(&bgra[y * row..(y + 1) * row]);
        }
        self.scaler.run(&self.bgra, &mut self.nv12)?;
        let t1 = std::time::Instant::now();
        self.nv12.set_pts(Some(self.pts));
        self.pts += 1;
        self.nv12.set_kind(if key { ff::picture::Type::I } else { ff::picture::Type::None });
        match &self.vaapi {
            Some(frames) => {
                let hw = frames.upload(&self.nv12)?;
                self.enc.send_frame(&hw)?
            }
            None => self.enc.send_frame(&self.nv12)?,
        }
        // Whatever the encoder has ready. Some GPU encoders (AMF) hand a frame back on the
        // next call: one frame of delay, which waiting doesn't remove.
        let mut out: Option<(Vec<u8>, bool)> = None;
        let mut pkt = ff::Packet::empty();
        while self.enc.receive_packet(&mut pkt).is_ok() {
            let data = pkt.data().unwrap_or_default();
            let is_key = pkt.is_key() || has_nal(data, 5);
            let entry = out.get_or_insert_with(|| (Vec::new(), false));
            entry.0.extend_from_slice(data);
            entry.1 |= is_key;
        }
        if let Some((data, k)) = &mut out {
            remember_headers(data, &mut self.headers);
            if *k && !has_nal(data, 7) && !self.headers.is_empty() {
                let mut with = self.headers.clone();
                with.extend_from_slice(data);
                *data = with;
            }
        }
        Ok((((t1 - t0).as_secs_f64(), t1.elapsed().as_secs_f64()), out))
    }
}

/// Annex-B NAL units: (type, bytes including the start code).
fn nals(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let s = if i > 0 && data[i - 1] == 0 { i - 1 } else { i };
            starts.push((s, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .filter(|(_, (_, h))| *h < data.len())
        .map(|(n, &(s, h))| {
            let end = starts.get(n + 1).map_or(data.len(), |&(next, _)| next);
            (data[h] & 0x1f, &data[s..end])
        })
        .collect()
}

fn has_nal(data: &[u8], kind: u8) -> bool {
    nals(data).iter().any(|(t, _)| *t == kind)
}

/// Keep the latest SPS (7) and PPS (8).
fn remember_headers(data: &[u8], headers: &mut Vec<u8>) {
    let units = nals(data);
    let sps = units.iter().find(|(t, _)| *t == 7);
    let pps = units.iter().find(|(t, _)| *t == 8);
    if let (Some((_, s)), Some((_, p))) = (sps, pps) {
        headers.clear();
        headers.extend_from_slice(s);
        headers.extend_from_slice(p);
    }
}

/// FFmpeg's H.264 decoder, on the GPU where the platform allows (D3D11VA, VideoToolbox,
/// VAAPI) and on the CPU otherwise, output as RGBA.
pub struct FfDecoder {
    dec: ff::decoder::Video,
    /// The GPU API decoding, if any ("D3D11VA", …).
    pub gpu: Option<&'static str>,
    scaler: Option<(scaling::Context, (u32, u32, Pixel))>,
    frame: ff::frame::Video,
    rgba: ff::frame::Video,
}

impl FfDecoder {
    /// `gpu`: try decoding on the GPU first.
    pub fn new(gpu: bool) -> Result<FfDecoder> {
        if !init() {
            bail!("FFmpeg didn't start");
        }
        let codec = ff::decoder::find(ff::codec::Id::H264).ok_or_else(|| anyhow!("no H.264 decoder"))?;
        let mut ctx = ff::codec::context::Context::new_with_codec(codec);
        ctx.set_flags(ff::codec::Flags::LOW_DELAY);
        let gpu = if gpu { kall_hwaccel::attach_decoder(&mut ctx) } else { None };
        if gpu.is_none() {
            // Slice threads add no latency (frame threads would hold frames back).
            ctx.set_threading(ff::threading::Config { kind: ff::threading::Type::Slice, count: 4 });
        }
        let dec = ctx.decoder().video()?;
        Ok(FfDecoder { dec, gpu, scaler: None, frame: ff::frame::Video::empty(), rgba: ff::frame::Video::empty() })
    }

    /// Decode one frame into `rgba` (packed, resized as needed); returns its size.
    pub fn decode(&mut self, data: &[u8], out: &mut Vec<u8>) -> Result<Option<(u32, u32)>> {
        self.dec.send_packet(&ff::Packet::copy(data))?;
        let mut got = None;
        while self.dec.receive_frame(&mut self.frame).is_ok() {
            // A GPU frame is copied back first (to NV12, usually).
            if let Some(sw) = kall_hwaccel::download(&self.frame)? {
                self.frame = sw;
            }
            let (w, h, fmt) = (self.frame.width(), self.frame.height(), self.frame.format());
            if self.scaler.as_ref().is_none_or(|(_, k)| *k != (w, h, fmt)) {
                let sc = scaling::Context::get(fmt, w, h, Pixel::RGBA, w, h, scaling::Flags::FAST_BILINEAR)?;
                self.scaler = Some((sc, (w, h, fmt)));
                self.rgba = ff::frame::Video::new(Pixel::RGBA, w, h);
            }
            let (sc, _) = self.scaler.as_mut().unwrap();
            sc.run(&self.frame, &mut self.rgba)?;
            let stride = self.rgba.stride(0);
            let row = w as usize * 4;
            out.resize(row * h as usize, 0);
            let src = self.rgba.data(0);
            for y in 0..h as usize {
                out[y * row..(y + 1) * row].copy_from_slice(&src[y * stride..y * stride + row]);
            }
            got = Some((w, h));
        }
        Ok(got)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_nal_units() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 1, 0x65, 9, 9];
        let types: Vec<u8> = super::nals(&data).iter().map(|(t, _)| *t).collect();
        assert_eq!(types, vec![7, 8, 5]);
        let mut h = Vec::new();
        super::remember_headers(&data, &mut h);
        assert_eq!(h, vec![0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3]);
    }
}

#[cfg(test)]
mod bench {
    /// `cargo test -p kall --release --features ffmpeg bench_hw -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn bench_hw() {
        let (w, h) = (1920usize, 1080usize);
        let frames: Vec<Vec<u8>> = (0..60)
            .map(|t| {
                let mut v = vec![0u8; w * h * 4];
                for y in 0..h {
                    for x in 0..w {
                        let o = (y * w + x) * 4;
                        v[o] = if (x / 8 + (y + t * 6) / 14) % 3 == 0 { 230 } else { 30 };
                        v[o + 1] = ((x * y + t * 50) % 255) as u8;
                        v[o + 2] = ((y + t * 6) % 256) as u8;
                        v[o + 3] = 255;
                    }
                }
                v
            })
            .collect();
        let mut enc = super::HwEncoder::open(w, h, 60, 8_000_000).unwrap();
        // BENCH_GPU_DECODE=1 decodes on the GPU (D3D11VA, VideoToolbox, VAAPI).
        let mut dec = super::FfDecoder::new(std::env::var_os("BENCH_GPU_DECODE").is_some()).unwrap();
        let (mut conv, mut code, mut decode, mut bytes, mut n) = (0.0, 0.0, 0.0, 0, 0);
        let mut rgba = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            let ((c, e), out) = enc.encode(f, i == 0).unwrap();
            if i >= 5 {
                (conv, code) = (conv + c, code + e);
                n += 1;
            }
            if let Some((d, _)) = out {
                bytes += d.len();
                let t = std::time::Instant::now();
                dec.decode(&d, &mut rgba).unwrap();
                decode += t.elapsed().as_secs_f64();
            }
        }
        println!("{} 1080p60 (decode on {}): convert {:.2} ms, encode {:.2} ms, decode {:.2} ms per frame, {} KB/frame", enc.name, dec.gpu.unwrap_or("CPU"), conv / n as f64 * 1e3, code / n as f64 * 1e3, decode / frames.len() as f64 * 1e3, bytes / frames.len() / 1024);
    }
}
