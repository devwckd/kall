//! Living on the desktop: app preferences, notifications, sounds outside calls, the tray icon
//! with its unread badge, and starting at login.

use std::collections::BTreeMap;
use std::sync::{Mutex, OnceLock};

use anyhow::{Result, anyhow};
use kall_proto::Id;

use crate::voice::sounds::{self, Cue};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NotifyLevel {
    /// Every message.
    All,
    /// @mentions and replies to you.
    #[default]
    Mentions,
    Nothing,
}

impl NotifyLevel {
    pub fn next(self) -> NotifyLevel {
        match self {
            NotifyLevel::All => NotifyLevel::Mentions,
            NotifyLevel::Mentions => NotifyLevel::Nothing,
            NotifyLevel::Nothing => NotifyLevel::All,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            NotifyLevel::All => "All messages",
            NotifyLevel::Mentions => "Mentions",
            NotifyLevel::Nothing => "Muted",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ThemeChoice {
    /// Follow the system's light/dark setting.
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AppPrefs {
    pub theme: ThemeChoice,
    pub sounds: bool,
    /// 0–1, for every sound kall plays.
    pub volume: f32,
    pub autostart: bool,
    /// channel id → notification level (text channels; unset = Mentions)
    pub notify: BTreeMap<String, NotifyLevel>,
    /// Shown the "still running in the tray" notice once.
    pub told_about_tray: bool,
}

impl Default for AppPrefs {
    fn default() -> Self {
        AppPrefs { theme: ThemeChoice::System, sounds: true, volume: 0.8, autostart: false, notify: BTreeMap::new(), told_about_tray: false }
    }
}

impl AppPrefs {
    pub fn level(&self, channel: Id) -> NotifyLevel {
        self.notify.get(&channel.to_string()).copied().unwrap_or_default()
    }
}

// ---- icon ----

/// The kall logo (a white ghost on a near-black rounded square), optionally with a red badge:
/// `Some(0)` a dot, `Some(n)` a count.
pub fn icon_rgba(size: u32, badge: Option<u32>) -> Vec<u8> {
    let s = size as f32;
    let mut px = vec![0u8; (size * size * 4) as usize];
    let put = |px: &mut Vec<u8>, x: u32, y: u32, c: [f32; 4]| {
        let o = ((y * size + x) * 4) as usize;
        let a = c[3];
        for i in 0..3 {
            px[o + i] = (c[i] * a * 255.0 + px[o + i] as f32 * (1.0 - a)).round() as u8;
        }
        px[o + 3] = ((a + px[o + 3] as f32 / 255.0 * (1.0 - a)) * 255.0).round() as u8;
    };
    // Coverage of a rounded rectangle at (x, y), antialiased over one pixel.
    let rrect = |x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        let dx = (x0 + r - x).max(x - (x1 - r)).max(0.0);
        let dy = (y0 + r - y).max(y - (y1 - r)).max(0.0);
        let d = (dx * dx + dy * dy).sqrt() - r;
        let inside = x >= x0 && x <= x1 && y >= y0 && y <= y1;
        if !inside { 0.0 } else { (0.5 - d).clamp(0.0, 1.0) }
    };
    // The ghost (docs/icon.svg) in its own 96-unit space, centred on (48, 50).
    let ghost = |gx: f32, gy: f32| {
        let eye = |cx: f32| ((gx - cx) / 5.0).powi(2) + ((gy - 46.0) / 7.0).powi(2) < 1.0;
        if eye(38.0) || eye(58.0) {
            return false;
        }
        if gy < 46.0 {
            return (gx - 48.0).powi(2) + (gy - 46.0).powi(2) < 28.0 * 28.0;
        }
        // Three scalloped lobes along the bottom.
        (20.0..=76.0).contains(&gx) && gy < 78.0 + 4.0 * (std::f32::consts::PI * (76.0 - gx) / 9.333).sin()
    };
    let k = 96.0 / 0.8 / s; // ghost units per pixel (the ghost is drawn at 80%)
    const SS: usize = 4;
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let a = rrect(fx, fy, 0.0, 0.0, s, s, s * 0.28);
            if a <= 0.0 {
                continue;
            }
            put(&mut px, x, y, [0.094, 0.098, 0.106, a]); // near-black #18191B
            let mut hits = 0;
            for j in 0..SS {
                for i in 0..SS {
                    let sx = x as f32 + (i as f32 + 0.5) / SS as f32 - s / 2.0;
                    let sy = y as f32 + (j as f32 + 0.5) / SS as f32 - s / 2.0;
                    hits += ghost(48.0 + sx * k, 50.0 + sy * k) as u32;
                }
            }
            if hits > 0 {
                put(&mut px, x, y, [1.0, 1.0, 1.0, a * hits as f32 / (SS * SS) as f32]);
            }
        }
    }
    if let Some(n) = badge {
        let r = s * 0.24;
        let (cx, cy) = (s - r - s * 0.02, r + s * 0.02);
        for y in 0..size {
            for x in 0..size {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                let a = (r + 0.5 - d).clamp(0.0, 1.0);
                if a > 0.0 {
                    put(&mut px, x, y, [0.93, 0.26, 0.27, a]);
                }
            }
        }
        if n > 0 {
            draw_count(&mut px, size, cx, cy, r, n);
        }
    }
    px
}

/// Digits in a 3×5 pixel font, scaled to fit the badge.
fn draw_count(px: &mut [u8], size: u32, cx: f32, cy: f32, r: f32, n: u32) {
    const DIGITS: [[u8; 5]; 10] = [
        [7, 5, 5, 5, 7],
        [2, 6, 2, 2, 7],
        [7, 1, 7, 4, 7],
        [7, 1, 7, 1, 7],
        [5, 5, 7, 1, 1],
        [7, 4, 7, 1, 7],
        [7, 4, 7, 5, 7],
        [7, 1, 1, 1, 1],
        [7, 5, 7, 5, 7],
        [7, 5, 7, 1, 7],
    ];
    let text: Vec<usize> = if n > 9 { vec![9] } else { vec![n as usize] };
    let cell = ((r * 1.1) / 5.0).floor().max(1.0);
    let w = 3.0 * cell;
    let x0 = (cx - w / 2.0).round();
    let y0 = (cy - 2.5 * cell).round();
    for (row, bits) in DIGITS[text[0]].iter().enumerate() {
        for col in 0..3 {
            if bits & (4 >> col) != 0 {
                for yy in 0..cell as u32 {
                    for xx in 0..cell as u32 {
                        let x = x0 as u32 + col * cell as u32 + xx;
                        let y = y0 as u32 + row as u32 * cell as u32 + yy;
                        if x < size && y < size {
                            let o = ((y * size + x) * 4) as usize;
                            px[o..o + 4].copy_from_slice(&[255, 255, 255, 255]);
                        }
                    }
                }
            }
        }
    }
}

pub fn icon_png(size: u32) -> Result<Vec<u8>> {
    let img = image::RgbaImage::from_raw(size, size, icon_rgba(size, None)).ok_or_else(|| anyhow!("icon"))?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png)?;
    Ok(out.into_inner())
}

// ---- tray ----

static TRAY: OnceLock<Mutex<Option<rune::Tray>>> = OnceLock::new();

pub enum TrayAction {
    Open,
    ToggleMute,
    Quit,
}

pub fn tray_menu(in_voice: bool, muted: bool) -> Vec<rune::TrayItem> {
    let mut items = vec![rune::TrayItem::action("open", "Open kall")];
    if in_voice {
        items.push(rune::TrayItem::check("mute", "Mute microphone", muted));
    }
    items.push(rune::TrayItem::action("quit", "Quit kall"));
    items
}

/// Put kall in the system tray. `on` runs for clicks (from the tray's thread).
pub fn start_tray(on: impl Fn(TrayAction) + Send + Sync + 'static) {
    let icon = rune::TrayIcon::rgba(32, 32, icon_rgba(32, None));
    let spec = rune::TraySpec::new("dev.kall.client", "kall", icon).tooltip("kall").menu(tray_menu(false, false));
    let tray = rune::Tray::new(spec, move |e| match e {
        rune::TrayEvent::Activate { .. } => on(TrayAction::Open),
        rune::TrayEvent::Menu(id) => match id.as_str() {
            "open" => on(TrayAction::Open),
            "mute" => on(TrayAction::ToggleMute),
            "quit" => on(TrayAction::Quit),
            _ => {}
        },
        _ => {}
    });
    match tray {
        Ok(t) => {
            TRAY.get_or_init(|| Mutex::new(None)).lock().unwrap().replace(t);
        }
        Err(e) => eprintln!("kall: no tray icon: {e:?}"),
    }
}

pub fn has_tray() -> bool {
    TRAY.get().is_some_and(|t| t.lock().unwrap().is_some())
}

/// Show the unread state on the tray icon.
pub fn set_badge(badge: Option<u32>, tooltip: &str) {
    let Some(t) = TRAY.get() else { return };
    if let Some(t) = t.lock().unwrap().as_ref() {
        t.set_icon(rune::TrayIcon::rgba(32, 32, icon_rgba(32, badge))).ok();
        t.set_tooltip(tooltip);
    }
}

pub fn set_tray_menu(in_voice: bool, muted: bool) {
    let Some(t) = TRAY.get() else { return };
    if let Some(t) = t.lock().unwrap().as_ref() {
        t.set_menu(tray_menu(in_voice, muted));
    }
}

pub fn drop_tray() {
    if let Some(t) = TRAY.get() {
        t.lock().unwrap().take();
    }
}

// ---- notifications ----

/// Give the process its notification identity. Windows needs it before the first window
/// opens (it's the process's AppUserModelID), so this runs first thing in `main`.
pub fn register_notifications() {
    let dir = std::env::temp_dir().join("kall");
    std::fs::create_dir_all(&dir).ok();
    let icon = dir.join("icon.png");
    // Rewrite it whenever the logo changed (an old copy would keep showing the old icon).
    if let Ok(png) = icon_png(256)
        && std::fs::read(&icon).ok().as_deref() != Some(png.as_slice())
    {
        std::fs::write(&icon, png).ok();
    }
    if let Err(e) = rune_notify::register(&rune_notify::Identity::new("dev.kall.client", "kall").icon(icon)) {
        eprintln!("kall: notifications unavailable: {e}");
    }
}

pub fn notify(id: &str, title: &str, body: &str, channel: Id) {
    let body: String = body.chars().take(240).collect();
    let n = rune_notify::Notification::new(id, title).body(body).info("channel", channel.to_string());
    if !rune_notify::post(&n) {
        eprintln!("kall: the system refused the notification (available: {})", rune_notify::available());
    }
}

// ---- sounds outside calls ----

/// Play a cue on the default output device on a short-lived thread (no call running).
pub fn play_alone(cue: Cue, gain: f32) {
    std::thread::spawn(move || {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        let Some(dev) = cpal::default_host().default_output_device() else { return };
        let Ok(sc) = dev.default_output_config() else { return };
        if sc.sample_format() != cpal::SampleFormat::F32 {
            return; // the common case everywhere; quieter than wrong
        }
        let cfg = sc.config();
        let (rate, ch) = (cfg.sample_rate, cfg.channels as usize);
        let src = sounds::render(cue);
        // Resample the 48 kHz cue to the device rate.
        let mut out = Vec::new();
        crate::voice::resample::Resampler::new(48_000, rate).process(&src, &mut out);
        let len = out.len();
        let data = std::sync::Arc::new(out);
        let pos = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (d, p) = (data.clone(), pos.clone());
        let Ok(stream) = dev.build_output_stream(
            cfg,
            move |buf: &mut [f32], _| {
                for frame in buf.chunks_mut(ch) {
                    let i = p.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let v = d.get(i).copied().unwrap_or(0.0) * gain;
                    frame.fill(v);
                }
            },
            |_| {},
            None,
        ) else {
            return;
        };
        if stream.play().is_ok() {
            std::thread::sleep(std::time::Duration::from_secs_f64(len as f64 / rate as f64 + 0.15));
        }
    });
}

// ---- start at login ----

fn launcher() -> Result<auto_launch::AutoLaunch> {
    let exe = std::env::current_exe()?;
    auto_launch::AutoLaunchBuilder::new()
        .set_app_name("kall")
        .set_app_path(&exe.to_string_lossy())
        .set_args(&["--minimized"])
        .build()
        .map_err(|e| anyhow!("{e}"))
}

pub fn set_autostart(on: bool) -> Result<()> {
    let l = launcher()?;
    if on { l.enable() } else { l.disable() }.map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badge_changes_the_icon() {
        let plain = icon_rgba(32, None);
        let dot = icon_rgba(32, Some(0));
        let three = icon_rgba(32, Some(3));
        assert_eq!(plain.len(), 32 * 32 * 4);
        assert_ne!(plain, dot);
        assert_ne!(dot, three);
        // The badge corner is red.
        let o = (3 * 32 + 28) * 4;
        assert!(dot[o] > 200 && dot[o + 1] < 120, "{:?}", &dot[o..o + 4]);
    }

    #[test]
    fn levels_cycle_and_default_to_mentions() {
        let p = AppPrefs::default();
        assert_eq!(p.level(5), NotifyLevel::Mentions);
        assert_eq!(NotifyLevel::Mentions.next().next().next(), NotifyLevel::Mentions);
    }
}

#[cfg(test)]
mod icons {
    /// Regenerate `crates/client/icons/`: `cargo test -p kall write_icons -- --ignored`.
    #[test]
    #[ignore = "writes files"]
    fn write_icons() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("icons");
        std::fs::create_dir_all(&dir).unwrap();
        for size in [32u32, 128, 256, 512] {
            let img = image::RgbaImage::from_raw(size, size, super::icon_rgba(size, None)).unwrap();
            img.save(dir.join(format!("{size}x{size}.png"))).unwrap();
        }
        let img = image::RgbaImage::from_raw(256, 256, super::icon_rgba(256, None)).unwrap();
        img.save_with_format(dir.join("icon.ico"), image::ImageFormat::Ico).unwrap();
    }
}
