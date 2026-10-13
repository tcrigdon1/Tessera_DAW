//! Shared helpers for instruments that play recorded samples (Sampler, Drum
//! Rack).

use std::sync::Arc;

use crate::model::AudioData;

/// The samples a track's instrument can play, indexed by the instrument's own
/// convention (Sampler: slot 0; Drum Rack: one slot per pad). Built on the UI
/// thread as part of the project snapshot and only read on the audio thread.
pub type SampleSlots = [Option<Arc<AudioData>>];

/// Returns the sample in `slot`, if it is loaded and long enough to play.
#[inline]
pub fn slot(samples: &SampleSlots, slot: usize) -> Option<&AudioData> {
    samples.get(slot).and_then(|s| s.as_deref()).filter(|d| d.frames() >= 4)
}

/// Reads one stereo frame at a fractional position using 4-point Hermite
/// interpolation. Positions outside the sample read as silence.
#[inline]
pub fn read_hermite(data: &AudioData, pos: f64) -> (f32, f32) {
    let n = data.frames() as isize;
    if pos < -1.0 || pos >= n as f64 {
        return (0.0, 0.0);
    }
    let i = pos.floor() as isize;
    let t = (pos - i as f64) as f32;
    let at = |ch: &[f32], k: isize| -> f32 {
        if k < 0 || k >= n {
            0.0
        } else {
            ch[k as usize]
        }
    };
    let interp = |ch: &[f32]| -> f32 {
        let xm1 = at(ch, i - 1);
        let x0 = at(ch, i);
        let x1 = at(ch, i + 1);
        let x2 = at(ch, i + 2);
        let c = (x1 - xm1) * 0.5;
        let v = x0 - x1;
        let w = c + v;
        let a = w + v + (x2 - x0) * 0.5;
        let b = w + a;
        ((a * t - b) * t + c) * t + x0
    };
    (interp(&data.left), interp(&data.right))
}

/// Finds transient onsets for slicing: positions (as fractions 0..1 of the
/// sample) where the short-term energy jumps. Always starts with 0. Runs on
/// the UI thread.
pub fn detect_transients(data: &AudioData, max: usize, sensitivity: f32) -> Vec<f32> {
    let n = data.frames();
    let mut out = vec![0.0f32];
    if n < 1024 || max <= 1 {
        return out;
    }
    let hop = 256usize;
    let energies: Vec<f32> = (0..n / hop)
        .map(|b| {
            let s = b * hop;
            (s..(s + hop).min(n))
                .map(|i| {
                    let v = 0.5 * (data.left[i] + data.right[i]);
                    v * v
                })
                .sum::<f32>()
                / hop as f32
        })
        .collect();
    // Ratio threshold: 1 (very sensitive) .. 8 (only big hits).
    let ratio = 1.5 + (1.0 - sensitivity.clamp(0.0, 1.0)) * 6.5;
    let floor = energies.iter().fold(0.0f32, |a, &b| a.max(b)) * 1e-3;
    let min_gap = (data.sample_rate as usize / 10 / hop).max(1); // 100 ms
    let mut last = 0usize;
    let mut candidates: Vec<(usize, f32)> = Vec::new();
    for b in 1..energies.len() {
        let prev = energies[b.saturating_sub(4)..b].iter().sum::<f32>() / (b - b.saturating_sub(4)) as f32;
        let e = energies[b];
        if e > floor && e > prev * ratio && b - last >= min_gap {
            candidates.push((b, e / (prev + 1e-9)));
            last = b;
        }
    }
    // Keep the strongest onsets if there are too many.
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    candidates.truncate(max - 1);
    let mut pos: Vec<f32> = candidates
        .iter()
        .map(|(b, _)| (*b * hop) as f32 / n as f32)
        .filter(|&p| p > 0.005)
        .collect();
    pos.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out.extend(pos);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(left: Vec<f32>) -> AudioData {
        AudioData {
            sample_rate: 48000,
            right: left.clone(),
            left,
        }
    }

    #[test]
    fn hermite_hits_samples_exactly_and_interpolates_lines() {
        let d = data((0..16).map(|i| i as f32).collect());
        for i in 1..14 {
            let (l, r) = read_hermite(&d, i as f64);
            assert!((l - i as f32).abs() < 1e-5 && (r - i as f32).abs() < 1e-5);
        }
        let (l, _) = read_hermite(&d, 5.5);
        assert!((l - 5.5).abs() < 1e-4);
        assert_eq!(read_hermite(&d, 100.0), (0.0, 0.0));
    }

    #[test]
    fn transients_are_found_at_hits() {
        let sr = 48000;
        let mut v = vec![0.0f32; sr * 2];
        for hit in [0usize, 24000, 48000, 72000] {
            for i in 0..4000 {
                v[hit + i] = (i as f32 * 0.05).sin() * (-(i as f32) / 800.0).exp();
            }
        }
        let t = detect_transients(&data(v), 16, 0.5);
        assert_eq!(t.len(), 4, "{t:?}");
        for (k, p) in t.iter().enumerate() {
            assert!((p - k as f32 * 0.25).abs() < 0.01, "{t:?}");
        }
    }
}
