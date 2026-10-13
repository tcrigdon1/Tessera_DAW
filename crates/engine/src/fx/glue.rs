//! Bus Compressor. (Stub: passes audio through until implemented.)

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct BusCompressorParams {
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl super::EffectParams for BusCompressorParams {}

pub struct BusCompressor {
    _sr: f32,
}

impl BusCompressor {
    pub fn new(sample_rate: f32) -> Self {
        Self { _sr: sample_rate }
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, _p: &BusCompressorParams, _left: &mut [f32], _right: &mut [f32], _ctx: &FxContext) {}
}

impl super::EffectRuntime for BusCompressor {}
