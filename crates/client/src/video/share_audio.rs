//! The sound that goes with a screen share: everything the PC plays except kall itself (so
//! friends don't hear their own voices back), as Opus in voice packets flagged as share audio.
//! Windows uses WASAPI process loopback; other platforms don't have it yet.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::voice::engine::VoiceLink;

/// Dropping this stops sending.
pub struct Running(Arc<AtomicBool>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

pub fn supported() -> bool {
    cfg!(windows)
}

#[cfg(windows)]
pub fn start(link: Arc<VoiceLink>) -> Result<Running> {
    use std::collections::VecDeque;
    use std::sync::mpsc::channel;

    use anyhow::anyhow;
    use kall_proto::voice::{FLAG_SHARE_AUDIO, FRAME_SAMPLES, MAX_OPUS_BYTES, encode_up};
    use opus::{Application, Bitrate, Channels, Encoder};
    use wasapi::{AudioClient, Direction, SampleType, StreamMode, WaveFormat, initialize_mta};

    let stop = Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    let (ok_tx, ok_rx) = channel();
    std::thread::Builder::new().name("kall-share-audio".into()).spawn(move || {
        let run = || -> Result<()> {
            initialize_mta().ok().map_err(|e| anyhow!("COM: {e:?}"))?;
            let fmt = WaveFormat::new(32, 32, SampleType::Float, 48_000, 2, None);
            // Everything except our own process tree.
            let mut client = AudioClient::new_application_loopback_client(std::process::id(), false).map_err(|e| anyhow!("loopback: {e}"))?;
            client
                .initialize_client(&fmt, Direction::Capture, StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 }, None)
                .map_err(|e| anyhow!("loopback: {e}"))?;
            let event = client.set_get_eventhandle().map_err(|e| anyhow!("{e}"))?;
            let capture = client.get_audiocaptureclient().map_err(|e| anyhow!("{e}"))?;
            let mut enc = Encoder::new(48_000, Channels::Stereo, Application::Audio)?;
            enc.set_bitrate(Bitrate::Bits(128_000))?;
            client.start_stream().map_err(|e| anyhow!("{e}"))?;
            ok_tx.send(Ok(())).ok();
            let mut bytes: VecDeque<u8> = VecDeque::new();
            let mut seq = 0u16;
            let frame_bytes = FRAME_SAMPLES * 2 * 4;
            while !st.load(Ordering::Relaxed) {
                if event.wait_for_event(200).is_err() {
                    continue; // silence: the loopback produces nothing while nothing plays
                }
                capture.read_from_device_to_deque(&mut bytes).map_err(|e| anyhow!("{e}"))?;
                while bytes.len() >= frame_bytes {
                    let raw: Vec<u8> = bytes.drain(..frame_bytes).collect();
                    let pcm: Vec<f32> = raw.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
                    let mut out = vec![0u8; MAX_OPUS_BYTES];
                    if let Ok(n) = enc.encode_float(&pcm, &mut out) {
                        seq = seq.wrapping_add(1);
                        link.send(encode_up(seq, FLAG_SHARE_AUDIO, &out[..n]));
                    }
                }
            }
            client.stop_stream().ok();
            Ok(())
        };
        if let Err(e) = run() {
            ok_tx.send(Err(e)).ok();
        }
    })?;
    ok_rx.recv_timeout(std::time::Duration::from_secs(5)).map_err(|_| anyhow::anyhow!("system audio didn't start"))??;
    Ok(Running(stop))
}

#[cfg(not(windows))]
pub fn start(_link: Arc<VoiceLink>) -> Result<Running> {
    anyhow::bail!("Sharing system audio isn't supported on this platform yet.")
}
