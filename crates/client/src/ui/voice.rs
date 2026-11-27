//! A voice channel: tiles for everyone in the call (camera video when it's on, LIVE badges for
//! screen shares), a stage for the screen share you're watching, and the call controls.


use kall_proto::{C2S, Channel, Id, StreamKind, VoiceMember};
use rune::tok::type_;
use rune::{El, SurfaceFit, TextStyle, Transition, col, el, oklch, oklcha, row, text};

use super::{HEADER, SOFT, accent, avatar, badge, glyph, hue, pal, panel};
use crate::model::Model;
use crate::{State, icons};

pub fn view(s: &State, m: &Model, ch: &Channel) -> El {
    let p = pal();
    let mut people: Vec<&VoiceMember> = m.voice.values().filter(|v| v.channel == ch.id).collect();
    people.sort_by_key(|v| (v.user != m.me, v.user));
    let here = s.in_voice_here(Some(ch.id));

    let mut header = row()
        .w_full()
        .h(52.0)
        .px(18.0)
        .gap(10.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], p.line)
        .child(glyph(&icons::SPEAKER, 20.0, p.accent2))
        .child(text(ch.name.clone()).font(HEADER).color(p.text));
    if !people.is_empty() {
        header = header.child(
            row()
                .key("count")
                .px(8.0)
                .h(20.0)
                .radius(10.0)
                .fill(p.green.alpha(0.12))
                .center()
                .child(text(format!("{} connected", people.len())).font(type_::LABEL).color(p.green)),
        );
    }
    header = header.child(el().grow(1.0)).child(text(format!("{} kbps", ch.bitrate / 1000)).font(type_::LABEL).color(p.faint));

    let body = if let Some(v) = here {
        let focus = s.media.focus.filter(|u| m.streams.contains_key(&(*u, StreamKind::Screen)) || (*u == m.me && s.media.sharing.is_some()));
        let main = match focus {
            Some(u) => stage(s, m, u, &people),
            None => grid(s, m, &people),
        };
        let note = row()
            .key("note")
            .gap(8.0)
            .px(12.0)
            .h(30.0)
            .center_y()
            .radius(15.0)
            .fill(p.ink(0.05))
            .child(text(hint(s)).font(type_::LABEL).color(p.faint));
        col()
            .w_full()
            .grow(1.0)
            .min_h(0.0)
            .px(18.0)
            .pt(18.0)
            .gap(14.0)
            .center()
            .child(main)
            .child(note)
            .child(controls(s, v))
    } else {
        lobby(m, ch, &people)
    };
    panel(col().grow(1.0).shrink().h_full()).child(header).child(body)
}

fn grid(s: &State, m: &Model, people: &[&VoiceMember]) -> El {
    let cols = if people.len() <= 1 { 1 } else if people.len() <= 4 { 2 } else { 3 };
    let mut grid = col().key("grid").w_full().max_w(1100.0).gap(14.0);
    for (r, chunk) in people.chunks(cols).enumerate() {
        let mut line = row().key(format!("grid-row-{r}")).w_full().gap(14.0);
        for v in chunk {
            let cell = tile(s, m, v, 64.0).w_full().aspect_ratio(16.0 / 9.0);
            line = line.child(el().key(format!("cell-{}", v.user)).grow(1.0).child(cell));
        }
        for k in chunk.len()..cols {
            line = line.child(el().key(format!("pad-{k}")).grow(1.0));
        }
        grid = grid.child(line);
    }
    grid
}

/// A screen share big, with everyone in a strip underneath.
fn stage(s: &State, m: &Model, user: Id, people: &[&VoiceMember]) -> El {
    let p = pal();
    let mine = user == m.me;
    let label = m.streams.get(&(user, StreamKind::Screen)).map(|i| i.label.clone()).or_else(|| s.media.sharing.as_ref().map(|sh| sh.info.label.clone())).unwrap_or_default();
    let title = if mine { format!("You're sharing {label}") } else { format!("{}'s screen · {label}", m.name(user)) };
    let mut video = el().key("stage-video").w_full().grow(1.0).min_h(0.0).radius(14.0).clip_rounded(14.0).fill(oklch(0.08, 0.0, 0.0)).center();
    match s.stream_pixels(user, StreamKind::Screen) {
        Some(px) => video = video.child(el().key("stage-pixels").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).pixels_fit(&px, SurfaceFit::Fit)),
        None => video = video.child(text("Waiting for the picture…").font(type_::BODY).color(p.faint)),
    }
    video = video.child(badge("LIVE", p.red).pin(Some(12.0), Some(12.0), None, None));
    let action = if mine {
        super::button("stop-share", "Stop sharing", 2).h(30.0).on_press(|s: &mut State| s.stop_share())
    } else {
        super::button("stop-watching", "Stop watching", 0).h(30.0).on_press(|s: &mut State| s.focus_stream(None))
    };
    let mut strip = row().key("strip").w_full().h(110.0).gap(10.0).justify_center();
    for v in people {
        strip = strip.child(tile(s, m, v, 40.0).h_full().aspect_ratio(16.0 / 9.0));
    }
    col()
        .key("stage")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .gap(10.0)
        .child(row().w_full().gap(10.0).center_y().child(text(title).font(type_::TITLE).color(p.text).grow(1.0).truncate()).child(action))
        .child(video)
        .child(strip)
}

fn lobby(m: &Model, ch: &Channel, people: &[&VoiceMember]) -> El {
    let p = pal();
    let channel = ch.id;
    let mut faces = row().gap(-10.0);
    for v in people {
        faces = faces.child(el().key(format!("face-{}", v.user)).radius(31.0).border(3.0, p.panel).child(avatar(m, v.user, 56.0, false)));
    }
    let join = accent(el().key("join").role("button").px(28.0).h(46.0).radius(23.0).fill(p.accent))
        .press_scale(0.95)
        .center()
        .child(text("Join voice").font(TextStyle::new(14.0, 600.0)).color(p.on_accent))
        .on_press(move |s: &mut State| s.join_voice(channel));
    let who = if people.is_empty() {
        "No one's here yet — be the first.".to_string()
    } else {
        let names: Vec<&str> = people.iter().map(|v| m.name(v.user)).collect();
        format!("{} {} hanging out", names.join(", "), if names.len() == 1 { "is" } else { "are" })
    };
    col()
        .w_full()
        .grow(1.0)
        .center()
        .gap(14.0)
        .child(faces)
        .child(text(ch.name.clone()).font(TextStyle::new(28.0, 750.0)).color(p.text).pt(6.0))
        .child(text(who).font(type_::BODY).color(p.muted))
        .child(el().h(6.0))
        .child(join)
}

#[derive(Clone, Copy, PartialEq)]
enum Tone {
    Plain,
    Active,
    Danger,
}

fn call_button(key: &'static str, i: &'static rune::Icon, tone: Tone, label: &'static str) -> El {
    let p = pal();
    let b = el()
        .key(key)
        .role("button")
        .label(label)
        .help(label)
        .size(52.0, 52.0)
        .radius(26.0)
        .press_scale(0.9)
        .motion(SOFT)
        .center()
        .child(glyph(i, 22.0, match tone {
            Tone::Danger => oklch(0.99, 0.0, 0.0),
            Tone::Active => p.on_accent,
            Tone::Plain => p.text,
        }));
    match tone {
        Tone::Danger => b.fill(p.red),
        Tone::Active => b.fill(p.accent),
        Tone::Plain => b.fill(p.ink(0.07)).hover_fill(p.ink(0.12)),
    }
}

fn controls(s: &State, v: &VoiceMember) -> El {
    let p = pal();
    let (muted, deaf) = (v.muted, v.deafened);
    let tone = |on: bool, t: Tone| if on { t } else { Tone::Plain };
    let cam_on = s.media.camera.is_some();
    let sharing = s.media.sharing.is_some();
    row()
        .key("dock")
        .gap(12.0)
        .padding(10.0)
        .mb(18.0)
        .center_y()
        .radius(36.0)
        .fill(p.surface)
        .border(1.0, p.line)
        .child(super::audio_menu::mic(call_button("mic", if muted { &icons::MIC_OFF } else { &icons::MIC }, tone(muted, Tone::Danger), "Mute (right-click for devices and volume)").on_press(move |s: &mut State| {
            s.send(C2S::VoiceState { muted: !muted, deafened: deaf && !muted });
        })))
        .child(super::audio_menu::speakers(
            call_button("deafen", if deaf { &icons::HEADPHONES_OFF } else { &icons::HEADPHONES }, tone(deaf, Tone::Danger), "Deafen (right-click for devices and volume)")
                .on_press(move |s: &mut State| s.send(C2S::VoiceState { muted: !deaf || muted, deafened: !deaf })),
        ))
        .child(call_button("camera", &icons::CAMERA, tone(cam_on, Tone::Active), "Camera").on_press(|s: &mut State| s.toggle_camera()))
        .child(call_button("share", &icons::SCREEN, tone(sharing, Tone::Active), "Share your screen").on_press(move |s: &mut State| {
            if sharing { s.stop_share() } else { s.open_share_picker() }
        }))
        .child(el().w(1.0).h(28.0).fill(p.line))
        .child(call_button("leave", &icons::PHONE_OFF, Tone::Danger, "Disconnect").on_press(|s: &mut State| s.leave_voice()))
}

fn hint(s: &State) -> String {
    match s.voice.mode {
        crate::voice::core::Mode::PushToTalk => {
            let key = crate::voice::PTT_KEYS.iter().find(|(k, _)| *k == s.voice.ptt_key).map_or(s.voice.ptt_key.as_str(), |(_, n)| n);
            format!("Push to talk: hold {key}")
        }
        _ => "Voice activity: just talk".into(),
    }
}

/// −  100%  + on a friend's tile while hovered.
fn volume_pill(s: &State, u: Id) -> El {
    let p = pal();
    let v = s.volume_of(u);
    row()
        .key("volume")
        .gap(2.0)
        .padding(3.0)
        .center_y()
        .radius(14.0)
        .fill(oklcha(0.1, 0.02, 280.0, 0.6))
        .backdrop_blur(10.0)
        .child(super::round_button("vol-down", &icons::DOWN, 24.0, false, p.text, "Quieter").on_press(move |s: &mut State| {
            let v = s.volume_of(u);
            s.set_volume(u, v - 0.1);
        }))
        .child(text(format!("{:.0}%", v * 100.0)).font(type_::LABEL).color(p.text).w(42.0).center())
        .child(super::round_button("vol-up", &icons::UP, 24.0, false, p.text, "Louder").on_press(move |s: &mut State| {
            let v = s.volume_of(u);
            s.set_volume(u, v + 0.1);
        }))
}

fn tile(s: &State, m: &Model, v: &VoiceMember, face: f32) -> El {
    let p = pal();
    let u: Id = v.user;
    let h = hue(m, u);
    let talking = s.speaking.contains(&u);
    let screen = m.streams.contains_key(&(u, StreamKind::Screen)) || (u == m.me && s.media.sharing.is_some());
    // Over video the name sits on a dark pill; on a plain tile, on the panel colour.
    let on_video = m.streams.contains_key(&(u, StreamKind::Camera)) || (u == m.me && s.media.camera.is_some());
    let on_video = on_video || screen;
    let (pill, pill_text) = if on_video { (oklcha(0.1, 0.0, 0.0, 0.55), oklch(0.97, 0.0, 0.0)) } else { (p.panel, p.text) };
    let mut name = row().gap(6.0).center_y().px(9.0).h(24.0).radius(6.0).fill(pill);
    if v.deafened {
        name = name.child(glyph(&icons::HEADPHONES_OFF, 13.0, p.red));
    } else if v.muted {
        name = name.child(glyph(&icons::MIC_OFF, 13.0, p.red));
    }
    name = name.child(text(if u == m.me { "you".to_string() } else { m.name(u).to_string() }).font(type_::LABEL).color(pill_text));
    let mut t = el()
        .key(format!("tile-{u}"))
        .radius(14.0)
        .clip_rounded(14.0)
        .center()
        .border(1.0, p.ink(0.08))
        .transition(Transition::scale(0.94))
        .fill(if p.dark { oklch(0.25, 0.02, h) } else { oklch(0.94, 0.02, h) })
        .on_hover_change(move |s: &mut State, inside, _| {
            if inside {
                s.hover_tile = Some(u);
            } else if s.hover_tile == Some(u) {
                s.hover_tile = None;
            }
        });
    let cam = (m.streams.contains_key(&(u, StreamKind::Camera)) || (u == m.me && s.media.camera.is_some())).then(|| s.stream_pixels(u, StreamKind::Camera)).flatten();
    // Video stops short of the edge while they talk, so the speaking ring stays visible.
    let inset = if talking { 2.5 } else { 0.0 };
    if screen {
        // A screen share takes over the tile: its live preview, a Watch button, and the
        // camera (if on) as a small picture in the corner.
        t = t.fill(oklch(0.12, 0.0, 0.0));
        t = match s.stream_preview(u) {
            Some(px) => t.child(el().key("screen").pin(Some(inset), Some(inset), Some(inset), Some(inset)).pixels_fit(&px, SurfaceFit::Fit)),
            None => t.child(glyph(&icons::SCREEN, 40.0, oklcha(1.0, 0.0, 0.0, 0.35))),
        };
        if let Some(px) = cam {
            t = t.child(el().key("cam").w(112.0).h(63.0).radius(8.0).clip_rounded(8.0).border(1.5, oklcha(1.0, 0.0, 0.0, 0.25)).pixels_fit(&px, SurfaceFit::Fill).pin(Some(10.0), Some(10.0), None, None));
        }
        let cta = if u == m.me { "You're sharing" } else { "Watch stream" };
        let pill = row()
            .key("watch")
            .gap(8.0)
            .center_y()
            .px(14.0)
            .h(36.0)
            .radius(18.0)
            .fill(if u == m.me { oklcha(0.1, 0.0, 0.0, 0.6) } else { p.accent })
            .child(glyph(&icons::SCREEN, 16.0, if u == m.me { oklch(0.97, 0.0, 0.0) } else { p.on_accent }))
            .child(text(cta).font(type_::TITLE).color(if u == m.me { oklch(0.97, 0.0, 0.0) } else { p.on_accent }));
        t = t
            .child(row().key("watch-row").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center().child(pill))
            .child(badge("LIVE", p.red).pin(None, None, Some(10.0), Some(10.0)))
            .role("button")
            .label(format!("Watch {}'s screen", m.name(u)))
            .hover_border(p.ink(0.3))
            .on_press(move |s: &mut State| s.focus_stream(Some(u)));
    } else {
        t = match cam {
            Some(px) => t.fill(oklch(0.08, 0.0, 0.0)).child(el().key("cam").pin(Some(inset), Some(inset), Some(inset), Some(inset)).radius(12.0).clip_rounded(12.0).pixels_fit(&px, SurfaceFit::Fill)),
            None => t.child(avatar(m, u, face, false)),
        };
    }
    t = t.child(name.pin(Some(10.0), None, None, Some(10.0)));
    if talking {
        t = t.border(2.5, p.accent);
    }
    if u != m.me && (s.hover_tile == Some(u) || (s.volume_of(u) - 1.0).abs() > 0.01) {
        t = t.child(volume_pill(s, u).pin(None, Some(10.0), Some(10.0), None));
    }
    t
}

/// Choose what to share: tabs for screens and windows, a grid of live thumbnails, and the
/// quality and sound in the footer.
pub fn picker(s: &State) -> Option<El> {
    use crate::video::capture::SourceKind;
    let p = pal();
    let pk = s.media.picker.as_ref()?;
    let sources = pk.sources.as_deref().unwrap_or_default();
    let count = |k: &SourceKind| sources.iter().filter(|s| &s.kind == k).count();

    let tab = |key: &'static str, title: String, kind: SourceKind| {
        let on = pk.tab == kind;
        let t = el()
            .key(key)
            .role("tab")
            .h(32.0)
            .px(16.0)
            .radius(8.0)
            .center()
            .child(text(title).font(type_::TITLE).color(if on { p.text } else { p.muted }))
            .on_press(move |s: &mut State| {
                if let Some(pk) = &mut s.media.picker {
                    pk.selected = pk.sources.as_ref().and_then(|ss| ss.iter().find(|s| s.kind == kind)).map(|s| s.id.clone());
                    pk.tab = kind.clone();
                }
            });
        if on { t.fill(if p.dark { p.ink(0.1) } else { p.panel }).shadow(1.0, 4.0, oklcha(0.0, 0.0, 0.0, if p.dark { 0.3 } else { 0.08 })) } else { t.hover_fill(p.hover) }
    };
    let tabs = row()
        .padding(3.0)
        .gap(3.0)
        .radius(10.0)
        .fill(p.ink(0.05))
        .child(tab("tab-screens", format!("Screens  {}", count(&SourceKind::Display)), SourceKind::Display))
        .child(tab("tab-windows", format!("Windows  {}", count(&SourceKind::Window)), SourceKind::Window));

    let mut grid = row().w_full().gap(12.0).flex_wrap();
    let shown: Vec<_> = sources.iter().filter(|s| s.kind == pk.tab).collect();
    if pk.sources.is_none() {
        grid = grid.child(text("Looking for screens and windows…").font(type_::BODY).color(p.faint).padding(12.0));
    } else if shown.is_empty() {
        grid = grid.child(text(if pk.tab == SourceKind::Display { "No screens found." } else { "No windows to share." }).font(type_::BODY).color(p.faint).padding(12.0));
    }
    let thumbs = pk.thumbs.lock().unwrap();
    for src in shown {
        let on = pk.selected.as_deref() == Some(src.id.as_str());
        let id = src.id.clone();
        let ico = if src.kind == SourceKind::Display { &icons::SCREEN } else { &icons::WINDOW };
        let mut shot = el().w(244.0).h(137.0).radius(10.0).clip_rounded(10.0).fill(p.ink(0.06)).center();
        shot = match thumbs.get(&src.id) {
            Some(px) => shot.fill(oklch(0.1, 0.0, 0.0)).child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).pixels_fit(px, SurfaceFit::Fit)),
            None => shot.child(glyph(ico, 28.0, p.faint)),
        };
        shot = if on { shot.border(2.5, p.accent) } else { shot.border(1.0, p.line).hover_border(p.ink(0.3)) };
        let card = col()
            .key(format!("src-{}", src.id))
            .role("button")
            .label(src.name.clone())
            .w(244.0)
            .gap(6.0)
            .press_scale(0.98)
            .child(shot)
            .child(
                row().w_full().gap(6.0).center_y().child(glyph(ico, 14.0, if on { p.accent2 } else { p.faint })).child(
                    text(src.name.clone()).font(type_::TITLE).color(if on { p.text } else { p.muted }).grow(1.0).shrink().truncate(),
                ),
            )
            .children((!src.detail.is_empty()).then(|| text(src.detail.clone()).font(type_::LABEL).color(p.faint).truncate()))
            .on_press(move |s: &mut State| {
                if let Some(pk) = &mut s.media.picker {
                    pk.selected = Some(id.clone());
                }
            });
        grid = grid.child(card);
    }
    drop(thumbs);

    let mut qualities = row().padding(3.0).gap(3.0).radius(10.0).fill(p.ink(0.05));
    for (i, q) in crate::video::QUALITIES.iter().enumerate() {
        let on = pk.quality == i;
        let c = el().key(format!("q-{i}")).role("button").label(q.label).h(30.0).px(12.0).radius(8.0).center().child(text(q.label).font(type_::LABEL).color(if on { p.on_accent } else { p.muted }));
        let c = if on { c.fill(p.accent) } else { c.hover_fill(p.hover) };
        qualities = qualities.child(c.on_press(move |s: &mut State| {
            if let Some(pk) = &mut s.media.picker {
                pk.quality = i;
            }
        }));
    }

    let audio_ok = crate::video::share_audio::supported();
    let src = pk.source();
    let sound_label = match src {
        _ if !audio_ok => "Sharing sound isn't supported here yet".to_string(),
        Some(s) if s.kind == SourceKind::Window && !s.detail.is_empty() => format!("Share {}'s sound", s.detail),
        Some(s) if s.kind == SourceKind::Window => "Share the app's sound".to_string(),
        _ => "Share computer sound (except kall)".to_string(),
    };
    let audio = row()
        .key("share-audio")
        .role("switch")
        .label(sound_label.clone())
        .checked(pk.audio)
        .gap(10.0)
        .center_y()
        .shrink()
        .opacity(if audio_ok { 1.0 } else { 0.5 })
        .child(
            el().size(36.0, 20.0)
                .radius(10.0)
                .fill(if pk.audio { p.accent } else { p.ink(0.15) })
                .motion(SOFT)
                .child(el().size(16.0, 16.0).radius(8.0).fill(oklch(0.98, 0.0, 0.0)).absolute(if pk.audio { 18.0 } else { 2.0 }, 2.0).motion(SOFT)),
        )
        .child(text(sound_label).font(type_::BODY).color(p.text).truncate())
        .on_press(move |s: &mut State| {
            if let Some(pk) = &mut s.media.picker
                && audio_ok
            {
                pk.audio = !pk.audio;
            }
        });

    let can_go = src.is_some();
    let mut go = super::button("go-live", "Go live", 1).opacity(if can_go { 1.0 } else { 0.5 });
    if can_go {
        go = go.on_press(|s: &mut State| s.start_share());
    }
    let footer = col()
        .w_full()
        .gap(12.0)
        .pt(14.0)
        .border_sides([1.0, 0.0, 0.0, 0.0], p.line)
        .child(row().w_full().gap(12.0).center_y().child(super::label("QUALITY")).child(qualities).child(el().grow(1.0)))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(audio)
                .child(el().grow(1.0))
                .child(super::button("cancel-share", "Cancel", 0).on_press(|s: &mut State| s.media.picker = None))
                .child(go),
        );
    let sheet = panel(col().w(820.0).h(620.0).padding(22.0).gap(16.0))
        .radius(14.0)
        .border(1.0, p.line)
        .shadow(12.0, 40.0, oklcha(0.0, 0.0, 0.0, if p.dark { 0.4 } else { 0.18 }))
        .transition(Transition::scale(0.97))
        .child(
            row()
                .w_full()
                .gap(16.0)
                .center_y()
                .child(text("Share your screen").font(TextStyle::new(17.0, 650.0)).color(p.text))
                .child(tabs)
                .child(el().grow(1.0))
                .child(super::round_button("close-picker", &icons::X, 32.0, false, p.muted, "Cancel").on_press(|s: &mut State| s.media.picker = None)),
        )
        .child(col().key("sources").w_full().grow(1.0).min_h(0.0).scroll(rune::ScrollSpec::default()).scroll_edge_fade(12.0).child(grid))
        .child(footer)
        .on_press(|_: &mut State| {});
    Some(
        el().key("picker")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .fill(oklcha(0.0, 0.0, 0.0, if p.dark { 0.5 } else { 0.25 }))
            .center()
            .on_press(|s: &mut State| s.media.picker = None)
            .child(sheet),
    )
}
