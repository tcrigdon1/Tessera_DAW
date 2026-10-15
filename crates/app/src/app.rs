//! Application state and the actions the UI performs on the project.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;
use tessera_engine::automation::{AutomationLane, AutomationTarget};
use tessera_engine::dsp::drum_rack::DrumRackParams;
use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::model::{
    ArrangementClip, AudioData, AudioRef, Clip, ClipContent, Id, Instrument, Project, TrackKind,
};
use tessera_engine::{analysis, demo, Command, EngineHandle};

use crate::audio::AudioOutput;
use crate::automation::AutomationUi;
use crate::plugins::PluginManager;
use crate::prefs::Preferences;
use crate::project_files::Autosave;
use crate::recording::TakeCollector;
use crate::ui;

/// Imported files at least this long are auto-warped.
pub const AUTO_WARP_MIN_SECS: f64 = 2.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Session,
    Arrangement,
}

impl View {
    /// The view the second window shows while this one is in the main window.
    pub fn other(self) -> View {
        match self {
            View::Session => View::Arrangement,
            View::Arrangement => View::Session,
        }
    }
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

/// Something that would throw away unsaved changes, waiting for the
/// "Save changes?" answer.
#[derive(Debug, Clone, PartialEq)]
pub enum PendingAction {
    Quit,
    New,
    NewFromTemplate(PathBuf),
    OpenDemo,
    /// Open a file (None: ask with the file dialog).
    Open(Option<PathBuf>),
}

pub struct TesseraApp {
    pub project: Project,
    pub engine: EngineHandle,
    /// Keeps the audio stream alive.
    pub audio: AudioOutput,
    /// User preferences (saved when changed).
    pub prefs: Preferences,
    /// Autosave, crash recovery.
    pub autosave: Autosave,
    /// Preferences window state.
    pub prefs_ui: crate::ui::prefs::PrefsUi,
    /// Export Audio dialog state.
    pub export_ui: crate::ui::export_dialog::ExportUi,
    /// "Save as Template" and other small file windows.
    pub file_dialogs: crate::ui::file_dialogs::FileDialogs,
    /// Waiting for an answer to "Save changes?".
    pub pending: Option<PendingAction>,
    /// The user confirmed quitting.
    quit_confirmed: bool,
    /// Help-menu toggles as last synced with the preferences.
    chrome_synced: (bool, bool, bool),
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub view: View,
    pub detail: DetailMode,
    pub selected_track: Option<Id>,
    pub selected_clip: Option<ClipRef>,
    pub arrangement: ArrangementState,
    pub piano: crate::ui::piano_roll::PianoRollState,
    pub keyboard_octave: i32,
    pub held_keys: Vec<(egui::Key, Id, u8)>,
    pub status: String,
    pub needs_sync: bool,
    pub peak_cache: HashMap<usize, Arc<Vec<f32>>>,
    pub show_browser: bool,
    /// View > Full Screen.
    pub full_screen: bool,
    /// The second window (View → Second Window) and where it opens.
    pub second_window: crate::ui::second_window::SecondWindow,
    pub tempo_editing: bool,
    pub automation: AutomationUi,
    /// Recorded input waiting to become clips.
    pub takes: TakeCollector,
    /// Browser categories, places, preview and drag-and-drop state.
    pub browser: crate::browser::BrowserState,
    /// Hardware MIDI ports and controller messages.
    pub midi: crate::midi::MidiIo,
    /// MIDI Map Mode (Ctrl+M), when on.
    pub midi_learn: Option<crate::midi::MidiLearn>,
    pub show_midi_prefs: bool,
    /// Session launch button held down (for Gate and Repeat clips).
    pub held_launch: Option<(Id, usize)>,
    pub(crate) midi_selected_sent: Option<Id>,
    /// Installed plug-ins and the running instances.
    pub plugins: PluginManager,
    /// Plug-in panels showing all their parameters.
    pub plugin_show_all: std::collections::HashSet<Id>,
    /// Device view selection, clipboard, folding and name windows.
    pub devices: crate::device_edit::DeviceUi,
    /// Undo history, selections and other arrangement/session workflow state.
    pub work: crate::workflow::WorkflowState,
    /// While set, the Device view is drawing this Instrument Rack chain's
    /// instrument in its track's place: instrument edits go to the chain.
    pub chain_edit: Option<(Id, Id)>,
    /// Help menu, Info View, key commands, splash and tour.
    pub chrome: crate::ui::chrome::ChromeState,
    last_title: String,
}

impl TesseraApp {
    pub fn new(cc: &eframe::CreationContext<'_>, open: Option<PathBuf>) -> Self {
        let prefs = Preferences::load();
        ui::theme::apply_theme(&cc.egui_ctx, prefs.theme);
        cc.egui_ctx.set_zoom_factor(prefs.zoom);
        let second_window = crate::ui::second_window::SecondWindow::from_prefs(&prefs);
        let (midi, midi_rx) = crate::midi::MidiIo::new();
        let (audio, engine) = crate::audio::start(&prefs.audio, midi_rx);
        let plugins = PluginManager::new(audio.sample_rate as f32);
        let mut app = TesseraApp {
            project: demo::demo_project(),
            engine,
            status: audio_status(&audio),
            audio,
            prefs,
            autosave: Autosave::start(),
            prefs_ui: Default::default(),
            export_ui: Default::default(),
            file_dialogs: Default::default(),
            pending: None,
            quit_confirmed: false,
            chrome_synced: (false, false, false),
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
            piano: Default::default(),
            keyboard_octave: 5,
            held_keys: Vec::new(),
            needs_sync: false,
            peak_cache: HashMap::new(),
            show_browser: true,
            full_screen: false,
            second_window,
            tempo_editing: false,
            automation: AutomationUi::default(),
            takes: TakeCollector::default(),
            browser: Default::default(),
            midi,
            midi_learn: None,
            show_midi_prefs: false,
            held_launch: None,
            midi_selected_sent: None,
            plugins,
            plugin_show_all: Default::default(),
            devices: Default::default(),
            work: Default::default(),
            chain_edit: None,
            chrome: Default::default(),
            last_title: String::new(),
        };
        if let Some(err) = app.browser.init() {
            app.status = err;
        }
        app.plugins.start_scan();
        if let Some(path) = open {
            app.open_path(path);
        } else {
            app.engine.load_project(&app.project, true);
            app.work.reset(&app.project);
        }
        app.select_first_clip();
        app.apply_project_defaults_to_engine();
        app.chrome.show_info = app.prefs.show_info_view;
        app.chrome.tooltips = app.prefs.tooltips;
        app.chrome.tour_done = app.prefs.tour_done;
        app.chrome_synced = (app.chrome.show_info, app.chrome.tooltips, app.chrome.tour_done);
        app
    }

    /// Saves the preferences, reporting a failure in the status bar.
    pub fn save_prefs(&mut self) {
        if let Err(e) = self.prefs.save() {
            self.status = format!("Could not save preferences: {e}");
        }
    }

    /// Restarts audio with the devices and settings in the preferences.
    /// The project, plug-ins and transport state move to the new engine.
    pub fn restart_audio(&mut self) {
        self.send(Command::Stop);
        // Close the old stream first: some drivers open a device only once.
        drop(std::mem::replace(&mut self.audio, AudioOutput::closed()));
        let midi_rx = self.midi.new_engine_ring();
        let (audio, engine) = crate::audio::start(&self.prefs.audio, midi_rx);
        self.audio = audio;
        self.engine = engine;
        self.status = audio_status(&self.audio);
        if let Some(r) = &self.audio.fallback_reason {
            self.status = format!("{} (settings not used: {r})", self.status);
        }
        self.plugins.sample_rate = self.audio.sample_rate as f32;
        self.plugins.clear();
        self.plugins.sync(&mut self.project);
        self.peak_cache.clear();
        self.takes = TakeCollector::default();
        self.engine.load_project(&self.project, true);
        self.apply_project_defaults_to_engine();
        self.needs_sync = false;
    }

    /// Commands the engine takes from the project outside the snapshot.
    fn apply_project_defaults_to_engine(&mut self) {
        let p = &self.project;
        let cmds = [
            Command::SetTempo(p.tempo),
            Command::SetTimeSignature(p.beats_per_bar),
            Command::SetMetronome(p.metronome),
            Command::SetQuantization(p.launch_quantization),
            Command::SetCountIn(p.count_in_bars),
            Command::SetLoop {
                enabled: p.loop_enabled,
                start: p.loop_start,
                length: p.loop_length,
            },
        ];
        for c in cmds {
            self.send(c);
        }
    }

    /// Plays a 440 Hz test tone for two seconds (Preferences → Audio).
    pub fn test_tone(&mut self) {
        let sr = self.audio.sample_rate;
        let n = sr as usize * 2;
        let tone: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f32 / sr as f32;
                // -12 dBFS with 10 ms fades.
                let fade = (t / 0.01).min((n - i) as f32 / sr as f32 / 0.01).min(1.0);
                0.25 * fade * (std::f32::consts::TAU * 440.0 * t).sin()
            })
            .collect();
        let data = AudioData {
            sample_rate: sr,
            left: tone.clone(),
            right: tone,
        };
        self.send(Command::PreviewSample(Arc::new(data)));
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
        self.touch();
    }

    pub fn send(&mut self, cmd: Command) {
        if !self.engine.send(cmd) {
            self.status = "Audio engine is busy; command dropped".into();
        }
    }

    pub fn flush(&mut self) {
        if self.needs_sync {
            self.needs_sync = false;
            self.plugins.sync(&mut self.project);
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
        self.learn_touch(tessera_engine::midi::mapping::MapTarget::Tempo);
        self.project.tempo = bpm.clamp(20.0, 999.0);
        self.touch();
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
        self.touch();
        self.send(cmd);
    }

    pub fn send_mixer(&mut self, track: Id) {
        self.mixer_edited(track);
        if let Some(t) = self.project.track(track) {
            let params = t.mixer;
            self.learn_mixer(track, params);
            self.touch();
            self.send(Command::SetMixer { track, params });
        }
    }

    pub fn send_instrument(&mut self, track: Id) {
        if let Some((t, chain)) = self.chain_edit.filter(|(t, _)| *t == track) {
            // An Instrument Rack chain's instrument is drawn in the track's
            // place (see `ui::devices`); route the edit to the chain.
            if let Some(instrument) = self.project.track(t).map(|t| t.instrument) {
                self.dirty = true;
                self.send(Command::SetChainInstrument {
                    track,
                    chain,
                    instrument,
                });
            }
            return;
        }
        self.instrument_edited(track);
        if let Some(t) = self.project.track(track) {
            let instrument = t.instrument;
            self.touch();
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
        self.poll_recording();
        self.poll_midi(now);
        self.poll_plugins();
        self.poll_conversion();
        let t = self.engine.shared.tempo.load();
        if !self.tempo_editing && (t - self.project.tempo).abs() > 1e-9 {
            self.project.tempo = t;
        }
        self.sync_automation(now);
    }

    // ----- tracks ------------------------------------------------------------

    pub fn add_track(&mut self, kind: TrackKind, instrument: Instrument) -> Id {
        let id = self.project.add_track(kind, instrument);
        let color = self.prefs.clip_palette.color(self.project.tracks.len() - 1);
        if let Some(t) = self.project.track_mut(id) {
            t.color = color;
        }
        self.selected_track = Some(id);
        self.changed();
        id
    }

    /// A new MIDI track: the Default MIDI Track template when one is saved,
    /// otherwise a Poly Synth.
    pub fn add_midi_track(&mut self) -> Id {
        let id = self.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        self.apply_default_track(id);
        id
    }

    pub fn add_drum_track(&mut self) -> Id {
        self.add_track(TrackKind::Midi, Instrument::DrumRack(DrumRackParams::standard()))
    }

    /// A new audio track, with the Default Audio Track template's effects
    /// when one is saved.
    pub fn add_audio_track(&mut self) -> Id {
        let id = self.add_track(TrackKind::Audio, Instrument::None);
        self.apply_default_track(id);
        id
    }

    pub fn delete_track(&mut self, id: Id) {
        self.project.remove_track(id);
        self.prune_mappings();
        if self.selected_track == Some(id) {
            self.selected_track = self.project.tracks.first().map(|t| t.id);
        }
        if self.selected_clip.map(|c| c.track()) == Some(id) {
            self.selected_clip = None;
        }
        self.changed();
    }

    pub fn set_instrument(&mut self, track: Id, instrument: Instrument) {
        if self.chain_edit.is_some_and(|(t, _)| t == track) {
            if let Some(t) = self.project.track_mut(track) {
                if t.instrument.kind_tag() != instrument.kind_tag() {
                    t.samples.clear();
                }
                t.instrument = instrument;
                self.changed();
            }
            return;
        }
        let chain_id = self.project.alloc_id();
        if let Some(t) = self.project.track_mut(track) {
            if t.kind == TrackKind::Midi {
                if t.instrument.kind_tag() != instrument.kind_tag() {
                    // Racks belong to the instrument they were made for. A new
                    // Instrument Rack wraps the old instrument in its first chain.
                    t.rack = matches!(instrument, Instrument::Rack(_)).then(|| {
                        let old = match t.instrument {
                            Instrument::Plugin(_) => Instrument::None,
                            i => i,
                        };
                        let mut chain = tessera_engine::fx::rack::RackChain::new(chain_id, old.name());
                        if matches!(old, Instrument::None) {
                            chain.name = "Chain 1".into();
                        } else {
                            chain.samples = std::mem::take(&mut t.samples);
                        }
                        chain.instrument = old;
                        tessera_engine::fx::rack::Rack::with_chain(chain)
                    });
                    // Samples belong to the old instrument's slots.
                    t.samples.clear();
                    // Instrument automation addresses parameters by index, so
                    // it means nothing to a different instrument.
                    let keep = |l: &AutomationLane| !matches!(l.target, AutomationTarget::Instrument(_));
                    t.automation.retain(keep);
                    let clips = t
                        .slots
                        .iter_mut()
                        .flatten()
                        .chain(t.arrangement.iter_mut().map(|a| &mut a.clip));
                    for c in clips {
                        c.envelopes
                            .retain(|e| !matches!(e.target, AutomationTarget::Instrument(_)));
                    }
                }
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
            ..Default::default()
        }
    }

    pub fn create_clip_in_slot(&mut self, track: Id, scene: usize) {
        let bars = self.project.beats_per_bar as f64 * self.prefs.new_clip_bars as f64;
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
        self.piano.selection.clear();
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
                ..Default::default()
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
                ..Default::default()
            });
            t.sort_arrangement();
        }
        self.select_clip(ClipRef::Arrangement { track, id });
        self.changed();
    }

    /// Loads a WAV file into a Session slot of an audio track.
    /// Wraps loaded audio for a new clip. Long files are warped to the
    /// project tempo (Ableton's "auto-warp long samples"); short one-shots
    /// play at their own speed. Returns the clip content and its length in
    /// beats.
    pub fn warped_audio(&mut self, path: &std::path::Path, data: AudioData) -> (AudioRef, f64) {
        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut audio = AudioRef::new(path.to_string_lossy(), None);
        let length = if self.prefs.auto_warp_long_samples && data.duration_secs() >= AUTO_WARP_MIN_SECS {
            let mut a = analysis::auto_warp(&data, self.project.tempo, self.project.beats_per_bar);
            if let Some(mode) = self.prefs.default_warp_mode {
                a.warp.mode = mode;
            }
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
        (audio, length)
    }

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
        let (audio, length) = self.warped_audio(&path, data);
        let clip = Clip {
            id,
            name,
            color,
            length,
            looping: true,
            gain_db: 0.0,
            content: ClipContent::Audio(audio),
            envelopes: Vec::new(),
            ..Default::default()
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

    /// Runs `action` now, or asks "Save changes?" first when the set has
    /// unsaved changes.
    pub fn request(&mut self, action: PendingAction) {
        if self.dirty {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
    }

    /// Does what was asked, without asking.
    pub fn perform(&mut self, action: PendingAction) {
        match action {
            PendingAction::Quit => self.quit_confirmed = true,
            PendingAction::New => self.new_project(),
            PendingAction::NewFromTemplate(p) => self.new_from_template(&p),
            PendingAction::OpenDemo => self.load_demo(),
            PendingAction::Open(Some(p)) => self.open_path(p),
            PendingAction::Open(None) => self.open_dialog(),
        }
    }

    /// File → New: the Default Set template if there is one, otherwise a
    /// set with a MIDI, a drum and an audio track. Preferences set the
    /// count-in and launch quantization.
    pub fn new_project(&mut self) {
        if let Some(t) = self.prefs.default_template().filter(|t| t.is_file()) {
            self.new_from_template(&t);
            return;
        }
        let mut p = Project {
            count_in_bars: self.prefs.count_in_bars,
            launch_quantization: self.prefs.launch_quantization,
            ..Project::default()
        };
        p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        p.add_track(TrackKind::Midi, Instrument::DrumRack(DrumRackParams::standard()));
        p.add_track(TrackKind::Audio, Instrument::None);
        for (i, t) in p.tracks.iter_mut().enumerate() {
            t.color = self.prefs.clip_palette.color(i);
        }
        self.load_project(p, None);
    }

    pub fn load_demo(&mut self) {
        self.load_project(demo::demo_project(), None);
    }

    /// Replaces the current set.
    pub fn load_project(&mut self, p: Project, path: Option<PathBuf>) {
        self.project = p;
        self.takes = TakeCollector::default();
        self.path = path;
        self.dirty = false;
        self.select_first_clip();
        self.peak_cache.clear();
        self.plugins.clear();
        self.plugins.sync(&mut self.project);
        self.engine.load_project(&self.project, true);
        self.apply_project_defaults_to_engine();
        self.needs_sync = false;
        self.work.reset(&self.project);
        self.refreeze_after_load();
    }

    pub fn open_path(&mut self, path: PathBuf) {
        let newer = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| tessera_engine::migrate::version_of(&v))
            .filter(|v| *v > tessera_engine::migrate::PROJECT_VERSION);
        match Project::load_from(&path) {
            Ok(p) => {
                self.status = match newer {
                    Some(v) => format!(
                        "Opened {} (saved by a newer Tessera, file version {v}; some settings may be missing)",
                        path.display()
                    ),
                    None => format!("Opened {}", path.display()),
                };
                self.prefs.add_recent(&path);
                self.save_prefs();
                self.load_project(p, Some(path));
            }
            Err(e) => {
                self.status = format!("Could not open {}: {e}", path.display());
                self.engine.load_project(&self.project, true);
            }
        }
    }

    pub fn open_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new().add_filter("Tessera project", &["tessera", "json"]);
        if let Some(dir) = &self.prefs.project_folder {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
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
        let mut dialog = rfd::FileDialog::new()
            .add_filter("Tessera project", &["tessera"])
            .set_file_name(format!("{}.tessera", self.project.name));
        if let Some(dir) = &self.prefs.project_folder {
            dialog = dialog.set_directory(dir);
        }
        if let Some(mut path) = dialog.save_file() {
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
        self.collect_recordings(&path);
        self.plugins.store_states(&mut self.project);
        match self.project.save_to(&path) {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.autosave.saved(&self.project, &path);
                self.prefs.add_recent(&path);
                self.save_prefs();
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

    /// File → Export Audio/Video: opens the export dialog.
    pub fn export_dialog(&mut self) {
        self.export_ui.open(&self.project, &self.prefs.export);
    }
}

impl eframe::App for TesseraApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        self.poll_engine(now);
        // Closing the window asks to save unsaved changes first.
        if ui.ctx().input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request(PendingAction::Quit);
        }
        if self.quit_confirmed {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let (minutes, dirty) = (self.prefs.autosave_minutes, self.dirty);
        self.autosave
            .tick(now, minutes, &self.project, self.path.as_deref(), dirty);
        ui::keyboard::handle(self, ui.ctx());
        ui::draw(self, ui);
        self.sync_chrome_prefs();
        if self.held_launch.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            self.release_launch();
        }
        self.flush();
        self.commit_history(ui.input(|i| i.pointer.any_down()), now);
        let title = format!("{}{} - Tessera", self.project.name, if self.dirty { "*" } else { "" });
        if title != self.last_title {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
        // Playheads, meters and clip progress animate continuously.
        ui.ctx().request_repaint();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.second_window.remember(&mut self.prefs);
        self.save_prefs();
        self.autosave.shutdown();
    }
}

fn audio_status(audio: &AudioOutput) -> String {
    let input = match (&audio.input_name, &audio.input_error) {
        (Some(name), _) => name.clone(),
        (None, Some(_)) => "could not be opened (see Preferences → Audio)".into(),
        (None, None) => "none".into(),
    };
    format!(
        "Audio: {} @ {} Hz · Input: {input}",
        audio.device_name, audio.sample_rate
    )
}

impl TesseraApp {
    /// The running audio device (for the status bar).
    pub fn audio_output(&self) -> &AudioOutput {
        &self.audio
    }

    /// Keeps the Help menu's toggles (Info View, tooltips, tour) and the
    /// preferences in step, whichever side changed them, and saves.
    fn sync_chrome_prefs(&mut self) {
        let chrome = (self.chrome.show_info, self.chrome.tooltips, self.chrome.tour_done);
        let prefs = (self.prefs.show_info_view, self.prefs.tooltips, self.prefs.tour_done);
        if chrome == prefs {
            self.chrome_synced = chrome;
            return;
        }
        if chrome != self.chrome_synced {
            (self.prefs.show_info_view, self.prefs.tooltips, self.prefs.tour_done) = chrome;
            self.save_prefs();
        } else {
            (self.chrome.show_info, self.chrome.tooltips, self.chrome.tour_done) = prefs;
        }
        self.chrome_synced = (self.chrome.show_info, self.chrome.tooltips, self.chrome.tour_done);
    }
}
