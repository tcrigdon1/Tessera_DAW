//! "Granular Synth": plays short, overlapping grains of the track's sample.
//!
//! Each held note runs its own grain cloud: a scheduler starts grains at the
//! set density, each grain reads a window of the sample (size in ms, shaped
//! by a Hann, triangle or faded-rectangle window) from around the current
//! scan position, with optional random position jitter (spray), random
//! pitch, reverse probability and stereo spread. Modes:
//!
//! * **Classic** – grains are pitched by the note (relative to C3 = MIDI 60
//!   when key tracking is on); the scan head starts at Position on note-on
//!   and moves through the file at Scan Speed (×realtime), stopping at the
//!   ends.
//! * **Loop** – like Classic, but the scan head loops inside
//!   `Position .. Position + Loop Len`.
//! * **Cloud** – grains start at random (Poisson) times instead of a steady
//!   rate, with doubled spray and fully random panning, for dense textures.
//!
//! After the grains come a multimode filter (with the amp envelope as its
//! modulation source), the amp ADSR and the volume. A global LFO modulates
//! the scan position, the grain size and the pitch.
//!
//! The sample comes from the track's slot 0 (like the Sampler); without one
//! the synth stays silent. All voices and grains are preallocated.

use super::envelope::Adsr;
use super::filter::Svf;
use super::sample::{self, SampleSlots};
use super::{gain_smooth_coef, lfo_hz, ExprGain, Performance, Rng, SYNC_RATES, SYNC_RATE_DEFAULT};
use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};
use crate::model::{db_to_gain, pan_gains, AudioData};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const MODES: &[&str] = &["Classic", "Loop", "Cloud"];
pub const SHAPES: &[&str] = &["Hann", "Triangle", "Rect"];
pub const FILTER_TYPES: &[&str] = &["Off", "LP", "BP", "HP"];
pub const LFO_SHAPES: &[&str] = &["Sine", "Tri", "Saw", "Square", "S&H"];

/// Voices (notes) that sound at once.
pub const VOICES: usize = 8;
/// Most grains one voice plays at once.
pub const GRAINS: usize = 32;
/// The note at which grains play at the sample's own pitch.
pub const ROOT_NOTE: u8 = 60;
const CONTROL: usize = 32;

device_params! {
    pub struct GranularParams {
        mode: "Mode", 0.0, 2.0, 0.0, ParamKind::Choice(MODES), Unit::Plain;
        position: "Position", 0.0, 1.0, 0.25, LIN, Unit::Percent;
        scan: "Scan Speed", -2.0, 2.0, 0.0, LIN, Unit::Plain;
        loop_len: "Loop Len", 0.01, 1.0, 0.25, LIN, Unit::Percent;
        spray: "Spray", 0.0, 1000.0, 15.0, LIN, Unit::Ms;
        size: "Grain Size", 5.0, 1000.0, 80.0, LOG, Unit::Ms;
        density: "Density", 1.0, 200.0, 24.0, LOG, Unit::Hz;
        shape: "Shape", 0.0, 2.0, 0.0, ParamKind::Choice(SHAPES), Unit::Plain;
        transpose: "Transpose", -48.0, 48.0, 0.0, LIN, Unit::Semitones;
        fine: "Fine", -100.0, 100.0, 0.0, LIN, Unit::Integer;
        key_track: "Key Track", 0.0, 1.0, 1.0, ParamKind::Toggle, Unit::Plain;
        rand_pitch: "Rand Pitch", 0.0, 24.0, 0.0, LIN, Unit::Plain;
        spread: "Spread", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        reverse: "Reverse", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        filter_type: "Filter", 0.0, 3.0, 0.0, ParamKind::Choice(FILTER_TYPES), Unit::Plain;
        cutoff: "Cutoff", 30.0, 20000.0, 8000.0, LOG, Unit::Hz;
        resonance: "Res", 0.0, 1.0, 0.2, LIN, Unit::Percent;
        filter_env: "Filter Env", -6.0, 6.0, 0.0, LIN, Unit::Plain;
        attack: "Attack", 0.5, 10000.0, 10.0, LOG, Unit::Ms;
        decay: "Decay", 1.0, 20000.0, 500.0, LOG, Unit::Ms;
        sustain: "Sustain", 0.0, 1.0, 0.8, LIN, Unit::Percent;
        release: "Release", 1.0, 20000.0, 400.0, LOG, Unit::Ms;
        velocity: "Velocity", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        lfo_shape: "LFO Shape", 0.0, 4.0, 0.0, ParamKind::Choice(LFO_SHAPES), Unit::Plain;
        lfo_rate: "LFO Rate", 0.05, 20.0, 0.5, LOG, Unit::Hz;
        lfo_pos: "LFO>Pos", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        lfo_size: "LFO>Size", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        lfo_pitch: "LFO>Pitch", -12.0, 12.0, 0.0, LIN, Unit::Plain;
        volume_db: "Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
        lfo_sync: "LFO Sync", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        lfo_sync_rate: "LFO Sync Rate", 0.0, 10.0, SYNC_RATE_DEFAULT, SYNC_RATES, Unit::Plain;
    }
}

/// Index of the LFO rate and of the LFO sync toggle (its synced rate
/// follows it).
pub const LFO_RATE: usize = 24;
pub const LFO_SYNC: usize = 29;

/// Automatable parameters, in [`GranularParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; GranularParams::COUNT] = specs_from_info(GranularParams::INFO);

/// Factory presets: (browser category, name, settings). They work on
/// whatever sample is loaded.
pub fn presets() -> Vec<(&'static str, &'static str, GranularParams)> {
    let d = GranularParams::default();
    vec![
        (
            "Textures",
            "Grain Cloud",
            GranularParams {
                mode: 2.0,
                position: 0.4,
                spray: 250.0,
                size: 120.0,
                density: 60.0,
                rand_pitch: 0.3,
                spread: 0.9,
                reverse: 0.3,
                filter_type: 1.0,
                cutoff: 6000.0,
                attack: 400.0,
                release: 1500.0,
                lfo_rate: 0.15,
                lfo_pos: 0.3,
                ..d
            },
        ),
        (
            "Textures",
            "Frozen Moment",
            GranularParams {
                mode: 0.0,
                position: 0.5,
                spray: 30.0,
                size: 200.0,
                density: 30.0,
                spread: 0.6,
                attack: 200.0,
                release: 1200.0,
                ..d
            },
        ),
        (
            "Textures",
            "Glitch Stutter",
            GranularParams {
                mode: 1.0,
                position: 0.2,
                loop_len: 0.05,
                scan: 1.0,
                spray: 0.0,
                size: 25.0,
                density: 18.0,
                shape: 2.0,
                reverse: 0.25,
                spread: 0.5,
                release: 120.0,
                ..d
            },
        ),
        (
            "Pad",
            "Granular Pad",
            GranularParams {
                mode: 2.0,
                position: 0.3,
                scan: 0.05,
                spray: 120.0,
                size: 300.0,
                density: 40.0,
                rand_pitch: 0.1,
                spread: 0.8,
                filter_type: 1.0,
                cutoff: 3500.0,
                filter_env: 1.0,
                attack: 900.0,
                decay: 2000.0,
                sustain: 0.7,
                release: 2500.0,
                lfo_rate: 0.2,
                lfo_size: 0.3,
                ..d
            },
        ),
        (
            "Pad",
            "Octave Shimmer",
            GranularParams {
                mode: 2.0,
                position: 0.35,
                spray: 80.0,
                size: 180.0,
                density: 50.0,
                transpose: 12.0,
                rand_pitch: 0.15,
                spread: 1.0,
                filter_type: 3.0,
                cutoff: 400.0,
                attack: 1200.0,
                release: 3000.0,
                lfo_rate: 4.0,
                lfo_pitch: 0.1,
                volume_db: -9.0,
                ..d
            },
        ),
    ]
}

#[derive(Clone, Copy)]
struct Grain {
    active: bool,
    /// Read position in source frames.
    pos: f64,
    /// Source frames per output sample (negative = reversed).
    step: f64,
    len: u32,
    age: u32,
    shape: u8,
    /// Hann window recursion state: cos oscillator.
    c1: f32,
    c2: f32,
    k: f32,
    gl: f32,
    gr: f32,
}

impl Grain {
    const OFF: Grain = Grain {
        active: false,
        pos: 0.0,
        step: 1.0,
        len: 1,
        age: 0,
        shape: 0,
        c1: 0.0,
        c2: 0.0,
        k: 0.0,
        gl: 0.0,
        gr: 0.0,
    };

    #[inline]
    fn window(&mut self) -> f32 {
        let t = self.age as f32 / self.len as f32;
        match self.shape {
            1 => 1.0 - (2.0 * t - 1.0).abs(),
            2 => (t * 10.0).min((1.0 - t) * 10.0).clamp(0.0, 1.0),
            _ => {
                // Hann = 0.5 - 0.5 cos(2πt), the cosine from a recursive
                // oscillator.
                let c = self.c1;
                let next = self.k * self.c1 - self.c2;
                self.c2 = self.c1;
                self.c1 = next;
                (0.5 - 0.5 * c).max(0.0)
            }
        }
    }
}

struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    age: u64,
    env: Adsr,
    filter_l: Svf,
    filter_r: Svf,
    /// Scan offset from Position, as a fraction of the file.
    offset: f64,
    /// Samples until the next grain starts.
    countdown: f32,
    grains: [Grain; GRAINS],
    /// Track plus note-expression bend in semitones, used by new grains.
    bend: f32,
    expr_gain: ExprGain,
}

impl Voice {
    fn new(sr: f32) -> Self {
        Self {
            note: ROOT_NOTE,
            velocity: 0.0,
            held: false,
            age: 0,
            env: Adsr::new(sr),
            filter_l: Svf::default(),
            filter_r: Svf::default(),
            offset: 0.0,
            countdown: 0.0,
            grains: [Grain::OFF; GRAINS],
            bend: 0.0,
            expr_gain: ExprGain::default(),
        }
    }

    fn active(&self) -> bool {
        self.env.is_active()
    }

    fn grain_count(&self) -> usize {
        self.grains.iter().filter(|g| g.active).count()
    }
}

pub struct GranularSynth {
    sample_rate: f32,
    pub perf: Performance,
    voices: Vec<Voice>,
    counter: u64,
    rng: Rng,
    lfo_phase: f32,
    lfo_hold: f32,
}

/// Uniform random number in 0..1.
#[inline]
fn unit(rng: &mut Rng) -> f32 {
    rng.next_bipolar() * 0.5 + 0.5
}

#[inline]
fn soft_clip(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    x / (1.0 + 0.25 * x * x).sqrt()
}

/// Per-block settings shared by every voice.
struct Block<'a> {
    data: &'a AudioData,
    mode: usize,
    head_base: f32,
    loop_len: f32,
    scan_step: f64,
    spray_frames: f32,
    grain_len: f32,
    interval: f32,
    grain_gain: f32,
    shape: u8,
    pitch: f32,
    key_track: bool,
    rand_pitch: f32,
    rate: f64,
    spread: f32,
    reverse: f32,
}

impl GranularSynth {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1000.0);
        Self {
            perf: Performance::default(),
            sample_rate: sr,
            voices: (0..VOICES).map(|_| Voice::new(sr)).collect(),
            counter: 0,
            rng: Rng::new(0x6a41_7e11),
            lfo_phase: 0.0,
            lfo_hold: 0.0,
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    /// Grains currently playing, over all voices.
    pub fn active_grains(&self) -> usize {
        self.voices.iter().map(|v| v.grain_count()).sum()
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        self.counter += 1;
        let idx = self.voices.iter().position(|v| !v.active()).unwrap_or_else(|| {
            let released = self
                .voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.env.is_releasing())
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i);
            released.unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            })
        });
        let v = &mut self.voices[idx];
        let fresh = !v.active();
        v.note = pitch;
        v.velocity = velocity as f32 / 127.0;
        v.held = true;
        v.age = self.counter;
        v.offset = 0.0;
        v.countdown = 0.0;
        if fresh {
            v.filter_l.reset();
            v.filter_r.reset();
            for g in &mut v.grains {
                g.active = false;
            }
            v.expr_gain.reset();
        }
        v.env.trigger();
    }

    pub fn note_off(&mut self, pitch: u8) {
        for v in &mut self.voices {
            if v.held && v.note == pitch {
                v.held = false;
                v.env.release();
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.env.release();
        }
    }

    pub fn panic(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.env.reset();
            for g in &mut v.grains {
                g.active = false;
            }
        }
    }

    fn lfo_value(&self, shape: usize) -> f32 {
        let ph = self.lfo_phase;
        match shape {
            1 => 1.0 - 4.0 * (ph - 0.5).abs(),
            2 => 2.0 * ph - 1.0,
            3 => {
                if ph < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            4 => self.lfo_hold,
            _ => (ph * std::f32::consts::TAU).sin(),
        }
    }

    pub fn render(&mut self, p: &GranularParams, samples: &SampleSlots, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let n = left.len().min(right.len());
        let (att, dec, sus, rel) = (
            p.attack.max(0.1) / 1000.0,
            p.decay.max(0.5) / 1000.0,
            p.sustain.clamp(0.0, 1.0),
            p.release.max(0.5) / 1000.0,
        );
        let Some(data) = sample::slot(samples, 0) else {
            // No sample: silence. Envelopes still run so notes finish.
            for v in &mut self.voices {
                for g in &mut v.grains {
                    g.active = false;
                }
                if v.active() {
                    for _ in 0..n {
                        v.env.next(att, dec, sus, rel);
                    }
                }
            }
            return;
        };
        let frames = data.frames();
        let data_sr = data.sample_rate.max(1) as f32;
        let rate = (data_sr / sr) as f64;
        let mode = (p.mode.round().max(0.0) as usize).min(2);
        let lfo_shape = (p.lfo_shape.round().max(0.0) as usize).min(4);
        let lfo_inc = lfo_hz(p.lfo_rate, p.lfo_sync >= 0.5, p.lfo_sync_rate, self.perf.tempo).clamp(0.01, 50.0) / sr;
        let filter_type = (p.filter_type.round().max(0.0) as usize).min(3);
        let q = 0.707 + p.resonance.clamp(0.0, 1.0) * 9.0;
        let vel_amt = p.velocity.clamp(0.0, 1.0);
        let out_gain = db_to_gain(p.volume_db);
        let spray_mul = if mode == 2 { 2.0 } else { 1.0 };
        let density = p.density.clamp(0.5, 400.0);
        let gk = gain_smooth_coef(sr);

        let mut start = 0;
        while start < n {
            let len = CONTROL.min(n - start);
            let lfo = self.lfo_value(lfo_shape);
            let ph = self.lfo_phase + lfo_inc * len as f32;
            if ph >= 1.0 {
                self.lfo_hold = self.rng.next_bipolar();
            }
            self.lfo_phase = ph.fract();
            let size_ms = (p.size * 2f32.powf(p.lfo_size.clamp(-1.0, 1.0) * lfo)).clamp(2.0, 2000.0);
            let grain_len = (size_ms * sr / 1000.0).max(8.0);
            let overlap = (size_ms / 1000.0 * density).clamp(1.0, GRAINS as f32);
            let b = Block {
                data,
                mode,
                head_base: p.position.clamp(0.0, 1.0) + 0.25 * p.lfo_pos.clamp(-1.0, 1.0) * lfo,
                loop_len: p.loop_len.clamp(0.001, 1.0),
                scan_step: p.scan.clamp(-4.0, 4.0) as f64 * rate / frames as f64,
                spray_frames: p.spray.clamp(0.0, 4000.0) * spray_mul * data_sr / 1000.0,
                grain_len,
                interval: sr / density,
                grain_gain: 0.9 / overlap.sqrt(),
                shape: (p.shape.round().clamp(0.0, 2.0)) as u8,
                pitch: p.transpose.clamp(-48.0, 48.0)
                    + p.fine.clamp(-100.0, 100.0) / 100.0
                    + p.lfo_pitch.clamp(-24.0, 24.0) * lfo,
                key_track: p.key_track >= 0.5,
                rand_pitch: p.rand_pitch.clamp(0.0, 48.0),
                rate,
                spread: p.spread.clamp(0.0, 1.0),
                reverse: p.reverse.clamp(0.0, 1.0),
            };
            for v in self.voices.iter_mut() {
                if !v.active() {
                    continue;
                }
                let vel_gain = 1.0 - vel_amt + vel_amt * v.velocity;
                v.bend = self.perf.bend_for(v.note);
                let g_target = self.perf.gain_for(v.note);
                for i in start..start + len {
                    let env = v.env.next(att, dec, sus, rel);
                    if !v.env.is_active() {
                        for g in &mut v.grains {
                            g.active = false;
                        }
                        break;
                    }
                    v.countdown -= 1.0;
                    if v.countdown <= 0.0 {
                        spawn(v, &b, &mut self.rng);
                        let jitter = if mode == 2 {
                            -(unit(&mut self.rng).max(1e-4)).ln()
                        } else {
                            1.0 + 0.1 * self.rng.next_bipolar()
                        };
                        v.countdown += (b.interval * jitter).max(1.0);
                    }
                    v.offset += b.scan_step;
                    let (mut l, mut r) = (0.0f32, 0.0f32);
                    for g in v.grains.iter_mut() {
                        if !g.active {
                            continue;
                        }
                        let w = g.window();
                        let (sl, sr_) = sample::read_hermite(data, g.pos);
                        l += sl * w * g.gl;
                        r += sr_ * w * g.gr;
                        g.pos += g.step;
                        g.age += 1;
                        if g.age >= g.len {
                            g.active = false;
                        }
                    }
                    if filter_type > 0 {
                        let cutoff = p.cutoff * 2f32.powf(p.filter_env.clamp(-8.0, 8.0) * env);
                        let fl = v.filter_l.process(l, cutoff, q, sr);
                        let fr = v.filter_r.process(r, cutoff, q, sr);
                        (l, r) = match filter_type {
                            1 => (fl.low, fr.low),
                            2 => (fl.band, fr.band),
                            _ => (fl.high, fr.high),
                        };
                    }
                    let g = env * vel_gain * out_gain * v.expr_gain.next(g_target, gk);
                    left[i] += soft_clip(l * g);
                    right[i] += soft_clip(r * g);
                }
            }
            start += len;
        }
    }
}

/// Starts one grain on `v` if a slot is free.
fn spawn(v: &mut Voice, b: &Block, rng: &mut Rng) {
    let Some(g) = v.grains.iter_mut().find(|g| !g.active) else {
        return;
    };
    let frames = b.data.frames();
    // Scan head as a fraction of the file.
    let base = b.head_base as f64;
    let head = match b.mode {
        0 => (base + v.offset).clamp(0.0, 1.0),
        1 => {
            let len = b.loop_len as f64;
            (base + v.offset.rem_euclid(len)).rem_euclid(1.0)
        }
        _ => (base + v.offset).rem_euclid(1.0),
    };
    let jitter = b.spray_frames * rng.next_bipolar();
    let pos = (head * frames as f64 + jitter as f64).clamp(0.0, (frames - 1) as f64);
    let key = if b.key_track {
        v.note as f32 - ROOT_NOTE as f32
    } else {
        0.0
    };
    let semis = b.pitch + v.bend + key + b.rand_pitch * rng.next_bipolar();
    let ratio = (2f64.powf(semis as f64 / 12.0) * b.rate).clamp(1.0 / 64.0, 16.0);
    let reversed = unit(rng) < b.reverse;
    let pan = if b.mode == 2 {
        rng.next_bipolar() * b.spread.max(0.5)
    } else {
        rng.next_bipolar() * b.spread
    };
    let (pl, pr) = pan_gains(pan);
    let len = b.grain_len as u32;
    let w = std::f32::consts::TAU / len as f32;
    *g = Grain {
        active: true,
        pos,
        step: if reversed { -ratio } else { ratio },
        len,
        age: 0,
        shape: b.shape,
        c1: 1.0,
        c2: w.cos(),
        k: 2.0 * w.cos(),
        gl: pl * b.grain_gain,
        gr: pr * b.grain_gain,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const SR: f32 = 48000.0;

    fn tone(freq: f32, secs: f32) -> Vec<Option<Arc<AudioData>>> {
        let n = (secs * 48000.0) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| (i as f32 * freq / 48000.0 * std::f32::consts::TAU).sin() * 0.5)
            .collect();
        vec![Some(Arc::new(AudioData {
            sample_rate: 48000,
            right: v.clone(),
            left: v,
        }))]
    }

    fn render(s: &mut GranularSynth, p: &GranularParams, smp: &SampleSlots, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            s.render(p, smp, cl, cr);
        }
        (l, r)
    }

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    fn zero_crossing_hz(x: &[f32]) -> f32 {
        let c = x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        c as f32 * SR / x.len() as f32
    }

    #[test]
    fn silent_without_sample() {
        let p = GranularParams::default();
        let mut s = GranularSynth::new(SR);
        s.note_on(60, 100);
        let empty: Vec<Option<Arc<AudioData>>> = Vec::new();
        let (l, r) = render(&mut s, &p, &empty, 4800);
        assert_eq!(peak(&l), 0.0);
        assert_eq!(peak(&r), 0.0);
        let none: Vec<Option<Arc<AudioData>>> = vec![None];
        let (l, _) = render(&mut s, &p, &none, 4800);
        assert_eq!(peak(&l), 0.0);
        assert_eq!(s.active_grains(), 0);
    }

    #[test]
    fn plays_a_sample_at_note_pitch() {
        let smp = tone(440.0, 2.0);
        let p = GranularParams {
            spread: 0.0,
            ..GranularParams::default()
        };
        let mut s = GranularSynth::new(SR);
        s.note_on(60, 100);
        let (l, r) = render(&mut s, &p, &smp, 24000);
        let pk = peak(&l).max(peak(&r));
        assert!(pk > 0.1 && pk < 1.5, "peak {pk}");
        let f = zero_crossing_hz(&l[4800..]);
        assert!((f - 440.0).abs() < 25.0, "{f}");
        // An octave up.
        let mut s = GranularSynth::new(SR);
        s.note_on(72, 100);
        let (l, _) = render(&mut s, &p, &smp, 24000);
        let f = zero_crossing_hz(&l[4800..]);
        assert!((f - 880.0).abs() < 50.0, "{f}");
    }

    #[test]
    fn grain_count_is_bounded() {
        let smp = tone(220.0, 1.0);
        let p = GranularParams {
            size: 1000.0,
            density: 200.0,
            mode: 2.0,
            ..GranularParams::default()
        };
        let mut s = GranularSynth::new(SR);
        s.note_on(60, 100);
        render(&mut s, &p, &smp, 24000);
        let g = s.active_grains();
        assert!(g <= GRAINS && g > GRAINS / 2, "{g}");
        for n in 0..20 {
            s.note_on(40 + n, 100);
        }
        let (l, _) = render(&mut s, &p, &smp, 24000);
        assert!(s.active_grains() <= VOICES * GRAINS);
        assert!(s.active_voices() <= VOICES);
        assert!(l.iter().all(|x| x.is_finite() && x.abs() < VOICES as f32 * 2.0));
    }

    #[test]
    fn density_sets_grain_rate() {
        let smp = tone(220.0, 1.0);
        let p = GranularParams {
            size: 10.0,
            density: 10.0,
            ..GranularParams::default()
        };
        let mut s = GranularSynth::new(SR);
        s.note_on(60, 100);
        // Count grain starts over one second.
        let mut starts = 0;
        let mut prev = 0;
        let mut l = [0.0f32; 16];
        let mut r = [0.0f32; 16];
        for _ in 0..3000 {
            s.render(&p, &smp, &mut l, &mut r);
            let now = s.active_grains();
            if now > prev {
                starts += now - prev;
            }
            prev = now;
        }
        assert!((8..=12).contains(&starts), "{starts}");
    }

    #[test]
    fn release_and_panic() {
        let smp = tone(330.0, 1.0);
        let p = GranularParams {
            release: 50.0,
            ..GranularParams::default()
        };
        let mut s = GranularSynth::new(SR);
        s.note_on(60, 100);
        render(&mut s, &p, &smp, 4800);
        s.note_off(60);
        render(&mut s, &p, &smp, 48000);
        assert_eq!(s.active_voices(), 0);
        let (l, _) = render(&mut s, &p, &smp, 1000);
        assert_eq!(peak(&l), 0.0);
        s.note_on(60, 100);
        render(&mut s, &p, &smp, 4800);
        s.panic();
        let (l, r) = render(&mut s, &p, &smp, 1000);
        assert_eq!(peak(&l).max(peak(&r)), 0.0);
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let smp = tone(500.0, 0.01); // a very short sample
        let mut rng = Rng::new(7);
        for trial in 0..40 {
            let mut p = GranularParams::default();
            for (i, info) in GranularParams::INFO.iter().enumerate() {
                let u = unit(&mut rng);
                let v = match trial % 3 {
                    0 => info.min,
                    1 => info.max,
                    _ => info.min + (info.max - info.min) * u,
                };
                p.set(i, v);
            }
            let mut s = GranularSynth::new(SR);
            for n in [0u8, 60, 127] {
                s.note_on(n, 127);
            }
            let (l, r) = render(&mut s, &p, &smp, 4800);
            assert!(
                l.iter().chain(r.iter()).all(|x| x.is_finite() && x.abs() <= 6.0),
                "trial {trial}"
            );
        }
    }

    #[test]
    fn presets_are_in_range_and_sound() {
        let smp = tone(220.0, 2.0);
        for (_, name, p) in presets() {
            for (i, info) in GranularParams::INFO.iter().enumerate() {
                let v = p.get(i);
                assert!(v >= info.min && v <= info.max, "{name}: {} = {v}", info.name);
            }
            let mut s = GranularSynth::new(SR);
            s.note_on(60, 100);
            let (l, r) = render(&mut s, &p, &smp, 96000);
            let pk = peak(&l).max(peak(&r));
            assert!(pk > 0.03 && pk < 1.5, "{name}: peak {pk}");
        }
    }
}
