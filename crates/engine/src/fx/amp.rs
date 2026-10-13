//! Amp: guitar and bass amplifier models.
//!
//! Each model is an original circuit sketch built from the same blocks: an
//! input high-pass and "bright" pre-emphasis, one to three asymmetric triode
//! style gain stages with coupling high-passes and interstage low-passes, a
//! passive-style tone stack (bass, middle, treble), and a push-pull power
//! stage whose supply sags under load (it clips earlier and compresses when
//! driven), followed by a presence shelf. The nonlinear chain runs at twice
//! the sample rate (see [`super::overdrive::Oversampler2x`]).
//!
//! Pair it with the Cabinet for a complete guitar rig.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::overdrive::{one_pole, tanh_fast, OnePole, Oversampler2x, ShortDelay, OS_LATENCY};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const MODELS: &[&str] = &["Clean", "Boost", "Blues", "Rock", "Lead", "Heavy", "Bass"];

device_params! {
    pub struct AmpParams {
        model: "Amp Type", 0.0, 6.0, 0.0, ParamKind::Choice(MODELS), Unit::Plain;
        gain: "Gain", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        bass: "Bass", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        middle: "Middle", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        treble: "Treble", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        presence: "Presence", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        volume: "Volume", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        output: "Output", -24.0, 12.0, 0.0, LIN, Unit::Db;
        dual_mono: "Dual Mono", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

/// Voicing of one amp model.
#[derive(Debug, Clone, Copy)]
struct Model {
    input_hp: f32,
    bright_hz: f32,
    bright_db: f32,
    gain_min_db: f32,
    gain_range_db: f32,
    stages: usize,
    bias: f32,
    interstage_lp: f32,
    bass_hz: f32,
    mid_hz: f32,
    mid_offset_db: f32,
    treble_hz: f32,
    power_min_db: f32,
    power_range_db: f32,
    sag: f32,
    post_lp: f32,
    level: f32,
}

const MODEL_TABLE: [Model; 7] = [
    // Clean: glassy, lots of headroom.
    Model {
        input_hp: 70.0,
        bright_hz: 2800.0,
        bright_db: 2.5,
        gain_min_db: -6.0,
        gain_range_db: 24.0,
        stages: 1,
        bias: 0.05,
        interstage_lp: 10000.0,
        bass_hz: 110.0,
        mid_hz: 600.0,
        mid_offset_db: -2.0,
        treble_hz: 2600.0,
        power_min_db: -6.0,
        power_range_db: 16.0,
        sag: 0.15,
        post_lp: 9500.0,
        level: 0.9,
    },
    // Boost: a pushed single-channel amp with a treble-boosted input.
    Model {
        input_hp: 100.0,
        bright_hz: 1600.0,
        bright_db: 5.0,
        gain_min_db: 4.0,
        gain_range_db: 28.0,
        stages: 1,
        bias: 0.12,
        interstage_lp: 8500.0,
        bass_hz: 120.0,
        mid_hz: 850.0,
        mid_offset_db: 1.0,
        treble_hz: 2500.0,
        power_min_db: 0.0,
        power_range_db: 18.0,
        sag: 0.25,
        post_lp: 8000.0,
        level: 0.7,
    },
    // Blues: warm, saggy, touch sensitive.
    Model {
        input_hp: 90.0,
        bright_hz: 1000.0,
        bright_db: 3.0,
        gain_min_db: 6.0,
        gain_range_db: 30.0,
        stages: 2,
        bias: 0.16,
        interstage_lp: 6500.0,
        bass_hz: 120.0,
        mid_hz: 700.0,
        mid_offset_db: 2.0,
        treble_hz: 2400.0,
        power_min_db: 4.0,
        power_range_db: 16.0,
        sag: 0.45,
        post_lp: 7000.0,
        level: 0.6,
    },
    // Rock: classic crunch with a mid push.
    Model {
        input_hp: 110.0,
        bright_hz: 1800.0,
        bright_db: 4.0,
        gain_min_db: 12.0,
        gain_range_db: 32.0,
        stages: 2,
        bias: 0.1,
        interstage_lp: 6000.0,
        bass_hz: 120.0,
        mid_hz: 900.0,
        mid_offset_db: 0.0,
        treble_hz: 2600.0,
        power_min_db: 4.0,
        power_range_db: 18.0,
        sag: 0.3,
        post_lp: 6500.0,
        level: 0.55,
    },
    // Lead: three cascaded stages, smooth sustain.
    Model {
        input_hp: 140.0,
        bright_hz: 1200.0,
        bright_db: 6.0,
        gain_min_db: 18.0,
        gain_range_db: 36.0,
        stages: 3,
        bias: 0.12,
        interstage_lp: 5500.0,
        bass_hz: 110.0,
        mid_hz: 1000.0,
        mid_offset_db: 2.0,
        treble_hz: 2800.0,
        power_min_db: 4.0,
        power_range_db: 16.0,
        sag: 0.2,
        post_lp: 6000.0,
        level: 0.5,
    },
    // Heavy: tight low end, scooped mids, high gain.
    Model {
        input_hp: 170.0,
        bright_hz: 2500.0,
        bright_db: 5.0,
        gain_min_db: 24.0,
        gain_range_db: 36.0,
        stages: 3,
        bias: 0.08,
        interstage_lp: 5200.0,
        bass_hz: 100.0,
        mid_hz: 650.0,
        mid_offset_db: -6.0,
        treble_hz: 3000.0,
        power_min_db: 2.0,
        power_range_db: 16.0,
        sag: 0.1,
        post_lp: 6500.0,
        level: 0.5,
    },
    // Bass: full low end, gentle growl.
    Model {
        input_hp: 30.0,
        bright_hz: 900.0,
        bright_db: 2.0,
        gain_min_db: -2.0,
        gain_range_db: 30.0,
        stages: 1,
        bias: 0.1,
        interstage_lp: 5000.0,
        bass_hz: 80.0,
        mid_hz: 500.0,
        mid_offset_db: -1.0,
        treble_hz: 2000.0,
        power_min_db: 0.0,
        power_range_db: 16.0,
        sag: 0.35,
        post_lp: 5500.0,
        level: 0.8,
    },
];

impl AmpParams {
    fn voicing(&self) -> &'static Model {
        &MODEL_TABLE[super::glue::choice(self.model, MODEL_TABLE.len())]
    }
}

impl super::EffectParams for AmpParams {
    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Sparkle Clean",
                Self {
                    model: 0.0,
                    gain: 0.3,
                    treble: 0.65,
                    presence: 0.6,
                    volume: 0.4,
                    ..d
                },
            ),
            (
                "Blues Breaker",
                Self {
                    model: 2.0,
                    gain: 0.55,
                    middle: 0.6,
                    volume: 0.7,
                    ..d
                },
            ),
            (
                "Modern Metal",
                Self {
                    model: 5.0,
                    gain: 0.75,
                    bass: 0.6,
                    middle: 0.35,
                    treble: 0.6,
                    presence: 0.65,
                    volume: 0.45,
                    output: -3.0,
                    ..d
                },
            ),
            (
                "Growl Bass",
                Self {
                    model: 6.0,
                    gain: 0.45,
                    bass: 0.65,
                    middle: 0.55,
                    treble: 0.4,
                    volume: 0.6,
                    ..d
                },
            ),
        ]
    }
}

/// Filter coefficients for one model and knob setting (at 2x the rate).
#[derive(Clone, Copy)]
struct AmpCoefs {
    input_hp: f32,
    bright: Coefs,
    coupling: f32,
    interstage: f32,
    tone: [Coefs; 3],
    presence: Coefs,
    post_lp: f32,
    sag_att: f32,
    sag_rel: f32,
}

impl AmpCoefs {
    fn new(p: &AmpParams, sr2: f32) -> Self {
        let m = p.voicing();
        let knob = |v: f32, range: f32| (v.clamp(0.0, 1.0) - 0.5) * 2.0 * range;
        Self {
            input_hp: one_pole(m.input_hp, sr2),
            bright: Coefs::design(FilterShape::HighShelf, m.bright_hz, 0.6, m.bright_db, sr2),
            coupling: one_pole(25.0, sr2),
            interstage: one_pole(m.interstage_lp, sr2),
            tone: [
                Coefs::design(FilterShape::LowShelf, m.bass_hz, 0.6, knob(p.bass, 12.0), sr2),
                Coefs::design(
                    FilterShape::Bell,
                    m.mid_hz,
                    0.7,
                    knob(p.middle, 10.0) + m.mid_offset_db,
                    sr2,
                ),
                Coefs::design(FilterShape::HighShelf, m.treble_hz, 0.6, knob(p.treble, 12.0), sr2),
            ],
            presence: Coefs::design(FilterShape::HighShelf, 4200.0, 0.7, knob(p.presence, 8.0), sr2),
            post_lp: one_pole(m.post_lp, sr2),
            sag_att: super::smoothing(0.004, sr2),
            sag_rel: super::smoothing(0.15, sr2),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Channel {
    input_hp: OnePole,
    bright: Biquad,
    coupling: [OnePole; 3],
    interstage: [OnePole; 3],
    tone: [Biquad; 3],
    presence: Biquad,
    post: OnePole,
    out_dc: OnePole,
    sag_env: f32,
}

impl Channel {
    /// The whole amp at the oversampled rate.
    #[inline]
    fn tick(&mut self, x: f32, m: &Model, c: &AmpCoefs, stage_gain: f32, power_gain: f32) -> f32 {
        let mut s = self.input_hp.hp(x, c.input_hp);
        s = self.bright.process(&c.bright, s);
        let bias = m.bias;
        let offset = tanh_fast(bias);
        for st in 0..m.stages.min(3) {
            let u = s * stage_gain + bias;
            // Triode-like: the positive half clips softer than the negative.
            let y = if u > 0.0 {
                tanh_fast(u)
            } else {
                0.8 * tanh_fast(u * 1.25)
            };
            s = self.coupling[st].hp(y - offset, c.coupling);
            s = self.interstage[st].lp(s, c.interstage);
        }
        for (f, k) in self.tone.iter_mut().zip(c.tone.iter()) {
            s = f.process(k, s);
        }
        // Power stage with supply sag: the ceiling drops under load.
        let headroom = 1.0 / (1.0 + m.sag * 3.0 * self.sag_env);
        let y = headroom * tanh_fast(s * power_gain / headroom);
        let a = y.abs();
        let coef = if a > self.sag_env { c.sag_att } else { c.sag_rel };
        self.sag_env = a + (self.sag_env - a) * coef;
        let y = self.presence.process(&c.presence, y);
        let y = self.post.lp(y, c.post_lp);
        self.out_dc.hp(y, c.coupling)
    }
}

pub struct Amp {
    sr: f32,
    os: [Oversampler2x; 2],
    dry: [ShortDelay; 2],
    ch: [Channel; 2],
    coefs: AmpCoefs,
    key: [f32; 5],
    stage_gain: f32,
    power_gain: f32,
    out: f32,
    mix: f32,
}

impl Amp {
    pub fn new(sample_rate: f32) -> Self {
        let p = AmpParams::default();
        Self {
            sr: sample_rate,
            os: [Oversampler2x::new(); 2],
            dry: [ShortDelay::new(); 2],
            ch: [Channel::default(); 2],
            coefs: AmpCoefs::new(&p, sample_rate * 2.0),
            key: [p.model, p.bass, p.middle, p.treble, p.presence],
            stage_gain: 1.0,
            power_gain: 1.0,
            out: 1.0,
            mix: 1.0,
        }
    }

    pub fn reset(&mut self) {
        for o in &mut self.os {
            o.reset();
        }
        for d in &mut self.dry {
            d.reset();
        }
        self.ch = [Channel::default(); 2];
    }

    pub fn process(&mut self, p: &AmpParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let key = [p.model, p.bass, p.middle, p.treble, p.presence];
        if key != self.key {
            self.key = key;
            self.coefs = AmpCoefs::new(p, self.sr * 2.0);
        }
        let m = p.voicing();
        let c = self.coefs;
        let total_db = m.gain_min_db + p.gain.clamp(0.0, 1.0) * m.gain_range_db;
        let stage_t = super::db(total_db / m.stages.max(1) as f32);
        let power_t = super::db(m.power_min_db + p.volume.clamp(0.0, 1.0) * m.power_range_db);
        let out_t = super::db(p.output.clamp(-24.0, 12.0)) * m.level;
        let mix_t = p.mix.clamp(0.0, 1.0);
        let smooth = super::smoothing(0.01, self.sr);
        let dual = p.dual_mono >= 0.5;
        for i in 0..left.len().min(right.len()) {
            self.stage_gain = stage_t + (self.stage_gain - stage_t) * smooth;
            self.power_gain = power_t + (self.power_gain - power_t) * smooth;
            self.out = out_t + (self.out - out_t) * smooth;
            self.mix = mix_t + (self.mix - mix_t) * smooth;
            let (sg, pg) = (self.stage_gain, self.power_gain);
            let (xl, xr) = (left[i], right[i]);
            let (wl, wr) = if dual {
                let ch = &mut self.ch;
                let (c0, c1) = ch.split_at_mut(1);
                let wl = self.os[0].process(xl, |s| c0[0].tick(s, m, &c, sg, pg));
                let wr = self.os[1].process(xr, |s| c1[0].tick(s, m, &c, sg, pg));
                (wl, wr)
            } else {
                let ch = &mut self.ch[0];
                let w = self.os[0].process(0.5 * (xl + xr), |s| ch.tick(s, m, &c, sg, pg));
                (w, w)
            };
            let dl = self.dry[0].process(xl, OS_LATENCY);
            let dr = self.dry[1].process(xr, OS_LATENCY);
            left[i] = dl + (wl * self.out - dl) * self.mix;
            right[i] = dr + (wr * self.out - dr) * self.mix;
        }
    }
}

impl super::EffectRuntime for Amp {}

#[cfg(test)]
mod tests {
    use super::super::overdrive::tests::tone_level;
    use super::super::EffectParams;
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48000.0;

    fn sine(f: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * f * i as f32 / SR).sin()).collect()
    }

    fn run(p: &AmpParams, l: &[f32], r: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let ctx = FxContext::new(SR, 120.0);
        let mut a = Amp::new(SR);
        let mut l = l.to_vec();
        let mut r = r.to_vec();
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            a.process(p, cl, cr, &ctx);
        }
        (l, r)
    }

    fn thd(y: &[f32], f: f32) -> f32 {
        let fund = tone_level(y, f, SR);
        let h: f32 = (2..6)
            .map(|k| tone_level(y, f * k as f32, SR).powi(2))
            .sum::<f32>()
            .sqrt();
        h / fund.max(1e-9)
    }

    #[test]
    fn models_differ_and_stay_bounded() {
        let x = sine(196.0, 0.4, 24000);
        let mut outs = Vec::new();
        let mut waves: Vec<Vec<f32>> = Vec::new();
        for m in 0..MODELS.len() {
            let p = AmpParams {
                model: m as f32,
                ..Default::default()
            };
            let (y, _) = run(&p, &x, &x);
            assert!(y.iter().all(|v| v.is_finite() && v.abs() < 3.0), "model {m}");
            assert!(tone_level(&y[8000..], 196.0, SR) > 0.02, "model {m} is silent");
            outs.push(thd(&y[8000..], 196.0));
            waves.push(y[8000..].to_vec());
        }
        // Clean is the cleanest, Heavy and Lead the most distorted.
        assert!(outs[0] < outs[5] && outs[0] < outs[4], "{outs:?}");
        for i in 0..waves.len() {
            for j in i + 1..waves.len() {
                let diff: f32 = waves[i].iter().zip(&waves[j]).map(|(a, b)| (a - b).powi(2)).sum();
                assert!(diff / waves[i].len() as f32 > 1e-4, "models {i} and {j} sound alike");
            }
        }
    }

    #[test]
    fn gain_increases_distortion() {
        let x = sine(220.0, 0.03, 24000);
        let lo = AmpParams {
            model: 2.0,
            gain: 0.0,
            ..Default::default()
        };
        let hi = AmpParams { gain: 1.0, ..lo };
        let a = thd(&run(&lo, &x, &x).0[8000..], 220.0);
        let b = thd(&run(&hi, &x, &x).0[8000..], 220.0);
        assert!(b > a * 1.5, "{a} {b}");
    }

    #[test]
    fn tone_controls_shape_the_spectrum() {
        let x = sine(80.0, 0.05, 24000);
        let p = AmpParams::default();
        let flat = tone_level(&run(&p, &x, &x).0[8000..], 80.0, SR);
        let boosted = tone_level(&run(&AmpParams { bass: 1.0, ..p }, &x, &x).0[8000..], 80.0, SR);
        assert!(boosted > flat * 1.5, "{flat} {boosted}");
        let x = sine(6000.0, 0.05, 24000);
        let flat = tone_level(&run(&p, &x, &x).0[8000..], 6000.0, SR);
        let bright = tone_level(&run(&AmpParams { presence: 1.0, ..p }, &x, &x).0[8000..], 6000.0, SR);
        assert!(bright > flat * 1.5, "{flat} {bright}");
    }

    #[test]
    fn sag_compresses_loud_input() {
        let p = AmpParams {
            model: 2.0,
            gain: 0.0,
            volume: 1.0,
            ..Default::default()
        };
        let q = tone_level(&run(&p, &sine(220.0, 0.05, 24000), &[0.0; 24000]).0[8000..], 220.0, SR);
        let l = tone_level(&run(&p, &sine(220.0, 0.8, 24000), &[0.0; 24000]).0[8000..], 220.0, SR);
        // 24 dB more input gives much less than 24 dB more output.
        assert!(l / q < 6.0, "{q} {l}");
    }

    #[test]
    fn stereo_modes_and_dry_mix() {
        let xl = sine(220.0, 0.3, 12000);
        let xr = sine(330.0, 0.3, 12000);
        let (l, r) = run(&AmpParams::default(), &xl, &xr);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-6));
        let (l, r) = run(
            &AmpParams {
                dual_mono: 1.0,
                ..Default::default()
            },
            &xl,
            &xr,
        );
        assert!(tone_level(&l[4000..], 330.0, SR) < 0.01 && tone_level(&r[4000..], 330.0, SR) > 0.05);
        let (l, _) = run(
            &AmpParams {
                mix: 0.0,
                ..Default::default()
            },
            &xl,
            &xr,
        );
        for i in 9600..12000 {
            assert!((l[i] - xl[i - OS_LATENCY]).abs() < 1e-3);
        }
    }

    #[test]
    fn presets_stay_finite() {
        let ctx = FxContext::new(SR, 120.0);
        for (_, p) in AmpParams::presets() {
            for dual in [0.0, 1.0] {
                let p = AmpParams { dual_mono: dual, ..p };
                let mut a = Amp::new(SR);
                let mut l: Vec<f32> = (0..2048).map(|i| ((i * 13) % 50) as f32 - 25.0).collect();
                let mut r = l.clone();
                a.process(&p, &mut l, &mut r, &ctx);
                a.process(&p, &mut l[..1], &mut r[..1], &ctx);
                assert!(l.iter().chain(r.iter()).all(|v| v.is_finite() && v.abs() < 20.0));
            }
        }
    }
}
