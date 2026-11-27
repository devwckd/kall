//! Files in chat, on the server: where they're kept, the transfer streams that move them, and
//! the hourly cleanup (expired files, uploads that were never sent).
//!
//! A file lives at `<data>/files/<id>`, and an image's preview at `<id>.thumb.png`. Uploads go
//! to a `.part` file first and are renamed into place once complete.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use kall_proto::files::{Attachment, FileKind, FileLimits, Transfer, TransferReply, clean_name, human_size};
use kall_proto::{Id, S2C};
use tokio::io::AsyncWriteExt;

use crate::db::now;
use crate::hub::Hub;

/// Longest side of an image preview.
const THUMB: u32 = 480;
/// Uploads that aren't sent in a message within this long are removed.
const UNSENT_MS: i64 = 24 * 3600 * 1000;

pub struct FileStore {
    pub dir: PathBuf,
    pub limits: FileLimits,
}

impl FileStore {
    pub fn new(dir: PathBuf, max_bytes: u64, keep_days: u32) -> FileStore {
        FileStore { dir, limits: FileLimits { max_bytes, keep_days } }
    }

    /// 100 MB, kept 30 days.
    pub fn defaults(dir: PathBuf) -> FileStore {
        FileStore::new(dir, 100_000_000, 30)
    }

    fn path(&self, id: Id) -> PathBuf {
        self.dir.join(id.to_string())
    }

    fn thumb_path(&self, id: Id) -> PathBuf {
        self.dir.join(format!("{id}.thumb.png"))
    }

    /// Remove files from disk (missing ones are fine).
    pub fn remove(&self, ids: &[Id]) {
        for &id in ids {
            std::fs::remove_file(self.path(id)).ok();
            std::fs::remove_file(self.thumb_path(id)).ok();
        }
    }
}

/// An image's size and a small PNG preview, or None if it isn't an image after all.
fn image_preview(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let img = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    let small = if w.max(h) > THUMB { img.thumbnail(THUMB, THUMB) } else { img };
    let mut png = std::io::Cursor::new(Vec::new());
    small.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some((w, h, png.into_inner()))
}

impl Hub {
    /// One transfer stream (see [`kall_proto::files`]): an upload or a download by `user`.
    pub async fn transfer(self: Arc<Self>, user: Id, mut send: quinn::SendStream, mut recv: quinn::RecvStream) -> Result<()> {
        let req: Transfer = kall_proto::recv(&mut recv).await?.ok_or_else(|| anyhow!("empty transfer"))?;
        let r = match req {
            Transfer::Upload { name, size } => self.upload(user, &name, size, &mut send, &mut recv).await,
            Transfer::Download { file, thumb } => self.download(user, file, thumb, &mut send).await,
        };
        if let Err(e) = &r {
            kall_proto::send(&mut send, &TransferReply::Error(e.to_string())).await.ok();
        }
        send.finish().ok();
        // Let the last bytes go out before the stream is dropped.
        send.stopped().await.ok();
        r
    }

    async fn upload(&self, user: Id, name: &str, size: u64, send: &mut quinn::SendStream, recv: &mut quinn::RecvStream) -> Result<()> {
        let max = self.files.limits.max_bytes;
        if size == 0 {
            bail!("That file is empty.");
        }
        if size > max {
            bail!("Files can be up to {}.", human_size(max));
        }
        let name = clean_name(name);
        tokio::fs::create_dir_all(&self.files.dir).await?;
        let part = self.files.dir.join(format!("upload-{}.part", crate::hub::random_code(12)));
        kall_proto::send(send, &TransferReply::Go).await?;
        let got = async {
            let mut f = tokio::fs::File::create(&part).await?;
            let mut total = 0u64;
            while let Some(chunk) = recv.read_chunk(64 * 1024, true).await? {
                total += chunk.bytes.len() as u64;
                if total > size {
                    bail!("More data than announced.");
                }
                f.write_all(&chunk.bytes).await?;
            }
            f.flush().await?;
            if total != size {
                bail!("The upload stopped early.");
            }
            Ok(())
        }
        .await;
        if let Err(e) = got {
            tokio::fs::remove_file(&part).await.ok();
            return Err(e);
        }
        // Images: check they decode, note their size, make a preview.
        let mut kind = FileKind::of(&name);
        let (mut w, mut h, mut thumb) = (0, 0, None);
        if kind == FileKind::Image {
            let p = part.clone();
            match tokio::task::spawn_blocking(move || image_preview(&p)).await? {
                Some((iw, ih, png)) => (w, h, thumb) = (iw, ih, Some(png)),
                None => kind = FileKind::Other,
            }
        }
        let att = self.db.add_file(user, &name, size, kind, w, h, thumb.is_some())?;
        tokio::fs::rename(&part, self.files.path(att.id)).await?;
        if let Some(png) = thumb {
            tokio::fs::write(self.files.thumb_path(att.id), png).await?;
        }
        tracing::info!("upload by {user}: {} ({})", att.name, human_size(att.size));
        kall_proto::send(send, &TransferReply::Stored(att)).await?;
        Ok(())
    }

    async fn download(&self, user: Id, file: Id, thumb: bool, send: &mut quinn::SendStream) -> Result<()> {
        let (att, owner, message) = self.db.file(file)?.ok_or_else(|| anyhow!("That file is gone."))?;
        // Sent files are for everyone on the server; unsent ones only for their uploader.
        let visible = match message {
            Some(m) => self.db.message(m)?.is_some(),
            None => owner == user,
        };
        if !visible {
            bail!("That file is gone.");
        }
        if att.expired {
            bail!("That file has expired.");
        }
        let path = if thumb { self.files.thumb_path(file) } else { self.files.path(file) };
        let mut f = tokio::fs::File::open(&path).await.map_err(|_| anyhow!("That file is gone."))?;
        let len = f.metadata().await?.len();
        kall_proto::send(send, &TransferReply::Size(len)).await?;
        tokio::io::copy(&mut f, send).await?;
        Ok(())
    }

    /// Every hour: expire old files (and tell everyone), and drop uploads never sent.
    pub fn start_file_janitor(self: &Arc<Self>) {
        let hub = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = hub.clean_files() {
                    tracing::warn!("file cleanup: {e:#}");
                }
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        });
    }

    pub fn clean_files(&self) -> Result<()> {
        let unsent = self.db.drop_unsent_files(now() - UNSENT_MS)?;
        self.files.remove(&unsent);
        let days = self.files.limits.keep_days;
        if days == 0 {
            return Ok(());
        }
        let expired = self.db.expire_files(now() - days as i64 * 24 * 3600 * 1000)?;
        self.files.remove(&expired.iter().map(|(f, _)| *f).collect::<Vec<_>>());
        let mut messages: Vec<Id> = expired.iter().map(|(_, m)| *m).collect();
        messages.dedup();
        for m in messages {
            if let Some(msg) = self.db.message(m)? {
                self.broadcast(S2C::MessageUpdated(msg));
            }
        }
        if !expired.is_empty() {
            tracing::info!("expired {} file(s)", expired.len());
        }
        Ok(())
    }

    /// The attachments a new message may carry: `user`'s own unsent uploads.
    pub fn check_attachments(&self, user: Id, files: &[Id]) -> Result<Vec<Attachment>> {
        if files.len() > 10 {
            bail!("Up to 10 files per message.");
        }
        let mut out = Vec::new();
        for &f in files {
            match self.db.file(f)? {
                Some((att, owner, None)) if owner == user && !att.expired => out.push(att),
                _ => bail!("That file isn't yours to send (or it was already sent)."),
            }
        }
        Ok(out)
    }
}
