//! Grain Delay: the delayed signal is chopped into overlapping windowed
//! grains, each of which can be re-pitched, scattered in time ("Spray") and
//! given a random pitch offset, and fed back into the delay line.
//!
//! Grains start at the grain frequency (1..150 per second) and last two grain
//! periods (capped so they stay within the line), so with no spray their
//! sin^2 windows overlap-add to exactly one and pitch 0 gives a clean delay.
//! A grain reads at `ratio` times real-time speed, which shifts its pitch;
//! a grain that reads faster than the write head starts far enough back that
//! it never overtakes it.

use super::auto_filter::soft_clip;
use super::echo::{div_beats, ECHO_DIVS};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

/// Longest delay time in seconds.
pub const MAX_SECONDS: f32 = 2.0;
const MAX_SPRAY_SECONDS: f32 = 0.5;
/// Longest grain in seconds.
const MAX_GRAIN_SECONDS: f32 = 0.4;
/// Bound on how far a fast grain may start behind the delay time.
const MAX_LEAD_SECONDS: f32 = 0.6;
const MAX_GRAINS: usize = 8;

device_params! {
    pub struct GrainDelayParams {
        sync: "Sync", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        div: "Time", 0.0, 17.0, 7.0, ParamKind::Choice(ECHO_DIVS), Unit::Plain;
        time: "Time", 1.0, 2000.0, 120.0, LOG, Unit::Ms;
        feedback: "Feedback", 0.0, 0.95, 0.3, LIN, Unit::Percent;
        pitch: "Pitch", -24.0, 24.0, 0.0, LIN, Unit::Semitones;
        frequency: "Frequency", 1.0, 150.0, 60.0, LOG, Unit::Hz;
        spray: "Spray", 0.0, 500.0, 2.0, LIN, Unit::Ms;
        random_pitch: "Rand Pitch", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        mix: "Dry/Wet", 0.0, 1.0, 0.5, LIN, Unit::Percent;
    }
}

impl GrainDelayParams {
    /// Delay time in seconds.
    pub fn delay_seconds(&self, tempo: f64) -> f32 {
        let t = if self.sync >= 0.5 {
            (div_beats(self.div) * 60.0 / tempo.max(1.0)) as f32
        } else {
            self.time * 0.001
        };
        t.clamp(0.001, MAX_SECONDS)
    }
}

impl super::EffectParams for GrainDelayParams {
    fn is_relevant(&self, index: usize) -> bool {
        let synced = self.sync >= 0.5;
        match index {
            1 => synced,
            2 => !synced,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Octave Shimmer",
                Self {
                    sync: 1.0,
                    div: 10.0,
                    pitch: 12.0,
                    feedback: 0.55,
                    frequency: 20.0,
                    spray: 15.0,
                    mix: 0.35,
                    ..d
                },
            ),
            (
                "Granular Cloud",
                Self {
                    time: 250.0,
                    feedback: 0.6,
                    frequency: 12.0,
                    spray: 220.0,
                    random_pitch: 0.25,
                    mix: 0.5,
                    ..d
                },
            ),
            (
                "Down Fifth Echo",
                Self {
                    sync: 1.0,
                    div: 8.0,
                    pitch: -7.0,
                    feedback: 0.45,
                    frequency: 40.0,
                    mix: 0.4,
                    ..d
                },
            ),
            (
                "Glitch Stutter",
                Self {
                    time: 30.0,
                    feedback: 0.7,
                    frequency: 4.0,
                    spray: 60.0,
                    random_pitch: 0.6,
                    mix: 0.5,
                    ..d
                },
            ),
        ]
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Grain {
    active: bool,
    age: f32,
    len: f32,
    /// Change of the delay per sample (1 - ratio).
    slope: f32,
    /// Delay at the grain's start, per channel, in samples.
    start: [f32; 2],
}

pub struct GrainDelay {
    sr: f32,
    buf: [Vec<f32>; 2],
    pos: usize,
    grains: [Grain; MAX_GRAINS],
    countdown: f32,
    rng: Rng,
    mix: f32,
    started: bool,
}

impl GrainDelay {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let secs = MAX_SECONDS + MAX_SPRAY_SECONDS + MAX_LEAD_SECONDS + MAX_GRAIN_SECONDS * 0.75 + 0.05;
        let n = (secs * sr) as usize + 16;
        Self {
            sr,
            buf: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            grains: [Grain::default(); MAX_GRAINS],
            countdown: 0.0,
            rng: Rng::new(0x9a1d_e1a7),
            mix: 0.5,
            started: false,
        }
    }

    pub fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
        self.grains = [Grain::default(); MAX_GRAINS];
        self.countdown = 0.0;
    }

    fn spawn(&mut self, p: &GrainDelayParams, delay: f32, period: f32) {
        let sr = self.sr;
        let rand_st = p.random_pitch.clamp(0.0, 1.0) * 12.0 * self.rng.next_bipolar();
        let st = (p.pitch.clamp(-24.0, 24.0) + rand_st).clamp(-36.0, 36.0);
        let ratio = (st / 12.0).exp2();
        let mut len = (2.0 * period).min(MAX_GRAIN_SECONDS * sr);
        let lead_per_sample = (ratio - 1.0).abs();
        if lead_per_sample > 1e-6 {
            len = len.min(MAX_LEAD_SECONDS * sr / lead_per_sample);
        }
        let len = len.max(16.0);
        let lead = ((ratio - 1.0) * len).max(0.0);
        let spray = p.spray.clamp(0.0, MAX_SPRAY_SECONDS * 1000.0) * 0.001 * sr;
        let mut start = [0.0f32; 2];
        for s in &mut start {
            let jitter = spray * (0.5 + 0.5 * self.rng.next_bipolar());
            *s = (delay + jitter + lead).max(2.0);
        }
        let grain = Grain {
            active: true,
            age: 0.0,
            len,
            slope: 1.0 - ratio,
            start,
        };
        // A free slot, or else the oldest grain.
        let slot = self.grains.iter().position(|g| !g.active).unwrap_or_else(|| {
            let mut best = 0;
            for (i, g) in self.grains.iter().enumerate() {
                if g.age / g.len > self.grains[best].age / self.grains[best].len {
                    best = i;
                }
            }
            best
        });
        self.grains[slot] = grain;
    }

    pub fn process(&mut self, p: &GrainDelayParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let sr = self.sr;
        let frames = left.len().min(right.len());
        let n = self.buf[0].len();
        let max_d = (n - 8) as f32;
        let delay = (p.delay_seconds(ctx.tempo) * sr).max(2.0);
        let period = sr / p.frequency.clamp(0.5, 200.0);
        let fb = p.feedback.clamp(0.0, 0.95);
        let target_mix = p.mix.clamp(0.0, 1.0);
        if !self.started {
            self.started = true;
            self.mix = target_mix;
        }
        let smooth = 1.0 - super::smoothing(0.02, sr);
        for k in 0..frames {
            self.mix += (target_mix - self.mix) * smooth;
            if self.countdown <= 0.0 {
                self.spawn(p, delay, period);
                self.countdown += period;
            }
            self.countdown -= 1.0;
            let mut wet = [0.0f32; 2];
            for g in self.grains.iter_mut().filter(|g| g.active) {
                let w = (std::f32::consts::PI * g.age / g.len).sin().powi(2);
                for c in 0..2 {
                    let d = (g.start[c] + g.slope * g.age).clamp(2.0, max_d);
                    wet[c] += w * super::auto_filter::read_cubic(&self.buf[c], self.pos, d);
                }
                g.age += 1.0;
                if g.age >= g.len {
                    g.active = false;
                }
            }
            let x = [left[k], right[k]];
            for c in 0..2 {
                let v = x[c] + soft_clip(wet[c] * fb);
                self.buf[c][self.pos] = if v.abs() < 1e-20 { 0.0 } else { v };
            }
            self.pos += 1;
            if self.pos == n {
                self.pos = 0;
            }
            let m = self.mix;
            left[k] = x[0] + (wet[0] - x[0]) * m;
            right[k] = x[1] + (wet[1] - x[1]) * m;
        }
    }
}

impl super::EffectRuntime for GrainDelay {}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(x: &[f32], freq: f32, sr: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            let ph = i as f64 * freq as f64 * std::f64::consts::TAU / sr as f64;
            re += *v as f64 * ph.cos();
            im += *v as f64 * ph.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    fn sine(freq: f32, sr: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / sr).sin() * 0.5)
            .collect()
    }

    #[test]
    fn plain_grains_are_a_clean_delay() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = GrainDelayParams {
            time: 100.0,
            feedback: 0.0,
            spray: 0.0,
            mix: 1.0,
            ..Default::default()
        };
        let mut g = GrainDelay::new(sr);
        let input = sine(330.0, sr, 24000);
        let mut l = input.clone();
        let mut r = input.clone();
        for (a, b) in l.chunks_mut(200).zip(r.chunks_mut(200)) {
            g.process(&p, a, b, &ctx);
        }
        // After the first full grain overlap, output == input delayed 4800.
        for i in 8000..24000 {
            assert!(
                (l[i] - input[i - 4800]).abs() < 1e-3,
                "{i}: {} {}",
                l[i],
                input[i - 4800]
            );
        }
    }

    #[test]
    fn synced_time() {
        let p = GrainDelayParams {
            sync: 1.0,
            div: 10.0,
            ..Default::default()
        };
        assert!((p.delay_seconds(120.0) - 0.5).abs() < 1e-6);
        // Clamped to the line length.
        let slow = GrainDelayParams { div: 17.0, ..p };
        assert!(slow.delay_seconds(60.0) <= MAX_SECONDS);
    }

    #[test]
    fn pitch_shifts_grains() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        for (st, out) in [(12.0, 880.0), (-12.0, 220.0), (7.0, 440.0 * 1.4983)] {
            let p = GrainDelayParams {
                time: 50.0,
                feedback: 0.0,
                spray: 0.0,
                pitch: st,
                frequency: 30.0,
                mix: 1.0,
                ..Default::default()
            };
            let mut g = GrainDelay::new(sr);
            let mut l = sine(440.0, sr, 48000);
            let mut r = l.clone();
            g.process(&p, &mut l, &mut r, &ctx);
            // Grains are not phase-coherent with each other, so measure in
            // short windows and average.
            let avg = |f: f32| {
                let chunks: Vec<f32> = l[12000..].chunks(1200).map(|c| level(c, f, sr)).collect();
                chunks.iter().sum::<f32>() / chunks.len() as f32
            };
            let shifted = avg(out);
            let orig = avg(440.0);
            assert!(shifted > 0.25 && shifted > orig * 5.0, "{st}: {shifted} {orig}");
        }
    }

    #[test]
    fn feedback_repeats_decay() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = GrainDelayParams {
            time: 200.0,
            feedback: 0.5,
            spray: 0.0,
            frequency: 20.0,
            mix: 1.0,
            ..Default::default()
        };
        let mut g = GrainDelay::new(sr);
        // A 40 ms burst.
        let mut l: Vec<f32> = sine(500.0, sr, 48000)
            .into_iter()
            .enumerate()
            .map(|(i, v)| if i < 1920 { v } else { 0.0 })
            .collect();
        let mut r = l.clone();
        g.process(&p, &mut l, &mut r, &ctx);
        let energy = |a: usize, b: usize| l[a..b].iter().map(|x| x * x).sum::<f32>();
        let e1 = energy(9600, 9600 + 3000);
        let e2 = energy(19200, 19200 + 3000);
        assert!(energy(0, 9000) < 1e-6);
        assert!(e1 > 1.0 && e2 < e1 * 0.5 && e2 > e1 * 0.05, "{e1} {e2}");
    }

    #[test]
    fn spray_and_random_pitch_stay_bounded() {
        let sr = 192000.0;
        let ctx = FxContext::new(sr, 120.0);
        for freq in [1.0, 150.0] {
            let p = GrainDelayParams {
                time: 2000.0,
                feedback: 0.95,
                pitch: 24.0,
                frequency: freq,
                spray: 500.0,
                random_pitch: 1.0,
                mix: 1.0,
                ..Default::default()
            };
            let mut g = GrainDelay::new(sr);
            let mut l: Vec<f32> = (0..200000).map(|i| if i % 300 < 150 { 1.0 } else { -1.0 }).collect();
            let mut r = l.clone();
            g.process(&p, &mut l[..1], &mut r[..1], &ctx);
            g.process(&p, &mut l[1..], &mut r[1..], &ctx);
            let low = GrainDelayParams { pitch: -24.0, ..p };
            g.process(&low, &mut l, &mut r, &ctx);
            assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 20.0));
        }
    }
}
