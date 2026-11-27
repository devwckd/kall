//! Voice datagrams through the SFU: who hears whom.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use kall_proto::client::{Connected, connect};
use kall_proto::voice;
use kall_proto::{C2S, ChannelKind, PROTOCOL_VERSION, S2C, Snapshot};
use kall_server::{Server, db::Db};

struct Client {
    c: Connected,
    snap: Snapshot,
    token: String,
}

impl Client {
    async fn send(&mut self, m: C2S) -> Result<()> {
        kall_proto::send(&mut self.c.send, &m).await
    }
    async fn expect<T>(&mut self, mut f: impl FnMut(S2C) -> Option<T>) -> Result<T> {
        for _ in 0..50 {
            let m = tokio::time::timeout(Duration::from_secs(5), kall_proto::recv(&mut self.c.recv)).await??.ok_or_else(|| anyhow!("closed"))?;
            if let Some(t) = f(m) {
                return Ok(t);
            }
        }
        bail!("never got the expected frame")
    }
    fn speak(&self, seq: u16) {
        self.c.conn.send_datagram(voice::encode_up(seq, 1, &[0xAB; 60]).into()).unwrap();
    }
    /// The (user, seq) of voice packets that arrive within `wait`.
    async fn heard(&self, wait: Duration) -> Vec<(u32, u16)> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + wait;
        while let Ok(Ok(d)) = tokio::time::timeout_at(deadline, self.c.conn.read_datagram()).await {
            let p = voice::decode_down(&d).unwrap();
            assert_eq!(p.opus, &[0xAB; 60]);
            out.push((p.user, p.seq));
        }
        out
    }
}

async fn login(addr: &str, first: C2S) -> Result<Client> {
    let mut c = connect(addr, None).await?;
    kall_proto::send(&mut c.send, &first).await?;
    let Some(S2C::AuthOk { token, .. }) = kall_proto::recv(&mut c.recv).await? else { bail!("auth failed") };
    let Some(S2C::Ready(snap)) = kall_proto::recv(&mut c.recv).await? else { bail!("no Ready") };
    Ok(Client { c, snap, token })
}

async fn join(addr: &str, invite: &str, name: &str) -> Result<Client> {
    login(addr, C2S::Register { version: PROTOCOL_VERSION, invite: invite.into(), username: name.into(), password: "correct horse".into() }).await
}

async fn start() -> Result<(tempfile::TempDir, String, String)> {
    let dir = tempfile::tempdir()?;
    let id = kall_server::tls::self_signed(dir.path(), &[])?;
    let server = Server::start(Db::open(&dir.path().join("kall.db"))?, "T".into(), "127.0.0.1:0".parse()?, id)?;
    let addr = server.endpoint.local_addr()?.to_string();
    let invite = server.owner_invite.clone().unwrap();
    tokio::spawn(async move { server.run().await });
    Ok((dir, addr, invite))
}

/// Voice belongs to the connection that joined: a second login of the same account (or a
/// reconnect while the old connection is still timing out) starts outside the call, and the
/// old connection ending takes the user out even though they're still online.
#[tokio::test(flavor = "multi_thread")]
async fn voice_follows_the_connection_that_joined() -> Result<()> {
    let (_dir, addr, invite) = start().await?;
    let mut first = join(&addr, &invite, "owner").await?;
    first.send(C2S::CreateInvite { uses: Some(1), hours: None }).await?;
    let code = first.expect(|m| if let S2C::InviteCreated(i) = m { Some(i.code) } else { None }).await?;
    let lounge = first.snap.channels.iter().find(|c| c.kind == ChannelKind::Voice).unwrap().id;
    let mut friend = join(&addr, &code, "friend").await?;
    let owner = first.snap.me;

    for cl in [&mut first, &mut friend] {
        cl.send(C2S::JoinVoice { channel: lounge }).await?;
        let me = cl.snap.me;
        cl.expect(|m| matches!(m, S2C::VoiceUpdated { user, state: Some(_) } if user == me).then_some(())).await?;
    }

    // A second session of the same account: sees itself in voice (the other session is),
    // but neither sends nor hears voice.
    let second = login(&addr, C2S::Resume { version: PROTOCOL_VERSION, token: first.token.clone() }).await?;
    assert!(second.snap.voice.iter().any(|v| v.user == owner));
    let w = Duration::from_millis(300);
    friend.speak(1);
    let (h1, h2) = tokio::join!(first.heard(w), second.heard(w));
    assert_eq!(h1.len(), 1);
    assert!(h2.is_empty(), "only the joined session hears voice");
    second.speak(2);
    assert!(friend.heard(w).await.is_empty(), "only the joined session can speak");

    // The joined session ends: the owner leaves voice although still online elsewhere.
    first.c.conn.close(0u32.into(), b"bye");
    friend.expect(|m| matches!(m, S2C::VoiceUpdated { user, state: None } if user == owner).then_some(())).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn voice_reaches_only_the_right_people() -> Result<()> {
    let (_dir, addr, owner_invite) = start().await?;

    let mut owner = join(&addr, &owner_invite, "owner").await?;
    owner.send(C2S::CreateInvite { uses: Some(3), hours: None }).await?;
    let code = owner.expect(|m| if let S2C::InviteCreated(i) = m { Some(i.code) } else { None }).await?;
    owner.send(C2S::CreateChannel { name: "Other".into(), kind: ChannelKind::Voice }).await?;
    let other = owner.expect(|m| if let S2C::ChannelUpdated(c) = m { Some(c.id) } else { None }).await?;
    let lounge = owner.snap.channels.iter().find(|c| c.kind == ChannelKind::Voice).unwrap().id;

    let mut a = join(&addr, &code, "ana").await?;
    let mut b = join(&addr, &code, "bob").await?;
    let mut c = join(&addr, &code, "cid").await?;
    for (cl, ch) in [(&mut owner, lounge), (&mut a, lounge), (&mut b, lounge), (&mut c, other)] {
        cl.send(C2S::JoinVoice { channel: ch }).await?;
        let me = cl.snap.me;
        cl.expect(|m| matches!(m, S2C::VoiceUpdated { user, state: Some(_) } if user == me).then_some(())).await?;
    }
    let ana = a.snap.me as u32;

    // Same channel hears it, tagged with the speaker; another channel doesn't; nobody hears themselves.
    for seq in 0..3 {
        a.speak(seq);
    }
    let w = Duration::from_millis(400);
    let (ho, hb, hc, ha) = tokio::join!(owner.heard(w), b.heard(w), c.heard(w), a.heard(w));
    assert_eq!(ho, vec![(ana, 0), (ana, 1), (ana, 2)]);
    assert_eq!(hb.len(), 3);
    assert!(hc.is_empty() && ha.is_empty());

    // Deafened listeners get nothing.
    b.send(C2S::VoiceState { muted: false, deafened: true }).await?;
    owner.expect(|m| matches!(m, S2C::VoiceUpdated { state: Some(v), .. } if v.deafened).then_some(())).await?;
    a.speak(3);
    let (ho, hb) = tokio::join!(owner.heard(w), b.heard(w));
    assert_eq!(ho, vec![(ana, 3)]);
    assert!(hb.is_empty());

    // Muted speakers are dropped at the server.
    a.send(C2S::VoiceState { muted: true, deafened: false }).await?;
    owner.expect(|m| matches!(m, S2C::VoiceUpdated { state: Some(v), .. } if v.muted).then_some(())).await?;
    a.speak(4);
    assert!(owner.heard(w).await.is_empty());

    // Garbage datagrams are ignored, and leaving voice stops forwarding.
    a.send(C2S::VoiceState { muted: false, deafened: false }).await?;
    owner.expect(|m| matches!(m, S2C::VoiceUpdated { state: Some(v), .. } if !v.muted).then_some(())).await?;
    a.c.conn.send_datagram(vec![9u8; 50].into())?;
    a.send(C2S::LeaveVoice).await?;
    owner.expect(|m| matches!(m, S2C::VoiceUpdated { state: None, .. }).then_some(())).await?;
    a.speak(5);
    assert!(owner.heard(w).await.is_empty());
    Ok(())
}

impl Client {
    /// Datagrams that arrive within `wait`, raw.
    async fn datagrams(&self, wait: Duration) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + wait;
        while let Ok(Ok(d)) = tokio::time::timeout_at(deadline, self.c.conn.read_datagram()).await {
            out.push(d.to_vec());
        }
        out
    }
}

/// Video and share audio reach only the people watching; streams end with the call.
#[tokio::test(flavor = "multi_thread")]
async fn video_goes_only_to_watchers() -> Result<()> {
    use kall_proto::video;
    use kall_proto::{StreamInfo, StreamKind};
    let (_dir, addr, invite) = start().await?;
    let mut a = join(&addr, &invite, "ana").await?;
    a.send(C2S::CreateInvite { uses: Some(2), hours: None }).await?;
    let code = a.expect(|m| if let S2C::InviteCreated(i) = m { Some(i.code) } else { None }).await?;
    let lounge = a.snap.channels.iter().find(|c| c.kind == ChannelKind::Voice).unwrap().id;
    let mut b = join(&addr, &code, "bob").await?;
    let mut c = join(&addr, &code, "cid").await?;
    for cl in [&mut a, &mut b, &mut c] {
        cl.send(C2S::JoinVoice { channel: lounge }).await?;
        let me = cl.snap.me;
        cl.expect(|m| matches!(m, S2C::VoiceUpdated { user, state: Some(_) } if user == me).then_some(())).await?;
    }
    let ana = a.snap.me;
    let info = StreamInfo { kind: StreamKind::Screen, width: 1920, height: 1080, fps: 30, audio: true, label: "Display 1".into() };
    a.send(C2S::StartStream(info.clone())).await?;
    b.expect(|m| matches!(&m, S2C::StreamUpdated { user, info: Some(i), .. } if *user == ana && *i == info).then_some(())).await?;

    let frame = vec![7u8; 2500];
    let send_frame = |a: &Client, id: u32| {
        for d in video::fragment(StreamKind::Screen, id, video::FLAG_KEYFRAME, &frame) {
            a.c.conn.send_datagram(d.into()).unwrap();
        }
    };
    let w = Duration::from_millis(300);
    send_frame(&a, 1);
    assert!(b.datagrams(w).await.is_empty(), "nobody is watching yet");

    // Bob starts watching: Ana is asked for a keyframe, then Bob (only) gets the fragments.
    b.send(C2S::WatchStream { user: ana, kind: StreamKind::Screen, watch: true }).await?;
    a.expect(|m| matches!(m, S2C::KeyframeRequested { kind: StreamKind::Screen }).then_some(())).await?;
    send_frame(&a, 2);
    a.c.conn.send_datagram(voice::encode_up(1, voice::FLAG_SHARE_AUDIO, &[0xAB; 60]).into())?;
    let (gb, gc) = tokio::join!(b.datagrams(w), c.datagrams(w));
    let mut r = video::Reassembler::default();
    let mut got_frame = false;
    let mut got_audio = false;
    for d in &gb {
        if let Some((user, f)) = video::decode_down(d) {
            assert_eq!(user as i64, ana);
            got_frame |= r.push(&f).iter().any(|x| matches!(x, video::Assembled::Frame(f) if f.data == frame));
        } else if voice::decode_down(d).is_some_and(|p| p.flags & voice::FLAG_SHARE_AUDIO != 0) {
            got_audio = true;
        }
    }
    assert!(got_frame && got_audio, "watcher gets the picture and the share audio");
    assert!(gc.is_empty(), "a non-watcher gets neither");

    // Leaving voice ends the stream for everyone.
    a.send(C2S::LeaveVoice).await?;
    b.expect(|m| matches!(m, S2C::StreamUpdated { user, info: None, .. } if user == ana).then_some(())).await?;
    Ok(())
}
