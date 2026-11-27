//! Sound device lists for the settings screen. Enumerating can take a while on some systems,
//! so it runs off the UI thread.
//!
//! On Linux, kall talks to PulseAudio (or PipeWire's PulseAudio server) when there is one, so
//! the lists match the system's sound settings; the "Monitor of …" loopback sources aren't
//! microphones and are left out. Without a sound server it falls back to ALSA, whose plugin
//! aliases (surround51:…, dmix:…, plughw:…) are hidden in favour of one entry per card.

use cpal::traits::{DeviceTrait, HostTrait};

#[derive(Clone, Debug, Default)]
pub struct Device {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, Default)]
pub struct Devices {
    pub inputs: Vec<Device>,
    pub outputs: Vec<Device>,
}

fn describe(d: &cpal::Device) -> Option<Device> {
    let id = d.id().ok()?.to_string();
    let name = d.description().map(|x| x.name().to_string()).unwrap_or_else(|_| id.clone());
    Some(Device { id, name })
}

/// Whether a device belongs in the list (see the module docs). `id` is cpal's "host:name".
pub fn worth_listing(id: &str) -> bool {
    let Some((host, name)) = id.split_once(':') else { return true };
    match host.to_ascii_lowercase().as_str() {
        // Loopback "Monitor of …" sources.
        "pulseaudio" | "pipewire" => !name.ends_with(".monitor"),
        // ALSA: one entry per card (hw:), not every plugin alias; "default" is System default.
        "alsa" => {
            let base = name.split(':').next().unwrap_or(name);
            base == "hw" || (!name.contains(':') && !matches!(name, "default" | "null" | "sysdefault"))
        }
        _ => true,
    }
}

pub fn list() -> Devices {
    let host = cpal::default_host();
    let keep = |d: Device| worth_listing(&d.id).then_some(d);
    Devices {
        inputs: host.input_devices().map(|ds| ds.filter_map(|d| describe(&d).and_then(keep)).collect()).unwrap_or_default(),
        outputs: host.output_devices().map(|ds| ds.filter_map(|d| describe(&d).and_then(keep)).collect()).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hides_monitors_and_alsa_aliases() {
        use super::worth_listing as w;
        assert!(w("pulseaudio:alsa_input.usb-Focusrite_Scarlett-00.analog-stereo"));
        assert!(!w("pulseaudio:alsa_output.pci-0000_0b_00.4.analog-stereo.monitor"));
        assert!(w("alsa:hw:CARD=Generic,DEV=0"));
        assert!(!w("alsa:surround51:CARD=Generic,DEV=0"));
        assert!(!w("alsa:plughw:CARD=Generic,DEV=0"));
        assert!(!w("alsa:dmix:CARD=Generic,DEV=0"));
        assert!(!w("alsa:default"));
        assert!(w("alsa:pipewire"));
        assert!(w("wasapi:{0.0.1.00000000}.{abc}"));
    }
}
