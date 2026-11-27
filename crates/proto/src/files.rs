//! Files in chat: what a message's attachments look like, and the little protocol on the
//! streams that carry them.
//!
//! Each upload or download is its own bidirectional QUIC stream, opened by the client after the
//! control stream, so a 100 MB file never holds up chat. The client sends one framed
//! [`Transfer`]; the server answers with framed [`TransferReply`]s and raw bytes:
//!
//! ```text
//! upload:   client → Upload { name, size }      server → Go | Error
//!           client → <size raw bytes>, finish   server → Stored(attachment) | Error
//! download: client → Download { file, thumb }   server → Size(n) <n raw bytes> | Error
//! ```
//! Closing the stream early cancels. Uploads that never end up in a message are removed by the
//! server after a while.

use serde::{Deserialize, Serialize};

use crate::Id;

/// What kind of file, for how the client shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileKind {
    /// Shown inline from its thumbnail.
    Image,
    /// Played inline where the platform can.
    Video,
    Audio,
    /// A card with a Download button.
    Other,
}

impl FileKind {
    /// By extension (the server double-checks images by decoding them).
    pub fn of(name: &str) -> FileKind {
        let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => FileKind::Image,
            "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" => FileKind::Video,
            "mp3" | "m4a" | "aac" | "wav" | "flac" | "ogg" | "opus" => FileKind::Audio,
            _ => FileKind::Other,
        }
    }
}

/// A file attached to a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub id: Id,
    pub name: String,
    pub size: u64,
    pub kind: FileKind,
    /// Image size in pixels (0 when unknown or not an image).
    pub width: u32,
    pub height: u32,
    /// The server has a small preview (images): `Download { thumb: true }`.
    pub thumb: bool,
    /// Removed after the server's retention period; only the name and size remain.
    pub expired: bool,
}

/// The server's file rules, sent at login.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileLimits {
    /// Largest upload, in bytes.
    pub max_bytes: u64,
    /// Files are removed this many days after upload (0: kept forever).
    pub keep_days: u32,
}

/// The first frame on a transfer stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Transfer {
    Upload { name: String, size: u64 },
    Download { file: Id, thumb: bool },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum TransferReply {
    /// Upload accepted: send the bytes.
    Go,
    /// Upload stored; attach it to a message with `SendMessage { attachments }`.
    Stored(Attachment),
    /// Download: this many raw bytes follow.
    Size(u64),
    Error(String),
}

/// "4.2 MB", for file cards and limits.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else if v < 10.0 { format!("{v:.1} {}", UNITS[u]) } else { format!("{v:.0} {}", UNITS[u]) }
}

/// A file name safe to show and to save under: no paths, no control characters, not empty.
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = base.chars().filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')).collect();
    let cleaned = cleaned.trim().trim_matches('.').to_string();
    let cleaned: String = cleaned.chars().take(120).collect();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_extension() {
        assert_eq!(FileKind::of("Cat.JPG"), FileKind::Image);
        assert_eq!(FileKind::of("clip.webm"), FileKind::Video);
        assert_eq!(FileKind::of("song.mp3"), FileKind::Audio);
        assert_eq!(FileKind::of("notes.txt"), FileKind::Other);
        assert_eq!(FileKind::of("noext"), FileKind::Other);
    }

    #[test]
    fn sizes_read_nicely() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(4_200_000), "4.2 MB");
        assert_eq!(human_size(100_000_000), "100 MB");
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("C:\\Users\\x\\photo.png"), "photo.png");
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("a<b>c?.txt"), "abc.txt");
        assert_eq!(clean_name("..."), "file");
    }
}
