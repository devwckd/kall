//! The voice thread: opens the microphone and speakers with cpal, converts to 48 kHz mono,
//! runs [`Core`] every 10 ms, sends packets as QUIC datagrams and feeds received ones in.
//!
//! The loop is clocked by the microphone (or a timer without one). Playback goes through a
//! small queue the output callback drains; it's kept near 40 ms so latency stays low.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use device_query::{DeviceQuery, DeviceState, Keycode};

use super::core::{Core, Mode, STEP, Settings};
use super::resample::{Resampler, downmix};
use super::sounds::Cue;

/// The network side of voice, shared with the net thread: the live QUIC connection to send
/// on, and where incoming voice datagrams go.
#[derive(Default)]
pub struct VoiceLink {
    pub conn: Mutex<Option<quinn::Connection>>,
    pub inbound: Mutex<Option<Sender<Vec<u8>>>>,
    /// Video datagrams go to the video receiver instead.
    pub video_inbound: Mutex<Option<Sender<Vec<u8>>>>,
}

impl VoiceLink {
    pub fn send(&self, datagram: Vec<u8>) {
        if let Some(c) = self.conn.lock().unwrap().as_ref() {
            c.send_datagram(datagram.into()).ok();
        }
    }
}

/// What the UI sets, and what the engine reports back.
pub struct Shared {
    pub settings: Mutex<Settings>,
    /// Per-user playback volume (1.0 = 100%).
    pub volumes: Mutex<HashMap<u32, f32>>,
    /// Push-to-talk key: a device_query key name ("LAlt", "Grave", …) or "Mouse4"/"Mouse5".
    pub ptt_key: Mutex<String>,
    pub cues: Mutex<Vec<(Cue, f32)>>,
    /// Who's audible right now (user ids), and whether we're sending.
    pub speaking: Mutex<HashSet<u32>>,
    pub transmitting: AtomicBool,
    /// Microphone level 0–1, as f32 bits.
    pub level: AtomicU32,
    pub error: Mutex<Option<String>>,
}

impl Shared {
    pub fn new(settings: Settings, ptt_key: String) -> Arc<Shared> {
        Arc::new(Shared {
            settings: Mutex::new(settings),
            volumes: Mutex::new(HashMap::new()),
            ptt_key: Mutex::new(ptt_key),
            cues: Mutex::new(Vec::new()),
            speaking: Mutex::new(HashSet::new()),
            transmitting: AtomicBool::new(false),
            level: AtomicU32::new(0),
            error: Mutex::new(None),
        })
    }
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
    pub fn cue(&self, cue: Cue, gain: f32) {
        self.cues.lock().unwrap().push((cue, gain));
    }
}

pub struct Engine {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    link: Arc<VoiceLink>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        *self.link.inbound.lock().unwrap() = None;
        if let Some(t) = self.thread.take() {
            t.join().ok();
        }
    }
}

type Queue = Arc<Mutex<VecDeque<f32>>>;

impl Engine {
    /// Start voice with the given devices (ids from [`super::devices`], `None` = default).
    pub fn start(link: Arc<VoiceLink>, shared: Arc<Shared>, input: Option<String>, output: Option<String>, wake: rune::Waker) -> Engine {
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        *link.inbound.lock().unwrap() = Some(tx);
        let (st, ln) = (stop.clone(), link.clone());
        let thread = std::thread::Builder::new()
            .name("kall-voice".into())
            .spawn(move || {
                if let Err(e) = run(&ln, &shared, input, output, rx, &st, &wake) {
                    *shared.error.lock().unwrap() = Some(format!("{e:#}"));
                    wake();
                }
            })
            .expect("voice thread");
        Engine { stop, thread: Some(thread), link }
    }
}

fn find(host: &cpal::Host, id: &Option<String>, input: bool) -> Option<cpal::Device> {
    if let Some(id) = id
        && let Ok(id) = id.parse()
        && let Some(d) = host.device_by_id(&id)
    {
        return Some(d);
    }
    if input { host.default_input_device() } else { host.default_output_device() }
}

fn input_stream<T>(dev: &cpal::Device, cfg: cpal::StreamConfig, q: Queue) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = cfg.channels as usize;
    let mut rs = Resampler::new(cfg.sample_rate, 48_000);
    let (mut mono, mut out) = (Vec::new(), Vec::new());
    Ok(dev.build_input_stream(
        cfg,
        move |data: &[T], _| {
            mono.clear();
            out.clear();
            let f: Vec<f32> = data.iter().map(|s| f32::from_sample(*s)).collect();
            downmix(&f, channels, &mut mono);
            rs.process(&mono, &mut out);
            let mut q = q.lock().unwrap();
            q.extend(out.iter().copied());
            // Don't let a stalled engine grow this without bound (keep 200 ms).
            let excess = q.len().saturating_sub(9_600);
            q.drain(..excess);
        },
        |e| eprintln!("kall: microphone error: {e}"),
        None,
    )?)
}

fn output_stream<T>(dev: &cpal::Device, cfg: cpal::StreamConfig, q: Queue) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = cfg.channels as usize;
    let mut rs = Resampler::new(48_000, cfg.sample_rate);
    let mut buf: VecDeque<f32> = VecDeque::new();
    let mut tmp = Vec::new();
    Ok(dev.build_output_stream(
        cfg,
        move |data: &mut [T], _| {
            let frames = data.len() / channels;
            // Pull 48 kHz audio and convert until there's enough at the device's rate.
            while buf.len() < frames {
                let chunk: Vec<f32> = {
                    let mut q = q.lock().unwrap();
                    let n = q.len().min(480);
                    q.drain(..n).collect()
                };
                if chunk.is_empty() {
                    break;
                }
                tmp.clear();
                rs.process(&chunk, &mut tmp);
                buf.extend(tmp.iter().copied());
            }
            for frame in data.chunks_mut(channels) {
                let v = buf.pop_front().unwrap_or(0.0);
                for s in frame {
                    *s = T::from_sample(v);
                }
            }
        },
        |e| eprintln!("kall: speaker error: {e}"),
        None,
    )?)
}

fn open_input(host: &cpal::Host, id: &Option<String>, q: Queue) -> Result<Option<cpal::Stream>> {
    let Some(dev) = find(host, id, true) else { return Ok(None) };
    let sc = dev.default_input_config().context("microphone config")?;
    let cfg = sc.config();
    eprintln!("kall voice: mic {:?} {} Hz {} ch {:?}", dev.description().map(|d| d.name().to_string()).unwrap_or_default(), cfg.sample_rate, cfg.channels, sc.sample_format());
    let s = match sc.sample_format() {
        SampleFormat::F32 => input_stream::<f32>(&dev, cfg, q)?,
        SampleFormat::I16 => input_stream::<i16>(&dev, cfg, q)?,
        SampleFormat::U16 => input_stream::<u16>(&dev, cfg, q)?,
        SampleFormat::I32 => input_stream::<i32>(&dev, cfg, q)?,
        f => return Err(anyhow!("unsupported microphone sample format {f:?}")),
    };
    s.play()?;
    Ok(Some(s))
}

fn open_output(host: &cpal::Host, id: &Option<String>, q: Queue) -> Result<cpal::Stream> {
    let dev = find(host, id, false).ok_or_else(|| anyhow!("no speakers or headphones found"))?;
    let sc = dev.default_output_config().context("speaker config")?;
    let cfg = sc.config();
    eprintln!("kall voice: speakers {:?} {} Hz {} ch {:?}", dev.description().map(|d| d.name().to_string()).unwrap_or_default(), cfg.sample_rate, cfg.channels, sc.sample_format());
    let s = match sc.sample_format() {
        SampleFormat::F32 => output_stream::<f32>(&dev, cfg, q)?,
        SampleFormat::I16 => output_stream::<i16>(&dev, cfg, q)?,
        SampleFormat::U16 => output_stream::<u16>(&dev, cfg, q)?,
        SampleFormat::I32 => output_stream::<i32>(&dev, cfg, q)?,
        f => return Err(anyhow!("unsupported speaker sample format {f:?}")),
    };
    s.play()?;
    Ok(s)
}

fn ptt_down(dq: &DeviceState, key: &str) -> bool {
    match key {
        "Mouse4" => dq.get_mouse().button_pressed.get(4).copied().unwrap_or(false),
        "Mouse5" => dq.get_mouse().button_pressed.get(5).copied().unwrap_or(false),
        k => k.parse::<Keycode>().is_ok_and(|k| dq.get_keys().contains(&k)),
    }
}

fn run(link: &VoiceLink, shared: &Shared, input: Option<String>, output: Option<String>, rx: Receiver<Vec<u8>>, stop: &AtomicBool, wake: &rune::Waker) -> Result<()> {
    let host = cpal::default_host();
    let mic_q: Queue = Default::default();
    let spk_q: Queue = Default::default();
    let mic = match open_input(&host, &input, mic_q.clone()) {
        Ok(m) => m,
        Err(e) => {
            *shared.error.lock().unwrap() = Some(format!("Microphone unavailable: {e:#}"));
            wake();
            None
        }
    };
    let _spk = open_output(&host, &output, spk_q.clone())?;
    let mut core = Core::new(shared.settings.lock().unwrap().bitrate)?;
    let dq = DeviceState::new();
    let mut frame = vec![0.0f32; STEP];
    let mut out = vec![0.0f32; STEP];
    let mut last_speaking: HashSet<u32> = HashSet::new();
    let mut last_tx = false;
    let mut next_tick = Instant::now();
    let mut level_wake = Instant::now();

    while !stop.load(Ordering::Relaxed) {
        // Wait for 10 ms of microphone audio, or the timer when there's no microphone.
        let have_mic = if mic.is_some() {
            let deadline = Instant::now() + Duration::from_millis(60);
            loop {
                let mut q = mic_q.lock().unwrap();
                if q.len() >= STEP {
                    for (d, s) in frame.iter_mut().zip(q.drain(..STEP)) {
                        *d = s;
                    }
                    break true;
                }
                drop(q);
                if Instant::now() > deadline || stop.load(Ordering::Relaxed) {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        } else {
            next_tick += Duration::from_millis(10);
            std::thread::sleep(next_tick.saturating_duration_since(Instant::now()));
            false
        };
        if !have_mic {
            frame.fill(0.0);
        }

        while let Ok(d) = rx.try_recv() {
            core.receive(&d);
        }
        for (cue, gain) in shared.cues.lock().unwrap().drain(..) {
            core.cue(cue, gain);
        }
        let settings = shared.settings.lock().unwrap().clone();
        let ptt = settings.mode == Mode::PushToTalk && ptt_down(&dq, &shared.ptt_key.lock().unwrap());
        for pkt in core.capture(&frame, &settings, ptt) {
            link.send(pkt);
        }
        {
            let vols = shared.volumes.lock().unwrap();
            core.render(&mut out, &settings, |u| vols.get(&u).copied().unwrap_or(1.0));
        }
        {
            let mut q = spk_q.lock().unwrap();
            // Keep playback ~40 ms ahead; drop when the speaker clock runs slower than the mic's.
            if q.len() < 2_880 {
                q.extend(out.iter().copied());
            }
        }

        shared.level.store(core.level.to_bits(), Ordering::Relaxed);
        shared.transmitting.store(core.transmitting, Ordering::Relaxed);
        let speaking: HashSet<u32> = core.speaking().collect();
        if speaking != last_speaking || core.transmitting != last_tx {
            *shared.speaking.lock().unwrap() = speaking.clone();
            last_speaking = speaking;
            last_tx = core.transmitting;
            wake();
        } else if level_wake.elapsed() > Duration::from_millis(100) {
            // The settings screen shows a live mic meter.
            level_wake = Instant::now();
            wake();
        }
    }
    Ok(())
}
