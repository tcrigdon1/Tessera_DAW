//! Application state and the actions the UI performs on the project.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;
use tessera_engine::dsp::drums::DrumKitParams;
use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::model::{
    ArrangementClip, AudioData, AudioRef, Clip, ClipContent, Id, Instrument, Project, TrackKind,
};
use tessera_engine::{analysis, demo, Command, EngineHandle};

use crate::audio::AudioOutput;
use crate::automation::AutomationUi;
use crate::ui;

/// Imported files at least this long are auto-warped.
pub const AUTO_WARP_MIN_SECS: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Session,
    Arrangement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailMode {
    Clip,
    Device,
}

/// Identifies a clip either in a Session slot or on the Arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipRef {
    Slot { track: Id, scene: usize },
    Arrangement { track: Id, id: Id },
}

impl ClipRef {
    pub fn track(&self) -> Id {
        match *self {
            ClipRef::Slot { track, .. } | ClipRef::Arrangement { track, .. } => track,
        }
    }
}

/// Arrangement view scroll/zoom state.
pub struct ArrangementState {
    pub px_per_beat: f32,
    pub scroll_beats: f64,
    pub scroll_y: f32,
    pub follow: bool,
    pub drag: Option<ArrDrag>,
}

#[derive(Debug, Clone, Copy)]
pub enum ArrDrag {
    Move { track: Id, id: Id, grab_offset: f64 },
    Resize { track: Id, id: Id },
    LoopMove { grab_offset: f64 },
    LoopStart,
    LoopEnd,
}

/// Piano roll state.
pub struct PianoRollState {
    pub top_pitch: i32,
    pub drag: Option<NoteDrag>,
    pub last_length: f64,
    pub selected_note: Option<usize>,
    pub centred_on: Option<ClipRef>,
    /// Note being auditioned: (track, pitch, release time).
    pub audition: Option<(Id, u8, f64)>,
}

#[derive(Debug, Clone, Copy)]
pub enum NoteDrag {
    Move {
        index: usize,
        grab_beat: f64,
        orig_start: f64,
    },
    Resize {
        index: usize,
    },
}

pub struct TesseraApp {
    pub project: Project,
    pub engine: EngineHandle,
    /// Keeps the audio stream alive.
    _audio: AudioOutput,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub view: View,
    pub detail: DetailMode,
    pub selected_track: Option<Id>,
    pub selected_clip: Option<ClipRef>,
    pub arrangement: ArrangementState,
    pub piano: PianoRollState,
    pub keyboard_octave: i32,
    pub held_keys: Vec<(egui::Key, Id, u8)>,
    pub status: String,
    pub needs_sync: bool,
    pub peak_cache: HashMap<usize, Arc<Vec<f32>>>,
    pub show_browser: bool,
    pub tempo_editing: bool,
    pub automation: AutomationUi,
    last_title: String,
}

impl TesseraApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        ui::theme::apply(&cc.egui_ctx);
        let (audio, engine) = crate::audio::start();
        let mut app = TesseraApp {
            project: demo::demo_project(),
            engine,
            status: format!("Audio: {} @ {} Hz", audio.device_name, audio.sample_rate),
            _audio: audio,
            path: None,
            dirty: false,
            view: View::Session,
            detail: DetailMode::Clip,
            selected_track: None,
            selected_clip: None,
            arrangement: ArrangementState {
                px_per_beat: 14.0,
                scroll_beats: 0.0,
                scroll_y: 0.0,
                follow: true,
                drag: None,
            },
            piano: PianoRollState {
                top_pitch: 84,
                drag: None,
                last_length: 0.25,
                selected_note: None,
                centred_on: None,
                audition: None,
            },
            keyboard_octave: 5,
            held_keys: Vec::new(),
            needs_sync: false,
            peak_cache: HashMap::new(),
            show_browser: true,
            tempo_editing: false,
            automation: AutomationUi::default(),
            last_title: String::new(),
        };
        if let Some(path) = open {
            app.open_path(path);
        } else {
            app.engine.load_project(&app.project, true);
        }
        app.select_first_clip();
        app
    }

    /// Selects the first Session clip so the detail view shows something useful.
    pub fn select_first_clip(&mut self) {
        self.selected_track = self.project.tracks.first().map(|t| t.id);
        self.selected_clip = None;
        for scene in 0..self.project.scenes.len() {
            if let Some(t) = self.project.tracks.iter().find(|t| t.slots[scene].is_some()) {
                let r = ClipRef::Slot { track: t.id, scene };
                self.select_clip(r);
                return;
            }
        }
    }

    // ----- engine sync -------------------------------------------------------

    /// Marks the project structure as changed; the engine snapshot is rebuilt
    /// at the end of the frame.
    pub fn changed(&mut self) {
        self.needs_sync = true;
        self.dirty = true;
    }

    pub fn send(&mut self, cmd: Command) {
        if !self.engine.send(cmd) {
            self.status = "Audio engine is busy; command dropped".into();
        }
    }

    pub fn flush(&mut self) {
        if self.needs_sync {
            self.needs_sync = false;
            self.engine.load_project(&self.project, false);
        }
        self.engine.collect_garbage();
    }

    // ----- transport ---------------------------------------------------------

    pub fn is_playing(&self) -> bool {
        self.engine.shared.playing.load(Ordering::Relaxed)
    }

    pub fn song_beat(&self) -> f64 {
        self.engine.shared.song_beat.load()
    }

    pub fn toggle_play(&mut self) {
        if self.is_playing() {
            self.send(Command::Stop);
        } else {
            self.send(Command::Play);
        }
    }

    pub fn set_tempo(&mut self, bpm: f64) {
        self.project.tempo = bpm.clamp(20.0, 999.0);
        self.dirty = true;
        let t = self.project.tempo;
        self.send(Command::SetTempo(t));
    }

    pub fn send_loop(&mut self) {
        let p = &self.project;
        let cmd = Command::SetLoop {
            enabled: p.loop_enabled,
            start: p.loop_start,
            length: p.loop_length,
        };
        self.dirty = true;
        self.send(cmd);
    }

    pub fn send_mixer(&mut self, track: Id) {
        self.mixer_edited(track);
        if let Some(t) = self.project.track(track) {
            let params = t.mixer;
            self.dirty = true;
            self.send(Command::SetMixer { track, params });
        }
    }

    pub fn send_instrument(&mut self, track: Id) {
        self.instrument_edited(track);
        if let Some(t) = self.project.track(track) {
            let instrument = t.instrument;
            self.dirty = true;
            self.send(Command::SetInstrument { track, instrument });
        }
    }

    /// Plays a short preview of a note on a track.
    pub fn audition(&mut self, track: Id, pitch: u8, now: f64) {
        if let Some((t, p, _)) = self.piano.audition.take() {
            self.send(Command::NoteOff { track: t, pitch: p });
        }
        self.send(Command::NoteOn {
            track,
            pitch,
            velocity: 100,
        });
        self.piano.audition = Some((track, pitch, now + 0.25));
    }

    /// Follows tempo changes made by the engine (scene tempos).
    pub fn poll_engine(&mut self, now: f64) {
        if let Some((t, p, until)) = self.piano.audition {
            if now >= until {
                self.piano.audition = None;
                self.send(Command::NoteOff { track: t, pitch: p });
            }
        }
        let t = self.engine.shared.tempo.load();
        if !self.tempo_editing && (t - self.project.tempo).abs() > 1e-9 {
            self.project.tempo = t;
        }
        self.sync_automation(now);
    }

    // ----- tracks ------------------------------------------------------------

    pub fn add_track(&mut self, kind: TrackKind, instrument: Instrument) -> Id {
        let id = self.project.add_track(kind, instrument);
        self.selected_track = Some(id);
        self.changed();
        id
    }

    pub fn add_midi_track(&mut self) -> Id {
        self.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()))
    }

    pub fn add_drum_track(&mut self) -> Id {
        self.add_track(TrackKind::Midi, Instrument::DrumKit(DrumKitParams::default()))
    }

    pub fn add_audio_track(&mut self) -> Id {
        self.add_track(TrackKind::Audio, Instrument::None)
    }

    pub fn delete_track(&mut self, id: Id) {
        self.project.remove_track(id);
        if self.selected_track == Some(id) {
            self.selected_track = self.project.tracks.first().map(|t| t.id);
        }
        if self.selected_clip.map(|c| c.track()) == Some(id) {
            self.selected_clip = None;
        }
        self.changed();
    }

    pub fn set_instrument(&mut self, track: Id, instrument: Instrument) {
        if let Some(t) = self.project.track_mut(track) {
            if t.kind == TrackKind::Midi {
                t.instrument = instrument;
                self.changed();
            }
        }
    }

    // ----- clips -------------------------------------------------------------

    pub fn clip(&self, r: ClipRef) -> Option<&Clip> {
        let t = self.project.track(r.track())?;
        match r {
            ClipRef::Slot { scene, .. } => t.slots.get(scene)?.as_ref(),
            ClipRef::Arrangement { id, .. } => t.arrangement.iter().find(|a| a.id == id).map(|a| &a.clip),
        }
    }

    pub fn clip_mut(&mut self, r: ClipRef) -> Option<&mut Clip> {
        let t = self.project.track_mut(r.track())?;
        match r {
            ClipRef::Slot { scene, .. } => t.slots.get_mut(scene)?.as_mut(),
            ClipRef::Arrangement { id, .. } => t.arrangement.iter_mut().find(|a| a.id == id).map(|a| &mut a.clip),
        }
    }

    pub fn new_midi_clip(&mut self, length: f64) -> Clip {
        let id = self.project.alloc_id();
        Clip {
            id,
            name: "MIDI Clip".into(),
            color: [0, 0, 0],
            length,
            looping: true,
            gain_db: 0.0,
            content: ClipContent::Midi { notes: Vec::new() },
            envelopes: Vec::new(),
        }
    }

    pub fn create_clip_in_slot(&mut self, track: Id, scene: usize) {
        let bars = self.project.beats_per_bar as f64;
        let mut clip = self.new_midi_clip(bars);
        let Some(t) = self.project.track(track) else { return };
        if t.kind != TrackKind::Midi {
            return;
        }
        clip.color = t.color;
        if let Some(slot) = self.project.track_mut(track).and_then(|t| t.slots.get_mut(scene)) {
            *slot = Some(clip);
        }
        self.select_clip(ClipRef::Slot { track, scene });
        self.detail = DetailMode::Clip;
        self.changed();
    }

    pub fn select_clip(&mut self, r: ClipRef) {
        self.selected_clip = Some(r);
        self.selected_track = Some(r.track());
        self.piano.selected_note = None;
    }

    pub fn delete_clip(&mut self, r: ClipRef) {
        if let Some(t) = self.project.track_mut(r.track()) {
            match r {
                ClipRef::Slot { scene, .. } => {
                    if let Some(s) = t.slots.get_mut(scene) {
                        *s = None;
                    }
                }
                ClipRef::Arrangement { id, .. } => t.arrangement.retain(|a| a.id != id),
            }
        }
        if self.selected_clip == Some(r) {
            self.selected_clip = None;
        }
        self.changed();
    }

    pub fn duplicate_slot(&mut self, track: Id, scene: usize) {
        let Some(mut clip) = self.clip(ClipRef::Slot { track, scene }).cloned() else {
            return;
        };
        clip.id = self.project.alloc_id();
        let Some(t) = self.project.track_mut(track) else { return };
        // Next empty slot below, adding a scene if needed.
        let target = (scene + 1..t.slots.len()).find(|&s| t.slots[s].is_none());
        let target = match target {
            Some(s) => s,
            None => {
                self.project.add_scene();
                self.project.scenes.len() - 1
            }
        };
        if let Some(t) = self.project.track_mut(track) {
            t.slots[target] = Some(clip);
        }
        self.select_clip(ClipRef::Slot { track, scene: target });
        self.changed();
    }

    /// Copies a Session clip onto the Arrangement at the playhead (bar aligned).
    pub fn slot_to_arrangement(&mut self, track: Id, scene: usize) {
        let Some(mut clip) = self.clip(ClipRef::Slot { track, scene }).cloned() else {
            return;
        };
        clip.id = self.project.alloc_id();
        let bpb = self.project.beats_per_bar as f64;
        let start = (self.song_beat() / bpb).floor() * bpb;
        let id = self.project.alloc_id();
        let length = clip.length;
        if let Some(t) = self.project.track_mut(track) {
            t.arrangement.push(ArrangementClip {
                id,
                start,
                length,
                clip,
            });
            t.sort_arrangement();
        }
        self.status = "Clip added to the Arrangement".into();
        self.changed();
    }

    pub fn create_arrangement_clip(&mut self, track: Id, start: f64) {
        let bpb = self.project.beats_per_bar as f64;
        let mut clip = self.new_midi_clip(bpb);
        let Some(t) = self.project.track(track) else { return };
        if t.kind != TrackKind::Midi {
            return;
        }
        clip.color = t.color;
        let id = self.project.alloc_id();
        if let Some(t) = self.project.track_mut(track) {
            t.arrangement.push(ArrangementClip {
                id,
                start,
                length: bpb,
                clip,
            });
            t.sort_arrangement();
        }
        self.select_clip(ClipRef::Arrangement { track, id });
        self.changed();
    }

    /// Loads a WAV file into a Session slot of an audio track.
    pub fn import_audio(&mut self, path: PathBuf, track: Id, scene: usize) {
        let data = match AudioData::load_wav(&path) {
            Ok(d) => d,
            Err(e) => {
                self.status = format!("Could not load {}: {e}", path.display());
                return;
            }
        };
        let Some(t) = self.project.track(track) else { return };
        if t.kind != TrackKind::Audio {
            self.status = "Audio files go on audio tracks".into();
            return;
        }
        let color = t.color;
        let id = self.project.alloc_id();
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut audio = AudioRef::new(path.to_string_lossy(), None);
        // Long files are warped to the project tempo (Ableton's "auto-warp
        // long samples"); short one-shots play at their own speed.
        let length = if data.duration_secs() >= AUTO_WARP_MIN_SECS {
            let a = analysis::auto_warp(&data, self.project.tempo, self.project.beats_per_bar);
            self.status = format!(
                "Warped {name} at {:.2} BPM ({}), {} beats",
                a.warp.bpm,
                a.warp.mode.label(),
                a.length
            );
            audio.warp = a.warp;
            a.length
        } else {
            // Length in beats at the current tempo, rounded to a 16th.
            audio.warp.transients = analysis::detect_transients(&data);
            audio.warp.bpm = self.project.tempo;
            (data.duration_secs() * self.project.tempo / 60.0 * 4.0)
                .round()
                .max(1.0)
                / 4.0
        };
        audio.data = Some(Arc::new(data));
        let clip = Clip {
            id,
            name,
            color,
            length,
            looping: true,
            gain_db: 0.0,
            content: ClipContent::Audio(audio),
            envelopes: Vec::new(),
        };
        if let Some(slot) = self.project.track_mut(track).and_then(|t| t.slots.get_mut(scene)) {
            *slot = Some(clip);
        }
        self.select_clip(ClipRef::Slot { track, scene });
        self.changed();
    }

    pub fn waveform_peaks(&mut self, data: &Arc<AudioData>) -> Arc<Vec<f32>> {
        let key = Arc::as_ptr(data) as usize;
        self.peak_cache
            .entry(key)
            .or_insert_with(|| Arc::new(data.peaks(2048)))
            .clone()
    }

    // ----- files -------------------------------------------------------------

    pub fn new_project(&mut self) {
        let mut p = Project::default();
        p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        p.add_track(TrackKind::Midi, Instrument::DrumKit(DrumKitParams::default()));
        p.add_track(TrackKind::Audio, Instrument::None);
        self.load(p, None);
    }

    pub fn load_demo(&mut self) {
        self.load(demo::demo_project(), None);
    }

    fn load(&mut self, p: Project, path: Option<PathBuf>) {
        self.project = p;
        self.path = path;
        self.dirty = false;
        self.select_first_clip();
        self.peak_cache.clear();
        self.engine.load_project(&self.project, true);
        self.needs_sync = false;
    }

    pub fn open_path(&mut self, path: PathBuf) {
        match Project::load_from(&path) {
            Ok(p) => {
                self.status = format!("Opened {}", path.display());
                self.load(p, Some(path));
            }
            Err(e) => {
                self.status = format!("Could not open {}: {e}", path.display());
                self.engine.load_project(&self.project, true);
            }
        }
    }

    pub fn open_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Tessera project", &["tessera", "json"])
            .pick_file()
        {
            self.open_path(path);
        }
    }

    pub fn save(&mut self) {
        match self.path.clone() {
            Some(p) => self.save_to(p),
            None => self.save_as(),
        }
    }

    pub fn save_as(&mut self) {
        if let Some(mut path) = rfd::FileDialog::new()
            .add_filter("Tessera project", &["tessera"])
            .set_file_name(format!("{}.tessera", self.project.name))
            .save_file()
        {
            if path.extension().is_none() {
                path.set_extension("tessera");
            }
            self.save_to(path);
        }
    }

    fn save_to(&mut self, path: PathBuf) {
        if let Some(stem) = path.file_stem() {
            self.project.name = stem.to_string_lossy().to_string();
        }
        match self.project.save_to(&path) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.path = Some(path);
                self.dirty = false;
            }
            Err(e) => self.status = format!("Save failed: {e}"),
        }
    }

    pub fn import_audio_dialog(&mut self, track: Id, scene: usize) {
        if let Some(path) = rfd::FileDialog::new().add_filter("WAV audio", &["wav"]).pick_file() {
            self.import_audio(path, track, scene);
        }
    }

    pub fn export_dialog(&mut self) {
        let beats = self.project.arrangement_length().max(self.project.beats_per_bar as f64);
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV audio", &["wav"])
            .set_file_name(format!("{}.wav", self.project.name))
            .save_file()
        {
            let sr = 44100;
            let samples = tessera_engine::render::render_arrangement(&self.project, sr, beats);
            match tessera_engine::render::write_wav(&path, &samples, sr) {
                Ok(()) => self.status = format!("Exported {}", path.display()),
                Err(e) => self.status = format!("Export failed: {e}"),
            }
        }
    }
}

impl eframe::App for TesseraApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        self.poll_engine(now);
        ui::keyboard::handle(self, ui.ctx());
        ui::draw(self, ui);
        self.flush();
        let title = format!("{}{} - Tessera", self.project.name, if self.dirty { "*" } else { "" });
        if title != self.last_title {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        // Playheads, meters and clip progress animate continuously.
        ui.ctx().request_repaint();
    }
}
