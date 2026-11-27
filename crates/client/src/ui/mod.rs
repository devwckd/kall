//! The window: login screen, or the app (sidebar, channel, members) with settings and toasts
//! layered on top. Look: clean and minimal, edge-to-edge panes split by hairlines, one emerald
//! accent, dark and light themes following the system.

pub mod audio_menu;
pub mod avatar_crop;
mod chat;
mod login;
mod members;
pub mod settings;
mod sidebar;
mod voice;

use std::cell::Cell;

use kall_proto::{ChannelKind, Id, Role, Status};
use rune::tok::type_;
use rune::{Color, Cx, El, Icon, Spring, TextStyle, col, el, icon, oklch, oklcha, row, text};

use crate::State;
use crate::model::Model;

pub struct Pal {
    pub dark: bool,
    /// Window background (the main pane).
    pub bg: Color,
    /// The main content pane.
    pub panel: Color,
    /// Sidebars.
    pub side: Color,
    /// Raised things: composer, cards, tiles, menus.
    pub surface: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub hover: Color,
    pub select: Color,
    pub accent: Color,
    /// The accent where it's text or an icon on the background (contrast-adjusted).
    pub accent2: Color,
    /// Text and icons on an accent fill.
    pub on_accent: Color,
    pub green: Color,
    pub yellow: Color,
    pub red: Color,
    pub line: Color,
    pub field: Color,
}

impl Pal {
    /// A neutral overlay: white on dark, black on light.
    pub fn ink(&self, a: f32) -> Color {
        if self.dark { oklcha(1.0, 0.0, 0.0, a) } else { oklcha(0.0, 0.0, 0.0, a * 0.9) }
    }
}

thread_local! {
    static DARK: Cell<bool> = const { Cell::new(true) };
}

pub fn pal() -> Pal {
    // Emerald (#10B981 ≈ oklch 0.70 0.15 163); neutrals with the faintest cool tint.
    if DARK.get() {
        Pal {
            dark: true,
            bg: oklch(0.195, 0.004, 265.0),
            panel: oklch(0.195, 0.004, 265.0),
            side: oklch(0.172, 0.004, 265.0),
            surface: oklch(0.235, 0.005, 265.0),
            text: oklch(0.95, 0.003, 265.0),
            muted: oklch(0.74, 0.006, 265.0),
            faint: oklch(0.56, 0.008, 265.0),
            hover: oklcha(1.0, 0.0, 0.0, 0.045),
            select: oklcha(1.0, 0.0, 0.0, 0.08),
            accent: oklch(0.70, 0.15, 163.0),
            accent2: oklch(0.78, 0.14, 163.0),
            on_accent: oklch(0.16, 0.03, 163.0),
            green: oklch(0.74, 0.17, 150.0),
            yellow: oklch(0.83, 0.15, 85.0),
            red: oklch(0.65, 0.2, 22.0),
            line: oklcha(1.0, 0.0, 0.0, 0.075),
            field: oklch(0.155, 0.004, 265.0),
        }
    } else {
        Pal {
            dark: false,
            bg: oklch(0.995, 0.0, 0.0),
            panel: oklch(0.995, 0.0, 0.0),
            side: oklch(0.972, 0.003, 265.0),
            surface: oklch(0.965, 0.003, 265.0),
            text: oklch(0.22, 0.006, 265.0),
            muted: oklch(0.45, 0.008, 265.0),
            faint: oklch(0.6, 0.008, 265.0),
            hover: oklcha(0.0, 0.0, 0.0, 0.04),
            select: oklcha(0.0, 0.0, 0.0, 0.065),
            accent: oklch(0.62, 0.15, 163.0),
            accent2: oklch(0.52, 0.13, 163.0),
            on_accent: oklch(0.99, 0.0, 0.0),
            green: oklch(0.62, 0.17, 150.0),
            yellow: oklch(0.72, 0.15, 75.0),
            red: oklch(0.58, 0.21, 25.0),
            line: oklcha(0.0, 0.0, 0.0, 0.085),
            field: oklch(0.995, 0.0, 0.0),
        }
    }
}

pub const NAME: TextStyle = TextStyle::new(14.0, 600.0);
pub const HEADER: TextStyle = TextStyle::new(15.0, 600.0);
pub const SMALL: TextStyle = TextStyle::new(11.0, 600.0);
pub const BODY: TextStyle = TextStyle::new(14.5, 400.0);
pub const SOFT: Spring = Spring::new(0.3, 0.0);

/// A stable hue per user, from their username.
pub fn hue(m: &Model, user: Id) -> f32 {
    let name = m.users.get(&user).map_or("", |u| u.username.as_str());
    let h = name.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    (h % 360) as f32
}

pub fn user_color(m: &Model, user: Id) -> Color {
    let dark = pal().dark;
    let (l, c) = if dark { (0.8, 0.11) } else { (0.5, 0.13) };
    match m.users.get(&user).map(|u| u.role) {
        Some(Role::Owner) => oklch(l, c + 0.03, 75.0),
        Some(Role::Admin) => oklch(l, c + 0.04, 25.0),
        Some(Role::Mod) => oklch(l, c, 230.0),
        _ => pal().text,
    }
}

pub fn status_color(status: Status, online: bool) -> Color {
    let p = pal();
    match (online, status) {
        (false, _) => p.faint,
        (true, Status::Online) => p.green,
        (true, Status::Idle) => p.yellow,
        (true, Status::Dnd) => p.red,
    }
}

/// A solid accent fill (the old gradient, now flat).
pub fn accent(e: El) -> El {
    e.fill(pal().accent)
}

/// A content pane: flat, edge to edge.
pub fn panel(e: El) -> El {
    e.fill(pal().panel)
}

/// The user's picture, or a flat circle with their initial; an accent ring while talking.
pub fn avatar(m: &Model, user: Id, size: f32, talking: bool) -> El {
    let p = pal();
    let mut a = el().size(size, size).radius(size / 2.0).motion(SOFT);
    if let Some((_, px)) = m.avatars.get(&user) {
        a = a.clip_rounded(size / 2.0).pixels(px);
    } else {
        let h = hue(m, user);
        let initial = m.name(user).chars().next().unwrap_or('?').to_uppercase().to_string();
        let (bg, fg) = if p.dark { (oklch(0.42, 0.07, h), oklch(0.93, 0.03, h)) } else { (oklch(0.9, 0.05, h), oklch(0.38, 0.09, h)) };
        a = a.fill(bg).center().child(text(initial).font(TextStyle::new((size * 0.42).round(), 600.0)).color(fg));
    }
    if talking {
        a = a.border((size / 16.0).max(2.0), p.accent);
    }
    a
}

/// Avatar with a presence dot in its corner (`ring`: the colour behind it).
pub fn avatar_dot(m: &Model, user: Id, size: f32, ring: Color) -> El {
    let (status, online) = m.users.get(&user).map_or((Status::Online, false), |u| (u.status, u.online));
    let d = (size * 0.34).round();
    let dot = el().size(d, d).radius(d / 2.0).fill(status_color(status, online)).border(2.5, ring).absolute(size - d + 2.0, size - d + 2.0);
    el().child(avatar(m, user, size, false)).child(dot)
}

pub fn glyph(i: &'static Icon, size: f32, c: Color) -> El {
    icon(i, size).color(c).a11y_hidden()
}

pub fn badge(label: impl Into<String>, c: Color) -> El {
    el().px(6.0)
        .h(18.0)
        .min_w(18.0)
        .radius(9.0)
        .fill(c)
        .center()
        .child(text(label.into()).font(SMALL).color(oklch(0.99, 0.0, 0.0)))
}

/// A circular icon button; `active` fills it with the accent.
pub fn round_button(key: &'static str, i: &'static Icon, size: f32, active: bool, fg: Color, label: &'static str) -> El {
    let p = pal();
    let b = el()
        .key(key)
        .role("button")
        .label(label)
        .help(label)
        .size(size, size)
        .radius(size / 2.0)
        .press_scale(0.94)
        .center()
        .child(glyph(i, (size * 0.48).round(), if active { p.on_accent } else { fg }));
    if active { b.fill(p.accent) } else { b.hover_fill(p.select) }
}

/// A text button. `kind`: 0 plain, 1 primary (accent), 2 danger.
pub fn button(key: impl Into<String>, label: impl Into<String>, kind: u8) -> El {
    let p = pal();
    let label = label.into();
    let fg = match kind {
        1 => p.on_accent,
        2 => oklch(0.99, 0.0, 0.0),
        _ => p.text,
    };
    let b = el()
        .key(key.into())
        .role("button")
        .label(label.clone())
        .h(34.0)
        .px(14.0)
        .radius(8.0)
        .press_scale(0.97)
        .center()
        .child(text(label).font(TextStyle::new(13.0, 600.0)).color(fg));
    match kind {
        1 => b.fill(p.accent).hover_fill(p.ink(0.08)),
        2 => b.fill(p.red).hover_fill(p.ink(0.08)),
        _ => b.fill(p.ink(0.06)).hover_fill(p.ink(0.1)),
    }
}

/// A text field.
pub fn field(input: El) -> El {
    let p = pal();
    input
        .h(40.0)
        .px(12.0)
        .radius(8.0)
        .fill(p.field)
        .border(1.0, p.line)
        .color(p.text)
        .single_line()
        .align_y(rune::Align::Center)
}

pub fn label(s: &'static str) -> El {
    text(s).font(SMALL).color(pal().faint)
}

fn backdrop() -> El {
    row().w_full().h_full().fill(pal().bg)
}

pub fn view(s: &State, cx: &mut Cx<State>) -> El {
    let dark = match s.app.theme {
        crate::desktop::ThemeChoice::System => cx.system_dark(),
        crate::desktop::ThemeChoice::Dark => true,
        crate::desktop::ThemeChoice::Light => false,
    };
    DARK.set(dark);
    let Some(m) = &s.m else {
        let root = backdrop().center().child(login::view(s));
        return with_toast(s, root);
    };
    let ch = m.channel(s.channel);
    let main = match ch {
        Some(c) if c.kind == ChannelKind::Voice => voice::view(s, m, c),
        Some(c) => chat::view(s, m, c),
        None => panel(col().grow(1.0).h_full().center()).child(text("No channels yet.").font(BODY).color(pal().muted)),
    };
    let mut root = backdrop().child(sidebar::view(s, m)).child(main);
    if ch.is_none_or(|c| c.kind == ChannelKind::Text) {
        root = root.child(members::view(s, m));
    }
    let mut layers = el().w_full().h_full().child(root);
    if let Some(st) = &s.settings {
        layers = layers.child(settings::view(s, m, st));
    }
    if let Some(pk) = voice::picker(s) {
        layers = layers.child(pk);
    }
    if let Some(menu) = audio_menu::layer(s) {
        layers = layers.child(menu);
    }
    with_toast(s, layers)
}

fn with_toast(s: &State, root: El) -> El {
    let p = pal();
    let mut layers = el().w_full().h_full().child(root.w_full().h_full());
    if let crate::Phase::Reconnecting { retry_in } = s.phase {
        layers = layers.child(
            row().key("reconnect").pin(Some(0.0), Some(18.0), Some(0.0), None).justify_center().child(
                row()
                    .px(14.0)
                    .h(32.0)
                    .gap(8.0)
                    .center_y()
                    .radius(16.0)
                    .fill(p.yellow)
                    .shadow(4.0, 16.0, oklcha(0.0, 0.0, 0.0, 0.2))
                    .child(text(format!("Connection lost — reconnecting in {retry_in}s…")).font(type_::TITLE).color(oklch(0.2, 0.02, 75.0))),
            ),
        );
    }
    if let Some((msg, _)) = &s.toast {
        layers = layers.child(
            // At the top, under the header: the bottom has the call controls and the composer.
            row().key("toast").pin(Some(0.0), Some(68.0), Some(0.0), None).justify_center().child(
                row()
                    .key("toast-pill")
                    .role("button")
                    .px(16.0)
                    .py(10.0)
                    .max_w(560.0)
                    .gap(8.0)
                    .center_y()
                    .radius(14.0)
                    .fill(p.surface)
                    .border(1.0, p.line)
                    .shadow(8.0, 24.0, oklcha(0.0, 0.0, 0.0, 0.25))
                    .transition(rune::Transition::offset(0.0, 16.0).combined(rune::Transition::opacity()))
                    .child(text(msg.clone()).font(type_::BODY).color(p.text).wrap())
                    .children(s.toast_watch.map(|u| button("toast-watch", "Watch", 1).h(30.0).on_press(move |s: &mut State| s.watch_stream(u))))
                    .on_press(|s: &mut State| s.toast = None),
            ),
        );
    }
    layers
}
