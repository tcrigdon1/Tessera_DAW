//! Velocity: shapes (drive), limits (out low/high) and randomizes note velocities.
//!
//! (Placeholder runtime: passes notes through.)

use super::{MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct VelocityParams {
        drive: "Drive", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        out_low: "Out Low", 1.0, 127.0, 1.0, LIN, Unit::Plain;
        out_high: "Out Hi", 1.0, 127.0, 127.0, LIN, Unit::Plain;
        random: "Random", 0.0, 64.0, 0.0, LIN, Unit::Plain;
    }
}

pub struct Velocity {}

impl Velocity {
    pub fn new(seed: u32) -> Self {
        let _ = seed;
        Self {}
    }

    pub fn reset(&mut self) {}

    pub fn process(
        &mut self,
        _p: &VelocityParams,
        input: &[MidiEvent],
        out: &mut Vec<MidiEvent>,
        _ctx: &MidiFxContext,
    ) {
        for e in input {
            super::emit(out, e.frame as usize, e.kind);
        }
    }
}
