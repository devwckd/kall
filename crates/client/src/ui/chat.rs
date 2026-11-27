//! A text channel: history with markdown, replies, edits, link previews and the unread line,
//! hover actions, and the composer with reply/edit bars and the typing indicator.

use chrono::{Datelike, Local, TimeZone};
use kall_proto::{C2S, Channel, Message};
use rune::tok::type_;
use rune::{El, Key, ScrollSpec, TextStyle, Transition, col, el, oklcha, row, text, text_input};
use rune_kit::{Markdown, MarkdownStyle};

use super::{BODY, HEADER, NAME, accent, avatar, glyph, pal, panel, round_button, user_color};
use crate::model::Model;
use crate::{State, icons};

fn local(ms: i64) -> chrono::DateTime<Local> {
    Local.timestamp_millis_opt(ms).single().unwrap_or_else(Local::now)
}

fn day_label(ms: i64) -> String {
    let d = local(ms).date_naive();
    let today = Local::now().date_naive();
    if d == today {
        "Today".into()
    } else if Some(d) == today.pred_opt() {
        "Yesterday".into()
    } else if d.year() == today.year() {
        d.format("%A, %B %-d").to_string()
    } else {
        d.format("%B %-d, %Y").to_string()
    }
}

fn time_label(ms: i64) -> String {
    let t = local(ms);
    let d = t.date_naive();
    let today = Local::now().date_naive();
    if d == today {
        format!("Today at {}", t.format("%H:%M"))
    } else if Some(d) == today.pred_opt() {
        format!("Yesterday at {}", t.format("%H:%M"))
    } else {
        t.format("%d/%m/%Y %H:%M").to_string()
    }
}

/// `@name` of a known user → bold, outside code.
fn decorate(m: &Model, body: &str) -> String {
    let mut out = String::with_capacity(body.len() + 8);
    let mut in_code = false;
    let mut chars = body.char_indices().peekable();
    let mut prev = ' ';
    while let Some((i, c)) = chars.next() {
        if c == '`' {
            in_code = !in_code;
        }
        if c == '@' && !in_code && !prev.is_alphanumeric() {
            let rest = &body[i + 1..];
            let end = rest.find(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))).unwrap_or(rest.len());
            let name = rest[..end].trim_end_matches('.');
            if !name.is_empty() && (name == "everyone" || m.users.values().any(|u| u.username == name.to_lowercase())) {
                out.push_str("**@");
                out.push_str(name);
                out.push_str("**");
                for _ in 0..name.chars().count() {
                    chars.next();
                }
                prev = 'a';
                continue;
            }
        }
        out.push(c);
        prev = c;
    }
    out
}

/// This channel's notification setting; click to cycle.
fn bell(s: &State, channel: kall_proto::Id) -> El {
    let p = pal();
    let level = s.app.level(channel);
    let (ico, color) = match level {
        crate::desktop::NotifyLevel::Nothing => (&icons::BELL_OFF, p.faint),
        crate::desktop::NotifyLevel::All => (&icons::BELL, p.accent2),
        crate::desktop::NotifyLevel::Mentions => (&icons::BELL, p.muted),
    };
    row()
        .key("bell")
        .role("button")
        .label(format!("Notifications: {}", level.label()))
        .help("Notifications for this channel")
        .gap(6.0)
        .px(10.0)
        .h(30.0)
        .center_y()
        .radius(15.0)
        .hover_fill(p.hover)
        .press_scale(0.96)
        .child(glyph(ico, 16.0, color))
        .child(text(level.label()).font(type_::LABEL).color(color))
        .on_press(move |s: &mut State| {
            let next = s.app.level(channel).next();
            s.app.notify.insert(channel.to_string(), next);
            s.save_app();
        })
}

fn md_style() -> MarkdownStyle {
    let p = pal();
    MarkdownStyle::neutral(p.dark).text(p.text).muted(p.muted).link(p.accent2)
}

fn reply_line(m: &Model, msg: &Message) -> Option<El> {
    let p = pal();
    let target = msg.reply_to?;
    let (who, snippet) = match m.find(msg.channel, target) {
        Some(t) => (Some(t.author), t.body.lines().next().unwrap_or("").chars().take(90).collect::<String>()),
        None => (None, "Original message was deleted or isn't loaded".to_string()),
    };
    let mut r = row().key("reply").pl(54.0).gap(6.0).center_y().h(20.0).child(glyph(&icons::REPLY, 13.0, p.faint));
    if let Some(a) = who {
        r = r.child(avatar(m, a, 16.0, false)).child(text(m.name(a).to_string()).font(TextStyle::new(12.5, 600.0)).color(user_color(m, a)));
    }
    Some(r.child(text(snippet).font(type_::LABEL).color(p.faint).truncate().shrink()))
}

fn preview_card(msg: &Message) -> Option<El> {
    let p = pal();
    let pv = msg.preview.as_ref()?;
    let url = pv.url.clone();
    let mut c = col()
        .key("preview")
        .max_w(460.0)
        .mt(6.0)
        .pl(14.0)
        .pr(14.0)
        .py(10.0)
        .gap(4.0)
        .radius(10.0)
        .fill(p.surface)
        .border_sides([0.0, 0.0, 0.0, 3.0], p.accent2)
        .child(text(pv.site.clone()).font(type_::LABEL).color(p.faint))
        .child(
            text(pv.title.clone())
                .key("preview-title")
                .role("link")
                .font(TextStyle::new(14.0, 650.0))
                .color(p.accent2)
                .wrap()
                .on_press_tx(move |_: &mut State, tx| {
                    tx.open_url(&url).ok();
                }),
        );
    if !pv.description.is_empty() {
        c = c.child(text(pv.description.clone()).font(type_::BODY).color(p.muted).wrap().max_lines(3));
    }
    Some(c)
}

fn actions(s: &State, m: &Model, msg: &Message) -> El {
    let p = pal();
    let id = msg.id;
    let mine = msg.author == m.me;
    let author_role = m.users.get(&msg.author).map(|u| u.role);
    let can_delete = mine || (m.my_role().can_delete_any() && author_role.is_none_or(|r| m.my_role().outranks(r)));
    let mut bar = row()
        .key("actions")
        .pin(None, Some(-14.0), Some(12.0), None)
        .padding(3.0)
        .gap(2.0)
        .radius(10.0)
        .fill(p.panel)
        .border(1.0, p.ink(0.1))
        .shadow(2.0, 10.0, oklcha(0.0, 0.0, 0.0, 0.12));
    if s.confirm_delete == Some(id) {
        return bar
            .child(text("Delete this message?").font(type_::LABEL).color(p.text).px(8.0).center_y())
            .child(super::button("del-yes", "Delete", 2).h(28.0).on_press(move |s: &mut State| {
                s.send(C2S::DeleteMessage { id });
                s.confirm_delete = None;
            }))
            .child(super::button("del-no", "Cancel", 0).h(28.0).on_press(|s: &mut State| s.confirm_delete = None));
    }
    bar = bar.child(round_button("reply", &icons::REPLY, 28.0, false, p.muted, "Reply").on_press(move |s: &mut State| {
        s.editing = None;
        s.reply_to = Some(id);
    }));
    if mine {
        let body = msg.body.clone();
        bar = bar.child(round_button("edit", &icons::PENCIL, 28.0, false, p.muted, "Edit").on_press(move |s: &mut State| {
            s.reply_to = None;
            s.editing = Some(id);
            s.draft.replace_text(&body);
        }));
    }
    if can_delete {
        bar = bar.child(round_button("delete", &icons::TRASH, 28.0, false, p.red, "Delete").on_press(move |s: &mut State| s.confirm_delete = Some(id)));
    }
    bar
}

pub fn view(s: &State, m: &Model, ch: &Channel) -> El {
    let p = pal();
    let header = row()
        .w_full()
        .h(52.0)
        .px(18.0)
        .gap(10.0)
        .center_y()
        .border_sides([0.0, 0.0, 1.0, 0.0], p.line)
        .child(glyph(&icons::HASH, 20.0, p.accent2))
        .child(text(ch.name.clone()).font(HEADER).color(p.text))
        .child(el().grow(1.0))
        .child(bell(s, ch.id));

    let log = m.logs.get(&ch.id);
    let mut list = col().w_full().gap(0.0).px(10.0).py(14.0);
    let complete = log.is_some_and(|l| l.complete);
    if complete {
        let hero = accent(el().size(64.0, 64.0).radius(22.0))
            .center()
            .child(glyph(&icons::HASH, 34.0, p.on_accent));
        list = list.child(
            col()
                .key("welcome")
                .gap(6.0)
                .px(12.0)
                .pb(18.0)
                .child(hero)
                .child(text(format!("Welcome to #{}", ch.name)).font(TextStyle::new(28.0, 750.0)).color(p.text).pt(10.0))
                .child(text(format!("This is the very beginning of #{}.", ch.name)).font(type_::BODY).color(p.muted)),
        );
    } else if log.is_some_and(|l| l.loaded) {
        let loading = log.is_some_and(|l| l.loading);
        list = list.child(
            row().key("older").w_full().justify_center().pb(10.0).child(
                super::button("load-older", if loading { "Loading…" } else { "Load older messages" }, 0).on_press(|s: &mut State| s.load_older()),
            ),
        );
    } else {
        list = list.child(text("Loading…").key("loading").font(type_::BODY).color(p.faint).padding(16.0));
    }

    let divider_after = s.divider.filter(|(c, _)| *c == ch.id).map(|(_, id)| id);
    let mut prev: Option<&Message> = None;
    let style = md_style();
    for msg in log.map(|l| l.msgs.as_slice()).unwrap_or_default() {
        let new_day = prev.is_none_or(|pm| local(pm.created).date_naive() != local(msg.created).date_naive());
        if new_day {
            list = list.child(
                row()
                    .key(format!("day-{}", msg.id))
                    .w_full()
                    .gap(10.0)
                    .center_y()
                    .py(12.0)
                    .px(12.0)
                    .child(el().grow(1.0).h(1.0).fill(p.line))
                    .child(text(day_label(msg.created)).font(type_::LABEL).color(p.faint))
                    .child(el().grow(1.0).h(1.0).fill(p.line)),
            );
        }
        let first_unread = divider_after.is_some_and(|d| msg.id > d && prev.is_none_or(|pm| pm.id <= d)) && msg.author != m.me;
        if first_unread {
            list = list.child(
                row()
                    .key("new-line")
                    .w_full()
                    .gap(8.0)
                    .center_y()
                    .py(6.0)
                    .px(12.0)
                    .child(el().grow(1.0).h(1.0).fill(p.red.alpha(0.6)))
                    .child(super::badge("NEW", p.red)),
            );
        }
        let grouped = !new_day
            && !first_unread
            && msg.reply_to.is_none()
            && prev.is_some_and(|pm| pm.author == msg.author && msg.created - pm.created < 5 * 60 * 1000);
        prev = Some(msg);

        let mention = msg.author != m.me && m.mentions_me(&msg.body);
        let body = Markdown::new(decorate(m, &msg.body)).render(&style);
        let mut content = col().grow(1.0).shrink().gap(2.0);
        if !grouped {
            let mut head = row()
                .gap(8.0)
                .center_y()
                .child(text(m.name(msg.author).to_string()).font(NAME).color(user_color(m, msg.author)))
                .child(text(time_label(msg.created)).font(type_::LABEL).color(p.faint));
            if let Some(u) = m.users.get(&msg.author)
                && u.role >= kall_proto::Role::Mod
            {
                head = head.child(text(u.role.name().to_uppercase()).font(super::SMALL).color(user_color(m, msg.author).alpha(0.8)));
            }
            content = content.child(head);
        }
        let mut body_row = col().w_full();
        if !msg.body.is_empty() {
            body_row = body_row.child(body);
        }
        if msg.edited.is_some() {
            body_row = body_row.child(text("(edited)").font(TextStyle::new(11.0, 400.0)).color(p.faint));
        }
        content = content.child(body_row).children(preview_card(msg)).children(super::attachments::in_message(s, msg));
        let lead = if grouped { el().w(42.0) } else { avatar(m, msg.author, 42.0, false) };
        let id = msg.id;
        let editing = s.editing == Some(id);
        let mut item = col()
            .key(format!("msg-{id}"))
            .w_full()
            .mt(if grouped { 0.0 } else { 10.0 })
            .radius(10.0)
            .transition(Transition::offset(0.0, 14.0).combined(Transition::opacity()))
            .on_hover_change(move |s: &mut State, inside, _| {
                if inside {
                    s.hover_msg = Some(id);
                } else if s.hover_msg == Some(id) {
                    s.hover_msg = None;
                    if s.confirm_delete == Some(id) {
                        s.confirm_delete = None;
                    }
                }
            })
            .children(if grouped { None } else { reply_line(m, msg) })
            .child(row().w_full().px(12.0).py(if grouped { 3.0 } else { 5.0 }).gap(14.0).child(lead).child(content));
        item = if mention {
            item.fill(p.yellow.alpha(0.08)).border_sides([0.0, 0.0, 0.0, 3.0], p.yellow)
        } else if editing || s.reply_to == Some(id) {
            item.fill(p.accent.alpha(0.1))
        } else {
            item.hover_fill(p.ink(0.035))
        };
        if s.hover_msg == Some(id) || s.confirm_delete == Some(id) {
            item = item.child(actions(s, m, msg));
        }
        list = list.child(item);
    }

    let mut pane = panel(col().grow(1.0).shrink().h_full())
        .child(header)
        .child(
            col()
                .key("messages")
                .w_full()
                .grow(1.0)
                .min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: true })
                .scroll_edge_fade(20.0)
                .child(list),
        )
        .child(composer(s, m, ch))
        // Drop files anywhere on the chat to attach them.
        .on_drop_files(|s: &mut State, files, _| {
            s.files.drop_hover = false;
            s.attach_paths(files);
        })
        .on_drop_hover(|s: &mut State, over, _| s.files.drop_hover = over);
    if s.files.drop_hover {
        pane = pane.child(
            el().key("drop-here")
                .pin(Some(12.0), Some(12.0), Some(12.0), Some(12.0))
                .radius(14.0)
                .fill(p.accent.alpha(0.12))
                .border(2.0, p.accent)
                .center()
                .child(
                    col()
                        .gap(8.0)
                        .items_center()
                        .child(glyph(&icons::PAPERCLIP, 32.0, p.accent2))
                        .child(text("Drop files to upload").font(TextStyle::new(17.0, 650.0)).color(p.text))
                        .child(text(format!("Up to {} each", kall_proto::files::human_size(m.limits.max_bytes))).font(type_::LABEL).color(p.muted)),
                ),
        );
    }
    pane
}

fn composer(s: &State, m: &Model, ch: &Channel) -> El {
    let p = pal();
    let mut bars = col().w_full().px(16.0);
    let bar = |key: &'static str, label: String| {
        row()
            .key(key)
            .w_full()
            .h(34.0)
            .px(14.0)
            .gap(8.0)
            .center_y()
            .radius(12.0)
            .fill(p.surface)
            .mb(-8.0)
            .pb(8.0)
            .child(text(label).font(type_::LABEL).color(p.muted).grow(1.0).truncate())
            .child(round_button("cancel", &icons::X, 24.0, false, p.muted, "Cancel").on_press(|s: &mut State| {
                if s.editing.take().is_some() {
                    s.draft.clear();
                }
                s.reply_to = None;
            }))
    };
    if let Some(id) = s.editing {
        let _ = id;
        bars = bars.child(bar("editing-bar", "Editing message — Esc to cancel".into()));
    } else if let Some(r) = s.reply_to.and_then(|id| m.find(ch.id, id)) {
        bars = bars.child(bar("reply-bar", format!("Replying to {}", m.name(r.author))));
    }

    let me = m.me;
    let composer = text_input(&s.draft, BODY)
        .single_line()
        .key("composer")
        .label("Message")
        .align_y(rune::Align::Center)
        .placeholder(format!("Message #{}", ch.name), p.faint)
        .color(p.text)
        .grow(1.0)
        .h(48.0)
        .px(16.0)
        .on_edit(|s: &mut State| &mut s.draft)
        .on_input_change(|s: &mut State, _| s.typed())
        .on_input_key(move |s: &mut State, k, _| match k.key {
            Key::Escape if s.editing.is_some() || s.reply_to.is_some() => {
                if s.editing.take().is_some() {
                    s.draft.clear();
                }
                s.reply_to = None;
                true
            }
            Key::Up if s.draft.text.is_empty() => {
                let last = s.m.as_ref().and_then(|m| m.logs.get(&s.channel)).and_then(|l| l.msgs.iter().rev().find(|x| x.author == me)).map(|x| (x.id, x.body.clone()));
                if let Some((id, body)) = last {
                    s.reply_to = None;
                    s.editing = Some(id);
                    s.draft.replace_text(&body);
                    return true;
                }
                false
            }
            _ => false,
        })
        // Pasting an image (a screenshot) or files attaches them; text pastes as usual.
        .on_input_paste(|s: &mut State, content, _| {
            if let Some(img) = content.image() {
                s.attach_png(img.png());
                true
            } else if !content.files().is_empty() {
                let files = content.files().to_vec();
                s.attach_paths(&files);
                true
            } else {
                false
            }
        })
        .on_submit(|s: &mut State, tx| s.send_draft(tx));
    let attach = round_button("attach", &icons::PAPERCLIP, 36.0, false, p.muted, "Attach files").on_press_tx(|_: &mut State, tx| {
        tx.open_files(rune::FileDialogOptions { title: "Attach files".into(), ..Default::default() }, |s: &mut State, res, _| match res {
            Ok(Some(paths)) => s.attach_paths(&paths),
            Ok(None) => {}
            Err(e) => s.toast(format!("Couldn't open the file dialog: {e}")),
        });
    });
    let ready = s.files.pending.iter().any(|p| matches!(p.state, crate::files::Up::Ready(_)));
    let can_send = (!s.draft.text.trim().is_empty() || ready) && !s.uploads_busy();
    let send_btn = round_button("send", &icons::SEND, 36.0, can_send, p.faint, "Send").on_press_tx(|s: &mut State, tx| s.send_draft(tx));
    let field = row()
        .w_full()
        .pr(6.0)
        .gap(6.0)
        .center_y()
        .radius(10.0)
        .fill(p.surface)
        .border(1.0, p.line)
        .pl(4.0)
        .child(attach)
        .child(composer)
        .child(send_btn);

    let typers = m.typing_in(ch.id, s.now);
    let typing = match typers.as_slice() {
        [] => String::new(),
        [a] => format!("{} is typing…", m.name(*a)),
        [a, b] => format!("{} and {} are typing…", m.name(*a), m.name(*b)),
        _ => "Several people are typing…".into(),
    };
    bars.children(super::attachments::chips(s)).child(field).child(row().key("typing").w_full().h(22.0).px(6.0).center_y().child(text(typing).font(type_::LABEL).color(p.muted)))
}
