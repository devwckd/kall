//! Webcams through nokhwa (Media Foundation, AVFoundation, V4L2).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use anyhow::{Result, anyhow};
use nokhwa::Camera;
use nokhwa::pixel_format::RgbFormat;
use nokhwa::utils::{ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType, Resolution};

use super::send::{RawFrame, Slot};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    pub index: u32,
    pub name: String,
}

pub fn list() -> Vec<CameraInfo> {
    nokhwa::query(ApiBackend::Auto)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|c| match c.index() {
            CameraIndex::Index(i) => Some(CameraInfo { index: *i, name: c.human_name() }),
            CameraIndex::String(_) => None,
        })
        .collect()
}

/// Dropping this turns the camera off.
pub struct Running(Arc<AtomicBool>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Open camera `index` (close to 720p30) and feed frames into `slot` until dropped.
pub fn start(index: u32, slot: Arc<Slot>) -> Result<Running> {
    let stop = Arc::new(AtomicBool::new(false));
    let st = stop.clone();
    let (ok_tx, ok_rx) = channel();
    // The camera handle isn't Send on every platform: it lives on its own thread.
    std::thread::Builder::new().name("kall-camera".into()).spawn(move || {
        let want = RequestedFormat::new::<RgbFormat>(RequestedFormatType::Closest(CameraFormat::new(Resolution::new(1280, 720), FrameFormat::MJPEG, 30)));
        let mut cam = match Camera::new(CameraIndex::Index(index), want).and_then(|mut c| c.open_stream().map(|_| c)) {
            Ok(c) => {
                ok_tx.send(Ok(())).ok();
                c
            }
            Err(e) => {
                ok_tx.send(Err(anyhow!("camera: {e}"))).ok();
                return;
            }
        };
        while !st.load(Ordering::Relaxed) {
            let Ok(buf) = cam.frame() else {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            };
            let Ok(img) = buf.decode_image::<RgbFormat>() else { continue };
            let (w, h) = (img.width() as usize, img.height() as usize);
            let mut bgra = Vec::with_capacity(w * h * 4);
            for p in img.as_raw().chunks_exact(3) {
                bgra.extend_from_slice(&[p[2], p[1], p[0], 255]);
            }
            slot.put(RawFrame { width: w, height: h, stride: w * 4, bgra });
        }
        cam.stop_stream().ok();
    })?;
    ok_rx.recv_timeout(Duration::from_secs(10)).map_err(|_| anyhow!("the camera didn't respond"))??;
    Ok(Running(stop))
}
