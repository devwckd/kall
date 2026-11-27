//! kall: the desktop client. One server, your friends, text and voice.
//!
//! `cargo run -p kall` opens the window. Headless snapshots use demo data and no network:
//! `cargo run -p kall -- --snap target/chat.png --scene chat` (scenes: login, chat, voice,
//! settings, server).

// Release builds are a windowed app on Windows: no console window next to kall. Dev builds
// keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#![forbid(unsafe_code)]

mod demo;
mod desktop;
mod media;
mod icons;
mod model;
mod net;
mod ui;
mod update;
mod video;
mod voice;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use kall_proto::{C2S, ChannelKind, Id, S2C, Status, StreamKind};
use rune::{AppSpec, TextEdit, Tx};

use crate::model::Model;
use crate::net::{Auth, Net, NetCmd, NetEvent};
use crate::voice::VoicePrefs;
use crate::voice::core::Settings as VoiceSettings;
use crate::voice::engine::{Engine, Shared};
use crate::voice::sounds::Cue;

pub enum Phase {
    Login,
    Connecting { server: String, attempt: u32 },
    Ready,
    Reconnecting { retry_in: u64 },
}

pub struct LoginForm {
    pub server: TextEdit,
    pub username: TextEdit,
    pub password: TextEdit,
    pub invite: TextEdit,
    pub register: bool,
    pub error: Option<String>,
    pub pin_mismatch: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Profile,
    Voice,
    App,
    Channels,
    Invites,
}

pub struct Settings {
    pub tab: Tab,
    pub display_name: TextEdit,
    pub status: Status,
    pub status_text: TextEdit,
    pub new_channel: TextEdit,
    pub new_kind: ChannelKind,
    /// Channel being renamed, and the draft name.
    pub rename: Option<(Id, TextEdit)>,
    pub confirm_delete: Option<Id>,
    pub invite_uses: Option<u32>,
    pub invite_hours: Option<u32>,
    /// A picked avatar not saved yet: the PNG to upload, and its picture for the preview.
    pub avatar: Option<(Vec<u8>, rune::Pixels)>,
}

pub struct State {
    pub net: Option<Net>,
    pub phase: Phase,
    pub login: LoginForm,
    pub m: Option<Model>,
    /// The open channel.
    pub channel: Id,
    /// (channel, last read when it was opened): where the "new" line goes.
    pub divider: Option<(Id, Id)>,
    pub draft: TextEdit,
    pub reply_to: Option<Id>,
    pub editing: Option<Id>,
    pub hover_msg: Option<Id>,
    pub confirm_delete: Option<Id>,
    pub member_open: Option<Id>,
    pub confirm_ban: Option<Id>,
    pub settings: Option<Settings>,
    pub toast: Option<(String, f64)>,
    /// The toast is about this person's new screen share: it gets a Watch button.
    pub toast_watch: Option<Id>,
    pub now: f64,
    last_typing: f64,
    pub voice: VoicePrefs,
    pub shared: Arc<Shared>,
    engine: Option<RunningVoice>,
    /// Who's audible right now (including me while I'm sending).
    pub speaking: HashSet<Id>,
    pub devices: Option<crate::voice::devices::Devices>,
    devices_rx: Option<std::sync::mpsc::Receiver<crate::voice::devices::Devices>>,
    pub hover_tile: Option<Id>,
    waker: Option<rune::Waker>,
    /// The voice channel *this window* joined. The server may show me in voice from another
    /// session; only this decides whether the engine runs here.
    pub voice_channel: Option<Id>,
    /// Re-joining after a reconnect: a stale "left voice" from the old connection is ignored.
    rejoining: bool,
    pub media: media::Media,
    pub app: desktop::AppPrefs,
    /// What the tray icon shows now: (badge, in voice, muted).
    tray_state: Option<(Option<u32>, bool, bool)>,
    /// A newer kall, found by the background check.
    pub update: Option<cargo_packager_updater::Update>,
    update_rx: Option<std::sync::mpsc::Receiver<cargo_packager_updater::Update>>,
    pub updating: bool,
    update_failed: Option<std::sync::mpsc::Receiver<String>>,
}

struct RunningVoice {
    _engine: Engine,
    input: Option<String>,
    output: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            net: None,
            phase: Phase::Login,
            login: LoginForm {
                server: TextEdit::new(""),
                username: TextEdit::new(""),
                password: TextEdit::new(""),
                invite: TextEdit::new(""),
                register: false,
                error: None,
                pin_mismatch: false,
            },
            m: None,
            channel: 0,
            divider: None,
            draft: TextEdit::new(""),
            reply_to: None,
            editing: None,
            hover_msg: None,
            confirm_delete: None,
            member_open: None,
            confirm_ban: None,
            settings: None,
            toast: None,
            toast_watch: None,
            now: 0.0,
            last_typing: -10.0,
            voice: VoicePrefs::default(),
            shared: Shared::new(VoiceSettings::default(), "LAlt".into()),
            engine: None,
            speaking: HashSet::new(),
            devices: None,
            devices_rx: None,
            hover_tile: None,
            waker: None,
            voice_channel: None,
            rejoining: false,
            media: media::Media::default(),
            app: desktop::AppPrefs::default(),
            tray_state: None,
            update: None,
            update_rx: None,
            updating: false,
            update_failed: None,
        }
    }
}

impl State {
    pub fn send(&self, msg: C2S) {
        if let Some(n) = &self.net {
            n.c2s(msg);
        }
    }

    pub fn cmd(&self, cmd: NetCmd) {
        if let Some(n) = &self.net {
            n.send(cmd);
        }
    }

    pub fn toast(&mut self, msg: impl Into<String>) {
        self.toast = Some((msg.into(), self.now + 5.0));
        self.toast_watch = None;
    }

    /// Jump to `user`'s screen share: open the call's channel and put it on the stage.
    pub fn watch_stream(&mut self, user: Id) {
        if let Some(ch) = self.m.as_ref().and_then(|m| m.voice.get(&user)).map(|v| v.channel) {
            self.select_channel(ch);
        }
        self.focus_stream(Some(user));
        self.toast = None;
        self.toast_watch = None;
    }

    pub fn connect(&mut self) {
        let f = &self.login;
        let server = f.server.text.trim().to_string();
        let username = f.username.text.trim().to_lowercase();
        let password = f.password.text.clone();
        if server.is_empty() || username.is_empty() || password.is_empty() || (f.register && f.invite.text.trim().is_empty()) {
            self.login.error = Some("Fill in every field.".into());
            return;
        }
        let auth = if f.register {
            Auth::Register { invite: f.invite.text.trim().to_string(), username, password }
        } else {
            Auth::Login { username, password }
        };
        self.login.error = None;
        self.cmd(NetCmd::Connect { server, auth });
    }

    pub fn select_channel(&mut self, id: Id) {
        let Some(m) = &self.m else { return };
        let Some(ch) = m.channel(id) else { return };
        if ch.kind == ChannelKind::Voice {
            if self.voice_channel != Some(id) {
                self.join_voice(id);
            }
        } else {
            let last_read = m.read.get(&id).map_or(0, |r| r.last_read);
            self.divider = m.unread(ch).then_some((id, last_read));
        }
        if self.channel != id {
            self.reply_to = None;
            self.editing = None;
            self.draft.clear();
        }
        self.channel = id;
    }

    /// Play a sound, if sounds are on: through the call when in one, otherwise on its own.
    pub fn play(&self, cue: Cue) {
        if !self.app.sounds || self.net.is_none() {
            return;
        }
        let gain = self.app.volume;
        if self.engine.is_some() {
            self.shared.cue(cue, gain);
        } else {
            desktop::play_alone(cue, gain);
        }
    }

    pub fn save_app(&self) {
        if self.net.is_some() {
            net::save_app_prefs(self.app.clone());
        }
    }

    /// A new message: notify and ping if its channel's setting says so and it isn't in front
    /// of us already.
    fn maybe_notify(&mut self, msg: &kall_proto::Message) {
        let Some(m) = &self.m else { return };
        if msg.author == m.me || m.me().is_some_and(|u| u.status == Status::Dnd) {
            return;
        }
        let reply_to_me = msg.reply_to.and_then(|r| m.find(msg.channel, r)).is_some_and(|r| r.author == m.me);
        let wanted = match self.app.level(msg.channel) {
            desktop::NotifyLevel::All => true,
            desktop::NotifyLevel::Mentions => reply_to_me || m.mentions_me(&msg.body),
            desktop::NotifyLevel::Nothing => false,
        };
        let looking = rune_notify::app_focused() && self.channel == msg.channel;
        if !wanted || looking {
            return;
        }
        let ch = m.channel(msg.channel).map_or_else(String::new, |c| c.name.clone());
        let title = format!("{} in #{ch}", m.name(msg.author));
        desktop::notify(&format!("msg-{}", msg.id), &title, &msg.body, msg.channel);
        self.play(Cue::Message);
    }

    /// Keep the tray icon's badge and menu current.
    fn sync_tray(&mut self) {
        let Some(m) = &self.m else { return };
        let mentions: u32 = m.read.values().map(|r| r.mentions).sum();
        let any_unread = m.channels.iter().any(|c| m.unread(c) && self.app.level(c.id) == desktop::NotifyLevel::All);
        let badge = if mentions > 0 { Some(mentions) } else if any_unread { Some(0) } else { None };
        let me = self.in_voice_here(None);
        let state = (badge, me.is_some(), me.is_some_and(|v| v.muted));
        if self.tray_state != Some(state) {
            self.tray_state = Some(state);
            let tip = match badge {
                Some(n) if n > 0 => format!("kall: {n} mention{}", if n == 1 { "" } else { "s" }),
                Some(_) => "kall: unread messages".into(),
                None => "kall".into(),
            };
            desktop::set_badge(badge, &tip);
            desktop::set_tray_menu(state.1, state.2);
        }
    }

    /// Hang up and close the connection cleanly (quitting, or restarting for an update).
    fn disconnect(&mut self) {
        self.media = media::Media::default();
        self.engine = None;
        if let Some(net) = &self.net {
            let conn = net.voice.conn.lock().unwrap().take();
            if let Some(conn) = conn {
                conn.close(0u32.into(), b"bye");
                // The net thread's runtime sends the close; give it a moment before we exit.
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }

    pub fn install_update(&mut self) {
        let Some(u) = self.update.take() else { return };
        self.updating = true;
        self.disconnect();
        let wake = self.waker.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // Only returns on failure: success restarts kall.
            if let Err(e) = update::install(&u) {
                tx.send(e.to_string()).ok();
                if let Some(w) = wake {
                    w();
                }
            }
        });
        self.update_failed = Some(rx);
    }

    pub fn join_voice(&mut self, channel: Id) {
        self.send(C2S::JoinVoice { channel });
        self.voice_channel = Some(channel);
    }

    pub fn leave_voice(&mut self) {
        self.send(C2S::LeaveVoice);
        self.voice_channel = None;
    }

    /// Am I in voice in this window (in this channel, if given)?
    pub fn in_voice_here(&self, channel: Option<Id>) -> Option<&kall_proto::VoiceMember> {
        let m = self.m.as_ref()?;
        let v = m.voice.get(&m.me)?;
        (self.voice_channel == Some(v.channel) && channel.is_none_or(|c| c == v.channel)).then_some(v)
    }

    pub fn send_draft(&mut self, tx: &mut Tx) {
        let body = self.draft.text.trim().to_string();
        if body.is_empty() {
            return;
        }
        match self.editing.take() {
            Some(id) => self.send(C2S::EditMessage { id, body }),
            None => {
                let reply_to = self.reply_to.take();
                self.send(C2S::SendMessage { channel: self.channel, body, reply_to });
                self.divider = None;
                tx.scroll_to_bottom("messages");
            }
        }
        self.draft.clear();
    }

    /// Draft changed: tell others we're typing, at most every 3 s.
    pub fn typed(&mut self) {
        if self.editing.is_none() && !self.draft.text.trim().is_empty() && self.now - self.last_typing > 3.0 {
            self.last_typing = self.now;
            self.send(C2S::Typing { channel: self.channel });
        }
    }

    fn on_event(&mut self, ev: NetEvent, tx: &mut Tx) {
        match ev {
            NetEvent::Prefill { server, username } => {
                if self.login.server.text.is_empty() {
                    self.login.server = TextEdit::new(if server.is_empty() { "localhost".to_string() } else { server });
                }
                if self.login.username.text.is_empty() {
                    self.login.username = TextEdit::new(username);
                }
            }
            NetEvent::Connecting { server, attempt } => {
                self.phase = if self.m.is_some() { Phase::Reconnecting { retry_in: 0 } } else { Phase::Connecting { server, attempt } };
            }
            NetEvent::Disconnected { retry_in } => self.phase = Phase::Reconnecting { retry_in },
            NetEvent::LoggedOut { reason, pin_mismatch } => {
                self.m = None;
                self.settings = None;
                self.phase = Phase::Login;
                self.login.error = reason;
                self.login.pin_mismatch = pin_mismatch;
                self.login.password.clear();
                self.voice_channel = None;
            }
            NetEvent::Notice(msg) => self.toast(msg),
            NetEvent::AvatarPrepared(Err(e)) => self.toast(e),
            NetEvent::AvatarPrepared(Ok(png)) => {
                if let Some(set) = &mut self.settings
                    && let Ok(img) = image::load_from_memory(&png)
                {
                    let rgba = img.to_rgba8();
                    let px = rune::Pixels::new();
                    px.set(rgba.width(), rgba.height(), rgba.into_raw());
                    set.avatar = Some((png, px));
                }
            }
            NetEvent::Avatar { user, rev, size, rgba } => {
                if let Some(m) = &mut self.m {
                    let px = rune::Pixels::new();
                    px.set(size, size, rgba);
                    m.avatars.insert(user, (rev, px));
                }
            }
            NetEvent::Msg(S2C::Ready(snap)) => {
                let avatars = self.m.take().map(|m| m.avatars).unwrap_or_default();
                let mut m = Model::new(snap);
                m.avatars = avatars;
                let keep = m.channel(self.channel).is_some();
                let first = m.first_text().unwrap_or(0);
                self.m = Some(m);
                self.phase = Phase::Ready;
                self.login.password.clear();
                self.login.invite.clear();
                self.login.error = None;
                if !keep {
                    self.channel = 0;
                    self.select_channel(first);
                }
                // Back after a dropped connection: rejoin the call we were in.
                let rejoin = self.voice_channel.filter(|c| self.m.as_ref().and_then(|m| m.channel(*c)).is_some_and(|c| c.kind == ChannelKind::Voice));
                self.voice_channel = None;
                if let Some(ch) = rejoin {
                    self.rejoining = true;
                    self.join_voice(ch);
                }
            }
            NetEvent::Msg(msg) => {
                let Some(m) = &self.m else { return };
                if let S2C::VoiceUpdated { user, state } = &msg {
                    let mine = m.voice.get(&m.me).map(|v| v.channel);
                    let before = m.voice.get(user).cloned();
                    if *user == m.me {
                        if let (Some(b), Some(a)) = (&before, state)
                            && b.muted != a.muted
                        {
                            self.play(if a.muted { Cue::Mute } else { Cue::Unmute });
                        }
                        match state {
                            Some(v) if self.voice_channel.is_some() => {
                                self.voice_channel = Some(v.channel);
                                self.rejoining = false;
                            }
                            None if !self.rejoining => self.voice_channel = None,
                            _ => {}
                        }
                    } else if mine.is_some() {
                        let was = before.as_ref().map(|v| v.channel) == mine;
                        let is = state.as_ref().map(|v| v.channel) == mine;
                        if is && !was {
                            self.play(Cue::Join);
                        } else if was && !is {
                            self.play(Cue::Leave);
                        }
                    }
                }
                // Someone in our call started sharing: say so, with a sound.
                if let Some(m) = &self.m
                    && let S2C::StreamUpdated { user, kind, info: Some(info) } = &msg
                    && *user != m.me
                    && !m.streams.contains_key(&(*user, *kind))
                    && let Some(mine) = m.voice.get(&m.me).map(|v| v.channel)
                    && m.voice.get(user).is_some_and(|v| v.channel == mine)
                {
                    let name = m.name(*user).to_string();
                    let (user, kind, label) = (*user, *kind, info.label.clone());
                    self.play(Cue::Stream);
                    match kind {
                        StreamKind::Screen => {
                            self.toast(if label.is_empty() { format!("{name} started sharing their screen") } else { format!("{name} started sharing {label}") });
                            self.toast_watch = Some(user);
                        }
                        StreamKind::Camera => self.toast(format!("{name} turned on their camera")),
                    }
                }
                if let S2C::MessageCreated(msg) = &msg {
                    self.maybe_notify(msg);
                }
                let Some(m) = &mut self.m else { return };
                let open = Some(self.channel);
                let fx = m.apply(msg, self.now, open);
                let gone = m.channel(self.channel).is_none().then(|| m.first_text().unwrap_or(0));
                if let Some(t) = fx.toast {
                    self.toast(t);
                }
                if let Some(first) = gone {
                    self.select_channel(first);
                }
                if let Some(kind) = fx.keyframe {
                    self.own_keyframe(kind);
                }
                if fx.new_message_in == Some(self.channel) {
                    tx.scroll_to_bottom("messages");
                }
            }
        }
    }

    /// Follow-ups after state changes: load history, mark read, fetch avatars.
    fn sync(&mut self) -> bool {
        let Some(m) = &mut self.m else { return false };
        let mut out = Vec::new();
        if let Some(ch) = m.channel(self.channel).cloned()
            && ch.kind == ChannelKind::Text
        {
            let log = m.logs.entry(ch.id).or_default();
            if !log.loaded && !log.loading {
                log.loading = true;
                out.push(C2S::LoadHistory { channel: ch.id, before: None, limit: 60 });
            }
            if m.unread(&ch) || m.mentions(ch.id) > 0 {
                let r = m.read.entry(ch.id).or_insert(kall_proto::ReadState { channel: ch.id, last_read: 0, mentions: 0 });
                r.last_read = ch.last_message;
                r.mentions = 0;
                if ch.last_message > 0 {
                    out.push(C2S::MarkRead { channel: ch.id, message: ch.last_message });
                }
            }
        }
        for u in m.users.values() {
            let have = m.avatars.get(&u.id).map_or(0, |(r, _)| *r);
            let asked = m.avatar_asked.get(&u.id).copied().unwrap_or(0);
            if u.avatar_rev > have && u.avatar_rev > asked {
                out.push(C2S::FetchAvatar { user: u.id });
            }
        }
        for msg in &out {
            if let C2S::FetchAvatar { user } = msg {
                let rev = m.users.get(user).map_or(0, |u| u.avatar_rev);
                m.avatar_asked.insert(*user, rev);
            }
        }
        let changed = !out.is_empty();
        for msg in out {
            self.send(msg);
        }
        changed
    }

    /// Start or stop the voice engine to match whether I'm in a voice channel, and keep its
    /// settings, volumes and speaking set in step with the app.
    fn sync_voice(&mut self) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.devices_rx
            && let Ok(d) = rx.try_recv()
        {
            self.devices = Some(d);
            self.devices_rx = None;
            changed = true;
        }
        let me = self.in_voice_here(None).cloned();
        let Some(net) = &self.net else { return changed };
        match (&me, &self.engine) {
            (Some(_), Some(r)) if r.input != self.voice.input || r.output != self.voice.output => {
                // New devices picked: restart on them.
                self.engine = None;
                return self.sync_voice();
            }
            (Some(_), None) => {
                *self.shared.error.lock().unwrap() = None;
                let wake = self.waker.clone().expect("waker");
                let engine = Engine::start(net.voice.clone(), self.shared.clone(), self.voice.input.clone(), self.voice.output.clone(), wake);
                self.engine = Some(RunningVoice { _engine: engine, input: self.voice.input.clone(), output: self.voice.output.clone() });
                self.play(Cue::Join);
                self.start_media();
            }
            (None, Some(_)) => {
                self.engine = None;
                self.stop_media();
                self.speaking.clear();
                changed = true;
            }
            _ => {}
        }
        if let (Some(v), Some(m)) = (&me, &self.m) {
            let bitrate = m.channel(v.channel).map_or(64_000, |c| c.bitrate);
            *self.shared.settings.lock().unwrap() = VoiceSettings {
                muted: v.muted,
                deafened: v.deafened,
                mode: self.voice.mode,
                vad_threshold: self.voice.vad_threshold,
                noise_suppression: self.voice.noise_suppression,
                echo_cancellation: self.voice.echo_cancellation,
                bitrate,
            };
            *self.shared.ptt_key.lock().unwrap() = self.voice.ptt_key.clone();
            let vols = m.users.values().filter_map(|u| self.voice.volumes.get(&u.username).map(|v| (u.id as u32, *v))).collect();
            *self.shared.volumes.lock().unwrap() = vols;
            let mut speaking: HashSet<Id> = self.shared.speaking.lock().unwrap().iter().map(|u| *u as Id).collect();
            if self.shared.transmitting.load(std::sync::atomic::Ordering::Relaxed) {
                speaking.insert(m.me);
            }
            if speaking != self.speaking {
                self.speaking = speaking;
                changed = true;
            }
        }
        let err = self.shared.error.lock().unwrap().take();
        if let Some(e) = err {
            self.toast = Some((e, self.now + 8.0));
            self.toast_watch = None;
            changed = true;
        }
        changed
    }

    /// List sound devices in the background (for the settings screen).
    pub fn refresh_devices(&mut self) {
        if self.net.is_none() {
            return; // headless snapshots don't touch devices
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = self.waker.clone();
        std::thread::spawn(move || {
            tx.send(crate::voice::devices::list()).ok();
            if let Some(w) = wake {
                w();
            }
        });
        self.devices_rx = Some(rx);
    }

    pub fn save_voice(&self) {
        if self.net.is_some() {
            net::save_voice_prefs(self.voice.clone());
        }
    }

    pub fn volume_of(&self, user: Id) -> f32 {
        self.m.as_ref().and_then(|m| m.users.get(&user)).and_then(|u| self.voice.volumes.get(&u.username)).copied().unwrap_or(1.0)
    }

    pub fn set_volume(&mut self, user: Id, v: f32) {
        let Some(name) = self.m.as_ref().and_then(|m| m.users.get(&user)).map(|u| u.username.clone()) else { return };
        let v = ((v * 10.0).round() / 10.0).clamp(0.0, 2.0);
        if (v - 1.0).abs() < 0.01 {
            self.voice.volumes.remove(&name);
        } else {
            self.voice.volumes.insert(name, v);
        }
        self.save_voice();
    }

    pub fn load_older(&mut self) {
        let Some(m) = &mut self.m else { return };
        let ch = self.channel;
        let log = m.logs.entry(ch).or_default();
        if log.loading || log.complete {
            return;
        }
        log.loading = true;
        let before = log.msgs.first().map(|m| m.id);
        self.send(C2S::LoadHistory { channel: ch, before, limit: 60 });
    }
}

fn setup(s: &mut State, wake: rune::Waker) {
    // Headless runs (snapshots, scripted tests) stay offline.
    if !is_headless() {
        s.voice = net::load_voice_prefs();
        s.app = net::load_app_prefs();
        // Look for a newer kall shortly after starting, then every 6 hours.
        if update::endpoint().is_some() {
            let (tx, rx) = std::sync::mpsc::channel();
            let w = wake.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(8));
                loop {
                    match update::check() {
                        Ok(Some(u)) => {
                            tx.send(u).ok();
                            w();
                            return;
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("kall: update check failed: {e:#}"),
                    }
                    std::thread::sleep(Duration::from_secs(6 * 3600));
                }
            });
            s.update_rx = Some(rx);
        }
        s.waker = Some(wake.clone());
        s.net = Some(net::start(wake));
    }
}

fn poll(s: &mut State, t: f64, tx: &mut Tx) -> bool {
    s.now = t;
    let mut changed = false;
    while let Some(ev) = s.net.as_ref().and_then(|n| n.events.try_recv().ok()) {
        s.on_event(ev, tx);
        changed = true;
    }
    changed |= s.sync();
    changed |= s.sync_voice();
    changed |= s.sync_media();
    s.sync_tray();
    if let Some(u) = s.update_rx.as_ref().and_then(|r| r.try_recv().ok()) {
        s.update = Some(u);
        changed = true;
    }
    if let Some(e) = s.update_failed.as_ref().and_then(|r| r.try_recv().ok()) {
        s.updating = false;
        s.toast(format!("Update failed: {e}"));
        changed = true;
    }
    // Expire the toast and typing indicators, waking when the next one is due.
    if let Some((_, until)) = s.toast {
        if t >= until {
            s.toast = None;
            changed = true;
        } else {
            tx.wake_after(Duration::from_secs_f64(until - t));
        }
    }
    if let Some(m) = &mut s.m {
        let mut next = f64::INFINITY;
        for users in m.typing.values_mut() {
            let before = users.len();
            users.retain(|_, until| *until > t);
            changed |= users.len() != before;
            for until in users.values() {
                next = next.min(*until);
            }
        }
        if next.is_finite() {
            tx.wake_after(Duration::from_secs_f64((next - t).max(0.05)));
        }
    }
    changed
}

/// The close button hides kall to the tray (it keeps running, in calls too). Say so, once.
fn on_close(s: &mut State, _: &mut Tx) {
    if rune::quitting() || !desktop::has_tray() {
        return;
    }
    if !s.app.told_about_tray {
        s.app.told_about_tray = true;
        s.save_app();
        desktop::notify("tray", "kall is still running", "It's in the system tray. Quit from the tray menu.", 0);
    }
}

/// Leaving for real: hang up and close the connection properly, so friends see us go at once
/// instead of after the server's 30 s idle timeout.
fn on_quit(s: &mut State, _: &mut Tx) {
    desktop::drop_tray();
    s.disconnect();
}

fn is_headless() -> bool {
    std::env::args().any(|a| matches!(a.as_str(), "--snap" | "--tree" | "--script" | "--strip" | "--video" | "--html" | "--list-scenes"))
}

fn main() {
    if !is_headless() {
        desktop::register_notifications();
        // The ghost on the window, the taskbar and Alt+Tab (the tray sets its own).
        if let Err(e) = desktop::icon_png(256).map_err(|e| e.to_string()).and_then(rune::set_app_icon) {
            eprintln!("kall: app icon: {e}");
        }
    }
    let app = rune::App::new(AppSpec::<State> {
        min_size: Some((960.0, 600.0)),
        setup: Some(setup),
        poll: Some(poll),
        scenes: demo::SCENES,
        ..AppSpec::new("kall", (1240.0, 780.0), State::default, ui::view)
    });
    let mut app = app.on_close(on_close).on_quit(on_quit);
    if !is_headless() {
        // Close hides to the tray; the tray (or the OS) quits.
        app = app.keep_running().on_ready(|w| {
            if std::env::args().any(|a| a == "--minimized") {
                w.hide();
            }
        });
        let h = app.handle();
        desktop::start_tray(move |a| match a {
            desktop::TrayAction::Open => h.open(),
            desktop::TrayAction::Quit => h.quit(),
            desktop::TrayAction::ToggleMute => h.update(|s: &mut State, _| {
                if let Some(v) = s.in_voice_here(None).cloned() {
                    s.send(C2S::VoiceState { muted: !v.muted, deafened: v.deafened && !v.muted });
                }
            }),
        });
        let h = app.handle();
        rune_notify::on_click(move |click| {
            h.open();
            if let Some(ch) = click.get("channel").and_then(|c| c.parse::<Id>().ok()).filter(|c| *c != 0) {
                h.update(move |s: &mut State, _| s.select_channel(ch));
            }
        });
    }
    app.run();
}
