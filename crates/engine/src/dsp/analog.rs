//! Analog Synth: a virtual-analog subtractive synthesizer with two
//! band-limited oscillators (PolyBLEP/PolyBLAMP), a noise source, two
//! zero-delay-feedback filters (state-variable and ladder) in serial or
//! parallel, filter and amp envelopes, two LFOs, glide, mono/poly modes and
//! up to four unison voices per note.
//!
//! Modulation (pitch, LFOs, filter envelope, cutoff coefficients) runs at a
//! control rate of [`CONTROL`] samples per voice; oscillators, filters and the
//! amp envelope run per sample.

use super::envelope::Adsr;
use super::{lfo_hz, midi_to_hz, Performance, Rng, SYNC_RATES, SYNC_RATE_DEFAULT};
use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};
use crate::model::db_to_gain;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);
const TOGGLE: ParamKind = ParamKind::Toggle;
const OSC_WAVES: ParamKind = ParamKind::Choice(&["Saw", "Pulse", "Triangle", "Sine"]);
const FILTER_TYPES: ParamKind = ParamKind::Choice(&["LP12", "LP24", "BP", "HP", "Notch"]);
const ROUTING: ParamKind = ParamKind::Choice(&["Serial", "Parallel"]);
const LFO_SHAPES: ParamKind = ParamKind::Choice(&["Sine", "Triangle", "Saw", "Square", "S&H"]);
const GLIDE_MODES: ParamKind = ParamKind::Choice(&["Off", "Legato", "Always"]);
const VOICE_MODES: ParamKind = ParamKind::Choice(&["Poly", "Mono"]);

/// Polyphony of the voice pool.
pub const VOICES: usize = 16;
/// Maximum unison oscillators per voice.
pub const MAX_UNISON: usize = 4;
/// Control-rate period in samples.
pub const CONTROL: u32 = 16;

device_params! {
    pub struct AnalogParams {
        o1_wave: "Osc1 Wave", 0.0, 3.0, 0.0, OSC_WAVES, Unit::Plain;
        o1_oct: "Osc1 Oct", -3.0, 3.0, 0.0, LIN, Unit::Integer;
        o1_semi: "Osc1 Semi", -12.0, 12.0, 0.0, LIN, Unit::Semitones;
        o1_fine: "Osc1 Fine", -50.0, 50.0, 0.0, LIN, Unit::Integer;
        o1_pw: "Osc1 PW", 0.05, 0.95, 0.5, LIN, Unit::Percent;
        o1_level: "Osc1 Level", 0.0, 1.0, 0.8, LIN, Unit::Percent;
        o2_wave: "Osc2 Wave", 0.0, 3.0, 0.0, OSC_WAVES, Unit::Plain;
        o2_oct: "Osc2 Oct", -3.0, 3.0, 0.0, LIN, Unit::Integer;
        o2_semi: "Osc2 Semi", -12.0, 12.0, 0.0, LIN, Unit::Semitones;
        o2_fine: "Osc2 Fine", -50.0, 50.0, 7.0, LIN, Unit::Integer;
        o2_pw: "Osc2 PW", 0.05, 0.95, 0.5, LIN, Unit::Percent;
        o2_level: "Osc2 Level", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        o2_sync: "Osc2 Sync", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        noise_level: "Noise", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        noise_color: "Noise Color", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        f1_type: "F1 Type", 0.0, 4.0, 1.0, FILTER_TYPES, Unit::Plain;
        f1_cutoff: "F1 Cutoff", 20.0, 20000.0, 1200.0, LOG, Unit::Hz;
        f1_res: "F1 Reso", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        f1_drive: "F1 Drive", 0.0, 1.0, 0.15, LIN, Unit::Percent;
        f1_key: "F1 Key", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        f1_env: "F1 Env", -6.0, 6.0, 2.5, LIN, Unit::Plain;
        f2_on: "F2 On", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        f2_type: "F2 Type", 0.0, 4.0, 3.0, FILTER_TYPES, Unit::Plain;
        f2_offset: "F2 Offset", -48.0, 48.0, -24.0, LIN, Unit::Semitones;
        f2_res: "F2 Reso", 0.0, 1.0, 0.2, LIN, Unit::Percent;
        routing: "Routing", 0.0, 1.0, 0.0, ROUTING, Unit::Plain;
        fe_attack: "FEnv Attack", 0.001, 20.0, 0.003, LOG, Unit::Seconds;
        fe_decay: "FEnv Decay", 0.001, 20.0, 0.4, LOG, Unit::Seconds;
        fe_sustain: "FEnv Sustain", 0.0, 1.0, 0.2, LIN, Unit::Percent;
        fe_release: "FEnv Release", 0.001, 20.0, 0.3, LOG, Unit::Seconds;
        fe_vel: "FEnv Vel", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        ae_attack: "Attack", 0.001, 20.0, 0.003, LOG, Unit::Seconds;
        ae_decay: "Decay", 0.001, 20.0, 0.6, LOG, Unit::Seconds;
        ae_sustain: "Sustain", 0.0, 1.0, 0.8, LIN, Unit::Percent;
        ae_release: "Release", 0.001, 20.0, 0.2, LOG, Unit::Seconds;
        ae_vel: "Amp Vel", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        l1_shape: "LFO1 Shape", 0.0, 4.0, 0.0, LFO_SHAPES, Unit::Plain;
        l1_rate: "LFO1 Rate", 0.05, 40.0, 4.0, LOG, Unit::Hz;
        l1_retrig: "LFO1 Retrig", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        l1_pitch: "LFO1>Pitch", 0.0, 24.0, 0.0, LIN, Unit::Plain;
        l1_filter: "LFO1>Filter", 0.0, 6.0, 0.0, LIN, Unit::Plain;
        l1_amp: "LFO1>Amp", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        l1_pwm: "LFO1>PWM", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        l2_shape: "LFO2 Shape", 0.0, 4.0, 1.0, LFO_SHAPES, Unit::Plain;
        l2_rate: "LFO2 Rate", 0.05, 40.0, 0.5, LOG, Unit::Hz;
        l2_retrig: "LFO2 Retrig", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        l2_pitch: "LFO2>Pitch", 0.0, 24.0, 0.0, LIN, Unit::Plain;
        l2_filter: "LFO2>Filter", 0.0, 6.0, 0.0, LIN, Unit::Plain;
        l2_amp: "LFO2>Amp", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        l2_pwm: "LFO2>PWM", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        glide_mode: "Glide", 0.0, 2.0, 0.0, GLIDE_MODES, Unit::Plain;
        glide_time: "Glide Time", 0.001, 5.0, 0.08, LOG, Unit::Seconds;
        voice_mode: "Voice Mode", 0.0, 1.0, 0.0, VOICE_MODES, Unit::Plain;
        unison: "Unison", 1.0, 4.0, 1.0, LIN, Unit::Integer;
        uni_detune: "Uni Detune", 0.0, 50.0, 12.0, LIN, Unit::Integer;
        uni_spread: "Uni Spread", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        vib_rate: "Vib Rate", 0.1, 12.0, 5.5, LOG, Unit::Hz;
        vib_amount: "Vibrato", 0.0, 1.0, 0.0, LIN, Unit::Plain;
        volume_db: "Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
        l1_sync: "LFO1 Sync", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        l1_sync_rate: "LFO1 Sync Rate", 0.0, 10.0, SYNC_RATE_DEFAULT, SYNC_RATES, Unit::Plain;
        l2_sync: "LFO2 Sync", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        l2_sync_rate: "LFO2 Sync Rate", 0.0, 10.0, SYNC_RATE_DEFAULT, SYNC_RATES, Unit::Plain;
    }
}

/// Parameter indices into [`AnalogParams::INFO`] (for panels).
pub mod idx {
    pub const O1_WAVE: usize = 0;
    pub const O1_PW: usize = 4;
    pub const O2_WAVE: usize = 6;
    pub const O2_PW: usize = 10;
    pub const O2_SYNC: usize = 12;
    pub const NOISE: usize = 13;
    pub const F1_TYPE: usize = 15;
    pub const F1_ENV: usize = 20;
    pub const F2_ON: usize = 21;
    pub const ROUTING: usize = 25;
    pub const FE_ATTACK: usize = 26;
    pub const FE_VEL: usize = 30;
    pub const AE_ATTACK: usize = 31;
    pub const AE_VEL: usize = 35;
    pub const L1_SHAPE: usize = 36;
    pub const L2_SHAPE: usize = 43;
    pub const LFO_LEN: usize = 7;
    pub const GLIDE_MODE: usize = 50;
    pub const VOICE_MODE: usize = 52;
    pub const UNISON: usize = 53;
    pub const VIB_RATE: usize = 56;
    pub const VOLUME: usize = 58;
    /// LFO sync toggles; each one's synced rate follows it.
    pub const L1_SYNC: usize = 59;
    pub const L2_SYNC: usize = 61;
}

/// Automatable parameters, in [`AnalogParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; AnalogParams::COUNT] = specs_from_info(AnalogParams::INFO);

/// Factory presets: (browser category, name, settings).
pub fn presets() -> Vec<(&'static str, &'static str, AnalogParams)> {
    let d = AnalogParams::default();
    vec![
        (
            "Bass",
            "Rubber Bass",
            AnalogParams {
                o1_wave: 0.0,
                o2_wave: 1.0,
                o2_oct: -1.0,
                o2_fine: 0.0,
                o2_pw: 0.35,
                o2_level: 0.7,
                f1_type: 1.0,
                f1_cutoff: 220.0,
                f1_res: 0.45,
                f1_drive: 0.45,
                f1_key: 0.4,
                f1_env: 3.5,
                fe_attack: 0.001,
                fe_decay: 0.22,
                fe_sustain: 0.05,
                fe_release: 0.12,
                fe_vel: 0.7,
                ae_attack: 0.001,
                ae_decay: 0.4,
                ae_sustain: 0.75,
                ae_release: 0.08,
                voice_mode: 1.0,
                glide_mode: 1.0,
                glide_time: 0.06,
                volume_db: -5.0,
                ..d
            },
        ),
        (
            "Lead",
            "Sync Lead",
            AnalogParams {
                o1_wave: 0.0,
                o1_level: 0.5,
                o2_wave: 0.0,
                o2_oct: 1.0,
                o2_fine: 0.0,
                o2_sync: 1.0,
                o2_level: 0.8,
                f1_type: 0.0,
                f1_cutoff: 3500.0,
                f1_res: 0.2,
                f1_env: 1.0,
                l1_shape: 1.0,
                l1_rate: 0.3,
                l1_pitch: 7.0,
                voice_mode: 1.0,
                glide_mode: 2.0,
                glide_time: 0.05,
                vib_amount: 0.25,
                vib_rate: 5.5,
                ae_release: 0.25,
                volume_db: -9.0,
                ..d
            },
        ),
        (
            "Pad",
            "Warm Strings",
            AnalogParams {
                o1_wave: 0.0,
                o2_wave: 1.0,
                o2_fine: 9.0,
                o2_pw: 0.3,
                o2_level: 0.7,
                f1_type: 1.0,
                f1_cutoff: 1600.0,
                f1_res: 0.15,
                f1_env: 1.0,
                fe_attack: 0.8,
                fe_decay: 2.0,
                fe_sustain: 0.6,
                fe_release: 1.5,
                ae_attack: 0.6,
                ae_decay: 1.5,
                ae_sustain: 0.85,
                ae_release: 1.6,
                l1_shape: 1.0,
                l1_rate: 0.35,
                l1_pwm: 0.6,
                l2_rate: 0.15,
                l2_filter: 0.4,
                unison: 3.0,
                uni_detune: 14.0,
                uni_spread: 0.9,
                volume_db: -8.0,
                ..d
            },
        ),
        (
            "Pluck",
            "Analog Pluck",
            AnalogParams {
                o1_wave: 1.0,
                o1_pw: 0.4,
                o2_wave: 0.0,
                o2_semi: 7.0,
                o2_fine: 4.0,
                o2_level: 0.35,
                f1_type: 1.0,
                f1_cutoff: 400.0,
                f1_res: 0.35,
                f1_env: 4.0,
                f1_key: 0.7,
                fe_attack: 0.001,
                fe_decay: 0.25,
                fe_sustain: 0.0,
                fe_release: 0.25,
                fe_vel: 0.8,
                ae_attack: 0.001,
                ae_decay: 0.6,
                ae_sustain: 0.0,
                ae_release: 0.4,
                volume_db: -7.0,
                ..d
            },
        ),
        (
            "Keys",
            "Hollow Organ",
            AnalogParams {
                o1_wave: 2.0,
                o1_level: 0.9,
                o2_wave: 1.0,
                o2_oct: 1.0,
                o2_fine: 0.0,
                o2_pw: 0.5,
                o2_level: 0.35,
                f1_type: 0.0,
                f1_cutoff: 2600.0,
                f1_res: 0.1,
                f1_env: 0.5,
                f2_on: 1.0,
                f2_type: 3.0,
                f2_offset: -36.0,
                routing: 0.0,
                ae_attack: 0.005,
                ae_decay: 0.5,
                ae_sustain: 1.0,
                ae_release: 0.08,
                vib_amount: 0.12,
                vib_rate: 6.5,
                volume_db: -9.0,
                ..d
            },
        ),
        (
            "Textures",
            "Resonant Sweep",
            AnalogParams {
                o1_wave: 0.0,
                o2_wave: 0.0,
                o2_fine: -12.0,
                noise_level: 0.25,
                noise_color: 0.4,
                f1_type: 2.0,
                f1_cutoff: 800.0,
                f1_res: 0.8,
                f1_env: 0.0,
                f2_on: 1.0,
                f2_type: 0.0,
                f2_offset: 12.0,
                f2_res: 0.6,
                routing: 1.0,
                l1_shape: 0.0,
                l1_rate: 0.12,
                l1_filter: 2.5,
                l2_shape: 4.0,
                l2_rate: 6.0,
                l2_filter: 0.5,
                ae_attack: 1.2,
                ae_release: 2.5,
                ae_sustain: 1.0,
                unison: 2.0,
                uni_detune: 8.0,
                volume_db: -8.0,
                ..d
            },
        ),
    ]
}

// ----- band-limited oscillators ------------------------------------------------

/// PolyBLEP residual for a unit-normalised step (jump of 2) at phase 0.
#[inline]
pub(crate) fn blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// PolyBLAMP residual (integrated PolyBLEP) for a slope change at phase 0.
#[inline]
pub(crate) fn blamp(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt - 1.0;
        -x * x * x / 3.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x * x / 3.0
    } else {
        0.0
    }
}

#[inline]
pub(crate) fn wrap01(x: f32) -> f32 {
    x - x.floor()
}

#[inline]
pub(crate) fn saw(phase: f32, dt: f32) -> f32 {
    2.0 * phase - 1.0 - blep(phase, dt)
}

/// Band-limited pulse with width `pw`, DC removed.
#[inline]
pub(crate) fn pulse(phase: f32, dt: f32, pw: f32) -> f32 {
    let mut v = if phase < pw { 1.0 } else { -1.0 };
    v += blep(phase, dt);
    v -= blep(wrap01(phase - pw + 1.0), dt);
    v - (2.0 * pw - 1.0)
}

/// Band-limited triangle (peak at phase 0, trough at 0.5).
#[inline]
pub(crate) fn triangle(phase: f32, dt: f32) -> f32 {
    let mut v = 2.0 * (2.0 * phase - 1.0).abs() - 1.0;
    v -= 4.0 * dt * blamp(phase, dt);
    v += 4.0 * dt * blamp(wrap01(phase + 0.5), dt);
    v
}

#[inline]
fn osc(wave: u8, phase: f32, dt: f32, pw: f32) -> f32 {
    match wave {
        0 => saw(phase, dt),
        1 => pulse(phase, dt, pw),
        2 => triangle(phase, dt),
        _ => (phase * std::f32::consts::TAU).sin(),
    }
}

/// Smooth saturator: tanh-like rational curve, exactly ±1 beyond ±3.
#[inline]
pub(crate) fn sat(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

// ----- filters -----------------------------------------------------------------

/// Control-rate filter coefficients.
#[derive(Debug, Clone, Copy, Default)]
struct Coef {
    g: f32,
    /// SVF damping (2 = no resonance) or ladder feedback (0..4).
    k: f32,
    drive: f32,
    mode: u8,
}

impl Coef {
    fn new(mode: u8, cutoff: f32, res: f32, drive: f32, sr: f32) -> Self {
        let fc = cutoff.clamp(16.0, sr * 0.45);
        let g = (std::f32::consts::PI * fc / sr).tan();
        let res = res.clamp(0.0, 1.0);
        let k = if mode == 1 { 4.05 * res } else { 2.0 - 1.95 * res };
        Self {
            g,
            k,
            drive: 1.0 + drive.clamp(0.0, 1.0) * 5.0,
            mode,
        }
    }
}

/// One channel of filter state: SVF integrators or four ladder stages.
#[derive(Debug, Clone, Copy, Default)]
struct FilterState {
    s: [f32; 4],
}

impl FilterState {
    #[inline]
    fn process(&mut self, x: f32, c: &Coef) -> f32 {
        let x = if c.drive > 1.001 {
            sat(x * c.drive) / c.drive.sqrt()
        } else {
            x
        } + 1e-20;
        if c.mode == 1 {
            self.ladder(x, c)
        } else {
            self.svf(x, c)
        }
    }

    #[inline]
    fn svf(&mut self, x: f32, c: &Coef) -> f32 {
        let g = c.g;
        let k = c.k;
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let [ic1, ic2, _, _] = &mut self.s;
        let v3 = x - *ic2;
        let v1 = a1 * *ic1 + a2 * v3;
        let v2 = *ic2 + a2 * *ic1 + a3 * v3;
        *ic1 = 2.0 * v1 - *ic1;
        *ic2 = 2.0 * v2 - *ic2;
        match c.mode {
            0 => v2,
            2 => v1 * k,
            3 => x - k * v1 - v2,
            _ => x - k * v1,
        }
    }

    /// Zero-delay-feedback 4-pole ladder (TPT one-poles, feedback solved
    /// exactly, saturation on the feedback junction).
    #[inline]
    fn ladder(&mut self, x: f32, c: &Coef) -> f32 {
        let g = c.g;
        let big_g = g / (1.0 + g);
        let beta = 1.0 / (1.0 + g);
        let g2 = big_g * big_g;
        let s = &mut self.s;
        let sigma = beta * (g2 * big_g * s[0] + g2 * s[1] + big_g * s[2] + s[3]);
        let comp = 1.0 + 0.6 * c.k;
        let u = (x * comp - c.k * sigma) / (1.0 + c.k * g2 * g2);
        let mut y = sat(u);
        for st in s.iter_mut() {
            let v = (y - *st) * big_g;
            let out = v + *st;
            *st = out + v;
            y = out;
        }
        y
    }
}

// ----- LFO -----------------------------------------------------------------------

#[inline]
fn lfo_value(shape: u8, phase: f32, held: f32) -> f32 {
    match shape {
        0 => (phase * std::f32::consts::TAU).sin(),
        1 => 1.0 - 4.0 * (phase - 0.5).abs(),
        2 => 1.0 - 2.0 * phase,
        3 => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        _ => held,
    }
}

// ----- voices ------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    age: u64,
    /// Current (gliding) pitch and its target, in MIDI notes.
    pitch: f32,
    target: f32,
    phase1: [f32; MAX_UNISON],
    phase2: [f32; MAX_UNISON],
    amp: Adsr,
    fenv: Adsr,
    fenv_value: f32,
    lfo_phase: [f32; 2],
    lfo_held: [f32; 2],
    vib_phase: f32,
    rng: Rng,
    noise_lp: f32,
    f1: [FilterState; 2],
    f2: [FilterState; 2],
    ctl: u32,
    // Control-rate values.
    dt1: [f32; MAX_UNISON],
    dt2: [f32; MAX_UNISON],
    pw1: f32,
    pw2: f32,
    amp_mod: f32,
    amp_mod_smooth: f32,
    c1: Coef,
    c2: Coef,
}

impl Voice {
    fn new(sr: f32, seed: u32) -> Self {
        Self {
            note: 0,
            velocity: 0.0,
            held: false,
            age: 0,
            pitch: 60.0,
            target: 60.0,
            phase1: [0.0; MAX_UNISON],
            phase2: [0.0; MAX_UNISON],
            amp: Adsr::new(sr),
            fenv: Adsr::new(sr),
            fenv_value: 0.0,
            lfo_phase: [0.0; 2],
            lfo_held: [0.0; 2],
            vib_phase: 0.0,
            rng: Rng::new(seed),
            noise_lp: 0.0,
            f1: [FilterState::default(); 2],
            f2: [FilterState::default(); 2],
            ctl: 0,
            dt1: [0.0; MAX_UNISON],
            dt2: [0.0; MAX_UNISON],
            pw1: 0.5,
            pw2: 0.5,
            amp_mod: 1.0,
            amp_mod_smooth: 1.0,
            c1: Coef::default(),
            c2: Coef::default(),
        }
    }

    fn start(
        &mut self,
        note: u8,
        velocity: u8,
        start_pitch: f32,
        counter: u64,
        lfo_phase: [f32; 2],
        retrig: [bool; 2],
    ) {
        let fresh = !self.amp.is_active();
        self.note = note;
        self.velocity = velocity as f32 / 127.0;
        self.held = true;
        self.age = counter;
        self.target = note as f32;
        self.pitch = start_pitch;
        if fresh {
            for i in 0..MAX_UNISON {
                // Unison copies start at random phases so they do not comb;
                // the first copy starts at zero for a consistent attack.
                self.phase1[i] = if i == 0 {
                    0.0
                } else {
                    self.rng.next_bipolar() * 0.5 + 0.5
                };
                self.phase2[i] = if i == 0 {
                    0.0
                } else {
                    self.rng.next_bipolar() * 0.5 + 0.5
                };
            }
            self.f1 = [FilterState::default(); 2];
            self.f2 = [FilterState::default(); 2];
            self.noise_lp = 0.0;
            self.vib_phase = 0.0;
            self.amp_mod_smooth = self.amp_mod;
        }
        for (i, r) in retrig.iter().enumerate() {
            self.lfo_phase[i] = if *r { 0.0 } else { lfo_phase[i] };
        }
        self.ctl = 0;
        self.amp.trigger();
        self.fenv.trigger();
    }

    fn release(&mut self) {
        self.held = false;
        self.amp.release();
        self.fenv.release();
    }

    /// Recomputes pitch, LFO and filter values (once per [`CONTROL`] samples).
    fn control(&mut self, p: &Shared, sr: f32) {
        let step = CONTROL as f32 / sr;
        // Glide.
        if (self.target - self.pitch).abs() > 1e-4 {
            self.pitch += (self.target - self.pitch) * p.glide_coef;
        } else {
            self.pitch = self.target;
        }
        // LFOs.
        let mut lfo = [0.0f32; 2];
        for (i, l) in lfo.iter_mut().enumerate() {
            let ph = self.lfo_phase[i] + p.lfo_rate[i] * step;
            if ph >= 1.0 {
                self.lfo_held[i] = self.rng.next_bipolar();
            }
            self.lfo_phase[i] = wrap01(ph);
            *l = lfo_value(p.lfo_shape[i], self.lfo_phase[i], self.lfo_held[i]);
        }
        self.vib_phase = wrap01(self.vib_phase + p.vib_rate * step);
        let vib = (self.vib_phase * std::f32::consts::TAU).sin() * p.vib_amount;
        let pitch_mod = lfo[0] * p.lfo_pitch[0] + lfo[1] * p.lfo_pitch[1] + vib;
        let base = self.pitch + pitch_mod;
        let f1 = midi_to_hz(base + p.o1_offset);
        let f2 = midi_to_hz(base + p.o2_offset);
        let nyq = 0.48;
        for u in 0..p.unison {
            let ratio = p.uni_ratio[u];
            self.dt1[u] = (f1 * ratio / sr).clamp(1e-6, nyq);
            self.dt2[u] = (f2 * ratio / sr).clamp(1e-6, nyq);
        }
        let pwm = lfo[0] * p.lfo_pwm[0] + lfo[1] * p.lfo_pwm[1];
        self.pw1 = (p.o1_pw + pwm * 0.45).clamp(0.05, 0.95);
        self.pw2 = (p.o2_pw + pwm * 0.45).clamp(0.05, 0.95);
        let mut am = 1.0;
        for (i, l) in lfo.iter().enumerate() {
            am *= 1.0 - p.lfo_amp[i] * (0.5 - 0.5 * l);
        }
        self.amp_mod = am;
        // Filters.
        let env_depth = p.f1_env * (1.0 - p.fe_vel * (1.0 - self.velocity));
        let octaves = env_depth * self.fenv_value
            + lfo[0] * p.lfo_filter[0]
            + lfo[1] * p.lfo_filter[1]
            + p.f1_key * (self.pitch - 60.0) / 12.0;
        let cutoff = p.f1_cutoff * octaves.clamp(-12.0, 12.0).exp2();
        self.c1 = Coef::new(p.f1_type, cutoff, p.f1_res, p.f1_drive, sr);
        if p.f2_on {
            let c2 = cutoff * (p.f2_offset / 12.0).exp2();
            self.c2 = Coef::new(p.f2_type, c2, p.f2_res, 0.0, sr);
        }
    }
}

/// Parameter values derived once per render call.
struct Shared {
    o1_wave: u8,
    o2_wave: u8,
    o1_offset: f32,
    o2_offset: f32,
    o1_pw: f32,
    o2_pw: f32,
    o1_level: f32,
    o2_level: f32,
    sync: bool,
    noise_level: f32,
    noise_coef: f32,
    f1_type: u8,
    f1_cutoff: f32,
    f1_res: f32,
    f1_drive: f32,
    f1_key: f32,
    f1_env: f32,
    f2_on: bool,
    f2_type: u8,
    f2_offset: f32,
    f2_res: f32,
    parallel: bool,
    fe_vel: f32,
    lfo_shape: [u8; 2],
    lfo_rate: [f32; 2],
    lfo_pitch: [f32; 2],
    lfo_filter: [f32; 2],
    lfo_amp: [f32; 2],
    lfo_pwm: [f32; 2],
    glide_coef: f32,
    unison: usize,
    uni_ratio: [f32; MAX_UNISON],
    uni_left: [f32; MAX_UNISON],
    uni_right: [f32; MAX_UNISON],
    vib_rate: f32,
    vib_amount: f32,
}

pub(crate) fn choice(v: f32, max: u8) -> u8 {
    (v.round().max(0.0) as u8).min(max)
}

impl Shared {
    fn new(p: &AnalogParams, sr: f32, perf: Performance) -> Self {
        let unison = (p.unison.round() as usize).clamp(1, MAX_UNISON);
        let mut uni_ratio = [1.0; MAX_UNISON];
        let mut uni_left = [1.0; MAX_UNISON];
        let mut uni_right = [1.0; MAX_UNISON];
        let norm = 1.0 / (unison as f32).sqrt();
        for u in 0..unison {
            let pos = if unison == 1 {
                0.0
            } else {
                u as f32 / (unison - 1) as f32 * 2.0 - 1.0
            };
            uni_ratio[u] = (pos * p.uni_detune.clamp(0.0, 50.0) / 1200.0).exp2();
            let pan = if unison == 1 {
                0.0
            } else {
                pos * p.uni_spread.clamp(0.0, 1.0)
            };
            let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
            // Constant power, normalised so a centred copy has gain `norm`.
            uni_left[u] = angle.cos() * std::f32::consts::SQRT_2 * norm;
            uni_right[u] = angle.sin() * std::f32::consts::SQRT_2 * norm;
        }
        let color = p.noise_color.clamp(0.0, 1.0);
        let noise_fc = (200.0 * (color * 6.5).exp2()).min(sr * 0.45);
        let glide = p.glide_time.max(0.001);
        Self {
            o1_wave: choice(p.o1_wave, 3),
            o2_wave: choice(p.o2_wave, 3),
            o1_offset: p.o1_oct.round() * 12.0 + p.o1_semi.round() + p.o1_fine / 100.0 + perf.bend,
            o2_offset: p.o2_oct.round() * 12.0 + p.o2_semi.round() + p.o2_fine / 100.0 + perf.bend,
            o1_pw: p.o1_pw.clamp(0.05, 0.95),
            o2_pw: p.o2_pw.clamp(0.05, 0.95),
            o1_level: p.o1_level.clamp(0.0, 1.0),
            o2_level: p.o2_level.clamp(0.0, 1.0),
            sync: p.o2_sync >= 0.5,
            noise_level: p.noise_level.clamp(0.0, 1.0),
            noise_coef: 1.0 - (-std::f32::consts::TAU * noise_fc / sr).exp(),
            f1_type: choice(p.f1_type, 4),
            f1_cutoff: p.f1_cutoff.clamp(20.0, 20000.0),
            f1_res: p.f1_res,
            f1_drive: p.f1_drive,
            f1_key: p.f1_key.clamp(0.0, 1.0),
            f1_env: p.f1_env.clamp(-6.0, 6.0),
            f2_on: p.f2_on >= 0.5,
            f2_type: choice(p.f2_type, 4),
            f2_offset: p.f2_offset.clamp(-48.0, 48.0),
            f2_res: p.f2_res,
            parallel: p.routing >= 0.5,
            fe_vel: p.fe_vel.clamp(0.0, 1.0),
            lfo_shape: [choice(p.l1_shape, 4), choice(p.l2_shape, 4)],
            lfo_rate: [
                lfo_hz(p.l1_rate, p.l1_sync >= 0.5, p.l1_sync_rate, perf.tempo).clamp(0.01, 100.0),
                lfo_hz(p.l2_rate, p.l2_sync >= 0.5, p.l2_sync_rate, perf.tempo).clamp(0.01, 100.0),
            ],
            lfo_pitch: [p.l1_pitch.clamp(0.0, 24.0), p.l2_pitch.clamp(0.0, 24.0)],
            lfo_filter: [p.l1_filter.clamp(0.0, 6.0), p.l2_filter.clamp(0.0, 6.0)],
            lfo_amp: [p.l1_amp.clamp(0.0, 1.0), p.l2_amp.clamp(0.0, 1.0)],
            lfo_pwm: [p.l1_pwm.clamp(0.0, 1.0), p.l2_pwm.clamp(0.0, 1.0)],
            glide_coef: 1.0 - (-(CONTROL as f32) * 4.0 / (glide * sr)).exp(),
            unison,
            uni_ratio,
            uni_left,
            uni_right,
            vib_rate: p.vib_rate.clamp(0.01, 50.0),
            vib_amount: p.vib_amount.clamp(0.0, 2.0),
        }
    }
}

pub struct AnalogSynth {
    sr: f32,
    pub perf: Performance,
    voices: [Voice; VOICES],
    counter: u64,
    /// Held notes in mono mode, oldest first.
    stack: [u8; 16],
    stack_len: usize,
    last_pitch: f32,
    free_lfo: [f32; 2],
    // Settings from the last render that note handling depends on.
    mono: bool,
    glide_mode: u8,
    retrig: [bool; 2],
}

impl AnalogSynth {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let mut s = Self {
            perf: Performance::default(),
            sr,
            voices: std::array::from_fn(|i| Voice::new(sr, 0x9E37_79B9 ^ (i as u32 * 7919 + 1))),
            counter: 0,
            stack: [0; 16],
            stack_len: 0,
            last_pitch: -1.0,
            free_lfo: [0.0; 2],
            mono: false,
            glide_mode: 0,
            retrig: [false; 2],
        };
        s.apply_modes(&AnalogParams::default());
        s
    }

    fn apply_modes(&mut self, p: &AnalogParams) {
        self.mono = p.voice_mode >= 0.5;
        self.glide_mode = choice(p.glide_mode, 2);
        self.retrig = [p.l1_retrig >= 0.5, p.l2_retrig >= 0.5];
    }

    /// Number of sounding voices.
    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.amp.is_active()).count()
    }

    fn any_held(&self) -> bool {
        self.voices.iter().any(|v| v.held)
    }

    fn start_pitch(&self, note: u8, legato: bool) -> f32 {
        let glide = match self.glide_mode {
            1 => legato,
            2 => true,
            _ => false,
        };
        if glide && self.last_pitch >= 0.0 {
            self.last_pitch
        } else {
            note as f32
        }
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(pitch);
            return;
        }
        self.counter += 1;
        if self.mono {
            self.stack_remove(pitch);
            let legato = self.stack_len > 0 && self.voices[0].amp.is_active() && self.voices[0].held;
            if self.stack_len == self.stack.len() {
                self.stack.copy_within(1.., 0);
                self.stack_len -= 1;
            }
            self.stack[self.stack_len] = pitch;
            self.stack_len += 1;
            if legato {
                // Change pitch without retriggering the envelopes.
                let v = &mut self.voices[0];
                v.note = pitch;
                v.target = pitch as f32;
                if self.glide_mode == 0 {
                    v.pitch = pitch as f32;
                }
            } else {
                let start = if self.glide_mode == 2 {
                    self.start_pitch(pitch, false)
                } else {
                    pitch as f32
                };
                let (free, retrig, counter) = (self.free_lfo, self.retrig, self.counter);
                self.voices[0].start(pitch, velocity, start, counter, free, retrig);
            }
            // Release any poly voices left over from a mode switch.
            for v in self.voices.iter_mut().skip(1) {
                v.release();
            }
            self.last_pitch = pitch as f32;
            return;
        }
        let legato = self.any_held();
        let start = self.start_pitch(pitch, legato);
        let idx = self
            .voices
            .iter()
            .position(|v| !v.amp.is_active())
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.amp.is_releasing())
                    .min_by_key(|(_, v)| v.age)
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
        let (free, retrig, counter) = (self.free_lfo, self.retrig, self.counter);
        self.voices[idx].start(pitch, velocity, start, counter, free, retrig);
        self.last_pitch = pitch as f32;
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
        if self.mono {
            self.stack_remove(pitch);
            let v = &mut self.voices[0];
            if v.held && v.note == pitch {
                if self.stack_len > 0 {
                    let next = self.stack[self.stack_len - 1];
                    v.note = next;
                    v.target = next as f32;
                    if self.glide_mode == 0 {
                        v.pitch = next as f32;
                    }
                    self.last_pitch = next as f32;
                } else {
                    v.release();
                }
            }
            return;
        }
        for v in &mut self.voices {
            if v.held && v.note == pitch {
                v.release();
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        self.stack_len = 0;
        for v in &mut self.voices {
            v.release();
        }
    }

    pub fn panic(&mut self) {
        self.stack_len = 0;
        for v in &mut self.voices {
            v.held = false;
            v.amp.reset();
            v.fenv.reset();
            v.fenv_value = 0.0;
        }
    }

    pub fn render(&mut self, p: &AnalogParams, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        self.apply_modes(p);
        let sr = self.sr;
        let sh = Shared::new(p, sr, self.perf);
        let out_gain = db_to_gain(p.volume_db.clamp(-80.0, 12.0)) * 1.8;
        let (aa, ad, as_, ar) = (
            p.ae_attack.max(0.0005),
            p.ae_decay.max(0.001),
            p.ae_sustain.clamp(0.0, 1.0),
            p.ae_release.max(0.001),
        );
        let (fa, fd, fs, fr) = (
            p.fe_attack.max(0.0005),
            p.fe_decay.max(0.001),
            p.fe_sustain.clamp(0.0, 1.0),
            p.fe_release.max(0.001),
        );
        let ae_vel = p.ae_vel.clamp(0.0, 1.0);
        let stereo = sh.unison > 1;
        for v in self.voices.iter_mut() {
            if !v.amp.is_active() {
                continue;
            }
            let vel_gain = (1.0 - ae_vel * (1.0 - v.velocity)) * out_gain;
            for i in 0..n {
                if v.ctl == 0 {
                    v.control(&sh, sr);
                    v.ctl = CONTROL;
                }
                v.ctl -= 1;
                let a = v.amp.next(aa, ad, as_, ar);
                v.fenv_value = v.fenv.next(fa, fd, fs, fr);
                if !v.amp.is_active() {
                    break;
                }
                // Oscillators (unison copies summed into L/R).
                let mut l = 0.0;
                let mut r = 0.0;
                for u in 0..sh.unison {
                    let (dt1, dt2) = (v.dt1[u], v.dt2[u]);
                    let s1 = osc(sh.o1_wave, v.phase1[u], dt1, v.pw1);
                    let s2 = osc(sh.o2_wave, v.phase2[u], dt2, v.pw2);
                    let s = s1 * sh.o1_level + s2 * sh.o2_level;
                    l += s * sh.uni_left[u];
                    r += s * sh.uni_right[u];
                    v.phase1[u] += dt1;
                    v.phase2[u] += dt2;
                    if v.phase1[u] >= 1.0 {
                        v.phase1[u] -= 1.0;
                        if sh.sync {
                            // Restart osc 2 at the fractional point where osc 1 wrapped.
                            v.phase2[u] = (v.phase1[u] * dt2 / dt1).min(0.999);
                        }
                    }
                    if v.phase2[u] >= 1.0 {
                        v.phase2[u] = wrap01(v.phase2[u]);
                    }
                }
                if sh.noise_level > 0.0 {
                    let w = v.rng.next_bipolar();
                    v.noise_lp += (w - v.noise_lp) * sh.noise_coef;
                    let nz = v.noise_lp * sh.noise_level * (1.4 - 0.6 * sh.noise_coef);
                    l += nz;
                    r += nz;
                }
                let yl = filter_pair(&mut v.f1[0], &mut v.f2[0], l, &v.c1, &v.c2, &sh);
                let yr = if stereo {
                    filter_pair(&mut v.f1[1], &mut v.f2[1], r, &v.c1, &v.c2, &sh)
                } else {
                    yl
                };
                v.amp_mod_smooth += (v.amp_mod - v.amp_mod_smooth) * 0.01;
                let g = a * vel_gain * v.amp_mod_smooth;
                left[i] += sat(yl * 0.8) * g;
                right[i] += sat(yr * 0.8) * g;
            }
        }
        // Free-running LFO phase for voices without retrigger.
        for i in 0..2 {
            self.free_lfo[i] = wrap01(self.free_lfo[i] + sh.lfo_rate[i] * n as f32 / sr);
        }
    }
}

#[inline]
fn filter_pair(f1: &mut FilterState, f2: &mut FilterState, x: f32, c1: &Coef, c2: &Coef, sh: &Shared) -> f32 {
    if !sh.f2_on {
        f1.process(x, c1)
    } else if sh.parallel {
        0.5 * (f1.process(x, c1) + f2.process(x, c2))
    } else {
        let y = f1.process(x, c1);
        f2.process(y, c2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(s: &mut AnalogSynth, p: &AnalogParams, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
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

    fn sine_patch() -> AnalogParams {
        AnalogParams {
            o1_wave: 3.0,
            o1_level: 1.0,
            o2_level: 0.0,
            f1_type: 0.0,
            f1_cutoff: 20000.0,
            f1_env: 0.0,
            f1_key: 0.0,
            f1_res: 0.0,
            f1_drive: 0.0,
            ae_attack: 0.001,
            ae_sustain: 1.0,
            ..AnalogParams::default()
        }
    }

    #[test]
    fn indices_match_labels() {
        let i = AnalogParams::INFO;
        assert_eq!(i[idx::O1_WAVE].name, "Osc1 Wave");
        assert_eq!(i[idx::O1_PW].name, "Osc1 PW");
        assert_eq!(i[idx::O2_WAVE].name, "Osc2 Wave");
        assert_eq!(i[idx::O2_PW].name, "Osc2 PW");
        assert_eq!(i[idx::O2_SYNC].name, "Osc2 Sync");
        assert_eq!(i[idx::NOISE].name, "Noise");
        assert_eq!(i[idx::F1_TYPE].name, "F1 Type");
        assert_eq!(i[idx::F1_ENV].name, "F1 Env");
        assert_eq!(i[idx::F2_ON].name, "F2 On");
        assert_eq!(i[idx::ROUTING].name, "Routing");
        assert_eq!(i[idx::FE_ATTACK].name, "FEnv Attack");
        assert_eq!(i[idx::FE_VEL].name, "FEnv Vel");
        assert_eq!(i[idx::AE_ATTACK].name, "Attack");
        assert_eq!(i[idx::AE_VEL].name, "Amp Vel");
        assert_eq!(i[idx::L1_SHAPE].name, "LFO1 Shape");
        assert_eq!(i[idx::L2_SHAPE].name, "LFO2 Shape");
        assert_eq!(i[idx::L1_SHAPE + idx::LFO_LEN].name, "LFO2 Shape");
        assert_eq!(i[idx::GLIDE_MODE].name, "Glide");
        assert_eq!(i[idx::VOICE_MODE].name, "Voice Mode");
        assert_eq!(i[idx::UNISON].name, "Unison");
        assert_eq!(i[idx::VIB_RATE].name, "Vib Rate");
        assert_eq!(i[idx::VOLUME].name, "Volume");
        assert_eq!(AnalogParams::COUNT, idx::L2_SYNC + 2);
        for inf in AnalogParams::INFO {
            assert!(inf.default >= inf.min && inf.default <= inf.max, "{}", inf.name);
        }
    }

    #[test]
    fn plays_at_the_right_pitch() {
        let mut s = AnalogSynth::new(SR);
        let p = sine_patch();
        s.note_on(69, 100);
        let (l, _) = run(&mut s, &p, SR as usize);
        let c = crossings(&l[4800..]) as f32 / 0.9;
        assert!((c - 440.0).abs() < 4.0, "crossings {c}");
        let pk = peak(&l);
        assert!(pk > 0.15 && pk < 1.0, "peak {pk}");
    }

    #[test]
    fn octave_and_semitone_offsets() {
        let mut s = AnalogSynth::new(SR);
        let p = AnalogParams {
            o1_oct: 1.0,
            o1_semi: -12.0,
            o1_fine: 0.0,
            ..sine_patch()
        };
        s.note_on(57, 100);
        let (l, _) = run(&mut s, &p, SR as usize);
        let c = crossings(&l[4800..]) as f32 / 0.9;
        assert!((c - 220.0).abs() < 3.0, "crossings {c}");
    }

    #[test]
    fn default_patch_peaks_in_range_and_releases() {
        let mut s = AnalogSynth::new(SR);
        let p = AnalogParams::default();
        s.note_on(60, 100);
        let (l, r) = run(&mut s, &p, 24000);
        let pk = peak(&l).max(peak(&r));
        assert!(pk > 0.15 && pk < 0.9, "peak {pk}");
        s.note_off(60);
        run(&mut s, &p, SR as usize * 2);
        assert_eq!(s.active_voices(), 0);
        let (l, _) = run(&mut s, &p, 1000);
        assert_eq!(peak(&l), 0.0);
    }

    #[test]
    fn panic_silences_immediately() {
        let mut s = AnalogSynth::new(SR);
        let p = AnalogParams {
            ae_release: 10.0,
            ..AnalogParams::default()
        };
        for n in [48, 52, 55] {
            s.note_on(n, 100);
        }
        run(&mut s, &p, 2000);
        s.panic();
        let (l, _) = run(&mut s, &p, 512);
        assert_eq!(peak(&l), 0.0);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn lowpass_cutoff_darkens() {
        let measure = |cutoff: f32| {
            let mut s = AnalogSynth::new(SR);
            let p = AnalogParams {
                f1_cutoff: cutoff,
                f1_env: 0.0,
                f1_key: 0.0,
                o2_level: 0.0,
                ..AnalogParams::default()
            };
            s.note_on(48, 100);
            let (l, _) = run(&mut s, &p, 9600);
            // Energy of the first difference approximates high-frequency content.
            let d: Vec<f32> = l[4800..].windows(2).map(|w| w[1] - w[0]).collect();
            rms(&d) / rms(&l[4800..]).max(1e-9)
        };
        for t in 0..5 {
            let mut s = AnalogSynth::new(SR);
            let p = AnalogParams {
                f1_type: t as f32,
                ..AnalogParams::default()
            };
            s.note_on(48, 100);
            let (l, _) = run(&mut s, &p, 4800);
            assert!(rms(&l) > 0.005, "filter type {t} silent");
        }
        let bright = measure(8000.0);
        let dark = measure(300.0);
        assert!(dark < bright * 0.5, "dark {dark} bright {bright}");
    }

    #[test]
    fn velocity_scales_level() {
        let level = |vel: u8| {
            let mut s = AnalogSynth::new(SR);
            let p = AnalogParams {
                ae_vel: 1.0,
                ..sine_patch()
            };
            s.note_on(60, vel);
            let (l, _) = run(&mut s, &p, 4800);
            rms(&l)
        };
        assert!(level(30) < level(127) * 0.4);
    }

    #[test]
    fn mono_mode_uses_one_voice_and_returns_to_held_note() {
        let mut s = AnalogSynth::new(SR);
        let p = AnalogParams {
            voice_mode: 1.0,
            ..sine_patch()
        };
        run(&mut s, &p, 64);
        s.note_on(57, 100);
        s.note_on(69, 100);
        let (l, _) = run(&mut s, &p, 24000);
        assert_eq!(s.active_voices(), 1);
        let c = crossings(&l[4800..]) as f32 / 0.4;
        assert!((c - 440.0).abs() < 8.0, "crossings {c}");
        s.note_off(69);
        let (l, _) = run(&mut s, &p, 24000);
        let c = crossings(&l[4800..]) as f32 / 0.4;
        assert!((c - 220.0).abs() < 6.0, "crossings {c}");
    }

    #[test]
    fn glide_moves_gradually() {
        let mut s = AnalogSynth::new(SR);
        let p = AnalogParams {
            voice_mode: 1.0,
            glide_mode: 2.0,
            glide_time: 0.5,
            ..sine_patch()
        };
        run(&mut s, &p, 64);
        s.note_on(45, 100);
        run(&mut s, &p, 4800);
        s.note_on(69, 100);
        let (l, _) = run(&mut s, &p, 4800);
        // Within the first 100 ms the pitch is still well below 440 Hz.
        let c = crossings(&l) as f32 / 0.1;
        assert!(c < 380.0 && c > 110.0, "crossings {c}");
    }

    #[test]
    fn unison_is_stereo() {
        let mut s = AnalogSynth::new(SR);
        let p = AnalogParams {
            unison: 4.0,
            uni_spread: 1.0,
            uni_detune: 20.0,
            ..AnalogParams::default()
        };
        s.note_on(60, 100);
        let (l, r) = run(&mut s, &p, 9600);
        let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        assert!(rms(&diff) > 0.01);
        assert!(peak(&l) < 1.2);
    }

    #[test]
    fn sync_changes_timbre() {
        let render = |sync: f32| {
            let mut s = AnalogSynth::new(SR);
            let p = AnalogParams {
                o1_level: 0.0,
                o2_level: 1.0,
                o2_semi: 7.0,
                o2_fine: 0.0,
                o2_sync: sync,
                f1_cutoff: 20000.0,
                f1_env: 0.0,
                ..AnalogParams::default()
            };
            s.note_on(48, 100);
            run(&mut s, &p, 4800).0
        };
        let a = render(0.0);
        let b = render(1.0);
        let diff: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
        assert!(rms(&diff) > 0.02);
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let mut lo = AnalogParams::default();
        let mut hi = AnalogParams::default();
        for (i, inf) in AnalogParams::INFO.iter().enumerate() {
            lo.set(i, inf.min);
            hi.set(i, inf.max);
        }
        hi.volume_db = 0.0;
        for p in [lo, hi] {
            for t in 0..5 {
                let p = AnalogParams {
                    f1_type: t as f32,
                    f2_type: (4 - t) as f32,
                    ..p
                };
                let mut s = AnalogSynth::new(SR);
                for n in [0u8, 24, 60, 100, 127] {
                    s.note_on(n, 127);
                }
                let (l, r) = run(&mut s, &p, 9600);
                assert!(l.iter().chain(&r).all(|x| x.is_finite()));
                assert!(peak(&l) < 8.0 && peak(&r) < 8.0, "peak {}", peak(&l));
            }
        }
        // Single-frame renders work too.
        let mut s = AnalogSynth::new(SR);
        s.note_on(60, 100);
        let mut a = [0.0];
        let mut b = [0.0];
        for _ in 0..100 {
            s.render(&AnalogParams::default(), &mut a, &mut b);
        }
        assert!(a[0].is_finite());
    }

    #[test]
    fn voice_pool_is_bounded() {
        let mut s = AnalogSynth::new(SR);
        for n in 0..40 {
            s.note_on(30 + n, 100);
        }
        run(&mut s, &AnalogParams::default(), 64);
        assert_eq!(s.active_voices(), VOICES);
    }

    #[test]
    fn presets_are_valid_and_sound() {
        let presets = presets();
        assert!(presets.len() >= 4);
        for (_, name, p) in presets {
            for (i, inf) in AnalogParams::INFO.iter().enumerate() {
                let v = p.get(i);
                assert!(v >= inf.min && v <= inf.max, "{name}: {}", inf.name);
            }
            let mut s = AnalogSynth::new(SR);
            s.note_on(60, 100);
            let (l, r) = run(&mut s, &p, 48000);
            let pk = peak(&l).max(peak(&r));
            assert!(pk > 0.05 && pk < 1.2, "{name}: peak {pk}");
        }
    }
}
