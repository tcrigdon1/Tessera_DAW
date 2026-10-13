//! Instruments and DSP building blocks.
//!
//! Everything here is allocation-free after construction so it can run on the
//! audio thread.

pub mod drums;
pub mod envelope;
pub mod filter;
pub mod synth;

use crate::model::Instrument;

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
}

impl InstrumentRuntime {
    pub fn new(instrument: &Instrument, sample_rate: f32) -> Self {
        match instrument {
            Instrument::None => InstrumentRuntime::None,
            Instrument::Synth(_) => InstrumentRuntime::Synth(Box::new(synth::PolySynth::new(sample_rate))),
            Instrument::DrumKit(_) => InstrumentRuntime::Drums(Box::new(drums::DrumKit::new(sample_rate))),
        }
    }

    pub fn kind_tag(&self) -> u8 {
        match self {
            InstrumentRuntime::None => 0,
            InstrumentRuntime::Synth(_) => 1,
            InstrumentRuntime::Drums(_) => 2,
        }
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        match self {
            InstrumentRuntime::None => {}
            InstrumentRuntime::Synth(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::Drums(d) => d.note_on(pitch, velocity),
        }
    }

    pub fn note_off(&mut self, pitch: u8) {
        match self {
            InstrumentRuntime::None => {}
            InstrumentRuntime::Synth(s) => s.note_off(pitch),
            InstrumentRuntime::Drums(_) => {}
        }
    }

    pub fn all_notes_off(&mut self) {
        match self {
            InstrumentRuntime::None => {}
            InstrumentRuntime::Synth(s) => s.all_notes_off(),
            InstrumentRuntime::Drums(_) => {}
        }
    }

    /// Hard reset: silences everything immediately, including tails.
    pub fn panic(&mut self) {
        match self {
            InstrumentRuntime::None => {}
            InstrumentRuntime::Synth(s) => s.panic(),
            InstrumentRuntime::Drums(d) => d.panic(),
        }
    }

    /// Adds the instrument's output to `left`/`right`.
    pub fn render(&mut self, instrument: &Instrument, left: &mut [f32], right: &mut [f32]) {
        match (self, instrument) {
            (InstrumentRuntime::Synth(s), Instrument::Synth(p)) => s.render(p, left, right),
            (InstrumentRuntime::Drums(d), Instrument::DrumKit(p)) => d.render(p, left, right),
            _ => {}
        }
    }
}
