//! Settings overlay: your profile, and for admins/mods the server's channels and invites.

use kall_proto::{C2S, ChannelKind, Status};
use rune::tok::type_;
use rune::{El, FileDialogOptions, TextEdit, TextStyle, Transition, col, el, oklch, oklcha, row, text, text_input};

use crate::desktop::ThemeChoice;

use super::{BODY, SMALL, avatar, button, field, glyph, label, pal, panel, round_button};
use crate::model::Model;
use crate::net::NetCmd;
use crate::voice::PTT_KEYS;
use crate::voice::core::Mode;
use crate::{Settings, State, Tab, icons};

pub fn open(s: &mut State, tab: Tab) {
    let Some(me) = s.m.as_ref().and_then(|m| m.me()) else { return };
    s.settings = Some(Settings {
        tab,
        display_name: TextEdit::new(me.display_name.clone()),
        status: me.status,
        status_text: TextEdit::new(me.status_text.clone()),
        new_channel: TextEdit::new(""),
        new_kind: ChannelKind::Text,
        rename: None,
        confirm_delete: None,
        invite_uses: Some(1),
        invite_hours: Some(24 * 7),
        avatar: None,
    });
    if tab == Tab::Invites {
        s.send(C2S::ListInvites);
    }
    if tab == Tab::Voice {
        s.refresh_devices();
    }
}

fn st(s: &mut State) -> &mut Settings {
    s.settings.as_mut().expect("settings open")
}

fn nav(key: &'static str, title: &'static str, tab: Tab, on: bool) -> El {
    let p = pal();
    let n = row()
        .key(key)
        .role("tab")
        .w_full()
        .h(34.0)
        .px(12.0)
        .center_y()
        .radius(8.0)
        .hover_fill(p.hover)
        .child(text(title).font(type_::TITLE).color(if on { p.text } else { p.muted }))
        .on_press(move |s: &mut State| {
            st(s).tab = tab;
            if tab == Tab::Invites {
                s.send(C2S::ListInvites);
            }
            if tab == Tab::Voice {
                s.refresh_devices();
            }
        });
    if on { n.fill(p.select) } else { n }
}

fn chip(key: impl Into<String>, label: impl Into<String>, on: bool) -> El {
    let p = pal();
    let c = el().key(key.into()).role("button").h(30.0).px(12.0).radius(15.0).center().press_scale(0.96).child(text(label.into()).font(type_::LABEL).color(if on { p.on_accent } else { p.muted }));
    if on { c.fill(p.accent) } else { c.fill(p.ink(0.06)).hover_fill(p.ink(0.1)) }
}

fn profile(m: &Model, set: &Settings) -> El {
    let p = pal();
    // A picked avatar shows here until the profile is saved (or the pick is undone).
    let shown = match &set.avatar {
        Some((_, px)) => el().key("avatar-new").size(80.0, 80.0).radius(40.0).clip_rounded(40.0).pixels(px),
        None => avatar(m, m.me, 80.0, false),
    };
    let hint = if set.avatar.is_some() { "New avatar: press Save to use it." } else { "PNG, JPEG or WebP. It's cropped to a square." };
    let mut actions = row().gap(8.0).child(button("avatar", "Change avatar", 0).on_press_tx(|_: &mut State, tx| {
        let opts = FileDialogOptions { title: "Choose an avatar".into(), ..Default::default() }.filter("Images", &["png", "jpg", "jpeg", "webp"]);
        tx.open_file(opts, |s: &mut State, res, _| match res {
            Ok(Some(path)) => s.cmd(NetCmd::PrepareAvatar(path)),
            Ok(None) => {}
            Err(e) => s.toast(format!("Couldn't open the file dialog: {e}")),
        });
    }));
    if set.avatar.is_some() {
        actions = actions.child(button("avatar-undo", "Undo", 0).on_press(|s: &mut State| st(s).avatar = None));
    }
    let pic = row()
        .gap(16.0)
        .center_y()
        .child(shown)
        .child(
            col()
                .gap(8.0)
                .child(text("Avatar").font(type_::TITLE).color(p.text))
                .child(text(hint).font(type_::LABEL).color(if set.avatar.is_some() { p.accent2 } else { p.faint }))
                .child(actions),
        );
    let mut statuses = row().gap(6.0);
    for (st_, name) in [(Status::Online, "Online"), (Status::Idle, "Idle"), (Status::Dnd, "Do not disturb")] {
        statuses = statuses.child(chip(format!("status-{name}"), name, set.status == st_).on_press(move |s: &mut State| st(s).status = st_));
    }
    col()
        .w_full()
        .gap(10.0)
        .child(pic)
        .child(label("DISPLAY NAME").pt(10.0))
        .child(field(text_input(&set.display_name, BODY).key("display-name").label("Display name").w_full().on_edit(|s: &mut State| &mut st(s).display_name)))
        .child(label("STATUS").pt(6.0))
        .child(statuses)
        .child(
            field(
                text_input(&set.status_text, BODY)
                    .key("status-text")
                    .label("Custom status")
                    .placeholder("What are you up to?", p.faint)
                    .w_full()
                    .on_edit(|s: &mut State| &mut st(s).status_text),
            ),
        )
        .child(row().w_full().pt(8.0).gap(8.0).child(el().grow(1.0)).child(button("save-profile", "Save", 1).on_press(|s: &mut State| {
            let set = st(s);
            let msg = C2S::SetProfile { display_name: set.display_name.text.clone(), status: set.status, status_text: set.status_text.text.clone() };
            let avatar = set.avatar.take().map(|(png, _)| png);
            s.send(msg);
            if let Some(png) = avatar {
                s.send(C2S::SetAvatar { png });
            }
            s.toast("Profile saved.");
        })))
        .child(el().h(1.0).w_full().fill(p.line).my(8.0))
        .child(row().w_full().gap(8.0).center_y().child(text("Log out of kall on this computer.").font(type_::BODY).color(p.muted).grow(1.0)).child(
            button("logout", "Log out", 2).on_press(|s: &mut State| {
                s.cmd(NetCmd::Logout);
                s.settings = None;
            }),
        ))
}

fn channels(m: &Model, set: &Settings) -> El {
    let p = pal();
    let mut list = col().w_full().gap(4.0);
    let ordered = m.channels.iter().filter(|c| c.kind == ChannelKind::Text).chain(m.channels.iter().filter(|c| c.kind == ChannelKind::Voice));
    for ch in ordered {
        let id = ch.id;
        let ico = if ch.kind == ChannelKind::Text { &icons::HASH } else { &icons::SPEAKER };
        let mut r = row().key(format!("mc-{id}")).w_full().h(42.0).px(8.0).gap(8.0).center_y().radius(10.0).fill(p.ink(0.03)).child(glyph(ico, 16.0, p.faint));
        match &set.rename {
            Some((rid, edit)) if *rid == id => {
                let bitrate = ch.bitrate;
                r = r
                    .child(field(text_input(edit, BODY).key("rename").label("Channel name").grow(1.0).h(32.0).on_edit(|s: &mut State| &mut st(s).rename.as_mut().unwrap().1).on_submit(move |s: &mut State, _| save_rename(s, bitrate))))
                    .child(button("rename-save", "Save", 1).h(30.0).on_press(move |s: &mut State| save_rename(s, bitrate)));
            }
            _ => {
                let name = ch.name.clone();
                r = r.child(text(ch.name.clone()).font(type_::TITLE).color(p.text).grow(1.0).truncate());
                if ch.kind == ChannelKind::Voice {
                    let (n2, b) = (ch.name.clone(), ch.bitrate);
                    let (n3, b3) = (n2.clone(), b);
                    r = r
                        .child(round_button("br-down", &icons::DOWN, 26.0, false, p.muted, "Lower bitrate").on_press(move |s: &mut State| {
                            s.send(C2S::UpdateChannel { id, name: n2.clone(), bitrate: b.saturating_sub(16_000).max(16_000) })
                        }))
                        .child(text(format!("{} kbps", ch.bitrate / 1000)).font(type_::LABEL).color(p.muted).w(56.0).center())
                        .child(round_button("br-up", &icons::UP, 26.0, false, p.muted, "Raise bitrate").on_press(move |s: &mut State| {
                            s.send(C2S::UpdateChannel { id, name: n3.clone(), bitrate: (b3 + 16_000).min(256_000) })
                        }));
                }
                r = r
                    .child(round_button("move-up", &icons::UP, 28.0, false, p.muted, "Move up").on_press(move |s: &mut State| s.send(C2S::MoveChannel { id, up: true })))
                    .child(round_button("move-down", &icons::DOWN, 28.0, false, p.muted, "Move down").on_press(move |s: &mut State| s.send(C2S::MoveChannel { id, up: false })))
                    .child(round_button("rename", &icons::PENCIL, 28.0, false, p.muted, "Rename").on_press(move |s: &mut State| st(s).rename = Some((id, TextEdit::new(name.clone())))));
                if set.confirm_delete == Some(id) {
                    r = r
                        .child(button("del-ch-yes", "Delete + its messages", 2).h(28.0).on_press(move |s: &mut State| {
                            s.send(C2S::DeleteChannel { id });
                            st(s).confirm_delete = None;
                        }))
                        .child(button("del-ch-no", "Keep", 0).h(28.0).on_press(|s: &mut State| st(s).confirm_delete = None));
                } else {
                    r = r.child(round_button("delete-ch", &icons::TRASH, 28.0, false, p.red, "Delete channel").on_press(move |s: &mut State| st(s).confirm_delete = Some(id)));
                }
            }
        }
        list = list.child(r);
    }
    let kind = set.new_kind;
    let create = row()
        .w_full()
        .gap(8.0)
        .center_y()
        .child(
            field(
                text_input(&set.new_channel, BODY)
                    .key("new-channel")
                    .label("New channel name")
                    .placeholder("new-channel", p.faint)
                    .grow(1.0)
                    .on_edit(|s: &mut State| &mut st(s).new_channel)
                    .on_submit(|s: &mut State, _| create_channel(s)),
            ),
        )
        .child(chip("kind-text", "Text", kind == ChannelKind::Text).on_press(|s: &mut State| st(s).new_kind = ChannelKind::Text))
        .child(chip("kind-voice", "Voice", kind == ChannelKind::Voice).on_press(|s: &mut State| st(s).new_kind = ChannelKind::Voice))
        .child(button("create-channel", "Create", 1).on_press(create_channel));
    col().w_full().gap(10.0).child(label("CHANNELS")).child(list).child(label("NEW CHANNEL").pt(10.0)).child(create)
}

fn save_rename(s: &mut State, bitrate: u32) {
    if let Some((id, edit)) = st(s).rename.take() {
        s.send(C2S::UpdateChannel { id, name: edit.text.clone(), bitrate });
    }
}

fn create_channel(s: &mut State) {
    let set = st(s);
    let name = set.new_channel.text.trim().to_string();
    if name.is_empty() {
        return;
    }
    let kind = set.new_kind;
    set.new_channel.clear();
    s.send(C2S::CreateChannel { name, kind });
}

fn invites(m: &Model, set: &Settings) -> El {
    let p = pal();
    let mut uses = row().gap(6.0).center_y().child(text("Uses").font(type_::LABEL).color(p.muted).w(56.0));
    for (n, name) in [(Some(1), "1"), (Some(5), "5"), (None, "No limit")] {
        uses = uses.child(chip(format!("uses-{name}"), name, set.invite_uses == n).on_press(move |s: &mut State| st(s).invite_uses = n));
    }
    let mut hours = row().gap(6.0).center_y().child(text("Expires").font(type_::LABEL).color(p.muted).w(56.0));
    for (h, name) in [(Some(24), "1 day"), (Some(24 * 7), "7 days"), (None, "Never")] {
        hours = hours.child(chip(format!("hours-{name}"), name, set.invite_hours == h).on_press(move |s: &mut State| st(s).invite_hours = h));
    }
    let mut c = col()
        .w_full()
        .gap(10.0)
        .child(text("Friends join with an invite code: they pick \"Create account\" on the login screen and paste it.").font(type_::BODY).color(p.muted).wrap())
        .child(uses)
        .child(hours)
        .child(row().child(button("make-invite", "Create invite", 1).on_press(|s: &mut State| {
            let set = st(s);
            let msg = C2S::CreateInvite { uses: set.invite_uses, hours: set.invite_hours };
            s.send(msg);
        })));
    if let Some(inv) = &m.last_invite {
        let code = inv.code.clone();
        c = c.child(
            row()
                .key(format!("new-invite-{}", inv.code))
                .w_full()
                .padding(12.0)
                .gap(10.0)
                .center_y()
                .radius(12.0)
                .fill(p.accent.alpha(0.12))
                .border(1.0, p.accent.alpha(0.35))
                .transition(Transition::scale(0.96))
                .child(text(inv.code.clone()).font(TextStyle::new(20.0, 700.0)).color(p.text).grow(1.0).selectable(true))
                .child(button("copy-invite", "Copy", 0).on_press_tx(move |s: &mut State, tx| {
                    tx.copy(code.clone());
                    s.toast("Invite code copied.");
                })),
        );
    }
    c = c.child(label("ACTIVE INVITES").pt(10.0));
    if m.invites.is_empty() {
        c = c.child(text("None.").font(type_::BODY).color(p.faint));
    }
    for inv in &m.invites {
        let code = inv.code.clone();
        let uses = inv.uses_left.map_or("unlimited uses".to_string(), |n| format!("{n} use{} left", if n == 1 { "" } else { "s" }));
        let exp = inv
            .expires
            .and_then(|e| chrono::TimeZone::timestamp_millis_opt(&chrono::Local, e).single())
            .map_or("never expires".to_string(), |d| format!("expires {}", d.format("%d/%m %H:%M")));
        c = c.child(
            row()
                .key(format!("inv-{}", inv.code))
                .w_full()
                .h(40.0)
                .px(10.0)
                .gap(10.0)
                .center_y()
                .radius(10.0)
                .fill(p.ink(0.03))
                .child(text(inv.code.clone()).font(TextStyle::new(14.0, 650.0)).color(p.text))
                .child(text(format!("{uses} · {exp}")).font(type_::LABEL).color(p.faint).grow(1.0))
                .child(button("revoke", "Revoke", 0).h(28.0).on_press(move |s: &mut State| s.send(C2S::RevokeInvite { code: code.clone() }))),
        );
    }
    c
}


fn device_list(key: &'static str, devices: Option<&[crate::voice::devices::Device]>, selected: &Option<String>, pick: fn(&mut State, Option<String>)) -> El {
    let p = pal();
    let mut col_ = col().w_full().gap(4.0);
    let row_ = |k: String, name: String, on: bool| {
        let r = row()
            .key(k)
            .role("button")
            .w_full()
            .h(34.0)
            .px(12.0)
            .gap(8.0)
            .center_y()
            .radius(8.0)
            .child(el().size(8.0, 8.0).radius(4.0).fill(if on { p.accent } else { p.ink(0.12) }))
            .child(text(name).font(type_::BODY).color(if on { p.text } else { p.muted }).grow(1.0).truncate());
        if on { r.fill(p.select) } else { r.hover_fill(p.hover) }
    };
    col_ = col_.child(row_(format!("{key}-default"), "System default".into(), selected.is_none()).on_press(move |s: &mut State| pick(s, None)));
    match devices {
        None => col_.child(text("Looking for devices…").font(type_::LABEL).color(p.faint).px(12.0)),
        Some(list) => {
            for d in list {
                let id = d.id.clone();
                col_ = col_.child(row_(format!("{key}-{}", d.id), d.name.clone(), selected.as_deref() == Some(d.id.as_str())).on_press(move |s: &mut State| pick(s, Some(id.clone()))));
            }
            col_
        }
    }
}

/// A volume stepper in 25% steps between `min` and `max` (also set from the mute/deafen
/// right-click menus).
fn volume_row(key: &'static str, title: &'static str, now: f32, min: f32, max: f32, pick: fn(&mut State) -> &mut f32) -> El {
    let p = pal();
    let step = move |s: &mut State, d: f32| {
        let v = pick(s);
        *v = ((*v + d) * 4.0).round() / 4.0;
        *v = v.clamp(min, max);
        s.save_voice();
    };
    row()
        .key(key)
        .gap(8.0)
        .center_y()
        .px(12.0)
        .pt(4.0)
        .child(text(title).font(type_::BODY).color(p.muted).w(120.0))
        .child(round_button("down", &icons::DOWN, 28.0, false, p.muted, "Quieter").on_press(move |s: &mut State| step(s, -0.25)))
        .child(text(format!("{:.0}%", now * 100.0)).font(type_::TITLE).color(p.text).w(52.0).center())
        .child(round_button("up", &icons::UP, 28.0, false, p.muted, "Louder").on_press(move |s: &mut State| step(s, 0.25)))
}

fn toggle(key: &'static str, label: &'static str, hint: &'static str, on: bool, flip: fn(&mut State)) -> El {
    let p = pal();
    let knob = el().size(18.0, 18.0).radius(9.0).fill(oklch(0.98, 0.0, 0.0)).absolute(if on { 20.0 } else { 2.0 }, 2.0).motion(super::SOFT);
    let sw = el().size(40.0, 22.0).radius(11.0).fill(if on { p.accent } else { p.ink(0.15) }).motion(super::SOFT).child(knob);
    row()
        .key(key)
        .role("switch")
        .label(label)
        .checked(on)
        .w_full()
        .gap(12.0)
        .center_y()
        .child(col().grow(1.0).shrink().child(text(label).font(type_::TITLE).color(p.text)).child(text(hint).font(type_::LABEL).color(p.faint).wrap()))
        .child(sw)
        .on_press(move |s: &mut State| {
            flip(s);
            s.save_voice();
            s.save_app();
        })
}

fn app_tab(s: &State) -> El {
    let p = pal();
    let a = &s.app;
    let vol = a.volume;
    let mut themes = row().gap(6.0);
    for (t, name) in [(ThemeChoice::System, "Match system"), (ThemeChoice::Dark, "Dark"), (ThemeChoice::Light, "Light")] {
        themes = themes.child(chip(format!("theme-{name}"), name, a.theme == t).on_press(move |s: &mut State| {
            s.app.theme = t;
            s.save_app();
        }));
    }
    col()
        .w_full()
        .gap(14.0)
        .child(label("APPEARANCE"))
        .child(themes)
        .child(label("SOUNDS").pt(6.0))
        .child(toggle("sounds", "Play sounds", "Message pings and the join, leave and mute cues.", a.sounds, |s| s.app.sounds = !s.app.sounds))
        .child(
            row()
                .gap(8.0)
                .center_y()
                .opacity(if a.sounds { 1.0 } else { 0.5 })
                .child(text("Volume").font(type_::BODY).color(p.muted).w(90.0))
                .child(round_button("sfx-down", &icons::DOWN, 28.0, false, p.muted, "Quieter").on_press(|s: &mut State| {
                    s.app.volume = ((s.app.volume - 0.1) * 10.0).round() / 10.0;
                    s.app.volume = s.app.volume.max(0.1);
                    s.save_app();
                    s.play(crate::voice::sounds::Cue::Message);
                }))
                .child(text(format!("{:.0}%", vol * 100.0)).font(type_::TITLE).color(p.text).w(48.0).center())
                .child(round_button("sfx-up", &icons::UP, 28.0, false, p.muted, "Louder").on_press(|s: &mut State| {
                    s.app.volume = (((s.app.volume + 0.1) * 10.0).round() / 10.0).min(1.0);
                    s.save_app();
                    s.play(crate::voice::sounds::Cue::Message);
                })),
        )
        .child(label("NOTIFICATIONS").pt(6.0))
        .child(text("Choose per channel with the bell in each channel's header: all messages, mentions and replies (the default), or nothing. Do Not Disturb silences everything.").font(type_::BODY).color(p.muted).wrap())
        .child(label("STARTUP").pt(6.0))
        .child(toggle("autostart", "Start kall when you log in", "It starts in the system tray.", a.autostart, |s| {
            let on = !s.app.autostart;
            match crate::desktop::set_autostart(on) {
                Ok(()) => s.app.autostart = on,
                Err(e) => s.toast(format!("Couldn't change that: {e:#}")),
            }
        }))
        .child(label("ABOUT").pt(6.0))
        .child(text(concat!("kall ", env!("CARGO_PKG_VERSION"))).key("version").font(type_::BODY).color(p.muted).selectable(true))
}

fn voice_tab(s: &State) -> El {
    let p = pal();
    let v = &s.voice;
    let devices = s.devices.as_ref();
    let mut modes = row().gap(6.0);
    for (mode, name) in [(Mode::VoiceActivity, "Voice activity"), (Mode::PushToTalk, "Push to talk")] {
        modes = modes.child(chip(format!("mode-{name}"), name, v.mode == mode).on_press(move |s: &mut State| {
            s.voice.mode = mode;
            s.save_voice();
        }));
    }
    let mut c = col().w_full().gap(10.0).child(label("INPUT MODE")).child(modes);
    if v.mode == Mode::PushToTalk {
        let mut keys = row().gap(6.0).flex_wrap();
        for &(k, name) in PTT_KEYS {
            keys = keys.child(chip(format!("ptt-{k}"), name, v.ptt_key == k).on_press(move |s: &mut State| {
                s.voice.ptt_key = k.into();
                s.save_voice();
            }));
        }
        c = c.child(text("Hold this key to talk. It works while other apps (games) are focused.").font(type_::LABEL).color(p.faint)).child(keys);
    } else {
        let t = v.vad_threshold;
        c = c.child(
            row()
                .gap(8.0)
                .center_y()
                .child(text("Sensitivity").font(type_::BODY).color(p.muted).w(90.0))
                .child(round_button("vad-down", &icons::DOWN, 28.0, false, p.muted, "More sensitive").on_press(|s: &mut State| {
                    s.voice.vad_threshold = (s.voice.vad_threshold - 0.1).max(0.1);
                    s.save_voice();
                }))
                .child(text(format!("{:.0}%", (1.0 - t) * 100.0)).font(type_::TITLE).color(p.text).w(48.0).center())
                .child(round_button("vad-up", &icons::UP, 28.0, false, p.muted, "Less sensitive").on_press(|s: &mut State| {
                    s.voice.vad_threshold = (s.voice.vad_threshold + 0.1).min(0.95);
                    s.save_voice();
                }))
                .child(text("Higher picks up quieter speech; lower ignores more background.").font(type_::LABEL).color(p.faint).grow(1.0).wrap()),
        );
    }
    // Live microphone meter while in a call.
    let in_call = s.m.as_ref().is_some_and(|m| m.voice.contains_key(&m.me));
    let lvl = if in_call { s.shared.level().sqrt().min(1.0) } else { 0.0 };
    let sending = s.m.as_ref().is_some_and(|m| s.speaking.contains(&m.me));
    c = c.child(label("MICROPHONE LEVEL").pt(8.0)).child(
        el().key("meter").w_full().h(10.0).radius(5.0).fill(p.ink(0.08)).child(
            el().h_full().w_frac(lvl).radius(5.0).fill(if sending { p.green } else { p.accent2 }),
        ),
    );
    if !in_call {
        c = c.child(text("Join a voice channel to see your mic level.").font(type_::LABEL).color(p.faint));
    }
    c.child(label("PROCESSING").pt(8.0))
        .child(toggle("ns", "Noise suppression", "Removes fans, keyboards and background hum (RNNoise).", v.noise_suppression, |s| s.voice.noise_suppression = !s.voice.noise_suppression))
        .child(toggle("aec", "Echo cancellation", "Stops friends hearing themselves when you use speakers.", v.echo_cancellation, |s| s.voice.echo_cancellation = !s.voice.echo_cancellation))
        .child(label("MICROPHONE").pt(8.0))
        .child(device_list("in", devices.map(|d| d.inputs.as_slice()), &v.input, |s, id| {
            s.voice.input = id;
            s.save_voice();
        }))
        .child(volume_row("mic-vol", "Mic volume", v.input_volume, 0.5, 2.0, |s| &mut s.voice.input_volume))
        .child(label("SPEAKERS / HEADPHONES").pt(8.0))
        .child(device_list("out", devices.map(|d| d.outputs.as_slice()), &v.output, |s, id| {
            s.voice.output = id;
            s.save_voice();
        }))
        .child(volume_row("out-vol", "Output volume", v.output_volume, 0.25, 2.0, |s| &mut s.voice.output_volume))
}

pub fn view(s: &State, m: &Model, set: &Settings) -> El {
    let p = pal();
    let role = m.my_role();
    let mut side = col().w(190.0).h_full().padding(12.0).gap(2.0).fill(p.side).border_sides([0.0, 1.0, 0.0, 0.0], p.line).child(text("USER").font(SMALL).color(p.faint).px(12.0).py(6.0)).child(nav("nav-profile", "My profile", Tab::Profile, set.tab == Tab::Profile))
        .child(nav("nav-voice", "Voice & audio", Tab::Voice, set.tab == Tab::Voice))
        .child(nav("nav-app", "App", Tab::App, set.tab == Tab::App));
    if role.can_manage_channels() || role.can_invite() {
        side = side.child(text("SERVER").font(SMALL).color(p.faint).px(12.0).pt(14.0).pb(6.0));
        if role.can_manage_channels() {
            side = side.child(nav("nav-channels", "Channels", Tab::Channels, set.tab == Tab::Channels));
        }
        side = side.child(nav("nav-invites", "Invites", Tab::Invites, set.tab == Tab::Invites));
    }
    let (title, body) = match set.tab {
        Tab::Profile => ("My profile", profile(m, set)),
        Tab::Voice => ("Voice & audio", voice_tab(s)),
        Tab::App => ("App", app_tab(s)),
        Tab::Channels => ("Channels", channels(m, set)),
        Tab::Invites => ("Invites", invites(m, set)),
    };
    let content = col()
        .grow(1.0)
        .shrink()
        .h_full()
        .child(
            row()
                .w_full()
                .h(56.0)
                .px(22.0)
                .center_y()
                .child(text(title).font(TextStyle::new(17.0, 650.0)).color(p.text).grow(1.0))
                .child(round_button("close-settings", &icons::X, 32.0, false, p.muted, "Close").on_press(|s: &mut State| s.settings = None)),
        )
        .child(col().key("settings-body").w_full().grow(1.0).min_h(0.0).scroll(rune::ScrollSpec::default()).child(col().w_full().px(22.0).pb(22.0).child(body)));
    let sheet = panel(row().w(780.0).h(560.0))
        .radius(14.0)
        .clip_rounded(14.0)
        .border(1.0, p.line)
        .shadow(12.0, 40.0, oklcha(0.0, 0.0, 0.0, if p.dark { 0.4 } else { 0.18 }))
        .transition(Transition::scale(0.97))
        .child(side)
        .child(content);
    el().key("settings")
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .fill(oklcha(0.0, 0.0, 0.0, if p.dark { 0.5 } else { 0.25 }))
        .center()
        .on_press(|s: &mut State| s.settings = None)
        .child(sheet.on_press(|_: &mut State| {}))
}
