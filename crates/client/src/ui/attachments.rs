//! Files in the chat: attachments under a message (image previews, inline video and audio,
//! file cards with Download / Open / Show in folder), the files waiting in the composer, and
//! the full-size image viewer.

use kall_proto::Message;
use kall_proto::files::{Attachment, FileKind, human_size};
use rune::tok::type_;
use rune::{El, Phase, SurfaceFit, col, el, oklch, oklcha, row, text};

use super::{glyph, pal, round_button};
use crate::files::{DlState, Up};
use crate::{State, icons};

const MAX_W: f32 = 380.0;
const MAX_H: f32 = 280.0;

fn mmss(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// A thin progress bar, `frac` 0–1.
fn bar(frac: f32) -> El {
    let p = pal();
    el().w_full().h(4.0).radius(2.0).fill(p.ink(0.12)).child(el().h_full().w_frac(frac.clamp(0.0, 1.0)).radius(2.0).fill(p.accent))
}

fn frac(done: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { done as f32 / total as f32 }
}

/// An image's size on screen: its own proportions, within the chat's limits.
fn fitted(att: &Attachment) -> (f32, f32) {
    if att.width == 0 || att.height == 0 {
        return (MAX_W, 200.0);
    }
    let (w, h) = (att.width as f32, att.height as f32);
    let k = (MAX_W / w).min(MAX_H / h).min(1.0);
    ((w * k).max(48.0), (h * k).max(48.0))
}

/// The attachments under a message.
pub fn in_message(s: &State, msg: &Message) -> Option<El> {
    if msg.attachments.is_empty() {
        return None;
    }
    let mut c = col().gap(6.0).pt(4.0);
    for att in &msg.attachments {
        c = c.child(if att.expired {
            expired(att)
        } else {
            match att.kind {
                FileKind::Image if att.thumb => image(s, att),
                FileKind::Video => video(s, att),
                FileKind::Audio => audio(s, att),
                _ => file_card(s, att),
            }
        });
    }
    Some(c)
}

fn card(key: String) -> El {
    let p = pal();
    row().key(key).w(MAX_W).min_h(56.0).px(12.0).py(10.0).gap(12.0).center_y().radius(10.0).fill(p.surface).border(1.0, p.line)
}

fn name_and_size(att: &Attachment, sub: String) -> El {
    let p = pal();
    col()
        .grow(1.0)
        .shrink()
        .gap(2.0)
        .child(text(att.name.clone()).font(type_::TITLE).color(p.text).truncate())
        .child(text(sub).font(type_::LABEL).color(p.faint).truncate())
}

fn expired(att: &Attachment) -> El {
    let p = pal();
    card(format!("att-{}", att.id)).opacity(0.7).child(glyph(&icons::FILE, 22.0, p.faint)).child(name_and_size(att, format!("{} · expired", human_size(att.size))))
}

fn image(s: &State, att: &Attachment) -> El {
    let p = pal();
    let (w, h) = fitted(att);
    let mut e = el().key(format!("att-{}", att.id)).role("button").label(format!("Open {}", att.name)).size(w, h).radius(10.0).clip_rounded(10.0).fill(p.ink(0.06)).center();
    e = match s.files.thumbs.get(&att.id) {
        Some(px) => e.child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).pixels_fit(px, SurfaceFit::Fill)),
        None => e.child(glyph(&icons::WINDOW, 24.0, p.faint)),
    };
    let a = att.clone();
    e.hover_border(p.ink(0.3)).on_press(move |s: &mut State| s.view_image(&a))
}

/// Download progress or failure for a cached file, as a sub-line.
fn cache_line(s: &State, att: &Attachment) -> Option<String> {
    let d = s.files.cache.get(&att.id)?;
    match &d.state {
        DlState::Running => Some(format!("Loading {:.0}%", frac(d.done, d.total) * 100.0)),
        DlState::Failed(e) => Some(e.clone()),
        DlState::Done(_) => None,
    }
}

/// The play/pause button and seekable progress of a player.
fn transport(att: &Attachment, player: Option<&rune::Player>, light: bool) -> El {
    let p = pal();
    let (fg, track) = if light { (oklch(0.98, 0.0, 0.0), oklcha(1.0, 0.0, 0.0, 0.3)) } else { (p.text, p.ink(0.12)) };
    let playing = player.is_some_and(|p| p.state() == rune::PlayerState::Playing);
    let (t, dur) = player.map_or((0.0, None), |p| (p.time(), p.duration()));
    let f = dur.map_or(0.0, |d| if d > 0.0 { (t / d) as f32 } else { 0.0 });
    let a = att.clone();
    let btn = el()
        .key("play")
        .role("button")
        .label(if playing { "Pause" } else { "Play" })
        .size(34.0, 34.0)
        .radius(17.0)
        .center()
        .fill(p.accent)
        .press_scale(0.94)
        .child(glyph(if playing { &icons::PAUSE } else { &icons::PLAY }, 16.0, p.on_accent))
        .on_press(move |s: &mut State| s.toggle_play(&a));
    let id = att.id;
    let seek = el()
        .key("seek")
        .role("slider")
        .label("Position")
        .grow(1.0)
        .h(16.0)
        .child(el().pin(Some(0.0), Some(6.0), Some(0.0), None).h(4.0).radius(2.0).fill(track))
        .child(el().pin(Some(0.0), Some(6.0), None, None).h(4.0).radius(2.0).w_frac(f.clamp(0.0, 1.0)).fill(p.accent))
        .on_drag(move |s: &mut State, d, _| {
            if let Some(pl) = s.files.players.get(&id)
                && let Some(dur) = pl.duration()
                && d.w > 0.0
            {
                let to = (d.x / d.w).clamp(0.0, 1.0) as f64 * dur;
                if d.phase == Phase::Up { pl.seek(to) } else { pl.seek_fast(to) }
            }
        });
    let time = if player.is_some() { format!("{} / {}", mmss(t), dur.map_or("–".into(), mmss)) } else { String::new() };
    row().w_full().gap(10.0).center_y().child(btn).child(seek).child(text(time).font(type_::LABEL).color(if light { fg } else { p.faint }))
}

/// A player that couldn't play (no media support on this platform): offer the file instead.
fn cant_play(s: &State, att: &Attachment) -> Option<El> {
    let pl = s.files.players.get(&att.id)?;
    let rune::PlayerState::Failed(_) = pl.state() else { return None };
    let p = pal();
    let path = match s.files.cache.get(&att.id).map(|d| &d.state) {
        Some(DlState::Done(path)) => path.clone(),
        _ => return None,
    };
    Some(
        card(format!("att-{}", att.id))
            .child(glyph(&icons::FILE, 22.0, p.faint))
            .child(name_and_size(att, "Can't play this here".into()))
            .child(super::button("open", "Open", 0).h(30.0).on_press(move |s: &mut State| {
                if let Err(e) = crate::files::open_path(&path) {
                    s.toast(format!("Couldn't open it: {e}"));
                }
            })),
    )
}

fn video(s: &State, att: &Attachment) -> El {
    let p = pal();
    if let Some(e) = cant_play(s, att) {
        return e;
    }
    let player = s.files.players.get(&att.id);
    let (w, h) = match player.and_then(|pl| pl.natural_size()) {
        Some((vw, vh)) if vw > 0 && vh > 0 => {
            let k = (MAX_W / vw as f32).min(MAX_H / vh as f32);
            (vw as f32 * k, vh as f32 * k)
        }
        _ => (MAX_W, MAX_W * 9.0 / 16.0),
    };
    let mut frame = el().key(format!("att-{}", att.id)).w(w).h(h).radius(10.0).clip_rounded(10.0).fill(oklch(0.1, 0.0, 0.0)).center();
    match player {
        Some(pl) => frame = frame.child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).video(pl, SurfaceFit::Fit)),
        None => {
            let sub = cache_line(s, att).unwrap_or_else(|| human_size(att.size));
            frame = frame.child(
                col()
                    .gap(8.0)
                    .items_center()
                    .child(el().size(52.0, 52.0).radius(26.0).fill(p.accent).center().child(glyph(&icons::PLAY, 22.0, p.on_accent)))
                    .child(text(att.name.clone()).font(type_::TITLE).color(oklch(0.97, 0.0, 0.0)).truncate())
                    .child(text(sub).font(type_::LABEL).color(oklcha(1.0, 0.0, 0.0, 0.7))),
            );
            if let Some(d) = s.files.cache.get(&att.id)
                && matches!(d.state, DlState::Running)
            {
                frame = frame.child(el().pin(Some(12.0), None, Some(12.0), Some(12.0)).child(bar(frac(d.done, d.total))));
            }
        }
    }
    let a = att.clone();
    let frame = frame.role("button").label(format!("Play {}", att.name)).on_press(move |s: &mut State| s.toggle_play(&a));
    if player.is_some() {
        col().gap(6.0).w(w).child(frame).child(transport(att, player, false))
    } else {
        frame
    }
}

fn audio(s: &State, att: &Attachment) -> El {
    if let Some(e) = cant_play(s, att) {
        return e;
    }
    let p = pal();
    let player = s.files.players.get(&att.id);
    let sub = cache_line(s, att).unwrap_or_else(|| human_size(att.size));
    let mut c = col()
        .key(format!("att-{}", att.id))
        .w(MAX_W)
        .padding(12.0)
        .gap(8.0)
        .radius(10.0)
        .fill(p.surface)
        .border(1.0, p.line)
        .child(row().w_full().gap(10.0).center_y().child(glyph(&icons::MUSIC, 18.0, p.faint)).child(name_and_size(att, sub)))
        .child(transport(att, player, false));
    if let Some(d) = s.files.cache.get(&att.id)
        && matches!(d.state, DlState::Running)
    {
        c = c.child(bar(frac(d.done, d.total)));
    }
    c
}

/// Open and Show-in-folder buttons for a downloaded file.
fn open_buttons(path: std::path::PathBuf) -> El {
    let p = pal();
    let (a, b) = (path.clone(), path);
    row()
        .gap(4.0)
        .child(round_button("open", &icons::EXTERNAL, 32.0, false, p.muted, "Open").on_press(move |s: &mut State| {
            if let Err(e) = crate::files::open_path(&a) {
                s.toast(format!("Couldn't open it: {e}"));
            }
        }))
        .child(round_button("reveal", &icons::FOLDER, 32.0, false, p.muted, "Show in folder").on_press(move |s: &mut State| {
            if let Err(e) = crate::files::reveal_path(&b) {
                s.toast(format!("Couldn't show it: {e}"));
            }
        }))
}

fn file_card(s: &State, att: &Attachment) -> El {
    let p = pal();
    let saved = s.files.saved.get(&att.id);
    let (sub, right): (String, El) = match saved.map(|d| (&d.state, d.done, d.total)) {
        Some((DlState::Running, done, total)) => {
            let id = att.id;
            (
                format!("{} of {}", human_size(done), human_size(total)),
                round_button("cancel", &icons::X, 32.0, false, p.muted, "Cancel").on_press(move |s: &mut State| s.cancel_download(id)),
            )
        }
        Some((DlState::Done(path), _, _)) => (format!("{} · saved to {}", human_size(att.size), path.parent().map_or(String::new(), |d| d.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()))), open_buttons(path.clone())),
        other => {
            let a = att.clone();
            let failed = matches!(other, Some((DlState::Failed(_), _, _)));
            (
                if failed { "Download failed".into() } else { human_size(att.size) },
                round_button("download", &icons::DOWNLOAD, 32.0, false, p.accent2, "Download").on_press(move |s: &mut State| s.save_file(&a)),
            )
        }
    };
    let mut c = col().key(format!("att-{}", att.id)).w(MAX_W).gap(8.0).padding(12.0).radius(10.0).fill(p.surface).border(1.0, p.line).child(
        row().w_full().gap(12.0).center_y().child(glyph(&icons::FILE, 24.0, p.accent2)).child(name_and_size(att, sub)).child(right),
    );
    if let Some(d) = saved
        && matches!(d.state, DlState::Running)
    {
        c = c.child(bar(frac(d.done, d.total)));
    }
    c
}

/// The files waiting to go out with the next message, above the composer.
pub fn chips(s: &State) -> Option<El> {
    if s.files.pending.is_empty() {
        return None;
    }
    let p = pal();
    let mut r = row().key("attach-chips").w_full().gap(8.0).flex_wrap().pb(8.0);
    for pd in &s.files.pending {
        let job = pd.job;
        let (sub, color, prog) = match &pd.state {
            Up::Sending => (format!("Uploading {:.0}%", frac(pd.done, pd.size) * 100.0), p.faint, Some(frac(pd.done, pd.size))),
            Up::Ready(_) => (human_size(pd.size), p.faint, None),
            Up::Failed(e) => (e.clone(), p.red, None),
        };
        let mut info = col().grow(1.0).shrink().gap(2.0).child(text(pd.name.clone()).font(type_::TITLE).color(p.text).truncate()).child(text(sub).font(type_::LABEL).color(color).truncate());
        if let Some(f) = prog {
            info = info.child(bar(f).mt(2.0));
        }
        let ico = match FileKind::of(&pd.name) {
            FileKind::Image => &icons::WINDOW,
            FileKind::Audio => &icons::MUSIC,
            FileKind::Video => &icons::CAMERA,
            FileKind::Other => &icons::FILE,
        };
        r = r.child(
            row()
                .key(format!("chip-{job}"))
                .w(240.0)
                .h(56.0)
                .pl(12.0)
                .pr(6.0)
                .gap(10.0)
                .center_y()
                .radius(10.0)
                .fill(p.surface)
                .border(1.0, if matches!(pd.state, Up::Failed(_)) { p.red.alpha(0.5) } else { p.line })
                .child(glyph(ico, 18.0, p.faint))
                .child(info)
                .child(round_button("remove", &icons::X, 26.0, false, p.muted, "Remove").on_press(move |s: &mut State| s.remove_pending(job))),
        );
    }
    Some(r)
}

/// The image open full size, over everything.
pub fn viewer(s: &State) -> Option<El> {
    let att = s.files.viewer.as_ref()?;
    let p = pal();
    let full = s.files.full.lock().unwrap().get(&att.id).cloned();
    let px = full.or_else(|| s.files.thumbs.get(&att.id).cloned());
    let mut stage = el().key("viewer-image").grow(1.0).w_full().min_h(0.0);
    if let Some(px) = &px {
        stage = stage.child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).pixels_fit(px, SurfaceFit::Fit));
    }
    let loading = s.files.cache.get(&att.id).filter(|d| matches!(d.state, DlState::Running)).map(|d| frac(d.done, d.total));
    let a = att.clone();
    let saved = s.files.saved.get(&att.id).and_then(|d| if let DlState::Done(p) = &d.state { Some(p.clone()) } else { None });
    let mut top = row()
        .w_full()
        .gap(10.0)
        .center_y()
        .child(
            col()
                .grow(1.0)
                .shrink()
                .child(text(att.name.clone()).font(type_::TITLE).color(oklch(0.97, 0.0, 0.0)).truncate())
                .child(text(format!("{} × {} · {}", att.width, att.height, human_size(att.size))).font(type_::LABEL).color(oklcha(1.0, 0.0, 0.0, 0.6))),
        );
    top = match saved {
        Some(path) => top.child(open_buttons(path)),
        None => top.child(super::button("viewer-save", "Download", 1).h(32.0).on_press(move |s: &mut State| s.save_file(&a))),
    };
    top = top.child(round_button("viewer-close", &icons::X, 34.0, false, oklch(0.97, 0.0, 0.0), "Close").on_press(|s: &mut State| s.files.viewer = None));
    let mut panel = col().w_full().h_full().padding(24.0).gap(14.0).child(top).child(stage);
    if let Some(f) = loading {
        panel = panel.child(row().w_full().justify_center().child(el().w(240.0).child(bar(f))));
    }
    let _ = p;
    Some(
        el().key("viewer")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .fill(oklcha(0.0, 0.0, 0.0, 0.88))
            .transition(rune::Transition::opacity())
            .child(panel.on_press(|_: &mut State| {})),
    )
}
