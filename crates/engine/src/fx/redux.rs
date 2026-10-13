//! Redux: sample-rate and bit-depth reduction.
//!
//! The downsampler holds one input sample for each period of the target rate
//! (with optional random jitter of the period). *Hard* mode is a plain
//! sample-and-hold with its stair-step aliasing; *Soft* mode glides linearly
//! between the held values, which takes the edge off the steps. Optional
//! low-pass filters before and after the reduction sit at the reduced
//! Nyquist frequency, shifted by an octave offset. The quantizer rounds to
//! 2^(bits-1) steps per unit with optional TPDF dither ("shaping") and an
//! optional DC shift (truncation instead of rounding).

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

device_params! {
    pub struct ReduxParams {
        rate: "Downsample", 50.0, 48000.0, 11025.0, LOG, Unit::Hz;
        jitter: "Jitter", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        mode: "Mode", 0.0, 1.0, 1.0, ParamKind::Choice(&["Soft", "Hard"]), Unit::Plain;
        bits_on: "Bit Reduction", 0.0, 1.0, 1.0, ParamKind::Toggle, Unit::Plain;
        bits: "Bits", 1.0, 16.0, 8.0, LIN, Unit::Integer;
        shaping: "Shaping", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        dc_shift: "DC Shift", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        pre_filter: "Pre-Filter", 0.0, 1.0, 1.0, ParamKind::Toggle, Unit::Plain;
        post_filter: "Post-Filter", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        filter_oct: "Filter Octave", -3.0, 3.0, 0.0, LIN, Unit::Plain;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl super::EffectParams for ReduxParams {
    fn is_relevant(&self, index: usize) -> bool {
        match index {
            4..=6 => self.bits_on >= 0.5,
            9 => self.pre_filter >= 0.5 || self.post_filter >= 0.5,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "12-bit Sampler",
                Self {
                    rate: 26040.0,
                    bits: 12.0,
                    mode: 0.0,
                    post_filter: 1.0,
                    ..d
                },
            ),
            (
                "8-bit Console",
                Self {
                    rate: 8000.0,
                    bits: 6.0,
                    pre_filter: 0.0,
                    ..d
                },
            ),
            (
                "Crushed Drums",
                Self {
                    rate: 4000.0,
                    jitter: 0.3,
                    bits: 4.0,
                    shaping: 0.4,
                    mix: 0.6,
                    ..d
                },
            ),
            (
                "Telephone Grit",
                Self {
                    rate: 3000.0,
                    bits_on: 0.0,
                    post_filter: 1.0,
                    mode: 0.0,
                    ..d
                },
            ),
        ]
    }
}

pub struct Redux {
    sr: f32,
    rng: Rng,
    /// Phase of the reduced-rate clock in periods.
    phase: f32,
    /// Length of the current period (1 without jitter).
    period: f32,
    held: [f32; 2],
    prev_held: [f32; 2],
    pre: [[Biquad; 2]; 2],
    post: [[Biquad; 2]; 2],
    coefs: [Coefs; 2],
    coef_key: f32,
}

/// Q values of a fourth-order Butterworth low-pass as two biquads.
const BUTTER_Q: [f32; 2] = [0.541, 1.307];

impl Redux {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            rng: Rng::new(0x0bad_5eed),
            phase: 1.0,
            period: 1.0,
            held: [0.0; 2],
            prev_held: [0.0; 2],
            pre: Default::default(),
            post: Default::default(),
            coefs: [Coefs::IDENTITY; 2],
            coef_key: -1.0,
        }
    }

    pub fn reset(&mut self) {
        self.held = [0.0; 2];
        self.prev_held = [0.0; 2];
        self.phase = 1.0;
        for f in self.pre.iter_mut().chain(self.post.iter_mut()).flatten() {
            f.reset();
        }
    }

    fn update_filters(&mut self, cutoff: f32) {
        if cutoff != self.coef_key {
            self.coef_key = cutoff;
            for (c, q) in self.coefs.iter_mut().zip(BUTTER_Q) {
                *c = Coefs::design(FilterShape::LowPass, cutoff, q, 0.0, self.sr);
            }
        }
    }

    pub fn process(&mut self, p: &ReduxParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let rate = p.rate.clamp(1.0, self.sr);
        let reduce = rate < self.sr * 0.999;
        let inc = rate / self.sr;
        let jitter = p.jitter.clamp(0.0, 1.0);
        let soft = p.mode < 0.5;
        let cutoff = (rate * 0.5 * 2f32.powf(p.filter_oct.clamp(-3.0, 3.0))).clamp(20.0, self.sr * 0.45);
        self.update_filters(cutoff);
        let pre_on = p.pre_filter >= 0.5 && reduce;
        let post_on = p.post_filter >= 0.5 && reduce;
        let bits_on = p.bits_on >= 0.5;
        let steps = 2f32.powf(p.bits.round().clamp(1.0, 16.0) - 1.0);
        let dither = p.shaping.clamp(0.0, 1.0);
        let truncate = p.dc_shift >= 0.5;
        let wet = p.mix.clamp(0.0, 1.0);
        let dry = 1.0 - wet;

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let x = [*l, *r];
            let mut s = x;
            if pre_on {
                for c in 0..2 {
                    for k in 0..2 {
                        s[c] = self.pre[c][k].process(&self.coefs[k], s[c]);
                    }
                }
            }
            let mut y = s;
            if reduce {
                self.phase += inc;
                if self.phase >= self.period {
                    self.phase -= self.period;
                    if self.phase >= 1.0 {
                        self.phase = 0.0;
                    }
                    self.prev_held = self.held;
                    self.held = s;
                    self.period = 1.0 + jitter * 0.5 * self.rng.next_bipolar();
                }
                if soft {
                    let t = (self.phase / self.period).clamp(0.0, 1.0);
                    for c in 0..2 {
                        y[c] = self.prev_held[c] + (self.held[c] - self.prev_held[c]) * t;
                    }
                } else {
                    y = self.held;
                }
            }
            if bits_on {
                for c in 0..2 {
                    let noise = if dither > 0.0 {
                        (self.rng.next_bipolar() + self.rng.next_bipolar()) * 0.5 * dither
                    } else {
                        0.0
                    };
                    let v = y[c].clamp(-4.0, 4.0) * steps + noise;
                    let q = if truncate { v.floor() } else { v.round() };
                    y[c] = q / steps;
                }
            }
            if post_on {
                for c in 0..2 {
                    for k in 0..2 {
                        y[c] = self.post[c][k].process(&self.coefs[k], y[c]);
                    }
                }
            }
            *l = x[0] * dry + y[0] * wet;
            *r = x[1] * dry + y[1] * wet;
        }
    }
}

impl super::EffectRuntime for Redux {}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(p: &ReduxParams, input: &[f32]) -> Vec<f32> {
        let mut rx = Redux::new(SR);
        let ctx = FxContext::new(SR, 120.0);
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        for (a, b) in l.chunks_mut(300).zip(r.chunks_mut(300)) {
            rx.process(p, a, b, &ctx);
        }
        assert!(l.iter().all(|x| x.is_finite()));
        l
    }

    fn sine(n: usize, f: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * f * std::f32::consts::TAU / SR).sin() * 0.5)
            .collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    #[test]
    fn hard_mode_holds_samples_for_the_period() {
        let p = ReduxParams {
            rate: 1000.0,
            bits_on: 0.0,
            pre_filter: 0.0,
            ..Default::default()
        };
        let out = run(&p, &sine(9600, 37.0));
        // 48 kHz / 1 kHz: runs of 48 identical samples.
        let mut changes = 0;
        for w in out.windows(2) {
            if w[0] != w[1] {
                changes += 1;
            }
        }
        assert!((198..=201).contains(&changes), "{changes}");
    }

    #[test]
    fn bit_reduction_quantizes() {
        let p = ReduxParams {
            rate: 48000.0,
            bits: 2.0,
            ..Default::default()
        };
        let out = run(&p, &sine(4800, 100.0));
        let mut levels: Vec<i32> = out.iter().map(|x| (x * 1000.0).round() as i32).collect();
        levels.sort();
        levels.dedup();
        assert!(levels.len() <= 3, "{levels:?}");
        // Sixteen bits are transparent.
        let p = ReduxParams { bits: 16.0, ..p };
        let input = sine(4800, 100.0);
        let out = run(&p, &input);
        assert!(out.iter().zip(&input).all(|(a, b)| (a - b).abs() < 1e-4));
    }

    #[test]
    fn pre_filter_reduces_aliasing() {
        let input = sine(48000, 9000.0);
        let base = ReduxParams {
            rate: 6000.0,
            bits_on: 0.0,
            pre_filter: 0.0,
            ..Default::default()
        };
        let raw = rms(&run(&base, &input)[4800..]);
        let filtered = rms(&run(
            &ReduxParams {
                pre_filter: 1.0,
                ..base
            },
            &input,
        )[4800..]);
        assert!(raw > 0.2, "{raw}");
        assert!(filtered < raw * 0.1, "{raw} {filtered}");
    }

    #[test]
    fn extremes_are_finite() {
        let input: Vec<f32> = (0..20000).map(|i| ((i * 7919) % 200) as f32 / 50.0 - 2.0).collect();
        for mode in [0.0, 1.0] {
            let p = ReduxParams {
                rate: 50.0,
                jitter: 1.0,
                mode,
                bits: 1.0,
                shaping: 1.0,
                dc_shift: 1.0,
                post_filter: 1.0,
                filter_oct: 3.0,
                ..Default::default()
            };
            let out = run(&p, &input);
            assert!(out.iter().all(|x| x.abs() < 10.0));
        }
    }
}
