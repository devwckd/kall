//! A per-speaker jitter buffer: reorders 20 ms packets, holds a little audio in reserve, and
//! says what to play each tick: a packet, a frame rebuilt from the next packet's FEC, or
//! loss concealment. A speaker who stops sending goes idle and is buffered again on return.

use std::collections::BTreeMap;

pub enum Play {
    Packet(Vec<u8>),
    /// Decode this (the *next*) packet with FEC to recover the missing one.
    Fec(Vec<u8>),
    /// Missing with nothing to rebuild from: let the decoder conceal it.
    Conceal,
    Idle,
}

pub struct Jitter {
    buf: BTreeMap<u32, Vec<u8>>,
    next: u32,
    /// The highest extended sequence seen, for unwrapping u16 sequence numbers.
    top: Option<u32>,
    playing: bool,
    misses: u32,
    /// Frames to collect before starting to play (40 ms).
    prebuffer: usize,
}

const MAX_FRAMES: usize = 12;
/// Concealed frames before a speaker counts as gone quiet.
const IDLE_AFTER: u32 = 4;

impl Default for Jitter {
    fn default() -> Self {
        Jitter { buf: BTreeMap::new(), next: 0, top: None, playing: false, misses: 0, prebuffer: 2 }
    }
}

impl Jitter {
    fn extend(&mut self, seq: u16) -> u32 {
        let Some(top) = self.top else {
            let e = 1 << 20 | seq as u32;
            self.top = Some(e);
            return e;
        };
        let base = top & !0xFFFF;
        let candidates = [base.wrapping_sub(0x10000) | seq as u32, base | seq as u32, (base + 0x10000) | seq as u32];
        let e = *candidates.iter().min_by_key(|c| (**c as i64 - top as i64).abs()).unwrap();
        if e > top {
            self.top = Some(e);
        }
        e
    }

    pub fn push(&mut self, seq: u16, opus: Vec<u8>) {
        let e = self.extend(seq);
        if self.playing && e < self.next {
            return; // too late
        }
        self.buf.insert(e, opus);
        while self.buf.len() > MAX_FRAMES {
            let (&first, _) = self.buf.iter().next().unwrap();
            self.buf.remove(&first);
            if self.playing {
                self.next = self.next.max(first + 1);
            }
        }
    }

    /// What to play for the next 20 ms.
    pub fn pop(&mut self) -> Play {
        if !self.playing {
            if self.buf.len() < self.prebuffer {
                return Play::Idle;
            }
            self.playing = true;
            self.misses = 0;
            self.next = *self.buf.keys().next().unwrap();
        }
        // Running far behind (a burst arrived): skip ahead to keep latency down.
        while self.buf.len() > self.prebuffer + 4 {
            let (&first, _) = self.buf.iter().next().unwrap();
            self.buf.remove(&first);
            self.next = self.next.max(first + 1);
        }
        let at = self.next;
        self.next += 1;
        if let Some(p) = self.buf.remove(&at) {
            self.misses = 0;
            return Play::Packet(p);
        }
        self.misses += 1;
        if self.misses > IDLE_AFTER || self.buf.is_empty() && self.misses > 1 {
            self.playing = false;
            return Play::Idle;
        }
        match self.buf.get(&(at + 1)) {
            Some(p) => Play::Fec(p.clone()),
            None => Play::Conceal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(p: &Play) -> String {
        match p {
            Play::Packet(b) => format!("P{}", b[0]),
            Play::Fec(b) => format!("F{}", b[0]),
            Play::Conceal => "C".into(),
            Play::Idle => "-".into(),
        }
    }

    fn run(j: &mut Jitter, n: usize) -> Vec<String> {
        (0..n).map(|_| kind(&j.pop())).collect()
    }

    #[test]
    fn prebuffers_then_reorders() {
        let mut j = Jitter::default();
        assert_eq!(run(&mut j, 1), ["-"]);
        j.push(11, vec![11]);
        j.push(10, vec![10]);
        j.push(12, vec![12]);
        assert_eq!(run(&mut j, 3), ["P10", "P11", "P12"]);
    }

    #[test]
    fn recovers_loss_with_fec_then_conceals_then_goes_idle() {
        let mut j = Jitter::default();
        for s in [0u16, 1, 3, 4] {
            j.push(s, vec![s as u8]);
        }
        assert_eq!(run(&mut j, 4), ["P0", "P1", "F3", "P3"]);
        assert_eq!(run(&mut j, 3), ["P4", "C", "-"]);
        // Back again after a pause: buffered from scratch.
        j.push(30, vec![30]);
        assert_eq!(run(&mut j, 1), ["-"]);
        j.push(31, vec![31]);
        assert_eq!(run(&mut j, 2), ["P30", "P31"]);
    }

    #[test]
    fn unwraps_sequence_numbers_and_drops_late_packets() {
        let mut j = Jitter::default();
        j.push(65534, vec![1]);
        j.push(65535, vec![2]);
        j.push(0, vec![3]);
        j.push(1, vec![4]);
        assert_eq!(run(&mut j, 2), ["P1", "P2"]);
        j.push(65534, vec![9]); // late duplicate
        assert_eq!(run(&mut j, 2), ["P3", "P4"]);
    }

    #[test]
    fn catches_up_after_a_burst() {
        let mut j = Jitter::default();
        for s in 0..10u16 {
            j.push(s, vec![s as u8]);
        }
        // Skips the oldest so at most prebuffer + 4 stay queued.
        assert_eq!(run(&mut j, 1), ["P4"]);
    }
}
