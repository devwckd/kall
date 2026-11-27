//! Video packets: one encoded frame split into datagram-sized fragments.
//!
//! ```text
//! client → server:  [2] [kind] [frame: u32 LE] [index: u16 LE] [count: u16 LE] [flags] [data …]
//! server → client:  [2] [user: u32 LE] [kind] [frame] [index] [count] [flags] [data …]
//! ```
//! A frame is usable only when every fragment arrives; a receiver that loses one drops the
//! frame and asks for a keyframe. The server forwards fragments only to people watching.
//!
//! A screen share also sends a preview: about once a second, a small frame that decodes on its
//! own ([`FLAG_PREVIEW`] | [`FLAG_KEYFRAME`]), with its own frame numbers. The server forwards
//! previews to everyone in the sender's voice channel, so their tiles show what's being shared
//! before anyone watches.

use std::collections::BTreeMap;

use crate::StreamKind;

pub const KIND_VIDEO: u8 = 2;
/// Payload bytes per fragment: fits a 1200-byte QUIC datagram with room to spare.
pub const FRAGMENT: usize = 1100;
pub const FLAG_KEYFRAME: u8 = 1;
/// A low-rate thumbnail frame, for everyone in the call (not just watchers).
pub const FLAG_PREVIEW: u8 = 2;
/// Frames bigger than this are refused (a 4K keyframe at high quality fits easily).
pub const MAX_FRAGMENTS: usize = 2048;

fn kind_byte(k: StreamKind) -> u8 {
    match k {
        StreamKind::Screen => 0,
        StreamKind::Camera => 1,
    }
}

fn kind_from(b: u8) -> Option<StreamKind> {
    match b {
        0 => Some(StreamKind::Screen),
        1 => Some(StreamKind::Camera),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment<'a> {
    pub kind: StreamKind,
    pub frame: u32,
    pub index: u16,
    pub count: u16,
    pub flags: u8,
    pub data: &'a [u8],
}

/// Split one encoded frame into upstream datagrams.
pub fn fragment(kind: StreamKind, frame: u32, flags: u8, data: &[u8]) -> Vec<Vec<u8>> {
    let chunks: Vec<&[u8]> = if data.is_empty() { vec![&[][..]] } else { data.chunks(FRAGMENT).collect() };
    let count = chunks.len() as u16;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let mut v = Vec::with_capacity(11 + c.len());
            v.push(KIND_VIDEO);
            v.push(kind_byte(kind));
            v.extend_from_slice(&frame.to_le_bytes());
            v.extend_from_slice(&(i as u16).to_le_bytes());
            v.extend_from_slice(&count.to_le_bytes());
            v.push(flags);
            v.extend_from_slice(c);
            v
        })
        .collect()
}

pub fn decode_up(b: &[u8]) -> Option<Fragment<'_>> {
    if b.len() < 11 || b[0] != KIND_VIDEO || b.len() - 11 > FRAGMENT {
        return None;
    }
    let f = Fragment {
        kind: kind_from(b[1])?,
        frame: u32::from_le_bytes(b[2..6].try_into().ok()?),
        index: u16::from_le_bytes([b[6], b[7]]),
        count: u16::from_le_bytes([b[8], b[9]]),
        flags: b[10],
        data: &b[11..],
    };
    (f.count > 0 && f.index < f.count && (f.count as usize) <= MAX_FRAGMENTS).then_some(f)
}

/// Server side: prefix the sender.
pub fn relay(user: u32, up: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(up.len() + 4);
    v.push(KIND_VIDEO);
    v.extend_from_slice(&user.to_le_bytes());
    v.extend_from_slice(&up[1..]);
    v
}

/// Client side: (sender, fragment).
pub fn decode_down(b: &[u8]) -> Option<(u32, Fragment<'_>)> {
    if b.len() < 15 || b[0] != KIND_VIDEO {
        return None;
    }
    let user = u32::from_le_bytes(b[1..5].try_into().ok()?);
    let mut up = Vec::with_capacity(b.len() - 4);
    up.push(KIND_VIDEO);
    up.extend_from_slice(&b[5..]);
    // Re-parse with the shared upstream layout (borrowing from `b` for the payload).
    let f = decode_up(&up)?;
    Some((user, Fragment { data: &b[15..], ..f }))
}

/// A frame put back together.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    pub id: u32,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// Collects fragments into frames, in order. A frame that can't complete (a fragment was
/// lost, or a newer frame finished first) is reported as a gap so the receiver can drop
/// predicted frames until the next keyframe.
#[derive(Default)]
pub struct Reassembler {
    pending: BTreeMap<u32, (u16, u8, Vec<Option<Vec<u8>>>)>,
    last_done: Option<u32>,
}

pub enum Assembled {
    Frame(Frame),
    /// Frames before this one were lost.
    Gap,
}

impl Reassembler {
    pub fn push(&mut self, f: &Fragment<'_>) -> Vec<Assembled> {
        if self.last_done.is_some_and(|d| f.frame.wrapping_sub(d) > u32::MAX / 2 || f.frame == d) {
            return vec![]; // old
        }
        let entry = self.pending.entry(f.frame).or_insert_with(|| (f.count, f.flags, vec![None; f.count as usize]));
        if entry.0 != f.count || (f.index as usize) >= entry.2.len() {
            return vec![];
        }
        entry.2[f.index as usize] = Some(f.data.to_vec());
        if entry.2.iter().any(Option::is_none) {
            // Keep at most a few frames in flight; older incomplete ones are lost.
            let mut out = Vec::new();
            while self.pending.len() > 4 {
                let (&old, _) = self.pending.iter().next().unwrap();
                self.pending.remove(&old);
                out.push(Assembled::Gap);
            }
            return out;
        }
        let (_, flags, parts) = self.pending.remove(&f.frame).unwrap();
        let mut out = Vec::new();
        // Anything older still pending can never be played in order now.
        let older: Vec<u32> = self.pending.keys().copied().filter(|&k| f.frame.wrapping_sub(k) < u32::MAX / 2).collect();
        let skipped = !older.is_empty() || self.last_done.is_some_and(|d| f.frame != d.wrapping_add(1));
        for k in older {
            self.pending.remove(&k);
        }
        if skipped {
            out.push(Assembled::Gap);
        }
        self.last_done = Some(f.frame);
        out.push(Assembled::Frame(Frame { id: f.frame, keyframe: flags & FLAG_KEYFRAME != 0, data: parts.into_iter().flatten().flatten().collect() }));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(out: Vec<Assembled>) -> Vec<String> {
        out.into_iter()
            .map(|a| match a {
                Assembled::Frame(f) => format!("F{}{}:{}", f.id, if f.keyframe { "k" } else { "" }, f.data.len()),
                Assembled::Gap => "gap".into(),
            })
            .collect()
    }

    #[test]
    fn fragments_round_trip_through_the_relay() {
        let data: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let parts = fragment(StreamKind::Screen, 7, FLAG_KEYFRAME, &data);
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.len() <= 1200));
        let mut r = Reassembler::default();
        let mut out = Vec::new();
        // Out of order is fine.
        for p in [&parts[2], &parts[0], &parts[1]] {
            let down = relay(42, p);
            let (user, f) = decode_down(&down).unwrap();
            assert_eq!(user, 42);
            assert_eq!(f.kind, StreamKind::Screen);
            out.extend(r.push(&f));
        }
        match out.pop() {
            Some(Assembled::Frame(f)) => {
                assert!(f.keyframe);
                assert_eq!(f.data, data);
            }
            _ => panic!("no frame"),
        }
    }

    #[test]
    fn lost_fragments_become_gaps() {
        let mut r = Reassembler::default();
        let mut push = |frame: u32, skip: bool| {
            let parts = fragment(StreamKind::Camera, frame, 0, &[1u8; 2500]);
            let mut out = Vec::new();
            for (i, p) in parts.iter().enumerate() {
                if skip && i == 1 {
                    continue;
                }
                out.extend(r.push(&decode_up(p).unwrap()));
            }
            frames(out)
        };
        assert_eq!(push(1, false), ["F1:2500"]);
        assert_eq!(push(2, true), Vec::<String>::new());
        assert_eq!(push(3, false), ["gap", "F3:2500"]);
        assert_eq!(push(4, false), ["F4:2500"]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(decode_up(&[2, 9, 0, 0, 0, 0, 0, 0, 1, 0, 0]).is_none(), "unknown kind");
        assert!(decode_up(&[2, 0, 0, 0, 0, 0, 5, 0, 1, 0, 0]).is_none(), "index past count");
        assert!(decode_down(&[2, 1, 2]).is_none());
    }
}
