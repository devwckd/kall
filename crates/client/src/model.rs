//! The client's copy of the server: users, channels, loaded messages, read state, voice,
//! typing. [`Model::apply`] folds each server event in.

use std::collections::{BTreeMap, HashMap};

use kall_proto::{Channel, ChannelKind, Id, Invite, Message, ReadState, Role, S2C, Snapshot, StreamInfo, StreamKind, User, VoiceMember};

#[derive(Default)]
pub struct Log {
    pub msgs: Vec<Message>,
    pub complete: bool,
    pub loading: bool,
    pub loaded: bool,
}

pub struct Model {
    pub server_name: String,
    pub me: Id,
    pub users: BTreeMap<Id, User>,
    pub channels: Vec<Channel>,
    pub logs: HashMap<Id, Log>,
    pub read: HashMap<Id, ReadState>,
    pub voice: HashMap<Id, VoiceMember>,
    /// Video streams people in voice are sending.
    pub streams: HashMap<(Id, StreamKind), StreamInfo>,
    /// channel → user → typing until (app clock seconds)
    pub typing: HashMap<Id, HashMap<Id, f64>>,
    pub avatars: HashMap<Id, (u32, rune::Pixels)>,
    /// Avatar revisions already asked for.
    pub avatar_asked: HashMap<Id, u32>,
    pub invites: Vec<Invite>,
    pub last_invite: Option<Invite>,
    /// The server's file size limit and retention.
    pub limits: kall_proto::files::FileLimits,
}

/// What the UI should react to after an event.
#[derive(Default)]
pub struct Effects {
    pub toast: Option<String>,
    pub new_message_in: Option<Id>,
    /// Someone needs a keyframe of this stream of ours.
    pub keyframe: Option<StreamKind>,
}

impl Model {
    pub fn new(s: Snapshot) -> Model {
        Model {
            server_name: s.server_name,
            me: s.me,
            users: s.users.into_iter().map(|u| (u.id, u)).collect(),
            channels: s.channels,
            logs: HashMap::new(),
            read: s.read.into_iter().map(|r| (r.channel, r)).collect(),
            voice: s.voice.into_iter().map(|v| (v.user, v)).collect(),
            streams: s.streams.into_iter().map(|(u, i)| ((u, i.kind), i)).collect(),
            typing: HashMap::new(),
            avatars: HashMap::new(),
            avatar_asked: HashMap::new(),
            invites: vec![],
            last_invite: None,
            limits: s.files,
        }
    }

    pub fn me(&self) -> Option<&User> {
        self.users.get(&self.me)
    }

    pub fn my_role(&self) -> Role {
        self.me().map_or(Role::Member, |u| u.role)
    }

    pub fn name(&self, id: Id) -> &str {
        self.users.get(&id).map_or("unknown", |u| u.display_name.as_str())
    }

    pub fn channel(&self, id: Id) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn first_text(&self) -> Option<Id> {
        self.channels.iter().find(|c| c.kind == ChannelKind::Text).map(|c| c.id)
    }

    pub fn unread(&self, ch: &Channel) -> bool {
        ch.kind == ChannelKind::Text && ch.last_message > self.read.get(&ch.id).map_or(0, |r| r.last_read)
    }

    pub fn mentions(&self, ch: Id) -> u32 {
        self.read.get(&ch).map_or(0, |r| r.mentions)
    }

    pub fn mentions_me(&self, body: &str) -> bool {
        let Some(me) = self.me() else { return false };
        kall_proto::mentions(body).iter().any(|m| *m == me.username || m == "everyone")
    }

    pub fn find(&self, channel: Id, id: Id) -> Option<&Message> {
        self.logs.get(&channel)?.msgs.iter().find(|m| m.id == id)
    }

    pub fn typing_in(&self, channel: Id, now: f64) -> Vec<Id> {
        let mut v: Vec<Id> = self.typing.get(&channel).map(|t| t.iter().filter(|(_, until)| **until > now).map(|(u, _)| *u).collect()).unwrap_or_default();
        v.sort();
        v
    }

    pub fn apply(&mut self, msg: S2C, now: f64, open_channel: Option<Id>) -> Effects {
        let mut fx = Effects::default();
        match msg {
            S2C::AuthOk { .. } | S2C::AuthError { .. } | S2C::Ready(_) | S2C::Avatar { .. } | S2C::Removed { .. } | S2C::Pong => {}
            S2C::History { channel, messages, complete } => {
                let log = self.logs.entry(channel).or_default();
                let mut merged = messages;
                merged.retain(|m| !log.msgs.iter().any(|x| x.id == m.id));
                merged.extend(log.msgs.drain(..));
                merged.sort_by_key(|m| m.id);
                log.msgs = merged;
                log.complete = complete;
                log.loading = false;
                log.loaded = true;
            }
            S2C::MessageCreated(m) => {
                if let Some(c) = self.channels.iter_mut().find(|c| c.id == m.channel) {
                    c.last_message = c.last_message.max(m.id);
                }
                if let Some(t) = self.typing.get_mut(&m.channel) {
                    t.remove(&m.author);
                }
                let mine = m.author == self.me;
                if mine {
                    let r = self.read.entry(m.channel).or_insert(ReadState { channel: m.channel, last_read: 0, mentions: 0 });
                    r.last_read = r.last_read.max(m.id);
                } else if self.mentions_me(&m.body) && open_channel != Some(m.channel) {
                    self.read.entry(m.channel).or_insert(ReadState { channel: m.channel, last_read: 0, mentions: 0 }).mentions += 1;
                }
                fx.new_message_in = Some(m.channel);
                if let Some(log) = self.logs.get_mut(&m.channel)
                    && log.loaded
                    && !log.msgs.iter().any(|x| x.id == m.id)
                {
                    log.msgs.push(m);
                }
            }
            S2C::MessageUpdated(m) => {
                if let Some(slot) = self.logs.get_mut(&m.channel).and_then(|l| l.msgs.iter_mut().find(|x| x.id == m.id)) {
                    *slot = m;
                }
            }
            S2C::MessageDeleted { channel, id } => {
                if let Some(l) = self.logs.get_mut(&channel) {
                    l.msgs.retain(|m| m.id != id);
                }
            }
            S2C::Typing { channel, user } => {
                self.typing.entry(channel).or_default().insert(user, now + 6.0);
            }
            S2C::ReadUpdated(r) => {
                self.read.insert(r.channel, r);
            }
            S2C::UserUpdated(u) => {
                self.users.insert(u.id, u);
            }
            S2C::UserRemoved { id } => {
                self.users.remove(&id);
                self.voice.remove(&id);
            }
            S2C::ChannelUpdated(c) => {
                match self.channels.iter_mut().find(|x| x.id == c.id) {
                    Some(slot) => *slot = c,
                    None => self.channels.push(c),
                }
                self.channels.sort_by_key(|c| (c.position, c.id));
            }
            S2C::ChannelDeleted { id } => {
                self.channels.retain(|c| c.id != id);
                self.logs.remove(&id);
                self.read.remove(&id);
            }
            S2C::ChannelsReordered { order } => {
                for c in &mut self.channels {
                    c.position = order.iter().position(|&id| id == c.id).unwrap_or(usize::MAX) as i64;
                }
                self.channels.sort_by_key(|c| (c.position, c.id));
            }
            S2C::Invites(v) => self.invites = v,
            S2C::InviteCreated(inv) => {
                self.invites.push(inv.clone());
                self.last_invite = Some(inv);
            }
            S2C::VoiceUpdated { user, state } => match state {
                Some(v) => {
                    self.voice.insert(user, v);
                }
                None => {
                    self.voice.remove(&user);
                }
            },
            S2C::StreamUpdated { user, kind, info } => match info {
                Some(i) => {
                    self.streams.insert((user, kind), i);
                }
                None => {
                    self.streams.remove(&(user, kind));
                }
            },
            S2C::KeyframeRequested { kind } => fx.keyframe = Some(kind),
            S2C::Error { message } => fx.toast = Some(message),
        }
        fx
    }
}
