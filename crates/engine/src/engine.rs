//! The real-time audio engine.
//!
//! # Threading model
//!
//! * The UI thread owns the [`Project`] document. Whenever its structure
//!   changes (tracks, clips, notes) the UI compiles it into an immutable
//!   [`EngineProject`] and sends it with [`Command::LoadProject`].
//! * The audio thread owns the [`Engine`]. On receiving a new snapshot it moves
//!   the per-track runtime state (playing voices, launched clips) from the old
//!   snapshot into the new one and sends the old snapshot back to the UI to be
//!   dropped there. The audio thread never allocates or frees memory and never
//!   takes a lock.
//! * Frequent small changes (transport, fader moves, clip launches) are sent as
//!   individual [`Command`]s.
//! * The engine publishes playback state (playhead, meters, which clips are
//!   playing) through [`SharedState`], made of atomics the UI can read at any
//!   time.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::atomic::{AtomicF32, AtomicF64};
use crate::dsp::InstrumentRuntime;
use crate::model::{
    db_to_gain, pan_gains, AudioData, Clip, ClipContent, Id, Instrument, MixerParams, Note, Project, TrackKind,
};
use crate::time::Quantization;
use crate::{MAX_BLOCK, MAX_TRACKS};

const MAX_EVENTS: usize = 2048;
const MAX_PREVIEW: usize = 64;
/// Fade applied at audio clip edges to avoid clicks, in seconds.
const AUDIO_FADE_SECS: f64 = 0.003;

/// Messages from the UI to the audio thread.
pub enum Command {
    /// Start playback from the start marker.
    Play,
    /// Continue playback from where it stopped.
    Continue,
    /// Stop playback. When already stopped, move the start marker to zero.
    Stop,
    /// Move the playhead and start marker (in beats).
    SetPosition(f64),
    SetTempo(f64),
    SetTimeSignature(u32),
    SetLoop {
        enabled: bool,
        start: f64,
        length: f64,
    },
    SetMetronome(bool),
    SetQuantization(Quantization),
    LaunchClip {
        track: Id,
        slot: usize,
    },
    StopTrack(Id),
    LaunchScene(usize),
    StopAllClips,
    BackToArrangement,
    SetMixer {
        track: Id,
        params: MixerParams,
    },
    SetMaster(MixerParams),
    SetInstrument {
        track: Id,
        instrument: Instrument,
    },
    /// Audition a note on a track's instrument (computer keyboard, piano roll).
    NoteOn {
        track: Id,
        pitch: u8,
        velocity: u8,
    },
    NoteOff {
        track: Id,
        pitch: u8,
    },
    /// Replace the project snapshot.
    LoadProject(Box<EngineProject>),
    /// Silence every voice immediately.
    Panic,
}

/// An immutable, audio-thread friendly copy of the project.
pub struct EngineProject {
    pub tempo: f64,
    pub beats_per_bar: u32,
    pub quantization: Quantization,
    pub metronome: bool,
    pub loop_enabled: bool,
    pub loop_start: f64,
    pub loop_length: f64,
    pub master: MixerParams,
    pub tracks: Vec<EngineTrack>,
    pub scene_tempos: Vec<Option<f64>>,
    /// When set, the engine takes tempo, loop and metronome settings from this
    /// snapshot and rewinds the transport (used when opening a project).
    pub reset_transport: bool,
}

pub struct EngineTrack {
    pub id: Id,
    pub kind: TrackKind,
    pub instrument: Instrument,
    pub mixer: MixerParams,
    pub slots: Vec<Option<EngineClip>>,
    pub arrangement: Vec<EngineArrangementClip>,
    rt: Box<TrackRuntime>,
}

#[derive(Clone)]
pub struct EngineClip {
    pub length: f64,
    pub looping: bool,
    pub gain: f32,
    pub content: EngineContent,
}

#[derive(Clone)]
pub enum EngineContent {
    Midi(Arc<[Note]>),
    Audio(Option<Arc<AudioData>>),
}

#[derive(Clone)]
pub struct EngineArrangementClip {
    pub start: f64,
    pub length: f64,
    pub clip: EngineClip,
}

impl EngineClip {
    fn from_model(c: &Clip) -> Self {
        let content = match &c.content {
            ClipContent::Midi { notes } => {
                let mut notes = notes.clone();
                notes.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
                EngineContent::Midi(notes.into())
            }
            ClipContent::Audio(a) => EngineContent::Audio(a.data.clone()),
        };
        EngineClip {
            length: c.length.max(1.0 / 64.0),
            looping: c.looping,
            gain: db_to_gain(c.gain_db),
            content,
        }
    }
}

impl EngineProject {
    /// Compiles the project document. Runs on the UI thread (allocates).
    pub fn from_model(p: &Project, sample_rate: f32) -> Self {
        let tracks = p
            .tracks
            .iter()
            .take(MAX_TRACKS)
            .map(|t| EngineTrack {
                id: t.id,
                kind: t.kind,
                instrument: t.instrument,
                mixer: t.mixer,
                slots: t.slots.iter().map(|s| s.as_ref().map(EngineClip::from_model)).collect(),
                arrangement: t
                    .arrangement
                    .iter()
                    .map(|a| EngineArrangementClip {
                        start: a.start,
                        length: a.length,
                        clip: EngineClip::from_model(&a.clip),
                    })
                    .collect(),
                rt: Box::new(TrackRuntime::new(&t.instrument, sample_rate)),
            })
            .collect();
        EngineProject {
            tempo: p.tempo,
            beats_per_bar: p.beats_per_bar,
            quantization: p.launch_quantization,
            metronome: p.metronome,
            loop_enabled: p.loop_enabled,
            loop_start: p.loop_start,
            loop_length: p.loop_length,
            master: p.master,
            tracks,
            scene_tempos: p.scenes.iter().map(|s| s.tempo).collect(),
            reset_transport: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum EventKind {
    // Order matters: at the same frame, offs are applied before ons.
    AllOff,
    Off(u8),
    On(u8, u8),
}

#[derive(Clone, Copy, Debug)]
struct MidiEvent {
    frame: u32,
    kind: EventKind,
}

impl MidiEvent {
    fn order(&self) -> (u32, u8) {
        let k = match self.kind {
            EventKind::AllOff => 0,
            EventKind::Off(_) => 1,
            EventKind::On(..) => 2,
        };
        (self.frame, k)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum QueuedAction {
    Launch(usize),
    Stop,
}

#[derive(Clone, Copy, Debug)]
struct Queued {
    action: QueuedAction,
    /// Session beat at which the action happens.
    at: f64,
}

/// Per-track state owned by the audio thread.
struct TrackRuntime {
    instrument: InstrumentRuntime,
    /// Session slot currently playing.
    playing: Option<usize>,
    /// Session beat at which the playing clip was launched.
    launch_beat: f64,
    queued: Option<Queued>,
    /// True while a Session clip overrides the Arrangement for this track.
    session_active: bool,
    /// Arrangement playback jumped (seek, loop); release held notes.
    arrangement_reset: bool,
    events: Vec<MidiEvent>,
    preview: Vec<(u8, u8, bool)>,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    gain_l: f32,
    gain_r: f32,
    meter_l: f32,
    meter_r: f32,
    clip_pos: f64,
}

impl TrackRuntime {
    fn new(instrument: &Instrument, sample_rate: f32) -> Self {
        Self {
            instrument: InstrumentRuntime::new(instrument, sample_rate),
            playing: None,
            launch_beat: 0.0,
            queued: None,
            session_active: false,
            arrangement_reset: false,
            events: Vec::with_capacity(MAX_EVENTS),
            preview: Vec::with_capacity(MAX_PREVIEW),
            buf_l: vec![0.0; MAX_BLOCK],
            buf_r: vec![0.0; MAX_BLOCK],
            gain_l: 0.0,
            gain_r: 0.0,
            meter_l: 0.0,
            meter_r: 0.0,
            clip_pos: 0.0,
        }
    }

    #[inline]
    fn push(&mut self, frame: usize, kind: EventKind) {
        if self.events.len() < self.events.capacity() {
            self.events.push(MidiEvent {
                frame: frame as u32,
                kind,
            });
        }
    }
}

/// Per-track values published to the UI.
#[derive(Default)]
pub struct TrackShared {
    pub id: AtomicU64,
    pub meter_l: AtomicF32,
    pub meter_r: AtomicF32,
    /// Playing slot, or -1.
    pub playing_slot: AtomicI32,
    /// Queued slot, -1 for none, -2 for a queued stop.
    pub queued_slot: AtomicI32,
    /// Position inside the playing session clip, in beats.
    pub clip_position: AtomicF64,
    pub session_active: AtomicBool,
}

/// State the audio thread publishes for the UI.
pub struct SharedState {
    pub sample_rate: f32,
    pub playing: AtomicBool,
    pub song_beat: AtomicF64,
    pub tempo: AtomicF64,
    pub cpu_load: AtomicF32,
    pub master_meter_l: AtomicF32,
    pub master_meter_r: AtomicF32,
    pub tracks: Vec<TrackShared>,
}

impl SharedState {
    fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            playing: AtomicBool::new(false),
            song_beat: AtomicF64::new(0.0),
            tempo: AtomicF64::new(120.0),
            cpu_load: AtomicF32::new(0.0),
            master_meter_l: AtomicF32::new(0.0),
            master_meter_r: AtomicF32::new(0.0),
            tracks: (0..MAX_TRACKS)
                .map(|_| {
                    let t = TrackShared::default();
                    t.playing_slot.store(-1, Ordering::Relaxed);
                    t.queued_slot.store(-1, Ordering::Relaxed);
                    t
                })
                .collect(),
        }
    }

    /// Finds the published state for a track id.
    pub fn track(&self, index: usize) -> Option<&TrackShared> {
        self.tracks.get(index)
    }
}

/// The UI side of the engine.
pub struct EngineHandle {
    commands: Producer<Command>,
    garbage: Consumer<Box<EngineProject>>,
    pub shared: Arc<SharedState>,
}

impl EngineHandle {
    /// Sends a command. Returns false if the queue was full.
    pub fn send(&mut self, cmd: Command) -> bool {
        self.commands.push(cmd).is_ok()
    }

    /// Compiles and sends a project snapshot.
    pub fn load_project(&mut self, p: &Project, reset_transport: bool) {
        let mut snap = EngineProject::from_model(p, self.shared.sample_rate);
        snap.reset_transport = reset_transport;
        self.send(Command::LoadProject(Box::new(snap)));
    }

    /// Drops old snapshots the audio thread has handed back.
    pub fn collect_garbage(&mut self) {
        while let Ok(old) = self.garbage.pop() {
            drop(old);
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.shared.sample_rate
    }
}

/// The audio-thread side of the engine.
pub struct Engine {
    sr: f64,
    project: Option<Box<EngineProject>>,
    commands: Consumer<Command>,
    garbage: Producer<Box<EngineProject>>,
    shared: Arc<SharedState>,

    playing: bool,
    song_beat: f64,
    session_beat: f64,
    start_marker: f64,
    tempo: f64,
    beats_per_bar: u32,
    quantization: Quantization,
    metronome: bool,
    loop_enabled: bool,
    loop_start: f64,
    loop_length: f64,
    master: MixerParams,

    master_l: Vec<f32>,
    master_r: Vec<f32>,
    master_gain_l: f32,
    master_gain_r: f32,
    master_meter_l: f32,
    master_meter_r: f32,

    last_click: i64,
    click_env: f32,
    click_phase: f32,
    click_freq: f32,
}

impl Engine {
    pub fn new(sample_rate: f32) -> (Engine, EngineHandle) {
        let (cmd_tx, cmd_rx) = RingBuffer::new(1024);
        let (gc_tx, gc_rx) = RingBuffer::new(256);
        let shared = Arc::new(SharedState::new(sample_rate));
        let engine = Engine {
            sr: sample_rate as f64,
            project: None,
            commands: cmd_rx,
            garbage: gc_tx,
            shared: shared.clone(),
            playing: false,
            song_beat: 0.0,
            session_beat: 0.0,
            start_marker: 0.0,
            tempo: 120.0,
            beats_per_bar: 4,
            quantization: Quantization::Bar,
            metronome: false,
            loop_enabled: false,
            loop_start: 0.0,
            loop_length: 16.0,
            master: MixerParams::default(),
            master_l: vec![0.0; MAX_BLOCK],
            master_r: vec![0.0; MAX_BLOCK],
            master_gain_l: 1.0,
            master_gain_r: 1.0,
            master_meter_l: 0.0,
            master_meter_r: 0.0,
            last_click: -1,
            click_env: 0.0,
            click_phase: 0.0,
            click_freq: 1000.0,
        };
        let handle = EngineHandle {
            commands: cmd_tx,
            garbage: gc_rx,
            shared,
        };
        (engine, handle)
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn song_beat(&self) -> f64 {
        self.song_beat
    }

    pub fn tempo(&self) -> f64 {
        self.tempo
    }

    fn beats_per_frame(&self) -> f64 {
        self.tempo / 60.0 / self.sr
    }

    fn retire(&mut self, old: Box<EngineProject>) {
        if let Err(rtrb::PushError::Full(old)) = self.garbage.push(old) {
            // Never free on the audio thread; leaking is the lesser evil and
            // only happens if the UI stops collecting garbage.
            std::mem::forget(old);
        }
    }

    fn track_mut(&mut self, id: Id) -> Option<&mut EngineTrack> {
        self.project.as_mut()?.tracks.iter_mut().find(|t| t.id == id)
    }

    fn for_each_track(&mut self, mut f: impl FnMut(&mut EngineTrack)) {
        if let Some(p) = self.project.as_mut() {
            for t in p.tracks.iter_mut() {
                f(t);
            }
        }
    }

    fn start_transport(&mut self, from: f64) {
        self.playing = true;
        self.song_beat = from.max(0.0);
        self.session_beat = self.song_beat;
        self.last_click = self.song_beat.ceil() as i64 - 1;
        self.for_each_track(|t| t.rt.arrangement_reset = true);
    }

    fn stop_transport(&mut self) {
        self.playing = false;
        self.for_each_track(|t| {
            t.rt.playing = None;
            t.rt.queued = None;
            t.rt.instrument.all_notes_off();
        });
    }

    fn handle_command(&mut self, cmd: Command) {
        match cmd {
            Command::Play => {
                if !self.playing {
                    self.start_transport(self.start_marker);
                }
            }
            Command::Continue => {
                if !self.playing {
                    self.start_transport(self.song_beat);
                }
            }
            Command::Stop => {
                if self.playing {
                    self.stop_transport();
                    self.song_beat = self.start_marker;
                } else {
                    self.start_marker = 0.0;
                    self.song_beat = 0.0;
                    self.for_each_track(|t| {
                        t.rt.session_active = false;
                    });
                }
            }
            Command::SetPosition(b) => {
                let b = b.max(0.0);
                self.start_marker = b;
                self.song_beat = b;
                self.last_click = b.ceil() as i64 - 1;
                self.for_each_track(|t| t.rt.arrangement_reset = true);
            }
            Command::SetTempo(t) => self.tempo = t.clamp(20.0, 999.0),
            Command::SetTimeSignature(n) => self.beats_per_bar = n.clamp(1, 32),
            Command::SetLoop { enabled, start, length } => {
                self.loop_enabled = enabled;
                self.loop_start = start.max(0.0);
                self.loop_length = length.max(0.25);
            }
            Command::SetMetronome(on) => self.metronome = on,
            Command::SetQuantization(q) => self.quantization = q,
            Command::LaunchClip { track, slot } => {
                if !self.playing {
                    self.start_transport(self.start_marker);
                }
                let at = self
                    .quantization
                    .next_boundary(self.session_beat, self.beats_per_bar as f64);
                if let Some(t) = self.track_mut(track) {
                    t.rt.queued = Some(Queued {
                        action: QueuedAction::Launch(slot),
                        at,
                    });
                }
            }
            Command::StopTrack(track) => {
                let at = self
                    .quantization
                    .next_boundary(self.session_beat, self.beats_per_bar as f64);
                if let Some(t) = self.track_mut(track) {
                    if t.rt.playing.is_some() || t.rt.queued.is_some() {
                        t.rt.queued = Some(Queued {
                            action: QueuedAction::Stop,
                            at,
                        });
                    }
                }
            }
            Command::LaunchScene(scene) => {
                if let Some(tempo) = self
                    .project
                    .as_ref()
                    .and_then(|p| p.scene_tempos.get(scene).copied().flatten())
                {
                    self.tempo = tempo.clamp(20.0, 999.0);
                }
                if !self.playing {
                    self.start_transport(self.start_marker);
                }
                let at = self
                    .quantization
                    .next_boundary(self.session_beat, self.beats_per_bar as f64);
                self.for_each_track(|t| {
                    let has_clip = t.slots.get(scene).map(|s| s.is_some()).unwrap_or(false);
                    if has_clip {
                        t.rt.queued = Some(Queued {
                            action: QueuedAction::Launch(scene),
                            at,
                        });
                    } else if t.rt.playing.is_some() || t.rt.queued.is_some() {
                        t.rt.queued = Some(Queued {
                            action: QueuedAction::Stop,
                            at,
                        });
                    }
                });
            }
            Command::StopAllClips => {
                let at = self
                    .quantization
                    .next_boundary(self.session_beat, self.beats_per_bar as f64);
                self.for_each_track(|t| {
                    if t.rt.playing.is_some() || t.rt.queued.is_some() {
                        t.rt.queued = Some(Queued {
                            action: QueuedAction::Stop,
                            at,
                        });
                    }
                });
            }
            Command::BackToArrangement => {
                self.for_each_track(|t| {
                    if t.rt.session_active {
                        t.rt.session_active = false;
                        t.rt.playing = None;
                        t.rt.queued = None;
                        t.rt.instrument.all_notes_off();
                        t.rt.arrangement_reset = true;
                    }
                });
            }
            Command::SetMixer { track, params } => {
                if let Some(t) = self.track_mut(track) {
                    t.mixer = params;
                }
            }
            Command::SetMaster(m) => self.master = m,
            Command::SetInstrument { track, instrument } => {
                if let Some(t) = self.track_mut(track) {
                    if t.instrument.kind_tag() == instrument.kind_tag() {
                        t.instrument = instrument;
                    }
                }
            }
            Command::NoteOn { track, pitch, velocity } => {
                if let Some(t) = self.track_mut(track) {
                    if t.rt.preview.len() < MAX_PREVIEW {
                        t.rt.preview.push((pitch, velocity, true));
                    }
                }
            }
            Command::NoteOff { track, pitch } => {
                if let Some(t) = self.track_mut(track) {
                    if t.rt.preview.len() < MAX_PREVIEW {
                        t.rt.preview.push((pitch, 0, false));
                    }
                }
            }
            Command::LoadProject(mut new) => {
                if let Some(old) = self.project.as_mut() {
                    for t in new.tracks.iter_mut() {
                        if let Some(o) = old.tracks.iter_mut().find(|o| o.id == t.id) {
                            if o.rt.instrument.kind_tag() == t.instrument.kind_tag() {
                                std::mem::swap(&mut o.rt, &mut t.rt);
                                if let Some(s) = t.rt.playing {
                                    if t.slots.get(s).map(|c| c.is_none()).unwrap_or(true) {
                                        t.rt.playing = None;
                                        t.rt.instrument.all_notes_off();
                                    }
                                }
                            }
                        }
                    }
                }
                self.master = new.master;
                if new.reset_transport {
                    self.tempo = new.tempo;
                    self.beats_per_bar = new.beats_per_bar;
                    self.quantization = new.quantization;
                    self.metronome = new.metronome;
                    self.loop_enabled = new.loop_enabled;
                    self.loop_start = new.loop_start;
                    self.loop_length = new.loop_length;
                    self.playing = false;
                    self.song_beat = 0.0;
                    self.start_marker = 0.0;
                    self.session_beat = 0.0;
                    for t in new.tracks.iter_mut() {
                        t.rt.instrument.panic();
                    }
                }
                if let Some(old) = self.project.replace(new) {
                    self.retire(old);
                }
            }
            Command::Panic => {
                self.for_each_track(|t| {
                    t.rt.instrument.panic();
                });
            }
        }
    }

    /// Renders interleaved stereo audio into `out` (length = frames * 2).
    pub fn process(&mut self, out: &mut [f32]) {
        let started = Instant::now();
        while let Ok(cmd) = self.commands.pop() {
            self.handle_command(cmd);
        }
        let frames = out.len() / 2;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(MAX_BLOCK);
            self.render_block(n);
            for i in 0..n {
                out[(done + i) * 2] = self.master_l[i].clamp(-1.0, 1.0);
                out[(done + i) * 2 + 1] = self.master_r[i].clamp(-1.0, 1.0);
            }
            done += n;
        }
        self.publish();
        if frames > 0 {
            let budget = frames as f64 / self.sr;
            let load = started.elapsed().as_secs_f64() / budget;
            let prev = self.shared.cpu_load.load() as f64;
            self.shared.cpu_load.store((prev * 0.9 + load * 0.1) as f32);
        }
    }

    fn publish(&self) {
        let s = &self.shared;
        s.playing.store(self.playing, Ordering::Relaxed);
        s.song_beat.store(self.song_beat);
        s.tempo.store(self.tempo);
        s.master_meter_l.store(self.master_meter_l);
        s.master_meter_r.store(self.master_meter_r);
        if let Some(p) = self.project.as_ref() {
            for (i, t) in p.tracks.iter().enumerate() {
                let ts = &s.tracks[i];
                ts.id.store(t.id, Ordering::Relaxed);
                ts.meter_l.store(t.rt.meter_l);
                ts.meter_r.store(t.rt.meter_r);
                ts.playing_slot
                    .store(t.rt.playing.map(|x| x as i32).unwrap_or(-1), Ordering::Relaxed);
                ts.queued_slot.store(
                    match t.rt.queued {
                        None => -1,
                        Some(Queued {
                            action: QueuedAction::Stop,
                            ..
                        }) => -2,
                        Some(Queued {
                            action: QueuedAction::Launch(s),
                            ..
                        }) => s as i32,
                    },
                    Ordering::Relaxed,
                );
                ts.clip_position.store(t.rt.clip_pos);
                ts.session_active.store(t.rt.session_active, Ordering::Relaxed);
            }
        }
    }

    fn render_block(&mut self, n: usize) {
        self.master_l[..n].fill(0.0);
        self.master_r[..n].fill(0.0);
        let mut pos = 0;
        while pos < n {
            let remaining = n - pos;
            let mut len = remaining;
            let mut loops = false;
            if self.playing && self.loop_enabled && self.loop_length > 0.0 {
                let loop_end = self.loop_start + self.loop_length;
                let bpf = self.beats_per_frame();
                let seg_end = self.song_beat + remaining as f64 * bpf;
                if self.song_beat < loop_end && seg_end >= loop_end {
                    len = (((loop_end - self.song_beat) / bpf).ceil() as usize).clamp(1, remaining);
                    loops = true;
                }
            }
            self.render_segment(pos, len);
            if loops {
                let loop_end = self.loop_start + self.loop_length;
                self.song_beat = self.loop_start + (self.song_beat - loop_end).max(0.0);
                self.last_click = self.song_beat.ceil() as i64 - 1;
                self.for_each_track(|t| t.rt.arrangement_reset = true);
            }
            pos += len;
        }
        self.finish_master(n);
    }

    /// Renders `frames` frames into the master bus starting at `offset`.
    fn render_segment(&mut self, offset: usize, frames: usize) {
        let bpf = self.beats_per_frame();
        let playing = self.playing;
        let song0 = self.song_beat;
        let session0 = self.session_beat;
        let tempo = self.tempo;
        let sr = self.sr;
        let meter_decay = (-(frames as f64) / sr / 0.25).exp() as f32;

        if let Some(project) = self.project.as_mut() {
            let any_solo = project.tracks.iter().any(|t| t.mixer.solo);
            for track in project.tracks.iter_mut() {
                let EngineTrack {
                    instrument,
                    mixer,
                    slots,
                    arrangement,
                    rt,
                    ..
                } = track;
                rt.buf_l[..frames].fill(0.0);
                rt.buf_r[..frames].fill(0.0);
                rt.events.clear();

                for (pitch, vel, on) in rt.preview.drain(..) {
                    let kind = if on {
                        EventKind::On(pitch, vel)
                    } else {
                        EventKind::Off(pitch)
                    };
                    // Can't call rt.push while draining; events is a separate field.
                    if rt.events.len() < rt.events.capacity() {
                        rt.events.push(MidiEvent { frame: 0, kind });
                    }
                }

                if playing {
                    render_session(rt, slots, session0, frames, bpf, tempo, sr);
                    if !rt.session_active {
                        if rt.arrangement_reset {
                            rt.push(0, EventKind::AllOff);
                            rt.arrangement_reset = false;
                        }
                        render_arrangement(rt, arrangement, song0, frames, bpf, tempo, sr);
                    }
                }

                // Instrument rendering, split at event boundaries.
                rt.events.sort_unstable_by_key(|e| e.order());
                let mut pos = 0usize;
                for i in 0..rt.events.len() {
                    let ev = rt.events[i];
                    let f = (ev.frame as usize).min(frames);
                    if f > pos {
                        let (l, r) = (&mut rt.buf_l[pos..f], &mut rt.buf_r[pos..f]);
                        rt.instrument.render(instrument, l, r);
                        pos = f;
                    }
                    match ev.kind {
                        EventKind::On(p, v) => rt.instrument.note_on(p, v),
                        EventKind::Off(p) => rt.instrument.note_off(p),
                        EventKind::AllOff => rt.instrument.all_notes_off(),
                    }
                }
                if pos < frames {
                    let (l, r) = (&mut rt.buf_l[pos..frames], &mut rt.buf_r[pos..frames]);
                    rt.instrument.render(instrument, l, r);
                }

                // Mixer: volume, pan, mute/solo with per-block gain ramps.
                let audible = !mixer.mute && (!any_solo || mixer.solo);
                let (pl, pr) = pan_gains(mixer.pan);
                let g = if audible { db_to_gain(mixer.volume_db) } else { 0.0 };
                let (tl, tr) = (g * pl, g * pr);
                let (sl, sr_) = (rt.gain_l, rt.gain_r);
                let inv = 1.0 / frames.max(1) as f32;
                let mut peak_l = 0.0f32;
                let mut peak_r = 0.0f32;
                for i in 0..frames {
                    let t = (i + 1) as f32 * inv;
                    let l = rt.buf_l[i] * (sl + (tl - sl) * t);
                    let r = rt.buf_r[i] * (sr_ + (tr - sr_) * t);
                    peak_l = peak_l.max(l.abs());
                    peak_r = peak_r.max(r.abs());
                    self.master_l[offset + i] += l;
                    self.master_r[offset + i] += r;
                }
                rt.gain_l = tl;
                rt.gain_r = tr;
                rt.meter_l = peak_l.max(rt.meter_l * meter_decay);
                rt.meter_r = peak_r.max(rt.meter_r * meter_decay);
            }
        }

        // Metronome.
        if playing && self.metronome {
            let bpb = self.beats_per_bar.max(1) as i64;
            for i in 0..frames {
                let b = song0 + i as f64 * bpf;
                let ib = (b + 1e-9).floor() as i64;
                if ib > self.last_click {
                    self.last_click = ib;
                    self.click_env = 1.0;
                    self.click_phase = 0.0;
                    self.click_freq = if ib.rem_euclid(bpb) == 0 { 1568.0 } else { 1046.0 };
                }
                if self.click_env > 1e-4 {
                    self.click_phase = (self.click_phase + self.click_freq / sr as f32) % 1.0;
                    let s = (self.click_phase * std::f32::consts::TAU).sin() * self.click_env * 0.4;
                    self.click_env *= 0.9985;
                    self.master_l[offset + i] += s;
                    self.master_r[offset + i] += s;
                }
            }
        } else if playing {
            self.last_click = (song0 + frames as f64 * bpf).floor() as i64;
        }

        if playing {
            self.song_beat += frames as f64 * bpf;
            self.session_beat += frames as f64 * bpf;
        }
    }

    fn finish_master(&mut self, n: usize) {
        let (pl, pr) = pan_gains(self.master.pan);
        let g = if self.master.mute {
            0.0
        } else {
            db_to_gain(self.master.volume_db)
        };
        let (tl, tr) = (g * pl, g * pr);
        let (sl, sr) = (self.master_gain_l, self.master_gain_r);
        let inv = 1.0 / n.max(1) as f32;
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for i in 0..n {
            let t = (i + 1) as f32 * inv;
            self.master_l[i] *= sl + (tl - sl) * t;
            self.master_r[i] *= sr + (tr - sr) * t;
            peak_l = peak_l.max(self.master_l[i].abs());
            peak_r = peak_r.max(self.master_r[i].abs());
        }
        self.master_gain_l = tl;
        self.master_gain_r = tr;
        let decay = (-(n as f64) / self.sr / 0.25).exp() as f32;
        self.master_meter_l = peak_l.max(self.master_meter_l * decay);
        self.master_meter_r = peak_r.max(self.master_meter_r * decay);
    }
}

/// Handles the Session view clip of a track for one segment, including
/// quantized launches and stops.
fn render_session(
    rt: &mut TrackRuntime,
    slots: &[Option<EngineClip>],
    s0: f64,
    frames: usize,
    bpf: f64,
    tempo: f64,
    sr: f64,
) {
    let s1 = s0 + frames as f64 * bpf;
    let mut cur_frame = 0usize;
    let mut cur_beat = s0;
    loop {
        if let Some(q) = rt.queued {
            if q.at < s1 {
                let at = q.at.max(cur_beat);
                let split = (((at - s0) / bpf).round() as usize).clamp(cur_frame, frames);
                play_session_clip(rt, slots, cur_frame, split, cur_beat, bpf, tempo, sr);
                if rt.playing.is_some() {
                    rt.push(split, EventKind::AllOff);
                }
                match q.action {
                    QueuedAction::Launch(slot) if slots.get(slot).map(|s| s.is_some()).unwrap_or(false) => {
                        rt.playing = Some(slot);
                        rt.launch_beat = at;
                        if !rt.session_active {
                            // Taking over from the arrangement: release its notes.
                            rt.push(split, EventKind::AllOff);
                        }
                        rt.session_active = true;
                    }
                    _ => rt.playing = None,
                }
                rt.queued = None;
                cur_frame = split;
                cur_beat = at;
                continue;
            }
        }
        play_session_clip(rt, slots, cur_frame, frames, cur_beat, bpf, tempo, sr);
        break;
    }
}

#[allow(clippy::too_many_arguments)]
fn play_session_clip(
    rt: &mut TrackRuntime,
    slots: &[Option<EngineClip>],
    f0: usize,
    f1: usize,
    beat0: f64,
    bpf: f64,
    tempo: f64,
    sr: f64,
) {
    let Some(slot) = rt.playing else { return };
    let Some(Some(clip)) = slots.get(slot) else {
        rt.playing = None;
        return;
    };
    let local0 = beat0 - rt.launch_beat;
    rt.clip_pos = if clip.looping {
        (local0 + (f1 - f0) as f64 * bpf).rem_euclid(clip.length)
    } else {
        local0 + (f1 - f0) as f64 * bpf
    };
    if f1 <= f0 {
        return;
    }
    if !render_clip(rt, clip, f0, f1, local0, bpf, tempo, sr) {
        rt.playing = None;
    }
}

fn render_arrangement(
    rt: &mut TrackRuntime,
    clips: &[EngineArrangementClip],
    g0: f64,
    frames: usize,
    bpf: f64,
    tempo: f64,
    sr: f64,
) {
    let g1 = g0 + frames as f64 * bpf;
    for a in clips {
        if a.start >= g1 {
            break;
        }
        let end = a.start + a.length;
        if end <= g0 {
            continue;
        }
        let seg0 = a.start.max(g0);
        let seg1 = end.min(g1);
        let f0 = (((seg0 - g0) / bpf).round() as usize).min(frames);
        let f1 = (((seg1 - g0) / bpf).round() as usize).min(frames);
        if f1 > f0 {
            render_clip(rt, &a.clip, f0, f1, seg0 - a.start, bpf, tempo, sr);
        }
        if end <= g1 {
            rt.push(f1.min(frames.saturating_sub(1)), EventKind::AllOff);
        }
    }
}

/// Renders a clip between frames `f0..f1`, where `local0` is the clip-local
/// beat at `f0`. MIDI clips emit events; audio clips are mixed straight into
/// the track buffer. Returns false once a non-looping clip has ended.
#[allow(clippy::too_many_arguments)]
fn render_clip(
    rt: &mut TrackRuntime,
    clip: &EngineClip,
    f0: usize,
    f1: usize,
    local0: f64,
    bpf: f64,
    tempo: f64,
    sr: f64,
) -> bool {
    let len = clip.length;
    let span = (f1 - f0) as f64 * bpf;
    let local1 = local0 + span;
    let frame_at = |l: f64| -> usize { (f0 + ((l - local0) / bpf).floor().max(0.0) as usize).min(f1 - 1) };

    match &clip.content {
        EngineContent::Midi(notes) => {
            let mut l = local0.max(0.0);
            while l < local1 - 1e-12 {
                let mut pos = if clip.looping { l.rem_euclid(len) } else { l };
                if pos >= len - 1e-12 {
                    if !clip.looping {
                        rt.push(frame_at(l), EventKind::AllOff);
                        return false;
                    }
                    pos = 0.0;
                }
                let seg_end = len.min(pos + (local1 - l));
                for n in notes.iter() {
                    if n.start >= pos && n.start < seg_end {
                        rt.push(frame_at(l + n.start - pos), EventKind::On(n.pitch, n.velocity));
                    }
                    let e = (n.start + n.length).min(len);
                    if e > pos && e <= seg_end {
                        rt.push(frame_at(l + e - pos), EventKind::Off(n.pitch));
                    }
                }
                let adv = seg_end - pos;
                if adv <= 0.0 {
                    break;
                }
                l += adv;
                if !clip.looping && seg_end >= len - 1e-12 {
                    rt.push(frame_at(l), EventKind::AllOff);
                    return false;
                }
            }
            true
        }
        EngineContent::Audio(data) => {
            let Some(data) = data else { return true };
            let n_src = data.frames();
            if n_src < 2 {
                return true;
            }
            let secs_per_beat = 60.0 / tempo;
            let src_rate = data.sample_rate as f64;
            let len_secs = len * secs_per_beat;
            let mut still = true;
            for f in f0..f1 {
                let l = local0 + (f - f0) as f64 * bpf;
                if l < 0.0 {
                    continue;
                }
                let pos = if clip.looping { l.rem_euclid(len) } else { l };
                if pos >= len {
                    still = false;
                    break;
                }
                let secs = pos * secs_per_beat;
                let src = secs * src_rate;
                let i = src as usize;
                if i + 1 >= n_src {
                    continue;
                }
                let frac = (src - i as f64) as f32;
                let fade = (secs / AUDIO_FADE_SECS)
                    .min((len_secs - secs) / AUDIO_FADE_SECS)
                    .clamp(0.0, 1.0) as f32;
                let g = clip.gain * fade;
                let sl = data.left[i] + (data.left[i + 1] - data.left[i]) * frac;
                let sr_ = data.right[i] + (data.right[i + 1] - data.right[i]) * frac;
                rt.buf_l[f] += sl * g;
                rt.buf_r[f] += sr_ * g;
            }
            let _ = sr;
            still
        }
    }
}

#[cfg(test)]
mod tests;
