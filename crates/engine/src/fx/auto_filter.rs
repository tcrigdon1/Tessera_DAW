//! Auto Filter: a resonant multi-mode filter whose cutoff is swept by an LFO
//! (free-running or locked to the song grid) and an envelope follower.
//!
//! This module also holds the small building blocks shared by the modulation
//! effects (Auto Pan, Phaser-Flanger, Shifter, Echo, Grain Delay): LFO shapes
//! and clock, tempo-sync divisions, a zero-delay-feedback state variable
//! filter with precomputed coefficients, an envelope follower and a cubic
//! delay-line reader.

use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

// ---------------------------------------------------------------------------
// Shared modulation helpers
// ---------------------------------------------------------------------------

/// LFO waveforms offered by the modulation effects.
pub const LFO_SHAPES: &[&str] = &["Sine", "Triangle", "Saw Up", "Saw Down", "Square", "S&H", "Random"];
pub const SHAPE_SINE: usize = 0;
pub const SHAPE_TRIANGLE: usize = 1;
pub const SHAPE_SAW_UP: usize = 2;
pub const SHAPE_SAW_DOWN: usize = 3;
pub const SHAPE_SQUARE: usize = 4;
pub const SHAPE_SAMPLE_HOLD: usize = 5;
pub const SHAPE_RANDOM: usize = 6;

/// Tempo-synced LFO periods, from a 32nd note to eight bars.
pub const SYNC_RATES: &[&str] = &[
    "1/32", "1/16T", "1/16", "1/16D", "1/8T", "1/8", "1/8D", "1/4T", "1/4", "1/4D", "1/2T", "1/2", "1/2D", "1 Bar",
    "2 Bars", "4 Bars", "8 Bars",
];
/// Index of "1/4" in [`SYNC_RATES`].
pub const SYNC_QUARTER: f32 = 8.0;
/// Index of "1 Bar" in [`SYNC_RATES`].
pub const SYNC_BAR: f32 = 13.0;

/// Period of a [`SYNC_RATES`] entry in beats.
pub fn sync_rate_beats(index: f32, beats_per_bar: u32) -> f64 {
    const NOTES: [f64; 13] = [
        0.125,
        1.0 / 6.0,
        0.25,
        0.375,
        1.0 / 3.0,
        0.5,
        0.75,
        2.0 / 3.0,
        1.0,
        1.5,
        4.0 / 3.0,
        2.0,
        3.0,
    ];
    let i = (index.round().max(0.0) as usize).min(SYNC_RATES.len() - 1);
    let bar = beats_per_bar.max(1) as f64;
    match i {
        0..=12 => NOTES[i],
        13 => bar,
        14 => bar * 2.0,
        15 => bar * 4.0,
        _ => bar * 8.0,
    }
}

/// Converts degrees (0..360) to a phase fraction.
#[inline]
pub fn degrees(d: f32) -> f32 {
    (d / 360.0).rem_euclid(1.0)
}

/// Wraps a phase into 0..1.
#[inline]
pub fn wrap(p: f32) -> f32 {
    p - p.floor()
}

/// The deterministic part of an LFO shape at `phase` (0..1), bipolar -1..1.
/// The random shapes return a stepped / smoothed pseudo-random pattern that is
/// stable for a given phase (used for previews).
pub fn shape_value(shape: usize, phase: f32) -> f32 {
    let p = wrap(phase);
    match shape {
        SHAPE_SINE => (p * std::f32::consts::TAU).sin(),
        SHAPE_TRIANGLE => {
            // Starts at 0 rising, like the sine.
            let q = wrap(p + 0.25);
            1.0 - 4.0 * (q - 0.5).abs()
        }
        SHAPE_SAW_UP => 2.0 * p - 1.0,
        SHAPE_SAW_DOWN => 1.0 - 2.0 * p,
        SHAPE_SQUARE => {
            if p < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        _ => {
            // Preview pattern: hashed steps over four cycles.
            let hash = |k: i32| {
                let mut x = (k as u32).wrapping_mul(0x9e37_79b9) ^ 0x5bd1_e995;
                x ^= x >> 15;
                x = x.wrapping_mul(0x2c1b_3c6d);
                x ^= x >> 12;
                (x as f32 / u32::MAX as f32) * 2.0 - 1.0
            };
            let t = phase * 4.0;
            let k = t.floor() as i32;
            if shape == SHAPE_SAMPLE_HOLD {
                hash(k)
            } else {
                let f = t - k as f32;
                let s = f * f * (3.0 - 2.0 * f);
                hash(k) + (hash(k + 1) - hash(k)) * s
            }
        }
    }
}

/// Keeps an LFO's phase, either free-running at a rate in Hz or synced to the
/// tempo (and locked to the song position while the transport plays).
#[derive(Debug, Clone, Copy, Default)]
pub struct LfoClock {
    phase: f64,
}

impl LfoClock {
    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    /// Advances the clock over a block of `frames` samples and returns the
    /// phase at the first sample plus the per-sample increment.
    pub fn block(&mut self, sync: bool, rate_hz: f32, div: f32, ctx: &FxContext, frames: usize) -> (f64, f64) {
        let sr = ctx.sample_rate.max(1.0) as f64;
        let (start, inc) = if sync {
            let period = sync_rate_beats(div, ctx.beats_per_bar).max(1e-3);
            let inc = ctx.tempo.clamp(1.0, 2000.0) / 60.0 / sr / period;
            if ctx.playing {
                ((ctx.song_beat / period).rem_euclid(1.0), inc)
            } else {
                (self.phase, inc)
            }
        } else {
            (self.phase, (rate_hz.max(0.0) as f64) / sr)
        };
        self.phase = (start + inc * frames as f64).rem_euclid(1.0);
        (start, inc)
    }
}

/// One channel of an LFO: evaluates the waveform at a phase and keeps the
/// random shapes' state (new values on each cycle).
#[derive(Debug, Clone)]
pub struct LfoVoice {
    last: f32,
    held: f32,
    from: f32,
    to: f32,
    rng: Rng,
}

impl LfoVoice {
    pub fn new(seed: u32) -> Self {
        let mut rng = Rng::new(seed);
        let held = rng.next_bipolar();
        let to = rng.next_bipolar();
        Self {
            last: 0.0,
            held,
            from: held,
            to,
            rng,
        }
    }

    /// Value at `phase` (0..1), bipolar -1..1.
    #[inline]
    pub fn value(&mut self, shape: usize, phase: f32) -> f32 {
        let p = wrap(phase);
        if p < self.last - 0.5 {
            // A new cycle started.
            self.held = self.rng.next_bipolar();
            self.from = self.to;
            self.to = self.rng.next_bipolar();
        }
        self.last = p;
        match shape {
            SHAPE_SAMPLE_HOLD => self.held,
            SHAPE_RANDOM => {
                let s = p * p * (3.0 - 2.0 * p);
                self.from + (self.to - self.from) * s
            }
            _ => shape_value(shape, p),
        }
    }
}

/// Coefficients of a trapezoidal (zero-delay-feedback) state variable filter.
#[derive(Debug, Clone, Copy)]
pub struct SvfCoefs {
    a1: f32,
    a2: f32,
    a3: f32,
    pub k: f32,
}

impl SvfCoefs {
    /// `cutoff` in Hz (clamped to 10 Hz .. 0.47 * sr), `q` >= 0.5.
    #[inline]
    pub fn new(cutoff: f32, q: f32, sample_rate: f32) -> Self {
        let fc = cutoff.clamp(10.0, sample_rate * 0.47);
        let g = (std::f32::consts::PI * fc / sample_rate).tan();
        let k = 1.0 / q.max(0.5);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        Self { a1, a2, a3: g * a2, k }
    }
}

/// State of one [`SvfCoefs`] filter for one channel.
#[derive(Debug, Clone, Copy, Default)]
pub struct SvfState {
    ic1: f32,
    ic2: f32,
}

impl SvfState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Returns (low, band, high).
    #[inline]
    pub fn tick(&mut self, c: &SvfCoefs, x: f32) -> (f32, f32, f32) {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        if self.ic1.abs() < 1e-20 {
            self.ic1 = 0.0;
        }
        if self.ic2.abs() < 1e-20 {
            self.ic2 = 0.0;
        }
        (v2, v1, x - c.k * v1 - v2)
    }
}

/// Peak envelope follower with separate attack and release.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnvFollower {
    pub env: f32,
}

impl EnvFollower {
    /// `attack` / `release` are one-pole coefficients ([`super::smoothing`]).
    #[inline]
    pub fn tick(&mut self, x: f32, attack: f32, release: f32) -> f32 {
        let a = x.abs();
        let c = if a > self.env { attack } else { release };
        self.env = a + (self.env - a) * c;
        if self.env < 1e-12 {
            self.env = 0.0;
        }
        self.env
    }
}

/// Reads a circular buffer `delay` samples behind `pos` (the slot the next
/// sample will be written to) with 4-point Hermite interpolation. `delay` is
/// clamped to 2 .. len - 3.
#[inline]
pub fn read_cubic(buf: &[f32], pos: usize, delay: f32) -> f32 {
    let n = buf.len();
    if n < 8 {
        return 0.0;
    }
    let d = if delay.is_finite() { delay } else { 2.0 }.clamp(2.0, (n - 3) as f32);
    let i = d as usize;
    let f = d - i as f32;
    let at = |k: usize| buf[(pos + 2 * n - k) % n];
    let xm1 = at(i - 1);
    let x0 = at(i);
    let x1 = at(i + 1);
    let x2 = at(i + 2);
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * f + c2) * f + c1) * f + x0
}

/// Soft clipper used in feedback loops (a rational tanh approximation that
/// stays within -1..1).
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

// ---------------------------------------------------------------------------
// Auto Filter
// ---------------------------------------------------------------------------

pub const FILTER_TYPES: &[&str] = &["Low-pass", "High-pass", "Band-pass", "Notch", "Morph"];
pub const TYPE_LOW: usize = 0;
pub const TYPE_HIGH: usize = 1;
pub const TYPE_BAND: usize = 2;
pub const TYPE_NOTCH: usize = 3;
pub const TYPE_MORPH: usize = 4;
const SLOPES: &[&str] = &["12 dB", "24 dB"];

/// Octaves of cutoff sweep at full LFO amount (each way).
pub const LFO_OCTAVES: f32 = 4.0;
/// Octaves of cutoff sweep at full envelope amount.
pub const ENV_OCTAVES: f32 = 5.0;

device_params! {
    pub struct AutoFilterParams {
        filter_type: "Type", 0.0, 4.0, 0.0, ParamKind::Choice(FILTER_TYPES), Unit::Plain;
        slope: "Slope", 0.0, 1.0, 0.0, ParamKind::Choice(SLOPES), Unit::Plain;
        freq: "Frequency", 20.0, 20000.0, 1200.0, LOG, Unit::Hz;
        resonance: "Resonance", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        morph: "Morph", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        drive: "Drive", 0.0, 24.0, 0.0, LIN, Unit::Db;
        lfo_amount: "LFO Amount", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        lfo_shape: "LFO Shape", 0.0, 6.0, 0.0, ParamKind::Choice(LFO_SHAPES), Unit::Plain;
        lfo_sync: "LFO Sync", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        lfo_rate: "LFO Rate", 0.01, 40.0, 1.0, LOG, Unit::Hz;
        lfo_div: "LFO Rate", 0.0, 16.0, SYNC_QUARTER, ParamKind::Choice(SYNC_RATES), Unit::Plain;
        lfo_phase: "Stereo Phase", 0.0, 360.0, 0.0, LIN, Unit::Integer;
        env_amount: "Env Amount", -1.0, 1.0, 0.0, LIN, Unit::Percent;
        env_attack: "Attack", 0.1, 100.0, 6.0, LOG, Unit::Ms;
        env_release: "Release", 5.0, 2000.0, 200.0, LOG, Unit::Ms;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl AutoFilterParams {
    pub fn kind(&self) -> usize {
        (self.filter_type.round().max(0.0) as usize).min(TYPE_MORPH)
    }

    pub fn steep(&self) -> bool {
        self.slope >= 0.5
    }

    /// Filter Q of the resonant stage.
    fn q(&self) -> f32 {
        let r = self.resonance.clamp(0.0, 1.0);
        if self.steep() {
            1.3066 * (1.0 + r * r * 14.0)
        } else {
            std::f32::consts::FRAC_1_SQRT_2 * (1.0 + r * r * 25.0)
        }
    }

    /// LP / BP / HP weights for the morph type.
    fn morph_weights(&self) -> [f32; 3] {
        let m = self.morph.clamp(0.0, 1.0);
        [
            (1.0 - 2.0 * m).max(0.0),
            1.0 - (2.0 * m - 1.0).abs(),
            (2.0 * m - 1.0).max(0.0),
        ]
    }

    /// Magnitude response in dB of the unmodulated filter at `freq` (Hz),
    /// matching the digital filter exactly (for drawing the curve).
    pub fn response_db(&self, freq: f32, sample_rate: f32) -> f32 {
        self.response_db_at(self.freq, freq, sample_rate)
    }

    /// Like [`AutoFilterParams::response_db`] with the cutoff at `cutoff`.
    pub fn response_db_at(&self, cutoff: f32, freq: f32, sample_rate: f32) -> f32 {
        let pi = std::f32::consts::PI;
        let fc = cutoff.clamp(10.0, sample_rate * 0.47);
        let w = (pi * freq.clamp(1.0, sample_rate * 0.499) / sample_rate).tan() / (pi * fc / sample_rate).tan();
        let kind = self.kind();
        let weights = self.morph_weights();
        let stage = |q: f32| -> f32 {
            let k = 1.0 / q.max(0.5);
            // s = jw: denominator 1 - w^2 + j k w.
            let dr = 1.0 - w * w;
            let di = k * w;
            let den = (dr * dr + di * di).max(1e-30);
            let (nr, ni) = match kind {
                TYPE_LOW => (1.0, 0.0),
                TYPE_HIGH => (-w * w, 0.0),
                TYPE_BAND => (0.0, k * w),
                TYPE_NOTCH => (1.0 - w * w, 0.0),
                _ => {
                    let [l, b, h] = weights;
                    (l - h * w * w, b * k * w)
                }
            };
            ((nr * nr + ni * ni) / den).sqrt()
        };
        let mag = if self.steep() {
            stage(0.5412) * stage(self.q())
        } else {
            stage(self.q())
        };
        20.0 * mag.max(1e-6).log10()
    }
}

impl super::EffectParams for AutoFilterParams {
    fn is_relevant(&self, index: usize) -> bool {
        let synced = self.lfo_sync >= 0.5;
        let lfo = self.lfo_amount > 0.0;
        let env = self.env_amount != 0.0;
        match index {
            4 => self.kind() == TYPE_MORPH,
            7 | 8 | 11 => lfo,
            9 => lfo && !synced,
            10 => lfo && synced,
            13 | 14 => env,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Slow Sweep",
                Self {
                    freq: 900.0,
                    resonance: 0.45,
                    slope: 1.0,
                    lfo_amount: 0.6,
                    lfo_sync: 1.0,
                    lfo_div: 15.0,
                    lfo_shape: SHAPE_TRIANGLE as f32,
                    ..d
                },
            ),
            (
                "Envelope Wah",
                Self {
                    filter_type: TYPE_BAND as f32,
                    freq: 350.0,
                    resonance: 0.6,
                    env_amount: 0.8,
                    env_attack: 3.0,
                    env_release: 120.0,
                    ..d
                },
            ),
            (
                "Stereo Random Hop",
                Self {
                    freq: 1500.0,
                    resonance: 0.5,
                    lfo_amount: 0.45,
                    lfo_sync: 1.0,
                    lfo_div: 5.0,
                    lfo_shape: SHAPE_SAMPLE_HOLD as f32,
                    lfo_phase: 180.0,
                    ..d
                },
            ),
            (
                "Driven Morph",
                Self {
                    filter_type: TYPE_MORPH as f32,
                    morph: 0.25,
                    freq: 700.0,
                    resonance: 0.55,
                    drive: 12.0,
                    lfo_amount: 0.3,
                    lfo_rate: 0.25,
                    ..d
                },
            ),
        ]
    }
}

pub struct AutoFilter {
    sr: f32,
    clock: LfoClock,
    voices: [LfoVoice; 2],
    /// Two cascaded stages per channel (the first only at 24 dB).
    stages: [[SvfState; 2]; 2],
    env: EnvFollower,
    /// Smoothed log2 of the base cutoff.
    log_freq: f32,
    mix: f32,
    started: bool,
}

impl AutoFilter {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate.max(1.0),
            clock: LfoClock::default(),
            voices: [LfoVoice::new(0x1234_5678), LfoVoice::new(0x0bad_cafe)],
            stages: [[SvfState::default(); 2]; 2],
            env: EnvFollower::default(),
            log_freq: 0.0,
            mix: 1.0,
            started: false,
        }
    }

    pub fn reset(&mut self) {
        for ch in &mut self.stages {
            for s in ch {
                s.reset();
            }
        }
        self.env = EnvFollower::default();
        self.clock.reset();
    }

    pub fn process(&mut self, p: &AutoFilterParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let sr = self.sr;
        let frames = left.len().min(right.len());
        let target_log = p.freq.clamp(10.0, 22000.0).log2();
        let target_mix = p.mix.clamp(0.0, 1.0);
        if !self.started {
            self.started = true;
            self.log_freq = target_log;
            self.mix = target_mix;
        }
        let smooth = 1.0 - super::smoothing(0.012, sr);
        let (start, inc) = self.clock.block(p.lfo_sync >= 0.5, p.lfo_rate, p.lfo_div, ctx, frames);
        let shape = (p.lfo_shape.round().max(0.0) as usize).min(SHAPE_RANDOM);
        let lfo_oct = p.lfo_amount.clamp(0.0, 1.0) * LFO_OCTAVES;
        let offset = degrees(p.lfo_phase);
        let env_oct = p.env_amount.clamp(-1.0, 1.0) * ENV_OCTAVES;
        let att = super::smoothing(p.env_attack.max(0.05) * 0.001, sr);
        let rel = super::smoothing(p.env_release.max(1.0) * 0.001, sr);
        let drive = super::db(p.drive.clamp(0.0, 36.0));
        let use_drive = p.drive > 0.01;
        let drive_comp = 1.0 / drive.sqrt();
        let kind = p.kind();
        let steep = p.steep();
        let q = p.q();
        let weights = p.morph_weights();
        for i in 0..frames {
            self.log_freq += (target_log - self.log_freq) * smooth;
            self.mix += (target_mix - self.mix) * smooth;
            let x = [left[i], right[i]];
            let e = self.env.tick((x[0] + x[1]) * 0.5, att, rel);
            let e_norm = if e > 1e-5 {
                ((20.0 * e.log10() + 40.0) / 40.0).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let ph = (start + inc * i as f64) as f32;
            let mut y = [0.0f32; 2];
            for c in 0..2 {
                let lfo = if lfo_oct > 0.0 {
                    let phase = wrap(ph + if c == 1 { offset } else { 0.0 });
                    self.voices[c].value(shape, phase)
                } else {
                    0.0
                };
                let oct = self.log_freq + lfo * lfo_oct + e_norm * env_oct;
                let cutoff = oct.clamp(3.3, 15.0).exp2();
                let mut s = x[c];
                if use_drive {
                    s = soft_clip(s * drive) * drive_comp;
                }
                if steep {
                    let c1 = SvfCoefs::new(cutoff, 0.5412, sr);
                    s = Self::stage(&mut self.stages[c][0], &c1, s, kind, weights);
                }
                let c2 = SvfCoefs::new(cutoff, q, sr);
                s = Self::stage(&mut self.stages[c][1], &c2, s, kind, weights);
                y[c] = s;
            }
            let m = self.mix;
            left[i] = x[0] + (y[0] - x[0]) * m;
            right[i] = x[1] + (y[1] - x[1]) * m;
        }
    }

    #[inline]
    fn stage(st: &mut SvfState, c: &SvfCoefs, x: f32, kind: usize, w: [f32; 3]) -> f32 {
        let (lo, band, hi) = st.tick(c, x);
        match kind {
            TYPE_LOW => lo,
            TYPE_HIGH => hi,
            TYPE_BAND => band * c.k,
            TYPE_NOTCH => lo + hi,
            _ => w[0] * lo + w[1] * band * c.k + w[2] * hi,
        }
    }
}

impl super::EffectRuntime for AutoFilter {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / sr).sin() * 0.5)
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    /// Steady-state gain of the filter for a sine at `freq`.
    fn run(p: &AutoFilterParams, freq: f32) -> f32 {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let mut f = AutoFilter::new(sr);
        let input = sine(freq, sr, 24000);
        let mut l = input.clone();
        let mut r = l.clone();
        for (a, b) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
            f.process(p, a, b, &ctx);
        }
        rms(&l[12000..]) / rms(&input[12000..])
    }

    #[test]
    fn lowpass_and_highpass_do_their_job() {
        let lp = AutoFilterParams {
            freq: 1000.0,
            resonance: 0.0,
            ..Default::default()
        };
        assert!(run(&lp, 100.0) > 0.9);
        assert!(run(&lp, 8000.0) < 0.03);
        let hp = AutoFilterParams {
            filter_type: TYPE_HIGH as f32,
            ..lp
        };
        assert!(run(&hp, 100.0) < 0.03);
        assert!(run(&hp, 8000.0) > 0.9);
    }

    #[test]
    fn steeper_slope_attenuates_more() {
        let p12 = AutoFilterParams {
            freq: 1000.0,
            resonance: 0.0,
            ..Default::default()
        };
        let p24 = AutoFilterParams { slope: 1.0, ..p12 };
        let a12 = run(&p12, 4000.0);
        let a24 = run(&p24, 4000.0);
        assert!(a24 < a12 * 0.3, "{a12} {a24}");
    }

    #[test]
    fn bandpass_and_notch() {
        let bp = AutoFilterParams {
            filter_type: TYPE_BAND as f32,
            freq: 1000.0,
            resonance: 0.4,
            ..Default::default()
        };
        assert!(run(&bp, 1000.0) > 0.8);
        assert!(run(&bp, 100.0) < 0.2);
        assert!(run(&bp, 10000.0) < 0.2);
        let notch = AutoFilterParams {
            filter_type: TYPE_NOTCH as f32,
            resonance: 0.0,
            ..bp
        };
        assert!(run(&notch, 1000.0) < 0.05);
        assert!(run(&notch, 100.0) > 0.9);
    }

    #[test]
    fn morph_moves_from_lowpass_to_highpass() {
        let m = AutoFilterParams {
            filter_type: TYPE_MORPH as f32,
            freq: 1000.0,
            resonance: 0.0,
            morph: 0.0,
            ..Default::default()
        };
        assert!(run(&m, 100.0) > 0.9 && run(&m, 8000.0) < 0.05);
        let m1 = AutoFilterParams { morph: 1.0, ..m };
        assert!(run(&m1, 100.0) < 0.05 && run(&m1, 8000.0) > 0.9);
        let mid = AutoFilterParams { morph: 0.5, ..m };
        assert!(run(&mid, 1000.0) > 0.9 && run(&mid, 100.0) < 0.3);
    }

    #[test]
    fn response_curve_matches_dsp() {
        for kind in 0..5 {
            let p = AutoFilterParams {
                filter_type: kind as f32,
                freq: 1000.0,
                resonance: 0.5,
                slope: 1.0,
                morph: 0.3,
                ..Default::default()
            };
            for f in [200.0, 700.0, 1500.0, 3000.0] {
                let measured = 20.0 * run(&p, f).max(1e-6).log10();
                let predicted = p.response_db(f, 48000.0);
                if predicted > -40.0 {
                    assert!(
                        (measured - predicted).abs() < 1.0,
                        "{kind} {f}: {measured} vs {predicted}"
                    );
                }
            }
        }
    }

    #[test]
    fn resonance_peaks_at_cutoff() {
        let p = AutoFilterParams {
            freq: 1000.0,
            resonance: 0.8,
            ..Default::default()
        };
        assert!(run(&p, 1000.0) > 3.0);
    }

    #[test]
    fn synced_lfo_follows_song_position() {
        // The LFO phase is a pure function of song_beat while playing.
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0).with_transport(true, 3.5, 4, false);
        let mut c = LfoClock::default();
        let (start, inc) = c.block(true, 1.0, SYNC_QUARTER, &ctx, 256);
        assert!((start - 0.5).abs() < 1e-9);
        assert!((inc - 2.0 / 48000.0).abs() < 1e-12);
        let ctx = ctx.with_transport(true, 6.0, 4, false);
        let (start, _) = c.block(true, 1.0, SYNC_BAR, &ctx, 256);
        assert!((start - 0.5).abs() < 1e-9);
        // Free-running continues smoothly across blocks.
        let ctx = FxContext::new(sr, 120.0);
        let mut c = LfoClock::default();
        let (s0, inc) = c.block(false, 2.0, 0.0, &ctx, 100);
        let (s1, _) = c.block(false, 2.0, 0.0, &ctx, 100);
        assert!((s1 - s0 - inc * 100.0).abs() < 1e-9);
    }

    #[test]
    fn lfo_sweeps_the_cutoff() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = AutoFilterParams {
            freq: 600.0,
            resonance: 0.0,
            lfo_amount: 1.0,
            lfo_rate: 2.0,
            ..Default::default()
        };
        let mut f = AutoFilter::new(sr);
        let mut l = sine(3000.0, sr, 48000);
        let mut r = l.clone();
        for (a, b) in l.chunks_mut(64).zip(r.chunks_mut(64)) {
            f.process(&p, a, b, &ctx);
        }
        // Loudness varies strongly over the LFO cycle.
        let levels: Vec<f32> = l.chunks(1200).map(rms).collect();
        let max = levels.iter().cloned().fold(0.0, f32::max);
        let min = levels.iter().cloned().fold(1.0, f32::min);
        assert!(max > min * 10.0, "{min} {max}");
    }

    #[test]
    fn stereo_phase_offsets_the_channels() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = AutoFilterParams {
            freq: 600.0,
            resonance: 0.0,
            lfo_amount: 1.0,
            lfo_rate: 2.0,
            lfo_phase: 180.0,
            ..Default::default()
        };
        let mut f = AutoFilter::new(sr);
        let mut l = sine(3000.0, sr, 24000);
        let mut r = l.clone();
        f.process(&p, &mut l, &mut r, &ctx);
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 500.0, "{diff}");
    }

    #[test]
    fn envelope_opens_the_filter() {
        let p = AutoFilterParams {
            freq: 200.0,
            resonance: 0.0,
            env_amount: 1.0,
            ..Default::default()
        };
        assert!(run(&p, 3000.0) > 0.5);
        let closed = AutoFilterParams { env_amount: 0.0, ..p };
        assert!(run(&closed, 3000.0) < 0.05);
    }

    #[test]
    fn extremes_stay_finite() {
        let sr = 44100.0;
        let ctx = FxContext::new(sr, 300.0);
        for kind in 0..5 {
            let p = AutoFilterParams {
                filter_type: kind as f32,
                slope: 1.0,
                freq: 20000.0,
                resonance: 1.0,
                drive: 24.0,
                lfo_amount: 1.0,
                lfo_rate: 40.0,
                env_amount: 1.0,
                ..Default::default()
            };
            let mut f = AutoFilter::new(sr);
            let mut l: Vec<f32> = (0..20000).map(|i| if i % 50 < 25 { 1.0 } else { -1.0 }).collect();
            let mut r = l.clone();
            f.process(&p, &mut l[..1], &mut r[..1], &ctx);
            f.process(&p, &mut l[1..], &mut r[1..], &ctx);
            f.process(&p, &mut [], &mut [], &ctx);
            assert!(l.iter().chain(r.iter()).all(|x| x.is_finite() && x.abs() < 50.0));
        }
    }

    #[test]
    fn cubic_read_interpolates() {
        let mut buf = vec![0.0f32; 64];
        for (i, v) in buf.iter_mut().enumerate() {
            *v = i as f32;
        }
        // Next write slot 40: delay 5.5 lies between samples 35 and 34.
        let v = read_cubic(&buf, 40, 5.5);
        assert!((v - 34.5).abs() < 1e-4, "{v}");
    }

    #[test]
    fn random_lfo_changes_each_cycle() {
        let mut v = LfoVoice::new(7);
        let a = v.value(SHAPE_SAMPLE_HOLD, 0.9);
        let b = v.value(SHAPE_SAMPLE_HOLD, 0.1);
        assert!(a != b);
        for s in 0..7 {
            for i in 0..100 {
                let x = v.value(s, i as f32 / 100.0);
                assert!((-1.0..=1.0).contains(&x), "{s} {x}");
            }
        }
    }
}
