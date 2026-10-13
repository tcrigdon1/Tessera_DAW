//! Erosion: degrades the sound by modulating a very short delay at audio
//! rate. The modulator is band-limited noise (shared by both channels, or
//! independent per channel in *Wide Noise* mode) or a sine. Audio-rate delay
//! modulation is phase modulation, so the result ranges from a fizzy,
//! breathy haze (noise) to metallic sidebands (sine).

use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::dsp::filter::Svf;
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

device_params! {
    pub struct ErosionParams {
        mode: "Mode", 0.0, 2.0, 0.0, ParamKind::Choice(&["Noise", "Wide Noise", "Sine"]), Unit::Plain;
        freq: "Frequency", 800.0, 18000.0, 6000.0, LOG, Unit::Hz;
        width: "Width", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        amount: "Amount", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl super::EffectParams for ErosionParams {
    fn is_relevant(&self, index: usize) -> bool {
        // Width shapes the noise band; a sine has none.
        !(index == 2 && self.mode >= 1.5)
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Air Hiss",
                Self {
                    mode: 1.0,
                    freq: 12000.0,
                    width: 0.8,
                    amount: 0.35,
                    ..d
                },
            ),
            (
                "Worn Cassette",
                Self {
                    mode: 0.0,
                    freq: 3000.0,
                    width: 0.6,
                    amount: 0.25,
                    mix: 0.7,
                },
            ),
            (
                "Metallic Ring",
                Self {
                    mode: 2.0,
                    freq: 2200.0,
                    amount: 0.4,
                    ..d
                },
            ),
        ]
    }
}

/// Largest delay swing in seconds at full amount.
const MAX_DEPTH: f32 = 0.0002;

pub struct Erosion {
    sr: f32,
    buf: [Vec<f32>; 2],
    pos: usize,
    rng: [Rng; 2],
    svf: [Svf; 2],
    phase: f32,
    amount: f32,
}

impl Erosion {
    pub fn new(sample_rate: f32) -> Self {
        let n = (MAX_DEPTH * 2.0 * sample_rate) as usize + 8;
        Self {
            sr: sample_rate,
            buf: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            rng: [Rng::new(0x1234_5677), Rng::new(0x7654_3211)],
            svf: Default::default(),
            phase: 0.0,
            amount: 0.0,
        }
    }

    pub fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
        for f in &mut self.svf {
            f.reset();
        }
    }

    pub fn process(&mut self, p: &ErosionParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let n = self.buf[0].len();
        let mode = p.mode.round().clamp(0.0, 2.0) as usize;
        let freq = p.freq.clamp(20.0, self.sr * 0.45);
        // Noise bandwidth: 0.1 .. 4 octaves.
        let bw = 0.1 + p.width.clamp(0.0, 1.0) * 3.9;
        let r = 2f32.powf(bw);
        let q = (r.sqrt() / (r - 1.0)).max(0.5);
        // Normalise the band-passed noise to roughly unit RMS (the band-pass
        // passes about fc / Q of the uniform noise spectrum).
        let band = (freq / q * std::f32::consts::FRAC_PI_2 / (self.sr * 0.5)).clamp(1e-4, 1.0);
        let norm = 1.0 / (band / 3.0).sqrt() * 0.5;
        let depth_max = MAX_DEPTH * self.sr;
        let base = depth_max + 2.0;
        let coef = super::smoothing(0.01, self.sr);
        let target = p.amount.clamp(0.0, 1.0);
        let inc = freq / self.sr;
        let wet = p.mix.clamp(0.0, 1.0);
        let dry = 1.0 - wet;

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let x = [*l, *r];
            self.amount = target + (self.amount - target) * coef;
            let mut m = [0.0f32; 2];
            match mode {
                2 => {
                    let s = (self.phase * std::f32::consts::TAU).sin();
                    self.phase += inc;
                    if self.phase >= 1.0 {
                        self.phase -= 1.0;
                    }
                    m = [s, s];
                }
                _ => {
                    let chans = if mode == 1 { 2 } else { 1 };
                    for c in 0..chans {
                        let w = self.rng[c].next_bipolar();
                        let b = self.svf[c].process(w, freq, q, self.sr).band;
                        m[c] = (b * norm).clamp(-1.0, 1.0);
                    }
                    if chans == 1 {
                        m[1] = m[0];
                    }
                }
            }
            for c in 0..2 {
                self.buf[c][self.pos] = x[c];
                let d = (base + m[c] * depth_max * self.amount).clamp(1.0, (n - 3) as f32);
                let i = d as usize;
                let fr = d - i as f32;
                let a = self.buf[c][(self.pos + n - i) % n];
                let b = self.buf[c][(self.pos + n - i - 1) % n];
                let y = a + (b - a) * fr;
                if c == 0 {
                    *l = x[0] * dry + y * wet;
                } else {
                    *r = x[1] * dry + y * wet;
                }
            }
            self.pos += 1;
            if self.pos == n {
                self.pos = 0;
            }
        }
    }
}

impl super::EffectRuntime for Erosion {}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(p: &ErosionParams, input: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let mut e = Erosion::new(SR);
        let ctx = FxContext::new(SR, 120.0);
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            e.process(p, a, b, &ctx);
        }
        assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 2.0));
        (l, r)
    }

    fn sine(n: usize, f: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * f * std::f32::consts::TAU / SR).sin() * 0.5)
            .collect()
    }

    /// Energy of `out` that is not explained by the input delayed by
    /// `delay` samples (the unmodulated path).
    fn residual(out: &[f32], input: &[f32], delay: f32) -> f32 {
        let i = delay as usize;
        let fr = delay - i as f32;
        let mut acc = 0.0;
        for k in 2000..out.len() {
            let a = input[k - i];
            let b = input[k - i - 1];
            let e = out[k] - (a + (b - a) * fr);
            acc += e * e;
        }
        acc
    }

    #[test]
    fn amount_degrades_the_signal() {
        let input = sine(24000, 3000.0);
        let delay = MAX_DEPTH * SR + 2.0;
        for mode in [0.0, 1.0, 2.0] {
            let clean = ErosionParams {
                mode,
                amount: 0.0,
                ..Default::default()
            };
            let (l, _) = run(&clean, &input);
            assert!(residual(&l, &input, delay) < 1e-3, "mode {mode} clean");
            let dirty = ErosionParams {
                mode,
                amount: 1.0,
                freq: 5000.0,
                ..Default::default()
            };
            let (l, _) = run(&dirty, &input);
            assert!(residual(&l, &input, delay) > 5.0, "mode {mode} degraded");
        }
    }

    #[test]
    fn wide_noise_decorrelates_channels() {
        let input = sine(24000, 2000.0);
        let p = ErosionParams {
            mode: 0.0,
            amount: 1.0,
            ..Default::default()
        };
        let (l, r) = run(&p, &input);
        assert_eq!(l, r);
        let p = ErosionParams { mode: 1.0, ..p };
        let (l, r) = run(&p, &input);
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 50.0, "{diff}");
    }
}
