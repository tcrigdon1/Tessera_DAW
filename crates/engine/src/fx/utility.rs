//! Utility: gain, stereo width, mono and phase inversion.

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct UtilityParams {
        gain: "Gain", -36.0, 36.0, 0.0, LIN, Unit::Db;
        width: "Width", 0.0, 2.0, 1.0, LIN, Unit::Percent;
        mono: "Mono", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        invert_l: "Phase L", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        invert_r: "Phase R", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
    }
}

pub struct Utility {
    sr: f32,
    gain: f32,
}

impl Utility {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            gain: -1.0,
        }
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, p: &UtilityParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let target = super::db(p.gain);
        if self.gain < 0.0 {
            self.gain = target;
        }
        let k = 1.0 - super::smoothing(0.01, self.sr);
        let width = if p.mono >= 0.5 { 0.0 } else { p.width };
        let pl = if p.invert_l >= 0.5 { -1.0 } else { 1.0 };
        let pr = if p.invert_r >= 0.5 { -1.0 } else { 1.0 };
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.gain += (target - self.gain) * k;
            let mid = (*l + *r) * 0.5;
            let side = (*l - *r) * 0.5 * width;
            *l = (mid + side) * self.gain * pl;
            *r = (mid - side) * self.gain * pr;
        }
    }
}

impl super::EffectParams for UtilityParams {}

impl super::EffectRuntime for Utility {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_and_phase() {
        let ctx = FxContext::new(48000.0, 120.0);
        let mut u = Utility::new(48000.0);
        let p = UtilityParams {
            mono: 1.0,
            invert_r: 1.0,
            ..Default::default()
        };
        let mut l = [1.0f32; 8];
        let mut r = [0.0f32; 8];
        u.process(&p, &mut l, &mut r, &ctx);
        assert!((l[7] - 0.5).abs() < 1e-6 && (r[7] + 0.5).abs() < 1e-6);
    }
}
