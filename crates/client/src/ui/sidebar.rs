//! Server banner, channel list with unread/mention marks and voice occupants, the voice panel
//! and your own user panel.


use kall_proto::{C2S, ChannelKind, Status, StreamKind};
use rune::tok::type_;
use rune::{El, ScrollSpec, TextStyle, Transition, col, el, row, text};

use super::{SMALL, avatar, avatar_dot, badge, glyph, pal, round_button};
use crate::model::Model;
use crate::{State, icons};

pub fn view(s: &State, m: &Model) -> El {
    let p = pal();
    let can_manage = m.my_role().can_manage_channels() || m.my_role().can_invite();
    let mut banner = row()
        .w_full()
        .h(56.0)
        .pl(16.0)
        .pr(10.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], p.line)
        .child(
            col()
                .grow(1.0)
                .shrink()
                .child(text(m.server_name.clone()).font(TextStyle::new(15.0, 650.0)).color(p.text).truncate())
                .child(text(format!("{} online · {} members", m.users.values().filter(|u| u.online).count(), m.users.len())).font(type_::LABEL).color(p.muted)),
        );
    if can_manage {
        banner = banner.child(round_button("server-settings", &icons::GEAR, 30.0, false, p.muted, "Server settings").on_press(|s: &mut State| {
            let tab = if s.m.as_ref().is_some_and(|m| m.my_role().can_manage_channels()) { crate::Tab::Channels } else { crate::Tab::Invites };
            crate::ui::settings::open(s, tab);
        }));
    }

    let mut list = col().w_full().gap(2.0).px(8.0).pb(8.0);
    let mut last_kind = None;
    // Text channels first, then voice, each in the server's order.
    let ordered = m.channels.iter().filter(|c| c.kind == ChannelKind::Text).chain(m.channels.iter().filter(|c| c.kind == ChannelKind::Voice));
    for ch in ordered {
        if last_kind != Some(ch.kind) {
            let label = if ch.kind == ChannelKind::Text { "TEXT CHANNELS" } else { "VOICE CHANNELS" };
            list = list.child(text(label).key(format!("section-{}", ch.id)).font(SMALL).color(p.faint).pt(14.0).pb(4.0).px(8.0));
            last_kind = Some(ch.kind);
        }
        let on = ch.id == s.channel;
        let unread = !on && m.unread(ch);
        let mentions = if on { 0 } else { m.mentions(ch.id) };
        let ico = if ch.kind == ChannelKind::Text { &icons::HASH } else { &icons::SPEAKER };
        let id = ch.id;
        let name_color = if on || unread { p.text } else { p.muted };
        let mut r = row()
            .key(format!("channel-{id}"))
            .role("button")
            .label(ch.name.clone())
            .w_full()
            .h(34.0)
            .px(10.0)
            .gap(8.0)
            .center_y()
            .radius(8.0)
            .hover_fill(p.hover)
            .press_scale(0.98)
            .child(glyph(ico, 17.0, if on { p.accent2 } else { p.faint }))
            .child(text(ch.name.clone()).font(TextStyle::new(13.5, if unread { 700.0 } else { 550.0 })).color(name_color).grow(1.0).truncate())
            .on_press(move |s: &mut State| s.select_channel(id));
        if mentions > 0 {
            r = r.child(badge(mentions.to_string(), p.red));
        }
        if unread {
            r = r.child(el().key("unread").w(4.0).h(8.0).radius(2.0).fill(p.text).absolute(-8.0, 13.0));
        }
        if on {
            r = r
                .fill(p.select)
                .child(el().key("bar").w(3.0).h(18.0).radius(1.5).fill(p.accent2).absolute(0.0, 8.0));
        }
        list = list.child(r);
        if ch.kind == ChannelKind::Voice {
            let mut people: Vec<_> = m.voice.values().filter(|v| v.channel == ch.id).collect();
            people.sort_by_key(|v| (v.user != m.me, v.user));
            for v in people {
                let mut r = row()
                    .key(format!("occupant-{}-{}", ch.id, v.user))
                    .w_full()
                    .h(30.0)
                    .pl(34.0)
                    .pr(6.0)
                    .gap(8.0)
                    .center_y()
                    .transition(Transition::offset(-8.0, 0.0).combined(Transition::opacity()))
                    .child(avatar(m, v.user, 22.0, s.speaking.contains(&v.user)))
                    .child(text(m.name(v.user).to_string()).font(type_::BODY).color(p.muted).grow(1.0).truncate());
                // Sharing: a LIVE pill (click to watch) and a camera mark.
                if m.streams.contains_key(&(v.user, StreamKind::Camera)) {
                    r = r.child(glyph(&icons::CAMERA, 14.0, p.faint));
                }
                if m.streams.contains_key(&(v.user, StreamKind::Screen)) {
                    let u = v.user;
                    r = r.child(
                        badge("LIVE", p.red)
                            .key(format!("live-{u}"))
                            .role("button")
                            .label(format!("Watch {}'s screen", m.name(u)))
                            .help("Watch")
                            .on_press(move |s: &mut State| s.watch_stream(u)),
                    );
                }
                if v.deafened {
                    r = r.child(glyph(&icons::HEADPHONES_OFF, 14.0, p.red));
                } else if v.muted {
                    r = r.child(glyph(&icons::MIC_OFF, 14.0, p.red));
                }
                list = list.child(r);
            }
        }
    }

    col().w(256.0).h_full().fill(p.side).border_sides([0.0, 1.0, 0.0, 0.0], p.line)
        .child(banner)
        .child(col().key("channels").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec::default()).scroll_edge_fade(16.0).child(list))
        .children(update_card(s))
        .children(voice_panel(s, m))
        .child(user_panel(s, m))
}

/// "A new kall is ready" with a restart button.
fn update_card(s: &State) -> Option<El> {
    let p = pal();
    let label = if s.updating {
        "Updating… kall will restart".to_string()
    } else {
        format!("kall {} is available", s.update.as_ref()?.version.trim_start_matches('v'))
    };
    let mut card = row()
        .key("update")
        .mx(8.0)
        .mb(6.0)
        .px(10.0)
        .py(8.0)
        .gap(8.0)
        .center_y()
        .radius(12.0)
        .fill(p.accent.alpha(0.12))
        .border(1.0, p.accent.alpha(0.35))
        .transition(Transition::offset(0.0, 12.0).combined(Transition::opacity()))
        .child(text(label).font(type_::LABEL).color(p.text).grow(1.0).shrink().wrap());
    if !s.updating {
        card = card.child(super::button("restart-update", "Restart", 1).h(28.0).on_press(|s: &mut State| s.install_update()));
    }
    Some(card)
}

fn voice_panel(s: &State, m: &Model) -> Option<El> {
    let p = pal();
    let v = s.in_voice_here(None)?;
    let ch = m.channel(v.channel)?;
    let channel = ch.id;
    let info = col()
        .grow(1.0)
        .shrink()
        .gap(2.0)
        .child(text("Voice Connected").font(type_::TITLE).color(p.green))
        .child(text(ch.name.clone()).font(type_::LABEL).color(p.muted).truncate())
        .on_press(move |s: &mut State| s.select_channel(channel));
    let hang = round_button("panel-leave", &icons::PHONE_OFF, 32.0, false, p.red, "Disconnect").on_press(|s: &mut State| s.leave_voice());
    Some(
        row()
            .key("voice-panel")
            .mx(8.0)
            .mb(6.0)
            .px(10.0)
            .py(8.0)
            .gap(6.0)
            .center_y()
            .radius(12.0)
            .fill(p.green.alpha(0.07))
            .border(1.0, p.green.alpha(0.18))
            .transition(Transition::offset(0.0, 12.0).combined(Transition::opacity()))
            .child(info)
            .child(hang),
    )
}

fn user_panel(s: &State, m: &Model) -> El {
    let p = pal();
    let Some(me) = m.me() else { return el() };
    let voice = s.in_voice_here(None);
    let (muted, deaf) = voice.map_or((false, false), |v| (v.muted, v.deafened));
    let status = match me.status {
        Status::Online => "Online",
        Status::Idle => "Idle",
        Status::Dnd => "Do not disturb",
    };
    let sub = if me.status_text.is_empty() { status.to_string() } else { me.status_text.clone() };
    let who = row()
        .key("me")
        .role("button")
        .label("Profile")
        .grow(1.0)
        .shrink()
        .gap(9.0)
        .center_y()
        .radius(10.0)
        .padding(4.0)
        .hover_fill(p.hover)
        .child(avatar_dot(m, m.me, 34.0, p.side))
        .child(
            col()
                .grow(1.0)
                .shrink()
                .child(text(me.display_name.clone()).font(type_::TITLE).color(p.text).truncate())
                .child(text(sub).font(type_::LABEL).color(p.faint).truncate()),
        )
        .on_press(|s: &mut State| crate::ui::settings::open(s, crate::Tab::Profile));
    let in_voice = voice.is_some();
    let mic = round_button("me-mic", if muted { &icons::MIC_OFF } else { &icons::MIC }, 32.0, false, if muted { p.red } else { p.muted }, "Mute")
        .opacity(if in_voice { 1.0 } else { 0.5 })
        .on_press(move |s: &mut State| {
            if in_voice {
                s.send(C2S::VoiceState { muted: !muted, deafened: deaf && !muted });
            }
        });
    let deafen = round_button(
        "me-deafen",
        if deaf { &icons::HEADPHONES_OFF } else { &icons::HEADPHONES },
        32.0,
        false,
        if deaf { p.red } else { p.muted },
        "Deafen",
    )
    .opacity(if in_voice { 1.0 } else { 0.5 })
    .on_press(move |s: &mut State| {
        if in_voice {
            s.send(C2S::VoiceState { muted: !deaf || muted, deafened: !deaf });
        }
    });
    row()
        .key("user-panel")
        .w_full()
        .h(58.0)
        .px(8.0)
        .gap(2.0)
        .center_y()
        .border_sides([1.0, 0.0, 0.0, 0.0], p.line)
        .child(who)
        .child(mic)
        .child(deafen)
}
