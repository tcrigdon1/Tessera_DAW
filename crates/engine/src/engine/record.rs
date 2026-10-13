//! Audio input, monitoring and recording.
//!
//! The audio device's input callback pushes interleaved stereo frames into a
//! lock-free ring that the engine reads once per block ([`AudioIn`]). Armed
//! audio tracks can monitor that input through their devices.
//!
//! Recording never touches the project from the audio thread. While any take
//! is running, the engine streams the raw input to the UI through a second
//! ring and sends [`RecEvent`]s that mark where each take starts and stops in
//! that stream (sample-accurately, including quantized Session starts). The
//! UI cuts the takes out of the stream, writes them to WAV files and adds the
//! clips to the project.
//!
//! Armed MIDI tracks record the same way, except that instead of audio the
//! engine sends the notes they play ([`RecEvent::Note`]) and the UI turns
//! them into MIDI clips.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};

use rtrb::{Consumer, Producer, RingBuffer};

use super::{EngineProject, EventKind};
use crate::atomic::{AtomicF32, AtomicF64};
use crate::model::{Id, TrackKind};

/// Most takes (tracks recording at once, plus queued Session recordings).
pub const MAX_TAKES: usize = 32;
/// Capacity of the input ring in frames.
pub const INPUT_RING_FRAMES: usize = 16384;
/// Capacity of the recording stream to the UI, in seconds.
const RECORD_RING_SECS: f32 = 20.0;

/// Where a take ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecTarget {
    Arrangement,
    Slot(usize),
}

/// Recording events for the UI. `frame` indexes the recording stream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RecEvent {
    Started {
        track: Id,
        target: RecTarget,
        /// Song beat (Arrangement) or session beat (Session) of the start.
        beat: f64,
        frame: u64,
        /// Frames by which the input lags what the user heard; the UI drops
        /// this many frames from the start of the take.
        latency: u32,
    },
    Stopped {
        track: Id,
        target: RecTarget,
        beat: f64,
        frame: u64,
    },
    /// A note played on a MIDI track with a take (queued or recording).
    /// `beat` is on the take's clock: song beats for the Arrangement,
    /// session beats for a slot.
    Note { track: Id, beat: f64, kind: EventKind },
    /// The UI fell behind and input was lost.
    Overflow,
}

/// The UI's end of the recording stream.
pub struct RecordTap {
    /// Interleaved stereo input frames, only while something records.
    pub audio: Consumer<f32>,
    pub events: Consumer<RecEvent>,
}

/// Creates the ring an input device writes interleaved stereo frames into.
pub fn input_ring() -> (Producer<f32>, Consumer<f32>) {
    RingBuffer::new(INPUT_RING_FRAMES * 2)
}

/// Published per-take state.
#[derive(Default)]
pub struct TakeShared {
    /// Track id, 0 for an unused entry.
    pub track: AtomicU64,
    /// Session slot, or -1 for the Arrangement.
    pub slot: AtomicI32,
    /// False while queued for a quantized start.
    pub recording: AtomicBool,
    pub start_beat: AtomicF64,
}

/// Recording state published for the UI.
pub struct RecordShared {
    /// Arrangement record is on.
    pub arrangement: AtomicBool,
    /// Beats of count-in left (0 when not counting in).
    pub count_in: AtomicF64,
    /// Input and output latency reported by the audio device, in frames.
    /// Written by the device code.
    pub io_latency: AtomicU32,
    pub input_connected: AtomicBool,
    pub input_peak: AtomicF32,
    pub takes: Vec<TakeShared>,
}

impl RecordShared {
    pub(super) fn new() -> Self {
        Self {
            arrangement: AtomicBool::new(false),
            count_in: AtomicF64::new(0.0),
            io_latency: AtomicU32::new(0),
            input_connected: AtomicBool::new(false),
            input_peak: AtomicF32::new(0.0),
            takes: (0..MAX_TAKES).map(|_| TakeShared::default()).collect(),
        }
    }

    /// The take for a track, if one is queued or running.
    pub fn take_for(&self, track: Id) -> Option<&TakeShared> {
        self.takes.iter().find(|t| t.track.load(Ordering::Relaxed) == track)
    }
}

/// Reads the input ring once per block.
pub(super) struct AudioIn {
    consumer: Option<Consumer<f32>>,
    primed: bool,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// Frames still waiting in the ring after the last read.
    pub backlog: usize,
    pub peak: f32,
}

impl AudioIn {
    pub fn new(max_block: usize) -> Self {
        Self {
            consumer: None,
            primed: false,
            l: vec![0.0; max_block],
            r: vec![0.0; max_block],
            backlog: 0,
            peak: 0.0,
        }
    }

    pub fn connect(&mut self, c: Consumer<f32>) {
        self.consumer = Some(c);
        self.primed = false;
    }

    /// Fills `l`/`r[..n]` with the next `n` input frames (silence if none).
    pub fn read(&mut self, n: usize, decay: f32) {
        self.l[..n].fill(0.0);
        self.r[..n].fill(0.0);
        let Some(c) = self.consumer.as_mut() else { return };
        let mut avail = c.slots() / 2;
        // Input and output callbacks are not in phase: keep one block of
        // cushion so reads do not run dry.
        if !self.primed {
            if avail < n * 2 {
                self.backlog = avail;
                return;
            }
            self.primed = true;
        }
        // Clock drift: drop old input so latency stays bounded.
        let high = (n * 6).max(8192);
        if avail > high {
            let skip = avail - n * 2;
            if let Ok(chunk) = c.read_chunk(skip * 2) {
                chunk.commit_all();
            }
            avail = n * 2;
        }
        let m = avail.min(n);
        if m < n {
            self.primed = false;
        }
        if let Ok(chunk) = c.read_chunk(m * 2) {
            let (a, b) = chunk.as_slices();
            let mut peak = 0.0f32;
            for (i, s) in a.iter().chain(b.iter()).enumerate() {
                let f = i / 2;
                if i % 2 == 0 {
                    self.l[f] = *s;
                } else {
                    self.r[f] = *s;
                }
                peak = peak.max(s.abs());
            }
            chunk.commit_all();
            self.peak = peak.max(self.peak * decay);
        }
        self.backlog = avail - m;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TakeState {
    /// Waiting for the quantized start (Session only).
    Queued {
        at: f64,
    },
    Recording,
}

#[derive(Debug, Clone, Copy)]
struct Take {
    track: Id,
    target: RecTarget,
    state: TakeState,
    start_beat: f64,
    /// Session beat at which a Session take stops.
    stop_at: Option<f64>,
    /// Play the new clip once it is recorded.
    play_after: bool,
    /// A MIDI take: records notes, not audio.
    midi: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingPlay {
    track: Id,
    slot: usize,
    beat: f64,
}

/// Metronome click used for the count-in.
#[derive(Default)]
struct Click {
    env: f32,
    phase: f32,
    freq: f32,
    last: i64,
}

pub(super) struct Recorder {
    takes: [Option<Take>; MAX_TAKES],
    pending: [Option<PendingPlay>; MAX_TAKES],
    /// Arrangement record is on.
    pub arrangement: bool,
    /// Beats of count-in remaining.
    pub count_in: f64,
    count_in_total: f64,
    click: Click,
    /// Index of the next frame in the recording stream.
    stream_frame: u64,
    block_active: bool,
    overflowed: bool,
    audio_tx: Producer<f32>,
    events_tx: Producer<RecEvent>,
}

impl Recorder {
    pub fn new(sample_rate: f32) -> (Recorder, RecordTap) {
        let (audio_tx, audio) = RingBuffer::new((RECORD_RING_SECS * sample_rate) as usize * 2);
        let (events_tx, events) = RingBuffer::new(4096);
        (
            Recorder {
                takes: [None; MAX_TAKES],
                pending: [None; MAX_TAKES],
                arrangement: false,
                count_in: 0.0,
                count_in_total: 0.0,
                click: Click {
                    last: -1,
                    ..Default::default()
                },
                stream_frame: 0,
                block_active: false,
                overflowed: false,
                audio_tx,
                events_tx,
            },
            RecordTap { audio, events },
        )
    }

    fn emit(&mut self, e: RecEvent) {
        if self.events_tx.push(e).is_err() {
            self.overflowed = true;
        }
    }

    fn take_index(&self, track: Id) -> Option<usize> {
        self.takes.iter().position(|t| t.map(|t| t.track) == Some(track))
    }

    /// Queues Session recording into an empty slot at the launch boundary.
    pub fn queue_slot(&mut self, track: Id, slot: usize, at: f64, midi: bool) {
        if self.take_index(track).is_some() {
            return;
        }
        self.pending.iter_mut().for_each(|p| {
            if p.map(|p| p.track) == Some(track) {
                *p = None;
            }
        });
        if let Some(free) = self.takes.iter_mut().find(|t| t.is_none()) {
            *free = Some(Take {
                track,
                target: RecTarget::Slot(slot),
                state: TakeState::Queued { at },
                start_beat: at,
                stop_at: None,
                play_after: false,
                midi,
            });
        }
    }

    /// Whether `track` has a MIDI take, queued or recording.
    pub fn records_midi(&self, track: Id) -> bool {
        self.takes.iter().flatten().any(|t| t.track == track && t.midi)
    }

    /// Passes on a note a MIDI track played, if the track has a take.
    pub fn note(&mut self, track: Id, song_beat: f64, session_beat: f64, kind: EventKind) {
        let Some(t) = self.takes.iter().flatten().find(|t| t.track == track && t.midi) else {
            return;
        };
        let beat = match t.target {
            RecTarget::Arrangement => song_beat,
            RecTarget::Slot(_) => session_beat,
        };
        self.emit(RecEvent::Note { track, beat, kind });
    }

    /// A clip launch on a track. Returns true when it ends the track's
    /// Session recording into that very slot (the new clip then plays and the
    /// launch itself must not be queued).
    pub fn launch(&mut self, track: Id, slot: usize, at: f64) -> bool {
        let Some(i) = self.take_index(track) else { return false };
        let Some(take) = self.takes[i].as_mut() else {
            return false;
        };
        let RecTarget::Slot(rec_slot) = take.target else {
            return false;
        };
        match take.state {
            TakeState::Queued { .. } => {
                self.takes[i] = None;
                false
            }
            TakeState::Recording => {
                take.stop_at = Some(at);
                take.play_after = rec_slot == slot;
                take.play_after
            }
        }
    }

    /// Stop button of a track, or a scene launch without a clip for it.
    pub fn stop_track(&mut self, track: Id, at: f64) {
        let Some(i) = self.take_index(track) else { return };
        let Some(take) = self.takes[i].as_mut() else { return };
        if !matches!(take.target, RecTarget::Slot(_)) {
            return;
        }
        match take.state {
            TakeState::Queued { .. } => self.takes[i] = None,
            TakeState::Recording => {
                take.stop_at = Some(at);
                take.play_after = false;
            }
        }
    }

    /// Ends every take immediately (transport stop).
    pub fn stop_all(&mut self, song_beat: f64, session_beat: f64) {
        for i in 0..MAX_TAKES {
            if let Some(t) = self.takes[i].take() {
                if t.state == TakeState::Recording {
                    let beat = match t.target {
                        RecTarget::Arrangement => song_beat,
                        RecTarget::Slot(_) => session_beat,
                    };
                    let frame = self.stream_frame;
                    self.emit(RecEvent::Stopped {
                        track: t.track,
                        target: t.target,
                        beat,
                        frame,
                    });
                }
            }
        }
        self.arrangement = false;
        self.count_in = 0.0;
    }

    pub fn start_count_in(&mut self, beats: f64) {
        self.count_in = beats;
        self.count_in_total = beats;
        self.click.last = -1;
    }

    /// Renders count-in clicks and counts down. Returns true when the
    /// count-in finished inside this segment.
    pub fn render_count_in(&mut self, l: &mut [f32], r: &mut [f32], bpf: f64, beats_per_bar: u32, sr: f32) -> bool {
        let bpb = beats_per_bar.max(1) as i64;
        for i in 0..l.len() {
            let elapsed = self.count_in_total - self.count_in + i as f64 * bpf;
            let ib = (elapsed + 1e-9).floor() as i64;
            if ib > self.click.last && elapsed < self.count_in_total - 1e-9 {
                self.click.last = ib;
                self.click.env = 1.0;
                self.click.phase = 0.0;
                self.click.freq = if ib.rem_euclid(bpb) == 0 { 1568.0 } else { 1046.0 };
            }
            if self.click.env > 1e-4 {
                self.click.phase = (self.click.phase + self.click.freq / sr) % 1.0;
                let s = (self.click.phase * std::f32::consts::TAU).sin() * self.click.env * 0.4;
                self.click.env *= 0.9985;
                l[i] += s;
                r[i] += s;
            }
        }
        self.count_in -= l.len() as f64 * bpf;
        if self.count_in <= 1e-9 {
            self.count_in = 0.0;
            self.click.env = 0.0;
            true
        } else {
            false
        }
    }

    /// Ends Arrangement takes at a loop jump; they restart at the loop start
    /// with the next segment.
    pub fn loop_jump(&mut self, frame_in_block: usize, loop_end: f64) {
        for i in 0..MAX_TAKES {
            if let Some(t) = self.takes[i] {
                if t.target == RecTarget::Arrangement {
                    self.takes[i] = None;
                    let frame = self.stream_frame + frame_in_block as u64;
                    self.emit(RecEvent::Stopped {
                        track: t.track,
                        target: t.target,
                        beat: loop_end,
                        frame,
                    });
                }
            }
        }
    }

    /// Advances takes through one rendered segment: quantized Session starts
    /// and stops, Arrangement punch in/out, and starting newly recorded
    /// clips once the UI has added them to the project.
    #[allow(clippy::too_many_arguments)]
    pub fn segment(
        &mut self,
        project: &mut EngineProject,
        offset: usize,
        frames: usize,
        song0: f64,
        session0: f64,
        bpf: f64,
        playing: bool,
        latency: u32,
        punch: (f64, f64),
    ) {
        let punched = song0 < punch.0 - 1e-9 || song0 >= punch.1 - 1e-9;
        let session1 = session0 + frames as f64 * bpf;
        let base = self.stream_frame + offset as u64;
        let frame_of = |beat: f64| (((beat - session0) / bpf).round().max(0.0) as usize).min(frames);

        for i in 0..MAX_TAKES {
            let Some(mut t) = self.takes[i] else { continue };
            let track = project.tracks.iter_mut().find(|x| x.id == t.track);
            let armed = track
                .as_ref()
                .map(|x| x.mixer.arm && x.kind == if t.midi { TrackKind::Midi } else { TrackKind::Audio })
                .unwrap_or(false);
            // Only audio takes need the input stream.
            let audio = !t.midi;

            // Disarming, deleting the track or stopping ends a take now.
            let arr = t.target == RecTarget::Arrangement;
            if !armed || !playing || (arr && (!self.arrangement || punched)) {
                self.takes[i] = None;
                if t.state == TakeState::Recording {
                    let beat = match t.target {
                        RecTarget::Arrangement => song0,
                        RecTarget::Slot(_) => session0,
                    };
                    self.emit(RecEvent::Stopped {
                        track: t.track,
                        target: t.target,
                        beat,
                        frame: base,
                    });
                    self.block_active |= audio;
                }
                continue;
            }

            if let TakeState::Queued { at } = t.state {
                if at < session1 {
                    let f = frame_of(at);
                    t.state = TakeState::Recording;
                    t.start_beat = at;
                    if let (Some(tr), RecTarget::Slot(_)) = (track, t.target) {
                        // Recording takes the track over from whatever played.
                        if tr.rt.playing.is_some() || !tr.rt.session_active {
                            tr.rt.push(f, EventKind::AllOff);
                        }
                        tr.rt.playing = None;
                        tr.rt.queued = None;
                        tr.rt.session_active = true;
                    }
                    self.emit(RecEvent::Started {
                        track: t.track,
                        target: t.target,
                        beat: at,
                        frame: base + f as u64,
                        latency,
                    });
                    self.block_active |= audio;
                }
            }

            if t.state == TakeState::Recording {
                self.block_active |= audio;
                if let (Some(stop), RecTarget::Slot(slot)) = (t.stop_at, t.target) {
                    if stop < session1 {
                        let f = frame_of(stop);
                        self.takes[i] = None;
                        self.emit(RecEvent::Stopped {
                            track: t.track,
                            target: t.target,
                            beat: stop,
                            frame: base + f as u64,
                        });
                        if t.play_after {
                            if let Some(p) = self.pending.iter_mut().find(|p| p.is_none()) {
                                *p = Some(PendingPlay {
                                    track: t.track,
                                    slot,
                                    beat: stop,
                                });
                            }
                        }
                        continue;
                    }
                }
            }
            self.takes[i] = Some(t);
        }

        // Arrangement punch-in for armed audio and MIDI tracks.
        if self.arrangement && playing {
            for tr in project.tracks.iter() {
                let recordable = matches!(tr.kind, TrackKind::Audio | TrackKind::Midi);
                if !recordable || !tr.mixer.arm || tr.rt.session_active || punched {
                    continue;
                }
                if self.take_index(tr.id).is_some() {
                    continue;
                }
                let Some(free) = self.takes.iter().position(|t| t.is_none()) else {
                    break;
                };
                self.takes[free] = Some(Take {
                    track: tr.id,
                    target: RecTarget::Arrangement,
                    state: TakeState::Recording,
                    start_beat: song0,
                    stop_at: None,
                    play_after: false,
                    midi: tr.kind == TrackKind::Midi,
                });
                self.emit(RecEvent::Started {
                    track: tr.id,
                    target: RecTarget::Arrangement,
                    beat: song0,
                    frame: base,
                    latency,
                });
                self.block_active |= tr.kind == TrackKind::Audio;
            }
        }

        // Start freshly recorded Session clips once they exist.
        for p in self.pending.iter_mut() {
            let Some(pp) = *p else { continue };
            match project.tracks.iter_mut().find(|x| x.id == pp.track) {
                None => *p = None,
                Some(tr) => {
                    if tr.rt.queued.is_some() || tr.rt.playing.is_some() {
                        // Something else was launched meanwhile.
                        *p = None;
                    } else if tr.slots.get(pp.slot).map(|s| s.is_some()).unwrap_or(false) {
                        tr.rt.playing = Some(pp.slot);
                        tr.rt.launch_beat = pp.beat;
                        tr.rt.session_active = true;
                        *p = None;
                    }
                }
            }
        }
    }

    /// Streams this block's input to the UI if anything recorded in it.
    pub fn end_block(&mut self, l: &[f32], r: &[f32]) {
        if !self.block_active {
            return;
        }
        self.block_active = false;
        let n = l.len();
        match self.audio_tx.write_chunk_uninit(n * 2) {
            Ok(chunk) => {
                let it = l.iter().zip(r.iter()).flat_map(|(a, b)| [*a, *b]);
                chunk.fill_from_iter(it);
                self.stream_frame += n as u64;
            }
            Err(_) => {
                // The UI stopped draining; frames are lost and the takes
                // will be shorter than their events say.
                if !self.overflowed {
                    self.overflowed = true;
                    self.emit(RecEvent::Overflow);
                }
            }
        }
    }

    pub fn publish(&self, shared: &RecordShared) {
        shared.arrangement.store(self.arrangement, Ordering::Relaxed);
        shared.count_in.store(self.count_in);
        for (t, s) in self.takes.iter().zip(shared.takes.iter()) {
            match t {
                None => s.track.store(0, Ordering::Relaxed),
                Some(t) => {
                    s.track.store(t.track, Ordering::Relaxed);
                    s.slot.store(
                        match t.target {
                            RecTarget::Arrangement => -1,
                            RecTarget::Slot(x) => x as i32,
                        },
                        Ordering::Relaxed,
                    );
                    s.recording.store(t.state == TakeState::Recording, Ordering::Relaxed);
                    s.start_beat.store(t.start_beat);
                }
            }
        }
    }
}
