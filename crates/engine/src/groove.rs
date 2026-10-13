//! Groove Pool: timing and accent templates applied to MIDI notes.
//!
//! A [`Groove`] is a short pattern defined on a grid (`base`, in beats). Each
//! grid step carries a timing offset and an accent. Applying a groove to notes
//! first pulls every note part of the way towards the nearest grid point
//! (`quantize`), then moves it by the step's offset (`timing`), adds a little
//! deterministic jitter (`random`) and scales its velocity by the step accent
//! (`velocity`). The global groove amount scales timing and velocity for every
//! groove at once.
//!
//! Grooves come from the factory [`presets`] or are extracted from existing
//! material: [`extract_from_notes`] for MIDI clips and [`extract_from_hits`]
//! for detected audio transients. [`apply`] is non-destructive; [`commit`]
//! bakes the result into the notes.
//!
//! Everything here is pure logic for the UI thread; nothing runs on the audio
//! thread.

use serde::{Deserialize, Serialize};

use crate::model::{Id, Note};

/// Largest number of steps an extracted groove may have.
pub const MAX_EXTRACTED_STEPS: usize = 64;
/// Upper limit of [`Groove::timing`] and the global groove amount (130 %).
pub const MAX_AMOUNT: f32 = 1.3;

/// One grid step of a groove.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GrooveStep {
    /// Timing offset from the grid point, in beats (may be negative).
    pub offset: f64,
    /// Accent 0..=1 (1 = loudest hit in the groove).
    pub velocity: f32,
}

impl Default for GrooveStep {
    fn default() -> Self {
        GrooveStep {
            offset: 0.0,
            velocity: 1.0,
        }
    }
}

/// A groove template from the Groove Pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Groove {
    pub id: Id,
    pub name: String,
    /// Grid the groove is defined on, in beats (0.25 = 1/16, 0.5 = 1/8, 1/3 for triplet 8ths etc.).
    pub base: f64,
    /// One entry per grid step; the pattern repeats every `steps.len() * base` beats.
    pub steps: Vec<GrooveStep>,
    /// 0..=1: how far notes are first pulled to the base grid before the groove offsets apply.
    pub quantize: f32,
    /// 0..=1.3 (130%): how much of the timing offsets is applied.
    pub timing: f32,
    /// 0..=1: random timing jitter, at most ±base/4 at 1.0. Deterministic (seeded), see [`apply`].
    pub random: f32,
    /// -1..=1: how strongly the step accents change velocities (negative inverts).
    pub velocity: f32,
}

impl Default for Groove {
    fn default() -> Self {
        Groove {
            id: 0,
            name: "Straight 16ths".to_string(),
            base: 0.25,
            steps: vec![GrooveStep::default(); 16],
            quantize: 0.0,
            timing: 1.0,
            random: 0.0,
            velocity: 0.0,
        }
    }
}

impl Groove {
    /// Creates a groove with the default parameters (quantize 0, timing 1, random 0, velocity 0).
    pub fn new(name: impl Into<String>, base: f64, steps: Vec<GrooveStep>) -> Self {
        Groove {
            name: name.into(),
            base,
            steps,
            ..Groove::default()
        }
    }

    /// Length of one groove cycle in beats (0 if the groove has no steps).
    pub fn cycle_length(&self) -> f64 {
        self.steps.len() as f64 * self.base
    }

    /// Mean accent over all steps (1 for an empty groove).
    pub fn mean_accent(&self) -> f32 {
        if self.steps.is_empty() {
            return 1.0;
        }
        self.steps.iter().map(|s| s.velocity).sum::<f32>() / self.steps.len() as f32
    }

    /// The step for grid index `index` (a multiple of `base`), wrapping around the cycle.
    pub fn step_at(&self, index: i64) -> Option<&GrooveStep> {
        if self.steps.is_empty() {
            return None;
        }
        let i = index.rem_euclid(self.steps.len() as i64) as usize;
        self.steps.get(i)
    }

    /// Whether the grid is usable (finite and positive).
    pub fn is_valid(&self) -> bool {
        self.base.is_finite() && self.base > 0.0
    }
}

/// Builds a swing groove: pairs of steps on `base`, the second step of each pair delayed by
/// `(percent - 0.5) * 2 * base` and accented with `off_accent`.
pub fn swing(name: &str, base: f64, percent: f64, off_accent: f32) -> Groove {
    let offset = (percent - 0.5) * 2.0 * base;
    let steps = vec![
        GrooveStep {
            offset: 0.0,
            velocity: 1.0,
        },
        GrooveStep {
            offset,
            velocity: off_accent,
        },
    ];
    Groove::new(name, base, steps)
}

/// Factory grooves (id = 0; the caller assigns ids).
pub fn presets() -> Vec<Groove> {
    let mut out = Vec::new();

    for p in [55u32, 62, 71] {
        out.push(swing(&format!("Swing 8ths {p}%"), 0.5, p as f64 / 100.0, 0.9));
    }
    for p in [54u32, 58, 62, 66] {
        out.push(swing(&format!("Swing 16ths {p}%"), 0.25, p as f64 / 100.0, 0.85));
    }

    // Triplet feel: the second 16th of every 8th lands on the last triplet.
    out.push(swing("Shuffle 16ths", 0.25, 2.0 / 3.0, 0.8));

    // Off-beat 16ths slightly early and a touch quieter.
    let push = (0..16)
        .map(|i| match i % 4 {
            0 => GrooveStep {
                offset: 0.0,
                velocity: 1.0,
            },
            2 => GrooveStep {
                offset: -0.01,
                velocity: 0.85,
            },
            _ => GrooveStep {
                offset: -0.02,
                velocity: 0.75,
            },
        })
        .collect();
    out.push(Groove::new("Push 16ths", 0.25, push));

    // Laid-back 8ths: off-beats late and noticeably quieter.
    let lazy = vec![
        GrooveStep {
            offset: 0.0,
            velocity: 1.0,
        },
        GrooveStep {
            offset: 0.04,
            velocity: 0.7,
        },
        GrooveStep {
            offset: 0.01,
            velocity: 0.9,
        },
        GrooveStep {
            offset: 0.05,
            velocity: 0.65,
        },
    ];
    out.push(Groove::new("Lazy 8ths", 0.5, lazy));

    // Small, irregular offsets and accents over one bar of 16ths, like a human player.
    const HUMAN: [(f64, f32); 16] = [
        (0.000, 1.00),
        (0.008, 0.72),
        (-0.006, 0.84),
        (0.012, 0.68),
        (-0.004, 0.94),
        (0.010, 0.70),
        (0.003, 0.80),
        (-0.009, 0.66),
        (0.005, 0.97),
        (-0.007, 0.74),
        (0.011, 0.86),
        (0.002, 0.69),
        (-0.010, 0.91),
        (0.006, 0.73),
        (-0.003, 0.82),
        (0.009, 0.71),
    ];
    let human = HUMAN
        .iter()
        .map(|&(offset, velocity)| GrooveStep { offset, velocity })
        .collect();
    let mut humanize = Groove::new("Humanize", 0.25, human);
    humanize.random = 0.1;
    out.push(humanize);

    // No timing change, accents on beats and 8th off-beats.
    let accent = (0..16)
        .map(|i| GrooveStep {
            offset: 0.0,
            velocity: match i % 4 {
                0 => 1.0,
                2 => 0.75,
                _ => 0.55,
            },
        })
        .collect();
    let mut straight_accent = Groove::new("Straight Accent 16ths", 0.25, accent);
    straight_accent.velocity = 0.5;
    out.push(straight_accent);

    out
}

/// Extracts a groove from MIDI notes.
///
/// For each base-grid step within one cycle (`clip_length` rounded up to a whole number of steps,
/// capped at 64 steps), averages the offset of the notes nearest that grid point (only notes within
/// ±base/2) and their normalised velocity (loudest = 1). Steps without notes get offset 0 and the
/// average velocity.
pub fn extract_from_notes(name: &str, notes: &[Note], clip_length: f64, base: f64) -> Groove {
    let hits: Vec<(f64, f32)> = notes.iter().map(|n| (n.start, n.velocity as f32)).collect();
    extract(name, &hits, clip_length, base)
}

/// Extracts a groove from detected audio hits: `hits` are (beat position, strength 0..=1) pairs,
/// e.g. transients of a warped audio loop converted to beats. Works like [`extract_from_notes`].
pub fn extract_from_hits(name: &str, hits: &[(f64, f32)], length: f64, base: f64) -> Groove {
    extract(name, hits, length, base)
}

fn extract(name: &str, hits: &[(f64, f32)], length: f64, base: f64) -> Groove {
    let mut groove = Groove {
        name: name.to_string(),
        ..Groove::default()
    };
    if !(base.is_finite() && base > 0.0) {
        groove.steps.clear();
        return groove;
    }
    groove.base = base;

    let raw_steps = if length.is_finite() && length > 0.0 {
        // Tolerate tiny float error so e.g. 4.0000000001 / 0.25 stays 16 steps.
        (length / base - 1e-9).ceil()
    } else {
        1.0
    };
    let n = (raw_steps.max(1.0) as usize).min(MAX_EXTRACTED_STEPS);

    let valid: Vec<(f64, f32)> = hits
        .iter()
        .copied()
        .filter(|(pos, strength)| pos.is_finite() && strength.is_finite() && *strength >= 0.0)
        .collect();
    let loudest = valid.iter().map(|&(_, s)| s).fold(0.0f32, f32::max);
    let norm = |s: f32| if loudest > 0.0 { s / loudest } else { 1.0 };

    let mut offset_sum = vec![0.0f64; n];
    let mut vel_sum = vec![0.0f32; n];
    let mut count = vec![0usize; n];
    let mut total_vel = 0.0f32;
    let mut total = 0usize;
    for &(pos, strength) in &valid {
        let k = (pos / base).round();
        let offset = pos - k * base;
        if offset.abs() > base / 2.0 + 1e-12 {
            continue;
        }
        let i = (k as i64).rem_euclid(n as i64) as usize;
        let v = norm(strength);
        offset_sum[i] += offset;
        vel_sum[i] += v;
        count[i] += 1;
        total_vel += v;
        total += 1;
    }
    let mean_vel = if total > 0 { total_vel / total as f32 } else { 1.0 };

    groove.steps = (0..n)
        .map(|i| {
            if count[i] > 0 {
                GrooveStep {
                    offset: offset_sum[i] / count[i] as f64,
                    velocity: (vel_sum[i] / count[i] as f32).clamp(0.0, 1.0),
                }
            } else {
                GrooveStep {
                    offset: 0.0,
                    velocity: mean_vel.clamp(0.0, 1.0),
                }
            }
        })
        .collect();
    groove
}

/// Applies a groove to notes non-destructively and returns the new notes (same order as input).
///
/// `amount` is the global groove amount 0..=1.3 that scales timing and velocity. For each note the
/// nearest grid point `g` (a multiple of `base`) picks the step `(g / base) mod steps.len()`; the
/// start is pulled towards `g` by `quantize`, moved by `step.offset * timing * amount`, then
/// jittered by up to `±random * base / 4` (deterministic, derived from `seed`, the note index and
/// pitch). Velocity becomes `v * (1 + velocity * amount * (step.velocity - mean_accent))`, clamped
/// to 1..=127. Starts never go below 0 and lengths are unchanged.
pub fn apply(groove: &Groove, notes: &[Note], amount: f32, seed: u64) -> Vec<Note> {
    if !groove.is_valid() {
        return notes.to_vec();
    }
    let base = groove.base;
    let amount = clamp_param(amount, 0.0, MAX_AMOUNT) as f64;
    let quantize = clamp_param(groove.quantize, 0.0, 1.0) as f64;
    let timing = clamp_param(groove.timing, 0.0, MAX_AMOUNT) as f64;
    let random = clamp_param(groove.random, 0.0, 1.0) as f64;
    let vel_amount = clamp_param(groove.velocity, -1.0, 1.0) as f64;
    let mean = groove.mean_accent() as f64;

    notes
        .iter()
        .enumerate()
        .map(|(index, note)| {
            if !note.start.is_finite() {
                return *note;
            }
            let k = (note.start / base).round();
            let g = k * base;
            let step = groove.step_at(k as i64).copied().unwrap_or(GrooveStep {
                offset: 0.0,
                velocity: mean as f32,
            });

            let mut start = note.start + (g - note.start) * quantize;
            if step.offset.is_finite() {
                start += step.offset * timing * amount;
            }
            if random > 0.0 {
                start += jitter(seed, index as u64, note.pitch) * random * base / 4.0;
            }
            let start = start.max(0.0);

            let accent = if step.velocity.is_finite() {
                step.velocity as f64
            } else {
                mean
            };
            let factor = 1.0 + vel_amount * amount * (accent - mean);
            let velocity = (note.velocity as f64 * factor).round().clamp(1.0, 127.0) as u8;

            Note {
                start,
                velocity,
                ..*note
            }
        })
        .collect()
}

/// Bakes the groove in ("Commit"): identical to [`apply`], provided for clarity at call sites
/// that replace the clip's notes with the result.
pub fn commit(groove: &Groove, notes: &[Note], amount: f32, seed: u64) -> Vec<Note> {
    apply(groove, notes, amount, seed)
}

fn clamp_param(v: f32, lo: f32, hi: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        lo.max(0.0).min(hi)
    }
}

/// Deterministic jitter in -1..=1 from a SplitMix64-style hash of seed, note index and pitch.
fn jitter(seed: u64, index: u64, pitch: u8) -> f64 {
    let mut x = seed ^ index.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (pitch as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    // Top 53 bits -> [0, 1], then -> [-1, 1].
    let unit = (x >> 11) as f64 / ((1u64 << 53) - 1) as f64;
    unit * 2.0 - 1.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: f64, velocity: u8) -> Note {
        Note {
            pitch: 60,
            velocity,
            start,
            length: 0.25,
        }
    }

    fn eighths(count: usize) -> Vec<Note> {
        (0..count).map(|i| note(i as f64 * 0.5, 100)).collect()
    }

    fn preset(name: &str) -> Groove {
        presets().into_iter().find(|g| g.name == name).expect("preset exists")
    }

    #[test]
    fn swing_preset_delays_every_second_eighth() {
        let g = preset("Swing 8ths 62%");
        let out = apply(&g, &eighths(8), 1.0, 0);
        let expected = (0.62 - 0.5) * 2.0 * 0.5;
        for (i, n) in out.iter().enumerate() {
            let grid = i as f64 * 0.5;
            let want = if i % 2 == 1 { grid + expected } else { grid };
            assert!((n.start - want).abs() < 1e-9, "note {i}: {} vs {want}", n.start);
            assert_eq!(n.length, 0.25);
        }
        // Timing and amount scale the offset.
        let half = apply(&g, &eighths(2), 0.5, 0);
        assert!((half[1].start - (0.5 + expected * 0.5)).abs() < 1e-9);
    }

    #[test]
    fn zero_amount_is_identity_for_on_grid_notes() {
        let mut g = preset("Swing 16ths 66%");
        g.quantize = 1.0;
        g.velocity = 1.0;
        let notes: Vec<Note> = (0..16).map(|i| note(i as f64 * 0.25, 90)).collect();
        assert_eq!(apply(&g, &notes, 0.0, 7), notes);
    }

    #[test]
    fn full_quantize_with_empty_offsets_snaps_to_grid() {
        let mut g = Groove {
            quantize: 1.0,
            ..Groove::default()
        };
        let notes = vec![note(0.07, 80), note(0.49, 80), note(1.13, 80), note(2.9, 80)];
        let out = apply(&g, &notes, 1.0, 0);
        let want = [0.0, 0.5, 1.25, 3.0];
        for (n, w) in out.iter().zip(want) {
            assert!((n.start - w).abs() < 1e-9, "{} vs {w}", n.start);
        }
        // Partial quantize moves part of the way.
        g.quantize = 0.5;
        let out = apply(&g, &[note(0.1, 80)], 1.0, 0);
        assert!((out[0].start - 0.05).abs() < 1e-9);
    }

    #[test]
    fn extract_from_notes_round_trips_swing() {
        let swung = preset("Swing 8ths 71%");
        let pattern = apply(&swung, &eighths(8), 1.0, 0);
        let g = extract_from_notes("Extracted", &pattern, 4.0, 0.5);
        assert_eq!(g.steps.len(), 8);
        assert_eq!(g.base, 0.5);
        for (i, s) in g.steps.iter().enumerate() {
            let want = swung.steps[i % 2].offset;
            assert!((s.offset - want).abs() < 1e-6, "step {i}: {} vs {want}", s.offset);
        }
        // Re-applying the extracted groove to straight notes reproduces the pattern.
        let again = apply(&g, &eighths(8), 1.0, 0);
        for (a, b) in again.iter().zip(&pattern) {
            assert!((a.start - b.start).abs() < 1e-6);
        }
    }

    #[test]
    fn extract_normalises_velocity_and_fills_gaps() {
        let notes = vec![note(0.0, 120), note(1.0, 60)];
        let g = extract_from_notes("V", &notes, 2.0, 0.5);
        assert_eq!(g.steps.len(), 4);
        assert!((g.steps[0].velocity - 1.0).abs() < 1e-6);
        assert!((g.steps[2].velocity - 0.5).abs() < 1e-6);
        assert!((g.steps[1].velocity - 0.75).abs() < 1e-6);
        assert_eq!(g.steps[1].offset, 0.0);
        // Cap at 64 steps.
        let long = extract_from_notes("L", &notes, 100.0, 0.25);
        assert_eq!(long.steps.len(), MAX_EXTRACTED_STEPS);
    }

    #[test]
    fn extract_from_hits_matches_notes() {
        let hits = vec![(0.0, 1.0), (0.27, 0.5), (0.5, 0.8), (0.79, 0.4)];
        let g = extract_from_hits("Loop", &hits, 1.0, 0.25);
        assert_eq!(g.steps.len(), 4);
        assert!((g.steps[1].offset - 0.02).abs() < 1e-9);
        assert!((g.steps[3].offset - 0.04).abs() < 1e-9);
        assert!((g.steps[1].velocity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn velocity_accents_change_and_clamp() {
        let mut g = preset("Straight Accent 16ths");
        g.velocity = 1.0;
        let notes = vec![note(0.0, 100), note(0.25, 100)];
        let out = apply(&g, &notes, 1.0, 0);
        assert!(out[0].velocity > 100, "beat louder: {}", out[0].velocity);
        assert!(out[1].velocity < 100, "off-beat quieter: {}", out[1].velocity);
        // Timing untouched by this groove.
        assert_eq!(out[1].start, 0.25);

        // Negative velocity inverts.
        g.velocity = -1.0;
        let inv = apply(&g, &notes, 1.0, 0);
        assert!(inv[0].velocity < 100 && inv[1].velocity > 100);

        // Clamping to 1..=127.
        let mut extreme = Groove::new(
            "Extreme",
            0.25,
            vec![
                GrooveStep {
                    offset: 0.0,
                    velocity: 1.0,
                },
                GrooveStep {
                    offset: 0.0,
                    velocity: -20.0,
                },
            ],
        );
        extreme.velocity = 1.0;
        let out = apply(&extreme, &[note(0.0, 127), note(0.25, 5)], 1.3, 0);
        assert_eq!(out[0].velocity, 127);
        assert_eq!(out[1].velocity, 1);
    }

    #[test]
    fn random_jitter_is_deterministic_and_bounded() {
        let g = Groove {
            random: 1.0,
            ..Groove::default()
        };
        let notes: Vec<Note> = (0..64).map(|i| note(1.0 + i as f64 * 0.25, 100)).collect();
        let a = apply(&g, &notes, 1.0, 42);
        let b = apply(&g, &notes, 1.0, 42);
        let c = apply(&g, &notes, 1.0, 43);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let bound = g.base / 4.0 + 1e-12;
        let mut moved = 0;
        for (n, o) in a.iter().zip(&notes) {
            let d = (n.start - o.start).abs();
            assert!(d <= bound, "jitter {d} exceeds {bound}");
            if d > 1e-6 {
                moved += 1;
            }
        }
        assert!(moved > 32);
    }

    #[test]
    fn never_produces_negative_starts() {
        let mut g = Groove::new(
            "Early",
            0.25,
            vec![GrooveStep {
                offset: -0.2,
                velocity: 1.0,
            }],
        );
        g.random = 1.0;
        g.timing = 1.3;
        let notes: Vec<Note> = (0..8).map(|i| note(i as f64 * 0.01, 100)).collect();
        for seed in 0..20 {
            for n in apply(&g, &notes, 1.3, seed) {
                assert!(n.start >= 0.0);
            }
        }
        for g in presets() {
            for n in apply(&g, &[note(0.0, 100), note(0.25, 100)], 1.3, 3) {
                assert!(n.start >= 0.0, "{}", g.name);
            }
        }
    }

    #[test]
    fn presets_are_well_formed() {
        let all = presets();
        assert!(all.len() >= 12);
        for g in &all {
            assert!(!g.steps.is_empty(), "{}", g.name);
            assert!(g.base > 0.0, "{}", g.name);
            assert_eq!(g.id, 0);
            for s in &g.steps {
                assert!((0.0..=1.0).contains(&s.velocity));
                assert!(s.offset.abs() < g.base);
            }
        }
        for name in [
            "Swing 8ths 55%",
            "Swing 8ths 62%",
            "Swing 8ths 71%",
            "Swing 16ths 54%",
            "Swing 16ths 58%",
            "Swing 16ths 62%",
            "Swing 16ths 66%",
            "Shuffle 16ths",
            "Push 16ths",
            "Lazy 8ths",
            "Humanize",
            "Straight Accent 16ths",
        ] {
            assert!(all.iter().any(|g| g.name == name), "missing {name}");
        }
    }

    #[test]
    fn invalid_groove_and_serde_defaults() {
        let g = Groove {
            base: 0.0,
            ..Groove::default()
        };
        let notes = vec![note(0.3, 90)];
        assert_eq!(apply(&g, &notes, 1.0, 0), notes);
        let empty = Groove::new("Empty", 0.25, Vec::new());
        assert_eq!(apply(&empty, &notes, 1.0, 0), notes);

        let parsed: Groove = serde_json::from_str(r#"{"name":"Partial"}"#).expect("parses");
        assert_eq!(parsed.name, "Partial");
        assert_eq!(parsed.base, 0.25);
        assert_eq!(parsed.steps.len(), 16);
    }
}
