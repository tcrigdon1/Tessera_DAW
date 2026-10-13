//! "Poly Synth": a two-oscillator subtractive polyphonic synthesizer.

use serde::{Deserialize, Serialize};

use super::envelope::Adsr;
use super::filter::Svf;
use super::midi_to_hz;
use crate::model::db_to_gain;

pub const VOICES: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Waveform {
    Sine,
    Triangle,
    Saw,
    Square,
}

impl Waveform {
    pub const ALL: [Waveform; 4] = [Waveform::Sine, Waveform::Triangle, Waveform::Saw, Waveform::Square];
    pub fn label(self) -> &'static str {
        match self {
            Waveform::Sine => "Sine",
            Waveform::Triangle => "Tri",
            Waveform::Saw => "Saw",
            Waveform::Square => "Square",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SynthParams {
    pub osc1: Waveform,
    pub osc2: Waveform,
    /// Osc 2 transpose in semitones.
    pub osc2_semitones: f32,
    /// Osc 2 fine detune in cents.
    pub osc2_detune: f32,
    /// 0 = only osc 1, 1 = only osc 2.
    pub osc_mix: f32,
    pub cutoff: f32,
    pub resonance: f32,
    /// Filter envelope depth in octaves.
    pub filter_env: f32,
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    pub f_attack: f32,
    pub f_decay: f32,
    pub f_sustain: f32,
    pub f_release: f32,
    pub volume_db: f32,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self {
            osc1: Waveform::Saw,
            osc2: Waveform::Square,
            osc2_semitones: 0.0,
            osc2_detune: 7.0,
            osc_mix: 0.4,
            cutoff: 1800.0,
            resonance: 0.9,
            filter_env: 2.0,
            attack: 0.005,
            decay: 0.3,
            sustain: 0.7,
            release: 0.25,
            f_attack: 0.002,
            f_decay: 0.35,
            f_sustain: 0.2,
            f_release: 0.3,
            volume_db: -10.0,
        }
    }
}

impl SynthParams {
    pub fn bass() -> Self {
        Self {
            osc1: Waveform::Saw,
            osc2: Waveform::Square,
            osc2_semitones: -12.0,
            osc2_detune: 0.0,
            osc_mix: 0.5,
            cutoff: 500.0,
            resonance: 1.4,
            filter_env: 3.0,
            attack: 0.002,
            decay: 0.2,
            sustain: 0.6,
            release: 0.08,
            f_attack: 0.001,
            f_decay: 0.18,
            f_sustain: 0.0,
            f_release: 0.1,
            volume_db: -8.0,
        }
    }

    pub fn pad() -> Self {
        Self {
            osc1: Waveform::Saw,
            osc2: Waveform::Saw,
            osc2_semitones: 0.0,
            osc2_detune: 12.0,
            osc_mix: 0.5,
            cutoff: 1200.0,
            resonance: 0.8,
            filter_env: 1.0,
            attack: 0.4,
            decay: 0.8,
            sustain: 0.8,
            release: 1.2,
            f_attack: 0.8,
            f_decay: 1.0,
            f_sustain: 0.5,
            f_release: 1.0,
            volume_db: -16.0,
        }
    }
}

#[derive(Debug, Clone)]
struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    age: u64,
    phase1: f32,
    phase2: f32,
    amp_env: Adsr,
    filt_env: Adsr,
    filter: Svf,
    tri1: f32,
    tri2: f32,
}

impl Voice {
    fn new(sr: f32) -> Self {
        Self {
            note: 0,
            velocity: 0.0,
            held: false,
            age: 0,
            phase1: 0.0,
            phase2: 0.0,
            amp_env: Adsr::new(sr),
            filt_env: Adsr::new(sr),
            filter: Svf::default(),
            tri1: 0.0,
            tri2: 0.0,
        }
    }
}

pub struct PolySynth {
    sample_rate: f32,
    voices: [Voice; VOICES],
    counter: u64,
}

/// PolyBLEP residual for band-limited discontinuities.
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let t = t / dt;
        t + t - t * t - 1.0
    } else if t > 1.0 - dt {
        let t = (t - 1.0) / dt;
        t * t + t + t + 1.0
    } else {
        0.0
    }
}

#[inline]
fn osc(wave: Waveform, phase: f32, dt: f32, tri_state: &mut f32) -> f32 {
    match wave {
        Waveform::Sine => (phase * std::f32::consts::TAU).sin(),
        Waveform::Saw => 2.0 * phase - 1.0 - poly_blep(phase, dt),
        Waveform::Square => {
            let mut v = if phase < 0.5 { 1.0 } else { -1.0 };
            v += poly_blep(phase, dt);
            v -= poly_blep((phase + 0.5) % 1.0, dt);
            v
        }
        Waveform::Triangle => {
            // Leaky-integrated band-limited square.
            let mut sq = if phase < 0.5 { 1.0 } else { -1.0 };
            sq += poly_blep(phase, dt);
            sq -= poly_blep((phase + 0.5) % 1.0, dt);
            *tri_state = dt * 4.0 * sq + (1.0 - dt * 0.5) * *tri_state;
            *tri_state
        }
    }
}

impl PolySynth {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            voices: std::array::from_fn(|_| Voice::new(sample_rate)),
            counter: 0,
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.amp_env.is_active()).count()
    }

    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.counter += 1;
        // Prefer a free voice, then the oldest releasing voice, then the oldest.
        let idx = self
            .voices
            .iter()
            .position(|v| !v.amp_env.is_active())
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.amp_env.is_releasing())
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
        let v = &mut self.voices[idx];
        let was_active = v.amp_env.is_active();
        v.note = note;
        v.velocity = velocity as f32 / 127.0;
        v.held = true;
        v.age = self.counter;
        if !was_active {
            v.phase1 = 0.0;
            v.phase2 = 0.0;
            v.filter.reset();
            v.tri1 = 0.0;
            v.tri2 = 0.0;
        }
        v.amp_env.trigger();
        v.filt_env.trigger();
    }

    pub fn note_off(&mut self, note: u8) {
        for v in &mut self.voices {
            if v.held && v.note == note {
                v.held = false;
                v.amp_env.release();
                v.filt_env.release();
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.amp_env.release();
            v.filt_env.release();
        }
    }

    pub fn panic(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.amp_env.reset();
            v.filt_env.reset();
        }
    }

    pub fn render(&mut self, p: &SynthParams, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let out_gain = db_to_gain(p.volume_db);
        let osc2_ratio = 2f32.powf((p.osc2_semitones + p.osc2_detune / 100.0) / 12.0);
        let q = 0.5 + p.resonance.max(0.0) * 2.0;
        for v in self.voices.iter_mut() {
            if !v.amp_env.is_active() {
                continue;
            }
            let f1 = midi_to_hz(v.note as f32);
            let f2 = f1 * osc2_ratio;
            let dt1 = (f1 / sr).min(0.5);
            let dt2 = (f2 / sr).min(0.5);
            let vel_gain = 0.3 + 0.7 * v.velocity;
            for i in 0..left.len() {
                let a = v.amp_env.next(p.attack, p.decay, p.sustain, p.release);
                if !v.amp_env.is_active() {
                    break;
                }
                let fe = v.filt_env.next(p.f_attack, p.f_decay, p.f_sustain, p.f_release);
                let s1 = osc(p.osc1, v.phase1, dt1, &mut v.tri1);
                let s2 = osc(p.osc2, v.phase2, dt2, &mut v.tri2);
                v.phase1 += dt1;
                if v.phase1 >= 1.0 {
                    v.phase1 -= 1.0;
                }
                v.phase2 += dt2;
                if v.phase2 >= 1.0 {
                    v.phase2 -= 1.0;
                }
                let mixed = s1 * (1.0 - p.osc_mix) + s2 * p.osc_mix;
                let cutoff = p.cutoff * 2f32.powf(p.filter_env * fe);
                let y = v.filter.process(mixed, cutoff, q, sr).low;
                let s = y * a * vel_gain * out_gain;
                left[i] += s;
                right[i] += s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synth_produces_sound_and_releases() {
        let mut s = PolySynth::new(48000.0);
        let p = SynthParams::default();
        let mut l = vec![0.0; 4800];
        let mut r = vec![0.0; 4800];
        s.note_on(60, 100);
        s.render(&p, &mut l, &mut r);
        let peak = l.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak > 0.05, "peak {peak}");
        assert!(peak < 2.0);
        s.note_off(60);
        for _ in 0..20 {
            l.fill(0.0);
            r.fill(0.0);
            s.render(&p, &mut l, &mut r);
        }
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn voice_stealing_never_exceeds_pool() {
        let mut s = PolySynth::new(48000.0);
        for n in 0..40 {
            s.note_on(40 + n, 100);
        }
        assert_eq!(s.active_voices(), VOICES);
    }
}
