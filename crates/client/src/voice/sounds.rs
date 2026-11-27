//! Little synthesized cues (join, leave, mute, unmute), so the app ships no sound files.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Join,
    Leave,
    Mute,
    Unmute,
    /// A message notification.
    Message,
}

/// 48 kHz mono samples for a cue: short sine notes with a soft envelope.
pub fn render(cue: Cue) -> Vec<f32> {
    let notes: &[(f32, f32)] = match cue {
        Cue::Join => &[(660.0, 0.07), (880.0, 0.11)],
        Cue::Leave => &[(880.0, 0.07), (587.0, 0.11)],
        Cue::Mute => &[(440.0, 0.06)],
        Cue::Unmute => &[(740.0, 0.06)],
        Cue::Message => &[(988.0, 0.05), (1318.0, 0.09)],
    };
    let mut out = Vec::new();
    for &(freq, secs) in notes {
        let n = (48_000.0 * secs) as usize;
        for i in 0..n {
            let t = i as f32 / 48_000.0;
            let attack = (i as f32 / 240.0).min(1.0);
            let release = ((n - i) as f32 / (n as f32 * 0.6)).min(1.0);
            let env = attack * release * release;
            let s = (t * freq * std::f32::consts::TAU).sin() + 0.25 * (t * freq * 2.0 * std::f32::consts::TAU).sin();
            out.push(0.18 * env * s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn cues_are_short_and_quiet() {
        for c in [super::Cue::Join, super::Cue::Leave, super::Cue::Mute, super::Cue::Unmute, super::Cue::Message] {
            let s = super::render(c);
            assert!(s.len() < 48_000 / 4);
            assert!(s.iter().all(|x| x.abs() < 0.3));
        }
    }
}
