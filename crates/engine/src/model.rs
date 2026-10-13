//! The project document: everything that gets saved to disk.
//!
//! Positions and lengths are measured in beats (quarter notes) so that the
//! project stays correct when the tempo changes.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::automation::AutomationLane;
use crate::dsp::drums::DrumKitParams;
use crate::dsp::synth::SynthParams;
use crate::time::Quantization;
use crate::warp::WarpSettings;

pub type Id = u64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub tempo: f64,
    pub beats_per_bar: u32,
    pub beat_unit: u32,
    pub launch_quantization: Quantization,
    pub metronome: bool,
    pub loop_enabled: bool,
    pub loop_start: f64,
    pub loop_length: f64,
    pub tracks: Vec<Track>,
    pub scenes: Vec<Scene>,
    pub master: MixerParams,
    pub next_id: Id,
}

impl Default for Project {
    fn default() -> Self {
        let mut p = Project {
            name: "Untitled".into(),
            tempo: 120.0,
            beats_per_bar: 4,
            beat_unit: 4,
            launch_quantization: Quantization::Bar,
            metronome: false,
            loop_enabled: false,
            loop_start: 0.0,
            loop_length: 16.0,
            tracks: Vec::new(),
            scenes: Vec::new(),
            master: MixerParams::default(),
            next_id: 1,
        };
        for _ in 0..8 {
            p.add_scene();
        }
        p
    }
}

impl Project {
    pub fn alloc_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn track_index(&self, id: Id) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn track(&self, id: Id) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: Id) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn add_scene(&mut self) -> usize {
        let n = self.scenes.len();
        self.scenes.push(Scene {
            name: format!("{}", n + 1),
            tempo: None,
        });
        for t in &mut self.tracks {
            t.slots.push(None);
        }
        n
    }

    pub fn insert_scene(&mut self, index: usize) {
        let index = index.min(self.scenes.len());
        self.scenes.insert(
            index,
            Scene {
                name: String::new(),
                tempo: None,
            },
        );
        for t in &mut self.tracks {
            t.slots.insert(index, None);
        }
        self.renumber_scenes();
    }

    pub fn remove_scene(&mut self, index: usize) {
        if index >= self.scenes.len() || self.scenes.len() <= 1 {
            return;
        }
        self.scenes.remove(index);
        for t in &mut self.tracks {
            t.slots.remove(index);
        }
        self.renumber_scenes();
    }

    fn renumber_scenes(&mut self) {
        for (i, s) in self.scenes.iter_mut().enumerate() {
            if s.name.is_empty() || s.name.parse::<usize>().is_ok() {
                s.name = format!("{}", i + 1);
            }
        }
    }

    /// Adds a track and returns its id.
    pub fn add_track(&mut self, kind: TrackKind, instrument: Instrument) -> Id {
        let id = self.alloc_id();
        let n = self.tracks.len();
        let name = match (&kind, &instrument) {
            (TrackKind::Audio, _) => format!("{} Audio", n + 1),
            (TrackKind::Midi, Instrument::DrumKit(_)) => format!("{} Drums", n + 1),
            (TrackKind::Midi, _) => format!("{} MIDI", n + 1),
        };
        self.tracks.push(Track {
            id,
            name,
            color: TRACK_COLORS[n % TRACK_COLORS.len()],
            kind,
            instrument,
            mixer: MixerParams::default(),
            slots: vec![None; self.scenes.len()],
            arrangement: Vec::new(),
            automation: Vec::new(),
        });
        id
    }

    pub fn remove_track(&mut self, id: Id) {
        self.tracks.retain(|t| t.id != id);
    }

    /// End of the last arrangement clip, in beats.
    pub fn arrangement_length(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|t| t.arrangement.iter())
            .map(|c| c.start + c.length)
            .fold(0.0, f64::max)
    }

    pub fn save_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let json = self.save_json().map_err(std::io::Error::other)?;
        std::fs::write(path, json)
    }

    /// Loads a project and re-reads any audio files it references.
    pub fn load_from(path: &Path) -> std::io::Result<Project> {
        let text = std::fs::read_to_string(path)?;
        let mut p: Project = serde_json::from_str(&text).map_err(std::io::Error::other)?;
        p.reload_audio(path.parent());
        Ok(p)
    }

    /// Loads audio data for every audio clip. Missing files are left empty.
    pub fn reload_audio(&mut self, base: Option<&Path>) {
        let visit = |clip: &mut Clip| {
            if let ClipContent::Audio(a) = &mut clip.content {
                if a.data.is_none() {
                    let mut p = std::path::PathBuf::from(&a.path);
                    if p.is_relative() {
                        if let Some(b) = base {
                            p = b.join(p);
                        }
                    }
                    a.data = AudioData::load_wav(&p).ok().map(Arc::new);
                }
            }
        };
        for t in &mut self.tracks {
            for c in t.slots.iter_mut().flatten() {
                visit(c);
            }
            for c in &mut t.arrangement {
                visit(&mut c.clip);
            }
        }
    }
}

pub const TRACK_COLORS: [[u8; 3]; 10] = [
    [255, 148, 166],
    [255, 165, 41],
    [204, 153, 39],
    [247, 244, 124],
    [191, 251, 0],
    [26, 255, 47],
    [37, 255, 168],
    [92, 255, 232],
    [139, 197, 255],
    [195, 152, 255],
];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    /// Optional tempo change applied when the scene is launched.
    pub tempo: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Instrument {
    None,
    Synth(SynthParams),
    DrumKit(DrumKitParams),
}

impl Instrument {
    pub fn name(&self) -> &'static str {
        match self {
            Instrument::None => "No Instrument",
            Instrument::Synth(_) => "Poly Synth",
            Instrument::DrumKit(_) => "Drum Kit",
        }
    }

    /// Identifies the instrument type so runtime state is only reused for the
    /// same kind of instrument.
    pub fn kind_tag(&self) -> u8 {
        match self {
            Instrument::None => 0,
            Instrument::Synth(_) => 1,
            Instrument::DrumKit(_) => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MixerParams {
    /// Fader level in dB. `-70` and below is treated as silence.
    pub volume_db: f32,
    /// -1 (left) .. 1 (right)
    pub pan: f32,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
}

impl Default for MixerParams {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            arm: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: Id,
    pub name: String,
    pub color: [u8; 3],
    pub kind: TrackKind,
    pub instrument: Instrument,
    pub mixer: MixerParams,
    /// One entry per scene.
    pub slots: Vec<Option<Clip>>,
    /// Clips on the Arrangement timeline, kept sorted by start.
    pub arrangement: Vec<ArrangementClip>,
    /// Arrangement automation lanes (positions in song beats).
    #[serde(default)]
    pub automation: Vec<AutomationLane>,
}

impl Track {
    pub fn sort_arrangement(&mut self) {
        self.arrangement
            .sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: Id,
    pub name: String,
    pub color: [u8; 3],
    /// Loop length of the clip content, in beats.
    pub length: f64,
    pub looping: bool,
    /// Clip gain in dB.
    #[serde(default)]
    pub gain_db: f32,
    pub content: ClipContent,
    /// Clip envelopes (positions in clip beats, looping with the clip).
    #[serde(default)]
    pub envelopes: Vec<AutomationLane>,
}

impl Default for Clip {
    fn default() -> Self {
        Clip {
            id: 0,
            name: String::new(),
            color: [0, 0, 0],
            length: 4.0,
            looping: true,
            gain_db: 0.0,
            content: ClipContent::Midi { notes: Vec::new() },
            envelopes: Vec::new(),
        }
    }
}

impl Clip {
    pub fn is_midi(&self) -> bool {
        matches!(self.content, ClipContent::Midi { .. })
    }

    pub fn notes(&self) -> &[Note] {
        match &self.content {
            ClipContent::Midi { notes } => notes,
            _ => &[],
        }
    }

    pub fn notes_mut(&mut self) -> Option<&mut Vec<Note>> {
        match &mut self.content {
            ClipContent::Midi { notes } => Some(notes),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ClipContent {
    Midi { notes: Vec<Note> },
    Audio(AudioRef),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub pitch: u8,
    pub velocity: u8,
    /// Start in beats relative to the clip start.
    pub start: f64,
    /// Length in beats.
    pub length: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AudioRef {
    pub path: String,
    #[serde(skip)]
    pub data: Option<Arc<AudioData>>,
    /// Warp markers and mode; unwarped by default.
    #[serde(default)]
    pub warp: WarpSettings,
}

impl AudioRef {
    pub fn new(path: impl Into<String>, data: Option<Arc<AudioData>>) -> Self {
        AudioRef {
            path: path.into(),
            data,
            warp: WarpSettings::default(),
        }
    }
}

/// Decoded audio, always stored as two channels of `f32`.
#[derive(Debug, Clone)]
pub struct AudioData {
    pub sample_rate: u32,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl AudioData {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames() as f64 / self.sample_rate as f64
    }

    pub fn load_wav(path: &Path) -> Result<AudioData, hound::Error> {
        let mut reader = hound::WavReader::open(path)?;
        let spec = reader.spec();
        let ch = spec.channels.max(1) as usize;
        let samples: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
            hound::SampleFormat::Int => {
                let scale = 1.0 / (1u64 << (spec.bits_per_sample.saturating_sub(1))) as f32;
                reader
                    .samples::<i32>()
                    .map(|s| s.map(|v| v as f32 * scale))
                    .collect::<Result<_, _>>()?
            }
        };
        let frames = samples.len() / ch;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for f in 0..frames {
            let l = samples[f * ch];
            let r = if ch > 1 { samples[f * ch + 1] } else { l };
            left.push(l);
            right.push(r);
        }
        Ok(AudioData {
            sample_rate: spec.sample_rate,
            left,
            right,
        })
    }

    /// Peak envelope with `buckets` points, used for waveform drawing.
    pub fn peaks(&self, buckets: usize) -> Vec<f32> {
        let n = self.frames();
        if n == 0 || buckets == 0 {
            return vec![0.0; buckets];
        }
        (0..buckets)
            .map(|b| {
                let s = b * n / buckets;
                let e = ((b + 1) * n / buckets).max(s + 1).min(n);
                (s..e)
                    .map(|i| self.left[i].abs().max(self.right[i].abs()))
                    .fold(0.0, f32::max)
            })
            .collect()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrangementClip {
    pub id: Id,
    /// Position on the timeline, in beats.
    pub start: f64,
    /// Length on the timeline, in beats. Content loops if this is longer than
    /// the clip's own loop length.
    pub length: f64,
    pub clip: Clip,
}

impl ArrangementClip {
    pub fn end(&self) -> f64 {
        self.start + self.length
    }
}

pub fn db_to_gain(db: f32) -> f32 {
    if db <= -70.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

pub fn gain_to_db(g: f32) -> f32 {
    if g <= 0.0 {
        -f32::INFINITY
    } else {
        20.0 * g.log10()
    }
}

/// Constant-power pan law: returns (left, right) gains.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let p = (pan.clamp(-1.0, 1.0) + 1.0) * 0.25 * std::f32::consts::PI;
    (p.cos() * std::f32::consts::SQRT_2, p.sin() * std::f32::consts::SQRT_2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pan_law_is_unity_at_centre_and_constant_power() {
        let (l, r) = pan_gains(0.0);
        assert!((l - 1.0).abs() < 1e-5 && (r - 1.0).abs() < 1e-5);
        for p in [-1.0, -0.5, 0.3, 1.0] {
            let (l, r) = pan_gains(p);
            assert!((l * l + r * r - 2.0).abs() < 1e-4);
        }
        let (l, r) = pan_gains(-1.0);
        assert!(r.abs() < 1e-5 && l > 1.4);
    }

    #[test]
    fn project_round_trips_through_json() {
        let p = crate::demo::demo_project();
        let json = p.save_json().unwrap();
        let q: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(q.tracks.len(), p.tracks.len());
        assert_eq!(q.scenes.len(), p.scenes.len());
        assert_eq!(q.tracks[0].slots.len(), p.scenes.len());
    }

    #[test]
    fn wav_files_load_as_stereo() {
        let path = std::env::temp_dir().join(format!("tessera-test-{}.wav", std::process::id()));
        let samples: Vec<f32> = (0..2000).map(|i| if i % 2 == 0 { 0.5 } else { -0.25 }).collect();
        crate::render::write_wav(&path, &samples, 22050).unwrap();
        let data = AudioData::load_wav(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(data.sample_rate, 22050);
        assert_eq!(data.frames(), 1000);
        assert!((data.left[10] - 0.5).abs() < 1e-3);
        assert!((data.right[10] + 0.25).abs() < 1e-3);
    }

    #[test]
    fn projects_save_and_load_from_disk() {
        let path = std::env::temp_dir().join(format!("tessera-test-{}.tessera", std::process::id()));
        let p = crate::demo::demo_project();
        p.save_to(&path).unwrap();
        let q = Project::load_from(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(q.name, p.name);
        assert_eq!(q.tracks[0].arrangement.len(), p.tracks[0].arrangement.len());
    }

    #[test]
    fn scenes_keep_slots_in_sync() {
        let mut p = Project::default();
        p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        p.insert_scene(2);
        assert_eq!(p.tracks[0].slots.len(), p.scenes.len());
        p.remove_scene(0);
        assert_eq!(p.tracks[0].slots.len(), p.scenes.len());
        assert_eq!(p.scenes[0].name, "1");
    }
}
