//! Video: screen share and webcam. [`capture`] finds and reads sources, [`send`] encodes and
//! fragments, [`recv`] reassembles and decodes into pixels the UI draws.

pub mod camera;
pub mod capture;
pub mod codec;
#[cfg(feature = "ffmpeg")]
pub mod ff;
pub mod recv;
pub mod send;
pub mod share_audio;

/// A screen-share quality the sharer picks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quality {
    pub label: &'static str,
    pub max_w: usize,
    pub max_h: usize,
    pub fps: u32,
    pub bitrate: u32,
}

pub const QUALITIES: &[Quality] = &[
    Quality { label: "720p 30", max_w: 1280, max_h: 720, fps: 30, bitrate: 2_500_000 },
    Quality { label: "1080p 30", max_w: 1920, max_h: 1080, fps: 30, bitrate: 4_500_000 },
    Quality { label: "1080p 60", max_w: 1920, max_h: 1080, fps: 60, bitrate: 8_000_000 },
    Quality { label: "1440p 30", max_w: 2560, max_h: 1440, fps: 30, bitrate: 8_000_000 },
];

pub const CAMERA_QUALITY: Quality = Quality { label: "720p 30", max_w: 1280, max_h: 720, fps: 30, bitrate: 1_500_000 };
