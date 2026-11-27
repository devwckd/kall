//! Who is connected, and what every request does. Each QUIC connection runs [`serve`]: auth on
//! the first frame of the control stream, then a read loop that hands requests to
//! [`Hub::handle`], while a writer task drains that connection's outbox.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use kall_proto::{
    C2S, Channel, ChannelKind, Id, MAX_AVATAR_BYTES, StreamInfo, StreamKind, MAX_MESSAGE_LEN, PROTOCOL_VERSION, Role, S2C, Snapshot, User, VoiceMember,
};
use rand::Rng;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};

use crate::db::{Db, now};
use crate::preview;

pub enum Out {
    Msg(S2C),
    Close,
}

struct Peer {
    conn: u64,
    tx: UnboundedSender<Out>,
    quic: quinn::Connection,
}

#[derive(Default)]
struct Live {
    peers: HashMap<Id, Vec<Peer>>,
    voice: HashMap<Id, VoiceMember>,
    /// Which of a user's connections is in voice: only it sends and hears voice, and only its
    /// end takes the user out of the channel. A second login (or a reconnect) starts outside.
    voice_conn: HashMap<Id, u64>,
    /// Video streams by (sender, kind), and who is watching each.
    streams: HashMap<(Id, StreamKind), StreamInfo>,
    watchers: HashMap<(Id, StreamKind), HashSet<Id>>,
}

impl Live {
    /// Take `user` out of voice; returns the streams that ended with it (None if not in voice).
    fn leave_voice(&mut self, user: Id) -> Option<Vec<StreamKind>> {
        self.voice_conn.remove(&user);
        self.voice.remove(&user)?;
        Some(self.end_streams(user))
    }

    /// Stop everything `user` sends, and stop them watching others.
    fn end_streams(&mut self, user: Id) -> Vec<StreamKind> {
        let ended: Vec<StreamKind> = self.streams.keys().filter(|(u, _)| *u == user).map(|(_, k)| *k).collect();
        for k in &ended {
            self.streams.remove(&(user, *k));
            self.watchers.remove(&(user, *k));
        }
        for w in self.watchers.values_mut() {
            w.remove(&user);
        }
        ended
    }

    /// The connection to send `user`'s media to, if they're in voice.
    fn media_peer(&self, user: Id) -> Option<&Peer> {
        let conn = self.voice_conn.get(&user)?;
        self.peers.get(&user)?.iter().find(|p| p.conn == *conn)
    }
}

pub struct Hub {
    pub db: Db,
    pub name: String,
    pub files: crate::files::FileStore,
    http: reqwest::Client,
    live: Mutex<Live>,
    next_conn: AtomicU64,
}

/// Who a connection belongs to once it has logged in.
struct Me {
    id: Id,
    username: String,
    token_hash: String,
    conn: u64,
    tx: UnboundedSender<Out>,
}

pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn random_code(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";
    let mut rng = rand::rng();
    (0..len).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect()
}

fn new_token() -> String {
    let mut b = [0u8; 32];
    rand::rng().fill(&mut b);
    hex::encode(b)
}

fn channel_name(kind: ChannelKind, raw: &str) -> Result<String> {
    let name = match kind {
        ChannelKind::Text => raw
            .trim()
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .chars()
            .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            .collect::<String>(),
        ChannelKind::Voice => raw.split_whitespace().collect::<Vec<_>>().join(" "),
    };
    if name.is_empty() || name.chars().count() > 32 {
        bail!("Channel names are 1–32 characters.");
    }
    Ok(name)
}

impl Hub {
    pub fn new(db: Db, name: String, files: crate::files::FileStore) -> Hub {
        Hub { db, name, files, http: preview::client(), live: Mutex::new(Live::default()), next_conn: AtomicU64::new(1) }
    }

    fn live(&self) -> std::sync::MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn view(&self, mut u: User) -> User {
        u.online = self.live().peers.contains_key(&u.id);
        u
    }

    fn user(&self, id: Id) -> Result<User> {
        let u = self.db.user(id)?.ok_or_else(|| anyhow!("no such user"))?;
        Ok(self.view(u))
    }

    pub fn broadcast(&self, msg: S2C) {
        for peers in self.live().peers.values() {
            for p in peers {
                p.tx.send(Out::Msg(msg.clone())).ok();
            }
        }
    }

    fn broadcast_except(&self, user: Id, msg: S2C) {
        for (id, peers) in self.live().peers.iter() {
            if *id != user {
                for p in peers {
                    p.tx.send(Out::Msg(msg.clone())).ok();
                }
            }
        }
    }

    fn send_user(&self, user: Id, msg: S2C) {
        if let Some(peers) = self.live().peers.get(&user) {
            for p in peers {
                p.tx.send(Out::Msg(msg.clone())).ok();
            }
        }
    }

    fn snapshot(&self, me: &Me) -> Result<Snapshot> {
        let users = self.db.users()?.into_iter().map(|u| self.view(u)).collect();
        let channels = self.db.channels()?;
        let read = channels
            .iter()
            .filter(|c| c.kind == ChannelKind::Text)
            .map(|c| self.db.read_state(me.id, &me.username, c.id))
            .collect::<Result<_>>()?;
        let (voice, streams) = {
            let live = self.live();
            (live.voice.values().cloned().collect(), live.streams.iter().map(|((u, _), i)| (*u, i.clone())).collect())
        };
        Ok(Snapshot { server_name: self.name.clone(), me: me.id, users, channels, read, voice, streams, files: self.files.limits })
    }

    /// Check the first frame; on success returns (user, username, token).
    async fn authenticate(&self, first: C2S) -> Result<(Id, String, String), String> {
        let bad = || "Wrong username or password.".to_string();
        let check_version = |v: u32| if v == PROTOCOL_VERSION { Ok(()) } else { Err(format!("Server speaks protocol {PROTOCOL_VERSION}, client speaks {v}: update kall.")) };
        let internal = |e: anyhow::Error| {
            tracing::error!("auth: {e:#}");
            "Server error.".to_string()
        };
        match first {
            C2S::Resume { version, token } => {
                check_version(version)?;
                let id = self.db.session_user(&hash_token(&token)).map_err(internal)?.ok_or("Session expired, log in again.")?;
                let u = self.db.user(id).map_err(internal)?.ok_or("Account gone.")?;
                Ok((id, u.username, token))
            }
            C2S::Login { version, username, password } => {
                check_version(version)?;
                let username = username.trim().to_lowercase();
                let Some(acct) = self.db.account(&username).map_err(internal)? else {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    return Err(bad());
                };
                if acct.banned {
                    return Err("You are banned from this server.".into());
                }
                let hash = acct.pass_hash.clone();
                let ok = tokio::task::spawn_blocking(move || verify_password(&password, &hash)).await.unwrap_or(false);
                if !ok {
                    tokio::time::sleep(Duration::from_millis(400)).await;
                    return Err(bad());
                }
                let token = new_token();
                self.db.add_session(&hash_token(&token), acct.id).map_err(internal)?;
                Ok((acct.id, username, token))
            }
            C2S::Register { version, invite, username, password } => {
                check_version(version)?;
                let username = username.trim().to_lowercase();
                if !kall_proto::valid_username(&username) {
                    return Err("Usernames are 2–32 characters: a–z, 0–9, _ - .".into());
                }
                if password.chars().count() < 8 {
                    return Err("Passwords need at least 8 characters.".into());
                }
                let hash = tokio::task::spawn_blocking(move || hash_password(&password)).await.map_err(|e| internal(e.into()))?.map_err(internal)?;
                let id = self.db.register(invite.trim(), &username, &hash).map_err(|e| e.to_string())?;
                let token = new_token();
                self.db.add_session(&hash_token(&token), id).map_err(internal)?;
                tracing::info!("new account {username}");
                if let Ok(u) = self.user(id) {
                    self.broadcast(S2C::UserUpdated(u));
                }
                Ok((id, username, token))
            }
            _ => Err("Log in first.".into()),
        }
    }

    fn attach(&self, user: Id, tx: UnboundedSender<Out>, quic: quinn::Connection) -> (u64, bool) {
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let first = {
            let mut live = self.live();
            let peers = live.peers.entry(user).or_default();
            peers.push(Peer { conn, tx, quic });
            peers.len() == 1
        };
        (conn, first)
    }

    fn announce_online(&self, user: Id) {
        if let Ok(u) = self.user(user) {
            self.broadcast(S2C::UserUpdated(u));
        }
    }

    fn detach(&self, user: Id, conn: u64) {
        let (last, left_voice) = {
            let mut live = self.live();
            let Some(peers) = live.peers.get_mut(&user) else { return };
            peers.retain(|p| p.conn != conn);
            let last = peers.is_empty();
            if last {
                live.peers.remove(&user);
            }
            let left_voice = if live.voice_conn.get(&user) == Some(&conn) { live.leave_voice(user) } else { None };
            (last, left_voice)
        };
        if let Some(ended) = left_voice {
            self.announce_left(user, ended);
        }
        if last && let Ok(u) = self.user(user) {
            self.broadcast(S2C::UserUpdated(u));
        }
    }

    /// The SFU: forward one voice datagram from `from` to everyone else in their voice channel.
    /// Muted senders and deafened listeners are skipped; nothing is decoded.
    pub fn relay_voice(&self, from: Id, conn: u64, data: &[u8]) {
        let Some(up) = kall_proto::voice::decode_up(data) else { return };
        let live = self.live();
        if live.voice_conn.get(&from) != Some(&conn) {
            return;
        }
        let Some(me) = live.voice.get(&from) else { return };
        // Mute silences the microphone, not a shared screen's audio.
        if me.muted && up.flags & kall_proto::voice::FLAG_SHARE_AUDIO == 0 {
            return;
        }
        let packet = bytes::Bytes::from(kall_proto::voice::relay(from as u32, &up));
        let share_audio = up.flags & kall_proto::voice::FLAG_SHARE_AUDIO != 0;
        let watching = live.watchers.get(&(from, StreamKind::Screen));
        for (uid, v) in &live.voice {
            if *uid == from || v.channel != me.channel || v.deafened {
                continue;
            }
            if share_audio && !watching.is_some_and(|w| w.contains(uid)) {
                continue;
            }
            let target = live.voice_conn.get(uid);
            for p in live.peers.get(uid).into_iter().flatten().filter(|p| Some(&p.conn) == target) {
                // Unreliable by design: a full send buffer just drops this 20 ms.
                p.quic.send_datagram(packet.clone()).ok();
            }
        }
    }

    fn announce_left(&self, user: Id, ended: Vec<StreamKind>) {
        for kind in ended {
            self.broadcast(S2C::StreamUpdated { user, kind, info: None });
        }
        self.broadcast(S2C::VoiceUpdated { user, state: None });
    }

    /// Forward one video fragment to everyone watching that stream (in the same channel), or a
    /// preview fragment to everyone else in the channel.
    pub fn relay_video(&self, from: Id, conn: u64, data: &[u8]) {
        let Some(up) = kall_proto::video::decode_up(data) else { return };
        let live = self.live();
        if live.voice_conn.get(&from) != Some(&conn) || !live.streams.contains_key(&(from, up.kind)) {
            return;
        }
        let Some(channel) = live.voice.get(&from).map(|v| v.channel) else { return };
        let packet = bytes::Bytes::from(kall_proto::video::relay(from as u32, data));
        if up.flags & kall_proto::video::FLAG_PREVIEW != 0 {
            for (uid, v) in &live.voice {
                if *uid != from
                    && v.channel == channel
                    && let Some(p) = live.media_peer(*uid)
                {
                    p.quic.send_datagram(packet.clone()).ok();
                }
            }
            return;
        }
        let Some(watchers) = live.watchers.get(&(from, up.kind)) else { return };
        for w in watchers {
            if live.voice.get(w).is_some_and(|v| v.channel == channel)
                && let Some(p) = live.media_peer(*w)
            {
                p.quic.send_datagram(packet.clone()).ok();
            }
        }
    }

    /// Kick every connection of `user`.
    fn remove(&self, user: Id, reason: &str) {
        let txs: Vec<_> = self.live().peers.get(&user).map(|ps| ps.iter().map(|p| p.tx.clone()).collect()).unwrap_or_default();
        for tx in txs {
            tx.send(Out::Msg(S2C::Removed { reason: reason.into() })).ok();
            tx.send(Out::Close).ok();
        }
    }

    async fn handle(self: &Arc<Self>, me: &Me, msg: C2S) -> Result<()> {
        let my = self.db.user(me.id)?.ok_or_else(|| anyhow!("account gone"))?;
        let reply = |m: S2C| {
            me.tx.send(Out::Msg(m)).ok();
        };
        match msg {
            C2S::Register { .. } | C2S::Login { .. } | C2S::Resume { .. } => bail!("Already logged in."),
            C2S::Ping => reply(S2C::Pong),
            C2S::Logout => {
                self.db.drop_session(&me.token_hash)?;
                me.tx.send(Out::Close).ok();
            }

            C2S::SendMessage { channel, body, reply_to, attachments } => {
                let body = body.trim().to_string();
                if (body.is_empty() && attachments.is_empty()) || body.chars().count() > MAX_MESSAGE_LEN {
                    bail!("Messages are 1–{MAX_MESSAGE_LEN} characters.");
                }
                let ch = self.text_channel(channel)?;
                self.check_attachments(me.id, &attachments)?;
                let reply_to = reply_to.filter(|&r| self.db.message(r).ok().flatten().is_some_and(|m| m.channel == ch.id));
                let msg = self.db.insert_message(ch.id, me.id, &body, reply_to)?;
                self.db.attach_files(msg.id, me.id, &attachments)?;
                let msg = self.db.message(msg.id)?.unwrap_or(msg);
                self.db.mark_read(me.id, ch.id, msg.id)?;
                self.broadcast(S2C::MessageCreated(msg.clone()));
                self.spawn_preview(msg.id, &body);
            }
            C2S::EditMessage { id, body } => {
                let body = body.trim().to_string();
                if body.is_empty() || body.chars().count() > MAX_MESSAGE_LEN {
                    bail!("Messages are 1–{MAX_MESSAGE_LEN} characters.");
                }
                let msg = self.db.message(id)?.ok_or_else(|| anyhow!("That message is gone."))?;
                if msg.author != me.id {
                    bail!("You can only edit your own messages.");
                }
                self.db.edit_message(id, &body)?;
                if let Some(m) = self.db.message(id)? {
                    self.broadcast(S2C::MessageUpdated(m));
                }
                self.spawn_preview(id, &body);
            }
            C2S::DeleteMessage { id } => {
                let msg = self.db.message(id)?.ok_or_else(|| anyhow!("That message is gone."))?;
                if msg.author != me.id {
                    let author = self.db.user(msg.author)?.map_or(Role::Member, |u| u.role);
                    if !(my.role.can_delete_any() && my.role.outranks(author)) {
                        bail!("You can't delete that message.");
                    }
                }
                self.db.delete_message(id)?;
                let files = self.db.remove_message_files(id)?;
                self.files.remove(&files);
                self.broadcast(S2C::MessageDeleted { channel: msg.channel, id });
            }
            C2S::LoadHistory { channel, before, limit } => {
                let (messages, complete) = self.db.history(channel, before, limit)?;
                reply(S2C::History { channel, messages, complete });
            }
            C2S::Typing { channel } => self.broadcast_except(me.id, S2C::Typing { channel, user: me.id }),
            C2S::MarkRead { channel, message } => {
                self.db.mark_read(me.id, channel, message)?;
                let rs = self.db.read_state(me.id, &me.username, channel)?;
                self.send_user(me.id, S2C::ReadUpdated(rs));
            }

            C2S::SetProfile { display_name, status, status_text } => {
                let display_name = display_name.split_whitespace().collect::<Vec<_>>().join(" ");
                let display_name = if display_name.is_empty() { my.username.clone() } else { display_name };
                if display_name.chars().count() > 32 || status_text.chars().count() > 128 {
                    bail!("Display names are up to 32 characters, statuses up to 128.");
                }
                self.db.set_profile(me.id, &display_name, status, status_text.trim())?;
                self.broadcast(S2C::UserUpdated(self.user(me.id)?));
            }
            C2S::SetAvatar { png } => {
                if png.len() > MAX_AVATAR_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
                    bail!("Avatars must be PNG images under 512 KB.");
                }
                self.db.set_avatar(me.id, &png)?;
                self.broadcast(S2C::UserUpdated(self.user(me.id)?));
            }
            C2S::FetchAvatar { user } => {
                if let Some((rev, png)) = self.db.avatar(user)? {
                    reply(S2C::Avatar { user, rev, png });
                }
            }

            C2S::CreateChannel { name, kind } => {
                self.require(my.role.can_manage_channels())?;
                let id = self.db.create_channel(&channel_name(kind, &name)?, kind)?;
                self.broadcast(S2C::ChannelUpdated(self.channel(id)?));
            }
            C2S::UpdateChannel { id, name, bitrate } => {
                self.require(my.role.can_manage_channels())?;
                let ch = self.channel(id)?;
                self.db.update_channel(id, &channel_name(ch.kind, &name)?, bitrate.clamp(8_000, 256_000))?;
                self.broadcast(S2C::ChannelUpdated(self.channel(id)?));
            }
            C2S::MoveChannel { id, up } => {
                self.require(my.role.can_manage_channels())?;
                let order = self.db.move_channel(id, up)?;
                self.broadcast(S2C::ChannelsReordered { order });
            }
            C2S::DeleteChannel { id } => {
                self.require(my.role.can_manage_channels())?;
                self.channel(id)?;
                if self.db.channels()?.len() <= 1 {
                    bail!("Keep at least one channel.");
                }
                let kicked: Vec<(Id, Vec<StreamKind>)> = {
                    let mut live = self.live();
                    let ids: Vec<Id> = live.voice.values().filter(|v| v.channel == id).map(|v| v.user).collect();
                    ids.into_iter().map(|u| (u, live.leave_voice(u).unwrap_or_default())).collect()
                };
                for (user, ended) in kicked {
                    self.announce_left(user, ended);
                }
                let files = self.db.delete_channel(id)?;
                self.files.remove(&files);
                self.broadcast(S2C::ChannelDeleted { id });
            }
            C2S::CreateInvite { uses, hours } => {
                self.require(my.role.can_invite())?;
                let expires = hours.map(|h| now() + h as i64 * 3_600_000);
                let inv = self.db.create_invite(&random_code(10), Some(me.id), uses.filter(|&u| u > 0), expires, false)?;
                reply(S2C::InviteCreated(inv));
            }
            C2S::ListInvites => {
                self.require(my.role.can_invite())?;
                reply(S2C::Invites(self.db.invites()?));
            }
            C2S::RevokeInvite { code } => {
                self.require(my.role.can_invite())?;
                self.db.revoke_invite(&code)?;
                reply(S2C::Invites(self.db.invites()?));
            }
            C2S::SetRole { user, role } => {
                let target = self.user(user)?;
                if !(my.role.can_grant(role) && my.role.outranks(target.role)) {
                    bail!("You can't give that role.");
                }
                self.db.set_role(user, role)?;
                self.broadcast(S2C::UserUpdated(self.user(user)?));
            }
            C2S::Kick { user } => {
                let target = self.user(user)?;
                self.require(my.role.can_kick() && my.role.outranks(target.role))?;
                tracing::info!("{} kicked {}", my.username, target.username);
                self.remove(user, &format!("You were kicked by {}.", my.display_name));
            }
            C2S::Ban { user } => {
                let target = self.user(user)?;
                self.require(my.role.can_ban() && my.role.outranks(target.role))?;
                tracing::info!("{} banned {}", my.username, target.username);
                self.db.set_banned(user, true)?;
                self.remove(user, &format!("You were banned by {}.", my.display_name));
                self.broadcast(S2C::UserRemoved { id: user });
            }
            C2S::Unban { user } => {
                self.require(my.role.can_ban())?;
                self.db.set_banned(user, false)?;
                self.broadcast(S2C::UserUpdated(self.user(user)?));
            }

            C2S::JoinVoice { channel } => {
                let ch = self.channel(channel)?;
                if ch.kind != ChannelKind::Voice {
                    bail!("That's not a voice channel.");
                }
                let state = VoiceMember { user: me.id, channel, muted: false, deafened: false };
                let ended = {
                    let mut live = self.live();
                    // Moving channels (or another session taking over) ends what was shared.
                    let ended = live.end_streams(me.id);
                    live.voice.insert(me.id, state.clone());
                    live.voice_conn.insert(me.id, me.conn);
                    ended
                };
                for kind in ended {
                    self.broadcast(S2C::StreamUpdated { user: me.id, kind, info: None });
                }
                self.broadcast(S2C::VoiceUpdated { user: me.id, state: Some(state) });
            }
            C2S::LeaveVoice => {
                let left = self.live().leave_voice(me.id);
                if let Some(ended) = left {
                    self.announce_left(me.id, ended);
                }
            }
            C2S::StartStream(info) => {
                if info.width == 0 || info.height == 0 || info.width > 7680 || info.height > 4320 || info.fps == 0 || info.fps > 120 {
                    bail!("That stream size isn't supported.");
                }
                let mut info = info;
                info.label = info.label.chars().take(80).collect();
                {
                    let mut live = self.live();
                    if live.voice_conn.get(&me.id) != Some(&me.conn) {
                        bail!("Join a voice channel to share.");
                    }
                    live.streams.insert((me.id, info.kind), info.clone());
                }
                self.broadcast(S2C::StreamUpdated { user: me.id, kind: info.kind, info: Some(info) });
            }
            C2S::StopStream { kind } => {
                let had = {
                    let mut live = self.live();
                    live.watchers.remove(&(me.id, kind));
                    live.streams.remove(&(me.id, kind)).is_some()
                };
                if had {
                    self.broadcast(S2C::StreamUpdated { user: me.id, kind, info: None });
                }
            }
            C2S::WatchStream { user, kind, watch } => {
                let mut live = self.live();
                if watch {
                    if !live.streams.contains_key(&(user, kind)) {
                        return Ok(());
                    }
                    live.watchers.entry((user, kind)).or_default().insert(me.id);
                    // A new viewer needs a keyframe to start from.
                    if let Some(p) = live.media_peer(user) {
                        p.tx.send(Out::Msg(S2C::KeyframeRequested { kind })).ok();
                    }
                } else if let Some(w) = live.watchers.get_mut(&(user, kind)) {
                    w.remove(&me.id);
                }
            }
            C2S::RequestKeyframe { user, kind } => {
                let live = self.live();
                if live.watchers.get(&(user, kind)).is_some_and(|w| w.contains(&me.id))
                    && let Some(p) = live.media_peer(user)
                {
                    p.tx.send(Out::Msg(S2C::KeyframeRequested { kind })).ok();
                }
            }
            C2S::VoiceState { muted, deafened } => {
                let state = self.live().voice.get_mut(&me.id).map(|v| {
                    v.muted = muted || deafened;
                    v.deafened = deafened;
                    v.clone()
                });
                if let Some(state) = state {
                    self.broadcast(S2C::VoiceUpdated { user: me.id, state: Some(state) });
                }
            }
        }
        Ok(())
    }

    fn require(&self, ok: bool) -> Result<()> {
        if ok { Ok(()) } else { bail!("You don't have permission to do that.") }
    }

    fn channel(&self, id: Id) -> Result<Channel> {
        self.db.channel(id)?.ok_or_else(|| anyhow!("That channel is gone."))
    }

    fn text_channel(&self, id: Id) -> Result<Channel> {
        let ch = self.channel(id)?;
        if ch.kind != ChannelKind::Text {
            bail!("That's not a text channel.");
        }
        Ok(ch)
    }

    fn spawn_preview(self: &Arc<Self>, id: Id, body: &str) {
        let Some(url) = preview::first_url(body) else { return };
        let hub = self.clone();
        tokio::spawn(async move {
            match preview::fetch(&hub.http, &url).await {
                Ok(Some(p)) => {
                    // The message may have been edited or deleted while we fetched.
                    let Ok(Some(msg)) = hub.db.message(id) else { return };
                    if preview::first_url(&msg.body).as_deref() != Some(url.as_str()) {
                        return;
                    }
                    if hub.db.set_preview(id, &p).is_ok()
                        && let Ok(Some(m)) = hub.db.message(id)
                    {
                        hub.broadcast(S2C::MessageUpdated(m));
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::debug!("preview {url}: {e:#}"),
            }
        });
    }
}

pub fn hash_password(password: &str) -> Result<String> {
    use argon2::password_hash::{PasswordHasher, SaltString};
    let mut raw = [0u8; 16];
    rand::rng().fill(&mut raw);
    let salt = SaltString::encode_b64(&raw).map_err(|e| anyhow!("{e}"))?;
    Ok(argon2::Argon2::default().hash_password(password.as_bytes(), &salt).map_err(|e| anyhow!("{e}"))?.to_string())
}

fn verify_password(password: &str, hash: &str) -> bool {
    use argon2::password_hash::{PasswordHash, PasswordVerifier};
    PasswordHash::new(hash).is_ok_and(|h| argon2::Argon2::default().verify_password(password.as_bytes(), &h).is_ok())
}

/// One client connection, from handshake to goodbye.
pub async fn serve(hub: Arc<Hub>, incoming: quinn::Incoming) -> Result<()> {
    let conn = incoming.await?;
    let remote = conn.remote_address();
    let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(10), conn.accept_bi()).await??;
    let first: C2S = tokio::time::timeout(Duration::from_secs(10), kall_proto::recv(&mut recv))
        .await??
        .ok_or_else(|| anyhow!("closed before auth"))?;
    let (id, username, token) = match hub.authenticate(first).await {
        Ok(ok) => ok,
        Err(reason) => {
            tracing::info!("{remote}: auth failed: {reason}");
            kall_proto::send(&mut send, &S2C::AuthError { reason }).await.ok();
            send.finish().ok();
            tokio::time::sleep(Duration::from_millis(300)).await;
            conn.close(0u32.into(), b"auth");
            return Ok(());
        }
    };
    kall_proto::send(&mut send, &S2C::AuthOk { token: token.clone(), username: username.clone() }).await?;
    tracing::info!("{remote}: {username} connected");

    let (tx, mut rx) = unbounded_channel::<Out>();
    // Ready goes out before anyone hears this user came online, so it is always the first frame.
    let (conn_id, first) = hub.attach(id, tx.clone(), conn.clone());
    let me = Me { id, username, token_hash: hash_token(&token), conn: conn_id, tx };
    match hub.snapshot(&me) {
        Ok(s) => {
            me.tx.send(Out::Msg(S2C::Ready(s))).ok();
        }
        Err(e) => tracing::error!("snapshot: {e:#}"),
    }
    if first {
        hub.announce_online(id);
    }

    // Every stream after the control stream is a file transfer.
    let tr_hub = hub.clone();
    let tr_conn = conn.clone();
    let transfers = tokio::spawn(async move {
        while let Ok((s, r)) = tr_conn.accept_bi().await {
            let hub = tr_hub.clone();
            tokio::spawn(async move {
                if let Err(e) = hub.transfer(id, s, r).await {
                    tracing::debug!("transfer: {e:#}");
                }
            });
        }
    });

    let voice_hub = hub.clone();
    let voice_conn = conn.clone();
    let voice = tokio::spawn(async move {
        while let Ok(d) = voice_conn.read_datagram().await {
            match d.first() {
                Some(&kall_proto::voice::KIND_VOICE) => voice_hub.relay_voice(id, conn_id, &d),
                Some(&kall_proto::video::KIND_VIDEO) => voice_hub.relay_video(id, conn_id, &d),
                _ => {}
            }
        }
    });

    let writer_conn = conn.clone();
    let writer = tokio::spawn(async move {
        while let Some(out) = rx.recv().await {
            match out {
                Out::Msg(m) => {
                    if kall_proto::send(&mut send, &m).await.is_err() {
                        break;
                    }
                }
                Out::Close => {
                    send.finish().ok();
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    writer_conn.close(0u32.into(), b"bye");
                    break;
                }
            }
        }
    });

    let result = async {
        while let Some(msg) = kall_proto::recv::<C2S>(&mut recv).await? {
            if let Err(e) = hub.handle(&me, msg).await {
                me.tx.send(Out::Msg(S2C::Error { message: e.to_string() })).ok();
            }
        }
        Ok::<_, anyhow::Error>(())
    }
    .await;
    hub.detach(me.id, me.conn);
    writer.abort();
    voice.abort();
    transfers.abort();
    tracing::info!("{remote}: {} disconnected", me.username);
    if let Err(e) = result
        && !matches!(e.downcast_ref::<quinn::ReadExactError>(), Some(quinn::ReadExactError::ReadError(quinn::ReadError::ConnectionLost(_))))
    {
        tracing::debug!("{remote}: {e:#}");
    }
    Ok(())
}

