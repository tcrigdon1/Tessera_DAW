//! Waveshaping saturation and distortion.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const CURVES: &[&str] = &["Soft Clip", "Hard Clip", "Tube", "Fold"];

device_params! {
    pub struct SaturatorParams {
        drive: "Drive", -12.0, 36.0, 6.0, LIN, Unit::Db;
        curve: "Curve", 0.0, 3.0, 0.0, ParamKind::Choice(CURVES), Unit::Plain;
        output: "Output", -36.0, 12.0, -3.0, LIN, Unit::Db;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

/// The transfer curve for one sample.
#[inline]
pub fn shape(curve: u32, x: f32) -> f32 {
    match curve {
        1 => x.clamp(-1.0, 1.0),
        2 => {
            // Asymmetric: the positive half saturates harder than the negative,
            // adding even harmonics. The DC offset is removed afterwards.
            const BIAS: f32 = 0.3;
            (x + BIAS).tanh() - BIAS.tanh()
        }
        3 => {
            // Sine wave folder: smooth up to full scale, then folds back.
            (x * std::f32::consts::FRAC_PI_2).sin()
        }
        _ => x.tanh(),
    }
}

pub struct Saturator {
    dc: [Biquad; 2],
    dc_coefs: Coefs,
}

impl Saturator {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            dc: [Biquad::default(); 2],
            dc_coefs: Coefs::design(
                FilterShape::HighPass,
                8.0,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                sample_rate,
            ),
        }
    }

    pub fn reset(&mut self) {
        for f in &mut self.dc {
            f.reset();
        }
    }

    pub fn process(&mut self, p: &SaturatorParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let drive = super::db(p.drive);
        let out = super::db(p.output);
        let curve = p.curve.round().max(0.0) as u32;
        let wet = p.mix;
        let dry = 1.0 - wet;
        let tube = curve == 2;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let mut y = [shape(curve, *l * drive), shape(curve, *r * drive)];
            if tube {
                for c in 0..2 {
                    y[c] = self.dc[c].process(&self.dc_coefs, y[c]);
                }
            }
            *l = *l * dry + y[0] * out * wet;
            *r = *r * dry + y[1] * out * wet;
        }
    }
}

impl super::EffectParams for SaturatorParams {}

impl super::EffectRuntime for Saturator {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_are_bounded() {
        for c in 0..4 {
            for i in -100..=100 {
                let x = i as f32 * 0.2;
                let y = shape(c, x);
                assert!(y.is_finite() && y.abs() <= 1.5, "curve {c} at {x}: {y}");
            }
            assert!(shape(c, 0.0).abs() < 1e-6);
        }
    }

    #[test]
    fn heavy_drive_squares_a_sine() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = SaturatorParams {
            drive: 30.0,
            output: 0.0,
            ..Default::default()
        };
        let mut s = Saturator::new(sr);
        let mut l: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let mut r = l.clone();
        s.process(&p, &mut l, &mut r, &ctx);
        let peak = l.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak <= 1.0 && peak > 0.99);
        // Most samples sit near full scale.
        let near = l.iter().filter(|x| x.abs() > 0.9).count();
        assert!(near > l.len() * 3 / 4, "{near}");
    }
}
