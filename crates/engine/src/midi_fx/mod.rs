//! MIDI effects: devices that sit before a track's instrument and transform
//! the notes it receives (Arpeggiator, Chord, Scale, Velocity, Note Length,
//! Pitch, Random).
//!
//! Like audio effects ([`crate::fx`]), each device's settings are a plain
//! `Copy` struct of `f32` parameters described by a static
//! [`ParamInfo`] table, so the Device view draws generic controls and single
//! parameters can be changed (and mapped to MIDI controllers) by index. The
//! audio-thread side is a [`MidiFxChain`]: one runtime per device, allocated
//! on the UI thread and moved between snapshots by device id so held notes
//! and arpeggiator state survive edits.
//!
//! Each device turns the block's note events (sorted by frame) into a new
//! list. Devices never allocate: output goes into a buffer preallocated with
//! [`MAX_MIDI_EVENTS`] capacity and is dropped when that is full.

pub mod arpeggiator;
pub mod chord;
pub mod note_length;
pub mod pitch;
pub mod random;
pub mod scale;
pub mod velocity;

use serde::{Deserialize, Serialize};

pub use crate::engine::{EventKind, MidiEvent};
use crate::fx::ParamInfo;
use crate::model::Id;

/// Capacity of every note event buffer on the audio thread.
pub const MAX_MIDI_EVENTS: usize = 2048;

/// Per-segment timing for MIDI effects.
#[derive(Debug, Clone, Copy)]
pub struct MidiFxContext {
    /// Frames in this segment. Output events must have `frame < frames`.
    pub frames: usize,
    /// Musical position at frame 0 in beats: the session clock while the
    /// transport plays, a free-running clock while it is stopped. It jumps
    /// when the transport starts or relocates.
    pub beat0: f64,
    /// Beats per frame (tempo / 60 / sample rate).
    pub bpf: f64,
    pub sample_rate: f32,
    pub playing: bool,
}

impl MidiFxContext {
    /// Frame at which `beat` falls in this segment, if it does.
    #[inline]
    pub fn frame_of(&self, beat: f64) -> Option<usize> {
        let f = ((beat - self.beat0) / self.bpf).ceil();
        (f >= 0.0 && (f as usize) < self.frames).then_some(f as usize)
    }

    /// Beat at the end of the segment.
    #[inline]
    pub fn beat1(&self) -> f64 {
        self.beat0 + self.frames as f64 * self.bpf
    }
}

/// Appends an event if there is room (never reallocates).
#[inline]
pub(crate) fn emit(out: &mut Vec<MidiEvent>, frame: usize, kind: EventKind) {
    if out.len() < out.capacity() {
        out.push(MidiEvent {
            frame: frame as u32,
            kind,
        });
    }
}

/// The kinds of MIDI effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiDeviceKind {
    Arpeggiator,
    Chord,
    Scale,
    Velocity,
    NoteLength,
    Pitch,
    Random,
}

impl MidiDeviceKind {
    pub const ALL: [MidiDeviceKind; 7] = [
        MidiDeviceKind::Arpeggiator,
        MidiDeviceKind::Chord,
        MidiDeviceKind::NoteLength,
        MidiDeviceKind::Pitch,
        MidiDeviceKind::Random,
        MidiDeviceKind::Scale,
        MidiDeviceKind::Velocity,
    ];

    pub fn name(self) -> &'static str {
        match self {
            MidiDeviceKind::Arpeggiator => "Arpeggiator",
            MidiDeviceKind::Chord => "Chord",
            MidiDeviceKind::Scale => "Scale",
            MidiDeviceKind::Velocity => "Velocity",
            MidiDeviceKind::NoteLength => "Note Length",
            MidiDeviceKind::Pitch => "Pitch",
            MidiDeviceKind::Random => "Random",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            MidiDeviceKind::Arpeggiator => "Plays held notes one after another in a rhythmic pattern",
            MidiDeviceKind::Chord => "Adds up to six notes at fixed intervals to every note",
            MidiDeviceKind::Scale => "Moves every note onto a scale",
            MidiDeviceKind::Velocity => "Shapes, limits and randomizes note velocities",
            MidiDeviceKind::NoteLength => "Sets the length of notes, or plays notes on release",
            MidiDeviceKind::Pitch => "Transposes notes and filters a pitch range",
            MidiDeviceKind::Random => "Randomly changes note pitches",
        }
    }

    pub fn default_params(self) -> MidiDeviceParams {
        match self {
            MidiDeviceKind::Arpeggiator => MidiDeviceParams::Arpeggiator(Default::default()),
            MidiDeviceKind::Chord => MidiDeviceParams::Chord(Default::default()),
            MidiDeviceKind::Scale => MidiDeviceParams::Scale(Default::default()),
            MidiDeviceKind::Velocity => MidiDeviceParams::Velocity(Default::default()),
            MidiDeviceKind::NoteLength => MidiDeviceParams::NoteLength(Default::default()),
            MidiDeviceKind::Pitch => MidiDeviceParams::Pitch(Default::default()),
            MidiDeviceKind::Random => MidiDeviceParams::Random(Default::default()),
        }
    }
}

/// Settings of one MIDI effect.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum MidiDeviceParams {
    Arpeggiator(arpeggiator::ArpeggiatorParams),
    Chord(chord::ChordParams),
    Scale(scale::ScaleParams),
    Velocity(velocity::VelocityParams),
    NoteLength(note_length::NoteLengthParams),
    Pitch(pitch::PitchParams),
    Random(random::RandomParams),
}

/// Calls `$e` with the inner params of every variant bound to `$p`.
macro_rules! each_params {
    ($v:expr, $p:ident => $e:expr) => {
        match $v {
            MidiDeviceParams::Arpeggiator($p) => $e,
            MidiDeviceParams::Chord($p) => $e,
            MidiDeviceParams::Scale($p) => $e,
            MidiDeviceParams::Velocity($p) => $e,
            MidiDeviceParams::NoteLength($p) => $e,
            MidiDeviceParams::Pitch($p) => $e,
            MidiDeviceParams::Random($p) => $e,
        }
    };
}

impl MidiDeviceParams {
    pub fn kind(&self) -> MidiDeviceKind {
        match self {
            MidiDeviceParams::Arpeggiator(_) => MidiDeviceKind::Arpeggiator,
            MidiDeviceParams::Chord(_) => MidiDeviceKind::Chord,
            MidiDeviceParams::Scale(_) => MidiDeviceKind::Scale,
            MidiDeviceParams::Velocity(_) => MidiDeviceKind::Velocity,
            MidiDeviceParams::NoteLength(_) => MidiDeviceKind::NoteLength,
            MidiDeviceParams::Pitch(_) => MidiDeviceKind::Pitch,
            MidiDeviceParams::Random(_) => MidiDeviceKind::Random,
        }
    }

    pub fn info(&self) -> &'static [ParamInfo] {
        match self {
            MidiDeviceParams::Arpeggiator(_) => arpeggiator::ArpeggiatorParams::INFO,
            MidiDeviceParams::Chord(_) => chord::ChordParams::INFO,
            MidiDeviceParams::Scale(_) => scale::ScaleParams::INFO,
            MidiDeviceParams::Velocity(_) => velocity::VelocityParams::INFO,
            MidiDeviceParams::NoteLength(_) => note_length::NoteLengthParams::INFO,
            MidiDeviceParams::Pitch(_) => pitch::PitchParams::INFO,
            MidiDeviceParams::Random(_) => random::RandomParams::INFO,
        }
    }

    pub fn get(&self, index: usize) -> f32 {
        each_params!(self, p => p.get(index))
    }

    pub fn set(&mut self, index: usize, value: f32) {
        each_params!(self, p => p.set(index, value))
    }

    /// Whether a parameter matters with the current settings (hidden in the
    /// Device view otherwise).
    pub fn is_relevant(&self, index: usize) -> bool {
        match self {
            MidiDeviceParams::NoteLength(p) => p.is_relevant(index),
            _ => true,
        }
    }
}

/// One MIDI effect on a track, as saved in the project.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MidiDevice {
    pub id: Id,
    /// Device activator; a switched-off device passes notes through.
    pub on: bool,
    pub params: MidiDeviceParams,
}

impl MidiDevice {
    pub fn new(id: Id, kind: MidiDeviceKind) -> Self {
        Self {
            id,
            on: true,
            params: kind.default_params(),
        }
    }

    pub fn kind(&self) -> MidiDeviceKind {
        self.params.kind()
    }
}

/// The audio-thread state of one device.
enum Runtime {
    Arpeggiator(Box<arpeggiator::Arpeggiator>),
    Chord(Box<chord::Chord>),
    Scale(Box<scale::Scale>),
    Velocity(Box<velocity::Velocity>),
    NoteLength(Box<note_length::NoteLength>),
    Pitch(Box<pitch::Pitch>),
    Random(Box<random::Random>),
}

impl Runtime {
    fn new(id: Id, params: &MidiDeviceParams, sr: f32) -> Self {
        // Seeds differ per device so two Random devices don't move together.
        let seed = (id as u32).wrapping_mul(2_654_435_761) | 1;
        match params {
            MidiDeviceParams::Arpeggiator(_) => Runtime::Arpeggiator(Box::new(arpeggiator::Arpeggiator::new(seed))),
            MidiDeviceParams::Chord(_) => Runtime::Chord(Box::default()),
            MidiDeviceParams::Scale(_) => Runtime::Scale(Box::default()),
            MidiDeviceParams::Velocity(_) => Runtime::Velocity(Box::new(velocity::Velocity::new(seed))),
            MidiDeviceParams::NoteLength(_) => Runtime::NoteLength(Box::new(note_length::NoteLength::new(sr))),
            MidiDeviceParams::Pitch(_) => Runtime::Pitch(Box::default()),
            MidiDeviceParams::Random(_) => Runtime::Random(Box::new(random::Random::new(seed))),
        }
    }

    fn matches(&self, params: &MidiDeviceParams) -> bool {
        matches!(
            (self, params),
            (Runtime::Arpeggiator(_), MidiDeviceParams::Arpeggiator(_))
                | (Runtime::Chord(_), MidiDeviceParams::Chord(_))
                | (Runtime::Scale(_), MidiDeviceParams::Scale(_))
                | (Runtime::Velocity(_), MidiDeviceParams::Velocity(_))
                | (Runtime::NoteLength(_), MidiDeviceParams::NoteLength(_))
                | (Runtime::Pitch(_), MidiDeviceParams::Pitch(_))
                | (Runtime::Random(_), MidiDeviceParams::Random(_))
        )
    }

    fn process(
        &mut self,
        params: &MidiDeviceParams,
        input: &[MidiEvent],
        out: &mut Vec<MidiEvent>,
        ctx: &MidiFxContext,
    ) {
        match (self, params) {
            (Runtime::Arpeggiator(d), MidiDeviceParams::Arpeggiator(p)) => d.process(p, input, out, ctx),
            (Runtime::Chord(d), MidiDeviceParams::Chord(p)) => d.process(p, input, out, ctx),
            (Runtime::Scale(d), MidiDeviceParams::Scale(p)) => d.process(p, input, out, ctx),
            (Runtime::Velocity(d), MidiDeviceParams::Velocity(p)) => d.process(p, input, out, ctx),
            (Runtime::NoteLength(d), MidiDeviceParams::NoteLength(p)) => d.process(p, input, out, ctx),
            (Runtime::Pitch(d), MidiDeviceParams::Pitch(p)) => d.process(p, input, out, ctx),
            (Runtime::Random(d), MidiDeviceParams::Random(p)) => d.process(p, input, out, ctx),
            _ => out.extend_from_slice(&input[..input.len().min(out.capacity() - out.len())]),
        }
    }

    fn reset(&mut self) {
        match self {
            Runtime::Arpeggiator(d) => d.reset(),
            Runtime::Chord(d) => d.reset(),
            Runtime::Scale(d) => d.reset(),
            Runtime::Velocity(d) => d.reset(),
            Runtime::NoteLength(d) => d.reset(),
            Runtime::Pitch(d) => d.reset(),
            Runtime::Random(d) => d.reset(),
        }
    }
}

struct ChainDevice {
    id: Id,
    on: bool,
    params: MidiDeviceParams,
    rt: Runtime,
    /// The device was switched off or reset: release what it was playing.
    release: bool,
}

/// A track's MIDI effects on the audio thread.
#[derive(Default)]
pub struct MidiFxChain {
    devices: Vec<ChainDevice>,
    scratch: Vec<MidiEvent>,
}

impl MidiFxChain {
    /// Builds the chain. Runs on the UI thread (allocates).
    pub fn new(devices: &[MidiDevice], sample_rate: f32) -> Self {
        Self {
            devices: devices
                .iter()
                .map(|d| ChainDevice {
                    id: d.id,
                    on: d.on,
                    params: d.params,
                    rt: Runtime::new(d.id, &d.params, sample_rate),
                    release: false,
                })
                .collect(),
            scratch: if devices.is_empty() {
                Vec::new()
            } else {
                Vec::with_capacity(MAX_MIDI_EVENTS)
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// Takes over the runtime state of devices that exist in `old` (same id
    /// and kind). Audio thread; swaps boxes, never allocates.
    pub fn adopt(&mut self, old: &mut MidiFxChain) {
        for d in self.devices.iter_mut() {
            if let Some(o) = old.devices.iter_mut().find(|o| o.id == d.id) {
                if o.rt.matches(&d.params) {
                    std::mem::swap(&mut d.rt, &mut o.rt);
                    d.release |= o.release;
                }
            }
        }
    }

    pub fn set_param(&mut self, device: Id, index: usize, value: f32) -> bool {
        match self.devices.iter_mut().find(|d| d.id == device) {
            Some(d) => {
                d.params.set(index, value);
                true
            }
            None => false,
        }
    }

    pub fn set_params(&mut self, device: Id, params: MidiDeviceParams) -> bool {
        match self.devices.iter_mut().find(|d| d.id == device) {
            Some(d) if d.params.kind() == params.kind() => {
                d.params = params;
                true
            }
            _ => false,
        }
    }

    pub fn set_on(&mut self, device: Id, on: bool) -> bool {
        match self.devices.iter_mut().find(|d| d.id == device) {
            Some(d) => {
                if d.on != on {
                    d.on = on;
                    d.release = true;
                }
                true
            }
            None => false,
        }
    }

    /// Forgets held notes and pending events in every device.
    pub fn reset(&mut self) {
        for d in self.devices.iter_mut() {
            d.rt.reset();
        }
    }

    /// Runs the block's events (sorted by frame) through every active device
    /// in order. On return `events` holds the result, sorted. `events` must
    /// have [`MAX_MIDI_EVENTS`] capacity.
    pub fn process(&mut self, events: &mut Vec<MidiEvent>, ctx: &MidiFxContext) {
        let mut released = false;
        for d in self.devices.iter_mut() {
            if std::mem::take(&mut d.release) {
                // Switching a device on or off can strand notes it started
                // (or hid); silence the instrument and start over.
                d.rt.reset();
                released = true;
            }
            if !d.on {
                continue;
            }
            self.scratch.clear();
            d.rt.process(&d.params, events, &mut self.scratch, ctx);
            std::mem::swap(events, &mut self.scratch);
            events.sort_unstable_by_key(|e| e.order());
        }
        if released && events.len() < events.capacity() {
            events.insert(
                0,
                MidiEvent {
                    frame: 0,
                    kind: EventKind::AllOff,
                },
            );
        }
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    pub fn ctx(frames: usize) -> MidiFxContext {
        // 120 BPM at 48 kHz.
        MidiFxContext {
            frames,
            beat0: 0.0,
            bpf: 120.0 / 60.0 / 48000.0,
            sample_rate: 48000.0,
            playing: true,
        }
    }

    pub fn on(frame: u32, pitch: u8, velocity: u8) -> MidiEvent {
        MidiEvent {
            frame,
            kind: EventKind::On(pitch, velocity),
        }
    }

    pub fn off(frame: u32, pitch: u8) -> MidiEvent {
        MidiEvent {
            frame,
            kind: EventKind::Off(pitch),
        }
    }

    pub fn buf() -> Vec<MidiEvent> {
        Vec::with_capacity(MAX_MIDI_EVENTS)
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::*;
    use super::*;

    #[test]
    fn params_round_trip_by_index() {
        for kind in MidiDeviceKind::ALL {
            let mut p = kind.default_params();
            assert_eq!(p.kind(), kind);
            for (i, info) in p.info().iter().enumerate() {
                assert_eq!(p.get(i), info.default, "{} {}", kind.name(), info.name);
                assert!(info.min <= info.default && info.default <= info.max, "{}", info.name);
                p.set(i, info.max + 1000.0);
                assert_eq!(p.get(i), info.clamp(info.max), "{} {}", kind.name(), info.name);
            }
        }
    }

    #[test]
    fn devices_serialize() {
        for kind in MidiDeviceKind::ALL {
            let d = MidiDevice::new(3, kind);
            let json = serde_json::to_string(&d).unwrap();
            let back: MidiDevice = serde_json::from_str(&json).unwrap();
            assert_eq!(back, d);
        }
    }

    #[test]
    fn switched_off_devices_pass_notes_through_and_release() {
        let devices = [MidiDevice::new(1, MidiDeviceKind::Pitch)];
        let mut chain = MidiFxChain::new(&devices, 48000.0);
        chain.set_param(1, 0, 12.0);
        let mut ev = buf();
        ev.push(on(0, 60, 100));
        chain.process(&mut ev, &ctx(64));
        assert_eq!(ev[0].kind, EventKind::On(72, 100));

        chain.set_on(1, false);
        let mut ev = buf();
        ev.push(on(5, 60, 100));
        chain.process(&mut ev, &ctx(64));
        assert_eq!(ev[0].kind, EventKind::AllOff);
        assert_eq!(ev[1].kind, EventKind::On(60, 100));
    }
}
