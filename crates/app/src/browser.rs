//! Browser state and actions: categories, user folders ("Places"), the
//! factory sample library, sample preview, and what happens when an item is
//! dropped somewhere in the app.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tessera_engine::dsp::drum_rack::{DrumRackParams, PadSource};
use tessera_engine::dsp::sample::detect_transients;
use tessera_engine::dsp::sampler::{PlayMode, SamplerParams, MAX_SLICES};
use tessera_engine::fx::DeviceKind;
use tessera_engine::model::{
    ArrangementClip, AudioData, AudioRef, Clip, ClipContent, Id, Instrument, SampleRef, TrackKind,
};
use tessera_engine::Command;

use crate::app::{ClipRef, DetailMode, TesseraApp};

/// Something that can be dragged out of the browser.
#[derive(Debug, Clone)]
pub enum BrowserItem {
    Instrument { name: String, instrument: Box<Instrument> },
    Effect(DeviceKind),
    Sample(PathBuf),
}

impl BrowserItem {
    pub fn label(&self) -> String {
        match self {
            BrowserItem::Instrument { name, .. } => name.clone(),
            BrowserItem::Effect(k) => k.name().to_string(),
            BrowserItem::Sample(p) => file_label(p),
        }
    }
}

/// Where a browser item was dropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DropTarget {
    /// Empty space: create a new track.
    NewTrack,
    /// A track header or mixer strip.
    Track(Id),
    /// A Session clip slot.
    Slot(Id, usize),
    /// An Arrangement lane at a beat position.
    Arrangement(Id, f64),
    /// The Device view of a track.
    Device(Id),
    /// A Drum Rack pad.
    Pad(Id, usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Sounds,
    Drums,
    Instruments,
    AudioEffects,
    Samples,
    Places,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Category::Sounds,
        Category::Drums,
        Category::Instruments,
        Category::AudioEffects,
        Category::Samples,
        Category::Places,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::Sounds => "Sounds",
            Category::Drums => "Drums",
            Category::Instruments => "Instruments",
            Category::AudioEffects => "Audio Effects",
            Category::Samples => "Samples",
            Category::Places => "Places",
        }
    }
}

/// One entry of a folder listing.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Saved between runs.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Settings {
    places: Vec<PathBuf>,
}

pub struct BrowserState {
    pub category: Category,
    pub search: String,
    /// User folders shown under Places.
    pub places: Vec<PathBuf>,
    /// Folder holding the factory sample library, once written.
    pub factory_dir: Option<PathBuf>,
    pub expanded: HashSet<PathBuf>,
    listings: HashMap<PathBuf, Vec<DirEntry>>,
    /// Item highlighted in the list.
    pub selected: Option<String>,
    /// Play samples when they are clicked.
    pub auto_preview: bool,
    /// Sample currently previewing, and its data for the waveform.
    pub previewing: Option<(PathBuf, Arc<AudioData>)>,
    /// Pad selected in the Drum Rack view, per track.
    pub drum_pad: HashMap<Id, usize>,
    /// Time each pad of the shown Drum Rack last lit up.
    pub pad_flash: [f64; 16],
}

impl Default for BrowserState {
    fn default() -> Self {
        Self {
            category: Category::Sounds,
            search: String::new(),
            places: Vec::new(),
            factory_dir: None,
            expanded: HashSet::new(),
            listings: HashMap::new(),
            selected: None,
            auto_preview: true,
            previewing: None,
            drum_pad: HashMap::new(),
            pad_flash: [-10.0; 16],
        }
    }
}

/// Per-user folder for settings and the factory library.
pub fn app_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("TESSERA_HOME") {
        return Some(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("Tessera"))
    } else if cfg!(target_os = "macos") {
        home.map(|h| h.join("Library/Application Support/Tessera"))
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|h| h.join(".local/share")))
            .map(|p| p.join("tessera"))
    }
}

/// File name without the extension.
pub fn file_label(p: &Path) -> String {
    p.file_stem()
        .or_else(|| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

pub fn is_audio_file(p: &Path) -> bool {
    p.extension()
        .map(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("wave"))
        .unwrap_or(false)
}

/// Bump when the factory library changes so it is rewritten.
const FACTORY_VERSION: &str = "1";

/// Writes the factory library if it is missing or outdated. Returns its folder.
pub fn ensure_factory_library(base: &Path) -> std::io::Result<PathBuf> {
    let dir = base.join("Factory Samples");
    let marker = dir.join(".version");
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(FACTORY_VERSION) {
        return Ok(dir);
    }
    let sr = 44100;
    for s in tessera_engine::library::factory_samples(sr) {
        let folder = dir.join(s.folder);
        std::fs::create_dir_all(&folder)?;
        let mut inter = Vec::with_capacity(s.data.frames() * 2);
        for i in 0..s.data.frames() {
            inter.push(s.data.left[i]);
            inter.push(s.data.right[i]);
        }
        tessera_engine::render::write_wav(&folder.join(format!("{}.wav", s.name)), &inter, sr)
            .map_err(std::io::Error::other)?;
    }
    std::fs::write(&marker, FACTORY_VERSION)?;
    Ok(dir)
}

impl BrowserState {
    /// Loads saved places and makes sure the factory library exists.
    pub fn init(&mut self) -> Option<String> {
        let base = app_dir()?;
        if let Ok(text) = std::fs::read_to_string(base.join("browser.json")) {
            if let Ok(s) = serde_json::from_str::<Settings>(&text) {
                self.places = s.places;
            }
        }
        if self.places.is_empty() {
            // Start with the user's Music folder (or home) as a place.
            if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
                let home = PathBuf::from(home);
                let music = home.join("Music");
                self.places.push(if music.is_dir() { music } else { home });
            }
        }
        match ensure_factory_library(&base) {
            Ok(dir) => {
                self.factory_dir = Some(dir);
                None
            }
            Err(e) => Some(format!("Could not write the factory sample library: {e}")),
        }
    }

    fn save(&self) {
        let Some(base) = app_dir() else { return };
        let s = Settings {
            places: self.places.clone(),
        };
        if std::fs::create_dir_all(&base).is_ok() {
            if let Ok(json) = serde_json::to_string_pretty(&s) {
                std::fs::write(base.join("browser.json"), json).ok();
            }
        }
    }

    pub fn add_place(&mut self, dir: PathBuf) {
        if !self.places.contains(&dir) {
            self.places.push(dir.clone());
            self.expanded.insert(dir);
            self.save();
        }
    }

    pub fn remove_place(&mut self, dir: &Path) {
        self.places.retain(|p| p != dir);
        self.save();
    }

    /// Lists a folder (sub-folders first, then audio files), cached.
    pub fn list(&mut self, dir: &Path) -> &[DirEntry] {
        self.listings.entry(dir.to_path_buf()).or_insert_with(|| {
            let mut v: Vec<DirEntry> = std::fs::read_dir(dir)
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| {
                            let path = e.path();
                            let name = e.file_name();
                            if name.to_string_lossy().starts_with('.') {
                                return None;
                            }
                            let is_dir = path.is_dir();
                            (is_dir || is_audio_file(&path)).then_some(DirEntry { path, is_dir })
                        })
                        .collect()
                })
                .unwrap_or_default();
            v.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then_with(|| {
                    file_label(&a.path)
                        .to_lowercase()
                        .cmp(&file_label(&b.path).to_lowercase())
                })
            });
            v
        })
    }

    /// Forgets cached folder listings (after files change on disk).
    pub fn refresh(&mut self) {
        self.listings.clear();
    }

    /// Audio files under `dir` whose names contain `query` (case-insensitive),
    /// searched a few levels deep.
    pub fn search_files(&mut self, dir: &Path, query: &str, out: &mut Vec<PathBuf>, depth: usize) {
        if depth == 0 || out.len() >= 200 {
            return;
        }
        let q = query.to_lowercase();
        let entries: Vec<DirEntry> = self.list(dir).to_vec();
        for e in entries {
            if e.is_dir {
                self.search_files(&e.path, query, out, depth - 1);
            } else if file_label(&e.path).to_lowercase().contains(&q) {
                out.push(e.path);
            }
        }
    }
}

impl TesseraApp {
    // ----- preview -----------------------------------------------------------

    /// Plays a sample file through the preview output.
    pub fn preview_sample(&mut self, path: &Path) {
        match AudioData::load_wav(path) {
            Ok(d) => {
                let d = Arc::new(d);
                self.send(Command::PreviewSample(d.clone()));
                self.browser.previewing = Some((path.to_path_buf(), d));
            }
            Err(e) => self.status = format!("Could not preview {}: {e}", path.display()),
        }
    }

    pub fn stop_preview(&mut self) {
        self.send(Command::StopPreview);
    }

    // ----- instruments and samples -------------------------------------------

    /// The selected Drum Rack pad of a track.
    pub fn drum_pad(&self, track: Id) -> usize {
        self.browser.drum_pad.get(&track).copied().unwrap_or(0)
    }

    /// Loads a sample file into an instrument slot (Sampler: 0, Drum Rack:
    /// pad index) and adapts the instrument (slice points, pad source).
    pub fn load_instrument_sample(&mut self, track: Id, slot: usize, path: &Path) -> bool {
        let sample = match SampleRef::load(path) {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Could not load {}: {e}", path.display());
                return false;
            }
        };
        let name = sample.name();
        let data = sample.data.clone();
        let Some(t) = self.project.track_mut(track) else {
            return false;
        };
        match &mut t.instrument {
            Instrument::Sampler(p) => {
                if p.mode == PlayMode::Slice {
                    if let Some(d) = &data {
                        let pts = detect_transients(d, MAX_SLICES, 0.5);
                        if pts.len() > 1 {
                            p.set_slices(&pts);
                        }
                    }
                }
                p.start = 0.0;
                p.end = 1.0;
            }
            Instrument::DrumRack(p) => {
                if let Some(pad) = p.pads.get_mut(slot) {
                    pad.source = PadSource::Sample;
                    pad.start = 0.0;
                    pad.reverse = false;
                    pad.decay = 0.0;
                }
            }
            _ => return false,
        }
        t.set_sample(slot, Some(sample));
        self.status = format!("Loaded {name}");
        self.changed();
        true
    }

    pub fn clear_instrument_sample(&mut self, track: Id, slot: usize) {
        if let Some(t) = self.project.track_mut(track) {
            if t.sample(slot).is_some() {
                t.set_sample(slot, None);
                self.changed();
            }
        }
    }

    pub fn load_instrument_sample_dialog(&mut self, track: Id, slot: usize) {
        if let Some(path) = rfd::FileDialog::new().add_filter("WAV audio", &["wav"]).pick_file() {
            self.load_instrument_sample(track, slot, &path);
        }
    }

    /// Puts a sample on a MIDI track the way a drop does: into its Sampler or
    /// the selected Drum Rack pad, or replacing another instrument with a
    /// Sampler.
    fn sample_to_midi_track(&mut self, track: Id, path: &Path, pad: Option<usize>) {
        let Some(t) = self.project.track(track) else { return };
        match t.instrument {
            Instrument::Sampler(_) => {
                self.load_instrument_sample(track, 0, path);
            }
            Instrument::DrumRack(_) => {
                let pad = pad.unwrap_or_else(|| self.drum_pad(track));
                self.load_instrument_sample(track, pad, path);
            }
            _ => {
                self.set_instrument(track, Instrument::Sampler(SamplerParams::default()));
                self.load_instrument_sample(track, 0, path);
            }
        }
        self.selected_track = Some(track);
        self.detail = DetailMode::Device;
    }

    /// Creates an audio clip for a file, sized to whole 16ths at the current
    /// tempo.
    fn audio_clip(&mut self, path: &Path, color: [u8; 3]) -> Option<Clip> {
        let data = match AudioData::load_wav(path) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("Could not load {}: {e}", path.display());
                return None;
            }
        };
        let beats = (data.duration_secs() * self.project.tempo / 60.0 * 4.0)
            .round()
            .max(1.0)
            / 4.0;
        Some(Clip {
            id: self.project.alloc_id(),
            name: file_label(path),
            color,
            length: beats,
            looping: true,
            gain_db: 0.0,
            content: ClipContent::Audio(AudioRef {
                path: path.to_string_lossy().to_string(),
                data: Some(Arc::new(data)),
            }),
        })
    }

    /// Places a sample on the Arrangement of an audio track.
    pub fn sample_to_arrangement(&mut self, track: Id, beat: f64, path: &Path) {
        let Some(color) = self.project.track(track).map(|t| t.color) else {
            return;
        };
        let Some(clip) = self.audio_clip(path, color) else {
            return;
        };
        let id = self.project.alloc_id();
        let length = clip.length;
        if let Some(t) = self.project.track_mut(track) {
            t.arrangement.push(ArrangementClip {
                id,
                start: beat.max(0.0),
                length,
                clip,
            });
            t.sort_arrangement();
        }
        self.select_clip(ClipRef::Arrangement { track, id });
        self.changed();
    }

    /// First empty Session slot of a track, adding a scene if all are full.
    fn free_slot(&mut self, track: Id) -> Option<usize> {
        let t = self.project.track(track)?;
        if let Some(s) = t.slots.iter().position(|s| s.is_none()) {
            return Some(s);
        }
        Some(self.project.add_scene())
    }

    fn new_audio_track_with(&mut self, path: &Path) {
        let id = self.add_audio_track();
        let n = self.project.tracks.len();
        if let Some(t) = self.project.track_mut(id) {
            t.name = format!("{n} {}", file_label(path));
        }
        self.import_audio(path.to_path_buf(), id, 0);
    }

    fn new_instrument_track(&mut self, name: &str, instrument: Instrument) -> Id {
        let id = self.add_track(TrackKind::Midi, instrument);
        let n = self.project.tracks.len();
        if let Some(t) = self.project.track_mut(id) {
            t.name = format!("{n} {name}");
        }
        self.detail = DetailMode::Device;
        id
    }

    /// Handles a browser item dropped (or double-clicked) onto a target.
    pub fn drop_item(&mut self, item: &BrowserItem, target: DropTarget) {
        let track = match target {
            DropTarget::NewTrack => None,
            DropTarget::Track(t)
            | DropTarget::Slot(t, _)
            | DropTarget::Arrangement(t, _)
            | DropTarget::Device(t)
            | DropTarget::Pad(t, _) => Some(t),
        };
        let kind = track.and_then(|t| self.project.track(t)).map(|t| t.kind);
        match item {
            BrowserItem::Instrument { name, instrument } => match (track, kind) {
                (Some(t), Some(TrackKind::Midi)) => {
                    self.set_instrument(t, **instrument);
                    self.selected_track = Some(t);
                    self.detail = DetailMode::Device;
                    self.status = format!("Loaded {name}");
                }
                _ => {
                    self.new_instrument_track(name, **instrument);
                }
            },
            BrowserItem::Effect(kind) => match track {
                Some(t) => self.add_device(t, *kind),
                None => match self.selected_bus() {
                    Some(b) => self.add_device(b, *kind),
                    None => self.status = "Drop effects onto a track".into(),
                },
            },
            BrowserItem::Sample(path) => match (target, kind) {
                (DropTarget::NewTrack, _) | (_, None) => self.new_audio_track_with(path),
                (DropTarget::Slot(t, scene), Some(TrackKind::Audio)) => self.import_audio(path.clone(), t, scene),
                (DropTarget::Arrangement(t, beat), Some(TrackKind::Audio)) => self.sample_to_arrangement(t, beat, path),
                (DropTarget::Track(t) | DropTarget::Device(t) | DropTarget::Pad(t, _), Some(TrackKind::Audio)) => {
                    if let Some(scene) = self.free_slot(t) {
                        self.import_audio(path.clone(), t, scene);
                    }
                }
                (DropTarget::Pad(t, pad), Some(TrackKind::Midi)) => self.sample_to_midi_track(t, path, Some(pad)),
                (_, Some(TrackKind::Midi)) => {
                    if let Some(t) = track {
                        self.sample_to_midi_track(t, path, None);
                    }
                }
            },
        }
    }

    /// Double-click in the browser: load onto the selected track (or a new
    /// one), like hot-swapping.
    pub fn load_item_on_selected(&mut self, item: &BrowserItem) {
        let target = match self.selected_track.filter(|t| self.project.track(*t).is_some()) {
            Some(t) => DropTarget::Device(t),
            None => DropTarget::NewTrack,
        };
        self.drop_item(item, target);
    }

    /// Builds a Drum Rack track from up to 16 sample files (e.g. a folder of
    /// one-shots).
    pub fn drum_rack_from_samples(&mut self, name: &str, files: &[PathBuf]) {
        let id = self.new_instrument_track(name, Instrument::DrumRack(DrumRackParams::empty()));
        for (pad, f) in files.iter().take(16).enumerate() {
            self.load_instrument_sample(id, pad, f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_library_is_written_once() {
        let dir = std::env::temp_dir().join(format!("tessera-browser-{}", std::process::id()));
        let lib = ensure_factory_library(&dir).unwrap();
        let drums: Vec<_> = std::fs::read_dir(lib.join("Drums")).unwrap().flatten().collect();
        assert!(drums.len() >= 10);
        let first = std::fs::metadata(drums[0].path()).unwrap().modified().unwrap();
        ensure_factory_library(&dir).unwrap();
        let again = std::fs::metadata(drums[0].path()).unwrap().modified().unwrap();
        assert_eq!(first, again, "not rewritten");
        let mut b = BrowserState::default();
        let listing = b.list(&lib).to_vec();
        assert!(listing.iter().all(|e| e.is_dir));
        let mut found = Vec::new();
        b.search_files(&lib, "kick", &mut found, 3);
        assert!(found.len() >= 3, "{found:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
