//! Vinyl Distortion: the wear and tear of records.
//!
//! * **Tracing model**: a stylus that cannot follow the groove adds
//!   even-order harmonics. A band of the signal (frequency and width) is fed
//!   through an even waveshaper; its DC-free output is added back.
//! * **Pinch**: a groove that narrows makes odd-order harmonics that come out
//!   of phase between the two channels. A band of the mid signal is fed
//!   through an odd waveshaper and its distortion is added to the left
//!   channel and subtracted from the right.
//! * **Crackle**: random impulses with decaying noise tails, band-passed,
//!   independent per channel (Stereo) or shared (Mono).
//!
//! *Drive* scales the signal into both shapers; *Soft* and *Hard* select the
//! shaper curves.

use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::dsp::filter::Svf;
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

device_params! {
    pub struct VinylParams {
        tracing_on: "Tracing", 0.0, 1.0, 1.0, ParamKind::Toggle, Unit::Plain;
        tracing_drive: "Tracing Drive", 0.0, 1.0, 0.4, LIN, Unit::Percent;
        tracing_freq: "Tracing Freq", 50.0, 18000.0, 2500.0, LOG, Unit::Hz;
        tracing_width: "Tracing Width", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        tracing_amount: "Tracing Amount", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        pinch_on: "Pinch", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        pinch_freq: "Pinch Freq", 50.0, 18000.0, 1200.0, LOG, Unit::Hz;
        pinch_width: "Pinch Width", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        pinch_amount: "Pinch Amount", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        drive: "Drive", -12.0, 24.0, 0.0, LIN, Unit::Db;
        mode: "Mode", 0.0, 1.0, 0.0, ParamKind::Choice(&["Soft", "Hard"]), Unit::Plain;
        crackle: "Crackle", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        density: "Density", 0.1, 100.0, 6.0, LOG, Unit::Hz;
        crackle_stereo: "Crackle Mode", 0.0, 1.0, 1.0, ParamKind::Choice(&["Mono", "Stereo"]), Unit::Plain;
    }
}

impl super::EffectParams for VinylParams {
    fn is_relevant(&self, index: usize) -> bool {
        match index {
            1..=4 => self.tracing_on >= 0.5,
            6..=8 => self.pinch_on >= 0.5,
            12 | 13 => self.crackle > 0.0,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Dusty Record",
                Self {
                    tracing_amount: 0.25,
                    crackle: 0.35,
                    density: 4.0,
                    ..d
                },
            ),
            (
                "Worn 78",
                Self {
                    tracing_drive: 0.8,
                    tracing_freq: 1800.0,
                    tracing_amount: 0.6,
                    pinch_on: 1.0,
                    pinch_amount: 0.5,
                    drive: 6.0,
                    mode: 1.0,
                    crackle: 0.6,
                    density: 25.0,
                    crackle_stereo: 0.0,
                    ..d
                },
            ),
            (
                "Crackle Only",
                Self {
                    tracing_on: 0.0,
                    crackle: 0.5,
                    density: 12.0,
                    ..d
                },
            ),
            (
                "Pinched Highs",
                Self {
                    tracing_on: 0.0,
                    pinch_on: 1.0,
                    pinch_freq: 5000.0,
                    pinch_width: 0.7,
                    pinch_amount: 0.7,
                    drive: 9.0,
                    ..d
                },
            ),
        ]
    }
}

/// Q for a band `width` (0..1 -> 0.2 .. 4 octaves) wide.
fn band_q(width: f32) -> f32 {
    let r = 2f32.powf(0.2 + width.clamp(0.0, 1.0) * 3.8);
    (r.sqrt() / (r - 1.0)).max(0.3)
}

/// One-pole DC blocker.
#[derive(Default, Clone, Copy)]
struct DcBlock {
    x1: f32,
    y1: f32,
}

impl DcBlock {
    #[inline]
    fn process(&mut self, x: f32, r: f32) -> f32 {
        let y = x - self.x1 + r * self.y1;
        self.x1 = x;
        self.y1 = if y.abs() < 1e-20 { 0.0 } else { y };
        y
    }
}

pub struct VinylDistortion {
    sr: f32,
    rng: Rng,
    tracing: [Svf; 2],
    tracing_dc: [DcBlock; 2],
    pinch: Svf,
    crackle_env: [f32; 2],
    crackle_filt: [Svf; 2],
}

impl VinylDistortion {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            rng: Rng::new(0x7a1e_c0de),
            tracing: Default::default(),
            tracing_dc: Default::default(),
            pinch: Svf::default(),
            crackle_env: [0.0; 2],
            crackle_filt: Default::default(),
        }
    }

    pub fn reset(&mut self) {
        for f in self.tracing.iter_mut().chain(self.crackle_filt.iter_mut()) {
            f.reset();
        }
        self.pinch.reset();
        self.tracing_dc = Default::default();
        self.crackle_env = [0.0; 2];
    }

    fn uniform(&mut self) -> f32 {
        self.rng.next_bipolar() * 0.5 + 0.5
    }

    pub fn process(&mut self, p: &VinylParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let drive = super::db(p.drive.clamp(-12.0, 24.0));
        let hard = p.mode >= 0.5;
        let tracing_on = p.tracing_on >= 0.5;
        let tg = drive * (1.0 + 9.0 * p.tracing_drive.clamp(0.0, 1.0));
        let t_amt = p.tracing_amount.clamp(0.0, 1.0);
        let t_freq = p.tracing_freq.clamp(20.0, 20000.0);
        let t_q = band_q(p.tracing_width);
        let pinch_on = p.pinch_on >= 0.5;
        let pg = drive * 3.0;
        let p_amt = p.pinch_amount.clamp(0.0, 1.0) * 2.0;
        let p_freq = p.pinch_freq.clamp(20.0, 20000.0);
        let p_q = band_q(p.pinch_width);
        let dc_r = 1.0 - 2.0 * std::f32::consts::PI * 20.0 / self.sr;
        let crackle = p.crackle.clamp(0.0, 1.0);
        let prob = p.density.clamp(0.1, 100.0) / self.sr;
        let stereo = p.crackle_stereo >= 0.5;
        let decay = (-1.0 / (0.0003 * self.sr)).exp();

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let x = [*l, *r];
            let mut y = x;
            if tracing_on && t_amt > 0.0 {
                for c in 0..2 {
                    let b = self.tracing[c].process(x[c], t_freq, t_q, self.sr).band;
                    let u = b * tg;
                    let e = if hard { u.abs().min(1.0) } else { u * u / (1.0 + u * u) };
                    let e = self.tracing_dc[c].process(e / tg, dc_r);
                    y[c] += e * t_amt;
                }
            }
            if pinch_on && p_amt > 0.0 {
                let mid = 0.5 * (x[0] + x[1]);
                let b = self.pinch.process(mid, p_freq, p_q, self.sr).band;
                let u = b * pg;
                let shaped = if hard { u.clamp(-1.0, 1.0) } else { u.tanh() };
                let o = shaped / pg - b;
                y[0] += o * p_amt;
                y[1] -= o * p_amt;
            }
            if crackle > 0.0 {
                let chans = if stereo { 2 } else { 1 };
                let mut out = [0.0f32; 2];
                for c in 0..chans {
                    if self.uniform() < prob {
                        // Mostly small ticks, now and then a loud pop.
                        let a = self.uniform();
                        let sign = if self.rng.next_bipolar() < 0.0 { -1.0 } else { 1.0 };
                        self.crackle_env[c] = sign * (0.1 + 0.9 * a * a * a);
                    }
                    let n = self.rng.next_bipolar().abs() * 0.5 + 0.5;
                    let s = self.crackle_env[c] * n;
                    self.crackle_env[c] *= decay;
                    if self.crackle_env[c].abs() < 1e-6 {
                        self.crackle_env[c] = 0.0;
                    }
                    out[c] = self.crackle_filt[c].process(s, 2500.0, 0.8, self.sr).band * 2.0;
                }
                if !stereo {
                    out[1] = out[0];
                }
                y[0] += out[0] * crackle;
                y[1] += out[1] * crackle;
            }
            *l = y[0];
            *r = y[1];
        }
    }
}

impl super::EffectRuntime for VinylDistortion {}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(p: &VinylParams, l_in: &[f32], r_in: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mut v = VinylDistortion::new(SR);
        let ctx = FxContext::new(SR, 120.0);
        let mut l = l_in.to_vec();
        let mut r = r_in.to_vec();
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            v.process(p, a, b, &ctx);
        }
        assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 10.0));
        (l, r)
    }

    fn sine(n: usize, f: f32, a: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * f * std::f32::consts::TAU / SR).sin() * a)
            .collect()
    }

    /// Magnitude of the `f` Hz component (single-bin DFT) over the tail.
    fn tone(s: &[f32], f: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        let start = s.len() / 2;
        for (k, &x) in s[start..].iter().enumerate() {
            let w = (k + start) as f64 * f as f64 * std::f64::consts::TAU / SR as f64;
            re += x as f64 * w.cos();
            im += x as f64 * w.sin();
        }
        ((re * re + im * im).sqrt() / (s.len() - start) as f64 * 2.0) as f32
    }

    const OFF: VinylParams = VinylParams {
        tracing_on: 0.0,
        tracing_drive: 0.4,
        tracing_freq: 2500.0,
        tracing_width: 0.5,
        tracing_amount: 0.3,
        pinch_on: 0.0,
        pinch_freq: 1200.0,
        pinch_width: 0.5,
        pinch_amount: 0.3,
        drive: 0.0,
        mode: 0.0,
        crackle: 0.0,
        density: 6.0,
        crackle_stereo: 1.0,
    };

    #[test]
    fn everything_off_is_transparent() {
        let x = sine(4800, 500.0, 0.5);
        let (l, _) = run(&OFF, &x, &x);
        assert_eq!(l, x);
    }

    #[test]
    fn tracing_adds_even_harmonics() {
        let x = sine(48000, 1000.0, 0.5);
        for mode in [0.0, 1.0] {
            let p = VinylParams {
                tracing_on: 1.0,
                tracing_freq: 1000.0,
                tracing_amount: 1.0,
                tracing_drive: 1.0,
                mode,
                ..OFF
            };
            let (l, r) = run(&p, &x, &x);
            let h2 = tone(&l, 2000.0);
            assert!(h2 > 0.01, "mode {mode} second harmonic {h2}");
            assert!(tone(&l, 3000.0) < h2 * 0.5 || mode >= 0.5);
            assert_eq!(l, r);
        }
    }

    #[test]
    fn pinch_adds_odd_harmonics_out_of_phase() {
        let x = sine(48000, 800.0, 0.5);
        let p = VinylParams {
            pinch_on: 1.0,
            pinch_freq: 800.0,
            pinch_amount: 1.0,
            drive: 6.0,
            ..OFF
        };
        let (l, r) = run(&p, &x, &x);
        let side: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a - b)).collect();
        let mid: Vec<f32> = l.iter().zip(&r).map(|(a, b)| 0.5 * (a + b)).collect();
        assert!(tone(&side, 2400.0) > 0.005, "third harmonic {}", tone(&side, 2400.0));
        assert!(tone(&side, 1600.0) < 0.002);
        assert!(mid.iter().zip(&x).all(|(m, x)| (m - x).abs() < 1e-5));
    }

    #[test]
    fn crackle_density_and_stereo() {
        let silence = vec![0.0f32; 96000];
        let p = VinylParams {
            crackle: 1.0,
            density: 20.0,
            ..OFF
        };
        let (l, r) = run(&p, &silence, &silence);
        // Count pops as rising edges above a threshold.
        let mut pops = 0;
        let mut quiet = 0usize;
        for &v in &l {
            if v.abs() > 0.01 {
                if quiet > 100 {
                    pops += 1;
                }
                quiet = 0;
            } else {
                quiet += 1;
            }
        }
        assert!((15..=70).contains(&pops), "{pops} pops in two seconds");
        assert_ne!(l, r);
        let (l, r) = run(
            &VinylParams {
                crackle_stereo: 0.0,
                ..p
            },
            &silence,
            &silence,
        );
        assert_eq!(l, r);
        assert!(l.iter().any(|x| x.abs() > 0.01));
    }

    #[test]
    fn extremes_are_bounded() {
        let x: Vec<f32> = (0..20000).map(|i| ((i * 31) % 97) as f32 / 30.0 - 1.6).collect();
        let p = VinylParams {
            tracing_on: 1.0,
            tracing_drive: 1.0,
            tracing_freq: 18000.0,
            tracing_width: 1.0,
            tracing_amount: 1.0,
            pinch_on: 1.0,
            pinch_freq: 50.0,
            pinch_width: 0.0,
            pinch_amount: 1.0,
            drive: 24.0,
            mode: 1.0,
            crackle: 1.0,
            density: 100.0,
            crackle_stereo: 1.0,
        };
        run(&p, &x, &x);
        run(
            &VinylParams {
                drive: -12.0,
                mode: 0.0,
                ..p
            },
            &x,
            &x,
        );
    }
}
