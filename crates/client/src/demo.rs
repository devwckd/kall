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
    ("stream-watching", |s| {
        // Watching Mira's share on the stage: her tile in the strip offers Stop watching.
        let stream = SCENES.iter().find(|(n, _)| *n == "stream").unwrap().1;
        stream(s);
        s.toast = None;
        s.toast_watch = None;
        s.media.focus = Some(2);
    }),
    ("share-screens", |s| share_picker(s, crate::video::capture::SourceKind::Display)),
    ("share-windows", |s| share_picker(s, crate::video::capture::SourceKind::Window)),
    ("mic-menu", |s| {
        // Right-clicked the call's mic button.
        load(s, 3);
        if let Some(m) = &mut s.m {
            m.voice.insert(1, VoiceMember { user: 1, channel: 3, muted: false, deafened: false });
        }
        s.voice_channel = Some(3);
        s.devices = Some(demo_devices());
        s.voice.input = Some("mic1".into());
        s.voice.input_volume = 1.25;
        s.audio_menu = Some(crate::ui::audio_menu::Open { side: crate::ui::audio_menu::Side::Input, x: 613.0, y: 700.0 });
    }),
    ("speaker-menu", |s| {
        // Right-clicked the headphones in the sidebar's user panel.
        load(s, 1);
        s.devices = Some(demo_devices());
        s.voice.output_volume = 0.8;
        s.audio_menu = Some(crate::ui::audio_menu::Open { side: crate::ui::audio_menu::Side::Output, x: 232.0, y: 746.0 });
    }),
    ("voice-settings", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::Voice);
        s.voice.mode = crate::voice::core::Mode::PushToTalk;
        s.devices = Some(crate::voice::devices::Devices {
            inputs: vec![crate::voice::devices::Device { id: "mic1".into(), name: "Microphone (USB Audio Interface)".into() }],
            outputs: vec![crate::voice::devices::Device { id: "out1".into(), name: "Headphones (USB Audio Interface)".into() }],
        });
        s.voice.input = Some("mic1".into());
    }),
    ("lobby", |s| load(s, 4)),
    ("settings", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::Profile);
    }),
    ("profile-new-avatar", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::Profile);
        if let Some(set) = &mut s.settings {
            set.avatar = Some((Vec::new(), fake_camera(128, 128)));
        }
    }),
    ("avatar-crop", |s| {
        load(s, 1);
        crate::ui::settings::open(s, Tab::Profile);
        // A drawn stand-in "photo": a warm gradient with a lighter disc off-centre.
        let (w, h) = (600u32, 400u32);
        let rgba = (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let t = (x + y) as f32 / (w + h) as f32;
                    let (dx, dy) = (x as f32 - 380.0, y as f32 - 170.0);
                    let disc = if dx * dx + dy * dy < 90.0 * 90.0 { 60.0 } else { 0.0 };
                    [(70.0 + 150.0 * t + disc).min(255.0) as u8, (90.0 + 60.0 * t + disc).min(255.0) as u8, (150.0 - 80.0 * t + disc).min(255.0) as u8, 255]
                })
            })
            .collect();
        let mut crop = crate::ui::avatar_crop::Crop::new(w, h, rgba);
        crop.zoom = 1.6;
        crop.cx = 380.0;
        crop.cy = 170.0;
        crop.clamp();
        if let Some(set) = &mut s.settings {
            set.crop = Some(crop);
        }
    }),
    ("files", |s| {
        // Every kind of attachment, and files waiting in the composer (made-up files and drawn
        // pictures, not real captures).
        load(s, 1);
        use crate::files::{Dl, DlState, Pending, Up};
        use kall_proto::files::{Attachment, FileKind};
        let att = |id, name: &str, size, kind, w, h, thumb, expired| Attachment { id, name: name.into(), size, kind, width: w, height: h, thumb, expired };
        let shot = att(900, "screenshot.png", 812_000, FileKind::Image, 1600, 900, true, false);
        let clip = att(901, "clutch-round.mp4", 48_200_000, FileKind::Video, 0, 0, false, false);
        let song = att(902, "lofi-demo.mp3", 6_100_000, FileKind::Audio, 0, 0, false, false);
        let doc = att(903, "dungeon-map.pdf", 2_400_000, FileKind::Other, 0, 0, false, false);
        let old = att(904, "old-build.zip", 61_000_000, FileKind::Other, 0, 0, false, true);
        s.files.thumbs.insert(900, fake_screen(480, 270));
        s.files.cache.insert(901, Dl { done: 19_000_000, total: 48_200_000, state: DlState::Running });
        s.files.saved.insert(903, Dl { done: 2_400_000, total: 2_400_000, state: DlState::Done(std::path::PathBuf::from("Downloads/dungeon-map.pdf")) });
        if let Some(m) = &mut s.m {
            let t = now();
            let log = m.logs.entry(1).or_default();
            log.msgs.retain(|m| m.id < 6);
            let mk = |id, author, body: &str, attachments| Message { id, channel: 1, author, body: body.into(), reply_to: None, created: t - 60_000, edited: None, preview: None, attachments };
            log.msgs.push(mk(20, 2, "look at this", vec![shot]));
            log.msgs.push(mk(21, 4, "", vec![clip, song]));
            log.msgs.push(mk(22, 1, "map for friday", vec![doc, old]));
        }
        s.files.pending = vec![
            Pending { job: 1, name: "vacation.jpg".into(), size: 3_400_000, done: 1_500_000, state: Up::Sending },
            Pending { job: 2, name: "notes.txt".into(), size: 1_200, done: 1_200, state: Up::Ready(att(905, "notes.txt", 1_200, FileKind::Other, 0, 0, false, false)) },
            Pending { job: 3, name: "huge-video.mkv".into(), size: 300_000_000, done: 0, state: Up::Failed("Files can be up to 100 MB.".into()) },
        ];
    }),
    ("image-viewer", |s| {
        load(s, 1);
        let shot = kall_proto::files::Attachment { id: 900, name: "screenshot.png".into(), size: 812_000, kind: kall_proto::files::FileKind::Image, width: 1600, height: 900, thumb: true, expired: false };
        s.files.full.lock().unwrap().insert(900, fake_screen(1600, 900));
        s.files.viewer = Some(shot);
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

/// The share picker over a call, with made-up sources and drawn thumbnails (never the real
/// screens or windows of the machine taking the snapshot).
fn share_picker(s: &mut State, tab: crate::video::capture::SourceKind) {
    use crate::video::capture::{Source, SourceKind};
    load(s, 3);
    let src = |id: &str, name: &str, detail: &str, kind: SourceKind| Source { id: id.into(), name: name.into(), detail: detail.into(), kind, pid: None };
    let sources = vec![
        src("d:1", "Display 1", "2560×1440", SourceKind::Display),
        src("d:2", "Display 2", "1920×1080", SourceKind::Display),
        src("w:1", "main.rs — kall", "Code", SourceKind::Window),
        src("w:2", "Pasta recipes — Browser", "Browser", SourceKind::Window),
        src("w:3", "Lo-fi beats", "Music", SourceKind::Window),
        src("w:4", "Dungeon Crawler", "Game", SourceKind::Window),
        src("w:5", "Notes", "Notes", SourceKind::Window),
    ];
    let mut pk = crate::media::Picker::new(Some(sources));
    {
        let mut t = pk.thumbs.lock().unwrap();
        t.insert("d:1".into(), fake_screen(400, 225));
        t.insert("d:2".into(), fake_camera(400, 225));
        t.insert("w:1".into(), fake_screen(400, 250));
        t.insert("w:2".into(), fake_camera(400, 260));
        t.insert("w:4".into(), fake_camera(400, 225));
    }
    pk.tab = tab.clone();
    if tab == SourceKind::Window {
        pk.selected = Some("w:2".into());
    }
    s.media.picker = Some(pk);
}

fn demo_devices() -> crate::voice::devices::Devices {
    use crate::voice::devices::Device;
    crate::voice::devices::Devices {
        inputs: vec![Device { id: "mic1".into(), name: "Microphone (USB Audio Interface)".into() }, Device { id: "mic2".into(), name: "Headset Microphone".into() }],
        outputs: vec![Device { id: "out1".into(), name: "Headphones (USB Audio Interface)".into() }, Device { id: "out2".into(), name: "Speakers".into() }],
    }
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
    let msg = |id, author, ago: i64, body: &str, reply_to| Message { id, channel: 1, author, body: body.into(), reply_to, created: t - ago * min, edited: None, preview: None, attachments: vec![] };
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
        files: kall_proto::files::FileLimits { max_bytes: 100_000_000, keep_days: 30 },
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
