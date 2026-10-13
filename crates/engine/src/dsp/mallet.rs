//! Mallet Synth: Physically modelled mallet, plate, membrane and tube resonators. (Stub: silent until implemented.)

use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct MalletParams {
        volume_db: "Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
    }
}

/// Automatable parameters, in [`MalletParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; MalletParams::COUNT] = specs_from_info(MalletParams::INFO);

/// Factory presets: (browser category, name, settings).
pub fn presets() -> Vec<(&'static str, &'static str, MalletParams)> {
    Vec::new()
}

pub struct MalletSynth {
    _sr: f32,
}

impl MalletSynth {
    pub fn new(sample_rate: f32) -> Self {
        Self { _sr: sample_rate }
    }

    pub fn note_on(&mut self, _pitch: u8, _velocity: u8) {}

    pub fn note_off(&mut self, _pitch: u8) {}

    pub fn all_notes_off(&mut self) {}

    pub fn panic(&mut self) {}

    pub fn render(&mut self, _p: &MalletParams, _left: &mut [f32], _right: &mut [f32]) {}
}
