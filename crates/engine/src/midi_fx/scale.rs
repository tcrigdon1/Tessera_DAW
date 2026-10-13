//! Scale: moves every note onto the nearest note of a scale.
//!
//! (Placeholder runtime: passes notes through.)

use super::{MidiEvent, MidiFxContext};
use crate::fx::{self, device_params, ParamKind, Unit};
use crate::midi::scale::{ScaleKind, ROOT_NAMES};

const LIN: ParamKind = ParamKind::Continuous(fx::Scale::Linear);

device_params! {
    pub struct ScaleParams {
        base: "Base", 0.0, 11.0, 0.0, ParamKind::Choice(&ROOT_NAMES), Unit::Plain;
        scale: "Scale", 0.0, 13.0, 0.0, ParamKind::Choice(&ScaleKind::LABELS), Unit::Plain;
        transpose: "Transpose", -36.0, 36.0, 0.0, LIN, Unit::Plain;
    }
}

pub struct Scale {}

impl Scale {
    pub fn new() -> Self {
        Self {}
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, _p: &ScaleParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        for e in input {
            super::emit(out, e.frame as usize, e.kind);
        }
    }
}

impl Default for Scale {
    fn default() -> Self {
        Self::new()
    }
}
