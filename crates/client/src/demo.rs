//! Demo data for headless snapshots (`--snap … --scene chat`), so the UI can be checked
//! without a server.

use kall_proto::{Channel, ChannelKind, Message, Preview, ReadState, Role, Snapshot, Status, StreamInfo, StreamKind, User, VoiceMember};

use crate::model::Model;
use crate::{State, Tab};

pub const SCENES: &[(&str, fn(&mut State))] = &[
    ("login", |s| {
        s.login.server = rune::TextEdit::new("kall.example.com");
        s.login.username = rune::TextEdit::new("sam");
    }),
    ("register", |s| {
        s.login.server = rune::TextEdit::new("kall.example.com");
        s.login.register = true;
        s.login.error = Some("That invite has expired.".into());
    }),
    ("chat", |s| load(s, 1)),
    ("reply", |s| {
        load(s, 1);
        s.reply_to = Some(4);
        s.hover_msg = Some(5);
        s.member_open = Some(2);
        s.draft = rune::TextEdit::new("sure, I'll bring the snacks");
    }),
    ("voice", |s| {
        load(s, 3);
        if let Some(m) = &mut s.m {
            m.voice.insert(1, VoiceMember { user: 1, channel: 3, muted: false, deafened: false });
        }
        s.voice_channel = Some(3);
        s.speaking = [1, 2].into_iter().collect();
        s.hover_tile = Some(2);
        s.voice.volumes.insert("kai".into(), 0.6);
    }),
    ("stream", |s| {
        // Mira shares a window (a drawn stand-in, not a real capture), Kai has a camera on,
        // and the "started sharing" toast is up.
        load(s, 3);
        if let Some(m) = &mut s.m {
            m.voice.insert(1, VoiceMember { user: 1, channel: 3, muted: false, deafened: false });
            m.streams.insert((2, StreamKind::Screen), StreamInfo { kind: StreamKind::Screen, width: 1920, height: 1080, fps: 30, audio: true, label: "Visual Studio Code".into() });
            m.streams.insert((4, StreamKind::Camera), StreamInfo { kind: StreamKind::Camera, width: 1280, height: 720, fps: 30, audio: false, label: "Camera".into() });
        }
        s.voice_channel = Some(3);
        s.speaking = [2].into_iter().collect();
        s.media.demo.insert((2, StreamKind::Screen), fake_screen(480, 270));
        s.media.demo.insert((4, StreamKind::Camera), fake_camera(320, 180));
        s.toast = Some(("Mira started sharing Visual Studio Code".into(), 1e9));
        s.toast_watch = Some(2);
    }),
    ("voice-settings", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::Voice);
        s.voice.mode = crate::voice::core::Mode::PushToTalk;
        s.devices = Some(crate::voice::devices::Devices {
            inputs: vec![crate::voice::devices::Device { id: "mic1".into(), name: "Microphone (Focusrite USB Audio)".into() }],
            outputs: vec![crate::voice::devices::Device { id: "out1".into(), name: "Headphones (Focusrite USB Audio)".into() }],
        });
        s.voice.input = Some("mic1".into());
    }),
    ("lobby", |s| load(s, 4)),
    ("settings", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::Profile);
    }),
    ("app-settings", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::App);
    }),
    ("server", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::Invites);
        if let Some(m) = &mut s.m {
            let inv = kall_proto::Invite { code: "q7mx4kd2ra".into(), uses_left: Some(1), expires: Some(now() + 86_400_000 * 7) };
            m.invites = vec![inv.clone(), kall_proto::Invite { code: "h3w9tnp2vc".into(), uses_left: None, expires: None }];
            m.last_invite = Some(inv);
        }
    }),
];

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn user(id: i64, name: &str, display: &str, role: Role, status: Status, text: &str, online: bool) -> User {
    User { id, username: name.into(), display_name: display.into(), role, status, status_text: text.into(), online, avatar_rev: 0 }
}

/// A stand-in for a shared window: a dark editor with coloured lines of "code".
pub fn fake_screen(w: u32, h: u32) -> rune::Pixels {
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let lines: &[(u32, u32, [u8; 3])] = &[(2, 18, [197, 134, 192]), (4, 30, [156, 220, 254]), (4, 22, [206, 145, 120]), (6, 34, [220, 220, 170]), (6, 12, [106, 153, 85]), (4, 26, [156, 220, 254]), (2, 8, [197, 134, 192])];
    for y in 0..h {
        for x in 0..w {
            let o = ((y * w + x) * 4) as usize;
            let mut c = if x < w / 6 { [37, 37, 38] } else { [30, 30, 30] };
            if y < h / 14 {
                c = [50, 50, 52];
            }
            let row = y.saturating_sub(h / 9) / (h / 22).max(1);
            if x >= w / 6 && y > h / 9 && (y - h / 9) % (h / 22).max(1) < (h / 44).max(1) {
                let (indent, len, col) = lines[row as usize % lines.len()];
                let x0 = w / 6 + w / 40 + indent * w / 120;
                if x >= x0 && x < x0 + len * w / 70 {
                    c = col;
                }
            }
            rgba[o..o + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    let px = rune::Pixels::new();
    px.replace(w, h, rgba);
    px
}

/// A stand-in for a webcam: a soft blue-to-amber gradient.
pub fn fake_camera(w: u32, h: u32) -> rune::Pixels {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let t = (x + y) as f32 / (w + h) as f32;
            rgba.extend([(60.0 + 150.0 * t) as u8, (80.0 + 60.0 * t) as u8, (140.0 - 80.0 * t) as u8, 255]);
        }
    }
    let px = rune::Pixels::new();
    px.replace(w, h, rgba);
    px
}

fn load(s: &mut State, channel: i64) {
    let t = now();
    let min = 60_000;
    let users = vec![
        user(1, "sam", "Sam", Role::Owner, Status::Online, "", true),
        user(2, "mira", "Mira", Role::Admin, Status::Online, "ranked grind", true),
        user(3, "theo", "theo", Role::Mod, Status::Idle, "", true),
        user(4, "kai", "Kai", Role::Member, Status::Dnd, "studying", true),
        user(5, "juno", "juno", Role::Member, Status::Online, "", true),
        user(6, "rex", "Rex", Role::Member, Status::Online, "", false),
    ];
    let msg = |id, author, ago: i64, body: &str, reply_to| Message { id, channel: 1, author, body: body.into(), reply_to, created: t - ago * min, edited: None, preview: None };
    let mut msgs = vec![
        msg(1, 2, 60 * 26, "anyone up for games tonight?", None),
        msg(2, 3, 60 * 26 - 2, "after 9 maybe", None),
        msg(3, 1, 52, "kall is **alive** 🎉 the server's running on the home box now", None),
        msg(4, 2, 50, "wait this is ours? no more discord nitro ads?", None),
        msg(5, 4, 49, "the `kall-server` image is tiny too", None),
        msg(6, 4, 48, "also: snacks for friday, who's bringing what @sam", None),
        msg(7, 5, 12, "I wrote up the plan here https://github.com/rust-lang/rust", None),
        msg(8, 1, 3, "I'll bring drinks", Some(6)),
        msg(9, 2, 1, "```rust\nfn main() {\n    println!(\"hello from kall\");\n}\n```", None),
    ];
    msgs[3].edited = Some(t);
    msgs[6].preview = Some(Preview {
        url: "https://github.com/rust-lang/rust".into(),
        site: "GitHub".into(),
        title: "rust-lang/rust: Empowering everyone to build reliable and efficient software.".into(),
        description: "Empowering everyone to build reliable and efficient software. Contribute to rust-lang/rust development by creating an account on GitHub.".into(),
    });
    let ch = |id, name: &str, kind, last| Channel { id, name: name.into(), kind, position: id, bitrate: 64_000, last_message: last };
    let snap = Snapshot {
        server_name: "The Basement".into(),
        me: 1,
        users,
        channels: vec![
            ch(1, "general", ChannelKind::Text, 9),
            ch(2, "memes", ChannelKind::Text, 20),
            ch(3, "Lounge", ChannelKind::Voice, 0),
            ch(4, "Gaming", ChannelKind::Voice, 0),
        ],
        read: vec![ReadState { channel: 1, last_read: 9, mentions: 0 }, ReadState { channel: 2, last_read: 12, mentions: 2 }],
        voice: vec![
            VoiceMember { user: 2, channel: 3, muted: false, deafened: false },
            VoiceMember { user: 4, channel: 3, muted: true, deafened: false },
            VoiceMember { user: 5, channel: 4, muted: false, deafened: true },
        ],
        streams: vec![],
    };
    let mut m = Model::new(snap);
    let log = m.logs.entry(1).or_default();
    log.msgs = msgs;
    log.loaded = true;
    log.complete = true;
    s.m = Some(m);
    s.phase = crate::Phase::Ready;
    s.channel = channel;
    s.divider = Some((1, 6));
}
