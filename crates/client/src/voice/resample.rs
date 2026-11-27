//! Channel and sample-rate conversion between a device's format and the engine's 48 kHz mono.
//! Linear interpolation: plenty for speech, and nothing to do at all for 48 kHz devices.

pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    pos: f64,
    prev: f32,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        Resampler { step: from as f64 / to as f64, pos: 0.0, prev: 0.0 }
    }

    pub fn is_identity(&self) -> bool {
        (self.step - 1.0).abs() < 1e-9
    }

    /// Resample `input`, appending to `out`. State carries across calls.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.is_identity() {
            out.extend_from_slice(input);
            return;
        }
        // Positions are relative to `prev` at index -1 and input[0] at 0.
        while self.pos < input.len() as f64 {
            let i = self.pos.floor() as isize;
            let t = (self.pos - i as f64) as f32;
            let a = if i == 0 { self.prev } else { input[(i - 1) as usize] };
            let b = input[i as usize];
            out.push(a + (b - a) * t);
            self.pos += self.step;
        }
        self.pos -= input.len() as f64;
        if let Some(&last) = input.last() {
            self.prev = last;
        }
    }
}

/// Interleaved device frames → mono.
pub fn downmix(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    if channels <= 1 {
        out.extend_from_slice(input);
        return;
    }
    let k = 1.0 / channels as f32;
    out.extend(input.chunks_exact(channels).map(|f| f.iter().sum::<f32>() * k));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_rates_without_drifting() {
        let mut r = Resampler::new(44_100, 48_000);
        let mut out = Vec::new();
        let tone: Vec<f32> = (0..44_100).map(|i| (i as f32 * 0.01).sin()).collect();
        for chunk in tone.chunks(441) {
            r.process(chunk, &mut out);
        }
        assert!((out.len() as i64 - 48_000).abs() <= 2, "{}", out.len());
        let mut same = Resampler::new(48_000, 48_000);
        let mut o2 = Vec::new();
        same.process(&[0.5, 0.25], &mut o2);
        assert_eq!(o2, [0.5, 0.25]);
    }

    #[test]
    fn downmixes() {
        let mut out = Vec::new();
        downmix(&[1.0, 0.0, 0.5, 0.5], 2, &mut out);
        assert_eq!(out, [0.5, 0.5]);
    }
}
