//! "Sampler": plays one audio sample across the keyboard, Simpler-style.
//!
//! Three playback modes:
//!
//! * **Classic** – the sample is played chromatically from the root note,
//!   optionally looping a region while the key is held.
//! * **One-Shot** – each note plays the sample from start to end, ignoring
//!   note-off.
//! * **Slice** – the sample is cut into slices (by transients or evenly) and
//!   each slice is mapped to its own key, starting at C1 (MIDI 36).
//!
//! Every voice runs through an amp envelope, a multimode filter with its own
//! envelope, and pan. The sample data is not part of the parameters (those
//! stay `Copy` so the UI can send them to the audio thread); it is passed to
//! [`Sampler::render`] as [`SampleSlots`] from the track's snapshot.

use super::{gain_smooth_coef, ExprGain, Performance};
use serde::{Deserialize, Serialize};

use super::envelope::Adsr;
use super::filter::Svf;
use super::sample::{self, SampleSlots};
use crate::model::{db_to_gain, pan_gains, AudioData};

pub const VOICES: usize = 16;
/// Most slices a sample can be cut into.
pub const MAX_SLICES: usize = 32;
/// Key of the first slice in Slice mode (C1).
pub const SLICE_BASE_NOTE: u8 = 36;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayMode {
    Classic,
    OneShot,
    Slice,
}

impl PlayMode {
    pub const ALL: [PlayMode; 3] = [PlayMode::Classic, PlayMode::OneShot, PlayMode::Slice];
    pub fn label(self) -> &'static str {
        match self {
            PlayMode::Classic => "Classic",
            PlayMode::OneShot => "1-Shot",
            PlayMode::Slice => "Slice",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterType {
    LowPass,
    HighPass,
    BandPass,
}

impl FilterType {
    pub const ALL: [FilterType; 3] = [FilterType::LowPass, FilterType::HighPass, FilterType::BandPass];
    pub fn label(self) -> &'static str {
        match self {
            FilterType::LowPass => "LP",
            FilterType::HighPass => "HP",
            FilterType::BandPass => "BP",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SamplerParams {
    pub mode: PlayMode,
    /// Region of the sample that plays, as fractions 0..1 of its length.
    pub start: f32,
    pub end: f32,
    /// Classic mode: loop while the key is held.
    pub loop_on: bool,
    /// Loop start as a fraction 0..1 of the start..end region.
    pub loop_start: f32,
    /// Plays the region backwards.
    pub reverse: bool,
    /// Note at which the sample plays at its original pitch.
    pub root: u8,
    pub transpose: f32,
    /// Fine tune in cents.
    pub detune: f32,
    /// Slice start points as fractions 0..1 of the whole sample, ascending.
    /// Only the first `slice_count` entries are used.
    pub slices: [f32; MAX_SLICES],
    pub slice_count: u8,
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    /// How much velocity affects the level, 0..1.
    pub velocity: f32,
    pub filter_on: bool,
    pub filter_type: FilterType,
    pub cutoff: f32,
    pub resonance: f32,
    /// Filter envelope depth in octaves.
    pub filter_env: f32,
    pub f_attack: f32,
    pub f_decay: f32,
    pub f_sustain: f32,
    pub f_release: f32,
    /// Most voices that play at once (1 = mono).
    pub voices: u8,
    pub pan: f32,
    pub volume_db: f32,
}

impl Default for SamplerParams {
    fn default() -> Self {
        Self {
            mode: PlayMode::Classic,
            start: 0.0,
            end: 1.0,
            loop_on: false,
            loop_start: 0.0,
            reverse: false,
            root: 60,
            transpose: 0.0,
            detune: 0.0,
            slices: even_slices(8),
            slice_count: 8,
            attack: 0.001,
            decay: 1.0,
            sustain: 1.0,
            release: 0.08,
            velocity: 0.6,
            filter_on: false,
            filter_type: FilterType::LowPass,
            cutoff: 18000.0,
            resonance: 0.7,
            filter_env: 0.0,
            f_attack: 0.001,
            f_decay: 0.4,
            f_sustain: 0.0,
            f_release: 0.2,
            voices: 8,
            pan: 0.0,
            volume_db: -3.0,
        }
    }
}

impl SamplerParams {
    /// Settings for chopping a loop: slice mode cut into `n` equal parts.
    pub fn sliced(n: usize) -> Self {
        let n = n.clamp(1, MAX_SLICES);
        Self {
            mode: PlayMode::Slice,
            slices: even_slices(n),
            slice_count: n as u8,
            voices: 1,
            ..Self::default()
        }
    }

    /// Replaces the slice points (fractions of the sample, any order).
    pub fn set_slices(&mut self, points: &[f32]) {
        let mut v: Vec<f32> = points.iter().map(|p| p.clamp(0.0, 1.0)).collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        v.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
        if v.first().is_none_or(|&f| f > 1e-4) {
            v.insert(0, 0.0);
        }
        v.truncate(MAX_SLICES);
        self.slices = [1.0; MAX_SLICES];
        self.slices[..v.len()].copy_from_slice(&v);
        self.slice_count = v.len() as u8;
    }

    /// The region `(start, end)` in fractions of the sample that a note plays,
    /// or `None` if the note has no slice.
    pub fn region_for(&self, note: u8) -> Option<(f32, f32)> {
        let (s, e) = order(self.start, self.end);
        match self.mode {
            PlayMode::Classic | PlayMode::OneShot => Some((s, e)),
            PlayMode::Slice => {
                let count = (self.slice_count as usize).clamp(1, MAX_SLICES);
                let i = note.checked_sub(SLICE_BASE_NOTE)? as usize;
                if i >= count {
                    return None;
                }
                let a = self.slices[i].clamp(0.0, 1.0);
                let b = if i + 1 < count {
                    self.slices[i + 1].clamp(a, 1.0)
                } else {
                    1.0
                };
                Some((a, b))
            }
        }
    }
}

fn order(a: f32, b: f32) -> (f32, f32) {
    let (a, b) = (a.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

pub fn even_slices(n: usize) -> [f32; MAX_SLICES] {
    let n = n.clamp(1, MAX_SLICES);
    std::array::from_fn(|i| if i < n { i as f32 / n as f32 } else { 1.0 })
}

#[derive(Debug, Clone)]
struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    age: u64,
    /// The play position still has to be set from the sample on the next
    /// render (note-on does not see the sample data).
    fresh: bool,
    /// Current position in source frames.
    pos: f64,
    /// Region in source frames: plays from `a` to `b` (or `b` to `a` reversed).
    a: f64,
    b: f64,
    loop_a: f64,
    looping: bool,
    gate: bool,
    amp_env: Adsr,
    filt_env: Adsr,
    filter_l: Svf,
    filter_r: Svf,
    /// Short fade at the region end to avoid clicks.
    tail: f32,
    expr_gain: ExprGain,
}

impl Voice {
    fn new(sr: f32) -> Self {
        Self {
            note: 0,
            velocity: 0.0,
            held: false,
            age: 0,
            fresh: false,
            pos: 0.0,
            a: 0.0,
            b: 0.0,
            loop_a: 0.0,
            looping: false,
            gate: true,
            amp_env: Adsr::new(sr),
            filt_env: Adsr::new(sr),
            filter_l: Svf::default(),
            filter_r: Svf::default(),
            tail: 1.0,
            expr_gain: ExprGain::default(),
        }
    }

    fn active(&self) -> bool {
        self.amp_env.is_active()
    }
}

pub struct Sampler {
    sample_rate: f32,
    pub perf: Performance,
    voices: [Voice; VOICES],
    counter: u64,
    /// Polyphony limit, taken from the parameters on each render.
    max_voices: u8,
}

impl Sampler {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            perf: Performance::default(),
            sample_rate,
            voices: std::array::from_fn(|_| Voice::new(sample_rate)),
            counter: 0,
            max_voices: SamplerParams::default().voices,
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active()).count()
    }

    /// Starts a note. Polyphony is limited by the `voices` parameter seen on
    /// the last render (mono = 1).
    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.counter += 1;
        let limit = (self.max_voices as usize).clamp(1, VOICES);
        let active = self.active_voices();
        let idx = if active >= limit {
            // Steal the oldest active voice.
            self.voices
                .iter()
                .enumerate()
                .filter(|(_, v)| v.active())
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)
                .unwrap_or(0)
        } else {
            self.voices
                .iter()
                .position(|v| !v.active())
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
                })
        };
        let v = &mut self.voices[idx];
        v.note = note;
        v.velocity = velocity as f32 / 127.0;
        v.held = true;
        v.age = self.counter;
        v.fresh = true;
        v.tail = 1.0;
        v.filter_l.reset();
        v.filter_r.reset();
        v.amp_env.reset();
        v.filt_env.reset();
        v.amp_env.trigger();
        v.filt_env.trigger();
    }

    pub fn note_off(&mut self, note: u8) {
        for v in &mut self.voices {
            if v.held && v.note == note {
                v.held = false;
                if v.gate {
                    v.amp_env.release();
                    v.filt_env.release();
                }
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

    /// Sets up a fresh voice's play region from the sample.
    fn start_voice(v: &mut Voice, p: &SamplerParams, data: &AudioData) -> bool {
        let Some((s, e)) = p.region_for(v.note) else {
            return false;
        };
        let n = data.frames() as f64;
        let a = (s as f64 * n).floor();
        let b = (e as f64 * n).min(n - 1.0);
        if b - a < 2.0 {
            return false;
        }
        v.a = a;
        v.b = b;
        v.looping = p.mode == PlayMode::Classic && p.loop_on;
        let la = a + (b - a) * p.loop_start.clamp(0.0, 0.98) as f64;
        v.loop_a = if p.reverse { b - (la - a) } else { la };
        v.pos = if p.reverse { b } else { a };
        // One-shot and slices play to the end regardless of note-off.
        v.gate = p.mode == PlayMode::Classic;
        true
    }

    /// Adds the output to `left`/`right`.
    pub fn render(&mut self, p: &SamplerParams, samples: &SampleSlots, left: &mut [f32], right: &mut [f32]) {
        self.max_voices = p.voices;
        let Some(data) = sample::slot(samples, 0) else {
            // No sample loaded: drop any notes.
            for v in &mut self.voices {
                if v.fresh {
                    v.fresh = false;
                    v.amp_env.reset();
                    v.filt_env.reset();
                }
            }
            return;
        };
        let sr = self.sample_rate;
        let out_gain = db_to_gain(p.volume_db);
        let (pl, pr) = pan_gains(p.pan);
        let q = 0.5 + p.resonance.max(0.0) * 2.0;
        let src_ratio = data.sample_rate as f64 / sr as f64;
        // About 2 ms of fade where a region ends.
        let tail_step = 1.0 / (0.002 * sr).max(1.0);
        let gk = gain_smooth_coef(sr);
        for v in self.voices.iter_mut() {
            if !v.active() {
                continue;
            }
            if v.fresh {
                v.fresh = false;
                v.expr_gain.reset();
                if !Self::start_voice(v, p, data) {
                    v.amp_env.reset();
                    v.filt_env.reset();
                    continue;
                }
            }
            let semis = match p.mode {
                PlayMode::Slice => p.transpose + p.detune / 100.0,
                _ => v.note as f32 - p.root as f32 + p.transpose + p.detune / 100.0,
            };
            let step = src_ratio * 2f64.powf((semis + self.perf.bend_for(v.note)) as f64 / 12.0);
            let g_target = self.perf.gain_for(v.note);
            let step = if p.reverse { -step } else { step };
            let vel_gain = 1.0 - p.velocity.clamp(0.0, 1.0) * (1.0 - v.velocity * v.velocity);
            for i in 0..left.len() {
                let a = v.amp_env.next(p.attack, p.decay, p.sustain, p.release);
                if !v.amp_env.is_active() {
                    break;
                }
                let fe = v.filt_env.next(p.f_attack, p.f_decay, p.f_sustain, p.f_release);
                let (mut l, mut r) = sample::read_hermite(data, v.pos);
                if p.filter_on {
                    let cutoff = p.cutoff * 2f32.powf(p.filter_env * fe);
                    let fl = v.filter_l.process(l, cutoff, q, sr);
                    let fr = v.filter_r.process(r, cutoff, q, sr);
                    (l, r) = match p.filter_type {
                        FilterType::LowPass => (fl.low, fr.low),
                        FilterType::HighPass => (fl.high, fr.high),
                        FilterType::BandPass => (fl.band, fr.band),
                    };
                }
                let g = a * vel_gain * out_gain * v.tail * v.expr_gain.next(g_target, gk);
                left[i] += l * g * pl;
                right[i] += r * g * pr;

                v.pos += step;
                let past_end = if step > 0.0 { v.pos >= v.b } else { v.pos <= v.a };
                if past_end {
                    if v.looping && v.held {
                        let span = if step > 0.0 { v.b - v.loop_a } else { v.loop_a - v.a };
                        if span > 1.0 {
                            v.pos = if step > 0.0 {
                                v.loop_a + (v.pos - v.b)
                            } else {
                                v.loop_a - (v.a - v.pos)
                            };
                            continue;
                        }
                    }
                    v.tail -= tail_step;
                    v.pos -= step; // hold the last frame while fading
                    if v.tail <= 0.0 {
                        v.amp_env.reset();
                        v.filt_env.reset();
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sine_sample(freq: f32, secs: f32) -> Arc<AudioData> {
        let sr = 48000;
        let n = (secs * sr as f32) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / sr as f32).sin() * 0.8)
            .collect();
        Arc::new(AudioData {
            sample_rate: sr,
            right: v.clone(),
            left: v,
        })
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count()
    }

    fn render(s: &mut Sampler, p: &SamplerParams, slots: &SampleSlots, frames: usize) -> Vec<f32> {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            s.render(p, slots, cl, cr);
        }
        l
    }

    #[test]
    fn plays_at_root_pitch_and_transposes_an_octave() {
        let slots = [Some(sine_sample(440.0, 1.0))];
        let p = SamplerParams::default();
        let mut s = Sampler::new(48000.0);
        s.note_on(60, 127);
        let a = render(&mut s, &p, &slots, 24000);
        s.panic();
        s.note_on(72, 127);
        let b = render(&mut s, &p, &slots, 24000);
        let (za, zb) = (zero_crossings(&a), zero_crossings(&b));
        assert!((za as i32 - 220).abs() <= 2, "root {za}");
        assert!((zb as i32 - 440).abs() <= 3, "octave {zb}");
    }

    #[test]
    fn resamples_between_rates() {
        // A 22.05 kHz sample must keep its pitch on a 48 kHz engine.
        let n = 22050;
        let v: Vec<f32> = (0..n)
            .map(|i| (i as f32 * 441.0 * std::f32::consts::TAU / 22050.0).sin())
            .collect();
        let slots = [Some(Arc::new(AudioData {
            sample_rate: 22050,
            right: v.clone(),
            left: v,
        }))];
        let mut s = Sampler::new(48000.0);
        s.note_on(60, 127);
        let out = render(&mut s, &SamplerParams::default(), &slots, 24000);
        let z = zero_crossings(&out);
        assert!((z as i32 - 220).abs() <= 3, "{z}");
    }

    #[test]
    fn classic_stops_after_release_and_one_shot_ignores_note_off() {
        let slots = [Some(sine_sample(200.0, 2.0))];
        let mut p = SamplerParams::default();
        let mut s = Sampler::new(48000.0);
        s.note_on(60, 100);
        render(&mut s, &p, &slots, 4800);
        s.note_off(60);
        render(&mut s, &p, &slots, 24000);
        assert_eq!(s.active_voices(), 0);

        p.mode = PlayMode::OneShot;
        s.note_on(60, 100);
        render(&mut s, &p, &slots, 4800);
        s.note_off(60);
        render(&mut s, &p, &slots, 24000);
        assert_eq!(s.active_voices(), 1, "one-shot keeps playing");
        render(&mut s, &p, &slots, 96000);
        assert_eq!(s.active_voices(), 0, "and ends with the sample");
    }

    #[test]
    fn loops_while_held() {
        let slots = [Some(sine_sample(300.0, 0.1))];
        let mut p = SamplerParams {
            loop_on: true,
            loop_start: 0.5,
            ..SamplerParams::default()
        };
        let mut s = Sampler::new(48000.0);
        s.note_on(60, 127);
        let out = render(&mut s, &p, &slots, 48000);
        // Still sounding after 10x the sample length.
        let late = out[40000..].iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(late > 0.2, "{late}");
        p.reverse = true;
        s.panic();
        s.note_on(60, 127);
        let out = render(&mut s, &p, &slots, 48000);
        let late = out[40000..].iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(late > 0.2, "reverse loop {late}");
    }

    #[test]
    fn slices_map_to_keys() {
        // Four quarter-second segments at different pitches.
        let sr = 48000usize;
        let mut v = Vec::new();
        for f in [200.0f32, 400.0, 800.0, 1600.0] {
            v.extend((0..sr / 4).map(|i| (i as f32 * f * std::f32::consts::TAU / sr as f32).sin()));
        }
        let slots = [Some(Arc::new(AudioData {
            sample_rate: sr as u32,
            right: v.clone(),
            left: v,
        }))];
        let p = SamplerParams::sliced(4);
        assert_eq!(p.region_for(SLICE_BASE_NOTE + 4), None);
        for (k, f) in [200usize, 400, 800, 1600].iter().enumerate() {
            let mut s = Sampler::new(48000.0);
            s.note_on(SLICE_BASE_NOTE + k as u8, 127);
            let out = render(&mut s, &p, &slots, 9600);
            let z = zero_crossings(&out[..9600]);
            let expected = f / 5;
            assert!((z as i32 - expected as i32).abs() <= 2, "slice {k}: {z} vs {expected}");
        }
    }

    #[test]
    fn mono_limits_voices_and_missing_sample_is_silent() {
        let slots = [Some(sine_sample(200.0, 1.0))];
        let p = SamplerParams {
            voices: 1,
            ..SamplerParams::default()
        };
        let mut s = Sampler::new(48000.0);
        render(&mut s, &p, &slots, 256);
        for n in 50..60 {
            s.note_on(n, 100);
        }
        render(&mut s, &p, &slots, 256);
        assert_eq!(s.active_voices(), 1);

        let empty: [Option<Arc<AudioData>>; 0] = [];
        let mut s = Sampler::new(48000.0);
        s.note_on(60, 100);
        let out = render(&mut s, &p, &empty, 1024);
        assert!(out.iter().all(|x| *x == 0.0));
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn filter_and_slices_settings_behave() {
        let mut p = SamplerParams::default();
        p.set_slices(&[0.5, 0.25, 0.0, 0.25, 0.75]);
        assert_eq!(p.slice_count, 4);
        assert_eq!(&p.slices[..4], &[0.0, 0.25, 0.5, 0.75]);

        let slots = [Some(sine_sample(6000.0, 0.5))];
        let mut s = Sampler::new(48000.0);
        s.note_on(60, 127);
        let open = render(&mut s, &p, &slots, 9600);
        p.filter_on = true;
        p.cutoff = 300.0;
        s.panic();
        s.note_on(60, 127);
        let closed = render(&mut s, &p, &slots, 9600);
        let rms = |x: &[f32]| (x[4800..].iter().map(|v| v * v).sum::<f32>() / 4800.0).sqrt();
        assert!(rms(&closed) < rms(&open) * 0.1, "{} vs {}", rms(&closed), rms(&open));
    }

    #[test]
    fn params_round_trip_and_old_files_get_defaults() {
        let p = SamplerParams::sliced(5);
        let json = serde_json::to_string(&p).unwrap();
        let q: SamplerParams = serde_json::from_str(&json).unwrap();
        assert_eq!(p, q);
        let partial: SamplerParams = serde_json::from_str(r#"{"root":48}"#).unwrap();
        assert_eq!(partial.root, 48);
        assert_eq!(partial.end, 1.0);
    }
}
