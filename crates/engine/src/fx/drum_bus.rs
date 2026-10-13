//! Drum Bus: an all-in-one processor for drums.
//!
//! Signal flow: Trim, an optional fixed fast bus compressor, Drive (three
//! curves, oversampled), Crunch (extra distortion of the mid-high band),
//! Damp (low-pass), a transient shaper and Boom, a tuned low-frequency
//! resonator that is triggered by kick-like transients in the input and
//! adds a decaying, slightly pitch-dropping sine underneath the drums.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::overdrive::{cubic_clip, one_pole, tanh_fast, OnePole, Oversampler2x, ShortDelay, OS_LATENCY};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use std::f32::consts::{FRAC_1_SQRT_2, TAU};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const DRIVE_TYPES: &[&str] = &["Soft", "Medium", "Hard"];

device_params! {
    pub struct DrumBusParams {
        drive: "Drive", 0.0, 1.0, 0.2, LIN, Unit::Percent;
        drive_type: "Drive Type", 0.0, 2.0, 0.0, ParamKind::Choice(DRIVE_TYPES), Unit::Plain;
        crunch: "Crunch", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        damp: "Damp", 1000.0, 20000.0, 20000.0, LOG, Unit::Hz;
        transients: "Transients", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        boom: "Boom", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        boom_freq: "Boom Freq", 30.0, 90.0, 50.0, LIN, Unit::Hz;
        boom_decay: "Boom Decay", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        compressor: "Compressor", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        trim: "Trim", -24.0, 6.0, 0.0, LIN, Unit::Db;
        output: "Output", -24.0, 12.0, 0.0, LIN, Unit::Db;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl DrumBusParams {
    /// Boom amplitude decay time constant in seconds.
    pub fn boom_decay_s(&self) -> f32 {
        0.05 * 40f32.powf(self.boom_decay.clamp(0.0, 1.0))
    }
}

impl super::EffectParams for DrumBusParams {
    fn is_relevant(&self, index: usize) -> bool {
        match index {
            6 | 7 => self.boom > 0.0,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Punchy Kit",
                Self {
                    drive: 0.3,
                    transients: 0.4,
                    compressor: 1.0,
                    damp: 14000.0,
                    ..d
                },
            ),
            (
                "Boom Bap",
                Self {
                    drive: 0.4,
                    drive_type: 1.0,
                    crunch: 0.3,
                    boom: 0.5,
                    boom_freq: 55.0,
                    boom_decay: 0.45,
                    damp: 9000.0,
                    ..d
                },
            ),
            (
                "Trashed",
                Self {
                    drive: 0.85,
                    drive_type: 2.0,
                    crunch: 0.7,
                    compressor: 1.0,
                    damp: 7000.0,
                    output: -6.0,
                    mix: 0.7,
                    ..d
                },
            ),
            (
                "Room Tamer",
                Self {
                    drive: 0.1,
                    transients: -0.5,
                    damp: 12000.0,
                    ..d
                },
            ),
        ]
    }
}

/// Fixed bus compressor settings.
const COMP_THRESHOLD_DB: f32 = -14.0;
const COMP_SLOPE: f32 = 0.75; // 4:1
const COMP_KNEE_DB: f32 = 6.0;
const COMP_MAKEUP_DB: f32 = 4.0;

#[derive(Clone, Copy, Default)]
struct DriveChannel {
    crunch_hp: OnePole,
    dc: OnePole,
    damp: [Biquad; 2],
}

pub struct DrumBus {
    sr: f32,
    os: [Oversampler2x; 2],
    dry: [ShortDelay; 2],
    det_delay: [ShortDelay; 2],
    ch: [DriveChannel; 2],
    damp: Coefs,
    damp_freq: f32,
    // Compressor.
    comp_db: f32,
    // Transient shaper envelopes.
    t_fast: f32,
    t_slow: f32,
    // Boom detector and oscillator.
    boom_lp: [Biquad; 2],
    boom_lp_coefs: Coefs,
    b_fast: f32,
    b_slow: f32,
    armed: bool,
    /// Samples left in the current hit's attack window.
    hit_left: u32,
    osc_phase: f32,
    osc_amp: f32,
    osc_glide: f32,
    resonator: crate::dsp::filter::Svf,
    // Smoothed gains.
    trim: f32,
    drive: f32,
    out: f32,
    mix: f32,
    boom: f32,
    meter_db: f32,
    comp_on: bool,
}

impl DrumBus {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            os: [Oversampler2x::new(); 2],
            dry: [ShortDelay::new(); 2],
            det_delay: [ShortDelay::new(); 2],
            ch: [DriveChannel::default(); 2],
            damp: Coefs::IDENTITY,
            damp_freq: 0.0,
            comp_db: 0.0,
            t_fast: 0.0,
            t_slow: 0.0,
            boom_lp: [Biquad::default(); 2],
            boom_lp_coefs: Coefs::design(FilterShape::LowPass, 150.0, FRAC_1_SQRT_2, 0.0, sample_rate),
            b_fast: 0.0,
            b_slow: 0.0,
            armed: true,
            hit_left: 0,
            osc_phase: 0.0,
            osc_amp: 0.0,
            osc_glide: 0.0,
            resonator: Default::default(),
            trim: 1.0,
            drive: 1.0,
            out: 1.0,
            mix: 1.0,
            boom: 0.0,
            meter_db: 0.0,
            comp_on: false,
        }
    }

    pub fn reset(&mut self) {
        for o in &mut self.os {
            o.reset();
        }
        for d in self.dry.iter_mut().chain(self.det_delay.iter_mut()) {
            d.reset();
        }
        self.ch = [DriveChannel::default(); 2];
        self.boom_lp = [Biquad::default(); 2];
        self.comp_db = 0.0;
        self.t_fast = 0.0;
        self.t_slow = 0.0;
        self.b_fast = 0.0;
        self.b_slow = 0.0;
        self.armed = true;
        self.hit_left = 0;
        self.osc_amp = 0.0;
        self.resonator.reset();
        self.meter_db = 0.0;
    }

    fn comp_gain_db(level_db: f32) -> f32 {
        let over = level_db - COMP_THRESHOLD_DB;
        if over.abs() <= COMP_KNEE_DB * 0.5 {
            let x = over + COMP_KNEE_DB * 0.5;
            -COMP_SLOPE * x * x / (2.0 * COMP_KNEE_DB)
        } else if over > 0.0 {
            -COMP_SLOPE * over
        } else {
            0.0
        }
    }

    pub fn process(&mut self, p: &DrumBusParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let sr = self.sr;
        let sr2 = sr * 2.0;
        let damp_f = p.damp.clamp(1000.0, 20000.0);
        let damp_on = damp_f < 19900.0;
        if damp_on && (damp_f - self.damp_freq).abs() > 0.5 {
            self.damp_freq = damp_f;
            self.damp = Coefs::design(FilterShape::LowPass, damp_f, FRAC_1_SQRT_2, 0.0, sr);
        }
        let smooth = super::smoothing(0.01, sr);
        let kind = super::glue::choice(p.drive_type, DRIVE_TYPES.len());
        let drive_amt = p.drive.clamp(0.0, 1.0);
        let drive_t = super::db(drive_amt * 30.0);
        // Partial level compensation so more drive is denser, not much louder.
        let comp_t = super::db(-drive_amt * 18.0);
        let crunch = p.crunch.clamp(0.0, 1.0);
        let a_crunch = one_pole(2500.0, sr2);
        let a_dc = one_pole(10.0, sr2);
        let comp_on = p.compressor >= 0.5;
        let c_att = super::smoothing(0.001, sr);
        let c_rel = super::smoothing(0.08, sr);
        let transients = p.transients.clamp(-1.0, 1.0);
        let tf_r = super::smoothing(0.03, sr);
        let (ts_a, ts_r) = (super::smoothing(0.025, sr), super::smoothing(0.25, sr));
        let (bf_a, bf_r) = (super::smoothing(0.001, sr), super::smoothing(0.02, sr));
        let (bs_a, bs_r) = (super::smoothing(0.03, sr), super::smoothing(0.3, sr));
        let boom_t = p.boom.clamp(0.0, 1.0);
        let boom_freq = p.boom_freq.clamp(30.0, 90.0);
        let osc_decay = super::smoothing(p.boom_decay_s(), sr);
        let glide = super::smoothing(0.015, sr);
        let res_q = 2.0 + 8.0 * p.boom_decay.clamp(0.0, 1.0);
        let trim_t = super::db(p.trim.clamp(-24.0, 6.0));
        let out_t = super::db(p.output.clamp(-24.0, 12.0));
        let mix_t = p.mix.clamp(0.0, 1.0);
        let damp = self.damp;
        let mut most = 0.0f32;
        for i in 0..left.len().min(right.len()) {
            self.trim = trim_t + (self.trim - trim_t) * smooth;
            self.drive = drive_t + (self.drive - drive_t) * smooth;
            self.out = out_t + (self.out - out_t) * smooth;
            self.mix = mix_t + (self.mix - mix_t) * smooth;
            self.boom = boom_t + (self.boom - boom_t) * smooth;
            let (xl, xr) = (left[i], right[i]);
            let (mut sl, mut sr_) = (xl * self.trim, xr * self.trim);

            // Fixed bus compressor.
            if comp_on {
                let peak = sl.abs().max(sr_.abs());
                let lvl = if peak > 1e-6 { 20.0 * peak.log10() } else { -120.0 };
                let target = Self::comp_gain_db(lvl);
                let c = if target < self.comp_db { c_att } else { c_rel };
                self.comp_db = target + (self.comp_db - target) * c;
                most = most.min(self.comp_db);
                let g = super::db(self.comp_db + COMP_MAKEUP_DB);
                sl *= g;
                sr_ *= g;
            } else {
                self.comp_db = 0.0;
            }

            // Drive and crunch, oversampled.
            let drive = self.drive;
            let mut wet = [0.0f32; 2];
            for c in 0..2 {
                let x = if c == 0 { sl } else { sr_ };
                let ch = &mut self.ch[c];
                let y = self.os[c].process(x, |s| {
                    let u = s * drive;
                    let mut y = match kind {
                        0 => tanh_fast(u),
                        1 => {
                            let v = cubic_clip(u);
                            ch.dc.hp(v + 0.15 * v * v, a_dc)
                        }
                        _ => u.clamp(-1.0, 1.0),
                    };
                    if crunch > 0.0 {
                        let h = ch.crunch_hp.hp(y, a_crunch);
                        y += crunch * (0.4 * tanh_fast(h * 10.0) - h);
                    }
                    y
                });
                let mut y = y * comp_t;
                if damp_on {
                    y = ch.damp[0].process(&damp, y);
                    y = ch.damp[1].process(&damp, y);
                }
                wet[c] = y;
            }

            // Transient shaper (stereo linked).
            if transients.abs() > 1e-3 {
                let peak = wet[0].abs().max(wet[1].abs());
                // The fast envelope jumps instantly so onsets are never
                // mistaken for sustain.
                self.t_fast = if peak > self.t_fast {
                    peak
                } else {
                    peak + (self.t_fast - peak) * tf_r
                };
                let c = if peak > self.t_slow { ts_a } else { ts_r };
                self.t_slow = peak + (self.t_slow - peak) * c;
                if self.t_slow > 1e-3 {
                    let diff = 20.0 * (self.t_fast.max(1e-9) / self.t_slow).log10();
                    let g = super::db((transients * diff.clamp(-24.0, 24.0)).clamp(-12.0, 12.0));
                    wet[0] *= g;
                    wet[1] *= g;
                }
            }

            // Boom: detect kicks on the (aligned) low end of the input.
            let dl = self.det_delay[0].process(xl, OS_LATENCY);
            let dr = self.det_delay[1].process(xr, OS_LATENCY);
            if self.boom > 1e-4 {
                let mono = 0.5 * (dl + dr) * self.trim;
                let lo = self.boom_lp[0].process(&self.boom_lp_coefs, mono);
                let lo = self.boom_lp[1].process(&self.boom_lp_coefs, lo);
                let a = lo.abs();
                let c = if a > self.b_fast { bf_a } else { bf_r };
                self.b_fast = a + (self.b_fast - a) * c;
                let c = if a > self.b_slow { bs_a } else { bs_r };
                self.b_slow = a + (self.b_slow - a) * c;
                if self.armed && self.b_fast > 0.003 && self.b_fast > self.b_slow * 1.6 {
                    self.armed = false;
                    self.hit_left = (0.03 * sr) as u32;
                    if self.osc_amp < 0.01 {
                        self.osc_phase = 0.0;
                    }
                    self.osc_glide = 1.0;
                } else if !self.armed && self.hit_left == 0 && self.b_fast < self.b_slow * 1.2 {
                    self.armed = true;
                }
                // Follow the hit's level through its attack.
                if self.hit_left > 0 {
                    self.hit_left -= 1;
                    self.osc_amp = self.osc_amp.max((self.b_fast * 1.5).min(1.0));
                }
                // A short downward pitch glide gives the hit some punch.
                let f = boom_freq * (1.0 + 0.5 * self.osc_glide);
                self.osc_glide *= glide;
                self.osc_phase += TAU * f / sr;
                if self.osc_phase > TAU {
                    self.osc_phase -= TAU;
                }
                let osc = self.osc_amp * self.osc_phase.sin();
                self.osc_amp *= osc_decay;
                if self.osc_amp < 1e-7 {
                    self.osc_amp = 0.0;
                }
                let res = self.resonator.process(lo, boom_freq, res_q, sr).band;
                let b = self.boom * (osc + 0.5 * res);
                wet[0] += b;
                wet[1] += b;
            }

            let d0 = self.dry[0].process(xl, OS_LATENCY);
            let d1 = self.dry[1].process(xr, OS_LATENCY);
            left[i] = d0 + (wet[0] * self.out - d0) * self.mix;
            right[i] = d1 + (wet[1] * self.out - d1) * self.mix;
        }
        self.meter_db = most;
        self.comp_on = comp_on;
    }
}

impl super::EffectRuntime for DrumBus {
    fn meter(&self) -> Option<f32> {
        self.comp_on.then_some(self.meter_db)
    }
}

#[cfg(test)]
mod tests {
    use super::super::overdrive::tests::tone_level;
    use super::super::{EffectParams, EffectRuntime};
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48000.0;

    fn run(p: &DrumBusParams, x: &[f32]) -> (DrumBus, Vec<f32>) {
        let ctx = FxContext::new(SR, 120.0);
        let mut d = DrumBus::new(SR);
        let mut l = x.to_vec();
        let mut r = x.to_vec();
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            d.process(p, cl, cr, &ctx);
        }
        (d, l)
    }

    /// Decaying noise bursts, like a drum loop (4 hits a second).
    fn drum_loop(n: usize) -> Vec<f32> {
        let mut rng = crate::dsp::Rng::new(9);
        (0..n)
            .map(|i| {
                let t = (i % 12000) as f32 / SR;
                0.8 * rng.next_bipolar() * (-t / 0.04).exp()
            })
            .collect()
    }

    fn sine(f: f32, a: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| a * (2.0 * PI * f * i as f32 / SR).sin()).collect()
    }

    #[test]
    fn drive_types_add_harmonics() {
        let x = sine(200.0, 0.5, 24000);
        let clean = DrumBusParams {
            drive: 0.0,
            drive_type: 0.0,
            ..Default::default()
        };
        let (_, y) = run(&clean, &x);
        let base = tone_level(&y[4000..], 600.0, SR);
        for kind in 0..3 {
            let p = DrumBusParams {
                drive: 0.8,
                drive_type: kind as f32,
                ..Default::default()
            };
            let (_, y) = run(&p, &x);
            let h3 = tone_level(&y[4000..], 600.0, SR);
            assert!(h3 > base + 0.02, "type {kind}: {base} {h3}");
            assert!(y.iter().all(|v| v.is_finite() && v.abs() < 3.0));
        }
        // Medium is asymmetric: second harmonic.
        let p = DrumBusParams {
            drive: 0.6,
            drive_type: 1.0,
            ..Default::default()
        };
        assert!(tone_level(&run(&p, &x).1[4000..], 400.0, SR) > 0.01);
    }

    #[test]
    fn damp_cuts_highs() {
        let x = sine(10000.0, 0.1, 24000);
        let p = DrumBusParams {
            drive: 0.0,
            ..Default::default()
        };
        let open = tone_level(&run(&p, &x).1[4000..], 10000.0, SR);
        let damped = tone_level(&run(&DrumBusParams { damp: 2000.0, ..p }, &x).1[4000..], 10000.0, SR);
        assert!(damped < open * 0.1, "{open} {damped}");
    }

    #[test]
    fn transients_shape_attack_vs_sustain() {
        let x = drum_loop(48000);
        let crest = |y: &[f32]| {
            let peak = y.iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let rms = (y.iter().map(|v| v * v).sum::<f32>() / y.len() as f32).sqrt();
            peak / rms
        };
        let base = DrumBusParams {
            drive: 0.0,
            ..Default::default()
        };
        let neutral = crest(&run(&base, &x).1[12000..]);
        let more = crest(
            &run(
                &DrumBusParams {
                    transients: 1.0,
                    ..base
                },
                &x,
            )
            .1[12000..],
        );
        let less = crest(
            &run(
                &DrumBusParams {
                    transients: -1.0,
                    ..base
                },
                &x,
            )
            .1[12000..],
        );
        assert!(more > neutral * 1.1 && less < neutral * 0.9, "{less} {neutral} {more}");
    }

    #[test]
    fn boom_rings_at_its_frequency_after_a_kick() {
        // A short low thump, then silence.
        let mut x = vec![0.0f32; 48000];
        for (i, v) in x.iter_mut().take(1200).enumerate() {
            *v = 0.8 * (2.0 * PI * 70.0 * i as f32 / SR).sin() * (-(i as f32) / 300.0).exp();
        }
        let p = DrumBusParams {
            drive: 0.0,
            boom: 1.0,
            boom_freq: 45.0,
            boom_decay: 0.3,
            ..Default::default()
        };
        let (_, y) = run(&p, &x);
        let tail = &y[4800..14400];
        let at = tone_level(tail, 45.0, SR);
        let off = tone_level(tail, 90.0, SR);
        assert!(at > 0.05 && at > off * 3.0, "{at} {off}");
        let (_, dry) = run(&DrumBusParams { boom: 0.0, ..p }, &x);
        assert!(tone_level(&dry[4800..14400], 45.0, SR) < at * 0.1);
        // Eventually it dies away.
        assert!(y[47000..].iter().all(|v| v.abs() < 0.02));
    }

    #[test]
    fn compressor_reduces_level_and_meters() {
        let x = sine(300.0, 0.9, 24000);
        let p = DrumBusParams {
            drive: 0.0,
            compressor: 1.0,
            ..Default::default()
        };
        let (d, y) = run(&p, &x);
        assert!(d.meter().unwrap() < -5.0);
        let (d2, y2) = run(&DrumBusParams { compressor: 0.0, ..p }, &x);
        assert!(d2.meter().is_none());
        assert!(tone_level(&y[8000..], 300.0, SR) < tone_level(&y2[8000..], 300.0, SR));
    }

    #[test]
    fn dry_mix_is_the_aligned_input() {
        let x = drum_loop(12000);
        let p = DrumBusParams {
            mix: 0.0,
            drive: 1.0,
            boom: 1.0,
            ..Default::default()
        };
        let ctx = FxContext::new(SR, 120.0);
        let mut d = DrumBus::new(SR);
        d.mix = 0.0;
        let mut l = x.clone();
        let mut r = x.clone();
        d.process(&p, &mut l, &mut r, &ctx);
        for i in OS_LATENCY..12000 {
            assert!((l[i] - x[i - OS_LATENCY]).abs() < 1e-5);
        }
    }

    #[test]
    fn presets_stay_finite() {
        let ctx = FxContext::new(SR, 120.0);
        for (_, p) in DrumBusParams::presets() {
            let mut d = DrumBus::new(SR);
            let mut l: Vec<f32> = (0..2048).map(|i| ((i * 37) % 64) as f32 - 32.0).collect();
            let mut r = l.clone();
            d.process(&p, &mut l, &mut r, &ctx);
            d.process(&p, &mut l[..1], &mut r[..1], &ctx);
            assert!(l.iter().all(|v| v.is_finite() && v.abs() < 200.0));
        }
    }
}
