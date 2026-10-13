//! Electric Piano: Physically modelled electric piano: mallet, tine, tone bar and pickup. (Stub: silent until implemented.)

use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct ElectricParams {
        volume_db: "Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
    }
}

/// Automatable parameters, in [`ElectricParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; ElectricParams::COUNT] = specs_from_info(ElectricParams::INFO);

/// Factory presets: (browser category, name, settings).
pub fn presets() -> Vec<(&'static str, &'static str, ElectricParams)> {
    Vec::new()
}

pub struct ElectricPiano {
    _sr: f32,
}

impl ElectricPiano {
    pub fn new(sample_rate: f32) -> Self {
        Self { _sr: sample_rate }
    }

    pub fn note_on(&mut self, _pitch: u8, _velocity: u8) {}

    pub fn note_off(&mut self, _pitch: u8) {}

    pub fn all_notes_off(&mut self) {}

    pub fn panic(&mut self) {}

    pub fn render(&mut self, _p: &ElectricParams, _left: &mut [f32], _right: &mut [f32]) {}
}
