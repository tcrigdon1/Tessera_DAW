//! Hybrid Reverb. (Stub: passes audio through until implemented.)

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct HybridReverbParams {
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl super::EffectParams for HybridReverbParams {}

pub struct HybridReverb {
    _sr: f32,
}

impl HybridReverb {
    pub fn new(sample_rate: f32) -> Self {
        Self { _sr: sample_rate }
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, _p: &HybridReverbParams, _left: &mut [f32], _right: &mut [f32], _ctx: &FxContext) {}
}

impl super::EffectRuntime for HybridReverb {}
