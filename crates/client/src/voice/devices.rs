//! Sound device lists for the settings screen. Enumerating can take a while on some systems,
//! so it runs off the UI thread.

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

pub fn list() -> Devices {
    let host = cpal::default_host();
    Devices {
        inputs: host.input_devices().map(|ds| ds.filter_map(|d| describe(&d)).collect()).unwrap_or_default(),
        outputs: host.output_devices().map(|ds| ds.filter_map(|d| describe(&d)).collect()).unwrap_or_default(),
    }
}
