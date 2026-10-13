//! Chord: adds up to six notes at fixed intervals (0 = off) to every incoming note.
//!
//! (Placeholder runtime: passes notes through.)

use super::{MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct ChordParams {
        shift1: "Shift 1", -36.0, 36.0, 4.0, LIN, Unit::Plain;
        shift2: "Shift 2", -36.0, 36.0, 7.0, LIN, Unit::Plain;
        shift3: "Shift 3", -36.0, 36.0, 0.0, LIN, Unit::Plain;
        shift4: "Shift 4", -36.0, 36.0, 0.0, LIN, Unit::Plain;
        shift5: "Shift 5", -36.0, 36.0, 0.0, LIN, Unit::Plain;
        shift6: "Shift 6", -36.0, 36.0, 0.0, LIN, Unit::Plain;
        velocity: "Velocity", 0.0, 2.0, 1.0, LIN, Unit::Percent;
    }
}

pub struct Chord {}

impl Chord {
    pub fn new() -> Self {
        Self {}
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, _p: &ChordParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        for e in input {
            super::emit(out, e.frame as usize, e.kind);
        }
    }
}
