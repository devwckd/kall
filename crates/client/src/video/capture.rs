//! Screens and windows to share. Windows uses Windows.Graphics.Capture (fast, GPU-backed,
//! with the cursor); macOS and Linux use xcap, polled at the stream's frame rate (untested
//! there so far).

use std::sync::Arc;

use anyhow::Result;

use super::send::Slot;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    Display,
    Window,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// "d:<n>" for displays, "w:<id>" for windows.
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
}

/// Dropping this stops the capture.
pub struct Capture(#[allow(dead_code)] Box<dyn Send>);

#[cfg(windows)]
mod imp {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::{Result, anyhow};
    use windows_capture::capture::{CaptureControl, Context, GraphicsCaptureApiHandler};
    use windows_capture::frame::Frame;
    use windows_capture::graphics_capture_api::InternalCaptureControl;
    use windows_capture::monitor::Monitor;
    use windows_capture::settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings, MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    };
    use windows_capture::window::Window;

    use super::{Source, SourceKind};
    use crate::video::send::{RawFrame, Slot};

    type BoxErr = Box<dyn std::error::Error + Send + Sync>;

    struct Handler {
        slot: Arc<Slot>,
        buf: Vec<u8>,
    }

    impl GraphicsCaptureApiHandler for Handler {
        type Flags = Arc<Slot>;
        type Error = BoxErr;

        fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
            Ok(Handler { slot: ctx.flags, buf: Vec::new() })
        }

        fn on_frame_arrived(&mut self, frame: &mut Frame, _: InternalCaptureControl) -> Result<(), Self::Error> {
            let fb = frame.buffer()?;
            let (w, h) = (fb.width() as usize, fb.height() as usize);
            let data = fb.as_nopadding_buffer(&mut self.buf);
            self.slot.put(RawFrame { width: w, height: h, stride: w * 4, bgra: data.to_vec() });
            Ok(())
        }
    }

    struct Running(Option<CaptureControl<Handler, BoxErr>>);

    impl Drop for Running {
        fn drop(&mut self) {
            if let Some(c) = self.0.take() {
                c.stop().ok();
            }
        }
    }

    pub fn list() -> Vec<Source> {
        let mut out = Vec::new();
        if let Ok(ms) = Monitor::enumerate() {
            for (i, m) in ms.iter().enumerate() {
                let idx = m.index().unwrap_or(i + 1);
                let size = match (m.width(), m.height()) {
                    (Ok(w), Ok(h)) => format!(" · {w}×{h}"),
                    _ => String::new(),
                };
                out.push(Source { id: format!("d:{idx}"), name: format!("Display {}{size}", i + 1), kind: SourceKind::Display });
            }
        }
        let me = std::process::id();
        if let Ok(ws) = Window::enumerate() {
            for w in ws {
                let Ok(title) = w.title() else { continue };
                if title.trim().is_empty() || !w.is_valid() || w.process_id().is_ok_and(|p| p == me) {
                    continue;
                }
                let app = w.process_name().unwrap_or_default();
                let name = if app.is_empty() { title } else { format!("{title} — {}", app.trim_end_matches(".exe")) };
                out.push(Source { id: format!("w:{}", w.as_raw_hwnd() as usize), name, kind: SourceKind::Window });
            }
        }
        out
    }

    pub fn start(source: &Source, fps: u32, slot: Arc<Slot>) -> Result<Box<dyn Send>> {
        let interval = MinimumUpdateIntervalSettings::Custom(Duration::from_secs_f64(1.0 / fps as f64));
        let ctl = match source.id.split_once(':') {
            Some(("d", n)) => {
                let m = Monitor::from_index(n.parse()?).map_err(|e| anyhow!("{e}"))?;
                let s = Settings::new(m, CursorCaptureSettings::WithCursor, DrawBorderSettings::Default, SecondaryWindowSettings::Default, interval, DirtyRegionSettings::Default, ColorFormat::Bgra8, slot);
                Handler::start_free_threaded(s).map_err(|e| anyhow!("{e}"))?
            }
            Some(("w", h)) => {
                let w = Window::from_raw_hwnd(h.parse::<usize>()? as *mut std::ffi::c_void);
                if !w.is_valid() {
                    return Err(anyhow!("that window is gone"));
                }
                let s = Settings::new(w, CursorCaptureSettings::WithCursor, DrawBorderSettings::Default, SecondaryWindowSettings::Default, interval, DirtyRegionSettings::Default, ColorFormat::Bgra8, slot);
                Handler::start_free_threaded(s).map_err(|e| anyhow!("{e}"))?
            }
            _ => return Err(anyhow!("unknown source")),
        };
        Ok(Box::new(Running(Some(ctl))))
    }
}

#[cfg(not(windows))]
mod imp {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use anyhow::{Result, anyhow};

    use super::{Source, SourceKind};
    use crate::video::send::{RawFrame, Slot};

    struct Running(Arc<AtomicBool>);

    impl Drop for Running {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    pub fn list() -> Vec<Source> {
        let mut out = Vec::new();
        for (i, m) in xcap::Monitor::all().unwrap_or_default().iter().enumerate() {
            if let Ok(id) = m.id() {
                out.push(Source { id: format!("d:{id}"), name: m.name().unwrap_or_else(|_| format!("Display {}", i + 1)), kind: SourceKind::Display });
            }
        }
        for w in xcap::Window::all().unwrap_or_default() {
            let (Ok(id), Ok(title)) = (w.id(), w.title()) else { continue };
            if title.trim().is_empty() || w.is_minimized().unwrap_or(false) || w.pid().is_ok_and(|p| p == std::process::id()) {
                continue;
            }
            let app = w.app_name().unwrap_or_default();
            out.push(Source { id: format!("w:{id}"), name: if app.is_empty() { title } else { format!("{title} — {app}") }, kind: SourceKind::Window });
        }
        out
    }

    fn grab(id: &str) -> Result<image::RgbaImage> {
        let (kind, n) = id.split_once(':').ok_or_else(|| anyhow!("bad source"))?;
        let n: u32 = n.parse()?;
        let img = if kind == "d" {
            let m = xcap::Monitor::all()?.into_iter().find(|m| m.id().ok() == Some(n)).ok_or_else(|| anyhow!("display gone"))?;
            m.capture_image()?
        } else {
            let w = xcap::Window::all()?.into_iter().find(|w| w.id().ok() == Some(n)).ok_or_else(|| anyhow!("window gone"))?;
            w.capture_image()?
        };
        Ok(image::RgbaImage::from_raw(img.width(), img.height(), img.into_raw()).ok_or_else(|| anyhow!("bad image"))?)
    }

    pub fn start(source: &Source, fps: u32, slot: Arc<Slot>) -> Result<Box<dyn Send>> {
        grab(&source.id)?; // fail now if it can't be captured at all
        let stop = Arc::new(AtomicBool::new(false));
        let (st, id) = (stop.clone(), source.id.clone());
        std::thread::spawn(move || {
            let interval = Duration::from_secs_f64(1.0 / fps as f64);
            while !st.load(Ordering::Relaxed) {
                let t = Instant::now();
                if let Ok(img) = grab(&id) {
                    let (w, h) = (img.width() as usize, img.height() as usize);
                    let mut bgra = img.into_raw();
                    for p in bgra.chunks_exact_mut(4) {
                        p.swap(0, 2);
                    }
                    slot.put(RawFrame { width: w, height: h, stride: w * 4, bgra });
                }
                std::thread::sleep(interval.saturating_sub(t.elapsed()));
            }
        });
        Ok(Box::new(Running(stop)))
    }
}

pub fn list() -> Vec<Source> {
    imp::list()
}

pub fn start(source: &Source, fps: u32, slot: Arc<Slot>) -> Result<Capture> {
    imp::start(source, fps, slot).map(Capture)
}
