//! Receiving video: reassemble fragments, decode, and publish frames as [`rune::Pixels`] the
//! UI draws. After a loss, predicted frames are dropped and a keyframe is requested.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use kall_proto::video::{Assembled, Reassembler, decode_down};
use kall_proto::{C2S, Id, StreamKind};

use super::codec::H264Decoder;

pub type Frames = Arc<Mutex<HashMap<(Id, StreamKind), rune::Pixels>>>;

pub struct Incoming {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The latest picture of each stream being received.
    pub frames: Frames,
    seen: Arc<AtomicU64>,
}

impl Drop for Incoming {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}

struct Stream {
    reasm: Reassembler,
    dec: H264Decoder,
    /// Waiting for a keyframe (just started, or lost something).
    need_key: bool,
    asked: Instant,
    last_seen: Instant,
    spare: Vec<u8>,
    rgba: Vec<u8>,
}

impl Incoming {
    /// `datagrams`: video datagrams from the server. `request`: sends control messages.
    pub fn start(datagrams: Receiver<Vec<u8>>, request: impl Fn(C2S) + Send + 'static, wake: rune::Waker) -> Incoming {
        let stop = Arc::new(AtomicBool::new(false));
        let frames: Frames = Default::default();
        let seen = Arc::new(AtomicU64::new(0));
        let (st, fr, sn) = (stop.clone(), frames.clone(), seen.clone());
        let thread = std::thread::Builder::new()
            .name("kall-video-recv".into())
            .spawn(move || run(&datagrams, &request, &st, &fr, &sn, &wake))
            .expect("video thread");
        Incoming { stop, thread: Some(thread), frames, seen }
    }

    /// Counts decoded frames, so the UI can tell something new arrived.
    pub fn frames_seen(&self) -> u64 {
        self.seen.load(Ordering::Relaxed)
    }
}

fn run(rx: &Receiver<Vec<u8>>, request: &dyn Fn(C2S), stop: &AtomicBool, frames: &Frames, seen: &AtomicU64, wake: &rune::Waker) {
    let mut streams: HashMap<(Id, StreamKind), Stream> = HashMap::new();
    let mut last_wake = Instant::now();
    let mut dirty = false;
    // Every 5 s: frames completed, decoded, dropped waiting for a keyframe, decode time.
    let (mut stat_t, mut n_frames, mut n_dec, mut n_skip, mut n_gap, mut t_dec) = (Instant::now(), 0u32, 0u32, 0u32, 0u32, 0f64);
    while !stop.load(Ordering::Relaxed) {
        let d = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(d) => Some(d),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if let Some(d) = d
            && let Some((user, frag)) = decode_down(&d)
        {
            let key = (user as Id, frag.kind);
            let s = match streams.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(v) => {
                    let Ok(dec) = H264Decoder::new() else { continue };
                    v.insert(Stream {
                        reasm: Reassembler::default(),
                        dec,
                        need_key: true,
                        asked: Instant::now() - Duration::from_secs(5),
                        last_seen: Instant::now(),
                        spare: Vec::new(),
                        rgba: Vec::new(),
                    })
                }
            };
            s.last_seen = Instant::now();
            for out in s.reasm.push(&frag) {
                match out {
                    Assembled::Gap => {
                        n_gap += 1;
                        s.need_key = true;
                    }
                    Assembled::Frame(f) => {
                        n_frames += 1;
                        if s.need_key && !f.keyframe {
                            n_skip += 1;
                            continue;
                        }
                        let t0 = Instant::now();
                        let r = s.dec.decode(&f.data, &mut s.rgba);
                        t_dec += t0.elapsed().as_secs_f64();
                        n_dec += 1;
                        match r {
                            Ok(Some((w, h))) => {
                                s.need_key = false;
                                let px = frames.lock().unwrap().entry(key).or_default().clone();
                                let mut buf = std::mem::take(&mut s.spare);
                                buf.clear();
                                buf.extend_from_slice(&s.rgba);
                                s.spare = px.replace(w, h, buf);
                                seen.fetch_add(1, Ordering::Relaxed);
                                dirty = true;
                            }
                            Ok(None) => {}
                            Err(_) => s.need_key = true,
                        }
                    }
                }
            }
            if s.need_key && s.asked.elapsed() > Duration::from_millis(600) {
                s.asked = Instant::now();
                request(C2S::RequestKeyframe { user: key.0, kind: key.1 });
            }
        }
        if stat_t.elapsed() >= Duration::from_secs(5) && n_frames > 0 {
            let secs = stat_t.elapsed().as_secs_f64();
            eprintln!(
                "kall video recv: {:.1} fps complete, {:.1} fps decoded, {n_skip} skipped, {n_gap} gaps, decode {:.1} ms",
                n_frames as f64 / secs,
                n_dec as f64 / secs,
                t_dec / n_dec.max(1) as f64 * 1e3
            );
            (stat_t, n_frames, n_dec, n_skip, n_gap, t_dec) = (Instant::now(), 0, 0, 0, 0, 0.0);
        }
        // Streams that went quiet (stopped, or we stopped watching) are forgotten.
        let before = streams.len();
        streams.retain(|_, s| s.last_seen.elapsed() < Duration::from_secs(5));
        if streams.len() != before {
            let mut f = frames.lock().unwrap();
            f.retain(|k, _| streams.contains_key(k));
            dirty = true;
        }
        // Redraw at most ~60 times a second.
        if dirty && last_wake.elapsed() > Duration::from_millis(15) {
            dirty = false;
            last_wake = Instant::now();
            wake();
        }
    }
}
