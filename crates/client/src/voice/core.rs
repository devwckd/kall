//! The device-free heart of voice: microphone processing → Opus → packets, and packets →
//! jitter buffers → Opus → mix. Everything runs in 10 ms steps of 48 kHz mono `f32` in
//! [-1, 1]; the engine feeds it from real devices, tests feed it synthetic audio.
//!
//! Capture chain: high-pass → echo canceller (AEC3, referenced to what we play) → gain control
//! → RNNoise (noise suppression, and the voice-activity probability that gates sending).

use std::collections::{HashMap, VecDeque};

use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear::{self, LinearPipeline};
use kall_proto::voice::{self, FRAME_SAMPLES, MAX_OPUS_BYTES, SAMPLE_RATE};
use nnnoiseless::DenoiseState;
use opus::{Application, Bitrate, Channels, Decoder, Encoder};

use super::jitter::{Jitter, Play};
use super::sounds::{self, Cue};

pub const STEP: usize = 480;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    /// Send while RNNoise hears a voice (with a short hangover).
    VoiceActivity,
    /// Send while the push-to-talk key is held.
    PushToTalk,
    /// Always send (tests, or an open mic).
    Always,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub muted: bool,
    pub deafened: bool,
    pub mode: Mode,
    /// Voice probability (0–1) above which we start sending.
    pub vad_threshold: f32,
    pub noise_suppression: bool,
    pub echo_cancellation: bool,
    pub bitrate: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { muted: false, deafened: false, mode: Mode::VoiceActivity, vad_threshold: 0.6, noise_suppression: true, echo_cancellation: true, bitrate: 64_000 }
    }
}

struct Peer {
    jitter: Jitter,
    decoder: Decoder,
    pcm: Vec<f32>,
    /// Ticks (10 ms) since we last played a real packet from them.
    quiet_for: u32,
}

pub struct Core {
    apm: LinearPipeline,
    denoise: Box<DenoiseState<'static>>,
    encoder: Encoder,
    bitrate: u32,
    pending: Vec<f32>,
    /// The last [`PREROLL`] samples of processed mic audio, sent when the voice gate opens.
    history: VecDeque<f32>,
    seq: u16,
    hangover: u32,
    steps: u32,
    /// Keyed by (user, is share audio): a screen share's sound mixes alongside the mic.
    peers: HashMap<(u32, bool), Peer>,
    cues: Vec<f32>,
    scratch: Vec<f32>,
    /// Peak level of the processed microphone signal over the last step (0–1).
    pub level: f32,
    pub transmitting: bool,
    pub vad: f32,
}

/// 400 ms of hangover after the voice detector lets go, so word endings aren't clipped.
const HANGOVER_STEPS: u32 = 40;
/// The voice detector needs a few frames to be sure; 60 ms of audio from before it opened
/// goes out first so the start of a word isn't lost.
const PREROLL: usize = 3 * FRAME_SAMPLES;
/// RNNoise needs a moment to learn the room's noise floor before its voice probability
/// means anything; until then voice activity doesn't open the gate.
const WARMUP_STEPS: u32 = 60;

impl Core {
    pub fn new(bitrate: u32) -> anyhow::Result<Core> {
        let format = AudioFormat::ten_ms(SAMPLE_RATE, 1);
        let apm = linear::builder(format, format)
            .enable_high_pass_filter(true)
            .enable_noise_suppression(false)
            .enable_gain_controller2(true)
            .initial_delay_ms(60)
            .build()
            .map_err(|e| anyhow::anyhow!("audio processing: {e}"))?;
        let mut encoder = Encoder::new(SAMPLE_RATE, Channels::Mono, Application::Voip)?;
        encoder.set_bitrate(Bitrate::Bits(bitrate as i32))?;
        encoder.set_inband_fec(true)?;
        encoder.set_packet_loss_perc(10)?;
        Ok(Core {
            apm,
            denoise: DenoiseState::new(),
            encoder,
            bitrate,
            pending: Vec::with_capacity(PREROLL + FRAME_SAMPLES),
            history: VecDeque::with_capacity(PREROLL + STEP),
            seq: 0,
            hangover: 0,
            steps: 0,
            peers: HashMap::new(),
            cues: Vec::new(),
            scratch: vec![0.0; STEP],
            level: 0.0,
            transmitting: false,
            vad: 0.0,
        })
    }

    pub fn cue(&mut self, cue: Cue, gain: f32) {
        let s = sounds::render(cue);
        if self.cues.len() < s.len() {
            self.cues.resize(s.len(), 0.0);
        }
        for (d, x) in self.cues.iter_mut().zip(s) {
            *d += x * gain;
        }
    }

    /// Process 10 ms of microphone audio. Returns the datagrams to send: one per finished
    /// 20 ms frame while transmitting, plus the pre-roll when the voice gate just opened.
    pub fn capture(&mut self, mic: &[f32], s: &Settings, ptt_down: bool) -> Vec<Vec<u8>> {
        debug_assert_eq!(mic.len(), STEP);
        if s.bitrate != self.bitrate {
            self.encoder.set_bitrate(Bitrate::Bits(s.bitrate as i32)).ok();
            self.bitrate = s.bitrate;
        }
        let mut cleaned = vec![0.0f32; STEP];
        if !matches!(self.apm.process_capture_frame(mic, &mut cleaned), Ok(true)) {
            cleaned.copy_from_slice(mic);
        }
        // RNNoise works on i16-scaled floats.
        let scaled: Vec<f32> = cleaned.iter().map(|x| x * 32768.0).collect();
        self.vad = self.denoise.process_frame(&mut self.scratch, &scaled);
        let out: Vec<f32> = if s.noise_suppression { self.scratch.iter().map(|x| x / 32768.0).collect() } else { cleaned };
        self.level = out.iter().fold(0.0f32, |m, x| m.max(x.abs())).min(1.0);

        let open = match s.mode {
            Mode::Always => true,
            Mode::PushToTalk => ptt_down,
            Mode::VoiceActivity => {
                self.steps = self.steps.saturating_add(1);
                if self.vad >= s.vad_threshold && self.steps > WARMUP_STEPS {
                    self.hangover = HANGOVER_STEPS;
                }
                if self.hangover > 0 {
                    self.hangover -= 1;
                    true
                } else {
                    false
                }
            }
        };
        let was = self.transmitting;
        self.transmitting = open && !s.muted;
        if !self.transmitting {
            self.pending.clear();
        } else {
            if !was && s.mode == Mode::VoiceActivity {
                self.pending.clear();
                self.pending.extend(self.history.iter().copied());
            }
            self.pending.extend_from_slice(&out);
        }
        self.history.extend(out.iter().copied());
        let excess = self.history.len().saturating_sub(PREROLL);
        self.history.drain(..excess);

        let mut packets = Vec::new();
        let whole = self.pending.len() / FRAME_SAMPLES * FRAME_SAMPLES;
        for frame in self.pending[..whole].chunks_exact(FRAME_SAMPLES) {
            let mut packet = vec![0u8; MAX_OPUS_BYTES];
            if let Ok(n) = self.encoder.encode_float(frame, &mut packet) {
                packet.truncate(n);
                self.seq = self.seq.wrapping_add(1);
                packets.push(voice::encode_up(self.seq, 1, &packet));
            }
        }
        self.pending.drain(..whole);
        packets
    }

    /// A datagram from the server.
    pub fn receive(&mut self, datagram: &[u8]) {
        let Some(p) = voice::decode_down(datagram) else { return };
        let share = p.flags & voice::FLAG_SHARE_AUDIO != 0;
        let peer = match self.peers.entry((p.user, share)) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => {
                let Ok(decoder) = Decoder::new(SAMPLE_RATE, Channels::Mono) else { return };
                v.insert(Peer { jitter: Jitter::default(), decoder, pcm: Vec::new(), quiet_for: u32::MAX })
            }
        };
        peer.jitter.push(p.seq, p.opus.to_vec());
    }

    /// Mix 10 ms of everyone's audio (and any cues) into `out`, each scaled by `volume(user)`.
    /// The mix is also the echo canceller's reference.
    pub fn render(&mut self, out: &mut [f32], s: &Settings, volume: impl Fn(u32) -> f32) {
        debug_assert_eq!(out.len(), STEP);
        out.fill(0.0);
        for (&(user, _), peer) in self.peers.iter_mut() {
            if peer.pcm.len() < STEP {
                let mut frame = vec![0.0f32; FRAME_SAMPLES];
                let decoded = match peer.jitter.pop() {
                    Play::Packet(p) => peer.decoder.decode_float(&p, &mut frame, false).ok().inspect(|_| peer.quiet_for = 0),
                    Play::Fec(p) => peer.decoder.decode_float(&p, &mut frame, true).ok(),
                    Play::Conceal => peer.decoder.decode_float(&[], &mut frame, false).ok(),
                    Play::Idle => None,
                };
                if let Some(n) = decoded {
                    peer.pcm.extend_from_slice(&frame[..n]);
                }
            }
            peer.quiet_for = peer.quiet_for.saturating_add(1);
            if peer.pcm.len() >= STEP {
                let gain = if s.deafened { 0.0 } else { volume(user) };
                for (o, x) in out.iter_mut().zip(peer.pcm.drain(..STEP)) {
                    *o += x * gain;
                }
            }
        }
        let n = self.cues.len().min(STEP);
        for (o, x) in out.iter_mut().zip(self.cues.drain(..n)) {
            *o += x;
        }
        // Soft clip so several loud speakers don't crackle.
        for o in out.iter_mut() {
            if o.abs() > 0.8 {
                *o = o.signum() * (0.8 + 0.2 * ((o.abs() - 0.8) / 0.2).tanh());
            }
        }
        if s.echo_cancellation {
            self.apm.handle_render_frame(out).ok();
        }
    }

    /// Users heard within the last 200 ms.
    pub fn speaking(&self) -> impl Iterator<Item = u32> + '_ {
        self.peers.iter().filter(|((_, share), p)| !share && p.quiet_for < 20).map(|((u, _), _)| *u)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(i: usize) -> f32 {
        let t = i as f32 / 48_000.0;
        0.3 * (t * 330.0 * std::f32::consts::TAU).sin() + 0.15 * (t * 990.0 * std::f32::consts::TAU).sin()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    /// Alice's core sends to Bob's core through the SFU's re-stamping, with some loss.
    #[test]
    fn audio_goes_through_and_survives_loss() {
        let s = Settings { mode: Mode::Always, noise_suppression: false, ..Default::default() };
        let mut alice = Core::new(64_000).unwrap();
        let mut bob = Core::new(64_000).unwrap();
        let mut heard = Vec::new();
        let mut out = vec![0.0; STEP];
        let mut sent = 0;
        for step in 0..300 {
            let mic: Vec<f32> = (0..STEP).map(|i| tone(step * STEP + i)).collect();
            for pkt in alice.capture(&mic, &s, false) {
                sent += 1;
                let up = voice::decode_up(&pkt).unwrap();
                // Drop every 9th packet, as a lossy link would.
                if sent % 9 != 0 {
                    bob.receive(&voice::relay(7, &up));
                }
            }
            bob.render(&mut out, &s, |_| 1.0);
            heard.extend_from_slice(&out);
        }
        assert_eq!(sent, 150, "one packet per 20 ms");
        assert!(bob.speaking().any(|u| u == 7));
        // After the first half-second (AGC settling, buffering), Bob hears a steady signal.
        let tail = &heard[48_000..];
        let quiet = tail.chunks(STEP).filter(|c| rms(c) < 0.02).count();
        assert!(rms(tail) > 0.05, "rms {}", rms(tail));
        assert!(quiet <= 2, "{quiet} silent steps despite loss recovery");
    }

    #[test]
    fn muting_and_push_to_talk_gate_sending() {
        let mut c = Core::new(32_000).unwrap();
        let mic: Vec<f32> = (0..STEP).map(tone).collect();
        let muted = Settings { mode: Mode::Always, muted: true, ..Default::default() };
        assert!((0..4).all(|_| c.capture(&mic, &muted, false).is_empty()));
        let ptt = Settings { mode: Mode::PushToTalk, ..Default::default() };
        assert!((0..4).all(|_| c.capture(&mic, &ptt, false).is_empty()));
        let sent: usize = (0..4).map(|_| c.capture(&mic, &ptt, true).len()).sum();
        assert_eq!(sent, 2);
    }

    #[test]
    fn opening_the_voice_gate_sends_the_preroll() {
        let mut c = Core::new(32_000).unwrap();
        let s = Settings { vad_threshold: 0.0, ..Default::default() };
        let mic: Vec<f32> = (0..STEP).map(tone).collect();
        let counts: Vec<usize> = (0..64).map(|_| c.capture(&mic, &s, false).len()).collect();
        // Silent through warm-up, then 60 ms of earlier audio at once (10 ms left over), then one per 20 ms.
        assert!(counts[..WARMUP_STEPS as usize].iter().all(|&n| n == 0));
        assert_eq!(&counts[WARMUP_STEPS as usize..], &[3, 1, 0, 1]);
    }

    #[test]
    fn silence_never_opens_the_voice_gate() {
        let mut c = Core::new(32_000).unwrap();
        let s = Settings::default();
        let mut seed = 12345u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        let mut max_vad = 0.0f32;
        let mut sent = 0;
        for _ in 0..200 {
            let hiss: Vec<f32> = (0..STEP).map(|_| noise() * 0.01).collect();
            sent += c.capture(&hiss, &s, false).len() as u32;
            max_vad = max_vad.max(c.vad);
        }
        assert_eq!(sent, 0, "background hiss opened the gate (max vad {max_vad})");
    }

    #[test]
    fn deafened_hears_only_cues() {
        let mut c = Core::new(32_000).unwrap();
        let s = Settings { deafened: true, ..Default::default() };
        c.cue(Cue::Join, 1.0);
        let mut out = vec![0.0; STEP];
        c.render(&mut out, &s, |_| 1.0);
        assert!(rms(&out) > 0.0);
    }
}
