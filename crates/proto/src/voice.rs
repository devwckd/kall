//! Voice packets: one 20 ms Opus frame per QUIC datagram.
//!
//! ```text
//! client → server:  [1] [seq: u16 LE] [flags: u8] [opus …]
//! server → client:  [1] [user: u32 LE] [seq: u16 LE] [flags: u8] [opus …]
//! ```
//! The server forwards each speaker's packets to the others in the same voice channel (an SFU)
//! without decoding them. `seq` counts 20 ms frames and wraps.

pub const KIND_VOICE: u8 = 1;
/// The packet is a screen share's system audio, not the microphone: it goes only to people
/// watching the share, and mute doesn't silence it.
pub const FLAG_SHARE_AUDIO: u8 = 2;
/// 48 kHz mono, 20 ms.
pub const SAMPLE_RATE: u32 = 48_000;
pub const FRAME_SAMPLES: usize = 960;
/// Leaves room for QUIC's own overhead inside a 1200-byte path MTU.
pub const MAX_OPUS_BYTES: usize = 1000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing<'a> {
    pub seq: u16,
    pub flags: u8,
    pub opus: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incoming<'a> {
    pub user: u32,
    pub seq: u16,
    pub flags: u8,
    pub opus: &'a [u8],
}

pub fn encode_up(seq: u16, flags: u8, opus: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + opus.len());
    v.push(KIND_VOICE);
    v.extend_from_slice(&seq.to_le_bytes());
    v.push(flags);
    v.extend_from_slice(opus);
    v
}

pub fn decode_up(b: &[u8]) -> Option<Outgoing<'_>> {
    if b.len() < 5 || b[0] != KIND_VOICE || b.len() - 4 > MAX_OPUS_BYTES {
        return None;
    }
    Some(Outgoing { seq: u16::from_le_bytes([b[1], b[2]]), flags: b[3], opus: &b[4..] })
}

/// Server side: re-stamp a client packet with its sender.
pub fn relay(user: u32, up: &Outgoing<'_>) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + up.opus.len());
    v.push(KIND_VOICE);
    v.extend_from_slice(&user.to_le_bytes());
    v.extend_from_slice(&up.seq.to_le_bytes());
    v.push(up.flags);
    v.extend_from_slice(up.opus);
    v
}

pub fn decode_down(b: &[u8]) -> Option<Incoming<'_>> {
    if b.len() < 9 || b[0] != KIND_VOICE {
        return None;
    }
    Some(Incoming { user: u32::from_le_bytes([b[1], b[2], b[3], b[4]]), seq: u16::from_le_bytes([b[5], b[6]]), flags: b[7], opus: &b[8..] })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let up = encode_up(65535, 1, b"opus!");
        let o = decode_up(&up).unwrap();
        assert_eq!((o.seq, o.flags, o.opus), (65535, 1, &b"opus!"[..]));
        let down = relay(42, &o);
        let i = decode_down(&down).unwrap();
        assert_eq!((i.user, i.seq, i.flags, i.opus), (42, 65535, 1, &b"opus!"[..]));
        assert!(decode_up(&[2, 0, 0, 0, 1]).is_none());
        assert!(decode_up(&[1, 0, 0]).is_none());
        assert!(decode_up(&vec![1; 2000]).is_none());
    }
}
