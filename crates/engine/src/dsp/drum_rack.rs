//! "Drum Rack": 16 pads on the notes C1..D#2 (MIDI 36..51, the General MIDI
//! drum layout), each with its own chain: a source (a recorded sample or a
//! synthesized drum), tuning, an envelope, a filter, drive, level and pan.
//! Pads can be grouped so that one cuts off another (choke groups, as with
//! open and closed hi-hats).
//!
//! Like the Sampler, sample data is passed to [`DrumRack::render`] as
//! [`SampleSlots`] (one slot per pad) so the parameters stay `Copy`.

use serde::{Deserialize, Serialize};

use super::envelope::exp_coef;
use super::filter::Svf;
use super::sample::{self, SampleSlots};
use super::Rng;
use crate::model::{db_to_gain, pan_gains};

pub const PADS: usize = 16;
/// Note of the first pad (C1).
pub const BASE_NOTE: u8 = 36;
/// Voices shared by all pads.
pub const VOICES: usize = 32;
/// Choke groups available (0 = none).
pub const CHOKE_GROUPS: u8 = 4;

/// Per-pad output buffers for pads that have their own device chain in a
/// Drum Rack's [`crate::fx::rack::Rack`]. Allocated on the UI thread.
pub struct PadBus {
    pub left: Vec<Vec<f32>>,
    pub right: Vec<Vec<f32>>,
    /// Bit `n` set: pad `n` renders into its own buffer.
    pub routed: u16,
    /// Frame in the pad buffers that the next render starts at.
    pub offset: usize,
}

impl PadBus {
    pub fn new(routed: u16) -> Self {
        Self {
            left: (0..PADS).map(|_| vec![0.0; crate::MAX_BLOCK]).collect(),
            right: (0..PADS).map(|_| vec![0.0; crate::MAX_BLOCK]).collect(),
            routed,
            offset: 0,
        }
    }

    /// Clears the first `frames` frames of every routed pad.
    pub fn clear(&mut self, frames: usize) {
        for pad in 0..PADS {
            if self.routed & (1 << pad) != 0 {
                self.left[pad][..frames].fill(0.0);
                self.right[pad][..frames].fill(0.0);
            }
        }
    }
}

/// What a pad plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PadSource {
    /// The sample loaded into this pad's slot.
    Sample,
    Kick,
    Snare,
    Clap,
    Rim,
    ClosedHat,
    OpenHat,
    Tom,
    Crash,
    Ride,
    Cowbell,
    Shaker,
}

impl PadSource {
    pub const ALL: [PadSource; 12] = [
        PadSource::Sample,
        PadSource::Kick,
        PadSource::Snare,
        PadSource::Clap,
        PadSource::Rim,
        PadSource::ClosedHat,
        PadSource::OpenHat,
        PadSource::Tom,
        PadSource::Crash,
        PadSource::Ride,
        PadSource::Cowbell,
        PadSource::Shaker,
    ];
    pub fn label(self) -> &'static str {
        match self {
            PadSource::Sample => "Sample",
            PadSource::Kick => "Kick",
            PadSource::Snare => "Snare",
            PadSource::Clap => "Clap",
            PadSource::Rim => "Rim",
            PadSource::ClosedHat => "Closed Hat",
            PadSource::OpenHat => "Open Hat",
            PadSource::Tom => "Tom",
            PadSource::Crash => "Crash",
            PadSource::Ride => "Ride",
            PadSource::Cowbell => "Cowbell",
            PadSource::Shaker => "Shaker",
        }
    }
}

/// One pad's chain.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PadParams {
    pub source: PadSource,
    /// Tuning in semitones.
    pub tune: f32,
    /// Decay time multiplier for synthesized sources; for samples the time
    /// over which the sample fades out (seconds, 0 = play it all).
    pub decay: f32,
    /// Sample start as a fraction 0..1.
    pub start: f32,
    pub reverse: bool,
    /// Low-pass cutoff in Hz (20 kHz = open).
    pub cutoff: f32,
    pub resonance: f32,
    /// Soft-clip drive, 0..1.
    pub drive: f32,
    pub volume_db: f32,
    pub pan: f32,
    /// Choke group 1..=4; 0 = none.
    pub choke: u8,
    pub mute: bool,
}

impl Default for PadParams {
    fn default() -> Self {
        Self {
            source: PadSource::Sample,
            tune: 0.0,
            decay: 1.0,
            start: 0.0,
            reverse: false,
            cutoff: 20000.0,
            resonance: 0.7,
            drive: 0.0,
            volume_db: 0.0,
            pan: 0.0,
            choke: 0,
            mute: false,
        }
    }
}

impl PadParams {
    pub fn synth(source: PadSource) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    fn tuned(mut self, tune: f32) -> Self {
        self.tune = tune;
        self
    }

    fn with_decay(mut self, decay: f32) -> Self {
        self.decay = decay;
        self
    }

    fn with_choke(mut self, g: u8) -> Self {
        self.choke = g;
        self
    }

    fn with_level(mut self, db: f32, pan: f32) -> Self {
        self.volume_db = db;
        self.pan = pan;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrumRackParams {
    pub pads: [PadParams; PADS],
    pub volume_db: f32,
}

impl Default for DrumRackParams {
    fn default() -> Self {
        Self::standard()
    }
}

impl DrumRackParams {
    /// Synthesized kit laid out like General MIDI drums, so clips written for
    /// the Drum Kit play the same parts.
    pub fn standard() -> Self {
        use PadSource::*;
        let p = PadParams::synth;
        Self {
            pads: [
                p(Kick),                                                           // 36 C1
                p(Rim).with_level(-4.0, 0.1),                                      // 37
                p(Snare),                                                          // 38
                p(Clap).with_level(-2.0, 0.0),                                     // 39
                p(Snare).tuned(3.0).with_decay(0.7),                               // 40
                p(Tom).tuned(-5.0).with_level(-2.0, -0.3),                         // 41
                p(ClosedHat).with_choke(1).with_level(-3.0, 0.25),                 // 42
                p(Tom).tuned(-3.0).with_level(-2.0, -0.2),                         // 43
                p(ClosedHat).with_decay(0.6).with_choke(1).with_level(-5.0, 0.25), // 44
                p(Tom).with_level(-2.0, 0.0),                                      // 45
                p(OpenHat).with_choke(1).with_level(-4.0, 0.25),                   // 46
                p(Tom).tuned(2.0).with_level(-2.0, 0.1),                           // 47
                p(Tom).tuned(5.0).with_level(-2.0, 0.2),                           // 48
                p(Crash).with_level(-4.0, -0.3),                                   // 49
                p(Tom).tuned(7.0).with_level(-2.0, 0.3),                           // 50
                p(Ride).with_level(-5.0, 0.3),                                     // 51
            ],
            volume_db: -4.0,
        }
    }

    /// Long, boomy kick and pitched toms in the style of classic analog drum
    /// machines.
    pub fn boom() -> Self {
        let mut k = Self::standard();
        k.pads[0] = PadParams::synth(PadSource::Kick).tuned(-3.0).with_decay(2.6);
        k.pads[2] = PadParams::synth(PadSource::Snare).tuned(-2.0).with_decay(1.3);
        k.pads[3] = PadParams::synth(PadSource::Clap).with_decay(1.4);
        k.pads[6].decay = 0.7;
        k.pads[10].decay = 1.5;
        k.pads[11] = PadParams::synth(PadSource::Cowbell).with_level(-6.0, 0.2);
        k.pads[15] = PadParams::synth(PadSource::Shaker).with_level(-6.0, -0.2);
        k
    }

    /// Short, dry and bright.
    pub fn tight() -> Self {
        let mut k = Self::standard();
        for (i, pad) in k.pads.iter_mut().enumerate() {
            pad.decay *= 0.55;
            if i != 0 {
                pad.tune += 2.0;
            }
        }
        k.pads[0].drive = 0.4;
        k
    }

    /// Dark and crunchy: filtered and driven.
    pub fn lofi() -> Self {
        let mut k = Self::standard();
        for pad in k.pads.iter_mut() {
            pad.cutoff = 4500.0;
            pad.drive = 0.5;
        }
        k.pads[0].cutoff = 1800.0;
        k
    }

    /// Every pad set to play its sample slot (for kits built from samples).
    pub fn empty() -> Self {
        Self {
            pads: [PadParams::default(); PADS],
            volume_db: -4.0,
        }
    }

    pub fn pad_for_note(note: u8) -> Option<usize> {
        let i = note.checked_sub(BASE_NOTE)? as usize;
        (i < PADS).then_some(i)
    }
}

/// Display name of a pad: its source, or `None` for a sample pad (the UI
/// shows the sample's file name).
pub fn pad_note_name(pad: usize) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    let n = BASE_NOTE as usize + pad;
    format!("{}{}", NAMES[n % 12], n / 12 - 2)
}

#[derive(Debug, Clone)]
struct Voice {
    active: bool,
    pad: usize,
    gain: f32,
    age: u64,
    fresh: bool,
    t: f32,
    phase: f32,
    phase2: f32,
    env: f32,
    noise_env: f32,
    noise_filter: Svf,
    pos: f64,
    filter_l: Svf,
    filter_r: Svf,
    /// Fade used when a voice is choked or a sample ends.
    fade: f32,
    fading: bool,
}

impl Voice {
    fn idle() -> Self {
        Self {
            active: false,
            pad: 0,
            gain: 0.0,
            age: 0,
            fresh: false,
            t: 0.0,
            phase: 0.0,
            phase2: 0.0,
            env: 0.0,
            noise_env: 0.0,
            noise_filter: Svf::default(),
            pos: 0.0,
            filter_l: Svf::default(),
            filter_r: Svf::default(),
            fade: 1.0,
            fading: false,
        }
    }
}

pub struct DrumRack {
    sample_rate: f32,
    voices: [Voice; VOICES],
    rng: Rng,
    counter: u64,
    /// Pads hit since the last render, for the UI's pad lights.
    pub hits: u16,
}

impl DrumRack {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            voices: std::array::from_fn(|_| Voice::idle()),
            rng: Rng::new(0x5eed_d00d),
            counter: 0,
            hits: 0,
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    /// Starts a pad. Mute and choke groups are applied on the next render,
    /// where the parameters are known.
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        let Some(pad) = DrumRackParams::pad_for_note(note) else {
            return;
        };
        self.counter += 1;
        self.hits |= 1 << pad;
        let idx = self.voices.iter().position(|v| !v.active).unwrap_or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)
                .unwrap_or(0)
        });
        let v = &mut self.voices[idx];
        *v = Voice::idle();
        v.active = true;
        v.fresh = true;
        v.pad = pad;
        v.age = self.counter;
        let vel = velocity as f32 / 127.0;
        v.gain = vel * vel;
        v.env = 1.0;
        v.noise_env = 1.0;
    }

    /// Applies mute and choke groups for voices started since the last render.
    fn start_fresh(&mut self, p: &DrumRackParams) {
        for i in 0..VOICES {
            if !(self.voices[i].active && self.voices[i].fresh) {
                continue;
            }
            let (pad, age) = (self.voices[i].pad, self.voices[i].age);
            let pp = p.pads[pad];
            if pp.mute {
                self.voices[i].active = false;
                continue;
            }
            // Choke: fade out older hits of other pads in the same group, and
            // older hits of this pad when it plays a sample (avoids smearing).
            for v in self.voices.iter_mut() {
                if !v.active || v.age >= age {
                    continue;
                }
                let other = p.pads[v.pad];
                if (pp.choke != 0 && other.choke == pp.choke && v.pad != pad)
                    || (v.pad == pad && pp.source == PadSource::Sample)
                {
                    v.fading = true;
                }
            }
        }
    }

    pub fn panic(&mut self) {
        for v in &mut self.voices {
            v.active = false;
        }
    }

    /// Releases all held notes: drums ring out, so this only stops samples
    /// that are fading out because of a choke.
    pub fn all_notes_off(&mut self) {}

    pub fn render(&mut self, p: &DrumRackParams, samples: &SampleSlots, left: &mut [f32], right: &mut [f32]) {
        self.render_routed(p, samples, left, right, None);
    }

    /// Like [`DrumRack::render`], but pads routed to their own chain (see
    /// [`PadBus`]) write into the pad's buffer at `bus.offset` instead.
    pub fn render_routed(
        &mut self,
        p: &DrumRackParams,
        samples: &SampleSlots,
        left: &mut [f32],
        right: &mut [f32],
        mut bus: Option<&mut PadBus>,
    ) {
        let sr = self.sample_rate;
        let dt = 1.0 / sr;
        let master = db_to_gain(p.volume_db);
        let fade_step = 1.0 / (0.004 * sr);
        self.start_fresh(p);
        for v in self.voices.iter_mut() {
            if !v.active {
                continue;
            }
            let pp = &p.pads[v.pad];
            let data = if pp.source == PadSource::Sample {
                match sample::slot(samples, v.pad) {
                    Some(d) => Some(d),
                    None => {
                        v.active = false;
                        continue;
                    }
                }
            } else {
                None
            };
            let pitch = 2f32.powf(pp.tune / 12.0);
            let decay = pp.decay.max(0.05);
            let (pl, pr) = pan_gains(pp.pan);
            let out = db_to_gain(pp.volume_db) * master * v.gain;
            let filtered = pp.cutoff < 19500.0;
            let q = 0.5 + pp.resonance.max(0.0) * 2.0;
            let drive = pp.drive.clamp(0.0, 1.0);
            let drive_gain = 1.0 + drive * 6.0;
            let drive_norm = 1.0 / drive_gain.tanh();

            if v.fresh {
                v.fresh = false;
                if let Some(d) = data {
                    let n = d.frames() as f64;
                    v.pos = if pp.reverse {
                        n - 1.0 - pp.start.clamp(0.0, 1.0) as f64 * n
                    } else {
                        pp.start.clamp(0.0, 1.0) as f64 * n
                    };
                }
            }
            let step = data.map(|d| d.sample_rate as f64 / sr as f64 * pitch as f64);
            let n = left.len();
            let (out_l, out_r): (&mut [f32], &mut [f32]) = match bus.as_deref_mut() {
                Some(b) if b.routed & (1 << v.pad) != 0 => {
                    let o = b.offset;
                    let (bl, br) = (&mut b.left[v.pad], &mut b.right[v.pad]);
                    (&mut bl[o..o + n], &mut br[o..o + n])
                }
                _ => (&mut *left, &mut *right),
            };
            for i in 0..n {
                let (mut l, mut r) = match data {
                    Some(d) => {
                        let s = sample::read_hermite(d, v.pos);
                        let st = step.unwrap_or(1.0);
                        v.pos += if pp.reverse { -st } else { st };
                        let ended = v.pos < 0.0 || v.pos >= d.frames() as f64;
                        // Optional fade-out "decay" for samples.
                        let env = if pp.decay < 8.0 && pp.decay > 0.0 && pp.source == PadSource::Sample {
                            v.env *= exp_coef(decay, sr);
                            v.env
                        } else {
                            1.0
                        };
                        if ended {
                            v.fading = true;
                        }
                        (s.0 * env, s.1 * env)
                    }
                    None => {
                        let s = synth_sample(pp.source, v, &mut self.rng, pitch, decay, sr, dt);
                        (s, s)
                    }
                };
                if filtered {
                    l = v.filter_l.process(l, pp.cutoff, q, sr).low;
                    r = v.filter_r.process(r, pp.cutoff, q, sr).low;
                }
                if drive > 0.0 {
                    l = (l * drive_gain).tanh() * drive_norm;
                    r = (r * drive_gain).tanh() * drive_norm;
                }
                if v.fading {
                    v.fade -= fade_step;
                    if v.fade <= 0.0 {
                        v.active = false;
                        break;
                    }
                }
                let g = out * v.fade;
                out_l[i] += l * g * pl;
                out_r[i] += r * g * pr;
                v.t += dt;
                if data.is_none() && (v.env < 1e-4 && v.noise_env < 1e-4 || v.t > 8.0) {
                    v.active = false;
                    break;
                }
                if data.is_some() && pp.decay > 0.0 && pp.decay < 8.0 && v.env < 1e-4 {
                    v.active = false;
                    break;
                }
            }
        }
    }
}

/// One sample of a synthesized drum. `pitch` scales frequencies, `decay`
/// scales envelope times.
#[inline]
fn synth_sample(src: PadSource, v: &mut Voice, rng: &mut Rng, pitch: f32, decay: f32, sr: f32, dt: f32) -> f32 {
    use std::f32::consts::TAU;
    let t = v.t;
    let s = match src {
        PadSource::Sample => 0.0,
        PadSource::Kick => {
            let f = (50.0 + 110.0 * (-t * 28.0).exp()) * pitch;
            v.phase = (v.phase + f * dt) % 1.0;
            v.env *= exp_coef(0.45 * decay, sr);
            v.noise_env = 0.0;
            let click = rng.next_bipolar() * (-t * 400.0).exp() * 0.3;
            ((v.phase * TAU).sin() + click) * v.env * 1.2
        }
        PadSource::Snare => {
            let f = (190.0 + 60.0 * (-t * 40.0).exp()) * pitch;
            v.phase = (v.phase + f * dt) % 1.0;
            v.env *= exp_coef(0.11 * decay, sr);
            v.noise_env *= exp_coef(0.18 * decay, sr);
            let hp = v.noise_filter.process(rng.next_bipolar(), 1500.0 * pitch, 0.7, sr).high;
            (v.phase * TAU).sin() * v.env * 0.5 + hp * v.noise_env * 0.6
        }
        PadSource::Clap => {
            let burst = if t < 0.03 {
                ((t * 100.0).fract() < 0.5) as i32 as f32
            } else {
                1.0
            };
            if t > 0.03 {
                v.noise_env *= exp_coef(0.15 * decay, sr);
            }
            v.env = 0.0;
            let bp = v.noise_filter.process(rng.next_bipolar(), 1200.0 * pitch, 2.0, sr).band;
            bp * v.noise_env * burst * 1.2
        }
        PadSource::Rim => {
            v.phase = (v.phase + 1700.0 * pitch * dt) % 1.0;
            v.env *= exp_coef(0.03 * decay, sr);
            v.noise_env = 0.0;
            (v.phase * TAU).sin() * v.env * 0.5
        }
        PadSource::ClosedHat | PadSource::OpenHat => {
            let base = if src == PadSource::OpenHat { 0.3 } else { 0.05 };
            v.noise_env *= exp_coef(base * decay, sr);
            v.env = 0.0;
            let hp = v.noise_filter.process(rng.next_bipolar(), 7500.0 * pitch, 0.9, sr).high;
            hp * v.noise_env * 0.35
        }
        PadSource::Tom => {
            let base = 130.0 * pitch;
            let f = base + base * 0.6 * (-t * 20.0).exp();
            v.phase = (v.phase + f * dt) % 1.0;
            v.env *= exp_coef(0.4 * decay, sr);
            v.noise_env = 0.0;
            (v.phase * TAU).sin() * v.env * 0.8
        }
        PadSource::Crash => {
            v.noise_env *= exp_coef(1.4 * decay, sr);
            v.env = 0.0;
            let hp = v.noise_filter.process(rng.next_bipolar(), 5000.0 * pitch, 0.6, sr).high;
            hp * v.noise_env * 0.3
        }
        PadSource::Ride => {
            // Metallic: two inharmonic squares plus filtered noise.
            v.phase = (v.phase + 587.0 * pitch * dt) % 1.0;
            v.phase2 = (v.phase2 + 845.0 * pitch * dt) % 1.0;
            v.noise_env *= exp_coef(1.8 * decay, sr);
            v.env = v.noise_env;
            let sq = ((v.phase < 0.5) as i32 as f32 - 0.5) + ((v.phase2 < 0.5) as i32 as f32 - 0.5);
            let n = rng.next_bipolar() * 0.5 + sq * 0.6;
            v.noise_filter.process(n, 6000.0 * pitch, 0.8, sr).high * v.noise_env * 0.3
        }
        PadSource::Cowbell => {
            v.phase = (v.phase + 540.0 * pitch * dt) % 1.0;
            v.phase2 = (v.phase2 + 800.0 * pitch * dt) % 1.0;
            v.env *= exp_coef(0.25 * decay, sr);
            v.noise_env = 0.0;
            let sq = ((v.phase < 0.5) as i32 as f32 - 0.5) + ((v.phase2 < 0.5) as i32 as f32 - 0.5);
            v.noise_filter.process(sq, 900.0 * pitch, 2.5, sr).band * v.env * 1.2
        }
        PadSource::Shaker => {
            let attack = (t / 0.015).min(1.0);
            v.noise_env *= exp_coef(0.12 * decay, sr);
            v.env = 0.0;
            let hp = v.noise_filter.process(rng.next_bipolar(), 9000.0 * pitch, 1.2, sr).band;
            hp * v.noise_env * attack * 0.5
        }
    };
    if !s.is_finite() {
        v.env = 0.0;
        v.noise_env = 0.0;
        return 0.0;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::AudioData;
    use std::sync::Arc;

    fn run(rack: &mut DrumRack, p: &DrumRackParams, slots: &SampleSlots, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            rack.render(p, slots, cl, cr);
        }
        (l, r)
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    #[test]
    fn every_synth_pad_in_every_kit_sounds_and_ends() {
        for kit in [
            DrumRackParams::standard(),
            DrumRackParams::boom(),
            DrumRackParams::tight(),
            DrumRackParams::lofi(),
        ] {
            for pad in 0..PADS {
                let mut d = DrumRack::new(48000.0);
                d.note_on(BASE_NOTE + pad as u8, 120);
                let (l, _) = run(&mut d, &kit, &[], 4800);
                assert!(peak(&l) > 0.005, "pad {pad} silent");
                assert!(peak(&l) < 4.0, "pad {pad} too loud");
                run(&mut d, &kit, &[], 48000 * 8);
                assert_eq!(d.active_voices(), 0, "pad {pad} never ended");
            }
        }
    }

    #[test]
    fn notes_outside_the_rack_are_ignored() {
        let mut d = DrumRack::new(48000.0);
        d.note_on(35, 100);
        d.note_on(52, 100);
        assert_eq!(d.active_voices(), 0);
    }

    #[test]
    fn sample_pads_play_their_slot_tuned_and_panned() {
        let n = 24000;
        let v: Vec<f32> = (0..n).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut slots: Vec<Option<Arc<AudioData>>> = vec![None; PADS];
        slots[3] = Some(Arc::new(AudioData {
            sample_rate: 48000,
            right: v.clone(),
            left: v,
        }));
        let mut p = DrumRackParams::empty();
        p.volume_db = 0.0;
        p.pads[3].pan = -1.0;
        let mut d = DrumRack::new(48000.0);
        // An empty sample pad is silent.
        d.note_on(BASE_NOTE + 2, 127);
        let (l, _) = run(&mut d, &p, &slots, 512);
        assert_eq!(peak(&l), 0.0);
        d.note_on(BASE_NOTE + 3, 127);
        let (l, r) = run(&mut d, &p, &slots, 4800);
        assert!(peak(&l) > 0.5, "{}", peak(&l));
        assert!(peak(&r) < 1e-4, "hard left");
        // Up an octave the sample ends in half the time.
        p.pads[3].tune = 12.0;
        let mut d = DrumRack::new(48000.0);
        d.note_on(BASE_NOTE + 3, 127);
        run(&mut d, &p, &slots, 12400);
        assert_eq!(d.active_voices(), 0);
    }

    #[test]
    fn closed_hat_chokes_open_hat() {
        let p = DrumRackParams::standard();
        let mut d = DrumRack::new(48000.0);
        d.note_on(46, 120);
        run(&mut d, &p, &[], 2400);
        assert_eq!(d.active_voices(), 1);
        d.note_on(42, 120);
        run(&mut d, &p, &[], 2400);
        // The open hat is gone, only the closed one remains.
        assert_eq!(d.active_voices(), 1);
        assert_eq!(d.voices.iter().find(|v| v.active).map(|v| v.pad), Some(6));
    }

    #[test]
    fn pad_settings_change_the_sound() {
        let base = DrumRackParams::standard();
        let render_pad = |p: &DrumRackParams, pad: usize| {
            let mut d = DrumRack::new(48000.0);
            d.note_on(BASE_NOTE + pad as u8, 120);
            run(&mut d, p, &[], 9600).0
        };
        let a = render_pad(&base, 0);
        let mut muted = base;
        muted.pads[0].mute = true;
        assert_eq!(peak(&render_pad(&muted, 0)), 0.0);
        let mut quiet = base;
        quiet.pads[0].volume_db = -20.0;
        assert!(peak(&render_pad(&quiet, 0)) < peak(&a) * 0.2);
        let mut dark = base;
        dark.pads[6].cutoff = 500.0;
        assert!(peak(&render_pad(&dark, 6)) < peak(&render_pad(&base, 6)) * 0.5);
    }

    #[test]
    fn params_round_trip() {
        let p = DrumRackParams::boom();
        let json = serde_json::to_string(&p).unwrap();
        let q: DrumRackParams = serde_json::from_str(&json).unwrap();
        assert_eq!(p, q);
        assert_eq!(pad_note_name(0), "C1");
        assert_eq!(pad_note_name(15), "D#2");
    }
}
