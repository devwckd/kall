//! Files in chat over real QUIC: upload, send, download, image previews, limits, who can see
//! what, deleting and expiry.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use kall_proto::client::{Connected, connect, download, upload};
use kall_proto::files::FileKind;
use kall_proto::{C2S, ChannelKind, PROTOCOL_VERSION, S2C, Snapshot};
use kall_server::files::FileStore;
use kall_server::{Server, db::Db};

struct Client {
    c: Connected,
    snap: Snapshot,
}

impl Client {
    async fn send(&mut self, m: C2S) -> Result<()> {
        kall_proto::send(&mut self.c.send, &m).await
    }

    async fn expect<T>(&mut self, mut f: impl FnMut(S2C) -> Option<T>) -> Result<T> {
        for _ in 0..50 {
            let m = tokio::time::timeout(Duration::from_secs(5), kall_proto::recv(&mut self.c.recv)).await.map_err(|_| anyhow!("timed out"))??.ok_or_else(|| anyhow!("closed"))?;
            if let Some(t) = f(m) {
                return Ok(t);
            }
        }
        bail!("never got the expected frame")
    }

    async fn get(&self, file: i64, thumb: bool) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        download(&self.c.conn, file, thumb, &mut out, |_, _| {}).await?;
        Ok(out)
    }
}

async fn join(addr: &str, invite: &str, user: &str) -> Result<Client> {
    let mut c = connect(addr, None).await?;
    let first = C2S::Register { version: PROTOCOL_VERSION, invite: invite.into(), username: user.into(), password: "correct horse".into() };
    kall_proto::send(&mut c.send, &first).await?;
    match kall_proto::recv(&mut c.recv).await? {
        Some(S2C::AuthOk { .. }) => {}
        other => bail!("auth: {other:?}"),
    }
    let snap = match kall_proto::recv(&mut c.recv).await? {
        Some(S2C::Ready(s)) => s,
        other => bail!("expected Ready, got {other:?}"),
    };
    Ok(Client { c, snap })
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 90]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

#[tokio::test(flavor = "multi_thread")]
async fn files_round_trip() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let identity = kall_server::tls::self_signed(dir.path(), &[])?;
    // A 1 MB limit so the "too big" case is cheap.
    let files = FileStore::new(dir.path().join("files"), 1_000_000, 30);
    let server = Server::start(Db::open(&dir.path().join("kall.db"))?, "T".into(), "127.0.0.1:0".parse()?, identity, files)?;
    let addr = server.endpoint.local_addr()?.to_string();
    let invite = server.owner_invite.clone().unwrap();
    let hub = server.hub.clone();
    tokio::spawn(async move { server.run().await });

    let mut sam = join(&addr, &invite, "sam").await?;
    assert_eq!(sam.snap.files.max_bytes, 1_000_000);
    sam.send(C2S::CreateInvite { uses: Some(1), hours: None }).await?;
    let code = sam.expect(|m| if let S2C::InviteCreated(i) = m { Some(i.code) } else { None }).await?;
    let mut mira = join(&addr, &code, "mira").await?;
    let general = sam.snap.channels.iter().find(|c| c.kind == ChannelKind::Text).unwrap().id;

    // Upload a document and an image, with progress.
    let doc = b"hello from a file".repeat(1000);
    let mut seen = 0;
    let a = upload(&sam.c.conn, "notes.txt", doc.len() as u64, &doc[..], |n| seen = n).await?;
    assert_eq!((a.kind, a.size, seen), (FileKind::Other, doc.len() as u64, doc.len() as u64));
    let pic = png(1200, 800);
    let b = upload(&sam.c.conn, "../../sneaky/photo.png", pic.len() as u64, &pic[..], |_| {}).await?;
    assert_eq!((b.kind, b.width, b.height, b.thumb, b.name.as_str()), (FileKind::Image, 1200, 800, true, "photo.png"));

    // Too big, and lying about the size, are refused.
    let big = vec![0u8; 1_000_001];
    assert!(upload(&sam.c.conn, "big.bin", big.len() as u64, &big[..], |_| {}).await.unwrap_err().to_string().contains("up to"));
    assert!(upload(&sam.c.conn, "short.bin", 100, &[1u8; 10][..], |_| {}).await.is_err());

    // Before it's sent, only the uploader can fetch it.
    assert_eq!(sam.get(a.id, false).await?, doc);
    assert!(mira.get(a.id, false).await.is_err());

    // Send both in a message (no text needed); everyone gets the attachments.
    sam.send(C2S::SendMessage { channel: general, body: String::new(), reply_to: None, attachments: vec![a.id, b.id] }).await?;
    let msg = mira.expect(|m| if let S2C::MessageCreated(m) = m { Some(m) } else { None }).await?;
    assert_eq!(msg.attachments.iter().map(|f| f.id).collect::<Vec<_>>(), vec![a.id, b.id]);
    assert_eq!(mira.get(a.id, false).await?, doc);
    assert_eq!(mira.get(b.id, false).await?, pic);
    let thumb = image::load_from_memory(&mira.get(b.id, true).await?)?;
    assert_eq!((thumb.width(), thumb.height()), (480, 320), "previews fit 480 px");

    // A file goes in one message only, and only its uploader can send it.
    sam.send(C2S::SendMessage { channel: general, body: "again".into(), reply_to: None, attachments: vec![a.id] }).await?;
    sam.expect(|m| matches!(m, S2C::Error { .. }).then_some(())).await?;
    let c = upload(&sam.c.conn, "c.txt", 3, &b"abc"[..], |_| {}).await?;
    mira.send(C2S::SendMessage { channel: general, body: "mine?".into(), reply_to: None, attachments: vec![c.id] }).await?;
    mira.expect(|m| matches!(m, S2C::Error { .. }).then_some(())).await?;

    // History carries attachments.
    mira.send(C2S::LoadHistory { channel: general, before: None, limit: 50 }).await?;
    let hist = mira.expect(|m| if let S2C::History { messages, .. } = m { Some(messages) } else { None }).await?;
    assert_eq!(hist.iter().find(|m| m.id == msg.id).unwrap().attachments.len(), 2);

    // Expiry: pretend 30 days passed. The message stays; its files say expired.
    hub.db.expire_files(i64::MAX)?;
    let m = hub.db.message(msg.id)?.unwrap();
    assert!(m.attachments.iter().all(|f| f.expired));
    assert!(mira.get(a.id, false).await.unwrap_err().to_string().contains("expired"));

    // Deleting the message removes its files.
    sam.send(C2S::DeleteMessage { id: msg.id }).await?;
    sam.expect(|m| matches!(m, S2C::MessageDeleted { .. }).then_some(())).await?;
    assert!(hub.db.file(a.id)?.is_none());
    assert!(!dir.path().join("files").join(a.id.to_string()).exists());
    Ok(())
}
