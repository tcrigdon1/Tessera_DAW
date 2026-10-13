//! The project document: everything that gets saved to disk.
//!
//! Positions and lengths are measured in beats (quarter notes) so that the
//! project stays correct when the tempo changes.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::automation::AutomationLane;
use crate::dsp::drum_rack::DrumRackParams;
use crate::dsp::drums::DrumKitParams;
use crate::dsp::fm::FmParams;
use crate::dsp::sampler::SamplerParams;
use crate::dsp::synth::SynthParams;
use crate::dsp::wavetable::WavetableParams;
use crate::fx::{Device, DeviceKind, DeviceParams, MAX_RETURNS};
use crate::groove::Groove;
use crate::midi::mapping::MidiMapping;
use crate::midi::scale::MusicalScale;
use crate::midi_fx::MidiDevice;
use crate::plugin::{PluginInstrument, PluginSlot};
use crate::time::Quantization;
use crate::timeline::{SignatureChange, TempoLane};
use crate::warp::WarpSettings;

pub type Id = u64;

/// Bus id of the master track in device-chain and mixer commands. Track and
/// return ids are allocated from 1.
pub const MASTER_BUS: Id = 0;

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
    /// Return tracks, fed by the tracks' sends.
    #[serde(default)]
    pub returns: Vec<ReturnTrack>,
    /// Insert effects on the master track.
    #[serde(default)]
    pub master_devices: Vec<Device>,
    /// Bars of count-in before recording starts (0 = none).
    #[serde(default)]
    pub count_in_bars: u32,
    /// Hardware controller knobs, faders and pads mapped to parameters.
    #[serde(default)]
    pub midi_mappings: Vec<MidiMapping>,
    /// VST3 and CLAP plug-ins used by devices and instruments.
    #[serde(default)]
    pub plugins: Vec<PluginSlot>,
    /// Arrangement locators (cue points), sorted by position.
    #[serde(default)]
    pub locators: Vec<Locator>,
    /// Song Tempo automation on the master track.
    #[serde(default)]
    pub tempo_lane: TempoLane,
    /// Time signature changes after the start (the signature at beat 0 is
    /// `beats_per_bar` / `beat_unit`).
    #[serde(default)]
    pub signatures: Vec<SignatureChange>,
    /// The Groove Pool.
    #[serde(default)]
    pub grooves: Vec<Groove>,
    /// Global groove amount, 0 ..= 1.3.
    #[serde(default = "default_groove_amount")]
    pub groove_amount: f32,
    /// Crossfader position, -1 (A) ..= 1 (B).
    #[serde(default)]
    pub crossfader: f32,
    /// Cue (pre-listen) output settings.
    #[serde(default)]
    pub cue: CueSettings,
    /// Punch-in/out region for Arrangement recording is the loop brace;
    /// these switch it on.
    #[serde(default)]
    pub punch_in: bool,
    #[serde(default)]
    pub punch_out: bool,
    /// MIDI Arrangement Overdub: new MIDI recordings add to the notes
    /// already on the Arrangement instead of replacing them.
    #[serde(default)]
    pub midi_overdub: bool,
}

fn default_groove_amount() -> f32 {
    1.0
}

/// A named position in the Arrangement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Locator {
    pub id: Id,
    pub name: String,
    pub beat: f64,
}

/// Solo buttons either solo (the default) or send tracks to the cue output
/// (Ableton's Solo/Cue switch). The cue output also carries the browser
/// preview.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CueSettings {
    /// Solo buttons become cue buttons.
    pub cue_mode: bool,
    /// Cue output level in dB.
    pub volume_db: f32,
}

impl Default for CueSettings {
    fn default() -> Self {
        Self {
            cue_mode: false,
            volume_db: 0.0,
        }
    }
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
            returns: Vec::new(),
            master_devices: Vec::new(),
            count_in_bars: 0,
            midi_mappings: Vec::new(),
            plugins: Vec::new(),
            locators: Vec::new(),
            tempo_lane: TempoLane::default(),
            signatures: Vec::new(),
            grooves: Vec::new(),
            groove_amount: 1.0,
            crossfader: 0.0,
            cue: CueSettings::default(),
            punch_in: false,
            punch_out: false,
            midi_overdub: false,
        };
        for _ in 0..8 {
            p.add_scene();
        }
        p.add_return(Some(DeviceKind::Reverb));
        p.add_return(Some(DeviceKind::Delay));
        let id = p.alloc_id();
        p.master_devices.push(Device::new(id, DeviceKind::Limiter));
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
            ..Default::default()
        });
        for t in &mut self.tracks {
            t.slots.push(None);
        }
        n
    }

    pub fn insert_scene(&mut self, index: usize) {
        let index = index.min(self.scenes.len());
        self.scenes.insert(index, Scene::default());
        for t in &mut self.tracks {
            t.slots.insert(index, None);
            for s in t.no_stop_slots.iter_mut() {
                if *s >= index {
                    *s += 1;
                }
            }
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
            t.no_stop_slots.retain(|&s| s != index);
            for s in t.no_stop_slots.iter_mut() {
                if *s > index {
                    *s -= 1;
                }
            }
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
            (TrackKind::Group, _) => format!("{} Group", n + 1),
            (TrackKind::Audio, _) => format!("{} Audio", n + 1),
            (TrackKind::Midi, Instrument::DrumKit(_) | Instrument::DrumRack(_)) => format!("{} Drums", n + 1),
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
            devices: Vec::new(),
            sends: vec![SEND_OFF_DB; self.returns.len()],
            input: TrackInput::default(),
            samples: Vec::new(),
            output: None,
            folded: false,
            frozen: false,
            freeze: None,
            delay_ms: 0.0,
            crossfade: CrossfadeAssign::None,
            no_stop_slots: Vec::new(),
            takes: Vec::new(),
            show_takes: false,
            midi_devices: Vec::new(),
            midi_input: MidiInput::default(),
        });
        id
    }

    /// Adds a return track, optionally with one effect on it, and returns its id.
    pub fn add_return(&mut self, effect: Option<DeviceKind>) -> Option<Id> {
        if self.returns.len() >= MAX_RETURNS {
            return None;
        }
        let id = self.alloc_id();
        let n = self.returns.len();
        let letter = (b'A' + n as u8) as char;
        let mut devices = Vec::new();
        if let Some(kind) = effect {
            let dev = self.alloc_id();
            let mut d = Device::new(dev, kind);
            // A return carries only the effect signal.
            if let Some(i) = d.params.info().iter().position(|p| p.name == "Dry/Wet") {
                d.params.set(i, 1.0);
            }
            devices.push(d);
        }
        let name = match effect {
            Some(k) => format!("{letter} {}", k.name()),
            None => format!("{letter} Return"),
        };
        self.returns.push(ReturnTrack {
            id,
            name,
            color: [0x9a, 0xa8, 0xc4],
            mixer: MixerParams::default(),
            devices,
        });
        for t in &mut self.tracks {
            t.sends.resize(self.returns.len(), SEND_OFF_DB);
        }
        Some(id)
    }

    pub fn remove_return(&mut self, id: Id) {
        let Some(index) = self.returns.iter().position(|r| r.id == id) else {
            return;
        };
        self.returns.remove(index);
        for t in &mut self.tracks {
            if index < t.sends.len() {
                t.sends.remove(index);
            }
        }
    }

    /// The device chain of a track, a return track or the master ([`MASTER_BUS`]).
    pub fn chain(&self, bus: Id) -> Option<&Vec<Device>> {
        if bus == MASTER_BUS {
            return Some(&self.master_devices);
        }
        if let Some(t) = self.track(bus) {
            return Some(&t.devices);
        }
        self.returns.iter().find(|r| r.id == bus).map(|r| &r.devices)
    }

    pub fn chain_mut(&mut self, bus: Id) -> Option<&mut Vec<Device>> {
        if bus == MASTER_BUS {
            return Some(&mut self.master_devices);
        }
        if let Some(i) = self.track_index(bus) {
            return Some(&mut self.tracks[i].devices);
        }
        self.returns.iter_mut().find(|r| r.id == bus).map(|r| &mut r.devices)
    }

    /// The saved plug-in of a slot (device id or track id).
    pub fn plugin(&self, slot: Id) -> Option<&PluginSlot> {
        self.plugins.iter().find(|p| p.slot == slot)
    }

    pub fn plugin_mut(&mut self, slot: Id) -> Option<&mut PluginSlot> {
        self.plugins.iter_mut().find(|p| p.slot == slot)
    }

    /// Slots that currently hold a plug-in: plug-in devices in any chain and
    /// tracks whose instrument is a plug-in.
    pub fn plugin_slots_in_use(&self) -> Vec<Id> {
        let chains = self
            .tracks
            .iter()
            .map(|t| &t.devices)
            .chain(self.returns.iter().map(|r| &r.devices))
            .chain(std::iter::once(&self.master_devices));
        let mut out: Vec<Id> = chains
            .flatten()
            .filter(|d| matches!(d.params, DeviceParams::Plugin(_)))
            .map(|d| d.id)
            .collect();
        out.extend(
            self.tracks
                .iter()
                .filter(|t| matches!(t.instrument, Instrument::Plugin(_)))
                .map(|t| t.id),
        );
        out
    }

    /// Forgets saved plug-ins whose device or instrument no longer exists.
    pub fn prune_plugins(&mut self) {
        let used = self.plugin_slots_in_use();
        self.plugins.retain(|p| used.contains(&p.slot));
    }

    /// Mixer settings of a track, a return or the master.
    pub fn mixer_mut(&mut self, bus: Id) -> Option<&mut MixerParams> {
        if bus == MASTER_BUS {
            return Some(&mut self.master);
        }
        if let Some(i) = self.track_index(bus) {
            return Some(&mut self.tracks[i].mixer);
        }
        self.returns.iter_mut().find(|r| r.id == bus).map(|r| &mut r.mixer)
    }

    /// Makes every track's send list match the number of returns (older
    /// projects and hand-edited files may disagree).
    pub fn fix_sends(&mut self) {
        let n = self.returns.len();
        for t in &mut self.tracks {
            t.sends.resize(n, SEND_OFF_DB);
        }
    }

    pub fn remove_track(&mut self, id: Id) {
        self.tracks.retain(|t| t.id != id);
        // Whatever was routed to it goes back to the master.
        for t in &mut self.tracks {
            if t.output == Some(id) {
                t.output = None;
            }
            if t.input.source == InputSource::Track(id) {
                t.input.source = InputSource::External;
            }
        }
        self.midi_mappings.retain(|m| !m.target.refers_to(id));
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
        p.fix_sends();
        p.reload_audio(path.parent());
        Ok(p)
    }

    /// Loads audio data for every audio clip. Missing files are left empty.
    pub fn reload_audio(&mut self, base: Option<&Path>) {
        let load = |path: &str| {
            let mut p = std::path::PathBuf::from(path);
            if p.is_relative() {
                if let Some(b) = base {
                    p = b.join(p);
                }
            }
            AudioData::load_wav(&p).ok().map(Arc::new)
        };
        let visit = |clip: &mut Clip| {
            if let ClipContent::Audio(a) = &mut clip.content {
                if a.data.is_none() {
                    a.data = load(&a.path);
                }
            }
        };
        for t in &mut self.tracks {
            for s in t.samples.iter_mut().flatten() {
                if s.data.is_none() {
                    s.data = load(&s.path);
                }
            }
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    /// Optional tempo change applied when the scene is launched.
    pub tempo: Option<f64>,
    /// Optional time signature (numerator, denominator) applied when the
    /// scene is launched.
    #[serde(default)]
    pub signature: Option<(u32, u32)>,
    /// What happens after the scene has played for a while.
    #[serde(default)]
    pub follow: SceneFollow,
    #[serde(default)]
    pub color: Option<[u8; 3]>,
}

/// Where a scene follow action goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SceneFollowAction {
    #[default]
    Next,
    Previous,
    First,
    Last,
    /// Any scene, possibly this one.
    Any,
    /// Any other scene.
    Other,
    /// Launch this scene again.
    Again,
    /// Stop all clips.
    Stop,
}

impl SceneFollowAction {
    pub const ALL: [SceneFollowAction; 8] = [
        SceneFollowAction::Next,
        SceneFollowAction::Previous,
        SceneFollowAction::First,
        SceneFollowAction::Last,
        SceneFollowAction::Any,
        SceneFollowAction::Other,
        SceneFollowAction::Again,
        SceneFollowAction::Stop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SceneFollowAction::Next => "Next",
            SceneFollowAction::Previous => "Previous",
            SceneFollowAction::First => "First",
            SceneFollowAction::Last => "Last",
            SceneFollowAction::Any => "Any",
            SceneFollowAction::Other => "Other",
            SceneFollowAction::Again => "Play Again",
            SceneFollowAction::Stop => "Stop",
        }
    }
}

/// A scene follow action: after `beats`, launch another scene.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SceneFollow {
    pub enabled: bool,
    pub action: SceneFollowAction,
    /// Time after the launch, in beats.
    pub beats: f64,
}

impl Default for SceneFollow {
    fn default() -> Self {
        Self {
            enabled: false,
            action: SceneFollowAction::Next,
            beats: 16.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
    /// A group track: sums the tracks routed to it through its own devices
    /// and mixer. It has no clips.
    Group,
}

/// Which side of the crossfader a track is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CrossfadeAssign {
    #[default]
    None,
    A,
    B,
}

impl CrossfadeAssign {
    pub fn label(self) -> &'static str {
        match self {
            CrossfadeAssign::None => "-",
            CrossfadeAssign::A => "A",
            CrossfadeAssign::B => "B",
        }
    }

    /// Gain for a crossfader position -1 (A) ..= 1 (B): full until the
    /// centre, then a constant-power fade out towards the far side.
    #[inline]
    pub fn gain(self, x: f32) -> f32 {
        let x = x.clamp(-1.0, 1.0);
        match self {
            CrossfadeAssign::None => 1.0,
            CrossfadeAssign::A => {
                if x <= 0.0 {
                    1.0
                } else {
                    (x * std::f32::consts::FRAC_PI_2).cos()
                }
            }
            CrossfadeAssign::B => {
                if x >= 0.0 {
                    1.0
                } else {
                    (-x * std::f32::consts::FRAC_PI_2).cos()
                }
            }
        }
    }
}

/// Audio rendered by Freeze: the Arrangement from beat 0 and each Session
/// clip, played instead of the instrument and devices. Not saved; frozen
/// tracks are rendered again after loading.
#[derive(Debug, Clone, Default)]
pub struct FreezeData {
    pub arrangement: Option<Arc<AudioData>>,
    /// Rendered length of the Arrangement, in beats.
    pub arrangement_beats: f64,
    /// One entry per scene: the rendered clip and its length in beats.
    pub slots: Vec<Option<(Arc<AudioData>, f64)>>,
    /// Tempo the audio was rendered at.
    pub tempo: f64,
}

/// One recording pass kept for comping.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TakeLane {
    pub id: Id,
    pub name: String,
    pub clips: Vec<ArrangementClip>,
}

/// A track's instrument and its settings. Kept `Copy` (no heap data) so
/// parameter changes can be sent to the audio thread as plain values; sample
/// data for the Sampler and Drum Rack lives in [`Track::samples`].
// Boxing the larger variants would put a heap pointer in a value that is
// copied to the audio thread, which must never free it.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Instrument {
    None,
    Synth(SynthParams),
    DrumKit(DrumKitParams),
    Sampler(SamplerParams),
    DrumRack(DrumRackParams),
    Fm(FmParams),
    Wavetable(WavetableParams),
    /// A VST3 or CLAP instrument; the track id names its slot in
    /// [`Project::plugins`].
    Plugin(PluginInstrument),
}

impl Instrument {
    pub fn name(&self) -> &'static str {
        match self {
            Instrument::None => "No Instrument",
            Instrument::Synth(_) => "Poly Synth",
            Instrument::DrumKit(_) => "Drum Kit",
            Instrument::Sampler(_) => "Sampler",
            Instrument::DrumRack(_) => "Drum Rack",
            Instrument::Fm(_) => "FM Synth",
            Instrument::Wavetable(_) => "Wavetable",
            Instrument::Plugin(_) => "Plug-in",
        }
    }

    /// Identifies the instrument type so runtime state is only reused for the
    /// same kind of instrument.
    pub fn kind_tag(&self) -> u8 {
        match self {
            Instrument::None => 0,
            Instrument::Synth(_) => 1,
            Instrument::DrumKit(_) => 2,
            Instrument::Sampler(_) => 3,
            Instrument::DrumRack(_) => 4,
            Instrument::Fm(_) => 5,
            Instrument::Wavetable(_) => 6,
            Instrument::Plugin(_) => 7,
        }
    }

    /// True for instruments that map notes to drum pads (the piano roll shows
    /// pad names instead of a keyboard).
    pub fn is_drums(&self) -> bool {
        matches!(self, Instrument::DrumKit(_) | Instrument::DrumRack(_))
    }

    /// How many sample slots the instrument uses.
    pub fn sample_slots(&self) -> usize {
        match self {
            Instrument::Sampler(_) => 1,
            Instrument::DrumRack(_) => crate::dsp::drum_rack::PADS,
            _ => 0,
        }
    }
}

/// A sample loaded into an instrument (Sampler, Drum Rack pad).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleRef {
    pub path: String,
    #[serde(skip)]
    pub data: Option<Arc<AudioData>>,
}

impl SampleRef {
    /// Loads a WAV file.
    pub fn load(path: &Path) -> Result<SampleRef, hound::Error> {
        let data = AudioData::load_wav(path)?;
        Ok(SampleRef {
            path: path.to_string_lossy().to_string(),
            data: Some(Arc::new(data)),
        })
    }

    /// File name without folder or extension.
    pub fn name(&self) -> String {
        Path::new(&self.path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.clone())
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
    /// Insert effects after the instrument.
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Send levels in dB, one per return track.
    #[serde(default)]
    pub sends: Vec<f32>,
    /// Audio input routing and monitoring.
    #[serde(default)]
    pub input: TrackInput,
    /// Samples played by the instrument (Sampler: slot 0, Drum Rack: one per
    /// pad).
    #[serde(default)]
    pub samples: Vec<Option<SampleRef>>,
    /// Where the track's output goes: `None` is the master, otherwise a
    /// group track (or any track listening to it).
    #[serde(default)]
    pub output: Option<Id>,
    /// A group track's members are hidden.
    #[serde(default)]
    pub folded: bool,
    /// Freeze: play rendered audio instead of the instrument and devices.
    #[serde(default)]
    pub frozen: bool,
    #[serde(skip)]
    pub freeze: Option<Arc<FreezeData>>,
    /// Track delay in milliseconds; negative plays earlier.
    #[serde(default)]
    pub delay_ms: f32,
    #[serde(default)]
    pub crossfade: CrossfadeAssign,
    /// Empty Session slots without a stop button (launching their scene
    /// leaves the track playing).
    #[serde(default)]
    pub no_stop_slots: Vec<usize>,
    /// Take lanes for comping, newest last.
    #[serde(default)]
    pub takes: Vec<TakeLane>,
    #[serde(default)]
    pub show_takes: bool,
    /// MIDI effects before the instrument.
    #[serde(default)]
    pub midi_devices: Vec<MidiDevice>,
    /// Where a MIDI track takes live notes from. Monitoring uses
    /// [`TrackInput::monitor`].
    #[serde(default)]
    pub midi_input: MidiInput,
}

impl Track {
    /// The sample in an instrument slot, if loaded.
    pub fn sample(&self, slot: usize) -> Option<&SampleRef> {
        self.samples.get(slot).and_then(|s| s.as_ref())
    }

    /// Puts a sample into an instrument slot, growing the list as needed.
    pub fn set_sample(&mut self, slot: usize, sample: Option<SampleRef>) {
        if self.samples.len() <= slot {
            self.samples.resize(slot + 1, None);
        }
        self.samples[slot] = sample;
    }
}

/// Send level treated as off.
pub const SEND_OFF_DB: f32 = -70.0;

/// A return track: an effect bus fed by the tracks' sends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReturnTrack {
    pub id: Id,
    pub name: String,
    pub color: [u8; 3],
    pub mixer: MixerParams,
    #[serde(default)]
    pub devices: Vec<Device>,
}

/// Which input channels an audio track listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum InputChannels {
    /// Inputs 1 and 2 as a stereo pair.
    #[default]
    Stereo,
    /// Input 1, mono.
    Left,
    /// Input 2, mono.
    Right,
}

impl InputChannels {
    pub const ALL: [InputChannels; 3] = [InputChannels::Stereo, InputChannels::Left, InputChannels::Right];

    pub fn label(self) -> &'static str {
        match self {
            InputChannels::Stereo => "In 1/2",
            InputChannels::Left => "In 1",
            InputChannels::Right => "In 2",
        }
    }

    /// Maps a stereo input frame to this routing.
    #[inline]
    pub fn route(self, l: f32, r: f32) -> (f32, f32) {
        match self {
            InputChannels::Stereo => (l, r),
            InputChannels::Left => (l, l),
            InputChannels::Right => (r, r),
        }
    }
}

/// When an audio track plays its live input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Monitor {
    /// Always.
    In,
    /// While the track is armed.
    #[default]
    Auto,
    /// Never.
    Off,
}

impl Monitor {
    pub fn label(self) -> &'static str {
        match self {
            Monitor::In => "In",
            Monitor::Auto => "Auto",
            Monitor::Off => "Off",
        }
    }

    pub fn next(self) -> Monitor {
        match self {
            Monitor::In => Monitor::Auto,
            Monitor::Auto => Monitor::Off,
            Monitor::Off => Monitor::In,
        }
    }
}

/// Which hardware MIDI input a MIDI track listens to.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MidiFrom {
    /// Every enabled input port.
    #[default]
    AllIns,
    /// One port, by name.
    Port(String),
    /// No hardware input.
    None,
}

/// A MIDI track's input routing.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MidiInput {
    pub from: MidiFrom,
    /// MIDI channel 0 ..= 15, or `None` for all channels.
    pub channel: Option<u8>,
}

impl MidiInput {
    /// Whether a message from `port` (see [`crate::midi::port_id`]) on
    /// `channel` reaches the track.
    pub fn accepts(&self, port: u64, channel: u8) -> bool {
        let port_ok = match &self.from {
            MidiFrom::AllIns => true,
            MidiFrom::Port(name) => crate::midi::port_id(name) == port,
            MidiFrom::None => false,
        };
        port_ok && self.channel.is_none_or(|c| c == channel)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TrackInput {
    pub channels: InputChannels,
    pub monitor: Monitor,
    /// Where an audio track's input comes from.
    #[serde(default)]
    pub source: InputSource,
}

/// An audio track's input: the audio interface, or another track's output
/// (post mixer), for resampling a track or bussing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum InputSource {
    #[default]
    External,
    Track(Id),
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
    /// Where playback starts, in beats (Start marker).
    #[serde(default)]
    pub start_marker: f64,
    /// Where the loop starts, in beats; the loop ends at `length`.
    #[serde(default)]
    pub loop_start: f64,
    /// Session launch settings.
    #[serde(default)]
    pub launch: ClipLaunch,
    /// Scale shown in the piano roll, if any.
    #[serde(default)]
    pub scale: Option<MusicalScale>,
    /// Groove from the Groove Pool applied to the clip's notes on playback.
    #[serde(default)]
    pub groove: Option<Id>,
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
            start_marker: 0.0,
            loop_start: 0.0,
            launch: ClipLaunch::default(),
            scale: None,
            groove: None,
        }
    }
}

impl Clip {
    /// Playback region (start and loop markers).
    pub fn region(&self) -> ClipRegion {
        ClipRegion::new(self.start_marker, self.loop_start, self.length, self.looping)
    }

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

/// The part of a clip that plays: from the start marker to the end, then
/// (when looping) from the loop start to the end over and over. Positions
/// are clip beats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClipRegion {
    pub start: f64,
    pub loop_start: f64,
    pub end: f64,
    pub looping: bool,
}

impl ClipRegion {
    /// Normalises markers: everything inside `0 .. end`, the loop at least
    /// 1/64 beat long.
    pub fn new(start: f64, loop_start: f64, end: f64, looping: bool) -> Self {
        let end = end.max(1.0 / 64.0);
        let clamp = |v: f64| {
            if v.is_finite() {
                v.clamp(0.0, end - 1.0 / 64.0)
            } else {
                0.0
            }
        };
        Self {
            start: clamp(start),
            loop_start: clamp(loop_start),
            end,
            looping,
        }
    }

    /// Length of the loop in beats.
    pub fn loop_length(&self) -> f64 {
        self.end - self.loop_start
    }

    /// Clip position `t` beats after the clip was launched, or `None` once a
    /// clip without loop has played to its end.
    #[inline]
    pub fn position(&self, t: f64) -> Option<f64> {
        let t = t.max(0.0);
        let p = self.start + t;
        if p < self.end {
            return Some(p);
        }
        if !self.looping {
            return None;
        }
        Some(self.loop_start + (p - self.end).rem_euclid(self.loop_length()))
    }

    /// Time from launch until the first pass reaches the end.
    pub fn first_pass(&self) -> f64 {
        self.end - self.start
    }
}

/// How a Session clip reacts to its launch button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LaunchMode {
    /// Press starts the clip.
    #[default]
    Trigger,
    /// Press starts, release stops.
    Gate,
    /// Press starts, the next press stops.
    Toggle,
    /// The clip restarts at the launch quantization for as long as the
    /// button is held, and stops on release.
    Repeat,
}

impl LaunchMode {
    pub const ALL: [LaunchMode; 4] = [
        LaunchMode::Trigger,
        LaunchMode::Gate,
        LaunchMode::Toggle,
        LaunchMode::Repeat,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LaunchMode::Trigger => "Trigger",
            LaunchMode::Gate => "Gate",
            LaunchMode::Toggle => "Toggle",
            LaunchMode::Repeat => "Repeat",
        }
    }
}

/// What happens when a clip's follow action time is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FollowKind {
    /// Keep playing.
    #[default]
    NoAction,
    Stop,
    /// Restart this clip.
    Again,
    Previous,
    Next,
    First,
    Last,
    /// Any clip of the group, possibly this one.
    Any,
    /// Any other clip of the group.
    Other,
}

impl FollowKind {
    pub const ALL: [FollowKind; 9] = [
        FollowKind::NoAction,
        FollowKind::Stop,
        FollowKind::Again,
        FollowKind::Previous,
        FollowKind::Next,
        FollowKind::First,
        FollowKind::Last,
        FollowKind::Any,
        FollowKind::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FollowKind::NoAction => "No Action",
            FollowKind::Stop => "Stop",
            FollowKind::Again => "Play Again",
            FollowKind::Previous => "Previous",
            FollowKind::Next => "Next",
            FollowKind::First => "First",
            FollowKind::Last => "Last",
            FollowKind::Any => "Any",
            FollowKind::Other => "Other",
        }
    }
}

/// Follow actions: after `time` beats (or the clip's loop length when
/// `linked`), do action A with probability `chance_a`, otherwise action B.
/// They act on the clip's group: the neighbouring clips in its track with no
/// empty slot in between.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FollowAction {
    pub enabled: bool,
    pub a: FollowKind,
    pub b: FollowKind,
    /// 0 ..= 1.
    pub chance_a: f32,
    /// Use the clip's length as the follow time.
    pub linked: bool,
    /// Follow time in beats when not linked.
    pub time: f64,
}

impl Default for FollowAction {
    fn default() -> Self {
        Self {
            enabled: false,
            a: FollowKind::Next,
            b: FollowKind::NoAction,
            chance_a: 1.0,
            linked: true,
            time: 4.0,
        }
    }
}

/// A Session clip's launch settings.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipLaunch {
    pub mode: LaunchMode,
    /// Launch quantization of this clip; `None` follows the global setting.
    pub quantization: Option<Quantization>,
    /// Take over the playing position of the clip it replaces.
    pub legato: bool,
    pub follow: FollowAction,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArrangementClip {
    pub id: Id,
    /// Position on the timeline, in beats.
    pub start: f64,
    /// Length on the timeline, in beats. Content loops if this is longer than
    /// the clip's own loop length.
    pub length: f64,
    pub clip: Clip,
    /// Fade-in and fade-out lengths of an audio clip, in beats.
    #[serde(default)]
    pub fade_in: f64,
    #[serde(default)]
    pub fade_out: f64,
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
