//! Instruments and DSP building blocks.
//!
//! Everything here is allocation-free after construction so it can run on the
//! audio thread.

pub mod analog;
pub mod drum_rack;
pub mod drums;
pub mod electric;
pub mod envelope;
pub mod filter;
pub mod fm;
pub mod granular;
pub mod mallet;
pub mod sample;
pub mod sampler;
pub mod simple;
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
    /// Uniform in 0..1.
    #[inline]
    pub fn next_unit(&mut self) -> f32 {
        ((self.next_bipolar() + 1.0) * 0.5).min(0.999_999)
    }
}

pub fn midi_to_hz(note: f32) -> f32 {
    440.0 * 2f32.powf((note - 69.0) / 12.0)
}

/// Pitch bend range of the built-in instruments, in semitones each way.
pub const BEND_RANGE: f32 = 2.0;

/// What the track tells its instrument besides notes: the pitch wheel and
/// the song tempo (for tempo-synced LFOs). Set before every render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Performance {
    /// Pitch bend in semitones.
    pub bend: f32,
    pub tempo: f32,
    /// Per-note pitch offset in semitones (note expression), indexed by MIDI note.
    pub note_bend: [f32; 128],
    /// Per-note level multiplier (note expression pressure), 1.0 = unchanged.
    pub note_gain: [f32; 128],
}

impl Default for Performance {
    fn default() -> Self {
        Self {
            bend: 0.0,
            tempo: 120.0,
            note_bend: [0.0; 128],
            note_gain: [1.0; 128],
        }
    }
}

impl Performance {
    /// Track bend plus the note's own expression bend.
    #[inline]
    pub fn bend_for(&self, note: u8) -> f32 {
        self.bend + self.note_bend[note as usize & 127]
    }
    /// The note's own expression level multiplier.
    #[inline]
    pub fn gain_for(&self, note: u8) -> f32 {
        self.note_gain[note as usize & 127]
    }
}

/// Per-sample one-pole coefficient (~5 ms) for smoothing a voice's
/// note-expression gain toward [`Performance::gain_for`], so pressure
/// changes don't click.
#[inline]
pub fn gain_smooth_coef(sr: f32) -> f32 {
    1.0 - (-1.0 / (0.005 * sr.max(1.0))).exp()
}

/// A voice's smoothed note-expression gain. It snaps to its target on the
/// first sample after [`ExprGain::reset`] (a fresh note), then glides.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExprGain(f32);

impl Default for ExprGain {
    fn default() -> Self {
        Self(-1.0)
    }
}

impl ExprGain {
    /// Makes the next [`ExprGain::next`] jump straight to its target.
    #[inline]
    pub fn reset(&mut self) {
        self.0 = -1.0;
    }
    /// Advances one sample toward `target` with coefficient `k`
    /// (see [`gain_smooth_coef`]) and returns the gain.
    #[inline]
    pub fn next(&mut self, target: f32, k: f32) -> f32 {
        if self.0 < 0.0 {
            self.0 = target;
        } else {
            self.0 += (target - self.0) * k;
        }
        self.0
    }
}

/// Tempo-synced LFO rates, for a `Choice` parameter.
pub const SYNC_RATE_NAMES: &[&str] = &[
    "4 Bars", "2 Bars", "1 Bar", "1/2", "1/4", "1/4T", "1/8", "1/8T", "1/16", "1/16T", "1/32",
];
/// Beats per LFO cycle of each [`SYNC_RATE_NAMES`] entry.
const SYNC_RATE_BEATS: [f32; 11] = [
    16.0,
    8.0,
    4.0,
    2.0,
    1.0,
    2.0 / 3.0,
    0.5,
    1.0 / 3.0,
    0.25,
    1.0 / 6.0,
    0.125,
];
/// The synced-rate parameter kind of the instruments' LFOs.
pub const SYNC_RATES: crate::fx::ParamKind = crate::fx::ParamKind::Choice(SYNC_RATE_NAMES);
/// The default synced rate: 1/4.
pub const SYNC_RATE_DEFAULT: f32 = 4.0;

/// An LFO's rate in Hz: `hz` when free, or the `rate`-th synced division at
/// `tempo` when `sync` is on.
pub fn lfo_hz(hz: f32, sync: bool, rate: f32, tempo: f32) -> f32 {
    if !sync {
        return hz;
    }
    let i = (rate.round().max(0.0) as usize).min(SYNC_RATE_BEATS.len() - 1);
    tempo.max(1.0) / 60.0 / SYNC_RATE_BEATS[i]
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
    /// An Instrument Rack; its chains travel with the snapshot
    /// (`EngineTrack::rack`).
    Rack,
    Analog(Box<analog::AnalogSynth>),
    Electric(Box<electric::ElectricPiano>),
    Mallet(Box<mallet::MalletSynth>),
    Simple(Box<simple::SimpleSynth>),
    Granular(Box<granular::GranularSynth>),
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
            Instrument::Rack(_) => InstrumentRuntime::Rack,
            Instrument::Analog(_) => InstrumentRuntime::Analog(Box::new(analog::AnalogSynth::new(sample_rate))),
            Instrument::Electric(_) => InstrumentRuntime::Electric(Box::new(electric::ElectricPiano::new(sample_rate))),
            Instrument::Mallet(_) => InstrumentRuntime::Mallet(Box::new(mallet::MalletSynth::new(sample_rate))),
            Instrument::Simple(_) => InstrumentRuntime::Simple(Box::new(simple::SimpleSynth::new(sample_rate))),
            Instrument::Granular(_) => InstrumentRuntime::Granular(Box::new(granular::GranularSynth::new(sample_rate))),
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
            InstrumentRuntime::Rack => 8,
            InstrumentRuntime::Analog(_) => 9,
            InstrumentRuntime::Electric(_) => 10,
            InstrumentRuntime::Mallet(_) => 11,
            InstrumentRuntime::Simple(_) => 12,
            InstrumentRuntime::Granular(_) => 13,
        }
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        match self {
            InstrumentRuntime::None | InstrumentRuntime::Plugin { .. } | InstrumentRuntime::Rack => {}
            InstrumentRuntime::Analog(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::Electric(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::Mallet(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::Simple(s) => s.note_on(pitch, velocity),
            InstrumentRuntime::Granular(s) => s.note_on(pitch, velocity),
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
            InstrumentRuntime::Analog(s) => s.note_off(pitch),
            InstrumentRuntime::Electric(s) => s.note_off(pitch),
            InstrumentRuntime::Mallet(s) => s.note_off(pitch),
            InstrumentRuntime::Simple(s) => s.note_off(pitch),
            InstrumentRuntime::Granular(s) => s.note_off(pitch),
            InstrumentRuntime::None
            | InstrumentRuntime::Drums(_)
            | InstrumentRuntime::DrumRack(_)
            | InstrumentRuntime::Rack
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
            InstrumentRuntime::Analog(s) => s.all_notes_off(),
            InstrumentRuntime::Electric(s) => s.all_notes_off(),
            InstrumentRuntime::Mallet(s) => s.all_notes_off(),
            InstrumentRuntime::Simple(s) => s.all_notes_off(),
            InstrumentRuntime::Granular(s) => s.all_notes_off(),
            InstrumentRuntime::None | InstrumentRuntime::Drums(_) | InstrumentRuntime::Rack => {}
        }
    }

    /// Hard reset: silences everything immediately, including tails.
    pub fn panic(&mut self) {
        match self {
            InstrumentRuntime::None | InstrumentRuntime::Rack => {}
            InstrumentRuntime::Plugin { release } => *release = true,
            InstrumentRuntime::Analog(s) => s.panic(),
            InstrumentRuntime::Electric(s) => s.panic(),
            InstrumentRuntime::Mallet(s) => s.panic(),
            InstrumentRuntime::Simple(s) => s.panic(),
            InstrumentRuntime::Granular(s) => s.panic(),
            InstrumentRuntime::Synth(s) => s.panic(),
            InstrumentRuntime::Drums(d) => d.panic(),
            InstrumentRuntime::Sampler(s) => s.panic(),
            InstrumentRuntime::DrumRack(d) => d.panic(),
            InstrumentRuntime::Fm(s) => s.panic(),
            InstrumentRuntime::Wavetable(s) => s.panic(),
        }
    }

    /// Hands the instrument the pitch wheel and tempo for the next render.
    pub fn set_performance(&mut self, perf: Performance) {
        match self {
            InstrumentRuntime::Synth(s) => s.perf = perf,
            InstrumentRuntime::Sampler(s) => s.perf = perf,
            InstrumentRuntime::Fm(s) => s.perf = perf,
            InstrumentRuntime::Wavetable(s) => s.perf = perf,
            InstrumentRuntime::Analog(s) => s.perf = perf,
            InstrumentRuntime::Electric(s) => s.perf = perf,
            InstrumentRuntime::Mallet(s) => s.perf = perf,
            InstrumentRuntime::Simple(s) => s.perf = perf,
            InstrumentRuntime::Granular(s) => s.perf = perf,
            // Drums don't bend; plug-ins get the tempo from the transport.
            InstrumentRuntime::None
            | InstrumentRuntime::Drums(_)
            | InstrumentRuntime::DrumRack(_)
            | InstrumentRuntime::Rack
            | InstrumentRuntime::Plugin { .. } => {}
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
            (InstrumentRuntime::Analog(s), Instrument::Analog(p)) => s.render(p, left, right),
            (InstrumentRuntime::Electric(s), Instrument::Electric(p)) => s.render(p, left, right),
            (InstrumentRuntime::Mallet(s), Instrument::Mallet(p)) => s.render(p, left, right),
            (InstrumentRuntime::Simple(s), Instrument::Simple(p)) => s.render(p, left, right),
            (InstrumentRuntime::Granular(s), Instrument::Granular(p)) => s.render(p, samples, left, right),
            _ => {}
        }
    }

    /// Like [`InstrumentRuntime::render`], with Drum Rack pads that have
    /// their own chain written into `bus`.
    pub fn render_routed(
        &mut self,
        instrument: &Instrument,
        samples: &SampleSlots,
        left: &mut [f32],
        right: &mut [f32],
        bus: Option<&mut drum_rack::PadBus>,
    ) {
        match (self, instrument) {
            (InstrumentRuntime::DrumRack(d), Instrument::DrumRack(p)) => d.render_routed(p, samples, left, right, bus),
            (s, i) => s.render(i, samples, left, right),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn render(i: &Instrument, perf: Performance) -> Vec<f32> {
        let mut rt = InstrumentRuntime::new(i, 48000.0);
        rt.set_performance(perf);
        rt.note_on(60, 100);
        let (mut l, mut r) = (vec![0.0; 9600], vec![0.0; 9600]);
        for (l, r) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            rt.render(i, &[], l, r);
        }
        l
    }

    #[test]
    fn pitch_bend_reaches_every_pitched_instrument() {
        let instruments = [
            Instrument::Synth(Default::default()),
            Instrument::Fm(Default::default()),
            Instrument::Wavetable(Default::default()),
            Instrument::Analog(Default::default()),
            Instrument::Electric(Default::default()),
            Instrument::Mallet(Default::default()),
            Instrument::Simple(Default::default()),
        ];
        for i in instruments.iter() {
            let flat = render(i, Performance::default());
            let bent = render(
                i,
                Performance {
                    bend: 2.0,
                    ..Default::default()
                },
            );
            let diff: f32 = flat.iter().zip(&bent).map(|(a, b)| (a - b).abs()).sum();
            assert!(diff > 1.0, "{} ignores the pitch wheel", i.name());
        }
    }

    #[test]
    fn note_expression_reaches_every_pitched_instrument() {
        let instruments = [
            Instrument::Synth(Default::default()),
            Instrument::Fm(Default::default()),
            Instrument::Wavetable(Default::default()),
            Instrument::Analog(Default::default()),
            Instrument::Electric(Default::default()),
            Instrument::Mallet(Default::default()),
            Instrument::Simple(Default::default()),
        ];
        for i in instruments.iter() {
            let flat = render(i, Performance::default());
            let mut other = Performance::default();
            other.note_bend[61] = 2.0;
            other.note_gain[61] = 0.0;
            assert_eq!(flat, render(i, other), "{}: another note's expression leaks", i.name());
            let mut bent = Performance::default();
            bent.note_bend[60] = 2.0;
            let diff: f32 = flat.iter().zip(&render(i, bent)).map(|(a, b)| (a - b).abs()).sum();
            assert!(diff > 1.0, "{} ignores the note bend", i.name());
            let mut muted = Performance::default();
            muted.note_gain[60] = 0.0;
            let peak = render(i, muted).iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(peak < 1e-6, "{} ignores the note gain: {peak}", i.name());
        }
    }

    #[test]
    fn synced_lfo_rates_follow_the_tempo() {
        assert_eq!(lfo_hz(3.0, false, 4.0, 90.0), 3.0);
        // A quarter note at 120 BPM is 2 Hz, a bar at 120 is 0.5 Hz.
        assert!((lfo_hz(3.0, true, 4.0, 120.0) - 2.0).abs() < 1e-6);
        assert!((lfo_hz(3.0, true, 2.0, 120.0) - 0.5).abs() < 1e-6);
        assert!((lfo_hz(3.0, true, 8.0, 60.0) - 4.0).abs() < 1e-6);
        assert_eq!(SYNC_RATE_NAMES.len(), SYNC_RATE_BEATS.len());
        assert_eq!(SYNC_RATE_NAMES[SYNC_RATE_DEFAULT as usize], "1/4");
    }

    #[test]
    fn lfo_sync_indices_name_the_right_parameters() {
        let names = |info: &[crate::fx::ParamInfo], rate: usize, sync: usize| {
            (info[rate].name, info[sync].name, info[sync + 1].name)
        };
        let a = analog::AnalogParams::INFO;
        assert_eq!(
            names(a, analog::idx::L1_SHAPE + 1, analog::idx::L1_SYNC),
            ("LFO1 Rate", "LFO1 Sync", "LFO1 Sync Rate")
        );
        assert_eq!(
            names(a, analog::idx::L2_SHAPE + 1, analog::idx::L2_SYNC),
            ("LFO2 Rate", "LFO2 Sync", "LFO2 Sync Rate")
        );
        let s = simple::SimpleParams::INFO;
        assert_eq!(
            names(s, simple::idx::LFO_SHAPE + 1, simple::idx::LFO_SYNC),
            ("LFO Rate", "LFO Sync", "LFO Sync Rate")
        );
        let m = mallet::MalletParams::INFO;
        assert_eq!(
            names(m, mallet::LFO_RATE, mallet::LFO_SYNC),
            ("LFO Rate", "LFO Sync", "LFO Sync Rate")
        );
        let g = granular::GranularParams::INFO;
        assert_eq!(
            names(g, granular::LFO_RATE, granular::LFO_SYNC),
            ("LFO Rate", "LFO Sync", "LFO Sync Rate")
        );
    }

    #[test]
    fn synced_lfos_change_with_the_tempo() {
        let mut a = analog::AnalogParams {
            l1_amp: 1.0,
            l1_sync: 1.0,
            l1_sync_rate: 8.0,
            ..Default::default()
        };
        let differs = |i: &Instrument| {
            let slow = render(
                i,
                Performance {
                    tempo: 60.0,
                    ..Default::default()
                },
            );
            let fast = render(
                i,
                Performance {
                    tempo: 180.0,
                    ..Default::default()
                },
            );
            slow.iter().zip(&fast).map(|(a, b)| (a - b).abs()).sum::<f32>() > 1.0
        };
        assert!(differs(&Instrument::Analog(a)));
        a.l1_sync = 0.0;
        assert!(!differs(&Instrument::Analog(a)), "a free LFO ignores the tempo");
    }
}
