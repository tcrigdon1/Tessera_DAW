//! Instruments and DSP building blocks.
//!
//! Everything here is allocation-free after construction so it can run on the
//! audio thread.

pub mod drum_rack;
pub mod drums;
pub mod envelope;
pub mod filter;
pub mod fm;
pub mod sample;
pub mod sampler;
pub mod synth;
pub mod wavetable;

use crate::model::Instrument;
use sample::SampleSlots;

/// A tiny, fast pseudo-random generator for noise sources.
#[derive(Debug, Clone)]
pub struct Rng(u32);

impl Rng {
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }
    /// White noise in -1..1.
    #[inline]
    pub fn next_bipolar(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// The audio-thread side of a track's instrument.
pub enum InstrumentRuntime {
    None,
    Synth(Box<synth::PolySynth>),
    Drums(Box<drums::DrumKit>),
    Sampler(Box<sampler::Sampler>),
    DrumRack(Box<drum_rack::DrumRack>),
    Fm(Box<fm::FmSynth>),
    Wavetable(Box<wavetable::Wavetable>),
    /// A hosted plug-in. Its processor travels with the snapshot
    /// (`EngineTrack::plugin`); `release` asks the engine to send it an
    /// all-notes-off at the start of the next block.
    Plugin {
        release: bool,
    },
}

impl InstrumentRuntime {
    pub fn new(instrument: &Instrument, sample_rate: f32) -> Self {
        match instrument {
            Instrument::None => InstrumentRuntime::None,
            Instrument::Synth(_) => InstrumentRuntime::Synth(Box::new(synth::PolySynth::new(sample_rate))),
            Instrument::DrumKit(_) => InstrumentRuntime::Drums(Box::new(drums::DrumKit::new(sample_rate))),
            Instrument::Sampler(_) => InstrumentRuntime::Sampler(Box::new(sampler::Sampler::new(sample_rate))),
            Instrument::DrumRack(_) => InstrumentRuntime::DrumRack(Box::new(drum_rack::DrumRack::new(sample_rate))),
            Instrument::Fm(_) => InstrumentRuntime::Fm(Box::new(fm::FmSynth::new(sample_rate))),
            Instrument::Wavetable(_) => InstrumentRuntime::Wavetable(Box::new(wavetable::Wavetable::new(sample_rate))),
            Instrument::Plugin(_) => InstrumentRuntime::Plugin { release: false },
        }
    }

    /// Matches [`Instrument::kind_tag`].
    pub fn kind_tag(&self) -> u8 {
        match self {
            InstrumentRuntime::None => 0,
            InstrumentRuntime::Synth(_) => 1,
            InstrumentRuntime::Drums(_) => 2,
            InstrumentRuntime::Sampler(_) => 3,
            InstrumentRuntime::DrumRack(_) => 4,
            InstrumentRuntime::Fm(_) => 5,
            InstrumentRuntime::Wavetable(_) => 6,
            InstrumentRuntime::Plugin { .. } => 7,
        }
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        match self {
            InstrumentRuntime::None | InstrumentRuntime::Plugin { .. } => {}
            InstrumentRuntime::Synth(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::Drums(d) => d.note_on(pitch, velocity),
            InstrumentRuntime::Sampler(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::DrumRack(d) => d.note_on(pitch, velocity),
            InstrumentRuntime::Fm(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::Wavetable(s) => s.note_on(pitch, velocity),
        }
    }

    pub fn note_off(&mut self, pitch: u8) {
        match self {
            InstrumentRuntime::Synth(s) => s.note_off(pitch),
            InstrumentRuntime::Sampler(s) => s.note_off(pitch),
            InstrumentRuntime::Fm(s) => s.note_off(pitch),
            InstrumentRuntime::Wavetable(s) => s.note_off(pitch),
            InstrumentRuntime::None
            | InstrumentRuntime::Drums(_)
            | InstrumentRuntime::DrumRack(_)
            | InstrumentRuntime::Plugin { .. } => {}
        }
    }

    pub fn all_notes_off(&mut self) {
        match self {
            InstrumentRuntime::Synth(s) => s.all_notes_off(),
            InstrumentRuntime::Sampler(s) => s.all_notes_off(),
            InstrumentRuntime::Fm(s) => s.all_notes_off(),
            InstrumentRuntime::Wavetable(s) => s.all_notes_off(),
            InstrumentRuntime::DrumRack(d) => d.all_notes_off(),
            InstrumentRuntime::Plugin { release } => *release = true,
            InstrumentRuntime::None | InstrumentRuntime::Drums(_) => {}
        }
    }

    /// Hard reset: silences everything immediately, including tails.
    pub fn panic(&mut self) {
        match self {
            InstrumentRuntime::None => {}
            InstrumentRuntime::Plugin { release } => *release = true,
            InstrumentRuntime::Synth(s) => s.panic(),
            InstrumentRuntime::Drums(d) => d.panic(),
            InstrumentRuntime::Sampler(s) => s.panic(),
            InstrumentRuntime::DrumRack(d) => d.panic(),
            InstrumentRuntime::Fm(s) => s.panic(),
            InstrumentRuntime::Wavetable(s) => s.panic(),
        }
    }

    /// Adds the instrument's output to `left`/`right`. `samples` are the
    /// track's loaded samples (see [`crate::model::Track::samples`]).
    pub fn render(&mut self, instrument: &Instrument, samples: &SampleSlots, left: &mut [f32], right: &mut [f32]) {
        match (self, instrument) {
            (InstrumentRuntime::Synth(s), Instrument::Synth(p)) => s.render(p, left, right),
            (InstrumentRuntime::Drums(d), Instrument::DrumKit(p)) => d.render(p, left, right),
            (InstrumentRuntime::Sampler(s), Instrument::Sampler(p)) => s.render(p, samples, left, right),
            (InstrumentRuntime::DrumRack(d), Instrument::DrumRack(p)) => d.render(p, samples, left, right),
            (InstrumentRuntime::Fm(s), Instrument::Fm(p)) => s.render(p, left, right),
            (InstrumentRuntime::Wavetable(s), Instrument::Wavetable(p)) => s.render(p, left, right),
            _ => {}
        }
    }

    /// Bitmask of Drum Rack pads hit since the last call (for pad lights).
    pub fn take_pad_hits(&mut self) -> u16 {
        match self {
            InstrumentRuntime::DrumRack(d) => std::mem::take(&mut d.hits),
            _ => 0,
        }
    }
}
