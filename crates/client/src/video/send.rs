//! Sending one video stream: the capture source drops frames into a [`Slot`] (latest wins),
//! this thread scales, encodes and fragments them into datagrams, and keeps a small local
//! preview.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kall_proto::StreamKind;
use kall_proto::video::{FLAG_KEYFRAME, FLAG_PREVIEW, fragment};

use super::Quality;
use super::codec::{H264Encoder, fit_bgra};
use crate::voice::engine::VoiceLink;

/// One captured frame: BGRA rows of `stride` bytes.
pub struct RawFrame {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub bgra: Vec<u8>,
}

/// Where a capture source leaves its newest frame.
#[derive(Default)]
pub struct Slot {
    frame: Mutex<Option<RawFrame>>,
    ready: Condvar,
}

impl Slot {
    pub fn put(&self, f: RawFrame) {
        *self.frame.lock().unwrap() = Some(f);
        self.ready.notify_one();
    }

    fn take(&self, wait: Duration) -> Option<RawFrame> {
        let mut g = self.frame.lock().unwrap();
        if g.is_none() {
            g = self.ready.wait_timeout(g, wait).unwrap().0;
        }
        g.take()
    }
}

pub struct Outgoing {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub slot: Arc<Slot>,
    keyframe: Arc<AtomicBool>,
    /// What we're sending, for our own tile.
    pub preview: rune::Pixels,
    previews: Arc<AtomicU64>,
}

impl Drop for Outgoing {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.slot.ready.notify_one();
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}

impl Outgoing {
    pub fn start(kind: StreamKind, q: Quality, link: Arc<VoiceLink>, wake: rune::Waker) -> Outgoing {
        let stop = Arc::new(AtomicBool::new(false));
        let slot = Arc::new(Slot::default());
        let keyframe = Arc::new(AtomicBool::new(true));
        let preview = rune::Pixels::new();
        let previews = Arc::new(AtomicU64::new(0));
        let (st, sl, kf, pv, pc) = (stop.clone(), slot.clone(), keyframe.clone(), preview.clone(), previews.clone());
        let thread = std::thread::Builder::new()
            .name("kall-video-send".into())
            .spawn(move || run(kind, q, &link, &st, &sl, &kf, &pv, &pc, &wake))
            .expect("video thread");
        Outgoing { stop, thread: Some(thread), slot, keyframe, preview, previews }
    }

    /// Counts preview updates, so the UI can tell something new arrived.
    pub fn previews(&self) -> u64 {
        self.previews.load(Ordering::Relaxed)
    }

    pub fn request_keyframe(&self) {
        self.keyframe.store(true, Ordering::Relaxed);
    }
}

#[allow(clippy::too_many_arguments)]
fn run(kind: StreamKind, q: Quality, link: &VoiceLink, stop: &AtomicBool, slot: &Slot, keyframe: &AtomicBool, preview: &rune::Pixels, previews: &AtomicU64, wake: &rune::Waker) {
    let mut enc = match H264Encoder::new(q.fps, q.bitrate, kind == StreamKind::Screen) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("kall video: {e:#}");
            return;
        }
    };
    let interval = Duration::from_secs_f64(1.0 / q.fps as f64);
    let mut next = Instant::now();
    let mut frame_id = 0u32;
    let mut last_preview = Instant::now() - Duration::from_secs(1);
    // Screen shares also send everyone in the call a small picture about once a second, so
    // their tiles show what's being shared. Each one is a keyframe: it decodes on its own.
    let mut thumbs = (kind == StreamKind::Screen).then(|| H264Encoder::new(1, 300_000, true).ok()).flatten();
    let mut last_thumb = Instant::now() - Duration::from_secs(2);
    let mut thumb_id = 0u32;
    let mut spare = Vec::new();
    // Every 5 s: frames in from capture, frames sent, time spent scaling and encoding.
    let (mut stat_t, mut n_in, mut n_out, mut t_scale, mut t_enc, mut bytes) = (Instant::now(), 0u32, 0u32, 0f64, 0f64, 0usize);
    let (mut t_yuv, mut t_h264, mut t_send) = (0f64, 0f64, 0f64);
    // Diagnostics: KALL_DUMP_FRAMES=dir saves the first 30 frames as raw BGRA.
    let dump = std::env::var_os("KALL_DUMP_FRAMES").map(std::path::PathBuf::from);
    let mut dumped = 0;
    while !stop.load(Ordering::Relaxed) {
        let Some(raw) = slot.take(Duration::from_millis(500)) else { continue };
        // Pace to the chosen frame rate; a faster source just gets frames dropped.
        let now = Instant::now();
        if now < next {
            std::thread::sleep(next - now);
        }
        next = Instant::now().max(next) + interval;
        if raw.width < 2 || raw.height < 2 {
            continue;
        }
        n_in += 1;
        let t0 = Instant::now();
        let (bgra, w, h) = fit_bgra(&raw.bgra, raw.width, raw.height, raw.stride, q.max_w, q.max_h);
        t_scale += t0.elapsed().as_secs_f64();
        if let Some(dir) = &dump
            && dumped < 30
        {
            std::fs::create_dir_all(dir).ok();
            std::fs::write(dir.join(format!("{dumped:02}_{w}x{h}.bgra")), &bgra).ok();
            dumped += 1;
        }
        let t1 = Instant::now();
        if keyframe.swap(false, Ordering::Relaxed) {
            enc.request_keyframe();
        }
        match enc.encode(&bgra, w, h) {
            Ok(Some((bits, key))) => {
                n_out += 1;
                bytes += bits.len();
                frame_id = frame_id.wrapping_add(1);
                let ts = Instant::now();
                for d in fragment(kind, frame_id, if key { FLAG_KEYFRAME } else { 0 }, &bits) {
                    link.send(d);
                }
                t_send += ts.elapsed().as_secs_f64();
            }
            Ok(None) => {}
            Err(e) => eprintln!("kall video: {e:#}"),
        }
        t_enc += t1.elapsed().as_secs_f64();
        t_yuv += enc.timing.0;
        t_h264 += enc.timing.1;
        if stat_t.elapsed() >= Duration::from_secs(5) {
            let secs = stat_t.elapsed().as_secs_f64();
            let n = n_in.max(1) as f64;
            eprintln!(
                "kall video send {kind:?}: {:.1} fps in, {:.1} fps out, {w}x{h}, scale {:.1} ms, encode {:.1} ms (yuv {:.1}, h264 {:.1}, send {:.1}), {:.0} kbit/s",
                n_in as f64 / secs,
                n_out as f64 / secs,
                t_scale / n * 1e3,
                t_enc / n * 1e3,
                t_yuv / n * 1e3,
                t_h264 / n * 1e3,
                t_send / n * 1e3,
                bytes as f64 * 8.0 / secs / 1e3
            );
            (stat_t, n_in, n_out, t_scale, t_enc, bytes) = (Instant::now(), 0, 0, 0.0, 0.0, 0);
            (t_yuv, t_h264, t_send) = (0.0, 0.0, 0.0);
        }
        // Our own preview: a small thumbnail a few times a second is plenty.
        if last_preview.elapsed() > Duration::from_millis(200) {
            last_preview = Instant::now();
            let (small, pw, ph) = fit_bgra(&bgra, w, h, w * 4, 480, 270);
            if let Some(enc) = thumbs.as_mut()
                && last_thumb.elapsed() >= Duration::from_secs(1)
            {
                last_thumb = Instant::now();
                enc.request_keyframe();
                if let Ok(Some((bits, _))) = enc.encode(&small, pw, ph) {
                    thumb_id = thumb_id.wrapping_add(1);
                    for d in fragment(kind, thumb_id, FLAG_KEYFRAME | FLAG_PREVIEW, &bits) {
                        link.send(d);
                    }
                }
            }
            let mut rgba = std::mem::take(&mut spare);
            rgba.clear();
            rgba.extend(small.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]));
            spare = preview.replace(pw as u32, ph as u32, rgba);
            previews.fetch_add(1, Ordering::Relaxed);
            wake();
        }
    }
}
