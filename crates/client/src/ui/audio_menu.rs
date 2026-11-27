//! Right-click the mute or deafen button for a quick panel, drawn by kall (so it looks the same
//! on every platform): pick the microphone or the speakers, drag their volume, and toggle mute
//! or deafen. It opens above the click and closes on a click outside.

use kall_proto::C2S;
use rune::{El, Phase, col, el, row, text};
use rune::tok::type_;

use super::{glyph, label, pal};
use crate::{State, Tab, icons};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Input,
    Output,
}

/// The open panel: which side, and the window point it was opened at.
#[derive(Clone, Copy, Debug)]
pub struct Open {
    pub side: Side,
    pub x: f32,
    pub y: f32,
}

const WIDTH: f32 = 300.0;

/// Right-click `e` (a mute button) for the microphone panel.
pub fn mic(e: El) -> El {
    e.on_secondary_press(|s: &mut State, press, _| open(s, press, Side::Input))
}

/// Right-click `e` (a deafen button) for the speakers panel.
pub fn speakers(e: El) -> El {
    e.on_secondary_press(|s: &mut State, press, _| open(s, press, Side::Output))
}

fn open(s: &mut State, press: rune::PointerPress, side: Side) {
    if s.devices.is_none() {
        s.refresh_devices();
    }
    s.audio_menu = Some(Open { side, x: press.x, y: press.y });
}

/// (min, max) of each side's volume.
fn range(side: Side) -> (f32, f32) {
    match side {
        Side::Input => (0.0, 2.0),
        Side::Output => (0.0, 2.0),
    }
}

fn volume(s: &mut State, side: Side) -> &mut f32 {
    match side {
        Side::Input => &mut s.voice.input_volume,
        Side::Output => &mut s.voice.output_volume,
    }
}

/// The panel layer, when one is open: a click-catcher over the window with the panel above
/// the point that opened it.
pub fn layer(s: &State) -> Option<El> {
    let at = s.audio_menu?;
    let p = pal();
    let side = at.side;
    let (title, selected, list) = match side {
        Side::Input => ("Microphone", &s.voice.input, s.devices.as_ref().map(|d| d.inputs.as_slice())),
        Side::Output => ("Output", &s.voice.output, s.devices.as_ref().map(|d| d.outputs.as_slice())),
    };

    // Mute/deafen toggle in the header, while in a call.
    let v = s.in_voice_here(None).cloned();
    let (on, what) = match side {
        Side::Input => (v.as_ref().is_some_and(|v| v.muted), "Mute"),
        Side::Output => (v.as_ref().is_some_and(|v| v.deafened), "Deafen"),
    };
    let mut header = row().w_full().center_y().gap(8.0).child(text(title).font(type_::TITLE).color(p.text).grow(1.0));
    if let Some(v) = v.clone() {
        let knob = el().size(16.0, 16.0).radius(8.0).fill(rune::oklch(0.98, 0.0, 0.0)).absolute(if on { 18.0 } else { 2.0 }, 2.0).motion(super::SOFT);
        header = header.child(
            row()
                .key("menu-toggle")
                .role("switch")
                .label(what)
                .checked(on)
                .gap(8.0)
                .center_y()
                .child(text(what).font(type_::LABEL).color(p.muted))
                .child(el().size(36.0, 20.0).radius(10.0).fill(if on { p.red } else { p.ink(0.15) }).motion(super::SOFT).child(knob))
                .on_press(move |s: &mut State| match side {
                    Side::Input => s.send(C2S::VoiceState { muted: !v.muted, deafened: v.deafened && !v.muted }),
                    Side::Output => s.send(C2S::VoiceState { muted: !v.deafened || v.muted, deafened: !v.deafened }),
                }),
        );
    }

    // Devices, like Settings: a dot on the one in use.
    let mut devices = col().w_full().gap(2.0);
    let dev_row = |key: String, name: String, on: bool, id: Option<String>| {
        let r = row()
            .key(key)
            .role("button")
            .w_full()
            .h(32.0)
            .px(10.0)
            .gap(8.0)
            .center_y()
            .radius(8.0)
            .child(el().size(8.0, 8.0).radius(4.0).fill(if on { p.accent } else { p.ink(0.12) }))
            .child(text(name).font(type_::BODY).color(if on { p.text } else { p.muted }).grow(1.0).shrink().truncate())
            .on_press(move |s: &mut State| {
                match side {
                    Side::Input => s.voice.input = id.clone(),
                    Side::Output => s.voice.output = id.clone(),
                }
                s.save_voice();
            });
        if on { r.fill(p.select) } else { r.hover_fill(p.hover) }
    };
    devices = devices.child(dev_row("dev-default".into(), "System default".into(), selected.is_none(), None));
    match list {
        None => devices = devices.child(text("Looking for devices…").font(type_::LABEL).color(p.faint).px(10.0).py(6.0)),
        Some(list) => {
            for d in list {
                let on = selected.as_deref() == Some(d.id.as_str());
                devices = devices.child(dev_row(format!("dev-{}", d.id), d.name.clone(), on, Some(d.id.clone())));
            }
        }
    }

    // Volume: a track you click or drag; saved when you let go.
    let (min, max) = range(side);
    let now = match side {
        Side::Input => s.voice.input_volume,
        Side::Output => s.voice.output_volume,
    };
    let frac = ((now - min) / (max - min)).clamp(0.0, 1.0);
    let track = el()
        .key("volume-track")
        .role("slider")
        .label(format!("{title} volume"))
        .grow(1.0)
        .h(24.0)
        .child(el().pin(Some(0.0), Some(10.0), Some(0.0), None).h(4.0).radius(2.0).fill(p.ink(0.15)))
        .child(el().pin(Some(0.0), Some(10.0), None, None).h(4.0).radius(2.0).w_frac(frac).fill(p.accent))
        // The knob sits between two spacers that split the free width by the value.
        .child(
            row()
                .pin(Some(0.0), Some(4.0), Some(0.0), None)
                .h(16.0)
                .child(el().grow(frac))
                .child(el().size(16.0, 16.0).radius(8.0).fill(rune::oklch(0.98, 0.0, 0.0)).border(1.0, p.ink(0.2)))
                .child(el().grow(1.0 - frac)),
        )
        .on_drag(move |s: &mut State, d, _| {
            if d.w > 0.0 {
                let v = min + (d.x / d.w).clamp(0.0, 1.0) * (max - min);
                *volume(s, side) = (v * 20.0).round() / 20.0; // 5% steps
            }
            if d.phase == Phase::Up {
                s.save_voice();
            }
        });
    let vol = row()
        .w_full()
        .gap(12.0)
        .center_y()
        .child(glyph(if side == Side::Input { &icons::MIC } else { &icons::HEADPHONES }, 16.0, p.faint))
        .child(track)
        .child(text(format!("{:.0}%", now * 100.0)).font(type_::TITLE).color(p.text).w(44.0));

    let settings = row()
        .key("menu-settings")
        .role("button")
        .w_full()
        .h(32.0)
        .px(10.0)
        .gap(8.0)
        .center_y()
        .radius(8.0)
        .hover_fill(p.hover)
        .child(glyph(&icons::GEAR, 15.0, p.faint))
        .child(text("Voice & audio settings").font(type_::BODY).color(p.muted))
        .on_press(|s: &mut State| {
            s.audio_menu = None;
            crate::ui::settings::open(s, Tab::Voice);
        });

    let panel = col()
        .key("audio-menu")
        .w(WIDTH)
        .padding(12.0)
        .gap(10.0)
        .radius(12.0)
        .fill(p.panel)
        .border(1.0, p.line)
        .shadow(8.0, 28.0, rune::oklcha(0.0, 0.0, 0.0, if p.dark { 0.45 } else { 0.18 }))
        .transition(rune::Transition::offset(0.0, 6.0).combined(rune::Transition::opacity()))
        .child(header)
        .child(label(if side == Side::Input { "INPUT DEVICE" } else { "OUTPUT DEVICE" }))
        .child(devices)
        .child(label("VOLUME").pt(2.0))
        .child(vol)
        .child(el().w_full().h(1.0).fill(p.line))
        .child(settings)
        .on_press(|_: &mut State| {});

    // A box from the top of the window down to the click, with the panel at its bottom, so
    // the panel's bottom edge sits just above where you clicked.
    let left = (at.x - WIDTH / 2.0).max(8.0);
    let anchor = col().pin(Some(left), Some(0.0), None, None).h((at.y - 10.0).max(0.0)).justify_end().child(panel);
    Some(
        el().key("audio-menu-layer")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .on_press(|s: &mut State| s.audio_menu = None)
            .on_secondary_press(|s: &mut State, _, _| s.audio_menu = None)
            .child(anchor),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_at_the_click_and_lists_devices() {
        let mut s = State::default();
        s.devices = Some(crate::voice::devices::Devices {
            inputs: vec![crate::voice::devices::Device { id: "mic1".into(), name: "USB Mic".into() }],
            outputs: vec![crate::voice::devices::Device { id: "spk1".into(), name: "Speakers".into() }],
        });
        assert!(layer(&s).is_none());
        open(&mut s, rune::PointerPress { x: 400.0, y: 700.0 }, Side::Input);
        let at = s.audio_menu.unwrap();
        assert_eq!((at.side, at.x, at.y), (Side::Input, 400.0, 700.0));
        assert!(layer(&s).is_some());
        *volume(&mut s, Side::Output) = 0.5;
        assert!((s.voice.output_volume - 0.5).abs() < 1e-6);
    }
}
