//! Note Length: gives every note a fixed length in time or synced to the tempo, triggered on note-on or note-off.
//!
//! (Placeholder runtime: passes notes through.)

use super::{MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const MODES: &[&str] = &["Time", "Sync"];
pub const TRIGGERS: &[&str] = &["Note On", "Note Off"];
pub const SYNC_LENGTHS: &[&str] = &["1/32", "1/16", "1/8", "1/4", "1/2", "1 Bar", "2 Bars", "4 Bars"];
/// Beats for each entry of [`SYNC_LENGTHS`] (bars assume 4/4).
pub const SYNC_BEATS: [f64; 8] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

device_params! {
    pub struct NoteLengthParams {
        mode: "Mode", 0.0, 1.0, 0.0, ParamKind::Choice(MODES), Unit::Plain;
        time: "Length", 1.0, 10000.0, 250.0, ParamKind::Continuous(Scale::Log), Unit::Ms;
        sync: "Sync", 0.0, 7.0, 3.0, ParamKind::Choice(SYNC_LENGTHS), Unit::Plain;
        gate: "Gate", 0.01, 2.0, 1.0, LIN, Unit::Percent;
        trigger: "Trigger", 0.0, 1.0, 0.0, ParamKind::Choice(TRIGGERS), Unit::Plain;
    }
}

impl NoteLengthParams {
    /// The millisecond length is hidden in Sync mode and vice versa.
    pub fn is_relevant(&self, index: usize) -> bool {
        match index {
            1 => self.mode < 0.5,
            2 => self.mode >= 0.5,
            _ => true,
        }
    }
}

pub struct NoteLength {}

impl NoteLength {
    pub fn new(sample_rate: f32) -> Self {
        let _ = sample_rate;
        Self {}
    }

    pub fn reset(&mut self) {}

    pub fn process(
        &mut self,
        _p: &NoteLengthParams,
        input: &[MidiEvent],
        out: &mut Vec<MidiEvent>,
        _ctx: &MidiFxContext,
    ) {
        for e in input {
            super::emit(out, e.frame as usize, e.kind);
        }
    }
}
