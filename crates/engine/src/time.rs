//! Musical time helpers.

use serde::{Deserialize, Serialize};

/// Global launch quantisation for Session view clips and scenes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Quantization {
    None,
    ThirtySecondNote,
    SixteenthNote,
    EighthNote,
    QuarterNote,
    HalfNote,
    #[default]
    Bar,
    TwoBars,
    FourBars,
    EightBars,
}

impl Quantization {
    pub const ALL: [Quantization; 10] = [
        Quantization::None,
        Quantization::EightBars,
        Quantization::FourBars,
        Quantization::TwoBars,
        Quantization::Bar,
        Quantization::HalfNote,
        Quantization::QuarterNote,
        Quantization::EighthNote,
        Quantization::SixteenthNote,
        Quantization::ThirtySecondNote,
    ];

    /// Grid size in beats, or `None` for immediate launches.
    pub fn beats(self, beats_per_bar: f64) -> Option<f64> {
        match self {
            Quantization::None => None,
            Quantization::ThirtySecondNote => Some(0.125),
            Quantization::SixteenthNote => Some(0.25),
            Quantization::EighthNote => Some(0.5),
            Quantization::QuarterNote => Some(1.0),
            Quantization::HalfNote => Some(2.0),
            Quantization::Bar => Some(beats_per_bar),
            Quantization::TwoBars => Some(beats_per_bar * 2.0),
            Quantization::FourBars => Some(beats_per_bar * 4.0),
            Quantization::EightBars => Some(beats_per_bar * 8.0),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Quantization::None => "None",
            Quantization::ThirtySecondNote => "1/32",
            Quantization::SixteenthNote => "1/16",
            Quantization::EighthNote => "1/8",
            Quantization::QuarterNote => "1/4",
            Quantization::HalfNote => "1/2",
            Quantization::Bar => "1 Bar",
            Quantization::TwoBars => "2 Bars",
            Quantization::FourBars => "4 Bars",
            Quantization::EightBars => "8 Bars",
        }
    }

    /// The first grid point at or after `beat`.
    pub fn next_boundary(self, beat: f64, beats_per_bar: f64) -> f64 {
        match self.beats(beats_per_bar) {
            None => beat,
            Some(grid) => {
                let n = (beat / grid - 1e-9).ceil();
                (n * grid).max(beat)
            }
        }
    }
}

/// Formats a beat position as `bar.beat.sixteenth`, 1-based like most DAWs.
pub fn format_position(beat: f64, beats_per_bar: u32) -> String {
    let beat = beat.max(0.0);
    let bpb = beats_per_bar.max(1) as f64;
    let bar = (beat / bpb).floor();
    let in_bar = beat - bar * bpb;
    let b = in_bar.floor();
    let six = ((in_bar - b) * 4.0).floor();
    format!("{}.{}.{}", bar as i64 + 1, b as i64 + 1, six as i64 + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantization_boundaries() {
        let q = Quantization::Bar;
        assert_eq!(q.next_boundary(0.0, 4.0), 0.0);
        assert_eq!(q.next_boundary(0.1, 4.0), 4.0);
        assert_eq!(q.next_boundary(4.0, 4.0), 4.0);
        assert_eq!(q.next_boundary(7.9, 4.0), 8.0);
        assert_eq!(Quantization::None.next_boundary(3.3, 4.0), 3.3);
        assert_eq!(Quantization::EighthNote.next_boundary(0.6, 4.0), 1.0);
    }

    #[test]
    fn position_format() {
        assert_eq!(format_position(0.0, 4), "1.1.1");
        assert_eq!(format_position(5.25, 4), "2.2.2");
    }
}
