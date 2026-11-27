//! Members grouped by role, online first. Click someone for their card: role changes, kick and
//! ban, as far as your own role allows.

use kall_proto::{C2S, Id, Role, Status, User};
use rune::tok::type_;
use rune::{El, ScrollSpec, Transition, col, row, text};

use super::{SMALL, avatar_dot, button, pal, status_color, user_color};
use crate::State;
use crate::model::Model;

fn sub(u: &User) -> String {
    if !u.online {
        return "offline".into();
    }
    if !u.status_text.is_empty() {
        return u.status_text.clone();
    }
    match u.status {
        Status::Online => "online",
        Status::Idle => "idle",
        Status::Dnd => "do not disturb",
    }
    .into()
}

fn card(s: &State, m: &Model, u: &User) -> El {
    let p = pal();
    let my = m.my_role();
    let id = u.id;
    let mut c = col()
        .key(format!("card-{id}"))
        .w_full()
        .mt(4.0)
        .mb(6.0)
        .padding(12.0)
        .gap(8.0)
        .radius(12.0)
        .fill(p.surface)
        .border(1.0, p.line)
        .transition(Transition::offset(0.0, -6.0).combined(Transition::opacity()))
        .child(text(format!("@{}", u.username)).font(type_::LABEL).color(p.muted))
        .child(
            row()
                .gap(6.0)
                .center_y()
                .child(rune::el().size(8.0, 8.0).radius(4.0).fill(status_color(u.status, u.online)))
                .child(text(u.role.name()).font(type_::LABEL).color(user_color(m, id))),
        );
    if id == m.me {
        return c.child(text("That's you.").font(type_::LABEL).color(p.faint));
    }
    if my.outranks(u.role) {
        let mut roles = row().gap(4.0).flex_wrap();
        for role in [Role::Member, Role::Mod, Role::Admin] {
            if my.can_grant(role) && role != u.role {
                let label = format!("Make {}", role.name());
                roles = roles.child(button(format!("role-{role:?}"), label, 0).h(28.0).on_press(move |s: &mut State| s.send(C2S::SetRole { user: id, role })));
            }
        }
        c = c.child(roles);
        let mut mod_row = row().gap(6.0);
        if my.can_kick() && u.online {
            mod_row = mod_row.child(button("kick", "Kick", 0).h(28.0).on_press(move |s: &mut State| s.send(C2S::Kick { user: id })));
        }
        if my.can_ban() {
            if s.confirm_ban == Some(id) {
                mod_row = mod_row.child(button("ban-yes", "Really ban", 2).h(28.0).on_press(move |s: &mut State| {
                    s.send(C2S::Ban { user: id });
                    s.confirm_ban = None;
                    s.member_open = None;
                }));
            } else {
                mod_row = mod_row.child(button("ban", "Ban", 2).h(28.0).on_press(move |s: &mut State| s.confirm_ban = Some(id)));
            }
        }
        c = c.child(mod_row);
    }
    c
}

pub fn view(s: &State, m: &Model) -> El {
    let p = pal();
    let mut list = col().w_full().px(8.0).py(14.0).gap(2.0);
    let ring = p.side;
    let groups: [(&str, Box<dyn Fn(&User) -> bool>); 4] = [
        ("ADMINS", Box::new(|u: &User| u.online && u.role >= Role::Admin)),
        ("MODERATORS", Box::new(|u: &User| u.online && u.role == Role::Mod)),
        ("ONLINE", Box::new(|u: &User| u.online && u.role == Role::Member)),
        ("OFFLINE", Box::new(|u: &User| !u.online)),
    ];
    for (title, pick) in groups.iter() {
        let mut people: Vec<&User> = m.users.values().filter(|u| pick(u)).collect();
        if people.is_empty() {
            continue;
        }
        people.sort_by_key(|u| u.display_name.to_lowercase());
        list = list.child(text(format!("{title} — {}", people.len())).key(format!("members-{title}")).font(SMALL).color(p.faint).px(10.0).pt(8.0).pb(6.0));
        for u in people {
            let id: Id = u.id;
            let open = s.member_open == Some(id);
            list = list.child(
                row()
                    .key(format!("member-{id}"))
                    .role("button")
                    .label(u.display_name.clone())
                    .w_full()
                    .h(46.0)
                    .px(8.0)
                    .gap(10.0)
                    .center_y()
                    .radius(10.0)
                    .fill(if open { p.select } else { rune::Color::TRANSPARENT })
                    .hover_fill(p.hover)
                    .opacity(if u.online { 1.0 } else { 0.5 })
                    .child(avatar_dot(m, id, 34.0, ring))
                    .child(
                        col()
                            .grow(1.0)
                            .shrink()
                            .child(text(u.display_name.clone()).font(type_::TITLE).color(user_color(m, id)).truncate())
                            .child(text(sub(u)).font(type_::LABEL).color(p.faint).truncate()),
                    )
                    .on_press(move |s: &mut State| {
                        s.member_open = if s.member_open == Some(id) { None } else { Some(id) };
                        s.confirm_ban = None;
                    }),
            );
            if open {
                list = list.child(card(s, m, u));
            }
        }
    }
    col().w(232.0).h_full().fill(p.side).border_sides([0.0, 0.0, 0.0, 1.0], p.line).child(col().key("member-list").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec::default()).child(list))
}
