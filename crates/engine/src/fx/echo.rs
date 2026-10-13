//! Echo: a characterful tape-style stereo delay.
//!
//! Signal flow per channel:
//!
//! ```text
//! in -> input gain -> gate -> [reverb: pre] -> + -> tape line -> HP/LP (resonant, modulated)
//!                                              ^                          |
//!                                              +-- feedback, saturation --+-- [reverb: feedback]
//!                                                                         |
//!                                                     [reverb: post] -> ducking -> width -> wet
//! ```
//!
//! The delay time glides to new settings and is read with cubic
//! interpolation, so time changes bend the pitch like tape instead of
//! clicking. A sine LFO ("Modulation") and a slow random drift plus flutter
//! ("Wobble") move the time; the LFO can also move the filters. Hiss
//! ("Noise") is recorded onto the loop.

use super::auto_filter::{read_cubic, soft_clip, EnvFollower, LfoVoice, SvfCoefs, SvfState, SHAPE_RANDOM};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

/// Synced delay times: triplet, straight and dotted for each note value.
pub const ECHO_DIVS: &[&str] = &[
    "1/32T", "1/32", "1/32D", "1/16T", "1/16", "1/16D", "1/8T", "1/8", "1/8D", "1/4T", "1/4", "1/4D", "1/2T", "1/2",
    "1/2D", "1/1T", "1/1", "1/1D",
];
const DIV: ParamKind = ParamKind::Choice(ECHO_DIVS);
pub const CHANNEL_MODES: &[&str] = &["Stereo", "Ping Pong", "Mid/Side"];
const MODE_PING: usize = 1;
const MODE_MS: usize = 2;
pub const REVERB_LOCATIONS: &[&str] = &["Pre", "Post", "Feedback"];
const REV_PRE: usize = 0;
const REV_POST: usize = 1;
/// Longest delay time in seconds.
pub const MAX_SECONDS: f32 = 6.0;
/// Extra room in the line for modulation.
const MOD_MARGIN_SECONDS: f32 = 0.05;
const CONTROL: usize = 16;

/// Length of an [`ECHO_DIVS`] entry in beats.
pub fn div_beats(index: f32) -> f64 {
    let i = (index.round().max(0.0) as usize).min(ECHO_DIVS.len() - 1);
    let base = 0.125 * (1 << (i / 3)) as f64;
    match i % 3 {
        0 => base * 2.0 / 3.0,
        1 => base,
        _ => base * 1.5,
    }
}

device_params! {
    pub struct EchoParams {
        sync: "Sync", 0.0, 1.0, 1.0, ParamKind::Toggle, Unit::Plain;
        div_l: "Left", 0.0, 17.0, 8.0, DIV, Unit::Plain;
        div_r: "Right", 0.0, 17.0, 8.0, DIV, Unit::Plain;
        time_l: "Left", 1.0, 6000.0, 375.0, LOG, Unit::Ms;
        time_r: "Right", 1.0, 6000.0, 375.0, LOG, Unit::Ms;
        link: "Link", 0.0, 1.0, 1.0, ParamKind::Toggle, Unit::Plain;
        channel_mode: "Mode", 0.0, 2.0, 0.0, ParamKind::Choice(CHANNEL_MODES), Unit::Plain;
        feedback: "Feedback", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        input_gain: "Input", -24.0, 24.0, 0.0, LIN, Unit::Db;
        hp_freq: "HP", 20.0, 2000.0, 80.0, LOG, Unit::Hz;
        hp_res: "HP Res", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        lp_freq: "LP", 300.0, 20000.0, 7000.0, LOG, Unit::Hz;
        lp_res: "LP Res", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        mod_rate: "Mod Rate", 0.01, 40.0, 0.6, LOG, Unit::Hz;
        mod_amount: "Mod Time", 0.0, 1.0, 0.1, LIN, Unit::Percent;
        mod_filter: "Mod Filter", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        reverb: "Reverb", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        reverb_decay: "Decay", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        reverb_loc: "Rev Loc", 0.0, 2.0, 1.0, ParamKind::Choice(REVERB_LOCATIONS), Unit::Plain;
        noise: "Noise", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        wobble: "Wobble", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        duck: "Duck", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        duck_threshold: "Duck Thr", -60.0, 0.0, -24.0, LIN, Unit::Db;
        duck_release: "Duck Rel", 10.0, 3000.0, 250.0, LOG, Unit::Ms;
        gate: "Gate", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        gate_threshold: "Gate Thr", -80.0, 0.0, -40.0, LIN, Unit::Db;
        gate_release: "Gate Rel", 5.0, 2000.0, 80.0, LOG, Unit::Ms;
        width: "Width", 0.0, 2.0, 1.0, LIN, Unit::Percent;
        mix: "Dry/Wet", 0.0, 1.0, 0.4, LIN, Unit::Percent;
    }
}

impl EchoParams {
    /// Delay times in seconds for the left and right channel.
    pub fn times(&self, tempo: f64) -> (f32, f32) {
        let linked = self.link >= 0.5;
        if self.sync >= 0.5 {
            let spb = 60.0 / tempo.max(1.0);
            let l = (div_beats(self.div_l) * spb) as f32;
            let r = if linked {
                l
            } else {
                (div_beats(self.div_r) * spb) as f32
            };
            (l.min(MAX_SECONDS), r.min(MAX_SECONDS))
        } else {
            let l = self.time_l * 0.001;
            let r = if linked { l } else { self.time_r * 0.001 };
            (l.min(MAX_SECONDS), r.min(MAX_SECONDS))
        }
    }

    fn channel_mode_index(&self) -> usize {
        (self.channel_mode.round().max(0.0) as usize).min(MODE_MS)
    }
}

impl super::EffectParams for EchoParams {
    fn is_relevant(&self, index: usize) -> bool {
        let synced = self.sync >= 0.5;
        let linked = self.link >= 0.5;
        match index {
            1 => synced,
            2 => synced && !linked,
            3 => !synced,
            4 => !synced && !linked,
            17 | 18 => self.reverb > 0.0,
            22 | 23 => self.duck >= 0.5,
            25 | 26 => self.gate >= 0.5,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Dub Tape",
                Self {
                    div_l: 8.0,
                    feedback: 0.75,
                    hp_freq: 150.0,
                    lp_freq: 2500.0,
                    lp_res: 0.3,
                    wobble: 0.35,
                    noise: 0.15,
                    input_gain: 4.0,
                    mix: 0.35,
                    ..d
                },
            ),
            (
                "Ping Pong Eighths",
                Self {
                    div_l: 7.0,
                    channel_mode: MODE_PING as f32,
                    feedback: 0.55,
                    mix: 0.3,
                    ..d
                },
            ),
            (
                "Ducked Vocal Echo",
                Self {
                    div_l: 10.0,
                    link: 0.0,
                    div_r: 11.0,
                    feedback: 0.45,
                    duck: 1.0,
                    duck_threshold: -30.0,
                    reverb: 0.3,
                    reverb_loc: REV_POST as f32,
                    mix: 0.35,
                    ..d
                },
            ),
            (
                "Ambient Wash",
                Self {
                    div_l: 11.0,
                    feedback: 0.7,
                    reverb: 0.6,
                    reverb_decay: 0.8,
                    reverb_loc: 2.0,
                    mod_amount: 0.4,
                    mod_rate: 0.2,
                    mod_filter: 0.3,
                    lp_freq: 4500.0,
                    width: 1.4,
                    mix: 0.45,
                    ..d
                },
            ),
        ]
    }
}

/// A Schroeder allpass diffuser.
struct Diffuser {
    buf: Vec<f32>,
    pos: usize,
}

impl Diffuser {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(1)],
            pos: 0,
        }
    }

    #[inline]
    fn tick(&mut self, x: f32, g: f32) -> f32 {
        let d = self.buf[self.pos];
        let w = x + d * g;
        let y = d - w * g;
        self.buf[self.pos] = if w.abs() < 1e-20 { 0.0 } else { w };
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
        y
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// A compact stereo reverb: two diffusers per side into a four-line feedback
/// delay network with a Householder matrix and damping.
struct SmallReverb {
    diff: [[Diffuser; 2]; 2],
    lines: [Vec<f32>; 4],
    pos: [usize; 4],
    damp: [f32; 4],
}

impl SmallReverb {
    fn new(sr: f32) -> Self {
        let ms = |m: f32| ((m * 0.001 * sr) as usize).max(1);
        Self {
            diff: [
                [Diffuser::new(ms(4.7)), Diffuser::new(ms(1.7))],
                [Diffuser::new(ms(5.3)), Diffuser::new(ms(1.3))],
            ],
            lines: [
                vec![0.0; ms(31.3)],
                vec![0.0; ms(37.9)],
                vec![0.0; ms(43.1)],
                vec![0.0; ms(49.7)],
            ],
            pos: [0; 4],
            damp: [0.0; 4],
        }
    }

    fn clear(&mut self) {
        for d in self.diff.iter_mut().flatten() {
            d.clear();
        }
        for l in &mut self.lines {
            l.fill(0.0);
        }
        self.damp = [0.0; 4];
    }

    /// Per-line feedback gains for a decay time.
    fn gains(&self, rt60: f32, sr: f32) -> [f32; 4] {
        let mut g = [0.0; 4];
        for (gi, l) in g.iter_mut().zip(&self.lines) {
            *gi = 10f32.powf(-3.0 * l.len() as f32 / (rt60.max(0.05) * sr));
        }
        g
    }

    #[inline]
    fn tick(&mut self, l: f32, r: f32, gains: &[f32; 4], damp: f32) -> (f32, f32) {
        let a = self.diff[0][0].tick(l, 0.6);
        let a = self.diff[0][1].tick(a, 0.6);
        let b = self.diff[1][0].tick(r, 0.6);
        let b = self.diff[1][1].tick(b, 0.6);
        let mut out = [0.0f32; 4];
        for k in 0..4 {
            out[k] = self.lines[k][self.pos[k]];
        }
        let sum = (out[0] + out[1] + out[2] + out[3]) * 0.5;
        let inject = [a, b, a, b];
        for k in 0..4 {
            let fb = (out[k] - sum) * gains[k];
            self.damp[k] = fb + (self.damp[k] - fb) * damp;
            if self.damp[k].abs() < 1e-20 {
                self.damp[k] = 0.0;
            }
            self.lines[k][self.pos[k]] = self.damp[k] + inject[k] * 0.5;
            self.pos[k] += 1;
            if self.pos[k] == self.lines[k].len() {
                self.pos[k] = 0;
            }
        }
        ((out[0] + out[2]) * 0.6, (out[1] + out[3]) * 0.6)
    }
}

pub struct Echo {
    sr: f32,
    buf: [Vec<f32>; 2],
    pos: usize,
    /// Gliding delay time per channel, in samples.
    cur: [f32; 2],
    hp: [SvfState; 2],
    lp: [SvfState; 2],
    /// Last filtered tape output per channel (the feedback signal).
    tape: [f32; 2],
    mod_phase: f64,
    flutter_phase: f64,
    drift: [LfoVoice; 2],
    drift_phase: f64,
    rng: Rng,
    hiss: [f32; 2],
    reverb: SmallReverb,
    gate_env: EnvFollower,
    gate_gain: f32,
    duck_env: EnvFollower,
    duck_db: f32,
    duck_on: bool,
}

impl Echo {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let n = ((MAX_SECONDS + MOD_MARGIN_SECONDS) * sr) as usize + 16;
        Self {
            sr,
            buf: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            cur: [-1.0; 2],
            hp: [SvfState::default(); 2],
            lp: [SvfState::default(); 2],
            tape: [0.0; 2],
            mod_phase: 0.0,
            flutter_phase: 0.0,
            drift: [LfoVoice::new(0x0dd_ba11), LfoVoice::new(0xfee1_900d)],
            drift_phase: 0.0,
            rng: Rng::new(0x7a9e_5eed),
            hiss: [0.0; 2],
            reverb: SmallReverb::new(sr),
            gate_env: EnvFollower::default(),
            gate_gain: 0.0,
            duck_env: EnvFollower::default(),
            duck_db: 0.0,
            duck_on: false,
        }
    }

    pub fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
        for s in self.hp.iter_mut().chain(self.lp.iter_mut()) {
            s.reset();
        }
        self.tape = [0.0; 2];
        self.reverb.clear();
        self.gate_env = EnvFollower::default();
        self.duck_env = EnvFollower::default();
        self.duck_db = 0.0;
    }

    pub fn process(&mut self, p: &EchoParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let sr = self.sr;
        let frames = left.len().min(right.len());
        let n = self.buf[0].len();
        let (tl, tr) = p.times(ctx.tempo);
        let target = [(tl * sr).max(2.0), (tr * sr).max(2.0)];
        for c in 0..2 {
            if self.cur[c] < 0.0 {
                self.cur[c] = target[c];
            }
        }
        let glide = 1.0 - super::smoothing(0.12, sr);
        let mode = p.channel_mode_index();
        let fb = p.feedback.clamp(0.0, 1.0) * 1.05;
        let in_gain = super::db(p.input_gain.clamp(-48.0, 36.0));
        let mod_inc = p.mod_rate.clamp(0.0, 100.0) as f64 / sr as f64;
        let mod_samples = p.mod_amount.clamp(0.0, 1.0) * 0.005 * sr;
        let mod_filter = p.mod_filter.clamp(0.0, 1.0) * 2.0;
        let wobble = p.wobble.clamp(0.0, 1.0);
        let wobble_samples = wobble * 0.004 * sr;
        let flutter_inc = 6.3 / sr as f64;
        let drift_inc = 0.7 / sr as f64;
        let noise = p.noise.clamp(0.0, 1.0);
        let noise_gain = noise * noise * 0.02;
        let hiss_coef = super::smoothing(1.0 / (std::f32::consts::TAU * 7000.0), sr);
        let rev_amt = p.reverb.clamp(0.0, 1.0);
        let rev_loc = (p.reverb_loc.round().max(0.0) as usize).min(2);
        let rev_gains = self
            .reverb
            .gains(0.3 + p.reverb_decay.clamp(0.0, 1.0).powi(2) * 8.0, sr);
        let rev_damp = super::smoothing(1.0 / (std::f32::consts::TAU * 6000.0), sr);
        let gate_on = p.gate >= 0.5;
        let gate_thr = super::db(p.gate_threshold);
        let gate_att = super::smoothing(0.0005, sr);
        let gate_rel = super::smoothing(p.gate_release.max(1.0) * 0.001, sr);
        let gate_open = super::smoothing(0.001, sr);
        self.duck_on = p.duck >= 0.5;
        let duck_att = super::smoothing(0.002, sr);
        let duck_rel = super::smoothing(p.duck_release.max(1.0) * 0.001, sr);
        let width = p.width.clamp(0.0, 2.0);
        let mix = p.mix.clamp(0.0, 1.0);
        let hp_q = 0.6 + p.hp_res.clamp(0.0, 1.0).powi(2) * 8.0;
        let lp_q = 0.6 + p.lp_res.clamp(0.0, 1.0).powi(2) * 8.0;
        let max_d = (n - 8) as f32;

        let mut i = 0;
        while i < frames {
            let len = CONTROL.min(frames - i);
            // Filter cutoffs follow the modulation LFO at control rate.
            let lfo = (self.mod_phase * std::f64::consts::TAU).sin() as f32;
            let fmod = (lfo * mod_filter).exp2();
            let hpc = SvfCoefs::new(p.hp_freq * fmod, hp_q, sr);
            let lpc = SvfCoefs::new(p.lp_freq * fmod, lp_q, sr);
            for k in i..i + len {
                let dry = [left[k], right[k]];
                let mut send = [dry[0] * in_gain, dry[1] * in_gain];

                // Gate on the delay input.
                if gate_on {
                    let e = self
                        .gate_env
                        .tick((send[0].abs()).max(send[1].abs()), gate_att, gate_rel);
                    let target = if e > gate_thr { 1.0 } else { 0.0 };
                    let c = if target > self.gate_gain { gate_open } else { gate_rel };
                    self.gate_gain = target + (self.gate_gain - target) * c;
                    send[0] *= self.gate_gain;
                    send[1] *= self.gate_gain;
                }
                if rev_amt > 0.0 && rev_loc == REV_PRE {
                    let (a, b) = self.reverb.tick(send[0], send[1], &rev_gains, rev_damp);
                    send = [send[0] + rev_amt * (a - send[0]), send[1] + rev_amt * (b - send[1])];
                }
                if mode == MODE_MS {
                    send = [(send[0] + send[1]) * 0.5, (send[0] - send[1]) * 0.5];
                }

                // Time modulation.
                let m = (self.mod_phase * std::f64::consts::TAU).sin() as f32;
                let flutter = (self.flutter_phase * std::f64::consts::TAU).sin() as f32;
                let mut delay = [0.0f32; 2];
                for c in 0..2 {
                    self.cur[c] += (target[c] - self.cur[c]) * glide;
                    let drift = if wobble > 0.0 {
                        let ph = (self.drift_phase + c as f64 * 0.37).rem_euclid(1.0) as f32;
                        self.drift[c].value(SHAPE_RANDOM, ph)
                    } else {
                        0.0
                    };
                    let wob = wobble_samples * (0.8 * drift + 0.2 * flutter);
                    let modulation = mod_samples * if c == 1 { -m } else { m };
                    delay[c] = (self.cur[c] + modulation + wob).clamp(2.0, max_d);
                }

                // Tape read, filtering.
                let mut out = [0.0f32; 2];
                for c in 0..2 {
                    let raw = read_cubic(&self.buf[c], self.pos, delay[c]);
                    let h = self.hp[c].tick(&hpc, raw).2;
                    out[c] = self.lp[c].tick(&lpc, h).0;
                }
                let mut loop_sig = out;
                if rev_amt > 0.0 && rev_loc == 2 {
                    let (a, b) = self.reverb.tick(out[0], out[1], &rev_gains, rev_damp);
                    loop_sig = [out[0] + rev_amt * (a - out[0]), out[1] + rev_amt * (b - out[1])];
                }

                // Hiss.
                let mut hiss = [0.0f32; 2];
                if noise_gain > 0.0 {
                    for c in 0..2 {
                        let w = self.rng.next_bipolar();
                        self.hiss[c] = w + (self.hiss[c] - w) * hiss_coef;
                        hiss[c] = self.hiss[c] * noise_gain;
                    }
                }

                // Write with feedback and tape saturation.
                let fb_sig = [loop_sig[0] * fb, loop_sig[1] * fb];
                let write = match mode {
                    MODE_PING => [(send[0] + send[1]) * 0.5 + fb_sig[1], fb_sig[0]],
                    _ => [send[0] + fb_sig[0], send[1] + fb_sig[1]],
                };
                for c in 0..2 {
                    let w = soft_clip((write[c] + hiss[c]) * 0.8) * 1.25;
                    self.buf[c][self.pos] = if w.abs() < 1e-20 { 0.0 } else { w };
                }
                self.pos += 1;
                if self.pos == n {
                    self.pos = 0;
                }
                self.mod_phase = (self.mod_phase + mod_inc).rem_euclid(1.0);
                self.flutter_phase = (self.flutter_phase + flutter_inc).rem_euclid(1.0);
                self.drift_phase = (self.drift_phase + drift_inc).rem_euclid(1.0);

                // Wet output.
                let mut wet = out;
                if mode == MODE_MS {
                    wet = [wet[0] + wet[1], wet[0] - wet[1]];
                }
                if rev_amt > 0.0 && rev_loc == REV_POST {
                    let (a, b) = self.reverb.tick(wet[0], wet[1], &rev_gains, rev_damp);
                    wet = [wet[0] + rev_amt * (a - wet[0]), wet[1] + rev_amt * (b - wet[1])];
                }
                if self.duck_on {
                    let e = self.duck_env.tick(dry[0].abs().max(dry[1].abs()), duck_att, duck_rel);
                    let over = if e > 1e-6 {
                        20.0 * e.log10() - p.duck_threshold
                    } else {
                        -100.0
                    };
                    self.duck_db = -(over.max(0.0) * 1.5).min(48.0);
                    let g = super::db(self.duck_db);
                    wet = [wet[0] * g, wet[1] * g];
                }
                let mid = (wet[0] + wet[1]) * 0.5;
                let side = (wet[0] - wet[1]) * 0.5 * width;
                left[k] = dry[0] * (1.0 - mix) + (mid + side) * mix;
                right[k] = dry[1] * (1.0 - mix) + (mid - side) * mix;
            }
            i += len;
        }
    }
}

impl super::EffectRuntime for Echo {
    fn meter(&self) -> Option<f32> {
        self.duck_on.then_some(self.duck_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> EchoParams {
        EchoParams {
            mod_amount: 0.0,
            hp_freq: 20.0,
            lp_freq: 20000.0,
            mix: 1.0,
            ..Default::default()
        }
    }

    fn impulse(p: &EchoParams, ctx: &FxContext, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut e = Echo::new(ctx.sample_rate);
        let mut l = vec![0.0f32; n];
        let mut r = vec![0.0f32; n];
        l[0] = 0.8;
        r[0] = 0.8;
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            e.process(p, a, b, ctx);
        }
        (l, r)
    }

    fn peak(x: &[f32], at: usize, w: usize) -> f32 {
        x[at.saturating_sub(w)..(at + w).min(x.len())]
            .iter()
            .fold(0.0f32, |a, b| a.max(b.abs()))
    }

    #[test]
    fn divisions() {
        assert_eq!(div_beats(10.0), 1.0);
        assert!((div_beats(9.0) - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(div_beats(11.0), 1.5);
        assert_eq!(div_beats(1.0), 0.125);
        assert_eq!(div_beats(17.0), 6.0);
    }

    #[test]
    fn synced_echo_arrives_on_time() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = EchoParams {
            div_l: 10.0, // 1/4 = 0.5 s
            feedback: 0.5,
            ..clean()
        };
        let (l, r) = impulse(&p, &ctx, 60000);
        let first = l.iter().position(|x| x.abs() > 0.1).unwrap();
        assert!((first as i64 - 24000).abs() <= 3, "first echo at {first}");
        let e1 = peak(&l, 24000, 20);
        let e2 = peak(&l, 48000, 20);
        assert!(e2 < e1 * 0.75 && e2 > e1 * 0.3, "{e1} {e2}");
        // Linked: right is the same.
        assert!((peak(&r, 24000, 20) - e1).abs() < 1e-4);
    }

    #[test]
    fn unlinked_times_and_ms_mode() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = EchoParams {
            sync: 0.0,
            link: 0.0,
            time_l: 100.0,
            time_r: 150.0,
            feedback: 0.0,
            ..clean()
        };
        let (l, r) = impulse(&p, &ctx, 12000);
        assert!(peak(&l, 4800, 5) > 0.2 && peak(&r, 4800, 5) < 0.01);
        assert!(peak(&r, 7200, 5) > 0.2 && peak(&l, 7200, 5) < 0.01);
        let ms = EchoParams {
            channel_mode: MODE_MS as f32,
            link: 1.0,
            ..p
        };
        let (l, r) = impulse(&ms, &ctx, 12000);
        assert!((peak(&l, 4800, 5) - peak(&r, 4800, 5)).abs() < 0.02);
    }

    #[test]
    fn ping_pong_alternates() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = EchoParams {
            sync: 0.0,
            time_l: 100.0,
            channel_mode: MODE_PING as f32,
            feedback: 0.8,
            ..clean()
        };
        let (l, r) = impulse(&p, &ctx, 20000);
        assert!(peak(&l, 4800, 10) > 0.3 && peak(&r, 4800, 10) < 0.01);
        assert!(peak(&r, 9600, 10) > 0.2 && peak(&l, 9600, 10) < 0.01);
    }

    #[test]
    fn feedback_filters_darken_repeats() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = EchoParams {
            sync: 0.0,
            time_l: 50.0,
            feedback: 0.9,
            lp_freq: 1000.0,
            ..clean()
        };
        // A burst of 8 kHz: repeats die quickly through the 1 kHz low-pass.
        let mut e = Echo::new(sr);
        let mut l: Vec<f32> = (0..48000)
            .map(|i| {
                if i < 480 {
                    (i as f32 * 8000.0 * std::f32::consts::TAU / sr).sin() * 0.5
                } else {
                    0.0
                }
            })
            .collect();
        let mut r = l.clone();
        e.process(&p, &mut l, &mut r, &ctx);
        assert!(peak(&l, 2400 + 240, 240) < 0.05);
    }

    #[test]
    fn time_changes_glide_without_clicks() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let mut p = EchoParams {
            sync: 0.0,
            time_l: 200.0,
            feedback: 0.3,
            ..clean()
        };
        let mut e = Echo::new(sr);
        let freq = 220.0;
        let mut l: Vec<f32> = (0..96000)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / sr).sin() * 0.4)
            .collect();
        let mut r = l.clone();
        for (k, (a, b)) in l.chunks_mut(256).zip(r.chunks_mut(256)).enumerate() {
            if k == 150 {
                p.time_l = 900.0;
            }
            e.process(&p, a, b, &ctx);
        }
        // Max slope stays near that of a sine at a moderate pitch.
        let max_step = l
            .windows(2)
            .skip(20000)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(max_step < 0.15, "{max_step}");
    }

    #[test]
    fn ducking_lowers_the_wet_while_input_is_loud() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = EchoParams {
            sync: 0.0,
            time_l: 100.0,
            feedback: 0.9,
            duck: 1.0,
            duck_threshold: -30.0,
            duck_release: 50.0,
            mix: 1.0,
            ..clean()
        };
        let mut e = Echo::new(sr);
        // Loud tone for 0.5 s, then silence.
        let mut l: Vec<f32> = (0..48000)
            .map(|i| {
                if i < 24000 {
                    (i as f32 * 300.0 * std::f32::consts::TAU / sr).sin() * 0.5
                } else {
                    0.0
                }
            })
            .collect();
        let mut r = l.clone();
        let mut gr = 0.0;
        for (k, (a, b)) in l.chunks_mut(256).zip(r.chunks_mut(256)).enumerate() {
            e.process(&p, a, b, &ctx);
            if k == 60 {
                gr = super::super::EffectRuntime::meter(&e).unwrap();
            }
        }
        assert!(gr < -20.0, "{gr}");
        let during = peak(&l, 20000, 2000);
        let after = peak(&l, 34000, 2000);
        assert!(after > during * 5.0, "{during} {after}");
    }

    #[test]
    fn gate_blocks_quiet_input() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = EchoParams {
            sync: 0.0,
            time_l: 100.0,
            feedback: 0.0,
            gate: 1.0,
            gate_threshold: -30.0,
            ..clean()
        };
        let mut e = Echo::new(sr);
        let mut l: Vec<f32> = (0..12000)
            .map(|i| (i as f32 * 300.0 * std::f32::consts::TAU / sr).sin() * 0.01)
            .collect();
        let mut r = l.clone();
        e.process(&p, &mut l, &mut r, &ctx);
        assert!(peak(&l, 9000, 2000) < 0.001);
    }

    #[test]
    fn reverb_adds_a_tail() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = EchoParams {
            sync: 0.0,
            time_l: 100.0,
            feedback: 0.0,
            reverb: 0.8,
            reverb_decay: 0.7,
            ..clean()
        };
        let (l, _) = impulse(&p, &ctx, 48000);
        let tail: f32 = l[12000..24000].iter().map(|x| x * x).sum();
        let (dry_l, _) = impulse(&EchoParams { reverb: 0.0, ..p }, &ctx, 48000);
        let dry_tail: f32 = dry_l[12000..24000].iter().map(|x| x * x).sum();
        assert!(tail > 1e-4 && dry_tail < 1e-9, "{tail} {dry_tail}");
    }

    #[test]
    fn extremes_stay_bounded() {
        let sr = 96000.0;
        let ctx = FxContext::new(sr, 30.0);
        for loc in 0..3 {
            for mode in 0..3 {
                let p = EchoParams {
                    div_l: 17.0,
                    link: 0.0,
                    div_r: 0.0,
                    channel_mode: mode as f32,
                    feedback: 1.0,
                    input_gain: 24.0,
                    hp_res: 1.0,
                    lp_res: 1.0,
                    mod_rate: 40.0,
                    mod_amount: 1.0,
                    mod_filter: 1.0,
                    reverb: 1.0,
                    reverb_decay: 1.0,
                    reverb_loc: loc as f32,
                    noise: 1.0,
                    wobble: 1.0,
                    width: 2.0,
                    mix: 1.0,
                    ..Default::default()
                };
                let mut e = Echo::new(sr);
                let mut l: Vec<f32> = (0..40000).map(|i| if i % 100 < 50 { 1.0 } else { -1.0 }).collect();
                let mut r = l.clone();
                e.process(&p, &mut l[..1], &mut r[..1], &ctx);
                e.process(&p, &mut l[1..], &mut r[1..], &ctx);
                assert!(
                    l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 40.0),
                    "{loc} {mode}"
                );
            }
        }
    }
}
