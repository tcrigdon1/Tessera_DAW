//! Analog Synth: Two-oscillator subtractive synthesizer with two filters, two envelopes and LFOs. (Stub: silent until implemented.)

use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct AnalogParams {
        volume_db: "Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
    }
}

/// Automatable parameters, in [`AnalogParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; AnalogParams::COUNT] = specs_from_info(AnalogParams::INFO);

/// Factory presets: (browser category, name, settings).
pub fn presets() -> Vec<(&'static str, &'static str, AnalogParams)> {
    Vec::new()
}

pub struct AnalogSynth {
    _sr: f32,
}

impl AnalogSynth {
    pub fn new(sample_rate: f32) -> Self {
        Self { _sr: sample_rate }
    }

    pub fn note_on(&mut self, _pitch: u8, _velocity: u8) {}

    pub fn note_off(&mut self, _pitch: u8) {}

    pub fn all_notes_off(&mut self) {}

    pub fn panic(&mut self) {}

    pub fn render(&mut self, _p: &AnalogParams, _left: &mut [f32], _right: &mut [f32]) {}
}
