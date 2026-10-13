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

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rtrb::{Consumer, Producer, RingBuffer};

use crate::atomic::{AtomicF32, AtomicF64};
use crate::automation::{
    instrument_params, AutomationTarget, ClipEnvelope, TrackAutomation, CONTROL_FRAMES, MAX_INSTRUMENT_PARAMS,
    PAN_SPEC, VOLUME_SPEC,
};
use crate::dsp::sample::SampleSlots;
use crate::dsp::{InstrumentRuntime, Rng};
use crate::fx::{DeviceParams, FxChain, FxContext, MAX_RETURNS};
use crate::midi::{MidiKind, MidiMessage};
use crate::midi_fx::{MidiDeviceParams, MidiFxChain, MidiFxContext};
use crate::model::{
    db_to_gain, pan_gains, AudioData, Clip, ClipContent, ClipLaunch, ClipRegion, Id, Instrument, MidiInput,
    MixerParams, Monitor, Note, Project, TrackInput, TrackKind, MASTER_BUS,
};
use crate::time::Quantization;
use crate::warp::{self, WarpPlayback, WarpState};
use crate::{MAX_BLOCK, MAX_TRACKS};

mod buses;
mod launch;
mod preview;
pub mod record;

pub use buses::EngineReturn;
pub use record::{input_ring, RecEvent, RecTarget, RecordShared, RecordTap};

const MAX_EVENTS: usize = crate::midi_fx::MAX_MIDI_EVENTS;
/// Hardware MIDI messages buffered per block.
const MAX_MIDI_IN: usize = 512;
/// Capacity of the hardware MIDI input ring.
pub const MIDI_RING: usize = 4096;
/// Device meters (gain reduction) published to the UI.
pub const MAX_DEVICE_METERS: usize = 256;
const MAX_PREVIEW: usize = 64;
/// Fade applied at audio clip edges to avoid clicks, in seconds.
const AUDIO_FADE_SECS: f64 = 0.003;
/// Smoothing time constant for automated mixer gains, in seconds.
const AUTOMATION_SMOOTHING_SECS: f64 = 0.001;

/// Messages from the UI to the audio thread.
// Instrument settings travel by value (see `Instrument`), so some variants
// are large; the queue is allocated once up front.
#[allow(clippy::large_enum_variant)]
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
    /// A Session clip's launch button was pressed.
    LaunchClip {
        track: Id,
        slot: usize,
    },
    /// The launch button was released (Gate and Repeat launch modes).
    ReleaseClip {
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
    /// Play a sample from the browser straight to the output.
    PreviewSample(Arc<AudioData>),
    StopPreview,
    /// Silence every voice immediately.
    Panic,
    /// Set one parameter of a device on a track, return or the master
    /// ([`MASTER_BUS`]).
    SetDeviceParam {
        bus: Id,
        device: Id,
        index: usize,
        value: f32,
    },
    /// Replace all parameters of a device (same kind only).
    SetDeviceParams {
        bus: Id,
        device: Id,
        params: DeviceParams,
    },
    SetDeviceOn {
        bus: Id,
        device: Id,
        on: bool,
    },
    /// Send level of a track to return `index`, in dB.
    SetSend {
        track: Id,
        index: usize,
        db: f32,
    },
    SetTrackInput {
        track: Id,
        input: TrackInput,
    },
    /// Arrangement record on/off. Turning it on while stopped starts the
    /// transport, after the count-in if one is set.
    SetArrangementRecord(bool),
    /// Record into an empty Session slot of an armed audio track, starting at
    /// the next launch boundary.
    RecordSlot {
        track: Id,
        slot: usize,
    },
    /// Bars of count-in before Arrangement recording.
    SetCountIn(u32),
    /// Set one parameter of a MIDI effect on a track.
    SetMidiDeviceParam {
        track: Id,
        device: Id,
        index: usize,
        value: f32,
    },
    SetMidiDeviceParams {
        track: Id,
        device: Id,
        params: MidiDeviceParams,
    },
    SetMidiDeviceOn {
        track: Id,
        device: Id,
        on: bool,
    },
    /// The track selected in the UI. Hardware MIDI plays it when no MIDI
    /// track is armed.
    SetSelectedTrack(Id),
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
    pub returns: Vec<EngineReturn>,
    pub master_chain: FxChain,
    pub count_in_bars: u32,
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
    pub chain: FxChain,
    /// Send gains (linear), one per return.
    pub sends: [f32; MAX_RETURNS],
    pub input: TrackInput,
    /// Samples for the instrument (Sampler, Drum Rack pads).
    pub samples: Vec<Option<Arc<AudioData>>>,
    /// MIDI effects before the instrument.
    pub midi_chain: MidiFxChain,
    /// Hardware MIDI input routing.
    pub midi_input: MidiInput,
    rt: Box<TrackRuntime>,
}

#[derive(Clone)]
pub struct EngineClip {
    pub length: f64,
    pub looping: bool,
    /// Start and loop markers.
    pub region: ClipRegion,
    pub launch: ClipLaunch,
    pub gain: f32,
    pub content: EngineContent,
    /// Clip envelopes, compiled against the track's automation slots.
    pub envelopes: Arc<[ClipEnvelope]>,
    /// Warp settings of an audio clip.
    pub warp: Option<Arc<WarpPlayback>>,
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
    fn from_model(c: &Clip, automation: &TrackAutomation) -> Self {
        let mut warp = None;
        let content = match &c.content {
            ClipContent::Midi { notes } => {
                let mut notes = notes.clone();
                notes.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
                EngineContent::Midi(notes.into())
            }
            ClipContent::Audio(a) => {
                let duration = a.data.as_ref().map(|d| d.duration_secs()).unwrap_or(0.0);
                warp = Some(Arc::new(WarpPlayback::new(&a.warp, duration)));
                EngineContent::Audio(a.data.clone())
            }
        };
        EngineClip {
            length: c.length.max(1.0 / 64.0),
            looping: c.looping,
            region: c.region(),
            launch: c.launch,
            gain: db_to_gain(c.gain_db),
            content,
            envelopes: automation.compile_clip(c).into(),
            warp,
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
            .map(|t| {
                let automation = TrackAutomation::new(t);
                EngineTrack {
                    id: t.id,
                    kind: t.kind,
                    instrument: t.instrument,
                    mixer: t.mixer,
                    slots: t
                        .slots
                        .iter()
                        .map(|s| s.as_ref().map(|c| EngineClip::from_model(c, &automation)))
                        .collect(),
                    arrangement: t
                        .arrangement
                        .iter()
                        .map(|a| EngineArrangementClip {
                            start: a.start,
                            length: a.length,
                            clip: EngineClip::from_model(&a.clip, &automation),
                        })
                        .collect(),
                    chain: FxChain::new(&t.devices, sample_rate),
                    sends: buses::send_gains(&t.sends),
                    input: t.input,
                    samples: t
                        .samples
                        .iter()
                        .map(|s| s.as_ref().and_then(|s| s.data.clone()))
                        .collect(),
                    midi_chain: MidiFxChain::new(&t.midi_devices, sample_rate),
                    midi_input: t.midi_input.clone(),
                    rt: Box::new(TrackRuntime::new(&t.instrument, sample_rate, automation, t.id)),
                }
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
            returns: p
                .returns
                .iter()
                .take(MAX_RETURNS)
                .map(|r| EngineReturn::from_model(r, sample_rate))
                .collect(),
            master_chain: FxChain::new(&p.master_devices, sample_rate),
            count_in_bars: p.count_in_bars,
            reset_transport: false,
        }
    }
}

/// A note event inside a block (also what MIDI effects transform).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EventKind {
    // Order matters: at the same frame, offs are applied before ons.
    AllOff,
    Off(u8),
    On(u8, u8),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MidiEvent {
    pub frame: u32,
    pub kind: EventKind,
}

impl MidiEvent {
    pub fn order(&self) -> (u32, u8) {
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
    /// Session beat at which the playing clip's position is zero (kept from
    /// the previous clip on a legato launch).
    launch_beat: f64,
    queued: Option<Queued>,
    /// Session beat of the playing clip's pending follow action.
    follow_at: Option<f64>,
    /// Repeat launch mode: the slot held down, the beat of its next restart
    /// and the interval.
    repeat: Option<(usize, f64, f64)>,
    /// The last launched slot (follow actions of a clip that played to its
    /// end still fire).
    last_slot: Option<usize>,
    rng: Rng,
    /// Sustain pedal (CC 64) from hardware MIDI is down.
    sustain: bool,
    /// Notes released while the sustain pedal was down.
    sustained: [bool; 128],
    /// Notes held down on hardware MIDI.
    live_held: [bool; 128],
    live_notes: u32,
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
    /// Automation buffers. Belongs to the snapshot the runtime is used with:
    /// when runtimes move between snapshots this is swapped back.
    automation: TrackAutomation,
    /// Grain alignment cache for Complex-mode warping.
    warp_state: WarpState,
    /// Drum Rack pads hit since the last publish.
    pad_hits: u16,
    /// Current (ramped) send gains.
    send_gains: [f32; MAX_RETURNS],
}

impl TrackRuntime {
    fn new(instrument: &Instrument, sample_rate: f32, automation: TrackAutomation, id: Id) -> Self {
        Self {
            instrument: InstrumentRuntime::new(instrument, sample_rate),
            playing: None,
            launch_beat: 0.0,
            queued: None,
            follow_at: None,
            repeat: None,
            last_slot: None,
            rng: Rng::new((id as u32).wrapping_mul(0x9e37_79b9) ^ 0x5bd1_e995),
            sustain: false,
            sustained: [false; 128],
            live_held: [false; 128],
            live_notes: 0,
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
            automation,
            warp_state: WarpState::default(),
            pad_hits: 0,
            send_gains: [0.0; MAX_RETURNS],
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

/// Published automated instrument parameter values, one per index of
/// [`crate::automation::instrument_params`].
pub struct AutoParams(pub [AtomicF32; MAX_INSTRUMENT_PARAMS]);

impl Default for AutoParams {
    fn default() -> Self {
        Self(std::array::from_fn(|_| AtomicF32::default()))
    }
}

impl std::ops::Deref for AutoParams {
    type Target = [AtomicF32; MAX_INSTRUMENT_PARAMS];

    fn deref(&self) -> &Self::Target {
        &self.0
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
    /// Automated volume (dB) and pan, NaN when not automated.
    pub auto_volume: AtomicF32,
    pub auto_pan: AtomicF32,
    /// Automated instrument parameters in their natural units, NaN when not
    /// automated (indexed like [`crate::automation::instrument_params`]).
    pub auto_params: AutoParams,
    /// Drum Rack pads hit since the UI last took this (bit per pad).
    pub pad_hits: AtomicU32,
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
    /// Return track meters, by return index.
    pub returns: Vec<TrackShared>,
    /// Gain reduction of compressors and limiters, by device id.
    pub device_meters: Vec<DeviceMeter>,
    pub record: RecordShared,
}

/// A device's published meter value.
#[derive(Default)]
pub struct DeviceMeter {
    /// Device id, 0 for an unused entry.
    pub id: AtomicU64,
    pub value: AtomicF32,
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
                    t.auto_volume.store(f32::NAN);
                    t.auto_pan.store(f32::NAN);
                    for p in t.auto_params.iter() {
                        p.store(f32::NAN);
                    }
                    t
                })
                .collect(),
            returns: (0..MAX_RETURNS).map(|_| TrackShared::default()).collect(),
            device_meters: (0..MAX_DEVICE_METERS).map(|_| DeviceMeter::default()).collect(),
            record: RecordShared::new(),
        }
    }

    /// Gain reduction in dB reported by a device, if it publishes one.
    pub fn device_meter(&self, device: Id) -> Option<f32> {
        self.device_meters
            .iter()
            .find(|m| m.id.load(Ordering::Relaxed) == device)
            .map(|m| m.value.load())
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
    preview_garbage: Consumer<Arc<AudioData>>,
    pub shared: Arc<SharedState>,
    /// Recorded input and take events from the audio thread.
    pub record: RecordTap,
}

/// Creates the ring hardware MIDI input writes into.
pub fn midi_ring() -> (Producer<MidiMessage>, Consumer<MidiMessage>) {
    RingBuffer::new(MIDI_RING)
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
        while let Ok(old) = self.preview_garbage.pop() {
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

    input: record::AudioIn,
    rec: record::Recorder,
    count_in_bars: u32,
    preview: preview::Previewer,
    /// Beat clock for MIDI effects while the transport is stopped.
    free_beat: f64,
    selected_track: Id,
    midi_in: Option<Consumer<MidiMessage>>,
    /// Hardware MIDI messages of the current block.
    midi_pending: [MidiMessage; MAX_MIDI_IN],
    midi_pending_len: usize,
}

impl Engine {
    pub fn new(sample_rate: f32) -> (Engine, EngineHandle) {
        let (cmd_tx, cmd_rx) = RingBuffer::new(1024);
        let (gc_tx, gc_rx) = RingBuffer::new(256);
        let (pgc_tx, pgc_rx) = RingBuffer::new(64);
        let shared = Arc::new(SharedState::new(sample_rate));
        let (rec, record_tap) = record::Recorder::new(sample_rate);
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
            input: record::AudioIn::new(MAX_BLOCK),
            rec,
            count_in_bars: 0,
            preview: preview::Previewer::new(pgc_tx),
            free_beat: 0.0,
            selected_track: 0,
            midi_in: None,
            midi_pending: [MidiMessage::default(); MAX_MIDI_IN],
            midi_pending_len: 0,
        };
        let handle = EngineHandle {
            commands: cmd_tx,
            garbage: gc_rx,
            preview_garbage: pgc_rx,
            shared,
            record: record_tap,
        };
        (engine, handle)
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Connects the audio input ring (see [`input_ring`]). Call before the
    /// engine moves to the audio thread.
    pub fn set_input(&mut self, input: Consumer<f32>) {
        self.input.connect(input);
        self.shared.record.input_connected.store(true, Ordering::Relaxed);
    }

    /// Connects hardware MIDI input (see [`midi_ring`]). Call before the
    /// engine moves to the audio thread.
    pub fn set_midi_input(&mut self, input: Consumer<MidiMessage>) {
        self.midi_in = Some(input);
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
            t.rt.follow_at = None;
            t.rt.repeat = None;
            t.rt.instrument.all_notes_off();
            // Notes the clips held stay held in an arpeggiator otherwise.
            t.midi_chain.reset();
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
                if self.playing || self.rec.count_in > 0.0 {
                    self.rec.stop_all(self.song_beat, self.session_beat);
                }
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
                let (now, global, bpb) = (self.session_beat, self.quantization, self.beats_per_bar as f64);
                let at = global.next_boundary(now, bpb);
                if self.rec.launch(track, slot, at) {
                    return;
                }
                if let Some(t) = self.track_mut(track) {
                    launch::press(t, slot, now, global, bpb);
                }
            }
            Command::ReleaseClip { track, slot } => {
                let (now, global, bpb) = (self.session_beat, self.quantization, self.beats_per_bar as f64);
                if let Some(t) = self.track_mut(track) {
                    launch::release(t, slot, now, global, bpb);
                }
            }
            Command::StopTrack(track) => {
                let at = self
                    .quantization
                    .next_boundary(self.session_beat, self.beats_per_bar as f64);
                self.rec.stop_track(track, at);
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
                if let Some(p) = self.project.as_ref() {
                    for t in p.tracks.iter() {
                        if !self.rec.launch(t.id, scene, at) {
                            self.rec.stop_track(t.id, at);
                        }
                    }
                }
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
                if let Some(p) = self.project.as_ref() {
                    for t in p.tracks.iter() {
                        self.rec.stop_track(t.id, at);
                    }
                }
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
                        t.rt.follow_at = None;
                        t.rt.repeat = None;
                        t.rt.instrument.all_notes_off();
                        t.rt.arrangement_reset = true;
                    }
                });
            }
            Command::SetMixer { track, params } => {
                if let Some(t) = self.track_mut(track) {
                    t.mixer = params;
                } else if let Some(r) = self
                    .project
                    .as_mut()
                    .and_then(|p| p.returns.iter_mut().find(|r| r.id == track))
                {
                    r.mixer = params;
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
                    new.master_chain.adopt(&mut old.master_chain);
                    for r in new.returns.iter_mut() {
                        if let Some(o) = old.returns.iter_mut().find(|o| o.id == r.id) {
                            r.adopt(o);
                        }
                    }
                    for t in new.tracks.iter_mut() {
                        if let Some(o) = old.tracks.iter_mut().find(|o| o.id == t.id) {
                            t.chain.adopt(&mut o.chain);
                            t.midi_chain.adopt(&mut o.midi_chain);
                            if o.rt.instrument.kind_tag() == t.instrument.kind_tag() {
                                std::mem::swap(&mut o.rt, &mut t.rt);
                                // Automation buffers belong to their snapshot.
                                std::mem::swap(&mut o.rt.automation, &mut t.rt.automation);
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
                    self.count_in_bars = new.count_in_bars;
                    self.rec.stop_all(self.song_beat, self.session_beat);
                    self.playing = false;
                    self.song_beat = 0.0;
                    self.start_marker = 0.0;
                    self.session_beat = 0.0;
                    for t in new.tracks.iter_mut() {
                        t.rt.instrument.panic();
                        t.chain.reset();
                        t.midi_chain.reset();
                    }
                    for r in new.returns.iter_mut() {
                        r.chain.reset();
                    }
                    new.master_chain.reset();
                }
                if let Some(old) = self.project.replace(new) {
                    self.retire(old);
                }
            }
            Command::PreviewSample(data) => self.preview.start(data),
            Command::StopPreview => self.preview.stop(),
            Command::Panic => {
                self.for_each_track(|t| {
                    t.rt.instrument.panic();
                    t.midi_chain.reset();
                    t.chain.reset();
                });
                if let Some(p) = self.project.as_mut() {
                    for r in p.returns.iter_mut() {
                        r.chain.reset();
                    }
                    p.master_chain.reset();
                }
            }
            Command::SetDeviceParam {
                bus,
                device,
                index,
                value,
            } => {
                if let Some(c) = self.chain_mut(bus) {
                    c.set_param(device, index, value);
                }
            }
            Command::SetDeviceParams { bus, device, params } => {
                if let Some(c) = self.chain_mut(bus) {
                    c.set_params(device, params);
                }
            }
            Command::SetDeviceOn { bus, device, on } => {
                if let Some(c) = self.chain_mut(bus) {
                    c.set_on(device, on);
                }
            }
            Command::SetSend { track, index, db } => {
                if let Some(t) = self.track_mut(track) {
                    if let Some(s) = t.sends.get_mut(index) {
                        *s = db_to_gain(db);
                    }
                }
            }
            Command::SetTrackInput { track, input } => {
                if let Some(t) = self.track_mut(track) {
                    t.input = input;
                }
            }
            Command::SetArrangementRecord(on) => {
                if on {
                    self.rec.arrangement = true;
                    // Armed audio tracks go back to the Arrangement to record there.
                    self.for_each_track(|t| {
                        if t.kind == TrackKind::Audio && t.mixer.arm && t.rt.session_active {
                            t.rt.session_active = false;
                            t.rt.playing = None;
                            t.rt.queued = None;
                            t.rt.arrangement_reset = true;
                        }
                    });
                    if !self.playing && self.rec.count_in <= 0.0 {
                        if self.count_in_bars > 0 {
                            let beats = (self.count_in_bars * self.beats_per_bar.max(1)) as f64;
                            self.rec.start_count_in(beats);
                        } else {
                            self.start_transport(self.start_marker);
                        }
                    }
                } else {
                    self.rec.arrangement = false;
                    if !self.playing {
                        self.rec.count_in = 0.0;
                    }
                }
            }
            Command::RecordSlot { track, slot } => {
                let ok = self
                    .project
                    .as_ref()
                    .and_then(|p| p.tracks.iter().find(|t| t.id == track))
                    .map(|t| t.kind == TrackKind::Audio && t.mixer.arm && t.slots.get(slot).is_some())
                    .unwrap_or(false);
                if ok {
                    if !self.playing {
                        self.start_transport(self.start_marker);
                    }
                    let at = self
                        .quantization
                        .next_boundary(self.session_beat, self.beats_per_bar as f64);
                    self.rec.queue_slot(track, slot, at);
                }
            }
            Command::SetCountIn(bars) => self.count_in_bars = bars.min(4),
            Command::SetMidiDeviceParam {
                track,
                device,
                index,
                value,
            } => {
                if let Some(t) = self.track_mut(track) {
                    t.midi_chain.set_param(device, index, value);
                }
            }
            Command::SetMidiDeviceParams { track, device, params } => {
                if let Some(t) = self.track_mut(track) {
                    t.midi_chain.set_params(device, params);
                }
            }
            Command::SetMidiDeviceOn { track, device, on } => {
                if let Some(t) = self.track_mut(track) {
                    t.midi_chain.set_on(device, on);
                }
            }
            Command::SetSelectedTrack(id) => self.selected_track = id,
        }
    }

    fn chain_mut(&mut self, bus: Id) -> Option<&mut FxChain> {
        let p = self.project.as_mut()?;
        if bus == MASTER_BUS {
            return Some(&mut p.master_chain);
        }
        if let Some(t) = p.tracks.iter_mut().find(|t| t.id == bus) {
            return Some(&mut t.chain);
        }
        p.returns.iter_mut().find(|r| r.id == bus).map(|r| &mut r.chain)
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
            if self.preview.is_playing() {
                let sr = self.sr;
                self.preview.mix(&mut self.master_l[..n], &mut self.master_r[..n], sr);
            }
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

    fn publish(&mut self) {
        let s = &self.shared;
        s.playing.store(self.playing, Ordering::Relaxed);
        s.song_beat.store(self.song_beat);
        s.tempo.store(self.tempo);
        s.master_meter_l.store(self.master_meter_l);
        s.master_meter_r.store(self.master_meter_r);
        s.record.input_peak.store(self.input.peak);
        self.rec.publish(&s.record);
        if let Some(p) = self.project.as_ref() {
            for (i, r) in p.returns.iter().enumerate() {
                let rs = &s.returns[i];
                rs.id.store(r.id, Ordering::Relaxed);
                rs.meter_l.store(r.meter_l);
                rs.meter_r.store(r.meter_r);
            }
            let chains = p
                .tracks
                .iter()
                .map(|t| &t.chain)
                .chain(p.returns.iter().map(|r| &r.chain))
                .chain(std::iter::once(&p.master_chain));
            let mut slots = s.device_meters.iter();
            for d in chains.flat_map(|c| c.devices.iter()) {
                if !matches!(d.params, DeviceParams::Compressor(_) | DeviceParams::Limiter(_)) {
                    continue;
                }
                let Some(m) = slots.next() else { break };
                m.id.store(d.id, Ordering::Relaxed);
                m.value.store(d.meter());
            }
            for m in slots {
                m.id.store(0, Ordering::Relaxed);
            }
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
                let auto = &t.rt.automation;
                let natural = |target, spec: crate::automation::ParamSpec| {
                    auto.last_value(target).map(|n| spec.denormalize(n)).unwrap_or(f32::NAN)
                };
                ts.auto_volume.store(natural(AutomationTarget::Volume, VOLUME_SPEC));
                ts.auto_pan.store(natural(AutomationTarget::Pan, PAN_SPEC));
                let specs = instrument_params(&t.instrument);
                for (i, p) in ts.auto_params.iter().enumerate() {
                    p.store(match specs.get(i) {
                        Some(spec) if !auto.is_empty() => natural(AutomationTarget::Instrument(i as u16), *spec),
                        _ => f32::NAN,
                    });
                }
                if t.rt.pad_hits != 0 {
                    ts.pad_hits.fetch_or(t.rt.pad_hits as u32, Ordering::Relaxed);
                }
            }
        }
        if let Some(p) = self.project.as_mut() {
            for t in p.tracks.iter_mut() {
                t.rt.pad_hits = 0;
            }
        }
    }

    fn render_block(&mut self, n: usize) {
        self.master_l[..n].fill(0.0);
        self.master_r[..n].fill(0.0);
        if let Some(p) = self.project.as_mut() {
            buses::clear_returns(&mut p.returns, n);
        }
        let decay = (-(n as f64) / self.sr / 0.25).exp() as f32;
        self.input.read(n, decay);
        self.read_midi();
        let mut pos = 0;
        while pos < n {
            let remaining = n - pos;
            let mut len = remaining;
            let mut loops = false;
            let counting = !self.playing && self.rec.count_in > 0.0;
            if counting {
                let bpf = self.beats_per_frame();
                len = ((self.rec.count_in / bpf).ceil() as usize).clamp(1, remaining);
            }
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
            if counting {
                let bpf = self.beats_per_frame();
                let (l, r) = (&mut self.master_l[pos..pos + len], &mut self.master_r[pos..pos + len]);
                if self.rec.render_count_in(l, r, bpf, self.beats_per_bar, self.sr as f32) {
                    self.start_transport(self.start_marker);
                }
            }
            if loops {
                let loop_end = self.loop_start + self.loop_length;
                self.rec.loop_jump(pos + len, loop_end);
                self.song_beat = self.loop_start + (self.song_beat - loop_end).max(0.0);
                self.last_click = self.song_beat.ceil() as i64 - 1;
                self.for_each_track(|t| t.rt.arrangement_reset = true);
            }
            pos += len;
        }
        self.finish_master(n);
        self.rec.end_block(&self.input.l[..n], &self.input.r[..n]);
    }

    /// Takes the hardware MIDI messages that arrived since the last block.
    fn read_midi(&mut self) {
        self.midi_pending_len = 0;
        let Some(rx) = self.midi_in.as_mut() else { return };
        while let Ok(m) = rx.pop() {
            if self.midi_pending_len < MAX_MIDI_IN {
                self.midi_pending[self.midi_pending_len] = m;
                self.midi_pending_len += 1;
            }
        }
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
        let ctx = FxContext::new(sr as f32, tempo);
        let midi_fx_beat = if playing { session0 } else { self.free_beat };
        // Hardware MIDI is delivered once per block, at its start.
        let midi_in = if offset == 0 {
            &self.midi_pending[..self.midi_pending_len]
        } else {
            &self.midi_pending[..0]
        };
        let selected = self.selected_track;
        let (in_l, in_r) = (
            &self.input.l[offset..offset + frames],
            &self.input.r[offset..offset + frames],
        );

        if let Some(project) = self.project.as_mut() {
            let any_solo = project.tracks.iter().any(|t| t.mixer.solo);
            let any_armed = project.tracks.iter().any(|t| t.kind == TrackKind::Midi && t.mixer.arm);
            let EngineProject { tracks, returns, .. } = &mut **project;
            for track in tracks.iter_mut() {
                let EngineTrack {
                    id,
                    kind,
                    instrument,
                    mixer,
                    slots,
                    arrangement,
                    chain,
                    sends,
                    input,
                    samples,
                    midi_chain,
                    midi_input,
                    rt,
                } = track;
                rt.buf_l[..frames].fill(0.0);
                rt.buf_r[..frames].fill(0.0);
                rt.events.clear();
                rt.automation.begin(frames);

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
                if *kind == TrackKind::Midi {
                    // Armed tracks (or, with none armed, the selected one)
                    // play hardware MIDI.
                    let live = match input.monitor {
                        Monitor::In => true,
                        Monitor::Auto => mixer.arm || (!any_armed && *id == selected),
                        Monitor::Off => false,
                    };
                    live_midi(rt, midi_input, midi_in, live);
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
                if !rt.session_active {
                    // While stopped, hold the value at the playhead.
                    let step = if playing { bpf } else { 0.0 };
                    rt.automation.write_lanes(song0, step, frames);
                }
                rt.automation.end(frames);

                // Instrument rendering, split at event boundaries.
                rt.events.sort_unstable_by_key(|e| e.order());
                if !midi_chain.is_empty() {
                    let mctx = MidiFxContext {
                        frames,
                        beat0: midi_fx_beat,
                        bpf,
                        sample_rate: sr as f32,
                        playing,
                    };
                    midi_chain.process(&mut rt.events, &mctx);
                }
                let mut pos = 0usize;
                for i in 0..rt.events.len() {
                    let ev = rt.events[i];
                    let f = (ev.frame as usize).min(frames);
                    if f > pos {
                        render_instrument(rt, instrument, samples, pos, f);
                        pos = f;
                    }
                    match ev.kind {
                        EventKind::On(p, v) => rt.instrument.note_on(p, v),
                        EventKind::Off(p) => rt.instrument.note_off(p),
                        EventKind::AllOff => rt.instrument.all_notes_off(),
                    }
                }
                if pos < frames {
                    render_instrument(rt, instrument, samples, pos, frames);
                }
                let hits = rt.instrument.take_pad_hits();
                if hits != 0 {
                    rt.pad_hits |= hits;
                }

                // Live input on audio tracks.
                let monitoring = match input.monitor {
                    Monitor::In => true,
                    Monitor::Auto => mixer.arm,
                    Monitor::Off => false,
                };
                if *kind == TrackKind::Audio && monitoring {
                    for i in 0..frames {
                        let (l, r) = input.channels.route(in_l[i], in_r[i]);
                        rt.buf_l[i] += l;
                        rt.buf_r[i] += r;
                    }
                }

                // Insert effects.
                if !chain.is_empty() {
                    chain.process(&mut rt.buf_l[..frames], &mut rt.buf_r[..frames], &ctx);
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
                if rt.automation.mixer_automated() {
                    // Automated volume/pan: evaluated every sample, lightly
                    // smoothed so steps in the envelope don't click.
                    let k = (1.0 - (-1.0 / (AUTOMATION_SMOOTHING_SECS * sr)).exp()) as f32;
                    let audible = if audible { 1.0 } else { 0.0 };
                    let (mut gl, mut gr) = (sl, sr_);
                    for i in 0..frames {
                        let (db, pan) = rt.automation.mixer_at(i, mixer.volume_db, mixer.pan);
                        let g = audible * db_to_gain(db);
                        let (pl, pr) = pan_gains(pan);
                        let (tl, tr) = (g * pl, g * pr);
                        gl += (tl - gl) * k;
                        gr += (tr - gr) * k;
                        // Snap when close so the smoother never runs into denormals.
                        if (tl - gl).abs() < 1e-6 {
                            gl = tl;
                        }
                        if (tr - gr).abs() < 1e-6 {
                            gr = tr;
                        }
                        let l = rt.buf_l[i] * gl;
                        let r = rt.buf_r[i] * gr;
                        peak_l = peak_l.max(l.abs());
                        peak_r = peak_r.max(r.abs());
                        self.master_l[offset + i] += l;
                        self.master_r[offset + i] += r;
                        rt.buf_l[i] = l;
                        rt.buf_r[i] = r;
                    }
                    rt.gain_l = gl;
                    rt.gain_r = gr;
                } else {
                    for i in 0..frames {
                        let t = (i + 1) as f32 * inv;
                        let l = rt.buf_l[i] * (sl + (tl - sl) * t);
                        let r = rt.buf_r[i] * (sr_ + (tr - sr_) * t);
                        peak_l = peak_l.max(l.abs());
                        peak_r = peak_r.max(r.abs());
                        self.master_l[offset + i] += l;
                        self.master_r[offset + i] += r;
                        rt.buf_l[i] = l;
                        rt.buf_r[i] = r;
                    }
                    rt.gain_l = tl;
                    rt.gain_r = tr;
                }
                // Post-fader sends.
                buses::add_sends(
                    returns,
                    sends,
                    &mut rt.send_gains,
                    &rt.buf_l[..frames],
                    &rt.buf_r[..frames],
                    offset,
                );
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

        if let Some(project) = self.project.as_mut() {
            let latency = self.shared.record.io_latency.load(Ordering::Relaxed) + self.input.backlog as u32;
            self.rec
                .segment(project, offset, frames, song0, session0, bpf, playing, latency);
        }

        if playing {
            self.song_beat += frames as f64 * bpf;
            self.session_beat += frames as f64 * bpf;
        }
        self.free_beat += frames as f64 * bpf;
    }

    fn finish_master(&mut self, n: usize) {
        if let Some(p) = self.project.as_mut() {
            let ctx = FxContext::new(self.sr as f32, self.tempo);
            let decay = (-(n as f64) / self.sr / 0.25).exp() as f32;
            let (ml, mr) = (&mut self.master_l[..n], &mut self.master_r[..n]);
            buses::mix_returns(&mut p.returns, ml, mr, &ctx, decay);
            if !p.master_chain.is_empty() {
                p.master_chain.process(ml, mr, &ctx);
            }
        }
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

/// Renders the instrument into frames `from..to` of the track buffer. While
/// instrument parameters are automated, the parameters are updated every
/// [`CONTROL_FRAMES`] frames.
fn render_instrument(rt: &mut TrackRuntime, base: &Instrument, samples: &SampleSlots, from: usize, to: usize) {
    if !rt.automation.instrument_automated() {
        rt.instrument
            .render(base, samples, &mut rt.buf_l[from..to], &mut rt.buf_r[from..to]);
        return;
    }
    let mut p = from;
    while p < to {
        let e = ((p / CONTROL_FRAMES + 1) * CONTROL_FRAMES).min(to);
        let params = rt.automation.instrument_at(base, p);
        rt.instrument
            .render(&params, samples, &mut rt.buf_l[p..e], &mut rt.buf_r[p..e]);
        p = e;
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
        if rt.queued.is_none() {
            launch::schedule(rt, slots, s1);
        }
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
                        let clip = slots[slot].as_ref().map(|c| c.launch).unwrap_or_default();
                        // Legato: the new clip continues from the old one's position.
                        if !(clip.legato && rt.playing.is_some()) {
                            rt.launch_beat = at;
                        }
                        rt.playing = Some(slot);
                        rt.last_slot = Some(slot);
                        rt.follow_at = slots[slot].as_ref().and_then(launch::follow_time).map(|t| at + t);
                        if rt.repeat.is_some_and(|(s, ..)| s != slot) {
                            rt.repeat = None;
                        }
                        if !rt.session_active {
                            // Taking over from the arrangement: release its notes.
                            rt.push(split, EventKind::AllOff);
                        }
                        rt.session_active = true;
                    }
                    _ => {
                        rt.playing = None;
                        rt.follow_at = None;
                        rt.repeat = None;
                    }
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
    let local1 = local0 + (f1 - f0) as f64 * bpf;
    rt.clip_pos = clip.region.position(local1).unwrap_or(clip.region.end);
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

/// Renders a clip between frames `f0..f1`, where `local0` is the clip time
/// (beats since its position zero) at `f0`. MIDI clips emit events; audio
/// clips are mixed straight into the track buffer. Returns false once a
/// non-looping clip has ended.
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
    let region = clip.region;
    let span = (f1 - f0) as f64 * bpf;
    let local1 = local0 + span;
    if !clip.envelopes.is_empty() {
        rt.automation.write_clip(&clip.envelopes, f0, f1, local0, bpf, &region);
    }
    let frame_at = |l: f64| -> usize { (f0 + ((l - local0) / bpf).floor().max(0.0) as usize).min(f1 - 1) };

    match &clip.content {
        EngineContent::Midi(notes) => {
            // Walk the clip in runs that don't cross its end (the loop wraps
            // there).
            let mut l = local0.max(0.0);
            while l < local1 - 1e-12 {
                let Some(pos) = region.position(l) else {
                    rt.push(frame_at(l), EventKind::AllOff);
                    return false;
                };
                let seg_end = region.end.min(pos + (local1 - l));
                for n in notes.iter() {
                    if n.start >= pos && n.start < seg_end {
                        rt.push(frame_at(l + n.start - pos), EventKind::On(n.pitch, n.velocity));
                    }
                    let e = (n.start + n.length).min(region.end);
                    if e > pos && e <= seg_end {
                        rt.push(frame_at(l + e - pos), EventKind::Off(n.pitch));
                    }
                }
                let adv = seg_end - pos;
                if adv <= 0.0 {
                    break;
                }
                l += adv;
                if !region.looping && seg_end >= region.end - 1e-12 {
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
            let first_pass = region.first_pass();
            let key = Arc::as_ptr(data) as usize;
            let mut still = true;
            for f in f0..f1 {
                let l = local0 + (f - f0) as f64 * bpf;
                if l < 0.0 {
                    continue;
                }
                let Some(pos) = region.position(l) else {
                    still = false;
                    break;
                };
                // Fades at the edges of the playing run, against clicks.
                let run_start = if l < first_pass {
                    region.start
                } else {
                    region.loop_start
                };
                let fade = ((pos - run_start) * secs_per_beat / AUDIO_FADE_SECS)
                    .min((region.end - pos) * secs_per_beat / AUDIO_FADE_SECS)
                    .clamp(0.0, 1.0) as f32;
                let g = clip.gain * fade;
                let secs = pos * secs_per_beat;
                let (sl, sr_) = match &clip.warp {
                    Some(w) => warp::sample_at(data, w, pos, secs_per_beat, key, &mut rt.warp_state),
                    None => warp::read(data, secs),
                };
                rt.buf_l[f] += sl * g;
                rt.buf_r[f] += sr_ * g;
            }
            let _ = sr;
            still
        }
    }
}

/// Feeds a MIDI track the hardware messages of this block when it is
/// `live`, handling the sustain pedal. When the track stops being live, the
/// notes it was playing from hardware are released.
fn live_midi(rt: &mut TrackRuntime, input: &MidiInput, messages: &[MidiMessage], live: bool) {
    if !live {
        if rt.live_notes > 0 {
            for p in 0..128u8 {
                if rt.live_held[p as usize] {
                    rt.live_held[p as usize] = false;
                    rt.push(0, EventKind::Off(p));
                }
            }
            rt.live_notes = 0;
            rt.sustain = false;
            rt.sustained = [false; 128];
        }
        return;
    }
    for m in messages {
        if !input.accepts(m.port, m.channel()) {
            continue;
        }
        match m.kind() {
            MidiKind::NoteOn { pitch, velocity } => {
                let p = pitch as usize & 127;
                rt.sustained[p] = false;
                if !rt.live_held[p] {
                    rt.live_held[p] = true;
                    rt.live_notes += 1;
                }
                rt.push(0, EventKind::On(pitch, velocity));
            }
            MidiKind::NoteOff { pitch } => {
                let p = pitch as usize & 127;
                if rt.sustain {
                    rt.sustained[p] = true;
                } else {
                    release_live(rt, pitch);
                }
            }
            MidiKind::Cc { cc: 64, value } => {
                let down = value >= 64;
                if rt.sustain && !down {
                    for p in 0..128u8 {
                        if rt.sustained[p as usize] {
                            rt.sustained[p as usize] = false;
                            release_live(rt, p);
                        }
                    }
                }
                rt.sustain = down;
            }
            MidiKind::Cc { cc: 120 | 123, .. } => {
                // All Sound Off / All Notes Off.
                rt.push(0, EventKind::AllOff);
                rt.live_held = [false; 128];
                rt.live_notes = 0;
            }
            _ => {}
        }
    }
}

fn release_live(rt: &mut TrackRuntime, pitch: u8) {
    let p = pitch as usize & 127;
    if rt.live_held[p] {
        rt.live_held[p] = false;
        rt.live_notes = rt.live_notes.saturating_sub(1);
    }
    rt.push(0, EventKind::Off(pitch));
}

#[cfg(test)]
mod fx_tests;
#[cfg(test)]
mod instrument_tests;
#[cfg(test)]
mod launch_tests;
#[cfg(test)]
mod tests;
