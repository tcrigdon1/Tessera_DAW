//! Electric Piano: a physically inspired tine / reed electric piano.
//!
//! Each voice models
//! - a **mallet**: a half-sine force pulse whose contact time depends on
//!   stiffness, velocity and key (short contact = brighter excitation), plus a
//!   band-passed noise "thump";
//! - a **tine** (or reed) as three decaying modal resonators with
//!   inharmonic partial ratios and frequency-dependent decay;
//! - a **tone bar**: a slightly detuned, slowly decaying mode coupled to the
//!   tine fundamental;
//! - a **damper** that shortens (and darkens) the decay on note-off and adds
//!   a little release noise;
//! - a **pickup**: an electromagnetic (folding, Rhodes-like) or
//!   electrostatic (asymmetric, Wurli-like) nonlinearity whose symmetry sets
//!   the even/odd harmonic balance and whose distance sets level and drive.
//!
//! A global tremolo / auto-pan finishes the sound.

use super::{midi_to_hz, Rng};
use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};
use crate::model::db_to_gain;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);
const PICKUPS: ParamKind = ParamKind::Choice(&["Magnetic", "Static"]);

/// Voice pool size (the "Voices" parameter limits it further).
pub const VOICES: usize = 24;
const MODES: usize = 4;
/// Render sub-block length (tremolo gains live on the stack per chunk).
const CHUNK: usize = 256;

device_params! {
    pub struct ElectricParams {
        m_stiffness: "Stiffness", 0.0, 1.0, 0.45, LIN, Unit::Percent;
        m_force: "Force", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        m_vel: "Vel Sens", 0.0, 1.0, 0.75, LIN, Unit::Percent;
        m_noise: "Noise", 0.0, 1.0, 0.25, LIN, Unit::Percent;
        m_noise_decay: "Noise Decay", 2.0, 200.0, 18.0, LOG, Unit::Ms;
        t_decay: "Tine Decay", 0.2, 20.0, 3.5, LOG, Unit::Seconds;
        t_color: "Tine Color", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        t_vel_bright: "Vel>Bright", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        t_key: "Key Scale", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        tb_decay: "Bar Decay", 0.2, 20.0, 6.0, LOG, Unit::Seconds;
        tb_level: "Bar Level", 0.0, 1.0, 0.45, LIN, Unit::Percent;
        d_tone: "Damp Tone", 0.0, 1.0, 0.4, LIN, Unit::Percent;
        d_amount: "Damping", 0.0, 1.0, 0.9, LIN, Unit::Percent;
        d_noise: "Rel Noise", 0.0, 1.0, 0.2, LIN, Unit::Percent;
        p_type: "Pickup", 0.0, 1.0, 0.0, PICKUPS, Unit::Plain;
        p_symmetry: "Symmetry", -1.0, 1.0, 0.3, LIN, Unit::Plain;
        p_distance: "Distance", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        volume_db: "Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
        voices: "Voices", 1.0, 24.0, 16.0, LIN, Unit::Integer;
        semitone: "Transpose", -24.0, 24.0, 0.0, LIN, Unit::Semitones;
        fine: "Fine", -50.0, 50.0, 0.0, LIN, Unit::Integer;
        stretch: "Stretch", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        release: "Release", 0.01, 3.0, 0.12, LOG, Unit::Seconds;
        trem_rate: "Trem Rate", 0.1, 12.0, 4.5, LOG, Unit::Hz;
        trem_depth: "Tremolo", 0.0, 1.0, 0.0, LIN, Unit::Percent;
    }
}

/// Parameter indices into [`ElectricParams::INFO`] (for panels).
pub mod idx {
    pub const STIFFNESS: usize = 0;
    pub const NOISE_DECAY: usize = 4;
    pub const T_DECAY: usize = 5;
    pub const T_KEY: usize = 8;
    pub const TB_DECAY: usize = 9;
    pub const TB_LEVEL: usize = 10;
    pub const D_TONE: usize = 11;
    pub const D_NOISE: usize = 13;
    pub const P_TYPE: usize = 14;
    pub const P_DISTANCE: usize = 16;
    pub const VOLUME: usize = 17;
    pub const TREM_DEPTH: usize = 24;
}

/// Automatable parameters, in [`ElectricParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; ElectricParams::COUNT] = specs_from_info(ElectricParams::INFO);

/// Factory presets: (browser category, name, settings).
pub fn presets() -> Vec<(&'static str, &'static str, ElectricParams)> {
    let d = ElectricParams::default();
    vec![
        ("Keys", "Classic Tines", d),
        (
            "Keys",
            "Barky Stage",
            ElectricParams {
                m_stiffness: 0.65,
                m_force: 0.8,
                t_color: 0.6,
                t_vel_bright: 0.8,
                p_symmetry: 0.6,
                p_distance: 0.15,
                tb_level: 0.35,
                volume_db: -9.0,
                ..d
            },
        ),
        (
            "Keys",
            "Reed Piano",
            ElectricParams {
                m_stiffness: 0.55,
                m_noise: 0.35,
                m_noise_decay: 10.0,
                t_decay: 2.2,
                t_color: 0.35,
                tb_level: 0.1,
                p_type: 1.0,
                p_symmetry: 0.5,
                p_distance: 0.3,
                release: 0.08,
                trem_rate: 5.5,
                trem_depth: 0.0,
                volume_db: -7.0,
                ..d
            },
        ),
        (
            "Keys",
            "Suitcase Tremolo",
            ElectricParams {
                m_stiffness: 0.35,
                t_color: 0.4,
                p_symmetry: 0.2,
                p_distance: 0.6,
                trem_rate: 4.2,
                trem_depth: 0.7,
                release: 0.2,
                volume_db: -5.0,
                ..d
            },
        ),
        (
            "Mallets",
            "Soft Bell Tines",
            ElectricParams {
                m_stiffness: 0.9,
                m_force: 0.3,
                m_noise: 0.05,
                t_decay: 8.0,
                t_color: 0.95,
                t_key: 0.3,
                tb_decay: 12.0,
                tb_level: 0.7,
                p_symmetry: 0.0,
                p_distance: 0.85,
                d_amount: 0.6,
                release: 0.6,
                stretch: 0.6,
                volume_db: -3.0,
                ..d
            },
        ),
        (
            "Pad",
            "Dreamy Sustain",
            ElectricParams {
                m_stiffness: 0.25,
                m_noise: 0.1,
                t_decay: 14.0,
                tb_decay: 18.0,
                tb_level: 0.6,
                d_amount: 0.4,
                release: 1.5,
                trem_rate: 1.2,
                trem_depth: 0.4,
                volume_db: -6.0,
                ..d
            },
        ),
    ]
}

/// Partial ratios of the tine (electromagnetic) and reed (electrostatic).
const TINE_RATIOS: [f32; 3] = [1.0, 7.1, 19.6];
const REED_RATIOS: [f32; 3] = [1.0, 6.27, 17.55];
/// The tone bar's frequency relative to the tine (slow beating).
const BAR_DETUNE: f32 = 1.0007;

#[derive(Debug, Clone, Copy, Default)]
struct Mode {
    re: f32,
    im: f32,
    /// Rotation scaled by the per-sample decay.
    c: f32,
    s: f32,
    /// Excitation weight.
    w: f32,
}

#[derive(Debug, Clone)]
struct Voice {
    active: bool,
    held: bool,
    note: u8,
    vel: f32,
    age: u64,
    modes: [Mode; MODES],
    /// Mallet pulse: position, length (samples) and per-sample amplitude.
    pulse_pos: u32,
    pulse_len: u32,
    pulse_amp: f32,
    noise_env: f32,
    noise_coef: f32,
    nf_ic1: f32,
    nf_ic2: f32,
    nf_g: f32,
    dc_x: f32,
    dc_y: f32,
    rng: Rng,
}

impl Voice {
    fn new(seed: u32) -> Self {
        Self {
            active: false,
            held: false,
            note: 0,
            vel: 0.0,
            age: 0,
            modes: [Mode::default(); MODES],
            pulse_pos: 0,
            pulse_len: 0,
            pulse_amp: 0.0,
            noise_env: 0.0,
            noise_coef: 0.0,
            nf_ic1: 0.0,
            nf_ic2: 0.0,
            nf_g: 0.1,
            dc_x: 0.0,
            dc_y: 0.0,
            rng: Rng::new(seed),
        }
    }

    fn energy(&self) -> f32 {
        self.modes.iter().map(|m| (m.re * m.re + m.im * m.im) * m.w * m.w).sum()
    }
}

/// Note-on data remembered until the next render applies it (note_on has
/// no access to the parameters).
#[derive(Debug, Clone, Copy, Default)]
struct Strike {
    pending: bool,
    release: bool,
}

pub struct ElectricPiano {
    sr: f32,
    voices: [Voice; VOICES],
    strikes: [Strike; VOICES],
    counter: u64,
    limit: usize,
    trem_phase: f32,
}

impl ElectricPiano {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate.max(1.0),
            voices: std::array::from_fn(|i| Voice::new(0x5DEE_CE6D ^ (i as u32 * 2_654_435 + 11))),
            strikes: [Strike::default(); VOICES],
            counter: 0,
            limit: ElectricParams::default().voices as usize,
            trem_phase: 0.0,
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        if velocity == 0 {
            self.note_off(pitch);
            return;
        }
        self.counter += 1;
        let limit = self.limit.clamp(1, VOICES);
        // Re-strike a tine that is still ringing (adds energy, no click).
        let idx = self.voices[..limit]
            .iter()
            .position(|v| v.active && v.note == pitch)
            .or_else(|| self.voices[..limit].iter().position(|v| !v.active))
            .unwrap_or_else(|| {
                // Steal the quietest released voice, else the oldest.
                self.voices[..limit]
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| !v.held)
                    .min_by(|a, b| a.1.energy().total_cmp(&b.1.energy()))
                    .or_else(|| self.voices[..limit].iter().enumerate().min_by_key(|(_, v)| v.age))
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        let v = &mut self.voices[idx];
        if v.note != pitch || !v.active {
            v.modes = [Mode::default(); MODES];
            v.nf_ic1 = 0.0;
            v.nf_ic2 = 0.0;
            v.dc_x = 0.0;
            v.dc_y = 0.0;
        }
        v.active = true;
        v.held = true;
        v.note = pitch;
        v.vel = velocity as f32 / 127.0;
        v.age = self.counter;
        self.strikes[idx] = Strike {
            pending: true,
            release: false,
        };
    }

    pub fn note_off(&mut self, pitch: u8) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            if v.held && v.note == pitch {
                v.held = false;
                self.strikes[i].release = true;
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for (i, v) in self.voices.iter_mut().enumerate() {
            if v.held {
                v.held = false;
                self.strikes[i].release = true;
            }
        }
    }

    pub fn panic(&mut self) {
        for (v, s) in self.voices.iter_mut().zip(self.strikes.iter_mut()) {
            v.active = false;
            v.held = false;
            v.modes = [Mode::default(); MODES];
            v.noise_env = 0.0;
            v.pulse_pos = v.pulse_len;
            *s = Strike::default();
        }
    }

    pub fn render(&mut self, p: &ElectricParams, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len());
        let mut start = 0;
        while start < n {
            let end = (start + CHUNK).min(n);
            self.render_chunk(p, &mut left[start..end], &mut right[start..end]);
            start = end;
        }
    }

    fn render_chunk(&mut self, p: &ElectricParams, left: &mut [f32], right: &mut [f32]) {
        let n = left.len().min(right.len()).min(CHUNK);
        let sr = self.sr;
        // Tremolo / auto-pan gains for this chunk.
        let depth = p.trem_depth.clamp(0.0, 1.0);
        let rate = p.trem_rate.clamp(0.01, 50.0);
        let mut trem_l = [1.0f32; CHUNK];
        let mut trem_r = [1.0f32; CHUNK];
        if depth > 0.0 {
            for i in 0..n {
                let ph = self.trem_phase + rate * i as f32 / sr;
                let m = (ph * std::f32::consts::TAU).sin();
                trem_l[i] = 1.0 - depth * 0.5 * (1.0 + m);
                trem_r[i] = 1.0 - depth * 0.5 * (1.0 - m);
            }
        }
        self.trem_phase = (self.trem_phase + rate * n as f32 / sr).fract();
        self.limit = (p.voices.round() as usize).clamp(1, VOICES);
        let gain = db_to_gain(p.volume_db.clamp(-80.0, 12.0)) * 1.25;
        let magnetic = p.p_type < 0.5;
        let ratios = if magnetic { TINE_RATIOS } else { REED_RATIOS };
        let stiff = p.m_stiffness.clamp(0.0, 1.0);
        let force = p.m_force.clamp(0.0, 1.0);
        let color = p.t_color.clamp(0.0, 1.0);
        let vel_bright = p.t_vel_bright.clamp(0.0, 1.0);
        let damp = p.d_amount.clamp(0.0, 1.0);
        let damp_exp = 1.5 * (1.0 - p.d_tone.clamp(0.0, 1.0));
        let release = p.release.clamp(0.005, 10.0);
        let dist = p.p_distance.clamp(0.0, 1.0);
        let k_p = 1.7 - 1.35 * dist;
        let loud = 1.15 - 0.3 * dist;
        let sym = p.p_symmetry.clamp(-1.0, 1.0);
        let noise_dec = (-1.0 / (p.m_noise_decay.clamp(1.0, 500.0) * 0.001 * sr)).exp();
        let stretch = p.stretch.clamp(0.0, 1.0);
        let tune = p.semitone.round().clamp(-48.0, 48.0) + p.fine.clamp(-100.0, 100.0) / 100.0;

        // Pickup transfer: y(x) normalised to unit slope at rest.
        let (s_off, f_rest, slope) = if magnetic {
            let s = sym * 0.5;
            let f = s / (1.0 + s * s);
            let d = (1.0 - s * s) / ((1.0 + s * s) * (1.0 + s * s));
            (s, f, d)
        } else {
            let s = sym * 0.4;
            let f = s / (1.0 - 0.4 * s);
            let d = 1.0 / ((1.0 - 0.4 * s) * (1.0 - 0.4 * s));
            (s, f, d)
        };
        // A distant pickup sees a smaller signal: quieter and cleaner.
        let pick_norm = loud / (k_p.max(0.9) * slope.max(0.3));

        for (vi, v) in self.voices.iter_mut().enumerate() {
            if !v.active {
                continue;
            }
            let strike = &mut self.strikes[vi];
            let note = v.note as f32;
            let key = (note - 60.0) / 12.0;
            let stretch_cents = stretch * 2.0 * key * key.abs();
            let f0 = midi_to_hz(note + tune + stretch_cents / 100.0);
            let vb = vel_bright * v.vel;
            // Decay times (held) per mode.
            let key_fac = (-key * p.t_key.clamp(0.0, 1.0) * 0.8).exp2();
            let tau_t = p.t_decay.clamp(0.05, 60.0) * key_fac;
            let tau_b = p.tb_decay.clamp(0.05, 60.0) * key_fac;
            let freqs = [f0 * ratios[0], f0 * ratios[1], f0 * ratios[2], f0 * BAR_DETUNE];
            let partial_key = (-key.max(-2.0) / 2.0).exp2().min(1.5);
            let weights = [
                1.0,
                (0.06 + 0.45 * color) * (0.4 + vb) * partial_key,
                (0.015 + 0.2 * color) * (0.2 + vb) * partial_key,
                p.tb_level.clamp(0.0, 1.0) * 0.6,
            ];
            for (k, m) in v.modes.iter_mut().enumerate() {
                let f = freqs[k];
                if f >= sr * 0.45 {
                    m.w = 0.0;
                    m.c = 0.0;
                    m.s = 0.0;
                    continue;
                }
                let ratio = f / f0;
                let tau_held = if k == 3 { tau_b } else { tau_t * ratio.powf(-0.7) };
                let mut inv = 1.0 / tau_held;
                if !v.held {
                    let tau_damp = release * ratio.powf(-damp_exp);
                    inv = (1.0 - damp) * inv + damp / tau_damp;
                }
                let r = (-inv / sr).exp();
                let w = std::f32::consts::TAU * f / sr;
                m.c = r * w.cos();
                m.s = r * w.sin();
                m.w = weights[k];
            }
            if strike.pending {
                strike.pending = false;
                let vel_amt = 1.0 - p.m_vel.clamp(0.0, 1.0) * (1.0 - v.vel.powf(1.4));
                let amp = vel_amt * (0.45 + 0.75 * force);
                let tc = (0.0032 - 0.0027 * stiff) * (1.0 - 0.5 * vb) * (1.0 - 0.25 * force) * (-key / 3.0).exp2();
                let tc = tc.min(0.4 / f0);
                let len = (tc * sr).max(2.0);
                v.pulse_len = len as u32;
                v.pulse_pos = 0;
                v.pulse_amp = amp * std::f32::consts::PI / (2.0 * len);
                v.noise_env = p.m_noise.clamp(0.0, 1.0) * 0.5 * vel_amt;
                v.noise_coef = noise_dec;
                let fc = ((900.0 + 2600.0 * stiff) * (key / 2.0).exp2()).min(sr * 0.4);
                v.nf_g = (std::f32::consts::PI * fc / sr).tan();
            }
            if strike.release {
                strike.release = false;
                let rn = p.d_noise.clamp(0.0, 1.0) * 0.06 * (0.3 + 0.7 * v.vel);
                if rn > v.noise_env {
                    v.noise_env = rn;
                }
                v.noise_coef = (-1.0 / (0.03 * sr)).exp();
                v.nf_g = (std::f32::consts::PI * 350.0 / sr).tan();
            }
            // Noise band-pass coefficients (Q about 1.2).
            let nk = 0.8;
            let na1 = 1.0 / (1.0 + v.nf_g * (v.nf_g + nk));
            let na2 = v.nf_g * na1;
            let na3 = v.nf_g * na2;

            for i in 0..n {
                // Mallet force.
                let mut force_in = 0.0;
                if v.pulse_pos < v.pulse_len {
                    let t = (v.pulse_pos as f32 + 0.5) / v.pulse_len as f32;
                    force_in = v.pulse_amp * (std::f32::consts::PI * t).sin();
                    v.pulse_pos += 1;
                }
                let mut x = 0.0;
                for m in v.modes.iter_mut() {
                    let re = m.re * m.c - m.im * m.s + force_in * m.w;
                    let im = m.re * m.s + m.im * m.c;
                    m.re = re;
                    m.im = im;
                    x += im;
                }
                // Pickup.
                let y = if magnetic {
                    let u = (x * k_p + s_off).clamp(-8.0, 8.0);
                    u / (1.0 + u * u) - f_rest
                } else {
                    let u = (x * k_p + s_off).clamp(-1.6, 1.6);
                    let g = u / (1.0 - 0.4 * u) - f_rest;
                    super::analog::sat(g * 0.8) * 1.25
                };
                let y = y * pick_norm;
                // DC blocker (asymmetric pickups produce an offset).
                let hp = y - v.dc_x + 0.9995 * v.dc_y;
                v.dc_x = y;
                v.dc_y = hp;
                // Mechanical noise.
                let mut out = hp;
                if v.noise_env > 1e-5 {
                    let w = v.rng.next_bipolar() * v.noise_env;
                    v.noise_env *= v.noise_coef;
                    let v3 = w - v.nf_ic2;
                    let v1 = na1 * v.nf_ic1 + na2 * v3;
                    let v2 = v.nf_ic2 + na2 * v.nf_ic1 + na3 * v3;
                    v.nf_ic1 = 2.0 * v1 - v.nf_ic1;
                    v.nf_ic2 = 2.0 * v2 - v.nf_ic2;
                    out += v1 * nk;
                }
                let s = out * gain;
                left[i] += s * trem_l[i];
                right[i] += s * trem_r[i];
            }
            // Free the voice once it has decayed.
            if v.pulse_pos >= v.pulse_len && v.noise_env <= 1e-5 && v.energy() < 1e-9 && v.dc_y.abs() < 1e-5 {
                v.active = false;
                v.held = false;
            }
            // Flush denormal-range state.
            for m in v.modes.iter_mut() {
                if m.re.abs() < 1e-25 {
                    m.re = 0.0;
                }
                if m.im.abs() < 1e-25 {
                    m.im = 0.0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(s: &mut ElectricPiano, p: &ElectricParams, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (cl, cr) in l.chunks_mut(128).zip(r.chunks_mut(128)) {
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

    /// Magnitude of the `freq` component (Goertzel).
    fn tone(x: &[f32], freq: f32) -> f32 {
        let w = std::f32::consts::TAU * freq / SR;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s0 = v + c * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - c * s1 * s2).max(0.0).sqrt() / x.len() as f32
    }

    #[test]
    fn indices_match_labels() {
        let i = ElectricParams::INFO;
        assert_eq!(i[idx::STIFFNESS].name, "Stiffness");
        assert_eq!(i[idx::NOISE_DECAY].name, "Noise Decay");
        assert_eq!(i[idx::T_DECAY].name, "Tine Decay");
        assert_eq!(i[idx::T_KEY].name, "Key Scale");
        assert_eq!(i[idx::TB_DECAY].name, "Bar Decay");
        assert_eq!(i[idx::TB_LEVEL].name, "Bar Level");
        assert_eq!(i[idx::D_TONE].name, "Damp Tone");
        assert_eq!(i[idx::D_NOISE].name, "Rel Noise");
        assert_eq!(i[idx::P_TYPE].name, "Pickup");
        assert_eq!(i[idx::P_DISTANCE].name, "Distance");
        assert_eq!(i[idx::VOLUME].name, "Volume");
        assert_eq!(i[idx::TREM_DEPTH].name, "Tremolo");
        assert_eq!(ElectricParams::COUNT, idx::TREM_DEPTH + 1);
    }

    #[test]
    fn plays_at_the_right_pitch_with_harmonics() {
        for p_type in [0.0, 1.0] {
            let mut s = ElectricPiano::new(SR);
            let p = ElectricParams {
                p_type,
                stretch: 0.0,
                ..ElectricParams::default()
            };
            s.note_on(57, 100); // 220 Hz
            let (l, _) = run(&mut s, &p, 24000);
            let x = &l[2400..21600];
            let f = tone(x, 220.0);
            assert!(f > tone(x, 207.0) * 5.0 && f > tone(x, 233.0) * 5.0, "type {p_type}");
            // Not a sine: the pickup adds clear 2nd and 3rd harmonics.
            let h2 = tone(x, 440.0);
            let h3 = tone(x, 660.0);
            assert!(h2 > f * 0.02, "type {p_type}: weak 2nd harmonic {h2} vs {f}");
            assert!(h2 + h3 > f * 0.05, "type {p_type}: too pure");
            let pk = peak(&l);
            assert!(pk > 0.1 && pk < 0.9, "type {p_type}: peak {pk}");
        }
    }

    #[test]
    fn symmetric_pickup_suppresses_even_harmonics() {
        let h2_ratio = |sym: f32| {
            let mut s = ElectricPiano::new(SR);
            let p = ElectricParams {
                p_symmetry: sym,
                p_distance: 0.1,
                tb_level: 0.0,
                t_color: 0.0,
                stretch: 0.0,
                m_noise: 0.0,
                ..ElectricParams::default()
            };
            s.note_on(57, 120);
            let (l, _) = run(&mut s, &p, 24000);
            let x = &l[2400..21600];
            tone(x, 440.0) / tone(x, 220.0)
        };
        assert!(h2_ratio(0.0) < 0.02, "{}", h2_ratio(0.0));
        assert!(h2_ratio(0.8) > h2_ratio(0.0) * 5.0);
    }

    #[test]
    fn velocity_brightens_and_louds() {
        let measure = |vel: u8| {
            let mut s = ElectricPiano::new(SR);
            let p = ElectricParams {
                m_noise: 0.0,
                ..ElectricParams::default()
            };
            s.note_on(60, vel);
            let (l, _) = run(&mut s, &p, 9600);
            let x = &l[480..];
            let d: Vec<f32> = x.windows(2).map(|w| w[1] - w[0]).collect();
            (rms(x), rms(&d) / rms(x).max(1e-9))
        };
        let (soft_level, soft_bright) = measure(30);
        let (hard_level, hard_bright) = measure(127);
        assert!(hard_level > soft_level * 2.0, "{soft_level} {hard_level}");
        assert!(hard_bright > soft_bright, "{soft_bright} {hard_bright}");
    }

    #[test]
    fn damper_stops_the_note() {
        let mut s = ElectricPiano::new(SR);
        let p = ElectricParams::default();
        s.note_on(60, 100);
        run(&mut s, &p, 4800);
        s.note_off(60);
        run(&mut s, &p, SR as usize * 2);
        assert_eq!(s.active_voices(), 0);
        let (l, _) = run(&mut s, &p, 512);
        assert_eq!(peak(&l), 0.0);

        // Without damping it keeps ringing much longer.
        let mut s = ElectricPiano::new(SR);
        let p = ElectricParams {
            d_amount: 0.0,
            ..ElectricParams::default()
        };
        s.note_on(60, 100);
        run(&mut s, &p, 4800);
        s.note_off(60);
        run(&mut s, &p, SR as usize / 2);
        let (l, _) = run(&mut s, &p, 4800);
        assert!(rms(&l) > 0.005, "{}", rms(&l));
    }

    #[test]
    fn notes_decay_while_held() {
        let mut s = ElectricPiano::new(SR);
        let p = ElectricParams {
            t_decay: 0.5,
            tb_decay: 0.5,
            ..ElectricParams::default()
        };
        s.note_on(72, 100);
        let (a, _) = run(&mut s, &p, 4800);
        run(&mut s, &p, 48000);
        let (b, _) = run(&mut s, &p, 4800);
        assert!(rms(&b) < rms(&a) * 0.1);
    }

    #[test]
    fn panic_and_voice_limit() {
        let mut s = ElectricPiano::new(SR);
        let p = ElectricParams {
            voices: 4.0,
            ..ElectricParams::default()
        };
        run(&mut s, &p, 32);
        for n in 40..60 {
            s.note_on(n, 100);
        }
        run(&mut s, &p, 256);
        assert_eq!(s.active_voices(), 4);
        s.panic();
        let (l, _) = run(&mut s, &p, 256);
        assert_eq!(peak(&l), 0.0);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn tremolo_pans() {
        let mut s = ElectricPiano::new(SR);
        let p = ElectricParams {
            trem_depth: 1.0,
            trem_rate: 5.0,
            t_decay: 10.0,
            ..ElectricParams::default()
        };
        s.note_on(60, 100);
        let (l, r) = run(&mut s, &p, 24000);
        let diff: Vec<f32> = l.iter().zip(&r).map(|(a, b)| a - b).collect();
        assert!(rms(&diff) > 0.01);
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let mut lo = ElectricParams::default();
        let mut hi = ElectricParams::default();
        for (i, inf) in ElectricParams::INFO.iter().enumerate() {
            lo.set(i, inf.min);
            hi.set(i, inf.max);
        }
        hi.volume_db = 0.0;
        for p in [lo, hi] {
            for t in [0.0, 1.0] {
                let p = ElectricParams { p_type: t, ..p };
                let mut s = ElectricPiano::new(SR);
                for n in [0u8, 21, 60, 108, 127] {
                    s.note_on(n, 127);
                }
                let (l, r) = run(&mut s, &p, 9600);
                for n in [0u8, 21, 60, 108, 127] {
                    s.note_off(n);
                }
                let (l2, _) = run(&mut s, &p, 4800);
                assert!(l.iter().chain(&r).chain(&l2).all(|x| x.is_finite()));
                assert!(peak(&l) < 8.0, "peak {}", peak(&l));
            }
        }
    }

    #[test]
    fn presets_are_valid_and_sound() {
        let presets = presets();
        assert!(presets.len() >= 4);
        for (_, name, p) in presets {
            for (i, inf) in ElectricParams::INFO.iter().enumerate() {
                let v = p.get(i);
                assert!(v >= inf.min && v <= inf.max, "{name}: {}", inf.name);
            }
            let mut s = ElectricPiano::new(SR);
            s.note_on(60, 100);
            let (l, r) = run(&mut s, &p, 24000);
            let pk = peak(&l).max(peak(&r));
            assert!(pk > 0.05 && pk < 1.2, "{name}: peak {pk}");
        }
    }
}
