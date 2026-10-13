//! Random: with some chance, moves each note by a random number of steps.
//!
//! (Placeholder runtime: passes notes through.)

use super::{MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const SIGNS: &[&str] = &["Add", "Sub", "Bi"];
pub const MODES: &[&str] = &["Random", "Alt"];

device_params! {
    pub struct RandomParams {
        chance: "Chance", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        choices: "Choices", 1.0, 24.0, 1.0, LIN, Unit::Plain;
        scale: "Scale", 1.0, 24.0, 12.0, LIN, Unit::Plain;
        sign: "Sign", 0.0, 2.0, 0.0, ParamKind::Choice(SIGNS), Unit::Plain;
        mode: "Mode", 0.0, 1.0, 0.0, ParamKind::Choice(MODES), Unit::Plain;
    }
}

pub struct Random {}

impl Random {
    pub fn new(seed: u32) -> Self {
        let _ = seed;
        Self {}
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, _p: &RandomParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        for e in input {
            super::emit(out, e.frame as usize, e.kind);
        }
    }
}
