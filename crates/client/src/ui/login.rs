//! Log in, or create an account with an invite code.

use rune::tok::type_;
use rune::{El, TextStyle, col, el, oklcha, row, text, text_input};

use super::{BODY, button, field, glyph, label, pal, panel};
use crate::net::NetCmd;
use crate::{Phase, State, icons};

fn tab(key: &'static str, title: &'static str, on: bool, register: bool) -> El {
    let p = pal();
    let t = el()
        .key(key)
        .role("tab")
        .grow(1.0)
        .h(34.0)
        .radius(9.0)
        .center()
        .press_scale(0.97)
        .child(text(title).font(type_::TITLE).color(if on { p.text } else { p.faint }))
        .on_press(move |s: &mut State| {
            s.login.register = register;
            s.login.error = None;
        });
    if on { t.fill(if p.dark { p.ink(0.1) } else { p.panel }).shadow(1.0, 4.0, oklcha(0.0, 0.0, 0.0, if p.dark { 0.3 } else { 0.08 })) } else { t.hover_fill(p.hover) }
}

pub fn view(s: &State) -> El {
    let p = pal();
    let f = &s.login;
    let busy = matches!(s.phase, Phase::Connecting { .. });

    let logo = glyph(&icons::GHOST, 96.0, p.text);
    let title = col()
        .gap(2.0)
        .items_center()
        .child(text("kall").font(TextStyle::new(26.0, 700.0)).color(p.text))
        .child(text("Voice and chat for you and your friends").font(type_::BODY).color(p.muted));

    let tabs = row()
        .w_full()
        .padding(3.0)
        .gap(3.0)
        .radius(10.0)
        .fill(p.ink(0.05))
        .child(tab("tab-login", "Log in", !f.register, false))
        .child(tab("tab-register", "Create account", f.register, true));

    let input = |key: &'static str, edit: &rune::TextEdit, placeholder: &'static str, pick: fn(&mut State) -> &mut rune::TextEdit| {
        field(text_input(edit, BODY).key(key).label(placeholder).placeholder(placeholder, p.faint).w_full().on_edit(pick).on_submit(|s: &mut State, _| s.connect()))
    };

    let mut form = col()
        .w_full()
        .gap(8.0)
        .child(label("SERVER"))
        .child(input("server", &f.server, "kall.example.com", |s| &mut s.login.server));
    if f.register {
        form = form.child(label("INVITE CODE").pt(6.0)).child(input("invite", &f.invite, "Paste the code a friend sent you", |s| &mut s.login.invite));
    }
    form = form
        .child(label("USERNAME").pt(6.0))
        .child(input("username", &f.username, "username", |s| &mut s.login.username))
        .child(label("PASSWORD").pt(6.0))
        .child(input("password", &f.password, if f.register { "At least 8 characters" } else { "password" }, |s| &mut s.login.password).secure());

    let action = if busy {
        "Connecting…"
    } else if f.register {
        "Create account"
    } else {
        "Log in"
    };
    let mut submit = button("submit", action, 1).w_full().h(42.0);
    if !busy {
        submit = submit.on_press(|s: &mut State| s.connect());
    } else {
        submit = submit.opacity(0.7);
    }

    let mut card = panel(col().w(380.0).padding(28.0).gap(16.0).items_center())
        .radius(14.0)
        .fill(p.surface)
        .border(1.0, p.line)
        .child(logo)
        .child(title)
        .child(tabs)
        .child(form)
        .child(submit);

    if let Some(err) = &f.error {
        let mut e = col()
            .key("error")
            .w_full()
            .gap(8.0)
            .padding(10.0)
            .radius(10.0)
            .fill(p.red.alpha(0.12))
            .border(1.0, p.red.alpha(0.3))
            .child(text(err.clone()).font(type_::BODY).color(p.red).wrap());
        if f.pin_mismatch {
            e = e.child(
                text("If you know the server's certificate was replaced, forget the old one:").font(type_::LABEL).color(p.muted).wrap(),
            );
            e = e.child(button("forget-pin", "Forget pinned certificate", 2).on_press(|s: &mut State| {
                let server = s.login.server.text.trim().to_string();
                s.cmd(NetCmd::ForgetPin(server));
                s.login.pin_mismatch = false;
                s.login.error = None;
            }));
        }
        card = card.child(e);
    }
    if let Phase::Connecting { server, attempt } = &s.phase {
        let msg = if *attempt == 0 { format!("Connecting to {server}…") } else { format!("Reconnecting to {server}…") };
        card = card.child(row().gap(8.0).center_y().child(text(msg).font(type_::LABEL).color(p.faint)));
    }
    card
}
