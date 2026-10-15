//! Tuplets, polyrhythms, polymeter and metric modulation.
//!
//! Note positions are continuous beat values (`f64`), and the engine places
//! each note on its own sample inside a block, so tuplets need no special
//! clock. What this module adds is exact grid arithmetic: a grid point is
//! computed as `(k · num · base) / den` from whole numbers, so the 7th
//! quintuplet lands on the correctly rounded value of 7/5 instead of the
//! sum of seven rounded 0.2 steps. Within a few thousand bars the error
//! stays far below one sample.

use serde::{Deserialize, Serialize};

use crate::model::Note;

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 {
        a.max(1)
    } else {
        gcd(b, a % b)
    }
}

/// `n` notes in the time of `m` of the same value: 3:2 is a triplet, 5:4 a
/// quintuplet, 2:3 a duplet (dotted feel). 1:1 is a straight grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tuplet {
    pub n: u32,
    pub m: u32,
}

impl Default for Tuplet {
    fn default() -> Self {
        Self::STRAIGHT
    }
}

impl Tuplet {
    pub const STRAIGHT: Tuplet = Tuplet { n: 1, m: 1 };
    /// Largest `n` or `m` offered for custom tuplets.
    pub const MAX: u32 = 32;

    pub const fn new(n: u32, m: u32) -> Self {
        Self { n, m }
    }

    /// Tuplets offered in menus, most common first.
    pub const COMMON: [Tuplet; 14] = [
        Tuplet::new(3, 2),
        Tuplet::new(2, 3),
        Tuplet::new(5, 4),
        Tuplet::new(4, 5),
        Tuplet::new(6, 4),
        Tuplet::new(7, 4),
        Tuplet::new(4, 3),
        Tuplet::new(3, 4),
        Tuplet::new(5, 3),
        Tuplet::new(5, 6),
        Tuplet::new(7, 8),
        Tuplet::new(9, 8),
        Tuplet::new(11, 8),
        Tuplet::new(13, 8),
    ];

    /// Clamps to a playable range; n == m becomes straight.
    pub fn sanitized(self) -> Self {
        let n = self.n.clamp(1, Self::MAX);
        let m = self.m.clamp(1, Self::MAX);
        if n == m {
            Self::STRAIGHT
        } else {
            Self { n, m }
        }
    }

    pub fn is_straight(self) -> bool {
        self.n == self.m
    }

    /// The step multiplier m/n as a reduced fraction.
    pub fn factor(self) -> (u64, u64) {
        let t = self.sanitized();
        let (num, den) = (t.m as u64, t.n as u64);
        let g = gcd(num, den);
        (num / g, den / g)
    }

    /// "5:4", or "Straight".
    pub fn label(self) -> String {
        if self.is_straight() {
            "Straight".into()
        } else {
            format!("{}:{}", self.n, self.m)
        }
    }

    /// Musical name of the common ones ("Quintuplet").
    pub fn name(self) -> Option<&'static str> {
        Some(match (self.n, self.m) {
            (3, 2) => "Triplet",
            (2, 3) => "Duplet (dotted)",
            (4, 3) => "Quadruplet",
            (3, 4) => "3 over 4",
            (4, 5) => "4 over 5",
            (5, 4) => "Quintuplet",
            (5, 3) => "5 over 3",
            (5, 6) => "5 over 6",
            (6, 4) => "Sextuplet",
            (7, 4) => "Septuplet",
            (7, 8) => "7 over 8",
            (9, 8) => "Nonuplet",
            (11, 8) => "11 over 8",
            (13, 8) => "13 over 8",
            _ => return None,
        })
    }

    /// "5:4 Quintuplet".
    pub fn long_label(self) -> String {
        match self.name() {
            Some(n) if !self.is_straight() => format!("{} {n}", self.label()),
            _ => self.label(),
        }
    }
}

/// An evenly spaced grid: a note value (`base`, in beats) divided by a tuplet
/// and, optionally, by a second tuplet nested inside it (quintuplets inside
/// triplets).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TupletGrid {
    pub base: f64,
    pub outer: Tuplet,
    pub inner: Tuplet,
}

impl TupletGrid {
    pub fn straight(base: f64) -> Self {
        Self::new(base, Tuplet::STRAIGHT, Tuplet::STRAIGHT)
    }

    pub fn new(base: f64, outer: Tuplet, inner: Tuplet) -> Self {
        Self {
            base: if base.is_finite() && base > 0.0 { base } else { 0.25 },
            outer: outer.sanitized(),
            inner: inner.sanitized(),
        }
    }

    /// The combined step multiplier as a reduced fraction (num, den).
    pub fn factor(&self) -> (u64, u64) {
        let (a, b) = self.outer.factor();
        let (c, d) = self.inner.factor();
        let (num, den) = (a * c, b * d);
        let g = gcd(num, den);
        (num / g, den / g)
    }

    pub fn is_straight(&self) -> bool {
        self.factor() == (1, 1)
    }

    /// Distance between grid points, in beats.
    pub fn step(&self) -> f64 {
        let (num, den) = self.factor();
        self.base * num as f64 / den as f64
    }

    /// Grid point `k`, computed in one rounding from whole numbers.
    pub fn point(&self, k: i64) -> f64 {
        let (num, den) = self.factor();
        (k as f64 * num as f64) * self.base / den as f64
    }

    /// Index of the grid point nearest `beat`.
    pub fn index(&self, beat: f64) -> i64 {
        (beat / self.step()).round() as i64
    }

    pub fn nearest(&self, beat: f64) -> f64 {
        self.point(self.index(beat))
    }

    /// The grid point at or before `beat`.
    pub fn floor(&self, beat: f64) -> f64 {
        self.point((beat / self.step() + 1e-9).floor() as i64)
    }

    /// How long until the tuplet grid lines up with the straight grid again
    /// (one tuplet group), in beats.
    pub fn group(&self) -> f64 {
        let (num, _) = self.factor();
        num as f64 * self.base
    }
}

/// Bar length in song beats for a clip meter `num/den` when the song's beat
/// is a `1/beat_unit` note: 7/8 in 4/4 is 3.5 beats.
pub fn bar_beats(num: u32, den: u32, beat_unit: u32) -> f64 {
    num.max(1) as f64 * beat_unit.max(1) as f64 / den.max(1) as f64
}

/// Clip meters offered in menus (polymeter against the song).
pub const METERS: [(u32, u32); 18] = [
    (2, 4),
    (3, 4),
    (4, 4),
    (5, 4),
    (6, 4),
    (7, 4),
    (3, 8),
    (5, 8),
    (6, 8),
    (7, 8),
    (9, 8),
    (11, 8),
    (13, 8),
    (5, 16),
    (7, 16),
    (13, 16),
    (15, 16),
    (17, 16),
];

/// Metric modulation: the tempo at which a tuplet note becomes the new
/// straight note of the same value. 3:2 at 100 BPM gives 150 BPM.
pub fn modulated_tempo(tempo: f64, t: Tuplet) -> f64 {
    let t = t.sanitized();
    tempo * t.n as f64 / t.m as f64
}

/// What a polyrhythm layer's span is measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpanUnit {
    Sixteenth,
    Eighth,
    Beat,
    Bar,
}

impl SpanUnit {
    pub const ALL: [SpanUnit; 4] = [SpanUnit::Sixteenth, SpanUnit::Eighth, SpanUnit::Beat, SpanUnit::Bar];

    pub fn label(self, plural: bool) -> &'static str {
        match (self, plural) {
            (SpanUnit::Sixteenth, false) => "sixteenth",
            (SpanUnit::Sixteenth, true) => "sixteenths",
            (SpanUnit::Eighth, false) => "eighth",
            (SpanUnit::Eighth, true) => "eighths",
            (SpanUnit::Beat, false) => "beat",
            (SpanUnit::Beat, true) => "beats",
            (SpanUnit::Bar, false) => "bar",
            (SpanUnit::Bar, true) => "bars",
        }
    }

    /// Length in beats.
    pub fn beats(self, beats_per_bar: f64) -> f64 {
        match self {
            SpanUnit::Sixteenth => 0.25,
            SpanUnit::Eighth => 0.5,
            SpanUnit::Beat => 1.0,
            SpanUnit::Bar => beats_per_bar.max(1.0),
        }
    }
}

/// One voice of a stacked polyrhythm: `pulses` evenly spaced hits across
/// `across` units, repeated through the clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PolyLayer {
    pub on: bool,
    pub pitch: u8,
    pub velocity: u8,
    pub pulses: u32,
    pub across: u32,
    pub unit: SpanUnit,
    /// Splits every pulse into this many equal hits (nested tuplets: 5
    /// inside each of 3). Hits between pulses play softer, like ghost notes.
    pub subdivide: u32,
    /// Euclidean fill: only this many of the hits play, spread as evenly as
    /// possible (0 plays them all).
    pub hits: u32,
    /// Rotates the Euclidean pattern by this many hits.
    pub rotate: u32,
    /// Accents the first hit of every cycle.
    pub accent: bool,
    /// Note length as a share of the gap between hits.
    pub gate: f64,
}

impl Default for PolyLayer {
    fn default() -> Self {
        Self {
            on: true,
            pitch: 36,
            velocity: 100,
            pulses: 3,
            across: 2,
            unit: SpanUnit::Beat,
            subdivide: 1,
            hits: 0,
            rotate: 0,
            accent: true,
            gate: 0.5,
        }
    }
}

impl PolyLayer {
    fn new(pitch: u8, pulses: u32, across: u32, unit: SpanUnit) -> Self {
        Self {
            pitch,
            pulses,
            across,
            unit,
            ..Self::default()
        }
    }

    fn sub(mut self, k: u32) -> Self {
        self.subdivide = k;
        self
    }

    fn euclid(mut self, hits: u32, rotate: u32) -> Self {
        self.hits = hits;
        self.rotate = rotate;
        self
    }

    fn vel(mut self, v: u8) -> Self {
        self.velocity = v;
        self
    }

    fn plain(mut self) -> Self {
        self.accent = false;
        self
    }

    /// Length of one cycle, in beats.
    pub fn span(&self, beats_per_bar: f64) -> f64 {
        self.across.max(1) as f64 * self.unit.beats(beats_per_bar)
    }

    /// Hits per cycle before the Euclidean fill.
    pub fn steps(&self) -> u32 {
        self.pulses.clamp(1, 64) * self.subdivide.clamp(1, 16)
    }

    /// Whether hit `i` of a cycle plays (Euclidean fill).
    fn plays(&self, i: u32) -> bool {
        let n = self.steps() as u64;
        let k = self.hits as u64;
        if k == 0 || k >= n {
            return true;
        }
        ((i as u64 + self.rotate as u64) * k) % n < k
    }

    /// "3 across 2 beats", "5 × 3 across 1 bar".
    pub fn describe(&self) -> String {
        let sub = if self.subdivide > 1 {
            format!(" × {}", self.subdivide)
        } else {
            String::new()
        };
        format!(
            "{}{} across {} {}",
            self.pulses,
            sub,
            self.across,
            self.unit.label(self.across != 1)
        )
    }
}

/// Generates the notes of `layers` from beat 0 up to `length`. Hit `j` of
/// cycle `c` starts at `(c · steps + j) · span / steps`, computed in one
/// rounding so long cycles never drift.
pub fn generate(layers: &[PolyLayer], length: f64, beats_per_bar: f64) -> Vec<Note> {
    let mut out = Vec::new();
    for l in layers.iter().filter(|l| l.on) {
        let span = l.span(beats_per_bar);
        let steps = l.steps() as u64;
        let sub = l.subdivide.clamp(1, 16) as u64;
        let gap = span / steps as f64;
        let len = (gap * l.gate.clamp(0.05, 1.0)).max(crate::midi::edit::MIN_LENGTH.min(gap));
        let mut k: u64 = 0;
        loop {
            let start = (k as f64 * span) / steps as f64;
            if start >= length - 1e-9 || out.len() >= 8192 {
                break;
            }
            let j = (k % steps) as u32;
            if l.plays(j) {
                let v = l.velocity as f64;
                let vel = if j == 0 && l.accent {
                    (v + 22.0).min(127.0)
                } else if !(j as u64).is_multiple_of(sub) {
                    // Between pulses: ghost notes.
                    v * 0.62
                } else {
                    v
                };
                out.push(Note {
                    pitch: l.pitch,
                    velocity: vel.round().clamp(1.0, 127.0) as u8,
                    start,
                    length: len.min(length - start),
                });
            }
            k += 1;
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    out
}

/// A named stack of layers.
#[derive(Debug, Clone, PartialEq)]
pub struct PolyPreset {
    pub name: &'static str,
    pub about: &'static str,
    pub layers: Vec<PolyLayer>,
}

// Drum notes the built-in Drum Kit plays (General MIDI numbers).
use crate::dsp::drums::{CLAP, CLOSED_HAT as HAT, HIGH_TOM, KICK, LOW_TOM, MID_TOM, OPEN_HAT, RIM, SNARE};

/// The built-in stacked polyrhythms.
pub fn presets() -> Vec<PolyPreset> {
    use SpanUnit::*;
    let backbeat = PolyLayer::new(SNARE, 4, 1, Bar).euclid(2, 1).plain();
    vec![
        PolyPreset {
            name: "3:2 Hemiola",
            about: "Three against two over every two beats, the classic cross-rhythm.",
            layers: vec![PolyLayer::new(KICK, 2, 2, Beat), PolyLayer::new(HAT, 3, 2, Beat)],
        },
        PolyPreset {
            name: "Micro 2:3 under 16ths",
            about: "Dotted-sixteenth pairs (2 in the time of 3) against straight sixteenths, so the kick drifts across the beat and only lines up every three beats.",
            layers: vec![
                PolyLayer::new(RIM, 4, 1, Beat).vel(80).plain(),
                PolyLayer::new(KICK, 2, 3, Sixteenth),
                backbeat.clone(),
            ],
        },
        PolyPreset {
            name: "5:4 Across the Bar",
            about: "Five even hits against four beats: close enough to line up that it sounds drunk rather than at war.",
            layers: vec![
                PolyLayer::new(KICK, 4, 1, Bar),
                PolyLayer::new(HIGH_TOM, 5, 1, Bar),
                PolyLayer::new(HAT, 8, 1, Bar).vel(70).plain(),
            ],
        },
        PolyPreset {
            name: "5:4 Micro (quintuplet 16ths)",
            about: "Quintuplet sixteenths on the rim over straight sixteenths on the hat.",
            layers: vec![
                PolyLayer::new(RIM, 5, 1, Beat).vel(85),
                PolyLayer::new(HAT, 4, 1, Beat).vel(70).plain(),
                PolyLayer::new(KICK, 4, 1, Bar).plain(),
                backbeat.clone(),
            ],
        },
        PolyPreset {
            name: "4:3",
            about: "Four against three over three beats (dotted eighths against quarters).",
            layers: vec![PolyLayer::new(KICK, 3, 3, Beat), PolyLayer::new(RIM, 4, 3, Beat)],
        },
        PolyPreset {
            name: "7:4 Septuplets",
            about: "Seven even hits across each bar against four on the floor.",
            layers: vec![
                PolyLayer::new(KICK, 4, 1, Bar),
                PolyLayer::new(CLAP, 7, 1, Bar),
                backbeat.clone(),
            ],
        },
        PolyPreset {
            name: "9:8 Nonuplets",
            about: "Nine against eight over two beats; the ride slowly slides away from the hats.",
            layers: vec![
                PolyLayer::new(HAT, 8, 2, Beat).vel(70).plain(),
                PolyLayer::new(RIM, 9, 2, Beat),
                PolyLayer::new(KICK, 4, 1, Bar).plain(),
            ],
        },
        PolyPreset {
            name: "11:8",
            about: "Eleven against eight across the bar.",
            layers: vec![PolyLayer::new(HAT, 8, 1, Bar).vel(75), PolyLayer::new(HIGH_TOM, 11, 1, Bar)],
        },
        PolyPreset {
            name: "3:4:5 Stack",
            about: "Three, four and five across the same bar, all meeting on the downbeat.",
            layers: vec![
                PolyLayer::new(KICK, 3, 1, Bar),
                PolyLayer::new(SNARE, 4, 1, Bar),
                PolyLayer::new(RIM, 5, 1, Bar),
            ],
        },
        PolyPreset {
            name: "Prime Stack 2:3:5:7",
            about: "Four layers that only share the downbeat: 2, 3, 5 and 7 across a bar.",
            layers: vec![
                PolyLayer::new(KICK, 2, 1, Bar),
                PolyLayer::new(SNARE, 3, 1, Bar),
                PolyLayer::new(RIM, 5, 1, Bar),
                PolyLayer::new(CLAP, 7, 1, Bar),
            ],
        },
        PolyPreset {
            name: "Cascade 2-3-4-6",
            about: "Every layer is a simple subdivision, but stacked they ripple.",
            layers: vec![
                PolyLayer::new(KICK, 2, 1, Bar),
                PolyLayer::new(LOW_TOM, 3, 1, Bar),
                PolyLayer::new(SNARE, 4, 1, Bar),
                PolyLayer::new(HAT, 6, 1, Bar),
            ],
        },
        PolyPreset {
            name: "Nested 5 in 3",
            about: "Quarter-note triplets, each split into five: quintuplets inside triplets, with ghost notes between the pulses.",
            layers: vec![
                PolyLayer::new(SNARE, 3, 2, Beat).sub(5),
                PolyLayer::new(KICK, 4, 1, Bar).plain(),
            ],
        },
        PolyPreset {
            name: "Nested 3 in 5",
            about: "Five pulses across the bar, each split into a triplet.",
            layers: vec![
                PolyLayer::new(RIM, 5, 1, Bar).sub(3),
                PolyLayer::new(KICK, 4, 1, Bar).plain(),
                backbeat.clone(),
            ],
        },
        PolyPreset {
            name: "Micro 2:3 + 5:4",
            about: "Stacked micro rhythms: dotted-16th kicks, quintuplet hats and a straight backbeat holding the floor.",
            layers: vec![
                PolyLayer::new(HAT, 5, 1, Beat).vel(70),
                PolyLayer::new(KICK, 2, 3, Sixteenth),
                backbeat.clone(),
            ],
        },
        PolyPreset {
            name: "Implied Tempo 3:2",
            about: "Eighth-note triplets accented in pairs, so the accents feel like a new, faster pulse (pair it with Metric Modulation 3:2).",
            layers: vec![
                PolyLayer::new(RIM, 3, 1, Beat).sub(2),
                PolyLayer::new(KICK, 3, 2, Beat).plain(),
            ],
        },
        PolyPreset {
            name: "7/16 over 4/4",
            about: "A seven-sixteenth kick figure that cycles against the bar (polymeter): it comes back to the downbeat every seven bars.",
            layers: vec![
                PolyLayer::new(KICK, 7, 7, Sixteenth).euclid(3, 0),
                PolyLayer::new(HAT, 4, 1, Beat).vel(70).plain(),
                backbeat.clone(),
            ],
        },
        PolyPreset {
            name: "Euclid 5/8 + 7/16",
            about: "Two Euclidean figures: five of eight eighths on toms, seven of sixteen sixteenths on the stick.",
            layers: vec![
                PolyLayer::new(MID_TOM, 8, 1, Bar).euclid(5, 0),
                PolyLayer::new(RIM, 16, 1, Bar).euclid(7, 2).vel(85),
                PolyLayer::new(OPEN_HAT, 2, 1, Bar).plain().vel(70),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuplet_factors_reduce() {
        assert_eq!(Tuplet::new(3, 2).factor(), (2, 3));
        assert_eq!(Tuplet::new(6, 4).factor(), (2, 3));
        assert_eq!(Tuplet::new(4, 4).sanitized(), Tuplet::STRAIGHT);
        let g = TupletGrid::new(0.5, Tuplet::new(3, 2), Tuplet::new(5, 4));
        // Triplet eighths (1/3 beat) split into five: 4/15 of a beat each.
        assert_eq!(g.factor(), (8, 15));
        assert!((g.step() - 4.0 / 15.0).abs() < 1e-15);
        assert!((g.group() - 4.0).abs() < 1e-12);
    }

    #[test]
    fn grid_points_are_exact_fractions() {
        let q = TupletGrid::new(0.25, Tuplet::new(5, 4), Tuplet::STRAIGHT);
        // 5 quintuplet sixteenths end exactly on the beat, far into a song.
        for bar in [0i64, 1, 999, 100_000] {
            let k = bar * 5 * 4;
            assert_eq!(q.point(k), (bar * 4) as f64);
        }
        assert_eq!(q.point(7), 7.0 / 5.0);
        let t = TupletGrid::new(1.0, Tuplet::new(7, 4), Tuplet::new(3, 2));
        assert_eq!(t.point(21), 8.0);
        assert_eq!(t.nearest(8.01), 8.0);
    }

    #[test]
    fn meters_and_modulation() {
        assert_eq!(bar_beats(7, 8, 4), 3.5);
        assert_eq!(bar_beats(15, 16, 4), 3.75);
        assert_eq!(bar_beats(5, 4, 4), 5.0);
        assert_eq!(modulated_tempo(100.0, Tuplet::new(3, 2)), 150.0);
        assert_eq!(modulated_tempo(150.0, Tuplet::new(2, 3)), 100.0);
    }

    #[test]
    fn layers_generate_even_hits() {
        let l = PolyLayer::new(36, 5, 1, SpanUnit::Bar);
        let notes = generate(&[l], 8.0, 4.0);
        assert_eq!(notes.len(), 10);
        assert_eq!(notes[5].start, 4.0);
        assert_eq!(notes[3].start, 12.0 / 5.0);
        // Accent on the downbeat of each cycle.
        assert!(notes[0].velocity > notes[1].velocity);
    }

    #[test]
    fn nested_layers_ghost_between_pulses() {
        let l = PolyLayer::new(38, 3, 2, SpanUnit::Beat).sub(5).plain();
        let notes = generate(&[l], 2.0, 4.0);
        assert_eq!(notes.len(), 15);
        assert_eq!(notes[5].start, 2.0 / 3.0);
        assert!(notes[1].velocity < notes[5].velocity);
    }

    #[test]
    fn euclid_spreads_hits() {
        let l = PolyLayer::new(36, 8, 1, SpanUnit::Bar).euclid(3, 0);
        let starts: Vec<f64> = generate(&[l], 4.0, 4.0).iter().map(|n| n.start).collect();
        assert_eq!(starts.len(), 3);
        assert_eq!(starts[0], 0.0);
    }

    #[test]
    fn every_preset_generates() {
        for p in presets() {
            assert!(!generate(&p.layers, 8.0, 4.0).is_empty(), "{}", p.name);
        }
    }
}
