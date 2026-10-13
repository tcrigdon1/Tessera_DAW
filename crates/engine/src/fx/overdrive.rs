//! Overdrive: a guitar pedal with three clipping circuits (Overdrive,
//! Distortion, Fuzz), a three-band tone stack with a switchable mid
//! frequency and a "Sub" switch that keeps the clean low end underneath the
//! distortion.
//!
//! The nonlinear stage runs at twice the sample rate between two half-band
//! FIR filters ([`Oversampler2x`]) to keep aliasing down; the dry signal is
//! delayed by the oversampler's latency so Dry/Wet blends without combing.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use std::f32::consts::FRAC_1_SQRT_2;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const TYPES: &[&str] = &["Overdrive", "Distortion", "Fuzz"];
pub const MID_FREQS: &[&str] = &["500 Hz", "1 kHz", "2 kHz"];
const MID_HZ: [f32; 3] = [500.0, 1000.0, 2000.0];
/// Crossover below which "Sub" keeps the clean signal.
const SUB_HZ: f32 = 120.0;

device_params! {
    pub struct OverdriveParams {
        kind: "Type", 0.0, 2.0, 0.0, ParamKind::Choice(TYPES), Unit::Plain;
        gain: "Gain", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        output: "Output", -24.0, 12.0, 0.0, LIN, Unit::Db;
        bass: "Bass", -15.0, 15.0, 0.0, LIN, Unit::Db;
        mid: "Mid", -15.0, 15.0, 0.0, LIN, Unit::Db;
        mid_freq: "Mid Freq", 0.0, 2.0, 1.0, ParamKind::Choice(MID_FREQS), Unit::Plain;
        treble: "Treble", -15.0, 15.0, 0.0, LIN, Unit::Db;
        sub: "Sub", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl super::EffectParams for OverdriveParams {
    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Warm Crunch",
                Self {
                    gain: 0.35,
                    mid: 3.0,
                    treble: -2.0,
                    ..d
                },
            ),
            (
                "Metal Wall",
                Self {
                    kind: 1.0,
                    gain: 0.8,
                    bass: 4.0,
                    mid: -6.0,
                    mid_freq: 0.0,
                    treble: 3.0,
                    output: -6.0,
                    ..d
                },
            ),
            (
                "Fuzz Bass",
                Self {
                    kind: 2.0,
                    gain: 0.7,
                    sub: 1.0,
                    treble: -4.0,
                    output: -6.0,
                    mix: 0.8,
                    ..d
                },
            ),
            (
                "Lead Boost",
                Self {
                    gain: 0.6,
                    mid: 6.0,
                    mid_freq: 2.0,
                    output: -3.0,
                    ..d
                },
            ),
        ]
    }
}

// ---------------------------------------------------------------------------
// Shared building blocks (also used by the Amp, Cabinet and Drum Bus).

/// Even-indexed taps of a 47-tap half-band low-pass (the odd ones are zero
/// except the centre tap, which is 0.5).
const HB_TAPS: usize = 24;
/// Latency of an up/down round trip in base-rate samples.
pub(crate) const OS_LATENCY: usize = 23;

fn halfband_taps() -> [f32; HB_TAPS] {
    let n = 2 * HB_TAPS - 1; // 47
    let c = (n / 2) as f64; // 23
    let mut taps = [0.0f32; HB_TAPS];
    let mut sum = 0.0f64;
    let mut raw = [0.0f64; HB_TAPS];
    for (k, v) in raw.iter_mut().enumerate() {
        let i = (2 * k) as f64;
        let t = i - c;
        let sinc = (std::f64::consts::PI * 0.5 * t).sin() / (std::f64::consts::PI * t);
        // Blackman window.
        let ph = 2.0 * std::f64::consts::PI * i / (n - 1) as f64;
        let w = 0.42 - 0.5 * ph.cos() + 0.08 * (2.0 * ph).cos();
        *v = sinc * w;
        sum += *v;
    }
    // Even taps sum to exactly 0.5 so DC passes at unity.
    for (t, r) in taps.iter_mut().zip(raw.iter()) {
        *t = (r * 0.5 / sum) as f32;
    }
    taps
}

/// Fixed-length history with the newest sample first.
#[derive(Clone, Copy)]
struct History {
    buf: [f32; 2 * HB_TAPS],
    pos: usize,
}

impl History {
    const fn new() -> Self {
        Self {
            buf: [0.0; 2 * HB_TAPS],
            pos: 0,
        }
    }

    #[inline]
    fn push(&mut self, x: f32) {
        self.pos = if self.pos == 0 { HB_TAPS - 1 } else { self.pos - 1 };
        self.buf[self.pos] = x;
        self.buf[self.pos + HB_TAPS] = x;
    }

    #[inline]
    fn slice(&self) -> &[f32] {
        &self.buf[self.pos..self.pos + HB_TAPS]
    }

    #[inline]
    fn get(&self, age: usize) -> f32 {
        self.buf[self.pos + age]
    }
}

#[inline]
fn dot(a: &[f32; HB_TAPS], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Two-times oversampling around a nonlinear function, with half-band FIR
/// interpolation and decimation.
#[derive(Clone, Copy)]
pub(crate) struct Oversampler2x {
    taps: [f32; HB_TAPS],
    up: History,
    down_even: History,
    down_odd: History,
}

impl Oversampler2x {
    pub fn new() -> Self {
        Self {
            taps: halfband_taps(),
            up: History::new(),
            down_even: History::new(),
            down_odd: History::new(),
        }
    }

    pub fn reset(&mut self) {
        self.up = History::new();
        self.down_even = History::new();
        self.down_odd = History::new();
    }

    /// Runs `f` on the two oversampled points for input `x` and returns the
    /// decimated result (delayed by [`OS_LATENCY`] samples).
    #[inline]
    pub fn process(&mut self, x: f32, mut f: impl FnMut(f32) -> f32) -> f32 {
        self.up.push(x);
        let a = 2.0 * dot(&self.taps, self.up.slice());
        let b = self.up.get((HB_TAPS - 1) / 2);
        let ya = f(a);
        let yb = f(b);
        self.down_even.push(ya);
        self.down_odd.push(yb);
        let y = dot(&self.taps, self.down_even.slice()) + 0.5 * self.down_odd.get(HB_TAPS / 2);
        if y.abs() < 1e-20 {
            0.0
        } else {
            y
        }
    }
}

/// First-order low/high-pass sections.
#[derive(Clone, Copy, Default)]
pub(crate) struct OnePole {
    z: f32,
}

/// Coefficient for [`OnePole`] at `cutoff` (Hz) and `sample_rate`.
pub(crate) fn one_pole(cutoff: f32, sample_rate: f32) -> f32 {
    1.0 - (-2.0 * std::f32::consts::PI * cutoff.clamp(1.0, sample_rate * 0.45) / sample_rate).exp()
}

impl OnePole {
    #[inline]
    pub fn lp(&mut self, x: f32, a: f32) -> f32 {
        self.z += a * (x - self.z);
        if self.z.abs() < 1e-20 {
            self.z = 0.0;
        }
        self.z
    }

    #[inline]
    pub fn hp(&mut self, x: f32, a: f32) -> f32 {
        x - self.lp(x, a)
    }
}

/// A short fixed delay (for aligning the dry signal with oversampled paths).
#[derive(Clone, Copy)]
pub(crate) struct ShortDelay {
    buf: [f32; 32],
    pos: usize,
}

impl ShortDelay {
    pub const fn new() -> Self {
        Self { buf: [0.0; 32], pos: 0 }
    }

    /// Pushes `x` and returns the sample from `delay` (< 32) samples ago.
    #[inline]
    pub fn process(&mut self, x: f32, delay: usize) -> f32 {
        self.buf[self.pos] = x;
        let y = self.buf[(self.pos + 32 - delay.min(31)) % 32];
        self.pos = (self.pos + 1) % 32;
        y
    }

    pub fn reset(&mut self) {
        self.buf = [0.0; 32];
    }
}

/// Cubic soft clipper: linear-ish near zero, flat at +-1 beyond |x| = 1.5.
#[inline]
pub(crate) fn cubic_clip(x: f32) -> f32 {
    let x = (x * (2.0 / 3.0)).clamp(-1.0, 1.0);
    1.5 * (x - x * x * x / 3.0)
}

/// Fast rational tanh approximation (accurate to ~1e-3, bounded by 1).
#[inline]
pub(crate) fn tanh_fast(x: f32) -> f32 {
    let x = x.clamp(-4.97, 4.97);
    let x2 = x * x;
    let y = x * (27.0 + x2) / (27.0 + 9.0 * x2);
    y.clamp(-1.0, 1.0)
}

// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct Channel {
    pre: OnePole,
    post: OnePole,
    dc: OnePole,
    fuzz_env: f32,
    tone: [Biquad; 3],
    /// Linkwitz-Riley split: clean lows from the dry signal, distorted highs.
    sub_lp: [Biquad; 2],
    sub_hp: [Biquad; 2],
}

pub struct Overdrive {
    sr: f32,
    os: [Oversampler2x; 2],
    dry: [ShortDelay; 2],
    ch: [Channel; 2],
    tone: [Coefs; 3],
    sub_lp: Coefs,
    sub_hp: Coefs,
    tone_key: [f32; 4],
    drive: f32,
    out: f32,
    mix: f32,
}

impl Overdrive {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            sr: sample_rate,
            os: [Oversampler2x::new(); 2],
            dry: [ShortDelay::new(); 2],
            ch: [Channel::default(); 2],
            tone: [Coefs::IDENTITY; 3],
            sub_lp: Coefs::design(FilterShape::LowPass, SUB_HZ, FRAC_1_SQRT_2, 0.0, sample_rate),
            sub_hp: Coefs::design(FilterShape::HighPass, SUB_HZ, FRAC_1_SQRT_2, 0.0, sample_rate),
            tone_key: [f32::NAN; 4],
            drive: 1.0,
            out: 1.0,
            mix: 1.0,
        };
        s.update_tone(&OverdriveParams::default());
        s
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

    fn update_tone(&mut self, p: &OverdriveParams) {
        let key = [p.bass, p.mid, p.mid_freq, p.treble];
        if key == self.tone_key {
            return;
        }
        self.tone_key = key;
        let sr = self.sr;
        let mf = MID_HZ[super::glue::choice(p.mid_freq, MID_HZ.len())];
        self.tone = [
            Coefs::design(FilterShape::LowShelf, 150.0, 0.7, p.bass.clamp(-15.0, 15.0), sr),
            Coefs::design(FilterShape::Bell, mf, 0.9, p.mid.clamp(-15.0, 15.0), sr),
            Coefs::design(FilterShape::HighShelf, 3200.0, 0.7, p.treble.clamp(-15.0, 15.0), sr),
        ];
    }

    pub fn process(&mut self, p: &OverdriveParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        self.update_tone(p);
        let sr2 = self.sr * 2.0;
        let kind = super::glue::choice(p.kind, TYPES.len());
        let g = p.gain.clamp(0.0, 1.0);
        // Drive range, pre-emphasis high-pass, post low-pass and level per circuit.
        let (drive_db, pre_hz, post_hz, level) = match kind {
            0 => (g * 36.0, 720.0, 6500.0, 0.8),
            1 => (6.0 + g * 44.0, 160.0, 5000.0, 0.45),
            _ => (12.0 + g * 42.0, 70.0, 4200.0, 0.45),
        };
        let a_pre = one_pole(pre_hz, sr2);
        let a_post = one_pole(post_hz, sr2);
        let a_dc = one_pole(15.0, sr2);
        let fuzz_att = one_pole(1.0 / 0.03, sr2);
        let smooth = super::smoothing(0.01, self.sr);
        let drive_t = super::db(drive_db);
        let out_t = super::db(p.output.clamp(-24.0, 12.0)) * level;
        let mix_t = p.mix.clamp(0.0, 1.0);
        let sub_on = p.sub >= 0.5;
        let tone = self.tone;
        let (sub_lp, sub_hp) = (self.sub_lp, self.sub_hp);
        for i in 0..left.len().min(right.len()) {
            self.drive = drive_t + (self.drive - drive_t) * smooth;
            self.out = out_t + (self.out - out_t) * smooth;
            self.mix = mix_t + (self.mix - mix_t) * smooth;
            let drive = self.drive;
            for c in 0..2 {
                let x = if c == 0 { left[i] } else { right[i] };
                let ch = &mut self.ch[c];
                let wet = self.os[c].process(x, |s| {
                    let y = match kind {
                        0 => {
                            // Clean signal plus soft-clipped, mid-focused drive.
                            let h = ch.pre.hp(s, a_pre);
                            s + 0.7 * tanh_fast(drive * h)
                        }
                        1 => cubic_clip(drive * ch.pre.hp(s, a_pre)),
                        _ => {
                            // Biased transistor: the operating point shifts
                            // with the signal level, so the clipped wave gets
                            // an uneven duty cycle (even harmonics) and
                            // sputters as notes decay.
                            let h = ch.pre.hp(s, a_pre);
                            ch.fuzz_env += fuzz_att * (h.abs() - ch.fuzz_env);
                            let u = drive * (h + 0.3 * ch.fuzz_env) - 0.15;
                            // Unequal rails: the negative half clips lower.
                            let y = if u > 0.0 {
                                tanh_fast(u)
                            } else {
                                0.55 * tanh_fast(u * 1.45)
                            };
                            ch.dc.hp(y, a_dc)
                        }
                    };
                    ch.post.lp(y, a_post)
                });
                let mut wet = wet;
                for (f, k) in ch.tone.iter_mut().zip(tone.iter()) {
                    wet = f.process(k, wet);
                }
                wet *= self.out;
                let dry = self.dry[c].process(x, OS_LATENCY);
                if sub_on {
                    let h = ch.sub_hp[0].process(&sub_hp, wet);
                    let h = ch.sub_hp[1].process(&sub_hp, h);
                    let l = ch.sub_lp[0].process(&sub_lp, dry);
                    wet = h + ch.sub_lp[1].process(&sub_lp, l);
                }
                let y = dry + (wet - dry) * self.mix;
                if c == 0 {
                    left[i] = y;
                } else {
                    right[i] = y;
                }
            }
        }
    }
}

impl super::EffectRuntime for Overdrive {}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::EffectParams;
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48000.0;

    /// Magnitude of the DFT bin nearest `freq`.
    pub(crate) fn tone_level(x: &[f32], freq: f32, sr: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, &v) in x.iter().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * freq as f64 * i as f64 / sr as f64;
            // Hann window.
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / x.len() as f64).cos();
            re += v as f64 * w * ph.cos();
            im += v as f64 * w * ph.sin();
        }
        ((re * re + im * im).sqrt() * 4.0 / x.len() as f64) as f32
    }

    #[test]
    fn oversampler_is_transparent_for_linear_functions() {
        let mut os = Oversampler2x::new();
        let x: Vec<f32> = (0..4800).map(|i| (2.0 * PI * 1000.0 * i as f32 / SR).sin()).collect();
        let y: Vec<f32> = x.iter().map(|&v| os.process(v, |s| s)).collect();
        for i in 1000..4800 {
            assert!(
                (y[i] - x[i - OS_LATENCY]).abs() < 2e-3,
                "{i}: {} vs {}",
                y[i],
                x[i - OS_LATENCY]
            );
        }
    }

    #[test]
    fn oversampling_reduces_aliasing() {
        // A hard-clipped 7 kHz tone: its 5th harmonic (35 kHz) aliases to
        // 13 kHz without oversampling.
        let f = 7000.0;
        let x: Vec<f32> = (0..16384).map(|i| (2.0 * PI * f * i as f32 / SR).sin()).collect();
        let clip = |s: f32| (s * 8.0).clamp(-1.0, 1.0);
        let naive: Vec<f32> = x.iter().map(|&v| clip(v)).collect();
        let mut os = Oversampler2x::new();
        let over: Vec<f32> = x.iter().map(|&v| os.process(v, clip)).collect();
        let a_naive = tone_level(&naive[1000..], 13000.0, SR);
        let a_over = tone_level(&over[1000..], 13000.0, SR);
        assert!(a_over < a_naive * 0.1, "naive {a_naive} oversampled {a_over}");
        // The fundamental survives.
        assert!(tone_level(&over[1000..], f, SR) > 0.8);
    }

    fn run(p: &OverdriveParams, x: &[f32]) -> Vec<f32> {
        let ctx = FxContext::new(SR, 120.0);
        let mut o = Overdrive::new(SR);
        let mut l = x.to_vec();
        let mut r = x.to_vec();
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            o.process(p, cl, cr, &ctx);
        }
        l
    }

    #[test]
    fn each_type_adds_harmonics_and_stays_bounded() {
        let x: Vec<f32> = (0..24000)
            .map(|i| 0.5 * (2.0 * PI * 220.0 * i as f32 / SR).sin())
            .collect();
        let mut prev_h3 = 0.0;
        for kind in 0..3 {
            let p = OverdriveParams {
                kind: kind as f32,
                gain: 0.8,
                ..Default::default()
            };
            let y = run(&p, &x);
            assert!(y.iter().all(|v| v.is_finite() && v.abs() < 4.0));
            let fund = tone_level(&y[4000..], 220.0, SR);
            let h3 = tone_level(&y[4000..], 660.0, SR);
            assert!(h3 > fund * 0.05, "type {kind}: {fund} {h3}");
            assert!(h3 != prev_h3);
            prev_h3 = h3;
        }
        // Fuzz is asymmetric: it has even harmonics.
        let p = OverdriveParams {
            kind: 2.0,
            gain: 0.5,
            ..Default::default()
        };
        let y = run(&p, &x);
        let h2 = tone_level(&y[4000..], 440.0, SR);
        assert!(h2 > 0.01, "{h2} {}", tone_level(&y[4000..], 220.0, SR));
    }

    #[test]
    fn tone_stack_and_mix() {
        let x: Vec<f32> = (0..24000)
            .map(|i| 0.05 * (2.0 * PI * 6000.0 * i as f32 / SR).sin())
            .collect();
        let base = OverdriveParams {
            gain: 0.0,
            ..Default::default()
        };
        let flat = tone_level(&run(&base, &x)[4000..], 6000.0, SR);
        let bright = tone_level(&run(&OverdriveParams { treble: 12.0, ..base }, &x)[4000..], 6000.0, SR);
        assert!(bright > flat * 2.5, "{flat} {bright}");
        let dry = run(
            &OverdriveParams {
                mix: 0.0,
                gain: 1.0,
                ..base
            },
            &x,
        );
        for i in 9600..24000 {
            assert!((dry[i] - x[i - OS_LATENCY]).abs() < 1e-3);
        }
    }

    #[test]
    fn sub_keeps_the_low_end() {
        let x: Vec<f32> = (0..48000)
            .map(|i| 0.3 * (2.0 * PI * 50.0 * i as f32 / SR).sin())
            .collect();
        let p = OverdriveParams {
            kind: 1.0,
            ..Default::default()
        };
        // With Sub on, 50 Hz comes through clean at its input level while
        // the distorted harmonics above the crossover remain.
        let y = run(&OverdriveParams { sub: 1.0, ..p }, &x);
        let fund = tone_level(&y[8000..], 50.0, SR);
        assert!((fund - 0.3).abs() < 0.03, "{fund}");
        assert!(tone_level(&y[8000..], 250.0, SR) > 0.02);
    }

    #[test]
    fn presets_stay_finite() {
        let ctx = FxContext::new(SR, 120.0);
        for (_, p) in OverdriveParams::presets() {
            let mut o = Overdrive::new(SR);
            let mut l: Vec<f32> = (0..2048).map(|i| ((i * 31) % 100) as f32 * 0.4 - 20.0).collect();
            let mut r = l.clone();
            o.process(&p, &mut l, &mut r, &ctx);
            o.process(&p, &mut l[..1], &mut r[..1], &ctx);
            assert!(l.iter().all(|v| v.is_finite() && v.abs() < 50.0));
        }
    }
}
