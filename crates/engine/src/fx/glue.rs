//! Bus Compressor: a console-style stereo bus compressor for gluing mixes
//! and groups together.
//!
//! The detector is stereo linked, optionally high-passed so the low end does
//! not pump the whole mix, and works in the log domain with stepped attack
//! and release times. "Auto" release combines a fast envelope that recovers
//! quickly after transients with a slow one that follows sustained
//! compression, so dense material breathes less than with a fixed release.
//! Range caps the maximum gain reduction, and an optional soft clipper
//! catches peaks after the makeup gain.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const RATIOS: &[&str] = &["2", "4", "10"];
const RATIO_VALUES: [f32; 3] = [2.0, 4.0, 10.0];
pub const ATTACKS: &[&str] = &["0.01 ms", "0.1 ms", "0.3 ms", "1 ms", "3 ms", "10 ms", "30 ms"];
const ATTACK_MS: [f32; 7] = [0.01, 0.1, 0.3, 1.0, 3.0, 10.0, 30.0];
pub const RELEASES: &[&str] = &["0.1 s", "0.2 s", "0.4 s", "0.6 s", "0.8 s", "1.2 s", "Auto"];
const RELEASE_S: [f32; 6] = [0.1, 0.2, 0.4, 0.6, 0.8, 1.2];
/// Index of "Auto" in [`RELEASES`].
pub const RELEASE_AUTO: usize = 6;

/// Detector high-pass frequencies at or below this are treated as off.
pub const HPF_OFF_HZ: f32 = 10.5;
/// Width of the (fixed) soft knee in dB.
const KNEE_DB: f32 = 4.0;

device_params! {
    pub struct BusCompressorParams {
        threshold: "Threshold", -40.0, 0.0, -10.0, LIN, Unit::Db;
        ratio: "Ratio", 0.0, 2.0, 1.0, ParamKind::Choice(RATIOS), Unit::Plain;
        attack: "Attack", 0.0, 6.0, 4.0, ParamKind::Choice(ATTACKS), Unit::Plain;
        release: "Release", 0.0, 6.0, 6.0, ParamKind::Choice(RELEASES), Unit::Plain;
        makeup: "Makeup", -12.0, 24.0, 0.0, LIN, Unit::Db;
        range: "Range", 1.0, 60.0, 60.0, LIN, Unit::Db;
        soft_clip: "Soft Clip", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        sc_hpf: "SC High-Pass", 10.0, 500.0, 10.0, LOG, Unit::Hz;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

pub(crate) fn choice(v: f32, len: usize) -> usize {
    (v.round().max(0.0) as usize).min(len.max(1) - 1)
}

impl BusCompressorParams {
    pub fn ratio_value(&self) -> f32 {
        RATIO_VALUES[choice(self.ratio, RATIO_VALUES.len())]
    }

    pub fn attack_ms(&self) -> f32 {
        ATTACK_MS[choice(self.attack, ATTACK_MS.len())]
    }

    /// Release in seconds, or `None` for Auto.
    pub fn release_s(&self) -> Option<f32> {
        let i = choice(self.release, RELEASES.len());
        RELEASE_S.get(i).copied()
    }

    /// Static gain curve including Range: gain change in dB (<= 0).
    pub fn gain_reduction_db(&self, level_db: f32) -> f32 {
        let slope = 1.0 - 1.0 / self.ratio_value();
        let over = level_db - self.threshold;
        let gr = if over.abs() <= KNEE_DB * 0.5 {
            let x = over + KNEE_DB * 0.5;
            -slope * x * x / (2.0 * KNEE_DB)
        } else if over > 0.0 {
            -slope * over
        } else {
            0.0
        };
        gr.max(-self.range.max(0.0))
    }
}

impl super::EffectParams for BusCompressorParams {
    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Mix Glue",
                Self {
                    threshold: -12.0,
                    ratio: 0.0,
                    attack: 5.0,
                    release: RELEASE_AUTO as f32,
                    makeup: 2.0,
                    range: 6.0,
                    sc_hpf: 90.0,
                    ..d
                },
            ),
            (
                "Drum Smash",
                Self {
                    threshold: -24.0,
                    ratio: 2.0,
                    attack: 3.0,
                    release: 1.0,
                    makeup: 10.0,
                    soft_clip: 1.0,
                    mix: 0.5,
                    ..d
                },
            ),
            (
                "Gentle Master",
                Self {
                    threshold: -6.0,
                    ratio: 0.0,
                    attack: 6.0,
                    release: RELEASE_AUTO as f32,
                    makeup: 1.0,
                    range: 3.0,
                    sc_hpf: 150.0,
                    ..d
                },
            ),
            (
                "Punchy Group",
                Self {
                    threshold: -16.0,
                    ratio: 1.0,
                    attack: 5.0,
                    release: 2.0,
                    makeup: 4.0,
                    range: 12.0,
                    sc_hpf: 60.0,
                    ..d
                },
            ),
        ]
    }
}

/// Gentle clipper with unity gain below 0.7 and a ceiling at 1.0.
#[inline]
pub(crate) fn soft_clip(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.7 {
        x
    } else {
        let y = 0.7 + 0.3 * ((a - 0.7) / 0.3).tanh();
        y.copysign(x)
    }
}

pub struct BusCompressor {
    sr: f32,
    hpf: [Biquad; 2],
    hpf_coefs: Coefs,
    hpf_freq: f32,
    /// Gain reduction envelopes in dB (<= 0): the main (or fast auto) one and
    /// the slow auto-release one.
    env_db: f32,
    slow_db: f32,
    /// Smoothed makeup and mix, to avoid zipper noise.
    makeup: f32,
    mix: f32,
    meter_db: f32,
}

impl BusCompressor {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            hpf: [Biquad::default(); 2],
            hpf_coefs: Coefs::IDENTITY,
            hpf_freq: 0.0,
            env_db: 0.0,
            slow_db: 0.0,
            makeup: 1.0,
            mix: 1.0,
            meter_db: 0.0,
        }
    }

    pub fn reset(&mut self) {
        for f in &mut self.hpf {
            f.reset();
        }
        self.env_db = 0.0;
        self.slow_db = 0.0;
        self.meter_db = 0.0;
    }

    pub fn process(&mut self, p: &BusCompressorParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let sr = self.sr;
        let hpf_on = p.sc_hpf > HPF_OFF_HZ;
        if hpf_on && (p.sc_hpf - self.hpf_freq).abs() > 0.01 {
            self.hpf_freq = p.sc_hpf;
            self.hpf_coefs = Coefs::design(
                FilterShape::HighPass,
                p.sc_hpf,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                sr,
            );
        }
        let att = super::smoothing(p.attack_ms() * 0.001, sr);
        let auto = p.release_s().is_none();
        let rel = super::smoothing(p.release_s().unwrap_or(0.1), sr);
        // Auto: the slow envelope only picks up sustained compression.
        let slow_att = super::smoothing(0.25, sr);
        let slow_rel = super::smoothing(1.2, sr);
        let smooth = super::smoothing(0.01, sr);
        let makeup_t = super::db(p.makeup.clamp(-12.0, 24.0));
        let mix_t = p.mix.clamp(0.0, 1.0);
        let clip = p.soft_clip >= 0.5;
        let mut most = 0.0f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let (dl, dr) = if hpf_on {
                (
                    self.hpf[0].process(&self.hpf_coefs, *l),
                    self.hpf[1].process(&self.hpf_coefs, *r),
                )
            } else {
                (*l, *r)
            };
            let peak = dl.abs().max(dr.abs());
            let level = if peak > 1e-6 { 20.0 * peak.log10() } else { -120.0 };
            let target = p.gain_reduction_db(level);
            let coef = if target < self.env_db { att } else { rel };
            self.env_db = target + (self.env_db - target) * coef;
            let gr = if auto {
                let c = if target < self.slow_db { slow_att } else { slow_rel };
                self.slow_db = target + (self.slow_db - target) * c;
                self.env_db.min(self.slow_db)
            } else {
                self.env_db
            };
            most = most.min(gr);
            self.makeup = makeup_t + (self.makeup - makeup_t) * smooth;
            self.mix = mix_t + (self.mix - mix_t) * smooth;
            let g = super::db(gr) * self.makeup;
            let (mut wl, mut wr) = (*l * g, *r * g);
            if clip {
                wl = soft_clip(wl);
                wr = soft_clip(wr);
            }
            *l += (wl - *l) * self.mix;
            *r += (wr - *r) * self.mix;
        }
        if self.env_db > -1e-6 {
            self.env_db = 0.0;
        }
        if self.slow_db > -1e-6 {
            self.slow_db = 0.0;
        }
        self.meter_db = most;
    }
}

impl super::EffectRuntime for BusCompressor {
    fn meter(&self) -> Option<f32> {
        Some(self.meter_db)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{EffectParams, EffectRuntime};
    use super::*;

    const SR: f32 = 48000.0;

    fn run_const(p: &BusCompressorParams, amp: f32, blocks: usize) -> (BusCompressor, f32) {
        let ctx = FxContext::new(SR, 120.0);
        let mut c = BusCompressor::new(SR);
        let mut out = 0.0;
        for _ in 0..blocks {
            let mut l = [amp; 512];
            let mut r = [amp; 512];
            c.process(p, &mut l, &mut r, &ctx);
            out = l[511];
        }
        (c, out)
    }

    #[test]
    fn static_curve_and_range() {
        let p = BusCompressorParams {
            threshold: -20.0,
            ratio: 1.0, // 4:1
            ..Default::default()
        };
        assert_eq!(p.gain_reduction_db(-40.0), 0.0);
        assert!((p.gain_reduction_db(0.0) + 15.0).abs() < 1e-3);
        let limited = BusCompressorParams { range: 6.0, ..p };
        assert!((limited.gain_reduction_db(0.0) + 6.0).abs() < 1e-3);
        // Continuous across the knee.
        let a = p.gain_reduction_db(-20.0 + KNEE_DB * 0.5 - 1e-3);
        let b = p.gain_reduction_db(-20.0 + KNEE_DB * 0.5 + 1e-3);
        assert!((a - b).abs() < 1e-2);
    }

    #[test]
    fn compresses_loud_signals_and_meters() {
        let p = BusCompressorParams {
            threshold: -20.0,
            ratio: 1.0,
            attack: 3.0,
            release: 1.0,
            ..Default::default()
        };
        let (c, out) = run_const(&p, 1.0, 100);
        let out_db = 20.0 * out.log10();
        assert!((out_db + 15.0).abs() < 0.3, "{out_db}");
        assert!((c.meter().unwrap() + 15.0).abs() < 0.3);
        let (c, quiet) = run_const(&p, 0.01, 50);
        assert!((quiet - 0.01).abs() < 1e-5);
        assert_eq!(c.meter(), Some(0.0));
    }

    #[test]
    fn ratios_differ() {
        let base = BusCompressorParams {
            threshold: -20.0,
            release: 0.0,
            ..Default::default()
        };
        let outs: Vec<f32> = (0..3)
            .map(|i| {
                run_const(
                    &BusCompressorParams {
                        ratio: i as f32,
                        ..base
                    },
                    1.0,
                    60,
                )
                .1
            })
            .collect();
        assert!(outs[0] > outs[1] && outs[1] > outs[2], "{outs:?}");
    }

    #[test]
    fn auto_release_recovers_after_a_short_burst() {
        let ctx = FxContext::new(SR, 120.0);
        let p = BusCompressorParams {
            threshold: -20.0,
            ratio: 2.0,
            attack: 2.0,
            release: RELEASE_AUTO as f32,
            ..Default::default()
        };
        let mut c = BusCompressor::new(SR);
        // A 5 ms burst, then a quiet signal.
        let mut l: Vec<f32> = (0..48000).map(|i| if i < 240 { 1.0 } else { 0.05 }).collect();
        let mut r = l.clone();
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            c.process(&p, cl, cr, &ctx);
        }
        // After 0.5 s the gain is nearly back to unity.
        assert!((l[24000] / 0.05) > 0.9, "{}", l[24000]);
    }

    #[test]
    fn soft_clip_bounds_output_and_dry_wet_blends() {
        let p = BusCompressorParams {
            threshold: 0.0,
            makeup: 24.0,
            soft_clip: 1.0,
            ..Default::default()
        };
        let (_, out) = run_const(&p, 0.9, 10);
        assert!(out <= 1.0 && out > 0.9, "{out}");
        let p = BusCompressorParams {
            threshold: -40.0,
            ratio: 2.0,
            mix: 0.0,
            ..Default::default()
        };
        let mut c = BusCompressor::new(SR);
        c.mix = 0.0;
        let ctx = FxContext::new(SR, 120.0);
        let mut l = [0.5f32; 4096];
        let mut r = [0.5f32; 4096];
        c.process(&p, &mut l, &mut r, &ctx);
        assert!((l[4095] - 0.5).abs() < 1e-4);
    }

    #[test]
    fn detector_high_pass_ignores_bass() {
        let ctx = FxContext::new(SR, 120.0);
        let run = |hpf: f32| {
            let p = BusCompressorParams {
                threshold: -20.0,
                sc_hpf: hpf,
                ..Default::default()
            };
            let mut c = BusCompressor::new(SR);
            let mut l: Vec<f32> = (0..48000)
                .map(|i| 0.8 * (2.0 * std::f32::consts::PI * 30.0 * i as f32 / SR).sin())
                .collect();
            let mut r = l.clone();
            for (cl, cr) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
                c.process(&p, cl, cr, &ctx);
            }
            c.meter().unwrap()
        };
        let off = run(10.0);
        let on = run(400.0);
        assert!(off < -5.0 && on > off + 5.0, "{off} {on}");
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let ctx = FxContext::new(SR, 120.0);
        for preset in BusCompressorParams::presets() {
            let mut c = BusCompressor::new(SR);
            let mut l: Vec<f32> = (0..2048).map(|i| ((i * 7919) % 200) as f32 / 10.0 - 10.0).collect();
            let mut r = l.clone();
            c.process(&preset.1, &mut l, &mut r, &ctx);
            c.process(&preset.1, &mut l[..1], &mut r[..1], &ctx);
            assert!(l.iter().chain(r.iter()).all(|v| v.is_finite()));
        }
    }
}
