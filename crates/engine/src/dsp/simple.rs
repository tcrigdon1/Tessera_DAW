//! Simple Synth: a compact, CPU-light "one panel" synthesizer with character.
//!
//! Oscillator 1 morphs continuously sine -> triangle -> saw -> pulse;
//! oscillator 2 is a detunable classic waveform; plus noise. One low-pass
//! filter (type I: 12 dB clean SVF, type II: 24 dB driven cascade), an amp
//! envelope, a second envelope that can cycle (and modulates filter, pitch
//! and shape), one LFO, analog-style drift, and poly / mono / stereo /
//! unison voice modes with glide.
//!
//! To stay light, all modulation and filter coefficients are computed once
//! per [`CONTROL`] samples and only two waveforms are evaluated for the
//! morphing oscillator.

use super::analog::{blamp, blep, choice, sat, wrap01};
use super::{gain_smooth_coef, lfo_hz, midi_to_hz, ExprGain, Performance, Rng, SYNC_RATES, SYNC_RATE_DEFAULT};
use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};
use crate::model::db_to_gain;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);
const TOGGLE: ParamKind = ParamKind::Toggle;
const OSC2_WAVES: ParamKind = ParamKind::Choice(&["Sine", "Triangle", "Saw", "Square"]);
const FILTER_TYPES: ParamKind = ParamKind::Choice(&["LP I", "LP II"]);
const ENV2_MODES: ParamKind = ParamKind::Choice(&["ADSR", "Cycle"]);
const LFO_SHAPES: ParamKind = ParamKind::Choice(&["Sine", "Triangle", "Saw", "Square", "S&H", "Wander"]);
const VOICE_MODES: ParamKind = ParamKind::Choice(&["Poly", "Mono", "Stereo", "Unison"]);

pub const VOICES: usize = 12;
const MAX_SUBS: usize = 4;
pub const CONTROL: u32 = 32;

device_params! {
    pub struct SimpleParams {
        o1_shape: "Shape", 0.0, 1.0, 0.66, LIN, Unit::Percent;
        o1_oct: "Osc1 Oct", -2.0, 2.0, 0.0, LIN, Unit::Integer;
        o2_wave: "Osc2 Wave", 0.0, 3.0, 2.0, OSC2_WAVES, Unit::Plain;
        o2_oct: "Osc2 Oct", -2.0, 2.0, 0.0, LIN, Unit::Integer;
        o2_detune: "Osc2 Detune", -100.0, 100.0, 8.0, LIN, Unit::Integer;
        o2_level: "Osc2 Level", 0.0, 1.0, 0.4, LIN, Unit::Percent;
        noise: "Noise", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        f_type: "Filter", 0.0, 1.0, 0.0, FILTER_TYPES, Unit::Plain;
        cutoff: "Cutoff", 20.0, 20000.0, 2400.0, LOG, Unit::Hz;
        res: "Reso", 0.0, 1.0, 0.25, LIN, Unit::Percent;
        key: "Key Track", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        f_env: "Env2>Filter", -5.0, 5.0, 2.0, LIN, Unit::Plain;
        f_lfo: "LFO>Filter", 0.0, 4.0, 0.0, LIN, Unit::Plain;
        a1: "Attack", 0.001, 20.0, 0.002, LOG, Unit::Seconds;
        d1: "Decay", 0.001, 20.0, 0.6, LOG, Unit::Seconds;
        s1: "Sustain", 0.0, 1.0, 0.7, LIN, Unit::Percent;
        r1: "Release", 0.001, 20.0, 0.25, LOG, Unit::Seconds;
        e2_mode: "Env2 Mode", 0.0, 1.0, 0.0, ENV2_MODES, Unit::Plain;
        a2: "Env2 Attack", 0.001, 20.0, 0.001, LOG, Unit::Seconds;
        d2: "Env2 Decay", 0.001, 20.0, 0.4, LOG, Unit::Seconds;
        s2: "Env2 Sustain", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        r2: "Env2 Release", 0.001, 20.0, 0.3, LOG, Unit::Seconds;
        e2_pitch: "Env2>Pitch", -24.0, 24.0, 0.0, LIN, Unit::Plain;
        e2_shape: "Env2>Shape", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        lfo_shape: "LFO Shape", 0.0, 5.0, 0.0, LFO_SHAPES, Unit::Plain;
        lfo_rate: "LFO Rate", 0.05, 40.0, 3.0, LOG, Unit::Hz;
        lfo_retrig: "LFO Retrig", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        lfo_pitch: "LFO>Pitch", 0.0, 12.0, 0.0, LIN, Unit::Plain;
        lfo_shape_amt: "LFO>Shape", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        lfo_amp: "LFO>Amp", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        drift: "Drift", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        voice_mode: "Voices", 0.0, 3.0, 0.0, VOICE_MODES, Unit::Plain;
        spread: "Spread", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        glide: "Glide", 0.0, 2.0, 0.0, LIN, Unit::Seconds;
        vel_amp: "Vel>Amp", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        vel_filter: "Vel>Filter", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        volume_db: "Volume", -40.0, 6.0, -8.0, LIN, Unit::Db;
        lfo_sync: "LFO Sync", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        lfo_sync_rate: "LFO Sync Rate", 0.0, 10.0, SYNC_RATE_DEFAULT, SYNC_RATES, Unit::Plain;
    }
}

/// Parameter indices into [`SimpleParams::INFO`] (for panels).
pub mod idx {
    pub const SHAPE: usize = 0;
    pub const O2_WAVE: usize = 2;
    pub const NOISE: usize = 6;
    pub const F_TYPE: usize = 7;
    pub const F_LFO: usize = 12;
    pub const A1: usize = 13;
    pub const E2_MODE: usize = 17;
    pub const S2: usize = 20;
    pub const E2_SHAPE: usize = 23;
    pub const LFO_SHAPE: usize = 24;
    pub const LFO_AMP: usize = 29;
    pub const DRIFT: usize = 30;
    pub const VOICE_MODE: usize = 31;
    pub const SPREAD: usize = 32;
    pub const VOLUME: usize = 36;
    /// The LFO sync toggle; its synced rate follows it.
    pub const LFO_SYNC: usize = 37;
}

/// Automatable parameters, in [`SimpleParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; SimpleParams::COUNT] = specs_from_info(SimpleParams::INFO);

/// Factory presets: (browser category, name, settings).
pub fn presets() -> Vec<(&'static str, &'static str, SimpleParams)> {
    let d = SimpleParams::default();
    vec![
        (
            "Bass",
            "Drift Bass",
            SimpleParams {
                o1_shape: 0.85,
                o2_wave: 3.0,
                o2_oct: -1.0,
                o2_detune: 0.0,
                o2_level: 0.5,
                f_type: 1.0,
                cutoff: 260.0,
                res: 0.35,
                key: 0.3,
                f_env: 3.0,
                d2: 0.25,
                s2: 0.0,
                a1: 0.001,
                d1: 0.5,
                s1: 0.8,
                r1: 0.08,
                voice_mode: 1.0,
                glide: 0.04,
                drift: 0.25,
                volume_db: -6.0,
                ..d
            },
        ),
        (
            "Lead",
            "Soft Lead",
            SimpleParams {
                o1_shape: 0.5,
                o2_wave: 2.0,
                o2_detune: 12.0,
                o2_level: 0.5,
                cutoff: 1800.0,
                res: 0.3,
                f_env: 1.5,
                lfo_rate: 5.0,
                lfo_pitch: 0.15,
                voice_mode: 2.0,
                spread: 0.4,
                glide: 0.06,
                r1: 0.3,
                volume_db: -7.0,
                ..d
            },
        ),
        (
            "Pad",
            "Wide Pad",
            SimpleParams {
                o1_shape: 0.7,
                o2_wave: 2.0,
                o2_detune: 7.0,
                o2_level: 0.6,
                cutoff: 900.0,
                res: 0.2,
                f_env: 1.2,
                a2: 1.5,
                d2: 3.0,
                s2: 0.5,
                r2: 2.0,
                a1: 0.9,
                d1: 2.0,
                s1: 0.85,
                r1: 2.2,
                lfo_rate: 0.25,
                lfo_shape: 5.0,
                f_lfo: 0.6,
                lfo_shape_amt: 0.2,
                voice_mode: 3.0,
                spread: 0.6,
                drift: 0.6,
                volume_db: -8.0,
                ..d
            },
        ),
        (
            "Pluck",
            "Glass Pluck",
            SimpleParams {
                o1_shape: 0.3,
                o2_wave: 0.0,
                o2_oct: 1.0,
                o2_detune: 3.0,
                o2_level: 0.4,
                cutoff: 600.0,
                res: 0.4,
                key: 0.8,
                f_env: 3.5,
                d2: 0.18,
                s2: 0.0,
                e2_shape: 0.4,
                a1: 0.001,
                d1: 0.7,
                s1: 0.0,
                r1: 0.5,
                vel_filter: 0.7,
                volume_db: -8.0,
                ..d
            },
        ),
        (
            "Textures",
            "Cycling Wobble",
            SimpleParams {
                o1_shape: 0.95,
                o2_wave: 3.0,
                o2_detune: -10.0,
                o2_level: 0.5,
                noise: 0.1,
                f_type: 1.0,
                cutoff: 400.0,
                res: 0.55,
                f_env: 3.0,
                e2_mode: 1.0,
                a2: 0.12,
                d2: 0.25,
                lfo_shape: 4.0,
                lfo_rate: 8.0,
                lfo_shape_amt: 0.3,
                drift: 0.5,
                volume_db: -8.0,
                ..d
            },
        ),
    ]
}

// ----- envelope with an optional cycling mode ------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Debug, Clone, Copy)]
struct Env {
    stage: Stage,
    value: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct EnvCoef {
    attack_step: f32,
    decay: f32,
    sustain: f32,
    release: f32,
    cycle: bool,
}

impl EnvCoef {
    /// Coefficients for advancing `step` samples at a time.
    fn new(a: f32, d: f32, s: f32, r: f32, cycle: bool, sr: f32, step: f32) -> Self {
        let pole = |t: f32| (-6.9 * step / (t.max(0.001) * sr)).exp();
        Self {
            attack_step: step / (a.max(0.0005) * sr),
            decay: pole(d),
            sustain: if cycle { 0.0 } else { s.clamp(0.0, 1.0) },
            release: pole(r),
            cycle,
        }
    }
}

impl Env {
    const fn new() -> Self {
        Self {
            stage: Stage::Idle,
            value: 0.0,
        }
    }

    fn trigger(&mut self) {
        self.stage = Stage::Attack;
    }

    fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    fn active(&self) -> bool {
        self.stage != Stage::Idle
    }

    #[inline]
    fn next(&mut self, c: &EnvCoef) -> f32 {
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                self.value += c.attack_step;
                if self.value >= 1.0 {
                    self.value = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                self.value = c.sustain + (self.value - c.sustain) * c.decay;
                if c.cycle {
                    if self.value < 0.01 {
                        self.stage = Stage::Attack;
                    }
                } else if (self.value - c.sustain).abs() < 1e-4 {
                    self.value = c.sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => self.value = c.sustain,
            Stage::Release => {
                self.value *= c.release;
                if self.value < 1e-4 {
                    self.value = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.value
    }
}

// ----- oscillators ---------------------------------------------------------------

/// Cheap sine from a phase in 0..1 (parabolic approximation, < 0.1 % error).
#[inline]
fn fast_sine(phase: f32) -> f32 {
    let x = phase * 2.0 - 1.0; // -1..1 maps to -pi..pi
    let y = 4.0 * x * (1.0 - x.abs());
    -(0.225 * (y * y.abs() - y) + y)
}

#[inline]
fn basic(wave: usize, phase: f32, dt: f32) -> f32 {
    match wave {
        0 => fast_sine(phase),
        1 => {
            // Triangle aligned with the sine (zero at phase 0, rising).
            let ph = wrap01(phase + 0.75);
            let mut v = 2.0 * (2.0 * ph - 1.0).abs() - 1.0;
            v -= 4.0 * dt * blamp(ph, dt);
            v += 4.0 * dt * blamp(wrap01(ph + 0.5), dt);
            v
        }
        2 => 2.0 * phase - 1.0 - blep(phase, dt),
        _ => {
            let mut v = if phase < 0.5 { 1.0 } else { -1.0 };
            v += blep(phase, dt);
            v -= blep(wrap01(phase + 0.5), dt);
            v
        }
    }
}

/// Morphing oscillator: shape 0 = sine, 1/3 = triangle, 2/3 = saw, 1 = square.
#[inline]
fn morph(shape: f32, phase: f32, dt: f32) -> f32 {
    let s = shape.clamp(0.0, 1.0) * 3.0;
    let i = (s as usize).min(2);
    let f = s - i as f32;
    if f < 0.002 {
        basic(i, phase, dt)
    } else if f > 0.998 {
        basic(i + 1, phase, dt)
    } else {
        let a = basic(i, phase, dt);
        let b = basic(i + 1, phase, dt);
        a + (b - a) * f
    }
}

// ----- filter ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct Svf {
    ic1: f32,
    ic2: f32,
}

#[derive(Debug, Clone, Copy, Default)]
struct SvfCoef {
    a1: f32,
    a2: f32,
    a3: f32,
}

impl SvfCoef {
    fn new(cutoff: f32, k: f32, sr: f32) -> Self {
        let g = (std::f32::consts::PI * cutoff.clamp(16.0, sr * 0.45) / sr).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        Self {
            a1,
            a2: g * a1,
            a3: g * g * a1,
        }
    }
}

impl Svf {
    #[inline]
    fn low(&mut self, x: f32, c: &SvfCoef) -> f32 {
        let v3 = x - self.ic2;
        let v1 = c.a1 * self.ic1 + c.a2 * v3;
        let v2 = self.ic2 + c.a2 * self.ic1 + c.a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        v2
    }
}

/// One channel of the filter (two SVF stages; type I uses only the first).
#[derive(Debug, Clone, Copy, Default)]
struct Channel {
    a: Svf,
    b: Svf,
}

impl Channel {
    #[inline]
    fn process(&mut self, x: f32, two: bool, ca: &SvfCoef, cb: &SvfCoef) -> f32 {
        let x = x + 1e-20;
        if two {
            let y = self.a.low(sat(x * 1.6) * 0.75, ca);
            self.b.low(y, cb)
        } else {
            self.a.low(x, ca)
        }
    }
}

// ----- voices ------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    age: u64,
    pitch: f32,
    target: f32,
    ph1: [f32; MAX_SUBS],
    ph2: [f32; MAX_SUBS],
    dt1: [f32; MAX_SUBS],
    dt2: [f32; MAX_SUBS],
    env1: Env,
    env2: Env,
    env1_value: f32,
    lfo_phase: f32,
    lfo_held: f32,
    lfo_wander: f32,
    rng: Rng,
    /// Fixed per-note drift offsets (cents, octaves) and slow wander.
    drift_pitch: f32,
    drift_cutoff: f32,
    wander: f32,
    shape: f32,
    amp_mod: f32,
    ca: SvfCoef,
    cb: SvfCoef,
    filt: [Channel; 2],
    ctl: u32,
    /// Track plus note-expression bend in semitones, refreshed every render.
    bend: f32,
    expr_gain: ExprGain,
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            note: 0,
            velocity: 0.0,
            held: false,
            age: 0,
            pitch: 60.0,
            target: 60.0,
            ph1: [0.0; MAX_SUBS],
            ph2: [0.0; MAX_SUBS],
            dt1: [0.0; MAX_SUBS],
            dt2: [0.0; MAX_SUBS],
            env1: Env::new(),
            env2: Env::new(),
            env1_value: 0.0,
            lfo_phase: 0.0,
            lfo_held: 0.0,
            lfo_wander: 0.0,
            rng: Rng::new(seed),
            drift_pitch: 0.0,
            drift_cutoff: 0.0,
            wander: 0.0,
            shape: 0.0,
            amp_mod: 1.0,
            ca: SvfCoef::default(),
            cb: SvfCoef::default(),
            filt: [Channel::default(); 2],
            ctl: 0,
            bend: 0.0,
            expr_gain: ExprGain::default(),
        }
    }

    fn active(&self) -> bool {
        self.env1.active()
    }
}

/// Values derived once per render.
struct Shared {
    sr: f32,
    shape: f32,
    o1_off: f32,
    o2_wave: usize,
    o2_off: f32,
    o2_level: f32,
    noise: f32,
    two_pole_pair: bool,
    cutoff: f32,
    k_a: f32,
    k_b: f32,
    key: f32,
    f_env: f32,
    f_lfo: f32,
    e1: EnvCoef,
    e2: EnvCoef,
    e2_pitch: f32,
    e2_shape: f32,
    lfo_shape: u8,
    lfo_rate: f32,
    lfo_pitch: f32,
    lfo_shape_amt: f32,
    lfo_amp: f32,
    drift: f32,
    subs: usize,
    sub_cents: [f32; MAX_SUBS],
    sub_l: [f32; MAX_SUBS],
    sub_r: [f32; MAX_SUBS],
    glide_coef: f32,
    vel_filter: f32,
}

impl Shared {
    fn new(p: &SimpleParams, sr: f32, mode: u8, perf: Performance) -> Self {
        let step = CONTROL as f32;
        let spread = p.spread.clamp(0.0, 1.0);
        let (subs, cents, pans): (usize, [f32; MAX_SUBS], [f32; MAX_SUBS]) = match mode {
            2 => (2, [-1.0, 1.0, 0.0, 0.0], [-1.0, 1.0, 0.0, 0.0]),
            3 => (4, [-1.0, -0.33, 0.33, 1.0], [-1.0, -0.33, 0.33, 1.0]),
            _ => (1, [0.0; MAX_SUBS], [0.0; MAX_SUBS]),
        };
        let norm = 1.0 / (subs as f32).sqrt();
        let mut sub_l = [0.0; MAX_SUBS];
        let mut sub_r = [0.0; MAX_SUBS];
        let mut sub_cents = [0.0; MAX_SUBS];
        let max_cents = if mode == 2 {
            4.0 + 20.0 * spread
        } else {
            2.0 + 28.0 * spread
        };
        for s in 0..subs {
            sub_cents[s] = cents[s] * max_cents;
            let pan = if subs == 1 { 0.0 } else { pans[s] * (0.3 + 0.7 * spread) };
            let ang = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            sub_l[s] = ang.cos() * std::f32::consts::SQRT_2 * norm;
            sub_r[s] = ang.sin() * std::f32::consts::SQRT_2 * norm;
        }
        let res = p.res.clamp(0.0, 1.0);
        let two = p.f_type >= 0.5;
        Self {
            sr,
            shape: p.o1_shape.clamp(0.0, 1.0),
            o1_off: p.o1_oct.round() * 12.0,
            o2_wave: choice(p.o2_wave, 3) as usize,
            o2_off: p.o2_oct.round() * 12.0 + p.o2_detune.clamp(-100.0, 100.0) / 100.0,
            o2_level: p.o2_level.clamp(0.0, 1.0),
            noise: p.noise.clamp(0.0, 1.0),
            two_pole_pair: two,
            cutoff: p.cutoff.clamp(20.0, 20000.0),
            k_a: if two { 1.6 } else { 2.0 - 1.93 * res },
            k_b: if two { 2.0 - 1.95 * res } else { 2.0 },
            key: p.key.clamp(0.0, 1.0),
            f_env: p.f_env.clamp(-5.0, 5.0),
            f_lfo: p.f_lfo.clamp(0.0, 4.0),
            e1: EnvCoef::new(p.a1, p.d1, p.s1, p.r1, false, sr, 1.0),
            e2: EnvCoef::new(p.a2, p.d2, p.s2, p.r2, p.e2_mode >= 0.5, sr, step),
            e2_pitch: p.e2_pitch.clamp(-24.0, 24.0),
            e2_shape: p.e2_shape.clamp(-1.0, 1.0),
            lfo_shape: choice(p.lfo_shape, 5),
            lfo_rate: lfo_hz(p.lfo_rate, p.lfo_sync >= 0.5, p.lfo_sync_rate, perf.tempo).clamp(0.01, 100.0),
            lfo_pitch: p.lfo_pitch.clamp(0.0, 12.0),
            lfo_shape_amt: p.lfo_shape_amt.clamp(0.0, 1.0),
            lfo_amp: p.lfo_amp.clamp(0.0, 1.0),
            drift: p.drift.clamp(0.0, 1.0),
            subs,
            sub_cents,
            sub_l,
            sub_r,
            glide_coef: if p.glide <= 0.0005 {
                1.0
            } else {
                1.0 - (-step * 4.0 / (p.glide * sr)).exp()
            },
            vel_filter: p.vel_filter.clamp(0.0, 1.0),
        }
    }
}

impl Voice {
    fn control(&mut self, s: &Shared) {
        let step = CONTROL as f32 / s.sr;
        self.pitch += (self.target - self.pitch) * s.glide_coef;
        let e2 = self.env2.next(&s.e2);
        // LFO.
        let ph = self.lfo_phase + s.lfo_rate * step;
        if ph >= 1.0 {
            self.lfo_held = self.rng.next_bipolar();
        }
        self.lfo_phase = wrap01(ph);
        let lfo = match s.lfo_shape {
            0 => (self.lfo_phase * std::f32::consts::TAU).sin(),
            1 => 1.0 - 4.0 * (self.lfo_phase - 0.5).abs(),
            2 => 1.0 - 2.0 * self.lfo_phase,
            3 => {
                if self.lfo_phase < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            4 => self.lfo_held,
            _ => {
                let c = (s.lfo_rate * step * 4.0).min(1.0);
                self.lfo_wander += (self.lfo_held - self.lfo_wander) * c;
                self.lfo_wander
            }
        };
        // Drift: slow random walk on top of the per-note offset.
        let w = self.rng.next_bipolar();
        self.wander += (w - self.wander) * (step * 1.5).min(1.0);
        let drift_cents = s.drift * (self.drift_pitch + self.wander * 4.0);
        let pitch = self.pitch + e2 * s.e2_pitch + lfo * s.lfo_pitch + drift_cents / 100.0 + self.bend;
        let f1 = midi_to_hz(pitch + s.o1_off);
        let f2 = midi_to_hz(pitch + s.o2_off);
        for i in 0..s.subs {
            let r = (s.sub_cents[i] / 1200.0).exp2();
            self.dt1[i] = (f1 * r / s.sr).clamp(1e-6, 0.48);
            self.dt2[i] = (f2 * r / s.sr).clamp(1e-6, 0.48);
        }
        self.shape = (s.shape + e2 * s.e2_shape + lfo * s.lfo_shape_amt * 0.5).clamp(0.0, 1.0);
        self.amp_mod = 1.0 - s.lfo_amp * (0.5 - 0.5 * lfo);
        let env_depth = s.f_env * (1.0 - s.vel_filter * (1.0 - self.velocity));
        let oct = env_depth * e2
            + lfo * s.f_lfo
            + s.key * (self.pitch - 60.0) / 12.0
            + s.drift * (self.drift_cutoff + self.wander * 0.08);
        let fc = s.cutoff * oct.clamp(-12.0, 12.0).exp2();
        self.ca = SvfCoef::new(fc, s.k_a, s.sr);
        if s.two_pole_pair {
            self.cb = SvfCoef::new(fc, s.k_b, s.sr);
        }
    }
}

pub struct SimpleSynth {
    sr: f32,
    pub perf: Performance,
    voices: [Voice; VOICES],
    counter: u64,
    noise: Rng,
    stack: [u8; 16],
    stack_len: usize,
    last_pitch: f32,
    free_lfo: f32,
    mode: u8,
    retrig: bool,
    glide_on: bool,
}

impl SimpleSynth {
    pub fn new(sample_rate: f32) -> Self {
        let mut s = Self {
            perf: Performance::default(),
            sr: sample_rate.max(1.0),
            voices: std::array::from_fn(|i| Voice::new(0x2545_F491 ^ (i as u32 * 104_729 + 3))),
            counter: 0,
            noise: Rng::new(0xC0FFEE),
            stack: [0; 16],
            stack_len: 0,
            last_pitch: -1.0,
            free_lfo: 0.0,
            mode: 0,
            retrig: false,
            glide_on: false,
        };
        s.apply_modes(&SimpleParams::default());
        s
    }

    fn apply_modes(&mut self, p: &SimpleParams) {
        self.mode = choice(p.voice_mode, 3);
        self.retrig = p.lfo_retrig >= 0.5;
        self.glide_on = p.glide > 0.0005;
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    fn start_voice(&mut self, idx: usize, pitch: u8, velocity: u8) {
        let start = if self.glide_on && self.last_pitch >= 0.0 {
            self.last_pitch
        } else {
            pitch as f32
        };
        let (retrig, free, counter) = (self.retrig, self.free_lfo, self.counter);
        let v = &mut self.voices[idx];
        let fresh = !v.active();
        v.note = pitch;
        v.velocity = velocity as f32 / 127.0;
        v.held = true;
        v.age = counter;
        v.target = pitch as f32;
        v.pitch = start;
        if fresh {
            for i in 0..MAX_SUBS {
                v.ph1[i] = if i == 0 { 0.0 } else { v.rng.next_bipolar() * 0.5 + 0.5 };
                v.ph2[i] = v.rng.next_bipolar() * 0.5 + 0.5;
            }
            v.filt = [Channel::default(); 2];
            v.expr_gain.reset();
        }
        v.drift_pitch = v.rng.next_bipolar() * 10.0;
        v.drift_cutoff = v.rng.next_bipolar() * 0.3;
        v.lfo_phase = if retrig { 0.0 } else { free };
        v.ctl = 0;
        v.env1.trigger();
        v.env2.trigger();
        self.last_pitch = pitch as f32;
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(pitch);
            return;
        }
        self.counter += 1;
        if self.mode == 1 {
            self.stack_remove(pitch);
            if self.stack_len == self.stack.len() {
                self.stack.copy_within(1.., 0);
                self.stack_len -= 1;
            }
            let legato = self.stack_len > 0 && self.voices[0].held;
            self.stack[self.stack_len] = pitch;
            self.stack_len += 1;
            if legato {
                let v = &mut self.voices[0];
                v.note = pitch;
                v.target = pitch as f32;
                if !self.glide_on {
                    v.pitch = pitch as f32;
                }
                self.last_pitch = pitch as f32;
            } else {
                self.start_voice(0, pitch, velocity);
            }
            for v in self.voices.iter_mut().skip(1) {
                v.held = false;
                v.env1.release();
                v.env2.release();
            }
            return;
        }
        let idx = self
            .voices
            .iter()
            .position(|v| !v.active())
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| !v.held)
                    .min_by(|a, b| a.1.env1_value.total_cmp(&b.1.env1_value))
                    .map(|(i, _)| i)
            })
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        self.start_voice(idx, pitch, velocity);
    }

    fn stack_remove(&mut self, pitch: u8) {
        let mut w = 0;
        for r in 0..self.stack_len {
            if self.stack[r] != pitch {
                self.stack[w] = self.stack[r];
                w += 1;
            }
        }
        self.stack_len = w;
    }

    pub fn note_off(&mut self, pitch: u8) {
        if self.mode == 1 {
            self.stack_remove(pitch);
            let glide = self.glide_on;
            let v = &mut self.voices[0];
            if v.held && v.note == pitch {
                if self.stack_len > 0 {
                    let next = self.stack[self.stack_len - 1];
                    v.note = next;
                    v.target = next as f32;
                    if !glide {
                        v.pitch = next as f32;
                    }
                    self.last_pitch = next as f32;
                } else {
                    v.held = false;
                    v.env1.release();
                    v.env2.release();
                }
            }
            return;
        }
        for v in &mut self.voices {
            if v.held && v.note == pitch {
                v.held = false;
                v.env1.release();
                v.env2.release();
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        self.stack_len = 0;
        for v in &mut self.voices {
            v.held = false;
            v.env1.release();
            v.env2.release();
        }
    }

    pub fn panic(&mut self) {
        self.stack_len = 0;
        for v in &mut self.voices {
            v.held = false;
            v.env1 = Env::new();
            v.env2 = Env::new();
            v.env1_value = 0.0;
        }
    }

    pub fn render(&mut self, p: &SimpleParams, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        self.apply_modes(p);
        let sh = Shared::new(p, self.sr, self.mode, self.perf);
        let gain = db_to_gain(p.volume_db.clamp(-80.0, 12.0)) * 1.1;
        let vel_amp = p.vel_amp.clamp(0.0, 1.0);
        let stereo = sh.subs > 1;
        let gk = gain_smooth_coef(self.sr);
        for v in self.voices.iter_mut() {
            if !v.active() {
                continue;
            }
            // Picked up at the next control tick.
            v.bend = self.perf.bend_for(v.note);
            let g_target = self.perf.gain_for(v.note);
            let vg = (1.0 - vel_amp * (1.0 - v.velocity)) * gain;
            for i in 0..n {
                if v.ctl == 0 {
                    v.control(&sh);
                    v.ctl = CONTROL;
                }
                v.ctl -= 1;
                let a = v.env1.next(&sh.e1);
                v.env1_value = a;
                if !v.env1.active() {
                    break;
                }
                let mut l = 0.0;
                let mut r = 0.0;
                for s in 0..sh.subs {
                    let x = morph(v.shape, v.ph1[s], v.dt1[s]) + basic(sh.o2_wave, v.ph2[s], v.dt2[s]) * sh.o2_level;
                    l += x * sh.sub_l[s];
                    r += x * sh.sub_r[s];
                    v.ph1[s] = wrap01(v.ph1[s] + v.dt1[s]);
                    v.ph2[s] = wrap01(v.ph2[s] + v.dt2[s]);
                }
                if sh.noise > 0.0 {
                    let nz = self.noise.next_bipolar() * sh.noise * 0.7;
                    l += nz;
                    r += nz;
                }
                let two = sh.two_pole_pair;
                let yl = v.filt[0].process(l, two, &v.ca, &v.cb);
                let yr = if stereo {
                    v.filt[1].process(r, two, &v.ca, &v.cb)
                } else {
                    yl
                };
                let g = a * vg * v.amp_mod * v.expr_gain.next(g_target, gk);
                left[i] += sat(yl) * g;
                right[i] += sat(yr) * g;
            }
        }
        self.free_lfo = wrap01(self.free_lfo + sh.lfo_rate * n as f32 / self.sr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(s: &mut SimpleSynth, p: &SimpleParams, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (cl, cr) in l.chunks_mut(200).zip(r.chunks_mut(200)) {
            s.render(p, cl, cr);
        }
        (l, r)
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
    }

    fn sine_patch() -> SimpleParams {
        SimpleParams {
            o1_shape: 0.0,
            o2_level: 0.0,
            cutoff: 20000.0,
            f_env: 0.0,
            key: 0.0,
            res: 0.0,
            drift: 0.0,
            s1: 1.0,
            ..SimpleParams::default()
        }
    }

    #[test]
    fn indices_match_labels() {
        let i = SimpleParams::INFO;
        assert_eq!(i[idx::SHAPE].name, "Shape");
        assert_eq!(i[idx::O2_WAVE].name, "Osc2 Wave");
        assert_eq!(i[idx::NOISE].name, "Noise");
        assert_eq!(i[idx::F_TYPE].name, "Filter");
        assert_eq!(i[idx::F_LFO].name, "LFO>Filter");
        assert_eq!(i[idx::A1].name, "Attack");
        assert_eq!(i[idx::E2_MODE].name, "Env2 Mode");
        assert_eq!(i[idx::S2].name, "Env2 Sustain");
        assert_eq!(i[idx::E2_SHAPE].name, "Env2>Shape");
        assert_eq!(i[idx::LFO_SHAPE].name, "LFO Shape");
        assert_eq!(i[idx::LFO_AMP].name, "LFO>Amp");
        assert_eq!(i[idx::DRIFT].name, "Drift");
        assert_eq!(i[idx::VOICE_MODE].name, "Voices");
        assert_eq!(i[idx::SPREAD].name, "Spread");
        assert_eq!(i[idx::VOLUME].name, "Volume");
        assert_eq!(SimpleParams::COUNT, idx::LFO_SYNC + 2);
    }

    #[test]
    fn fast_sine_is_accurate() {
        for i in 0..1000 {
            let ph = i as f32 / 1000.0;
            let e = (fast_sine(ph) - (ph * std::f32::consts::TAU).sin()).abs();
            assert!(e < 0.002, "phase {ph} error {e}");
        }
    }

    #[test]
    fn plays_at_the_right_pitch() {
        let mut s = SimpleSynth::new(SR);
        let p = sine_patch();
        s.note_on(69, 100);
        let (l, _) = run(&mut s, &p, SR as usize);
        let c = crossings(&l[4800..]) as f32 / 0.9;
        assert!((c - 440.0).abs() < 4.0, "crossings {c}");
    }

    #[test]
    fn shape_morph_changes_timbre_continuously() {
        let brightness = |shape: f32| {
            let mut s = SimpleSynth::new(SR);
            let p = SimpleParams {
                o1_shape: shape,
                ..sine_patch()
            };
            s.note_on(48, 100);
            let (l, _) = run(&mut s, &p, 9600);
            let d: Vec<f32> = l[2400..].windows(2).map(|w| w[1] - w[0]).collect();
            rms(&d) / rms(&l[2400..]).max(1e-9)
        };
        let sine = brightness(0.0);
        let tri = brightness(1.0 / 3.0);
        let saw = brightness(2.0 / 3.0);
        assert!(sine < tri && tri < saw, "{sine} {tri} {saw}");
        assert!(brightness(0.5).is_finite());
    }

    #[test]
    fn default_peaks_in_range_and_releases() {
        let mut s = SimpleSynth::new(SR);
        let p = SimpleParams::default();
        s.note_on(60, 100);
        let (l, r) = run(&mut s, &p, 24000);
        let pk = peak(&l).max(peak(&r));
        assert!(pk > 0.15 && pk < 0.9, "peak {pk}");
        s.note_off(60);
        run(&mut s, &p, SR as usize * 2);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn panic_silences() {
        let mut s = SimpleSynth::new(SR);
        let p = SimpleParams {
            r1: 10.0,
            ..SimpleParams::default()
        };
        s.note_on(60, 100);
        s.note_on(64, 100);
        run(&mut s, &p, 1000);
        s.panic();
        let (l, _) = run(&mut s, &p, 400);
        assert_eq!(peak(&l), 0.0);
    }

    #[test]
    fn cutoff_darkens() {
        let measure = |cutoff: f32, t: f32| {
            let mut s = SimpleSynth::new(SR);
            let p = SimpleParams {
                cutoff,
                f_type: t,
                f_env: 0.0,
                key: 0.0,
                drift: 0.0,
                ..SimpleParams::default()
            };
            s.note_on(48, 100);
            let (l, _) = run(&mut s, &p, 9600);
            let d: Vec<f32> = l[4800..].windows(2).map(|w| w[1] - w[0]).collect();
            rms(&d) / rms(&l[4800..]).max(1e-9)
        };
        for t in [0.0, 1.0] {
            let bright = measure(10000.0, t);
            let dark = measure(300.0, t);
            assert!(dark < bright * 0.5, "type {t}: dark {dark} bright {bright}");
        }
        // Type II is steeper.
        assert!(measure(500.0, 1.0) < measure(500.0, 0.0));
    }

    #[test]
    fn cycling_env2_modulates_repeatedly() {
        let mut s = SimpleSynth::new(SR);
        let p = SimpleParams {
            e2_mode: 1.0,
            a2: 0.01,
            d2: 0.03,
            ..SimpleParams::default()
        };
        s.note_on(60, 100);
        run(&mut s, &p, 4800);
        // After 100 ms a one-shot env 2 would have settled; a cycling one keeps
        // moving through its whole range.
        let mut lo: f32 = 1.0;
        let mut hi: f32 = 0.0;
        for _ in 0..200 {
            s.voices[0].control(&Shared::new(&p, SR, 0, Performance::default()));
            let v = s.voices[0].env2.value;
            lo = lo.min(v);
            hi = hi.max(v);
        }
        assert!(hi > 0.9 && lo < 0.1, "lo {lo} hi {hi}");
    }

    #[test]
    fn drift_varies_between_notes() {
        let p = SimpleParams {
            drift: 1.0,
            ..sine_patch()
        };
        let mut s = SimpleSynth::new(SR);
        s.note_on(69, 100);
        s.note_on(69, 100);
        let (l, _) = run(&mut s, &p, 48000);
        // Two copies of the same note slightly apart beat against each other.
        let first = rms(&l[0..12000]);
        let mut min_rms = f32::MAX;
        let mut max_rms: f32 = 0.0;
        for c in l[2400..].chunks(2400) {
            let r = rms(c);
            min_rms = min_rms.min(r);
            max_rms = max_rms.max(r);
        }
        assert!(first > 0.0);
        assert!(max_rms > min_rms * 1.1, "no beating: {min_rms} {max_rms}");
    }

    #[test]
    fn voice_modes() {
        for mode in 0..4 {
            let mut s = SimpleSynth::new(SR);
            let p = SimpleParams {
                voice_mode: mode as f32,
                spread: 1.0,
                ..SimpleParams::default()
            };
            run(&mut s, &p, 32);
            s.note_on(60, 100);
            s.note_on(67, 100);
            let (l, r) = run(&mut s, &p, 9600);
            let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
            if mode >= 2 {
                assert!(rms(&diff) > 0.005, "mode {mode} not stereo");
            } else {
                assert!(rms(&diff) < 1e-6, "mode {mode} not mono");
            }
            assert_eq!(s.active_voices(), if mode == 1 { 1 } else { 2 });
            assert!(peak(&l) < 1.2, "mode {mode} peak {}", peak(&l));
        }
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let mut lo = SimpleParams::default();
        let mut hi = SimpleParams::default();
        for (i, inf) in SimpleParams::INFO.iter().enumerate() {
            lo.set(i, inf.min);
            hi.set(i, inf.max);
        }
        hi.volume_db = 0.0;
        for p in [lo, hi] {
            for mode in 0..4 {
                let p = SimpleParams {
                    voice_mode: mode as f32,
                    ..p
                };
                let mut s = SimpleSynth::new(SR);
                for n in [0u8, 30, 60, 100, 127] {
                    s.note_on(n, 127);
                }
                let (l, r) = run(&mut s, &p, 9600);
                assert!(l.iter().chain(&r).all(|x| x.is_finite()));
                assert!(peak(&l) < 8.0);
            }
        }
    }

    #[test]
    fn presets_are_valid_and_sound() {
        let presets = presets();
        assert!(presets.len() >= 4);
        for (_, name, p) in presets {
            for (i, inf) in SimpleParams::INFO.iter().enumerate() {
                let v = p.get(i);
                assert!(v >= inf.min && v <= inf.max, "{name}: {}", inf.name);
            }
            let mut s = SimpleSynth::new(SR);
            s.note_on(60, 100);
            let (l, r) = run(&mut s, &p, 48000);
            let pk = peak(&l).max(peak(&r));
            assert!(pk > 0.05 && pk < 1.2, "{name}: peak {pk}");
        }
    }
}
