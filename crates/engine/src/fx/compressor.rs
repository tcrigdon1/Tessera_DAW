//! Feed-forward stereo-linked compressor with soft knee.

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

device_params! {
    pub struct CompressorParams {
        threshold: "Threshold", -60.0, 0.0, -18.0, LIN, Unit::Db;
        ratio: "Ratio", 1.0, 20.0, 4.0, LOG, Unit::Ratio;
        attack: "Attack", 0.05, 200.0, 5.0, LOG, Unit::Ms;
        release: "Release", 5.0, 2000.0, 120.0, LOG, Unit::Ms;
        knee: "Knee", 0.0, 18.0, 6.0, LIN, Unit::Db;
        makeup: "Makeup", -12.0, 24.0, 0.0, LIN, Unit::Db;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl CompressorParams {
    /// Static gain curve: gain reduction in dB (<= 0) for an input level.
    pub fn gain_reduction_db(&self, level_db: f32) -> f32 {
        let slope = if self.ratio >= 19.95 {
            1.0
        } else {
            1.0 - 1.0 / self.ratio
        };
        let over = level_db - self.threshold;
        let knee = self.knee.max(0.0);
        if knee > 0.0 && over.abs() <= knee * 0.5 {
            let x = over + knee * 0.5;
            -slope * x * x / (2.0 * knee)
        } else if over > 0.0 {
            -slope * over
        } else {
            0.0
        }
    }
}

pub struct Compressor {
    sr: f32,
    /// Smoothed gain reduction in dB (<= 0).
    env_db: f32,
    /// Largest gain reduction in the last block, for the meter.
    pub meter_db: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            env_db: 0.0,
            meter_db: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.env_db = 0.0;
        self.meter_db = 0.0;
    }

    pub fn process(&mut self, p: &CompressorParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let att = super::smoothing(p.attack * 0.001, self.sr);
        let rel = super::smoothing(p.release * 0.001, self.sr);
        let makeup = super::db(p.makeup);
        let wet = p.mix;
        let dry = 1.0 - wet;
        let mut most = 0.0f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let peak = l.abs().max(r.abs());
            let level = if peak > 1e-6 { 20.0 * peak.log10() } else { -120.0 };
            let target = p.gain_reduction_db(level);
            // Attack when more reduction is needed, release otherwise.
            let coef = if target < self.env_db { att } else { rel };
            self.env_db = target + (self.env_db - target) * coef;
            most = most.min(self.env_db);
            let g = super::db(self.env_db) * makeup;
            *l *= dry + wet * g;
            *r *= dry + wet * g;
        }
        if self.env_db > -1e-6 {
            self.env_db = 0.0;
        }
        self.meter_db = most;
    }
}

impl super::EffectParams for CompressorParams {}

impl super::EffectRuntime for Compressor {
    fn meter(&self) -> Option<f32> {
        Some(self.meter_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_curve() {
        let p = CompressorParams {
            threshold: -20.0,
            ratio: 4.0,
            knee: 0.0,
            ..Default::default()
        };
        assert_eq!(p.gain_reduction_db(-30.0), 0.0);
        assert!((p.gain_reduction_db(-8.0) + 9.0).abs() < 1e-4);
        let soft = CompressorParams { knee: 6.0, ..p };
        // Continuous at the knee edges.
        assert!((soft.gain_reduction_db(-23.0)).abs() < 1e-4);
        assert!((soft.gain_reduction_db(-17.0) - p.gain_reduction_db(-17.0)).abs() < 1e-4);
    }

    #[test]
    fn reduces_loud_signals_and_leaves_quiet_ones() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = CompressorParams {
            threshold: -20.0,
            ratio: 4.0,
            knee: 0.0,
            attack: 1.0,
            ..Default::default()
        };
        let run = |amp: f32| {
            let mut c = Compressor::new(sr);
            let mut out = 0.0f32;
            for _ in 0..20 {
                let mut l = [amp; 1000];
                let mut r = [amp; 1000];
                c.process(&p, &mut l, &mut r, &ctx);
                out = l[999];
            }
            (out, c.meter_db)
        };
        let (quiet, gr) = run(0.01); // -40 dB
        assert!((quiet - 0.01).abs() < 1e-5 && gr == 0.0);
        let (loud, gr) = run(1.0); // 0 dB: 20 dB over, 15 dB of reduction
        let out_db = 20.0 * loud.log10();
        assert!((out_db + 15.0).abs() < 0.2, "{out_db}");
        assert!((gr + 15.0).abs() < 0.2);
    }
}
