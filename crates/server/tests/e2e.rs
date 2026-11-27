//! A real server on localhost and real QUIC clients: register, chat, moderate.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use kall_proto::client::{Connected, connect};
use kall_proto::{C2S, ChannelKind, PROTOCOL_VERSION, Role, S2C, Snapshot};
use kall_server::{Server, db::Db};

struct Client {
    conn: Connected,
    snap: Snapshot,
    token: String,
}

impl Client {
    async fn send(&mut self, m: C2S) -> Result<()> {
        kall_proto::send(&mut self.conn.send, &m).await
    }

    async fn next(&mut self) -> Result<S2C> {
        tokio::time::timeout(Duration::from_secs(5), kall_proto::recv(&mut self.conn.recv))
            .await
            .map_err(|_| anyhow!("timed out waiting for the server"))??
            .ok_or_else(|| anyhow!("stream closed"))
    }

    /// Skip frames until one matches.
    async fn expect<T>(&mut self, mut f: impl FnMut(S2C) -> Option<T>) -> Result<T> {
        for _ in 0..50 {
            if let Some(t) = f(self.next().await?) {
                return Ok(t);
            }
        }
        bail!("never got the expected frame")
    }
}

async fn login(addr: &str, first: C2S) -> Result<Client> {
    let mut conn = connect(addr, None).await?;
    kall_proto::send(&mut conn.send, &first).await?;
    let token = match kall_proto::recv(&mut conn.recv).await? {
        Some(S2C::AuthOk { token, .. }) => token,
        Some(S2C::AuthError { reason }) => bail!("auth: {reason}"),
        other => bail!("unexpected {other:?}"),
    };
    let snap = match kall_proto::recv(&mut conn.recv).await? {
        Some(S2C::Ready(s)) => s,
        other => bail!("expected Ready, got {other:?}"),
    };
    Ok(Client { conn, snap, token })
}

fn register(invite: &str, user: &str) -> C2S {
    C2S::Register { version: PROTOCOL_VERSION, invite: invite.into(), username: user.into(), password: "correct horse".into() }
}

#[tokio::test(flavor = "multi_thread")]
async fn chat_and_moderation_round_trip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let identity = kall_server::tls::self_signed(dir.path(), &[])?;
    let server = Server::start(Db::open(&dir.path().join("kall.db"))?, "Test".into(), "127.0.0.1:0".parse()?, identity)?;
    let addr = server.endpoint.local_addr()?.to_string();
    let owner_invite = server.owner_invite.clone().expect("fresh server has an owner invite");
    tokio::spawn(async move { server.run().await });

    // The owner signs up with the bootstrap invite and makes an invite for a friend.
    let mut owner = login(&addr, register(&owner_invite, "sam")).await?;
    assert_eq!(owner.snap.server_name, "Test");
    let me = owner.snap.users.iter().find(|u| u.id == owner.snap.me).unwrap();
    assert_eq!(me.role, Role::Owner);
    owner.send(C2S::CreateInvite { uses: Some(1), hours: None }).await?;
    let code = owner.expect(|m| if let S2C::InviteCreated(i) = m { Some(i.code) } else { None }).await?;

    // Bad attempts are refused.
    assert!(login(&addr, register("nope", "mira")).await.is_err());
    assert!(login(&addr, register(&code, "Bad Name")).await.is_err());

    let mut mira = login(&addr, register(&code, "mira")).await?;
    assert!(login(&addr, register(&code, "theo")).await.is_err(), "invite was single-use");
    owner.expect(|m| matches!(m, S2C::UserUpdated(u) if u.username == "mira" && u.online).then_some(())).await?;

    // Chat: send, receive, reply, edit, mention counts, delete.
    let general = owner.snap.channels.iter().find(|c| c.kind == ChannelKind::Text).unwrap().id;
    owner.send(C2S::SendMessage { channel: general, body: "hello **@mira**".into(), reply_to: None }).await?;
    let first = mira.expect(|m| if let S2C::MessageCreated(m) = m { Some(m) } else { None }).await?;
    assert_eq!(first.body, "hello **@mira**");
    mira.send(C2S::SendMessage { channel: general, body: "hi!".into(), reply_to: Some(first.id) }).await?;
    let reply = owner.expect(|m| if let S2C::MessageCreated(m) = m { m.reply_to.is_some().then_some(m) } else { None }).await?;
    assert_eq!(reply.reply_to, Some(first.id));

    mira.send(C2S::EditMessage { id: first.id, body: "hacked".into() }).await?;
    mira.expect(|m| matches!(m, S2C::Error { .. }).then_some(())).await?;
    owner.send(C2S::EditMessage { id: first.id, body: "hello again @mira".into() }).await?;
    let edited = mira.expect(|m| if let S2C::MessageUpdated(m) = m { Some(m) } else { None }).await?;
    assert!(edited.edited.is_some());

    // A fresh login sees the history and its unread mention.
    let mira2 = login(&addr, C2S::Resume { version: PROTOCOL_VERSION, token: mira.token.clone() }).await?;
    let rs = mira2.snap.read.iter().find(|r| r.channel == general).unwrap();
    assert_eq!(rs.mentions, 0, "mira's own reply marked the channel read");
    drop(mira2);
    mira.send(C2S::LoadHistory { channel: general, before: None, limit: 50 }).await?;
    let (msgs, complete) = mira.expect(|m| if let S2C::History { messages, complete, .. } = m { Some((messages, complete)) } else { None }).await?;
    assert_eq!(msgs.len(), 2);
    assert!(complete);

    // Members can't manage channels; the owner can.
    mira.send(C2S::CreateChannel { name: "secret".into(), kind: ChannelKind::Text }).await?;
    mira.expect(|m| matches!(m, S2C::Error { .. }).then_some(())).await?;
    owner.send(C2S::CreateChannel { name: "Memes And Stuff".into(), kind: ChannelKind::Text }).await?;
    let ch = mira.expect(|m| if let S2C::ChannelUpdated(c) = m { Some(c) } else { None }).await?;
    assert_eq!(ch.name, "memes-and-stuff");

    // Voice presence is shared.
    let lounge = owner.snap.channels.iter().find(|c| c.kind == ChannelKind::Voice).unwrap().id;
    mira.send(C2S::JoinVoice { channel: lounge }).await?;
    owner.expect(|m| matches!(m, S2C::VoiceUpdated { state: Some(v), .. } if v.channel == lounge).then_some(())).await?;

    // Mods can delete members' messages; banning ends the session for good.
    owner.send(C2S::SetRole { user: mira.snap.me, role: Role::Mod }).await?;
    owner.expect(|m| matches!(m, S2C::UserUpdated(u) if u.role == Role::Mod).then_some(())).await?;
    owner.send(C2S::Ban { user: mira.snap.me }).await?;
    mira.expect(|m| matches!(m, S2C::Removed { .. }).then_some(())).await?;
    owner.expect(|m| matches!(m, S2C::VoiceUpdated { state: None, .. }).then_some(())).await?;
    let again = login(&addr, C2S::Resume { version: PROTOCOL_VERSION, token: mira.token.clone() }).await;
    assert!(again.is_err(), "banned sessions are gone");
    let pw = login(&addr, C2S::Login { version: PROTOCOL_VERSION, username: "mira".into(), password: "correct horse".into() }).await;
    assert!(pw.is_err(), "banned users can't log in");

    // The pinned fingerprint is stable across connections.
    let first = connect(&addr, None).await?;
    assert!(!first.ca_verified, "a self-signed certificate is pinned, not CA-verified");
    let a = first.fingerprint;
    let b = connect(&addr, Some(a.clone())).await?.fingerprint;
    assert_eq!(a, b);
    assert!(connect(&addr, Some("00:11".into())).await.is_err(), "a wrong pin is refused");
    Ok(())
}
