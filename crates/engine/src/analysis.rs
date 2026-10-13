//! Offline audio analysis for warping: transient detection, tempo
//! estimation and auto-warp. Runs on the UI thread (allocates).

use crate::model::AudioData;
use crate::warp::{WarpMarker, WarpMode, WarpSettings};

/// Analysis frame hop, in seconds.
const HOP_SECS: f64 = 0.005;
/// Transients closer than this are merged.
const MIN_SPACING: f64 = 0.045;

/// Onset strength per analysis frame, plus the frame hop in samples.
fn onset_strength(data: &AudioData) -> (Vec<f32>, usize) {
    let hop = ((data.sample_rate as f64 * HOP_SECS) as usize).max(16);
    let n = data.frames();
    let frames = n / hop;
    let mut energy = Vec::with_capacity(frames);
    let mut prev = 0.0f32;
    for f in 0..frames {
        let mut e = 0.0f32;
        let mut hf = 0.0f32;
        for i in f * hop..(f + 1) * hop {
            let x = 0.5 * (data.left[i] + data.right[i]);
            let d = x - prev;
            prev = x;
            e += x * x;
            hf += d * d;
        }
        // High frequencies mark attacks better than plain energy.
        energy.push((e + 4.0 * hf) / hop as f32);
    }
    let floor = 1e-7f32;
    let mut onset = vec![0.0f32; frames];
    for f in 0..frames {
        let lo = f.saturating_sub(4);
        // Before the file starts there is silence.
        let past = energy[lo..f].iter().sum::<f32>() / 4.0;
        let rise = (energy[f] + floor).ln() - (past + floor).ln();
        // Ignore rises out of near silence that stay quiet.
        if energy[f] > 1e-5 {
            onset[f] = rise.max(0.0);
        }
    }
    (onset, hop)
}

/// Finds transient positions (seconds).
pub fn detect_transients(data: &AudioData) -> Vec<f64> {
    let (onset, hop) = onset_strength(data);
    let sr = data.sample_rate as f64;
    let mut out: Vec<f64> = Vec::new();
    let n = onset.len();
    for f in 0..n {
        let o = onset[f];
        if o < 1.0 {
            continue;
        }
        let lo = f.saturating_sub(3);
        let hi = (f + 4).min(n);
        if onset[lo..hi].iter().any(|&x| x > o) {
            continue;
        }
        let window = f.saturating_sub(40)..(f + 40).min(n);
        let mean = onset[window.clone()].iter().sum::<f32>() / window.len() as f32;
        if o < mean + 0.6 {
            continue;
        }
        // Refine to the first sample that reaches 30% of the attack's peak.
        let start = (f.saturating_sub(1)) * hop;
        let end = ((f + 2) * hop).min(data.frames());
        let peak = (start..end)
            .map(|i| data.left[i].abs().max(data.right[i].abs()))
            .fold(0.0f32, f32::max);
        let at = (start..end)
            .find(|&i| data.left[i].abs().max(data.right[i].abs()) >= peak * 0.3)
            .unwrap_or(f * hop);
        let t = (at as f64 / sr - 0.0005).max(0.0);
        if out.last().is_none_or(|&last| t - last >= MIN_SPACING) {
            out.push(t);
        }
    }
    out
}

/// Estimates the tempo in BPM (folded into 80..160), or `None` when the audio
/// has no clear pulse.
pub fn estimate_tempo(data: &AudioData) -> Option<f64> {
    let (onset, _) = onset_strength(data);
    let fps = 1.0 / HOP_SECS;
    let min_lag = (fps * 60.0 / 200.0) as usize;
    let max_lag = (fps * 60.0 / 50.0) as usize;
    if onset.len() < max_lag * 2 {
        return None;
    }
    let mean = onset.iter().sum::<f32>() / onset.len() as f32;
    let centred: Vec<f32> = onset.iter().map(|o| o - mean).collect();
    let ac = |lag: usize| -> f32 {
        centred[..centred.len() - lag]
            .iter()
            .zip(&centred[lag..])
            .map(|(a, b)| a * b)
            .sum::<f32>()
            / (centred.len() - lag) as f32
    };
    let zero = ac(0);
    if zero <= 1e-9 {
        return None;
    }
    let scores: Vec<f32> = (min_lag..=max_lag).map(ac).collect();
    let mut best = 0;
    let mut best_score = f32::NEG_INFINITY;
    for (i, &s) in scores.iter().enumerate() {
        let bpm = 60.0 * fps / (min_lag + i) as f64;
        // Prefer common tempos.
        let w = (-0.5 * ((bpm / 120.0).log2() / 1.0).powi(2)).exp() as f32;
        if s * w > best_score {
            best_score = s * w;
            best = i;
        }
    }
    if scores[best] < zero * 0.1 {
        return None;
    }
    // Parabolic refinement of the peak.
    let mut lag = (min_lag + best) as f64;
    if best > 0 && best + 1 < scores.len() {
        let (a, b, c) = (scores[best - 1], scores[best], scores[best + 1]);
        let d = a - 2.0 * b + c;
        if d.abs() > 1e-12 {
            lag += (0.5 * (a - c) / d) as f64;
        }
    }
    let mut bpm = 60.0 * fps / lag;
    while bpm < 80.0 {
        bpm *= 2.0;
    }
    while bpm >= 160.0 {
        bpm /= 2.0;
    }
    Some(bpm)
}

/// The result of auto-warping a file.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoWarp {
    pub warp: WarpSettings,
    /// Suggested clip length in beats.
    pub length: f64,
}

/// Analyses a file and returns warp settings that lock it to the beat grid,
/// in the spirit of Ableton's auto-warp: transients, an estimated tempo
/// (snapped so loops come out as whole bars), and a first marker on the
/// downbeat. Files without a clear pulse keep their speed at `fallback_bpm`.
pub fn auto_warp(data: &AudioData, fallback_bpm: f64, beats_per_bar: u32) -> AutoWarp {
    let transients = detect_transients(data);
    let dur = data.duration_secs();
    let bpb = beats_per_bar.max(1) as f64;
    let tempo = estimate_tempo(data);
    let mut bpm = tempo.unwrap_or(fallback_bpm);
    let mut snapped = false;
    let starts_on_a_hit = transients.first().is_some_and(|&t| t <= 0.05);
    if tempo.is_some() && starts_on_a_hit {
        // Loops start right on a hit: snap the tempo so the file is a whole
        // number of bars (or beats).
        let beats = dur * bpm / 60.0;
        let bars = (beats / bpb).round();
        if bars >= 1.0 && ((beats - bars * bpb) / beats).abs() < 0.04 {
            bpm = bars * bpb * 60.0 / dur;
            snapped = true;
        } else if beats.round() >= 1.0 && ((beats - beats.round()) / beats).abs() < 0.03 {
            bpm = beats.round() * 60.0 / dur;
            snapped = true;
        }
    }
    // Loops start at the file start; other material at the first transient.
    let first = match transients.first() {
        Some(&t) if !snapped && t > 0.05 => t,
        _ => 0.0,
    };
    let percussive = tempo.is_some() && transients.len() as f64 >= 0.75 * (dur * bpm / 60.0);
    let warp = WarpSettings {
        enabled: true,
        mode: if percussive { WarpMode::Beats } else { WarpMode::Complex },
        markers: vec![WarpMarker { beat: 0.0, secs: first }],
        bpm,
        transpose: 0.0,
        transients,
    };
    let end = warp.secs_to_beat(dur);
    let length = if snapped {
        end.round()
    } else {
        (end * 4.0).floor() / 4.0
    }
    .max(0.25);
    AutoWarp { warp, length }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drum-like loop: decaying noise bursts on every beat at `bpm`.
    fn click_loop(bpm: f64, beats: usize, sr: u32, offset: f64) -> AudioData {
        let spb = 60.0 / bpm;
        let n = ((beats as f64 * spb + offset) * sr as f64) as usize;
        let mut v = vec![0.0f32; n];
        let mut seed = 12345u32;
        for b in 0..beats {
            let start = ((offset + b as f64 * spb) * sr as f64) as usize;
            let amp = if b % 4 == 0 { 0.9 } else { 0.6 };
            for i in 0..(0.08 * sr as f64) as usize {
                if start + i >= n {
                    break;
                }
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let noise = seed as f32 / u32::MAX as f32 * 2.0 - 1.0;
                let env = (-(i as f32) / (0.015 * sr as f32)).exp();
                v[start + i] = noise * env * amp;
            }
        }
        AudioData {
            sample_rate: sr,
            left: v.clone(),
            right: v,
        }
    }

    #[test]
    fn finds_transients_on_the_beats() {
        let data = click_loop(100.0, 8, 44100, 0.0);
        let t = detect_transients(&data);
        assert_eq!(t.len(), 8, "{t:?}");
        for (i, x) in t.iter().enumerate() {
            assert!((x - i as f64 * 0.6).abs() < 0.006, "transient {i} at {x}");
        }
    }

    #[test]
    fn silence_has_no_transients_or_tempo() {
        let data = AudioData {
            sample_rate: 44100,
            left: vec![0.0; 44100 * 3],
            right: vec![0.0; 44100 * 3],
        };
        assert!(detect_transients(&data).is_empty());
        assert_eq!(estimate_tempo(&data), None);
    }

    #[test]
    fn estimates_loop_tempo() {
        for bpm in [92.0, 120.0, 135.0] {
            let data = click_loop(bpm, 16, 44100, 0.0);
            let est = estimate_tempo(&data).unwrap();
            assert!((est - bpm).abs() < 2.0, "{bpm}: estimated {est}");
        }
    }

    #[test]
    fn auto_warp_locks_a_loop_to_whole_bars() {
        let data = click_loop(97.0, 8, 44100, 0.0);
        let a = auto_warp(&data, 120.0, 4);
        assert!(a.warp.enabled);
        assert!((a.warp.bpm - 97.0).abs() < 0.01, "snapped bpm {}", a.warp.bpm);
        assert_eq!(a.length, 8.0);
        assert_eq!(a.warp.mode, WarpMode::Beats);
        assert_eq!(a.warp.markers[0].secs, 0.0);
    }

    #[test]
    fn auto_warp_puts_the_first_marker_on_the_first_hit() {
        // Not a loop: starts late and has an odd length.
        let mut data = click_loop(110.0, 7, 44100, 0.3);
        let extra = vec![0.0; 44100 / 7];
        data.left.extend_from_slice(&extra);
        data.right.extend_from_slice(&extra);
        let a = auto_warp(&data, 120.0, 4);
        assert!((a.warp.markers[0].secs - 0.3).abs() < 0.01, "{a:?}");
        assert!((a.warp.bpm - 110.0).abs() < 2.0, "bpm {}", a.warp.bpm);
    }
}
