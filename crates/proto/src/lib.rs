//! kall's wire protocol: the messages client and server exchange on the QUIC control stream,
//! and the length-prefixed postcard framing that carries them.
//!
//! One QUIC connection per client. The client opens one bidirectional stream first (the
//! control stream) and every [`C2S`]/[`S2C`] message travels on it as a frame:
//! `u32 little-endian length` + postcard bytes. Media (voice, video) uses QUIC datagrams, and
//! each file transfer its own stream ([`files`]).

use serde::{Deserialize, Serialize};

#[cfg(feature = "client")]
pub mod client;
pub mod files;
pub mod video;
pub mod voice;

pub const ALPN: &[u8] = b"kall/1";
pub const PROTOCOL_VERSION: u32 = 3;
pub const DEFAULT_PORT: u16 = 7744;
/// Largest control frame either side accepts (avatars travel inline).
pub const MAX_FRAME: usize = 2 * 1024 * 1024;
pub const MAX_MESSAGE_LEN: usize = 4000;
pub const MAX_AVATAR_BYTES: usize = 512 * 1024;

pub type Id = i64;
/// Milliseconds since the Unix epoch.
pub type Millis = i64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Role {
    Member = 0,
    Mod = 1,
    Admin = 2,
    Owner = 3,
}

impl Role {
    pub fn can_manage_channels(self) -> bool {
        self >= Role::Admin
    }
    pub fn can_invite(self) -> bool {
        self >= Role::Mod
    }
    pub fn can_delete_any(self) -> bool {
        self >= Role::Mod
    }
    pub fn can_kick(self) -> bool {
        self >= Role::Mod
    }
    pub fn can_ban(self) -> bool {
        self >= Role::Admin
    }
    /// Moderation only goes downwards: you act on people below your own role.
    pub fn outranks(self, other: Role) -> bool {
        self > other
    }
    /// Roles `self` may hand out: anything below its own (only the owner makes admins).
    pub fn can_grant(self, role: Role) -> bool {
        self >= Role::Admin && role < self
    }
    pub fn name(self) -> &'static str {
        match self {
            Role::Member => "Member",
            Role::Mod => "Moderator",
            Role::Admin => "Admin",
            Role::Owner => "Owner",
        }
    }
    pub fn from_i64(v: i64) -> Role {
        match v {
            3 => Role::Owner,
            2 => Role::Admin,
            1 => Role::Mod,
            _ => Role::Member,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Online,
    Idle,
    Dnd,
}

impl Status {
    pub fn from_i64(v: i64) -> Status {
        match v {
            1 => Status::Idle,
            2 => Status::Dnd,
            _ => Status::Online,
        }
    }
    pub fn to_i64(self) -> i64 {
        match self {
            Status::Online => 0,
            Status::Idle => 1,
            Status::Dnd => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChannelKind {
    Text,
    Voice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: Id,
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub status: Status,
    pub status_text: String,
    pub online: bool,
    /// Bumped whenever the avatar changes; 0 means none.
    pub avatar_rev: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Channel {
    pub id: Id,
    pub name: String,
    pub kind: ChannelKind,
    pub position: i64,
    /// Opus bitrate for voice channels, in bits per second.
    pub bitrate: u32,
    /// Newest message id in the channel (0 when empty).
    pub last_message: Id,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Preview {
    pub url: String,
    pub site: String,
    pub title: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: Id,
    pub channel: Id,
    pub author: Id,
    pub body: String,
    pub reply_to: Option<Id>,
    pub created: Millis,
    pub edited: Option<Millis>,
    pub preview: Option<Preview>,
    pub attachments: Vec<files::Attachment>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReadState {
    pub channel: Id,
    pub last_read: Id,
    /// Unread messages that @mention you.
    pub mentions: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VoiceMember {
    pub user: Id,
    pub channel: Id,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StreamKind {
    Screen,
    Camera,
}

/// A video stream someone in voice is sending.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub kind: StreamKind,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// The screen share carries the sharer's system audio.
    pub audio: bool,
    /// What's shared: "Display 1", a window title, a camera name.
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Invite {
    pub code: String,
    pub uses_left: Option<u32>,
    pub expires: Option<Millis>,
}

/// Everything a client needs after logging in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub server_name: String,
    pub me: Id,
    pub users: Vec<User>,
    pub channels: Vec<Channel>,
    pub read: Vec<ReadState>,
    pub voice: Vec<VoiceMember>,
    pub streams: Vec<(Id, StreamInfo)>,
    pub files: files::FileLimits,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum C2S {
    /// First frame: create an account with an invite code.
    Register { version: u32, invite: String, username: String, password: String },
    /// First frame: log in with a password.
    Login { version: u32, username: String, password: String },
    /// First frame: log in with a session token from an earlier login.
    Resume { version: u32, token: String },
    /// End this session and forget its token.
    Logout,

    /// `attachments`: ids from uploads on transfer streams (see [`files`]). The body may be
    /// empty when there are attachments.
    SendMessage { channel: Id, body: String, reply_to: Option<Id>, attachments: Vec<Id> },
    EditMessage { id: Id, body: String },
    DeleteMessage { id: Id },
    /// Messages older than `before` (newest first when None), up to `limit`.
    LoadHistory { channel: Id, before: Option<Id>, limit: u32 },
    Typing { channel: Id },
    MarkRead { channel: Id, message: Id },

    SetProfile { display_name: String, status: Status, status_text: String },
    /// A PNG, already square and small (the client resizes to 128×128).
    SetAvatar { png: Vec<u8> },
    FetchAvatar { user: Id },

    CreateChannel { name: String, kind: ChannelKind },
    UpdateChannel { id: Id, name: String, bitrate: u32 },
    MoveChannel { id: Id, up: bool },
    DeleteChannel { id: Id },
    CreateInvite { uses: Option<u32>, hours: Option<u32> },
    ListInvites,
    RevokeInvite { code: String },
    SetRole { user: Id, role: Role },
    Kick { user: Id },
    Ban { user: Id },
    Unban { user: Id },

    JoinVoice { channel: Id },
    LeaveVoice,
    VoiceState { muted: bool, deafened: bool },
    /// Start (or change) a video stream; needs to be in voice. Video goes as datagrams.
    StartStream(StreamInfo),
    StopStream { kind: StreamKind },
    /// Start or stop receiving someone's stream (video is only sent to watchers).
    WatchStream { user: Id, kind: StreamKind, watch: bool },
    /// A watcher lost a frame: ask the sender for a keyframe.
    RequestKeyframe { user: Id, kind: StreamKind },

    Ping,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum S2C {
    /// Login worked: keep `token` to `Resume` later. A `Ready` follows.
    AuthOk { token: String, username: String },
    AuthError { reason: String },
    Ready(Snapshot),

    History { channel: Id, messages: Vec<Message>, complete: bool },
    MessageCreated(Message),
    MessageUpdated(Message),
    MessageDeleted { channel: Id, id: Id },
    Typing { channel: Id, user: Id },
    ReadUpdated(ReadState),

    UserUpdated(User),
    UserRemoved { id: Id },
    Avatar { user: Id, rev: u32, png: Vec<u8> },

    ChannelUpdated(Channel),
    ChannelDeleted { id: Id },
    ChannelsReordered { order: Vec<Id> },
    Invites(Vec<Invite>),
    InviteCreated(Invite),

    VoiceUpdated { user: Id, state: Option<VoiceMember> },
    StreamUpdated { user: Id, kind: StreamKind, info: Option<StreamInfo> },
    /// Someone started watching or lost a frame: send a keyframe soon.
    KeyframeRequested { kind: StreamKind },

    /// You were kicked or banned; the server closes the connection after this.
    Removed { reason: String },
    Error { message: String },
    Pong,
}

/// Write one frame.
pub async fn send<T: Serialize>(stream: &mut quinn::SendStream, msg: &T) -> anyhow::Result<()> {
    let bytes = postcard::to_stdvec(msg)?;
    anyhow::ensure!(bytes.len() <= MAX_FRAME, "frame too large ({} bytes)", bytes.len());
    let mut buf = Vec::with_capacity(4 + bytes.len());
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(&bytes);
    stream.write_all(&buf).await?;
    Ok(())
}

/// Read one frame; `Ok(None)` when the peer finished the stream cleanly.
pub async fn recv<T: for<'de> Deserialize<'de>>(stream: &mut quinn::RecvStream) -> anyhow::Result<Option<T>> {
    let mut len = [0u8; 4];
    match stream.read_exact(&mut len).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(0)) => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    anyhow::ensure!(len <= MAX_FRAME, "frame too large ({len} bytes)");
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(Some(postcard::from_bytes(&buf)?))
}

/// `@name` mentions in a message body, lowercased.
pub fn mentions(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = body.char_indices().peekable();
    let mut prev: Option<char> = None;
    while let Some((i, c)) = chars.next() {
        if c == '@' && prev.is_none_or(|p| !p.is_alphanumeric()) {
            let rest = &body[i + 1..];
            let end = rest.find(|ch: char| !(ch.is_alphanumeric() || ch == '_' || ch == '-' || ch == '.')).unwrap_or(rest.len());
            let name = rest[..end].trim_end_matches('.');
            if !name.is_empty() {
                out.push(name.to_lowercase());
            }
        }
        prev = Some(c);
    }
    out
}

/// Usernames: 2–32 of `a-z 0-9 _ - .`, stored lowercase.
pub fn valid_username(name: &str) -> bool {
    (2..=32).contains(&name.len()) && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_mentions() {
        assert_eq!(mentions("hey @Mira and @theo_2. email a@b.c"), vec!["mira", "theo_2"]);
        assert!(mentions("no mentions").is_empty());
    }

    #[test]
    fn roles_only_act_downwards() {
        assert!(Role::Owner.can_grant(Role::Admin));
        assert!(!Role::Admin.can_grant(Role::Admin));
        assert!(Role::Admin.outranks(Role::Mod));
        assert!(!Role::Mod.can_grant(Role::Member));
    }
}
