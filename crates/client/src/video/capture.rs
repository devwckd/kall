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
    /// The window's title, or "Display 1".
    pub name: String,
    /// The app that owns the window, or the display's resolution.
    pub detail: String,
    pub kind: SourceKind,
    /// The window's process, so its sound can be shared on its own.
    pub pid: Option<u32>,
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
                    (Ok(w), Ok(h)) => format!("{w}×{h}"),
                    _ => String::new(),
                };
                out.push(Source { id: format!("d:{idx}"), name: format!("Display {}", i + 1), detail: size, kind: SourceKind::Display, pid: None });
            }
        }
        let me = std::process::id();
        if let Ok(ws) = Window::enumerate() {
            for w in ws {
                let Ok(title) = w.title() else { continue };
                if title.trim().is_empty() || !w.is_valid() || w.process_id().is_ok_and(|p| p == me) {
                    continue;
                }
                let app = w.process_name().unwrap_or_default().trim_end_matches(".exe").to_string();
                out.push(Source { id: format!("w:{}", w.as_raw_hwnd() as usize), name: title, detail: app, kind: SourceKind::Window, pid: w.process_id().ok() });
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
                let size = match (m.width(), m.height()) {
                    (Ok(w), Ok(h)) => format!("{w}×{h}"),
                    _ => String::new(),
                };
                out.push(Source { id: format!("d:{id}"), name: m.name().unwrap_or_else(|_| format!("Display {}", i + 1)), detail: size, kind: SourceKind::Display, pid: None });
            }
        }
        for w in xcap::Window::all().unwrap_or_default() {
            let (Ok(id), Ok(title)) = (w.id(), w.title()) else { continue };
            if title.trim().is_empty() || w.is_minimized().unwrap_or(false) || w.pid().is_ok_and(|p| p == std::process::id()) {
                continue;
            }
            out.push(Source { id: format!("w:{id}"), name: title, detail: w.app_name().unwrap_or_default(), kind: SourceKind::Window, pid: w.pid().ok() });
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

/// The share picker's thumbnails, by source id.
pub type Thumbs = Arc<std::sync::Mutex<std::collections::HashMap<String, rune::Pixels>>>;

/// Keep `thumbs` filled with small pictures of `sources`, refreshed every couple of seconds,
/// until `stop` is set. Uses xcap's one-shot capture on every platform (cheap, no stream).
pub fn thumbnails(sources: Vec<Source>, thumbs: Thumbs, fresh: Arc<std::sync::atomic::AtomicU64>, stop: Arc<std::sync::atomic::AtomicBool>, wake: rune::Waker) {
    use std::sync::atomic::Ordering;
    std::thread::Builder::new()
        .name("kall-thumbs".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let monitors = xcap::Monitor::all().unwrap_or_default();
                let windows = xcap::Window::all().unwrap_or_default();
                let mut display_n = 0;
                for src in &sources {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    let n = src.id.split_once(':').and_then(|(_, n)| n.parse::<u64>().ok());
                    let img = match src.kind {
                        SourceKind::Display => {
                            display_n += 1;
                            // Windows numbers displays itself; elsewhere the id is xcap's.
                            let m = if cfg!(windows) { monitors.get(display_n - 1) } else { monitors.iter().find(|m| m.id().ok().map(u64::from) == n) };
                            m.and_then(|m| m.capture_image().ok())
                        }
                        SourceKind::Window => windows.iter().find(|w| w.id().ok().map(u64::from) == n.map(|n| n & 0xFFFF_FFFF)).and_then(|w| w.capture_image().ok()),
                    };
                    let Some(img) = img else { continue };
                    let (w, h) = (img.width() as usize, img.height() as usize);
                    if w < 2 || h < 2 {
                        continue;
                    }
                    // fit_bgra only moves bytes around, so it scales RGBA just as well.
                    let (small, tw, th) = super::codec::fit_bgra(img.as_raw(), w, h, w * 4, 400, 225);
                    let px = thumbs.lock().unwrap().entry(src.id.clone()).or_default().clone();
                    px.replace(tw as u32, th as u32, small);
                    fresh.fetch_add(1, Ordering::Relaxed);
                    wake();
                }
                for _ in 0..20 {
                    if stop.load(Ordering::Relaxed) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// Real capture on this machine: lists sources and fills thumbnails. Prints counts only, so
    /// nothing on screen is saved. `cargo test -p kall thumbnails_fill -- --ignored`
    #[test]
    #[ignore = "captures the real screen"]
    fn thumbnails_fill() {
        let sources = super::list();
        let (displays, windows) = (sources.iter().filter(|s| s.kind == super::SourceKind::Display).count(), sources.iter().filter(|s| s.kind == super::SourceKind::Window).count());
        let with_pid = sources.iter().filter(|s| s.pid.is_some()).count();
        println!("{displays} displays, {windows} windows ({with_pid} with a process id)");
        assert!(displays > 0);
        let thumbs: super::Thumbs = Default::default();
        let fresh = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        super::thumbnails(sources.clone(), thumbs.clone(), fresh.clone(), stop.clone(), Arc::new(|| {}));
        let t = std::time::Instant::now();
        while fresh.load(Ordering::Relaxed) < sources.len() as u64 && t.elapsed().as_secs() < 15 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        stop.store(true, Ordering::Relaxed);
        let got = thumbs.lock().unwrap().len();
        println!("{got}/{} thumbnails in {:.1}s", sources.len(), t.elapsed().as_secs_f64());
        assert!(thumbs.lock().unwrap().keys().any(|k| k.starts_with("d:")), "every display gets a thumbnail");
    }
}
