//! Arpeggiator: plays the held notes one at a time in a pattern, synced to the tempo.
//!
//! (Placeholder runtime: passes notes through.)

use super::{MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const STYLES: &[&str] = &[
    "Up",
    "Down",
    "Up Down",
    "Down Up",
    "Up & Down",
    "Down & Up",
    "Converge",
    "Diverge",
    "Pinky Up",
    "Thumb Up",
    "Played",
    "Random",
    "Chord",
];
pub const RATES: &[&str] = &["1/32", "1/16T", "1/16", "1/8T", "1/8", "1/4T", "1/4", "1/2", "1 Bar"];
/// Beats per step for each entry of [`RATES`].
pub const RATE_BEATS: [f64; 9] = [0.125, 1.0 / 6.0, 0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 1.0, 2.0, 4.0];

device_params! {
    pub struct ArpeggiatorParams {
        style: "Style", 0.0, 12.0, 0.0, ParamKind::Choice(STYLES), Unit::Plain;
        rate: "Rate", 0.0, 8.0, 2.0, ParamKind::Choice(RATES), Unit::Plain;
        gate: "Gate", 0.01, 2.0, 0.5, LIN, Unit::Percent;
        steps: "Steps", 0.0, 8.0, 0.0, LIN, Unit::Plain;
        distance: "Distance", -24.0, 24.0, 12.0, LIN, Unit::Plain;
        hold: "Hold", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        velocity_decay: "Vel Decay", 0.0, 1.0, 0.0, LIN, Unit::Percent;
    }
}

pub struct Arpeggiator {}

impl Arpeggiator {
    pub fn new(seed: u32) -> Self {
        let _ = seed;
        Self {}
    }

    pub fn reset(&mut self) {}

    pub fn process(
        &mut self,
        _p: &ArpeggiatorParams,
        input: &[MidiEvent],
        out: &mut Vec<MidiEvent>,
        _ctx: &MidiFxContext,
    ) {
        for e in input {
            super::emit(out, e.frame as usize, e.kind);
        }
    }
}
