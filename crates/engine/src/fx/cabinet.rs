//! Cabinet. (Stub: passes audio through until implemented.)

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct CabinetParams {
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl super::EffectParams for CabinetParams {}

pub struct Cabinet {
    _sr: f32,
}

impl Cabinet {
    pub fn new(sample_rate: f32) -> Self {
        Self { _sr: sample_rate }
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, _p: &CabinetParams, _left: &mut [f32], _right: &mut [f32], _ctx: &FxContext) {}
}

impl super::EffectRuntime for Cabinet {}
