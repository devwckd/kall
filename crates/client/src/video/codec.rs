//! H.264 for screen share and camera, behind two small types. With the `ffmpeg` feature the
//! encoder uses the GPU (NVENC, AMF, Quick Sync, VideoToolbox: see [`super::ff`]) and the
//! decoder FFmpeg's; otherwise, or when no hardware encoder opens, OpenH264 (software, all
//! platforms). `KALL_SOFTWARE_VIDEO=1` forces OpenH264. Both make the same Annex-B stream.

use anyhow::{Result, anyhow};
use openh264::OpenH264API;
use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::{BgraSliceU8, YUVBuffer, YUVSource};

pub struct H264Encoder {
    enc: Option<Encoder>,
    #[cfg(feature = "ffmpeg")]
    hw: Option<super::ff::HwEncoder>,
    /// Why the hardware encoder isn't used (logged once).
    #[cfg(feature = "ffmpeg")]
    hw_error: Option<String>,
    width: usize,
    height: usize,
    fps: u32,
    bitrate: u32,
    screen: bool,
    force_key: bool,
    /// Seconds the last frame spent in colour conversion and in the encoder.
    pub timing: (f64, f64),
}

fn make(fps: u32, bitrate: u32, screen: bool) -> Result<Encoder> {
    // Measured on real 1080p desktop frames (`bench_encoders`): OpenH264's screen-content mode
    // takes ~40 ms a frame whatever the thread count, while camera mode with size-limited
    // slices (which is what lets its threads work in parallel) takes ~8 ms. So screens use
    // camera mode too; at our bitrates text stays sharp.
    let cfg = EncoderConfig::new()
        .bitrate(BitRate::from_bps(bitrate))
        .max_frame_rate(FrameRate::from_hz(fps as f32))
        .rate_control_mode(RateControlMode::Bitrate)
        .usage_type(UsageType::CameraVideoRealTime)
        .complexity(Complexity::Low)
        .adaptive_quantization(!screen)
        .background_detection(!screen)
        // A keyframe every 4 s bounds how long a viewer who lost one waits.
        .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 4))
        .num_threads(4)
        .max_slice_len(4000);
    Encoder::with_api_config(OpenH264API::from_source(), cfg).map_err(|e| anyhow!("H.264 encoder: {e}"))
}

/// Hardware video is off (`KALL_SOFTWARE_VIDEO=1`).
fn software_only() -> bool {
    std::env::var("KALL_SOFTWARE_VIDEO").is_ok_and(|v| v == "1")
}

impl H264Encoder {
    pub fn new(fps: u32, bitrate: u32, screen: bool) -> Result<H264Encoder> {
        Ok(H264Encoder {
            enc: None,
            #[cfg(feature = "ffmpeg")]
            hw: None,
            #[cfg(feature = "ffmpeg")]
            hw_error: None,
            width: 0,
            height: 0,
            fps,
            bitrate,
            screen,
            force_key: true,
            timing: (0.0, 0.0),
        })
    }

    /// What's encoding: "NVENC", "AMF", "Quick Sync", "VideoToolbox" or "OpenH264".
    pub fn backend(&self) -> &'static str {
        #[cfg(feature = "ffmpeg")]
        if let Some(h) = &self.hw {
            return h.name;
        }
        "OpenH264"
    }

    /// Open an encoder for a new frame size: hardware first, OpenH264 otherwise.
    fn reopen(&mut self, width: usize, height: usize) -> Result<()> {
        self.width = width;
        self.height = height;
        self.force_key = true;
        #[cfg(feature = "ffmpeg")]
        {
            self.hw = None;
            if !software_only() && self.hw_error.is_none() {
                match super::ff::HwEncoder::open(width, height, self.fps, self.bitrate) {
                    Ok(h) => {
                        eprintln!("kall video: encoding with {}", h.name);
                        self.hw = Some(h);
                        self.enc = None;
                        return Ok(());
                    }
                    Err(e) => {
                        eprintln!("kall video: {e:#}; using OpenH264");
                        self.hw_error = Some(e.to_string());
                    }
                }
            }
        }
        let _ = software_only();
        self.enc = Some(make(self.fps, self.bitrate, self.screen)?);
        Ok(())
    }

    pub fn request_keyframe(&mut self) {
        self.force_key = true;
    }

    /// Encode one packed BGRA frame (even width and height). Returns the Annex-B bitstream
    /// and whether it's a keyframe; `None` when the rate controller skipped the frame.
    pub fn encode(&mut self, bgra: &[u8], width: usize, height: usize) -> Result<Option<(Vec<u8>, bool)>> {
        if (width, height) != (self.width, self.height) {
            // A new size (window resized): start a fresh stream.
            self.reopen(width, height)?;
        }
        #[cfg(feature = "ffmpeg")]
        if let Some(hw) = &mut self.hw {
            let key = std::mem::take(&mut self.force_key);
            match hw.encode(bgra, key) {
                Ok((timing, out)) => {
                    self.timing = timing;
                    return Ok(out.filter(|(d, _)| !d.is_empty()));
                }
                Err(e) => {
                    // A GPU encoder that fails mid-stream (driver reset, session limit): fall
                    // back to OpenH264 for the rest of this share.
                    eprintln!("kall video: {} failed ({e:#}); switching to OpenH264", hw.name);
                    self.hw = None;
                    self.hw_error = Some(e.to_string());
                    self.enc = Some(make(self.fps, self.bitrate, self.screen)?);
                    self.force_key = true;
                }
            }
        }
        let Some(enc) = self.enc.as_mut() else { return Ok(None) };
        let t0 = std::time::Instant::now();
        let yuv = YUVBuffer::from_bgra8_source(BgraSliceU8::new(bgra, (width, height)));
        let t1 = std::time::Instant::now();
        if std::mem::take(&mut self.force_key) {
            enc.force_intra_frame();
        }
        let bits = enc.encode(&yuv).map_err(|e| anyhow!("encode: {e}"))?;
        self.timing = ((t1 - t0).as_secs_f64(), t1.elapsed().as_secs_f64());
        let key = matches!(bits.frame_type(), FrameType::IDR | FrameType::I);
        if matches!(bits.frame_type(), FrameType::Skip | FrameType::Invalid) {
            return Ok(None);
        }
        let data = bits.to_vec();
        Ok((!data.is_empty()).then_some((data, key)))
    }
}

pub struct H264Decoder {
    dec: Option<Decoder>,
    #[cfg(feature = "ffmpeg")]
    ff: Option<super::ff::FfDecoder>,
}

impl H264Decoder {
    pub fn new() -> Result<H264Decoder> {
        #[cfg(feature = "ffmpeg")]
        if !software_only()
            && let Ok(ff) = super::ff::FfDecoder::new()
        {
            return Ok(H264Decoder { dec: None, ff: Some(ff) });
        }
        Ok(H264Decoder {
            dec: Some(Decoder::new().map_err(|e| anyhow!("H.264 decoder: {e}"))?),
            #[cfg(feature = "ffmpeg")]
            ff: None,
        })
    }

    /// Decode one frame into `rgba` (resized as needed); returns its size.
    pub fn decode(&mut self, data: &[u8], rgba: &mut Vec<u8>) -> Result<Option<(u32, u32)>> {
        #[cfg(feature = "ffmpeg")]
        if let Some(ff) = &mut self.ff {
            return ff.decode(data, rgba);
        }
        let Some(dec) = self.dec.as_mut() else { return Ok(None) };
        let Some(img) = dec.decode(data).map_err(|e| anyhow!("decode: {e}"))? else { return Ok(None) };
        let (w, h) = img.dimensions();
        rgba.resize(w * h * 4, 0);
        img.write_rgba8(rgba);
        Ok(Some((w as u32, h as u32)))
    }
}

/// Fit `src` (BGRA rows of `stride` bytes) inside `max_w`×`max_h`, keeping the aspect ratio,
/// with even dimensions (H.264 needs them). Bilinear when scaling down; a plain copy otherwise.
pub fn fit_bgra(src: &[u8], sw: usize, sh: usize, stride: usize, max_w: usize, max_h: usize) -> (Vec<u8>, usize, usize) {
    let scale = (max_w as f64 / sw as f64).min(max_h as f64 / sh as f64).min(1.0);
    let dw = (((sw as f64 * scale) as usize) & !1).max(2);
    let dh = (((sh as f64 * scale) as usize) & !1).max(2);
    let mut out = vec![0u8; dw * dh * 4];
    if dw == sw && dh == sh {
        for y in 0..dh {
            out[y * dw * 4..(y + 1) * dw * 4].copy_from_slice(&src[y * stride..y * stride + dw * 4]);
        }
        return (out, dw, dh);
    }
    if dw == (sw & !1) && dh == (sh & !1) {
        // Just trimming an odd column/row.
        for y in 0..dh {
            out[y * dw * 4..(y + 1) * dw * 4].copy_from_slice(&src[y * stride..y * stride + dw * 4]);
        }
        return (out, dw, dh);
    }
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    for y in 0..dh {
        let syf = ((y as f32 + 0.5) * fy - 0.5).max(0.0);
        let y0 = (syf as usize).min(sh - 1);
        let y1 = (y0 + 1).min(sh - 1);
        let ty = syf - y0 as f32;
        for x in 0..dw {
            let sxf = ((x as f32 + 0.5) * fx - 0.5).max(0.0);
            let x0 = (sxf as usize).min(sw - 1);
            let x1 = (x0 + 1).min(sw - 1);
            let tx = sxf - x0 as f32;
            let p = |xx: usize, yy: usize, c: usize| src[yy * stride + xx * 4 + c] as f32;
            let o = (y * dw + x) * 4;
            for c in 0..4 {
                let top = p(x0, y0, c) + (p(x1, y0, c) - p(x0, y0, c)) * tx;
                let bot = p(x0, y1, c) + (p(x1, y1, c) - p(x0, y1, c)) * tx;
                out[o + c] = (top + (bot - top) * ty + 0.5) as u8;
            }
        }
    }
    (out, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: usize, h: usize, t: usize) -> Vec<u8> {
        let mut v = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let o = (y * w + x) * 4;
                v[o] = ((x + t) % 256) as u8;
                v[o + 1] = ((y * 2) % 256) as u8;
                v[o + 2] = if (x / 40 + y / 40) % 2 == 0 { 220 } else { 30 };
                v[o + 3] = 255;
            }
        }
        v
    }

    #[test]
    fn encodes_and_decodes_with_keyframes_on_request() {
        let (w, h) = (640, 360);
        let mut enc = H264Encoder::new(30, 1_500_000, true).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        let mut rgba = Vec::new();
        let mut keys = Vec::new();
        for t in 0..20 {
            if t == 10 {
                enc.request_keyframe();
            }
            let Some((bits, key)) = enc.encode(&pattern(w, h, t * 3), w, h).unwrap() else { continue };
            keys.push((t, key));
            if let Some((dw, dh)) = dec.decode(&bits, &mut rgba).unwrap() {
                assert_eq!((dw, dh), (w as u32, h as u32));
            }
        }
        println!("encoder: {} keys {:?}", enc.backend(), keys);
        // GPU encoders may hand a frame back one call late.
        assert!(keys.first().is_some_and(|&(t, k)| t <= 1 && k), "starts with a keyframe");
        assert!(keys.iter().any(|&(t, k)| (10..=11).contains(&t) && k), "keyframe on request");
        assert!(keys.iter().filter(|(_, k)| !k).count() > 10, "mostly predicted frames");
        // The checkerboard survived: red channel (index 0 in RGBA) high in the first square.
        assert!(rgba[0] > 150, "decoded red {}", rgba[0]);
    }

    #[test]
    fn fits_inside_the_box_with_even_sizes() {
        let src = pattern(2561, 1441, 0);
        let (out, w, h) = fit_bgra(&src, 2561, 1441, 2561 * 4, 1920, 1080);
        assert_eq!((w, h), (1918, 1080));
        assert_eq!(out.len(), w * h * 4);
        let (_, w, h) = fit_bgra(&src, 2561, 1441, 2561 * 4, 4000, 4000);
        assert_eq!((w, h), (2560, 1440));
    }
}

#[cfg(test)]
mod bench {
    use std::time::Instant;

    use openh264::OpenH264API;
    use openh264::encoder::{BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode, UsageType};
    use openh264::formats::{BgraSliceU8, YUVBuffer};

    /// `cargo test -p kall --release bench_encoders -- --ignored --nocapture`
    #[test]
    #[ignore = "benchmark"]
    fn bench_encoders() {
        let (w, h) = (1920usize, 1080usize);
        let frames: Vec<Vec<u8>> = (0..30)
            .map(|t| {
                let mut v = vec![0u8; w * h * 4];
                for y in 0..h {
                    for x in 0..w {
                        let o = (y * w + x) * 4;
                        // Text-like stripes that scroll, like a code editor or a web page.
                        v[o] = if (x / 8 + (y + t * 6) / 14) % 3 == 0 { 230 } else { 30 };
                        v[o + 1] = ((x * y + t * 50) % 255) as u8;
                        v[o + 2] = ((y + t * 6) % 256) as u8;
                        v[o + 3] = 255;
                    }
                }
                v
            })
            .collect();
        // A desktop: detailed static texture (text-like noise), one small region changing.
        let mut seed = 7u32;
        let mut base = vec![0u8; w * h * 4];
        for px in base.chunks_exact_mut(4) {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = if (seed >> 24) % 7 == 0 { 20 } else { 235 };
            px.copy_from_slice(&[v, v, v, 255]);
        }
        let desktop: Vec<Vec<u8>> = (0..30)
            .map(|t| {
                let mut f = base.clone();
                for y in 400..520 {
                    for x in (600 + t * 10)..(760 + t * 10) {
                        let o = (y * w + x) * 4;
                        f[o..o + 4].copy_from_slice(&[40, 120, 220, 255]);
                    }
                }
                f
            })
            .collect();
        let mode = std::env::var("BENCH_CONTENT").unwrap_or_default();
        // BENCH_FRAMES=dir: real frames saved by KALL_DUMP_FRAMES (1920x1080 only).
        let real: Option<Vec<Vec<u8>>> = std::env::var_os("BENCH_FRAMES").map(|d| {
            let mut files: Vec<_> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.path()).filter(|p| p.to_string_lossy().ends_with("1920x1080.bgra")).collect();
            files.sort();
            files.iter().map(|p| std::fs::read(p).unwrap()).collect()
        });
        let frames = match (real, mode.as_str()) {
            (Some(r), _) => {
                println!("content: {} real frames", r.len());
                r
            }
            (None, "desktop") => desktop,
            _ => frames,
        };
        let n = frames.len();
        let t = Instant::now();
        let yuvs: Vec<YUVBuffer> = frames.iter().map(|f| YUVBuffer::from_bgra8_source(BgraSliceU8::new(f, (w, h)))).collect();
        println!("bgra->yuv: {:.1} ms/frame", t.elapsed().as_secs_f64() * 1e3 / n as f64);
        for (usage, name) in [(UsageType::ScreenContentRealTime, "screen"), (UsageType::CameraVideoRealTime, "camera")] {
            for threads in [1u16, 4, 8] {
                for slice in [0u32, 1100, 4000] {
                    let cx = Complexity::Low;
                    let mut cfg = EncoderConfig::new()
                        .bitrate(BitRate::from_bps(4_500_000))
                        .max_frame_rate(FrameRate::from_hz(30.0))
                        .rate_control_mode(RateControlMode::Bitrate)
                        .usage_type(usage)
                        .complexity(cx)
                        .adaptive_quantization(false)
                        .background_detection(false)
                        .intra_frame_period(IntraFramePeriod::from_num_frames(120))
                        .num_threads(threads);
                    if slice > 0 {
                        cfg = cfg.max_slice_len(slice);
                    }
                    let mut enc = Encoder::with_api_config(OpenH264API::from_source(), cfg).unwrap();
                    enc.encode(&yuvs[0]).unwrap();
                    let t = Instant::now();
                    let mut bytes = 0;
                    for y in &yuvs[1..] {
                        bytes += enc.encode(y).unwrap().to_vec().len();
                    }
                    println!("{name:6} threads {threads} slice {slice}: {:.1} ms/frame, {} KB/frame", t.elapsed().as_secs_f64() * 1e3 / (n - 1) as f64, bytes / (n - 1) / 1024);
                }
            }
        }
    }
}
