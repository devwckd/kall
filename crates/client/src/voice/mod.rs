//! Voice chat: a device-free [`core`] (DSP, Opus, jitter buffers, mixing) driven by an
//! [`engine`] thread that talks to the sound devices and the network.

pub mod core;
pub mod devices;
pub mod engine;
pub mod jitter;
pub mod resample;
pub mod sounds;

use std::collections::BTreeMap;

use self::core::Mode;

/// Voice settings that persist between runs.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct VoicePrefs {
    pub input: Option<String>,
    pub output: Option<String>,
    pub mode: Mode,
    pub ptt_key: String,
    pub vad_threshold: f32,
    pub noise_suppression: bool,
    pub echo_cancellation: bool,
    /// username → playback volume (1.0 = 100%)
    pub volumes: BTreeMap<String, f32>,
    /// Camera to use (nokhwa index); the first one when unset.
    pub camera: Option<u32>,
    /// How loud you are to others (1.0 = 100%), after noise suppression and auto gain.
    pub input_volume: f32,
    /// Master volume for everyone you hear (1.0 = 100%), on top of per-person volumes.
    pub output_volume: f32,
}

impl Default for VoicePrefs {
    fn default() -> Self {
        VoicePrefs {
            input: None,
            output: None,
            mode: Mode::VoiceActivity,
            ptt_key: "LAlt".into(),
            vad_threshold: 0.6,
            noise_suppression: true,
            echo_cancellation: true,
            volumes: BTreeMap::new(),
            camera: None,
            input_volume: 1.0,
            output_volume: 1.0,
        }
    }
}

/// Push-to-talk keys offered in settings: (device_query name, label).
pub const PTT_KEYS: &[(&str, &str)] = &[
    ("LAlt", "Left Alt"),
    ("LControl", "Left Ctrl"),
    ("CapsLock", "Caps Lock"),
    ("Grave", "` (backtick)"),
    ("V", "V"),
    ("Mouse4", "Mouse 4"),
    ("Mouse5", "Mouse 5"),
];
