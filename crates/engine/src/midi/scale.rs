//! Musical scales, shared by the piano roll (highlighting, fold to scale)
//! and the Scale MIDI effect.

use serde::{Deserialize, Serialize};

/// Scale types. Each is a set of semitone offsets from the root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ScaleKind {
    #[default]
    Major,
    Minor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    HarmonicMinor,
    MelodicMinor,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    WholeTone,
    Chromatic,
}

impl ScaleKind {
    pub const ALL: [ScaleKind; 14] = [
        ScaleKind::Major,
        ScaleKind::Minor,
        ScaleKind::Dorian,
        ScaleKind::Phrygian,
        ScaleKind::Lydian,
        ScaleKind::Mixolydian,
        ScaleKind::Locrian,
        ScaleKind::HarmonicMinor,
        ScaleKind::MelodicMinor,
        ScaleKind::MajorPentatonic,
        ScaleKind::MinorPentatonic,
        ScaleKind::Blues,
        ScaleKind::WholeTone,
        ScaleKind::Chromatic,
    ];

    /// Labels in [`ScaleKind::ALL`] order (also the Scale device's choices).
    pub const LABELS: [&'static str; 14] = [
        "Major",
        "Minor",
        "Dorian",
        "Phrygian",
        "Lydian",
        "Mixolydian",
        "Locrian",
        "Harmonic Minor",
        "Melodic Minor",
        "Major Pentatonic",
        "Minor Pentatonic",
        "Blues",
        "Whole Tone",
        "Chromatic",
    ];

    pub fn label(self) -> &'static str {
        Self::LABELS[self.index()]
    }

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    /// Semitone offsets from the root, ascending, starting with 0.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            ScaleKind::Major => &[0, 2, 4, 5, 7, 9, 11],
            ScaleKind::Minor => &[0, 2, 3, 5, 7, 8, 10],
            ScaleKind::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            ScaleKind::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            ScaleKind::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            ScaleKind::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            ScaleKind::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            ScaleKind::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            ScaleKind::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            ScaleKind::MajorPentatonic => &[0, 2, 4, 7, 9],
            ScaleKind::MinorPentatonic => &[0, 3, 5, 7, 10],
            ScaleKind::Blues => &[0, 3, 5, 6, 7, 10],
            ScaleKind::WholeTone => &[0, 2, 4, 6, 8, 10],
            ScaleKind::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        }
    }

    /// Bit `n` set when pitch class `n` (relative to the root) is in the scale.
    pub fn mask(self) -> u16 {
        self.intervals().iter().fold(0u16, |m, &i| m | 1 << i)
    }
}

/// Root note names for pickers.
pub const ROOT_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// A scale on a root note (0 = C).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MusicalScale {
    pub root: u8,
    pub kind: ScaleKind,
}

impl MusicalScale {
    pub fn new(root: u8, kind: ScaleKind) -> Self {
        Self { root: root % 12, kind }
    }

    pub fn name(&self) -> String {
        format!("{} {}", ROOT_NAMES[self.root as usize % 12], self.kind.label())
    }

    /// True when the pitch belongs to the scale.
    pub fn contains(&self, pitch: u8) -> bool {
        let pc = (pitch as i32 - self.root as i32).rem_euclid(12);
        self.kind.mask() & (1 << pc) != 0
    }

    /// True when the pitch is the root.
    pub fn is_root(&self, pitch: u8) -> bool {
        (pitch as i32 - self.root as i32).rem_euclid(12) == 0
    }

    /// The nearest scale pitch at or below `pitch` (ties go down), clamped to
    /// 0 ..= 127.
    pub fn snap(&self, pitch: u8) -> u8 {
        for d in 0..12i32 {
            for p in [pitch as i32 - d, pitch as i32 + d] {
                if (0..=127).contains(&p) && self.contains(p as u8) {
                    return p as u8;
                }
            }
        }
        pitch
    }

    /// Moves `pitch` by `steps` scale degrees (pitches not in the scale are
    /// snapped first). Returns `None` outside the MIDI range.
    pub fn step(&self, pitch: u8, steps: i32) -> Option<u8> {
        let mut p = self.snap(pitch) as i32;
        let dir = steps.signum();
        for _ in 0..steps.abs() {
            loop {
                p += dir;
                if !(0..=127).contains(&p) {
                    return None;
                }
                if self.contains(p as u8) {
                    break;
                }
            }
        }
        Some(p as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c_major_contains_white_keys() {
        let s = MusicalScale::new(0, ScaleKind::Major);
        let white: Vec<u8> = (60..72).filter(|&p| s.contains(p)).collect();
        assert_eq!(white, vec![60, 62, 64, 65, 67, 69, 71]);
        assert!(s.is_root(48));
        assert_eq!(s.snap(61), 60);
        assert_eq!(s.snap(66), 65);
        assert_eq!(s.step(60, 2), Some(64));
        assert_eq!(s.step(64, -3), Some(59));
        assert_eq!(s.step(127, 1), None);
    }

    #[test]
    fn roots_shift_the_scale() {
        let a_minor = MusicalScale::new(9, ScaleKind::Minor);
        let c_major = MusicalScale::new(0, ScaleKind::Major);
        for p in 0..128u8 {
            assert_eq!(a_minor.contains(p), c_major.contains(p));
        }
        assert_eq!(a_minor.name(), "A Minor");
        for (i, k) in ScaleKind::ALL.iter().enumerate() {
            assert_eq!(ScaleKind::from_index(i), *k);
            assert_eq!(k.intervals()[0], 0);
        }
    }
}
