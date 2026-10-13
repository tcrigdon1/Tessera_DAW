//! Phaser-Flanger: three classic modulation effects in one device.
//!
//! - Phaser: a chain of first-order allpass filters (two per notch) whose
//!   corner frequencies are swept by an LFO and an envelope follower; mixing
//!   the chain with the dry signal carves moving notches.
//! - Flanger: a very short delay swept over a wide range with positive or
//!   negative feedback (comb filter).
//! - Doubler: a longer, gently and randomly wandering delay per channel that
//!   thickens a part like a second take.

use super::auto_filter::{
    degrees, wrap, EnvFollower, LfoClock, LfoVoice, SvfCoefs, SvfState, LFO_SHAPES, SHAPE_RANDOM, SYNC_BAR, SYNC_RATES,
};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const MODES: &[&str] = &["Phaser", "Flanger", "Doubler"];
pub const MODE_PHASER: usize = 0;
pub const MODE_FLANGER: usize = 1;
pub const MODE_DOUBLER: usize = 2;
/// Most notches (each uses two allpass stages).
pub const MAX_NOTCHES: usize = 12;
const MAX_STAGES: usize = MAX_NOTCHES * 2;
/// Longest delay in the flanger / doubler line.
const MAX_DELAY_MS: f32 = 60.0;
/// Phaser sweep range (octaves each way) at full amount.
const PHASER_OCTAVES: f32 = 3.0;
/// Flanger sweep range (octaves of delay time each way) at full amount.
const FLANGER_OCTAVES: f32 = 2.0;
/// Coefficient update interval for the phaser.
const CONTROL: usize = 16;

device_params! {
    pub struct PhaserFlangerParams {
        mode: "Mode", 0.0, 2.0, 0.0, ParamKind::Choice(MODES), Unit::Plain;
        notches: "Notches", 2.0, 12.0, 4.0, LIN, Unit::Integer;
        center: "Frequency", 50.0, 18000.0, 800.0, LOG, Unit::Hz;
        time: "Time", 0.1, 40.0, 2.5, LOG, Unit::Ms;
        feedback: "Feedback", -0.95, 0.95, 0.4, LIN, Unit::Percent;
        spread: "Spread", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        sync: "Sync", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        rate: "Rate", 0.01, 20.0, 0.4, LOG, Unit::Hz;
        div: "Rate", 0.0, 16.0, SYNC_BAR, ParamKind::Choice(SYNC_RATES), Unit::Plain;
        lfo_shape: "LFO Shape", 0.0, 6.0, 0.0, ParamKind::Choice(LFO_SHAPES), Unit::Plain;
        amount: "Amount", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        stereo: "Stereo", 0.0, 360.0, 90.0, LIN, Unit::Integer;
        env_amount: "Env Mod", -1.0, 1.0, 0.0, LIN, Unit::Percent;
        safe_bass: "Safe Bass", 5.0, 3000.0, 50.0, LOG, Unit::Hz;
        mix: "Dry/Wet", 0.0, 1.0, 0.5, LIN, Unit::Percent;
    }
}

impl PhaserFlangerParams {
    pub fn mode_index(&self) -> usize {
        (self.mode.round().max(0.0) as usize).min(MODE_DOUBLER)
    }

    fn notch_count(&self) -> usize {
        (self.notches.round().max(1.0) as usize).min(MAX_NOTCHES)
    }
}

impl super::EffectParams for PhaserFlangerParams {
    fn is_relevant(&self, index: usize) -> bool {
        let mode = self.mode_index();
        let synced = self.sync >= 0.5;
        match index {
            1 | 2 | 5 => mode == MODE_PHASER,
            3 => mode != MODE_PHASER,
            4 => mode != MODE_DOUBLER,
            7 => !synced,
            8 => synced,
            9 => mode != MODE_DOUBLER,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Classic Phaser",
                Self {
                    notches: 3.0,
                    center: 700.0,
                    feedback: 0.55,
                    rate: 0.25,
                    amount: 0.7,
                    ..d
                },
            ),
            (
                "Jet Flanger",
                Self {
                    mode: MODE_FLANGER as f32,
                    time: 2.0,
                    feedback: 0.8,
                    sync: 1.0,
                    div: 14.0,
                    lfo_shape: 1.0,
                    amount: 0.8,
                    ..d
                },
            ),
            (
                "Negative Flange",
                Self {
                    mode: MODE_FLANGER as f32,
                    time: 1.2,
                    feedback: -0.7,
                    rate: 0.15,
                    amount: 0.6,
                    ..d
                },
            ),
            (
                "Vocal Doubler",
                Self {
                    mode: MODE_DOUBLER as f32,
                    time: 14.0,
                    rate: 1.2,
                    amount: 0.6,
                    mix: 0.4,
                    safe_bass: 120.0,
                    ..d
                },
            ),
        ]
    }
}

pub struct PhaserFlanger {
    sr: f32,
    clock: LfoClock,
    voices: [LfoVoice; 2],
    env: EnvFollower,
    /// Allpass states and current coefficients per channel.
    ap: [[f32; MAX_STAGES]; 2],
    coef: [[f32; MAX_STAGES]; 2],
    last: [f32; 2],
    buf: [Vec<f32>; 2],
    pos: usize,
    /// Smoothed delay time in samples per channel.
    delay: [f32; 2],
    bass: [SvfState; 2],
    started: bool,
}

impl PhaserFlanger {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let n = (MAX_DELAY_MS * 0.001 * sr) as usize + 16;
        Self {
            sr,
            clock: LfoClock::default(),
            voices: [LfoVoice::new(0x0f0f_1234), LfoVoice::new(0x7777_abcd)],
            env: EnvFollower::default(),
            ap: [[0.0; MAX_STAGES]; 2],
            coef: [[0.0; MAX_STAGES]; 2],
            last: [0.0; 2],
            buf: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            delay: [0.0; 2],
            bass: [SvfState::default(); 2],
            started: false,
        }
    }

    pub fn reset(&mut self) {
        self.ap = [[0.0; MAX_STAGES]; 2];
        self.last = [0.0; 2];
        for b in &mut self.buf {
            b.fill(0.0);
        }
        for s in &mut self.bass {
            s.reset();
        }
        self.env = EnvFollower::default();
        self.clock.reset();
    }

    /// Allpass coefficient for a corner frequency.
    #[inline]
    fn ap_coef(freq: f32, sr: f32) -> f32 {
        let t = (std::f32::consts::PI * freq.clamp(10.0, sr * 0.46) / sr).tan();
        (t - 1.0) / (t + 1.0)
    }

    pub fn process(&mut self, p: &PhaserFlangerParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let sr = self.sr;
        let frames = left.len().min(right.len());
        let mode = p.mode_index();
        let (start, inc) = self.clock.block(p.sync >= 0.5, p.rate, p.div, ctx, frames);
        let shape = if mode == MODE_DOUBLER {
            SHAPE_RANDOM
        } else {
            (p.lfo_shape.round().max(0.0) as usize).min(SHAPE_RANDOM)
        };
        let amount = p.amount.clamp(0.0, 1.0);
        let stereo = degrees(p.stereo);
        let env_amt = p.env_amount.clamp(-1.0, 1.0);
        let att = super::smoothing(0.003, sr);
        let rel = super::smoothing(0.15, sr);
        let fb = if mode == MODE_DOUBLER {
            0.0
        } else {
            p.feedback.clamp(-0.95, 0.95)
        };
        let mix = p.mix.clamp(0.0, 1.0);
        let bass = SvfCoefs::new(p.safe_bass, std::f32::consts::FRAC_1_SQRT_2, sr);
        let use_bass = p.safe_bass > 6.0;
        let stages = p.notch_count() * 2;
        let spread = p.spread.clamp(0.0, 1.0);
        let n = self.buf[0].len();
        let ms = sr * 0.001;
        let max_d = MAX_DELAY_MS * ms;
        let glide = 1.0 - super::smoothing(0.02, sr);

        let mut i = 0;
        while i < frames {
            let len = CONTROL.min(frames - i);
            // Control-rate modulation for this slice.
            let ph = (start + inc * i as f64) as f32;
            let e = self.env.env;
            let e_norm = if e > 1e-5 {
                ((20.0 * e.log10() + 40.0) / 40.0).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let mut lfo = [0.0f32; 2];
            for c in 0..2 {
                lfo[c] = self.voices[c].value(shape, wrap(ph + if c == 1 { stereo } else { 0.0 }));
            }
            let mut target_delay = [0.0f32; 2];
            if mode == MODE_PHASER {
                for c in 0..2 {
                    let oct = lfo[c] * amount * PHASER_OCTAVES + e_norm * env_amt * 4.0;
                    let f = p.center * oct.clamp(-10.0, 10.0).exp2();
                    for s in 0..stages {
                        let t = if stages > 1 {
                            s as f32 / (stages - 1) as f32 - 0.5
                        } else {
                            0.0
                        };
                        self.coef[c][s] = Self::ap_coef(f * (t * spread * 3.0).exp2(), sr);
                    }
                }
            } else {
                for c in 0..2 {
                    let d_ms = if mode == MODE_FLANGER {
                        let oct = lfo[c] * amount * FLANGER_OCTAVES - e_norm * env_amt * 2.0;
                        p.time * oct.clamp(-8.0, 8.0).exp2()
                    } else {
                        // The doubler's right channel sits a little later.
                        let base = p.time * if c == 1 { 1.37 } else { 1.0 };
                        base * (1.0 + 0.25 * amount * lfo[c]) + e_norm * env_amt * 3.0
                    };
                    target_delay[c] = (d_ms * ms).clamp(2.0, max_d);
                }
                if !self.started {
                    self.delay = target_delay;
                }
            }
            self.started = true;

            for k in i..i + len {
                let x = [left[k], right[k]];
                self.env.tick((x[0] + x[1]) * 0.5, att, rel);
                let mut wet = [0.0f32; 2];
                for c in 0..2 {
                    let w = if mode == MODE_PHASER {
                        let mut s = x[c] + super::auto_filter::soft_clip(self.last[c] * fb);
                        for st in 0..stages {
                            let a = self.coef[c][st];
                            let y = a * s + self.ap[c][st];
                            self.ap[c][st] = s - a * y;
                            s = y;
                        }
                        if self.ap[c][0].abs() < 1e-20 {
                            self.ap[c] = [0.0; MAX_STAGES];
                        }
                        self.last[c] = s;
                        s
                    } else {
                        self.delay[c] += (target_delay[c] - self.delay[c]) * glide;
                        let y = super::auto_filter::read_cubic(&self.buf[c], self.pos, self.delay[c]);
                        let v = x[c] + super::auto_filter::soft_clip(y * fb);
                        self.buf[c][self.pos] = if v.abs() < 1e-20 { 0.0 } else { v };
                        y
                    };
                    wet[c] = if use_bass { self.bass[c].tick(&bass, w).2 } else { w };
                }
                if mode != MODE_PHASER {
                    self.pos += 1;
                    if self.pos == n {
                        self.pos = 0;
                    }
                }
                left[k] = x[0] * (1.0 - mix) + wet[0] * mix;
                right[k] = x[1] * (1.0 - mix) + wet[1] * mix;
            }
            i += len;
        }
    }
}

impl super::EffectRuntime for PhaserFlanger {}

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

    fn gain(p: &PhaserFlangerParams, freq: f32) -> f32 {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let mut fx = PhaserFlanger::new(sr);
        let input = sine(freq, sr, 24000);
        let mut l = input.clone();
        let mut r = input.clone();
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            fx.process(p, a, b, &ctx);
        }
        rms(&l[12000..]) / rms(&input[12000..])
    }

    #[test]
    fn static_phaser_has_notches() {
        let p = PhaserFlangerParams {
            notches: 2.0,
            spread: 0.0,
            center: 1000.0,
            amount: 0.0,
            feedback: 0.0,
            safe_bass: 5.0,
            ..Default::default()
        };
        // Two notches => four stages => 180 degrees at the frequency where each
        // stage gives 45 degrees, i.e. below the corner, and 540 above.
        let pi = std::f32::consts::PI;
        let notch = ((pi * 1000.0 / 48000.0).tan() * (pi / 8.0).tan()).atan() * 48000.0 / pi;
        let g_notch = gain(&p, notch);
        let g_pass = gain(&p, 1000.0);
        assert!(g_notch < 0.05, "notch {g_notch}");
        assert!(g_pass > 0.9, "pass {g_pass}");
    }

    #[test]
    fn phaser_sweeps_over_time() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = PhaserFlangerParams {
            rate: 2.0,
            amount: 1.0,
            feedback: 0.5,
            ..Default::default()
        };
        let mut fx = PhaserFlanger::new(sr);
        let mut l = sine(600.0, sr, 48000);
        let mut r = l.clone();
        fx.process(&p, &mut l, &mut r, &ctx);
        let levels: Vec<f32> = l.chunks(1200).skip(2).map(rms).collect();
        let max = levels.iter().cloned().fold(0.0, f32::max);
        let min = levels.iter().cloned().fold(1.0, f32::min);
        assert!(max > min * 3.0, "{min} {max}");
        // Stereo offset: channels differ.
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 100.0);
    }

    #[test]
    fn flanger_is_a_comb_at_the_delay_time() {
        // Static 1 ms flanger at 50% mix: notch at 500 Hz, peak at 1 kHz.
        let p = PhaserFlangerParams {
            mode: MODE_FLANGER as f32,
            time: 1.0,
            amount: 0.0,
            feedback: 0.0,
            safe_bass: 5.0,
            ..Default::default()
        };
        assert!(gain(&p, 500.0) < 0.05);
        assert!(gain(&p, 1000.0) > 0.95);
        // Negative feedback moves the resonance.
        let neg = PhaserFlangerParams { feedback: -0.8, ..p };
        assert!(gain(&neg, 1000.0) < gain(&p, 1000.0));
    }

    #[test]
    fn safe_bass_keeps_lows_dry() {
        let p = PhaserFlangerParams {
            mode: MODE_FLANGER as f32,
            time: 10.0,
            amount: 0.0,
            feedback: 0.0,
            safe_bass: 1000.0,
            mix: 1.0,
            ..Default::default()
        };
        assert!(gain(&p, 60.0) < 0.05);
        assert!(gain(&p, 8000.0) > 0.9);
    }

    #[test]
    fn doubler_widens() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = PhaserFlangerParams {
            mode: MODE_DOUBLER as f32,
            time: 12.0,
            mix: 0.5,
            ..Default::default()
        };
        let mut fx = PhaserFlanger::new(sr);
        let mut l = sine(440.0, sr, 24000);
        let mut r = l.clone();
        fx.process(&p, &mut l, &mut r, &ctx);
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 500.0, "{diff}");
        assert!(l.iter().all(|x| x.is_finite() && x.abs() < 1.0));
    }

    #[test]
    fn extremes_stay_finite() {
        let sr = 192000.0;
        let ctx = FxContext::new(sr, 120.0);
        for mode in 0..3 {
            for fb in [-0.95, 0.95] {
                let p = PhaserFlangerParams {
                    mode: mode as f32,
                    notches: 12.0,
                    center: 18000.0,
                    time: 40.0,
                    feedback: fb,
                    spread: 1.0,
                    rate: 20.0,
                    amount: 1.0,
                    env_amount: 1.0,
                    mix: 1.0,
                    ..Default::default()
                };
                let mut fx = PhaserFlanger::new(sr);
                let mut l: Vec<f32> = (0..30000).map(|i| if i % 64 < 32 { 1.0 } else { -1.0 }).collect();
                let mut r = l.clone();
                fx.process(&p, &mut l[..1], &mut r[..1], &ctx);
                fx.process(&p, &mut l[1..], &mut r[1..], &ctx);
                assert!(
                    l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 20.0),
                    "{mode} {fb}"
                );
            }
        }
    }
}
