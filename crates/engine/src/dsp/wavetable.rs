//! "Wavetable": a two-oscillator morphing wavetable synthesizer.
//!
//! Signal flow per voice:
//!
//! ```text
//! Osc 1 (unison) ─┐                 ┌─ Filter 1 ─┐
//! Sub (sine -1oct)┤── routing ──────┤            ├── amp env × volume × pan ── out
//! Osc 2 (unison) ─┤                 └─ Filter 2 ─┘
//! Noise ──────────┘
//! ```
//!
//! The oscillators read from a built-in bank of procedurally generated,
//! band-limited wavetables (see [`bank`]). Modulation (Env 2/3, LFO 1/2,
//! velocity, key tracking and Macro 1) runs at control rate, once every
//! [`CONTROL`] samples, and is routed through an 8-slot matrix. Continuous
//! targets (wavetable position, levels, cutoff, output gain) are linearly
//! ramped between control ticks so they never zipper.
//!
//! Everything after construction is allocation-free: the wavetable bank is a
//! process-wide `OnceLock` built by [`Wavetable::new`] (on the UI thread) and
//! never freed.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::envelope::{exp_coef, Adsr};
use super::filter::Svf;
use super::{midi_to_hz, Rng};
use crate::model::db_to_gain;

/// Polyphony.
pub const VOICES: usize = 16;
/// Maximum unison voices per oscillator.
pub const MAX_UNISON: usize = 8;
/// Number of slots in the modulation matrix.
pub const MOD_SLOTS: usize = 8;
/// Number of built-in wavetables.
pub const TABLE_COUNT: usize = 8;
/// Frames (morph steps) per wavetable.
pub const FRAMES: usize = 16;
/// Band-limited copies of each frame, one per octave.
pub const MIP_LEVELS: usize = 10;
/// Modulation is evaluated once every `CONTROL` samples.
pub const CONTROL: usize = 16;
/// Number of points returned by [`frame_samples`].
pub const DISPLAY_POINTS: usize = 256;

pub const TABLE_NAMES: [&str; TABLE_COUNT] = [
    "Basic Shapes",
    "PWM",
    "Harmonic Sweep",
    "Formant",
    "Sync Sweep",
    "FM",
    "Digital",
    "Organ",
];

/// Length of the time-domain buffer each frame is generated at.
const GEN_LEN: usize = 4096;

/// Highest harmonic kept in mip level `m`. Level `m` is used for fundamentals
/// up to `sample_rate / 2 / harmonics`, so nothing ever lands above Nyquist.
const fn mip_harmonics(m: usize) -> usize {
    512 >> m
}

/// Table length of mip level `m`: 4× oversampled relative to its highest
/// harmonic (so linear interpolation stays clean), between 256 and 2048.
const fn mip_len(m: usize) -> usize {
    let l = 4 * mip_harmonics(m);
    if l < 256 {
        256
    } else if l > 2048 {
        2048
    } else {
        l
    }
}

const MIP_LEN: [usize; MIP_LEVELS] = {
    let mut a = [0; MIP_LEVELS];
    let mut m = 0;
    while m < MIP_LEVELS {
        a[m] = mip_len(m);
        m += 1;
    }
    a
};

/// Phase shift that turns a 32-bit phase into a table index for each mip.
const MIP_SHIFT: [u32; MIP_LEVELS] = {
    let mut a = [0; MIP_LEVELS];
    let mut m = 0;
    while m < MIP_LEVELS {
        a[m] = 32 - mip_len(m).trailing_zeros();
        m += 1;
    }
    a
};

/// Offset of each mip inside a frame. Every mip has one guard sample at the
/// end (a copy of sample 0) so interpolation never needs to wrap.
const MIP_OFFSET: [usize; MIP_LEVELS] = {
    let mut a = [0; MIP_LEVELS];
    let mut acc = 0;
    let mut m = 0;
    while m < MIP_LEVELS {
        a[m] = acc;
        acc += mip_len(m) + 1;
        m += 1;
    }
    a
};

const FRAME_STRIDE: usize = MIP_OFFSET[MIP_LEVELS - 1] + MIP_LEN[MIP_LEVELS - 1] + 1;
const TABLE_STRIDE: usize = FRAMES * FRAME_STRIDE;

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterType {
    LowPass,
    HighPass,
    BandPass,
    Notch,
    Peak,
}

impl FilterType {
    pub const ALL: [FilterType; 5] = [
        FilterType::LowPass,
        FilterType::HighPass,
        FilterType::BandPass,
        FilterType::Notch,
        FilterType::Peak,
    ];
    pub fn label(self) -> &'static str {
        match self {
            FilterType::LowPass => "Low-pass",
            FilterType::HighPass => "High-pass",
            FilterType::BandPass => "Band-pass",
            FilterType::Notch => "Notch",
            FilterType::Peak => "Peak",
        }
    }
}

/// How the oscillators reach the two filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterRouting {
    /// Everything → Filter 1 → Filter 2.
    Serial,
    /// Everything → Filter 1 and Filter 2 side by side; outputs averaged.
    Parallel,
    /// Osc 1 + Sub → Filter 1, Osc 2 + Noise → Filter 2; outputs summed.
    Split,
}

impl FilterRouting {
    pub const ALL: [FilterRouting; 3] = [FilterRouting::Serial, FilterRouting::Parallel, FilterRouting::Split];
    pub fn label(self) -> &'static str {
        match self {
            FilterRouting::Serial => "Serial",
            FilterRouting::Parallel => "Parallel",
            FilterRouting::Split => "Split",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LfoShape {
    Sine,
    Triangle,
    Saw,
    Square,
    SampleHold,
}

impl LfoShape {
    pub const ALL: [LfoShape; 5] = [
        LfoShape::Sine,
        LfoShape::Triangle,
        LfoShape::Saw,
        LfoShape::Square,
        LfoShape::SampleHold,
    ];
    pub fn label(self) -> &'static str {
        match self {
            LfoShape::Sine => "Sine",
            LfoShape::Triangle => "Tri",
            LfoShape::Saw => "Saw",
            LfoShape::Square => "Square",
            LfoShape::SampleHold => "S&H",
        }
    }
}

/// Modulation sources. Ranges: Env 2/3 0..1, LFO 1/2 -1..1, Velocity 0..1,
/// KeyTrack `(note - 60) / 60` (so -1 at note 0, +1 at note 120),
/// Macro 1 0..1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModSource {
    None,
    Env2,
    Env3,
    Lfo1,
    Lfo2,
    Velocity,
    KeyTrack,
    Macro1,
}

impl ModSource {
    pub const ALL: [ModSource; 8] = [
        ModSource::None,
        ModSource::Env2,
        ModSource::Env3,
        ModSource::Lfo1,
        ModSource::Lfo2,
        ModSource::Velocity,
        ModSource::KeyTrack,
        ModSource::Macro1,
    ];
    pub const COUNT: usize = Self::ALL.len();
    pub fn label(self) -> &'static str {
        match self {
            ModSource::None => "None",
            ModSource::Env2 => "Env 2",
            ModSource::Env3 => "Env 3",
            ModSource::Lfo1 => "LFO 1",
            ModSource::Lfo2 => "LFO 2",
            ModSource::Velocity => "Velocity",
            ModSource::KeyTrack => "Key",
            ModSource::Macro1 => "Macro 1",
        }
    }
}

/// Modulation destinations. Scaling of `amount × source` (with amount in
/// -1..1):
///
/// - `Osc1Position`/`Osc2Position`: 1.0 = full wavetable range (added, then clamped 0..1).
/// - `Osc1Pitch`/`Osc2Pitch`: 1.0 = ±24 semitones.
/// - `Filter1Cutoff`/`Filter2Cutoff`: 1.0 = ±5 octaves (so KeyTrack × 1.0 is
///   exactly 100 % key tracking).
/// - `Filter1Res`: 1.0 = full resonance range (added, clamped 0..1).
/// - `Osc1Level`/`Osc2Level`: 1.0 = full level range (added, clamped 0..1).
/// - `Volume`: gain × (1 + m): -1.0 mutes, +1.0 doubles (+6 dB).
/// - `Pan`: 1.0 = hard right, -1.0 hard left (added, clamped).
/// - `Lfo1Rate`: 1.0 = ±4 octaves (×16 / ÷16).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModDest {
    None,
    Osc1Position,
    Osc2Position,
    Osc1Pitch,
    Osc2Pitch,
    Filter1Cutoff,
    Filter2Cutoff,
    Filter1Res,
    Osc1Level,
    Osc2Level,
    Volume,
    Pan,
    Lfo1Rate,
}

impl ModDest {
    pub const ALL: [ModDest; 13] = [
        ModDest::None,
        ModDest::Osc1Position,
        ModDest::Osc2Position,
        ModDest::Osc1Pitch,
        ModDest::Osc2Pitch,
        ModDest::Filter1Cutoff,
        ModDest::Filter2Cutoff,
        ModDest::Filter1Res,
        ModDest::Osc1Level,
        ModDest::Osc2Level,
        ModDest::Volume,
        ModDest::Pan,
        ModDest::Lfo1Rate,
    ];
    pub const COUNT: usize = Self::ALL.len();
    pub fn label(self) -> &'static str {
        match self {
            ModDest::None => "None",
            ModDest::Osc1Position => "Osc 1 Pos",
            ModDest::Osc2Position => "Osc 2 Pos",
            ModDest::Osc1Pitch => "Osc 1 Pitch",
            ModDest::Osc2Pitch => "Osc 2 Pitch",
            ModDest::Filter1Cutoff => "Filt 1 Freq",
            ModDest::Filter2Cutoff => "Filt 2 Freq",
            ModDest::Filter1Res => "Filt 1 Res",
            ModDest::Osc1Level => "Osc 1 Level",
            ModDest::Osc2Level => "Osc 2 Level",
            ModDest::Volume => "Volume",
            ModDest::Pan => "Pan",
            ModDest::Lfo1Rate => "LFO 1 Rate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OscParams {
    pub enabled: bool,
    /// Index into [`TABLE_NAMES`] (clamped).
    pub table: usize,
    /// 0..1 morph across the table's frames.
    pub position: f32,
    /// Transpose in semitones.
    pub semitones: f32,
    /// Fine detune in cents.
    pub cents: f32,
    /// 0..1.
    pub level: f32,
    /// -1 (left) .. 1 (right).
    pub pan: f32,
    /// Unison voices, 1..=8.
    pub unison: u8,
    /// 0..1; 1.0 = outermost unison voices ±50 cents.
    pub unison_detune: f32,
    /// 0..1 stereo spread of the unison voices.
    pub unison_spread: f32,
}

impl Default for OscParams {
    fn default() -> Self {
        Self {
            enabled: true,
            table: 0,
            position: 0.6,
            semitones: 0.0,
            cents: 0.0,
            level: 0.8,
            pan: 0.0,
            unison: 1,
            unison_detune: 0.25,
            unison_spread: 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilterParams {
    pub enabled: bool,
    pub kind: FilterType,
    /// Hz.
    pub cutoff: f32,
    /// 0..1 (Q 0.707 .. 12).
    pub resonance: f32,
    /// 0..1 soft-saturation drive before the filter (0 = clean).
    pub drive: f32,
}

impl Default for FilterParams {
    fn default() -> Self {
        Self {
            enabled: true,
            kind: FilterType::LowPass,
            cutoff: 4000.0,
            resonance: 0.2,
            drive: 0.0,
        }
    }
}

/// ADSR times in seconds, sustain 0..1.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EnvParams {
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
}

impl EnvParams {
    pub const fn new(attack: f32, decay: f32, sustain: f32, release: f32) -> Self {
        Self {
            attack,
            decay,
            sustain,
            release,
        }
    }
}

impl Default for EnvParams {
    fn default() -> Self {
        Self::new(0.005, 0.4, 0.8, 0.3)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LfoParams {
    pub shape: LfoShape,
    /// Hz (0.01..40).
    pub rate: f32,
    /// Restart the LFO phase on every note-on; otherwise new voices pick up
    /// a free-running phase.
    pub retrigger: bool,
}

impl Default for LfoParams {
    fn default() -> Self {
        Self {
            shape: LfoShape::Sine,
            rate: 1.0,
            retrigger: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModSlot {
    pub source: ModSource,
    pub dest: ModDest,
    /// -1..1; see [`ModDest`] for scaling.
    pub amount: f32,
}

impl Default for ModSlot {
    fn default() -> Self {
        Self {
            source: ModSource::None,
            dest: ModDest::None,
            amount: 0.0,
        }
    }
}

impl ModSlot {
    pub const fn new(source: ModSource, dest: ModDest, amount: f32) -> Self {
        Self { source, dest, amount }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WavetableParams {
    pub osc1: OscParams,
    pub osc2: OscParams,
    /// Sine one octave below the played note, 0..1.
    pub sub_level: f32,
    /// White noise, 0..1.
    pub noise_level: f32,
    pub filter1: FilterParams,
    pub filter2: FilterParams,
    pub routing: FilterRouting,
    /// Hard-wired to amplitude.
    pub amp_env: EnvParams,
    pub env2: EnvParams,
    pub env3: EnvParams,
    pub lfo1: LfoParams,
    pub lfo2: LfoParams,
    /// 0..1, a performance knob usable as a modulation source.
    pub macro1: f32,
    pub mod_matrix: [ModSlot; MOD_SLOTS],
    /// Portamento time in seconds (0 = off).
    pub glide: f32,
    /// 0..1: how much velocity scales amplitude.
    pub velocity_sens: f32,
    pub volume_db: f32,
}

fn matrix(slots: &[ModSlot]) -> [ModSlot; MOD_SLOTS] {
    let mut m = [ModSlot::default(); MOD_SLOTS];
    for (d, s) in m.iter_mut().zip(slots) {
        *d = *s;
    }
    m
}

impl Default for WavetableParams {
    fn default() -> Self {
        Self {
            osc1: OscParams::default(),
            osc2: OscParams {
                enabled: false,
                table: 2,
                position: 0.3,
                cents: 7.0,
                level: 0.6,
                ..OscParams::default()
            },
            sub_level: 0.0,
            noise_level: 0.0,
            filter1: FilterParams::default(),
            filter2: FilterParams {
                enabled: false,
                kind: FilterType::HighPass,
                cutoff: 80.0,
                resonance: 0.0,
                drive: 0.0,
            },
            routing: FilterRouting::Serial,
            amp_env: EnvParams::new(0.005, 0.4, 0.8, 0.3),
            env2: EnvParams::new(0.002, 0.5, 0.2, 0.4),
            env3: EnvParams::new(0.5, 1.0, 0.5, 1.0),
            lfo1: LfoParams {
                shape: LfoShape::Sine,
                rate: 0.5,
                retrigger: false,
            },
            lfo2: LfoParams {
                shape: LfoShape::Triangle,
                rate: 4.0,
                retrigger: true,
            },
            macro1: 0.0,
            mod_matrix: matrix(&[
                ModSlot::new(ModSource::Env2, ModDest::Filter1Cutoff, 0.3),
                ModSlot::new(ModSource::Lfo1, ModDest::Osc1Position, 0.15),
            ]),
            glide: 0.0,
            velocity_sens: 0.6,
            volume_db: -12.0,
        }
    }
}

impl WavetableParams {
    /// Names of the built-in presets, in the order of [`WavetableParams::preset`].
    pub const PRESET_NAMES: [&'static str; 5] = ["Init", "Growl Bass", "Glass Pad", "Pluck", "Supersaw Lead"];

    /// Built-in preset by index into [`WavetableParams::PRESET_NAMES`].
    pub fn preset(index: usize) -> Self {
        match index {
            1 => Self::growl_bass(),
            2 => Self::glass_pad(),
            3 => Self::pluck(),
            4 => Self::supersaw_lead(),
            _ => Self::default(),
        }
    }

    /// Driven FM bass with an LFO sweeping the table and a sub.
    pub fn growl_bass() -> Self {
        Self {
            osc1: OscParams {
                table: 5,
                position: 0.35,
                level: 0.8,
                ..OscParams::default()
            },
            osc2: OscParams {
                enabled: true,
                table: 4,
                position: 0.2,
                semitones: -12.0,
                cents: 0.0,
                level: 0.5,
                unison: 2,
                unison_detune: 0.1,
                unison_spread: 0.3,
                ..OscParams::default()
            },
            sub_level: 0.5,
            filter1: FilterParams {
                enabled: true,
                kind: FilterType::LowPass,
                cutoff: 500.0,
                resonance: 0.35,
                drive: 0.4,
            },
            amp_env: EnvParams::new(0.002, 0.3, 0.8, 0.1),
            env2: EnvParams::new(0.001, 0.25, 0.1, 0.15),
            lfo1: LfoParams {
                shape: LfoShape::Triangle,
                rate: 3.0,
                retrigger: true,
            },
            mod_matrix: matrix(&[
                ModSlot::new(ModSource::Env2, ModDest::Filter1Cutoff, 0.5),
                ModSlot::new(ModSource::Lfo1, ModDest::Osc1Position, 0.35),
                ModSlot::new(ModSource::Lfo1, ModDest::Filter1Cutoff, 0.15),
                ModSlot::new(ModSource::Macro1, ModDest::Osc2Position, 0.8),
                ModSlot::new(ModSource::KeyTrack, ModDest::Filter1Cutoff, 0.5),
            ]),
            glide: 0.03,
            velocity_sens: 0.4,
            volume_db: -11.0,
            ..Self::default()
        }
    }

    /// Slow, wide, shimmering pad from the harmonic and formant tables.
    pub fn glass_pad() -> Self {
        Self {
            osc1: OscParams {
                table: 2,
                position: 0.45,
                level: 0.7,
                unison: 4,
                unison_detune: 0.25,
                unison_spread: 0.9,
                ..OscParams::default()
            },
            osc2: OscParams {
                enabled: true,
                table: 3,
                position: 0.2,
                semitones: 12.0,
                cents: 5.0,
                level: 0.4,
                unison: 3,
                unison_detune: 0.2,
                unison_spread: 1.0,
                ..OscParams::default()
            },
            filter1: FilterParams {
                enabled: true,
                kind: FilterType::LowPass,
                cutoff: 2500.0,
                resonance: 0.15,
                drive: 0.0,
            },
            filter2: FilterParams {
                enabled: true,
                kind: FilterType::HighPass,
                cutoff: 120.0,
                resonance: 0.0,
                drive: 0.0,
            },
            amp_env: EnvParams::new(0.8, 1.0, 0.85, 1.8),
            env2: EnvParams::new(1.5, 2.0, 0.4, 1.5),
            env3: EnvParams::new(3.0, 2.0, 0.7, 2.0),
            lfo1: LfoParams {
                shape: LfoShape::Sine,
                rate: 0.15,
                retrigger: false,
            },
            lfo2: LfoParams {
                shape: LfoShape::Triangle,
                rate: 0.23,
                retrigger: false,
            },
            mod_matrix: matrix(&[
                ModSlot::new(ModSource::Env2, ModDest::Filter1Cutoff, 0.25),
                ModSlot::new(ModSource::Lfo1, ModDest::Osc1Position, 0.3),
                ModSlot::new(ModSource::Lfo2, ModDest::Osc2Position, 0.25),
                ModSlot::new(ModSource::Env3, ModDest::Osc2Level, 0.3),
                ModSlot::new(ModSource::Lfo2, ModDest::Pan, 0.2),
            ]),
            velocity_sens: 0.3,
            volume_db: -14.0,
            ..Self::default()
        }
    }

    /// Short percussive pluck with an enveloped filter.
    pub fn pluck() -> Self {
        Self {
            osc1: OscParams {
                table: 1,
                position: 0.3,
                level: 0.8,
                unison: 2,
                unison_detune: 0.08,
                unison_spread: 0.5,
                ..OscParams::default()
            },
            osc2: OscParams {
                enabled: true,
                table: 6,
                position: 0.1,
                semitones: 12.0,
                level: 0.25,
                ..OscParams::default()
            },
            noise_level: 0.05,
            filter1: FilterParams {
                enabled: true,
                kind: FilterType::LowPass,
                cutoff: 700.0,
                resonance: 0.3,
                drive: 0.1,
            },
            amp_env: EnvParams::new(0.001, 0.35, 0.0, 0.25),
            env2: EnvParams::new(0.001, 0.18, 0.0, 0.2),
            env3: EnvParams::new(0.001, 0.4, 0.0, 0.3),
            mod_matrix: matrix(&[
                ModSlot::new(ModSource::Env2, ModDest::Filter1Cutoff, 0.7),
                ModSlot::new(ModSource::Env3, ModDest::Osc1Position, -0.3),
                ModSlot::new(ModSource::Velocity, ModDest::Filter1Cutoff, 0.2),
                ModSlot::new(ModSource::KeyTrack, ModDest::Filter1Cutoff, 0.6),
            ]),
            velocity_sens: 0.7,
            volume_db: -10.0,
            ..Self::default()
        }
    }

    /// Thick detuned saw lead with glide and vibrato on LFO 2.
    pub fn supersaw_lead() -> Self {
        let saw = 10.0 / (FRAMES - 1) as f32;
        Self {
            osc1: OscParams {
                table: 0,
                position: saw,
                level: 0.8,
                unison: 7,
                unison_detune: 0.35,
                unison_spread: 1.0,
                ..OscParams::default()
            },
            osc2: OscParams {
                enabled: true,
                table: 0,
                position: saw,
                semitones: 12.0,
                cents: 0.0,
                level: 0.35,
                unison: 3,
                unison_detune: 0.25,
                unison_spread: 0.8,
                ..OscParams::default()
            },
            filter1: FilterParams {
                enabled: true,
                kind: FilterType::LowPass,
                cutoff: 5000.0,
                resonance: 0.2,
                drive: 0.15,
            },
            amp_env: EnvParams::new(0.01, 0.5, 0.9, 0.35),
            env2: EnvParams::new(0.01, 0.6, 0.5, 0.4),
            lfo2: LfoParams {
                shape: LfoShape::Sine,
                rate: 5.5,
                retrigger: true,
            },
            mod_matrix: matrix(&[
                ModSlot::new(ModSource::Env2, ModDest::Filter1Cutoff, 0.2),
                ModSlot::new(ModSource::Lfo2, ModDest::Osc1Pitch, 0.006),
                ModSlot::new(ModSource::Lfo2, ModDest::Osc2Pitch, 0.006),
                ModSlot::new(ModSource::Macro1, ModDest::Filter1Cutoff, 0.4),
            ]),
            glide: 0.06,
            velocity_sens: 0.3,
            volume_db: -15.0,
            ..Self::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Wavetable bank
// ---------------------------------------------------------------------------

/// All built-in wavetables: `TABLE_COUNT × FRAMES` single-cycle frames, each
/// stored at `MIP_LEVELS` band-limited resolutions.
pub struct WavetableBank {
    data: Vec<f32>,
}

static BANK: OnceLock<WavetableBank> = OnceLock::new();

/// The shared wavetable bank, built on first use and never freed.
pub fn bank() -> &'static WavetableBank {
    BANK.get_or_init(WavetableBank::build)
}

impl WavetableBank {
    pub fn table_count(&self) -> usize {
        TABLE_COUNT
    }

    /// Size of the bank's sample data in bytes.
    pub fn size_bytes(&self) -> usize {
        self.data.len() * std::mem::size_of::<f32>()
    }

    /// One band-limited single-cycle frame (without the guard sample).
    /// Mip level `m` contains harmonics `1..=512 >> m`.
    pub fn frame(&self, table: usize, frame: usize, mip: usize) -> &[f32] {
        let base = table.min(TABLE_COUNT - 1) * TABLE_STRIDE
            + frame.min(FRAMES - 1) * FRAME_STRIDE
            + MIP_OFFSET[mip.min(MIP_LEVELS - 1)];
        &self.data[base..base + MIP_LEN[mip.min(MIP_LEVELS - 1)]]
    }

    fn build() -> Self {
        let mut data = vec![0.0f32; TABLE_COUNT * TABLE_STRIDE];
        let mut re = vec![0.0f64; GEN_LEN];
        let mut im = vec![0.0f64; GEN_LEN];
        let max_len = MIP_LEN[0];
        let mut mre = vec![0.0f64; max_len];
        let mut mim = vec![0.0f64; max_len];
        for table in 0..TABLE_COUNT {
            for frame in 0..FRAMES {
                let pos = frame as f64 / (FRAMES - 1) as f64;
                re.fill(0.0);
                im.fill(0.0);
                if !generate(table, pos, &mut re, &mut im) {
                    // Time domain → spectrum.
                    fft(&mut re, &mut im, false);
                }
                let base = table * TABLE_STRIDE + frame * FRAME_STRIDE;
                for m in 0..MIP_LEVELS {
                    let len = MIP_LEN[m];
                    let h_max = mip_harmonics(m).min(len / 2 - 1);
                    let r = &mut mre[..len];
                    let i = &mut mim[..len];
                    r.fill(0.0);
                    i.fill(0.0);
                    let scale = len as f64 / GEN_LEN as f64;
                    for h in 1..=h_max {
                        r[h] = re[h] * scale;
                        i[h] = im[h] * scale;
                        r[len - h] = re[h] * scale;
                        i[len - h] = -im[h] * scale;
                    }
                    fft(r, i, true);
                    let out = &mut data[base + MIP_OFFSET[m]..base + MIP_OFFSET[m] + len + 1];
                    for (o, &v) in out.iter_mut().zip(r.iter()) {
                        *o = v as f32;
                    }
                    out[len] = out[0];
                }
                // Normalize the whole frame by the full-bandwidth peak so the
                // mips stay level-matched with each other.
                let peak = data[base..base + MIP_LEN[0]].iter().fold(0.0f32, |a, b| a.max(b.abs()));
                if peak > 1e-9 {
                    let g = 1.0 / peak;
                    for v in &mut data[base..base + FRAME_STRIDE] {
                        *v *= g;
                    }
                }
            }
        }
        Self { data }
    }
}

/// Draws a frame for the UI: `DISPLAY_POINTS` samples of `table` at morph
/// `position` (0..1), interpolated between frames like the oscillator does.
/// Allocates; UI thread only.
pub fn frame_samples(table: usize, position: f32) -> Vec<f32> {
    let b = bank();
    // Mip 3 is 256 samples long and holds 64 harmonics.
    const MIP: usize = 3;
    let fpos = position.clamp(0.0, 1.0) * (FRAMES - 1) as f32;
    let fi = (fpos as usize).min(FRAMES - 2);
    let ff = fpos - fi as f32;
    let a = b.frame(table, fi, MIP);
    let c = b.frame(table, fi + 1, MIP);
    (0..DISPLAY_POINTS)
        .map(|i| {
            let x = i * a.len() / DISPLAY_POINTS;
            a[x] + (c[x] - a[x]) * ff
        })
        .collect()
}

/// In-place iterative radix-2 complex FFT. `inverse` uses the opposite sign
/// and scales by 1/n. Length must be a power of two.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    // Bit reversal.
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * std::f64::consts::TAU / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let ncr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = ncr;
            }
        }
        len <<= 1;
    }
    if inverse {
        let s = 1.0 / n as f64;
        for (r, i) in re.iter_mut().zip(im.iter_mut()) {
            *r *= s;
            *i *= s;
        }
    }
}

/// Adds a sine partial of amplitude `amp` at harmonic `h` to a spectrum of
/// length `GEN_LEN`.
fn add_partial(im: &mut [f64], h: usize, amp: f64) {
    if (1..GEN_LEN / 2).contains(&h) {
        im[h] -= amp * GEN_LEN as f64 / 2.0;
    }
}

/// Fills one frame of `table` at morph position `pos` (0..1). Writes either a
/// time-domain cycle into `re` (returns false) or a spectrum into `re`/`im`
/// (returns true).
fn generate(table: usize, pos: f64, re: &mut [f64], im: &mut [f64]) -> bool {
    use std::f64::consts::TAU;
    let n = re.len();
    let time = |re: &mut [f64], f: &dyn Fn(f64) -> f64| {
        for (i, v) in re.iter_mut().enumerate() {
            *v = f(i as f64 / n as f64);
        }
    };
    match table {
        // Basic Shapes: sine → triangle → saw → square.
        0 => {
            let sine = |x: f64| (TAU * x).sin();
            let tri = |x: f64| 1.0 - 4.0 * (((x + 0.25) % 1.0) - 0.5).abs();
            let saw = |x: f64| 2.0 * ((x + 0.5) % 1.0) - 1.0;
            let square = |x: f64| if x < 0.5 { 1.0 } else { -1.0 };
            let seg = pos * 3.0;
            let k = (seg.floor() as usize).min(2);
            let t = seg - k as f64;
            time(re, &|x| {
                let (a, b) = match k {
                    0 => (sine(x), tri(x)),
                    1 => (tri(x), saw(x)),
                    _ => (saw(x), square(x)),
                };
                a + (b - a) * t
            });
            false
        }
        // PWM: pulse width 50 % → 3 %.
        1 => {
            let w = 0.5 - 0.47 * pos;
            time(re, &|x| if x < w { 1.0 } else { -1.0 });
            false
        }
        // Harmonic sweep: 1 → 64 harmonics with a saw-like 1/k rolloff.
        2 => {
            let count = 2f64.powf(pos * 6.0);
            for k in 1..=64 {
                let fade = (count - k as f64 + 1.0).clamp(0.0, 1.0);
                add_partial(im, k, fade / k as f64);
            }
            true
        }
        // Formant: vowel morph A → E → I → O → U on a 110 Hz glottal source.
        3 => {
            const VOWELS: [[f64; 3]; 5] = [
                [800.0, 1150.0, 2900.0],
                [400.0, 2000.0, 2600.0],
                [300.0, 2300.0, 3000.0],
                [450.0, 800.0, 2830.0],
                [325.0, 700.0, 2530.0],
            ];
            const GAINS: [f64; 3] = [1.0, 0.6, 0.25];
            let seg = pos * 4.0;
            let k = (seg.floor() as usize).min(3);
            let t = seg - k as f64;
            let base = 110.0;
            for h in 1..=48 {
                let f = h as f64 * base;
                let mut amp = 0.35 / h as f64;
                for (fi, g) in GAINS.iter().enumerate() {
                    let fc = VOWELS[k][fi] + (VOWELS[k + 1][fi] - VOWELS[k][fi]) * t;
                    let bw = 90.0 + fc * 0.06;
                    amp += g * (-((f - fc) / bw).powi(2)).exp();
                }
                add_partial(im, h, amp);
            }
            true
        }
        // Sync sweep: hard-synced saw, slave ratio 1 → 8, faded at the reset.
        4 => {
            let ratio = 1.0 + pos * 7.0;
            time(re, &|x| {
                let s = 2.0 * ((x * ratio) % 1.0) - 1.0;
                s * (1.0 - x).sqrt()
            });
            false
        }
        // FM: 2:1 modulator, index 0 → 5.
        5 => {
            let index = pos * 5.0;
            time(re, &|x| (TAU * x + index * (TAU * 2.0 * x).sin()).sin());
            false
        }
        // Digital: bit-crushed, sample-held sine getting coarser.
        6 => {
            let steps = 2f64.powf(8.0 - pos * 5.0).round();
            let levels = 2f64.powf(7.0 - pos * 6.0).round().max(1.0);
            time(re, &|x| {
                let held = (x * steps).floor() / steps;
                ((TAU * held).sin() * levels).round() / levels
            });
            false
        }
        // Organ: drawbar registrations (8', 4', 2 2/3', 2', 1 3/5', 1 1/3', 1').
        _ => {
            const HARM: [usize; 7] = [1, 2, 3, 4, 5, 6, 8];
            const REGS: [[f64; 7]; 4] = [
                [8.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                [8.0, 6.0, 4.0, 0.0, 0.0, 0.0, 0.0],
                [8.0, 8.0, 6.0, 6.0, 0.0, 4.0, 6.0],
                [8.0, 8.0, 8.0, 8.0, 8.0, 8.0, 8.0],
            ];
            let seg = pos * 3.0;
            let k = (seg.floor() as usize).min(2);
            let t = seg - k as f64;
            for (d, &h) in HARM.iter().enumerate() {
                let bar = REGS[k][d] + (REGS[k + 1][d] - REGS[k][d]) * t;
                if bar > 0.0 {
                    // ~3 dB per drawbar step, fading smoothly to silence.
                    let amp = 10f64.powf((bar - 8.0) * 3.0 / 20.0) * bar.min(1.0);
                    add_partial(im, h, amp);
                }
            }
            true
        }
    }
}

/// Picks the mip level whose highest harmonic stays below Nyquist at `freq`.
#[inline]
fn pick_mip(freq: f32, sample_rate: f32) -> usize {
    let ratio = freq * 1024.0 / sample_rate;
    if ratio <= 1.0 {
        0
    } else {
        (ratio.log2().ceil() as usize).min(MIP_LEVELS - 1)
    }
}

#[inline]
fn hz_to_inc(freq: f32, sample_rate: f32) -> u32 {
    (freq.clamp(0.0, sample_rate * 0.49) as f64 / sample_rate as f64 * 4_294_967_296.0) as u32
}

/// Cheap rational tanh approximation (exact ±1 clamp beyond |x| = 3).
#[inline]
fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// Reads one linearly interpolated sample from a mip slice (with guard).
#[inline]
fn read_table(t: &[f32], phase: u32, shift: u32, mask: u32, scale: f32) -> f32 {
    let idx = (phase >> shift) as usize;
    let fr = (phase & mask) as f32 * scale;
    let a = t[idx];
    a + (t[idx + 1] - a) * fr
}

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

/// Per-render values derived from the parameters, shared by all voices.
struct Shared {
    unison: [usize; 2],
    ratio: [[f32; MAX_UNISON]; 2],
    max_ratio: [f32; 2],
    gain_l: [[f32; MAX_UNISON]; 2],
    gain_r: [[f32; MAX_UNISON]; 2],
    table_base: [usize; 2],
    drive_gain: [f32; 2],
    drive_comp: [f32; 2],
    lfo_free: [f32; 2],
}

impl Shared {
    fn new(p: &WavetableParams, lfo_free: [f32; 2]) -> Self {
        let mut s = Shared {
            unison: [1; 2],
            ratio: [[1.0; MAX_UNISON]; 2],
            max_ratio: [1.0; 2],
            gain_l: [[0.0; MAX_UNISON]; 2],
            gain_r: [[0.0; MAX_UNISON]; 2],
            table_base: [0; 2],
            drive_gain: [1.0; 2],
            drive_comp: [1.0; 2],
            lfo_free,
        };
        for (o, op) in [&p.osc1, &p.osc2].into_iter().enumerate() {
            let n = (op.unison as usize).clamp(1, MAX_UNISON);
            s.unison[o] = n;
            s.table_base[o] = op.table.min(TABLE_COUNT - 1) * TABLE_STRIDE;
            let norm = 1.0 / (n as f32).sqrt();
            let detune = op.unison_detune.clamp(0.0, 1.0);
            let spread = op.unison_spread.clamp(0.0, 1.0);
            for k in 0..n {
                let u = if n == 1 {
                    0.0
                } else {
                    -1.0 + 2.0 * k as f32 / (n - 1) as f32
                };
                let r = 2f32.powf(u * detune * 50.0 / 1200.0);
                s.ratio[o][k] = r;
                s.max_ratio[o] = s.max_ratio[o].max(r);
                // Alternate sides so neighbouring detunes don't bunch up.
                let side = if k % 2 == 0 { 1.0 } else { -1.0 };
                let pan = (op.pan + u * spread * side).clamp(-1.0, 1.0);
                s.gain_l[o][k] = norm * (1.0 - pan).min(1.0);
                s.gain_r[o][k] = norm * (1.0 + pan).min(1.0);
            }
        }
        for (f, fp) in [&p.filter1, &p.filter2].into_iter().enumerate() {
            let g = 1.0 + fp.drive.clamp(0.0, 1.0) * 15.0;
            s.drive_gain[f] = g;
            s.drive_comp[f] = 1.0 / g.sqrt();
        }
        s
    }
}

#[inline]
fn lfo_value(shape: LfoShape, phase: f32, hold: f32) -> f32 {
    match shape {
        LfoShape::Sine => (phase * std::f32::consts::TAU).sin(),
        LfoShape::Triangle => 1.0 - 4.0 * (phase - 0.5).abs(),
        LfoShape::Saw => 2.0 * phase - 1.0,
        LfoShape::Square => {
            if phase < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
        LfoShape::SampleHold => hold,
    }
}

/// Resonance 0..1 → SVF Q.
#[inline]
fn res_to_q(res: f32) -> f32 {
    let r = res.clamp(0.0, 1.0);
    0.707 + 11.3 * r * r
}

#[inline]
fn run_filter(svf: &mut Svf, kind: FilterType, x: f32, cutoff: f32, q: f32, sr: f32) -> f32 {
    let o = svf.process(x, cutoff, q, sr);
    let k = 1.0 / q.max(0.5);
    match kind {
        FilterType::LowPass => o.low,
        FilterType::HighPass => o.high,
        FilterType::BandPass => k * o.band,
        FilterType::Notch => o.low + o.high,
        FilterType::Peak => o.low - o.high,
    }
}

#[derive(Debug, Clone)]
struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    age: u64,
    /// Current (gliding) pitch in semitones.
    pitch: f32,
    phases: [[u32; MAX_UNISON]; 2],
    inc: [[u32; MAX_UNISON]; 2],
    mip: [usize; 2],
    sub_phase: u32,
    sub_inc: u32,
    amp_env: Adsr,
    /// Env 2/3 run at control rate.
    env2: Adsr,
    env3: Adsr,
    lfo_phase: [f32; 2],
    lfo_hold: [f32; 2],
    lfo_init: bool,
    /// `[filter][channel]`.
    filters: [[Svf; 2]; 2],
    rng: Rng,
    /// Samples until the next control tick.
    ctrl_left: usize,
    /// No ramps yet: jump straight to the first control values.
    fresh: bool,
    pos: [f32; 2],
    pos_step: [f32; 2],
    osc_gain: [f32; 2],
    osc_gain_step: [f32; 2],
    cutoff: [f32; 2],
    cutoff_step: [f32; 2],
    out_gain: [f32; 2],
    out_gain_step: [f32; 2],
    q: [f32; 2],
}

impl Voice {
    fn new(sr: f32, seed: u32) -> Self {
        let ctrl_sr = sr / CONTROL as f32;
        Self {
            note: 0,
            velocity: 0.0,
            held: false,
            age: 0,
            pitch: 0.0,
            phases: [[0; MAX_UNISON]; 2],
            inc: [[0; MAX_UNISON]; 2],
            mip: [0; 2],
            sub_phase: 0,
            sub_inc: 0,
            amp_env: Adsr::new(sr),
            env2: Adsr::new(ctrl_sr),
            env3: Adsr::new(ctrl_sr),
            lfo_phase: [0.0; 2],
            lfo_hold: [0.0; 2],
            lfo_init: true,
            filters: Default::default(),
            rng: Rng::new(seed),
            ctrl_left: 0,
            fresh: true,
            pos: [0.0; 2],
            pos_step: [0.0; 2],
            osc_gain: [0.0; 2],
            osc_gain_step: [0.0; 2],
            cutoff: [1000.0; 2],
            cutoff_step: [0.0; 2],
            out_gain: [0.0; 2],
            out_gain_step: [0.0; 2],
            q: [0.707; 2],
        }
    }

    /// Evaluates modulation and sets up the ramps for the next `CONTROL`
    /// samples.
    fn control_tick(&mut self, p: &WavetableParams, sh: &Shared, sr: f32) {
        let ctrl_sr = sr / CONTROL as f32;
        let target = self.note as f32;
        if p.glide <= 0.0 {
            self.pitch = target;
        } else {
            let c = exp_coef(p.glide, ctrl_sr);
            self.pitch = target + (self.pitch - target) * c;
        }

        let lfos = [&p.lfo1, &p.lfo2];
        if self.lfo_init {
            self.lfo_init = false;
            for (x, lp) in lfos.iter().enumerate() {
                self.lfo_phase[x] = if lp.retrigger { 0.0 } else { sh.lfo_free[x] };
                self.lfo_hold[x] = self.rng.next_bipolar();
            }
        }

        let e2 = self
            .env2
            .next(p.env2.attack, p.env2.decay, p.env2.sustain, p.env2.release);
        let e3 = self
            .env3
            .next(p.env3.attack, p.env3.decay, p.env3.sustain, p.env3.release);
        let l1 = lfo_value(p.lfo1.shape, self.lfo_phase[0], self.lfo_hold[0]);
        let l2 = lfo_value(p.lfo2.shape, self.lfo_phase[1], self.lfo_hold[1]);
        let mut src = [0.0f32; ModSource::COUNT];
        src[ModSource::Env2 as usize] = e2;
        src[ModSource::Env3 as usize] = e3;
        src[ModSource::Lfo1 as usize] = l1;
        src[ModSource::Lfo2 as usize] = l2;
        src[ModSource::Velocity as usize] = self.velocity;
        src[ModSource::KeyTrack as usize] = (self.note as f32 - 60.0) / 60.0;
        src[ModSource::Macro1 as usize] = p.macro1.clamp(0.0, 1.0);
        let mut m = [0.0f32; ModDest::COUNT];
        for slot in &p.mod_matrix {
            if slot.source != ModSource::None && slot.dest != ModDest::None {
                m[slot.dest as usize] += src[slot.source as usize] * slot.amount.clamp(-1.0, 1.0);
            }
        }

        // Advance LFOs.
        for (x, lp) in lfos.iter().enumerate() {
            let mut rate = lp.rate.clamp(0.01, 40.0);
            if x == 0 {
                rate *= 2f32.powf(4.0 * m[ModDest::Lfo1Rate as usize]);
            }
            self.lfo_phase[x] += rate / ctrl_sr;
            if self.lfo_phase[x] >= 1.0 {
                self.lfo_phase[x] = self.lfo_phase[x].fract();
                self.lfo_hold[x] = self.rng.next_bipolar();
            }
        }

        // Oscillators.
        let oscs = [&p.osc1, &p.osc2];
        let pos_d = [ModDest::Osc1Position, ModDest::Osc2Position];
        let pitch_d = [ModDest::Osc1Pitch, ModDest::Osc2Pitch];
        let level_d = [ModDest::Osc1Level, ModDest::Osc2Level];
        let mut pos_t = [0.0; 2];
        let mut gain_t = [0.0; 2];
        for o in 0..2 {
            let op = oscs[o];
            pos_t[o] = (op.position + m[pos_d[o] as usize]).clamp(0.0, 1.0);
            gain_t[o] = if op.enabled {
                (op.level + m[level_d[o] as usize]).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let semis = self.pitch + op.semitones + op.cents / 100.0 + 24.0 * m[pitch_d[o] as usize];
            let f = midi_to_hz(semis.clamp(-48.0, 160.0));
            self.mip[o] = pick_mip(f * sh.max_ratio[o], sr);
            for k in 0..sh.unison[o] {
                self.inc[o][k] = hz_to_inc(f * sh.ratio[o][k], sr);
            }
        }
        self.sub_inc = hz_to_inc(midi_to_hz(self.pitch - 12.0), sr);

        // Filters.
        let max_fc = sr * 0.45;
        let cut_t = [
            (p.filter1.cutoff * 2f32.powf(5.0 * m[ModDest::Filter1Cutoff as usize])).clamp(20.0, max_fc),
            (p.filter2.cutoff * 2f32.powf(5.0 * m[ModDest::Filter2Cutoff as usize])).clamp(20.0, max_fc),
        ];
        self.q = [
            res_to_q(p.filter1.resonance + m[ModDest::Filter1Res as usize]),
            res_to_q(p.filter2.resonance),
        ];

        // Output gain and pan.
        let vel_gain = 1.0 - p.velocity_sens.clamp(0.0, 1.0) * (1.0 - self.velocity);
        let vol = (1.0 + m[ModDest::Volume as usize]).max(0.0) * vel_gain;
        let pan = m[ModDest::Pan as usize].clamp(-1.0, 1.0);
        let out_t = [vol * (1.0 - pan).min(1.0), vol * (1.0 + pan).min(1.0)];

        if self.fresh {
            self.fresh = false;
            self.pos = pos_t;
            self.osc_gain = gain_t;
            self.cutoff = cut_t;
            self.out_gain = out_t;
            self.pos_step = [0.0; 2];
            self.osc_gain_step = [0.0; 2];
            self.cutoff_step = [0.0; 2];
            self.out_gain_step = [0.0; 2];
        } else {
            let inv = 1.0 / CONTROL as f32;
            for i in 0..2 {
                self.pos_step[i] = (pos_t[i] - self.pos[i]) * inv;
                self.osc_gain_step[i] = (gain_t[i] - self.osc_gain[i]) * inv;
                self.cutoff_step[i] = (cut_t[i] - self.cutoff[i]) * inv;
                self.out_gain_step[i] = (out_t[i] - self.out_gain[i]) * inv;
            }
        }
    }

    /// Renders one oscillator's unison stack for the current sample.
    #[inline]
    fn osc(&mut self, o: usize, sh: &Shared, data: &[f32]) -> (f32, f32) {
        let fpos = self.pos[o].clamp(0.0, 1.0) * (FRAMES - 1) as f32;
        let fi = (fpos as usize).min(FRAMES - 2);
        let ff = fpos - fi as f32;
        let mip = self.mip[o];
        let len = MIP_LEN[mip];
        let a_base = sh.table_base[o] + fi * FRAME_STRIDE + MIP_OFFSET[mip];
        let b_base = a_base + FRAME_STRIDE;
        let fa = &data[a_base..a_base + len + 1];
        let fb = &data[b_base..b_base + len + 1];
        let shift = MIP_SHIFT[mip];
        let mask = (1u32 << shift) - 1;
        let scale = 1.0 / (1u32 << shift) as f32;
        let (mut l, mut r) = (0.0, 0.0);
        let n = sh.unison[o];
        let phases = &mut self.phases[o][..n];
        let incs = &self.inc[o][..n];
        for k in 0..n {
            let ph = phases[k];
            let sa = read_table(fa, ph, shift, mask, scale);
            let sb = read_table(fb, ph, shift, mask, scale);
            let s = sa + (sb - sa) * ff;
            l += s * sh.gain_l[o][k];
            r += s * sh.gain_r[o][k];
            phases[k] = ph.wrapping_add(incs[k]);
        }
        (l, r)
    }

    #[inline]
    fn filter(&mut self, f: usize, kind: FilterType, sh: &Shared, x: (f32, f32), drive: bool, sr: f32) -> (f32, f32) {
        let (mut l, mut r) = x;
        if drive {
            let g = sh.drive_gain[f];
            let c = sh.drive_comp[f];
            l = soft_clip(l * g) * c;
            r = soft_clip(r * g) * c;
        }
        let [fl, fr] = &mut self.filters[f];
        let (fc, q) = (self.cutoff[f], self.q[f]);
        (run_filter(fl, kind, l, fc, q, sr), run_filter(fr, kind, r, fc, q, sr))
    }

    /// Renders up to `left.len()` samples (no control tick inside). Returns
    /// false once the voice has finished.
    fn render_span(
        &mut self,
        p: &WavetableParams,
        sh: &Shared,
        data: &[f32],
        sr: f32,
        out: (&mut [f32], &mut [f32]),
        master: f32,
    ) -> bool {
        let (left, right) = out;
        let env = p.amp_env;
        let on = [p.osc1.enabled, p.osc2.enabled];
        let sub_level = p.sub_level.clamp(0.0, 1.0);
        let noise_level = p.noise_level.clamp(0.0, 1.0);
        let sub_table = &data[MIP_OFFSET[MIP_LEVELS - 1]..MIP_OFFSET[MIP_LEVELS - 1] + MIP_LEN[MIP_LEVELS - 1] + 1];
        let sub_shift = MIP_SHIFT[MIP_LEVELS - 1];
        let sub_mask = (1u32 << sub_shift) - 1;
        let sub_scale = 1.0 / (1u32 << sub_shift) as f32;
        let f_on = [p.filter1.enabled, p.filter2.enabled];
        let f_kind = [p.filter1.kind, p.filter2.kind];
        let f_drive = [p.filter1.drive > 0.0, p.filter2.drive > 0.0];
        for (lo, ro) in left.iter_mut().zip(right.iter_mut()) {
            let a = self.amp_env.next(env.attack, env.decay, env.sustain, env.release);
            if !self.amp_env.is_active() {
                return false;
            }
            for i in 0..2 {
                self.pos[i] += self.pos_step[i];
                self.osc_gain[i] += self.osc_gain_step[i];
                self.cutoff[i] += self.cutoff_step[i];
                self.out_gain[i] += self.out_gain_step[i];
            }

            let mut p1 = (0.0, 0.0);
            let mut p2 = (0.0, 0.0);
            if on[0] {
                let (l, r) = self.osc(0, sh, data);
                p1 = (l * self.osc_gain[0], r * self.osc_gain[0]);
            }
            if on[1] {
                let (l, r) = self.osc(1, sh, data);
                p2 = (l * self.osc_gain[1], r * self.osc_gain[1]);
            }
            if sub_level > 0.0 {
                let s = read_table(sub_table, self.sub_phase, sub_shift, sub_mask, sub_scale) * sub_level;
                p1.0 += s;
                p1.1 += s;
            }
            self.sub_phase = self.sub_phase.wrapping_add(self.sub_inc);
            if noise_level > 0.0 {
                let n = self.rng.next_bipolar() * noise_level;
                p2.0 += n;
                p2.1 += n;
            }

            let (yl, yr) = match p.routing {
                FilterRouting::Serial => {
                    let mut x = (p1.0 + p2.0, p1.1 + p2.1);
                    for f in 0..2 {
                        if f_on[f] {
                            x = self.filter(f, f_kind[f], sh, x, f_drive[f], sr);
                        }
                    }
                    x
                }
                FilterRouting::Parallel => {
                    let x = (p1.0 + p2.0, p1.1 + p2.1);
                    match (f_on[0], f_on[1]) {
                        (false, false) => x,
                        (true, false) => self.filter(0, f_kind[0], sh, x, f_drive[0], sr),
                        (false, true) => self.filter(1, f_kind[1], sh, x, f_drive[1], sr),
                        (true, true) => {
                            let a = self.filter(0, f_kind[0], sh, x, f_drive[0], sr);
                            let b = self.filter(1, f_kind[1], sh, x, f_drive[1], sr);
                            ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
                        }
                    }
                }
                FilterRouting::Split => {
                    let a = if f_on[0] {
                        self.filter(0, f_kind[0], sh, p1, f_drive[0], sr)
                    } else {
                        p1
                    };
                    let b = if f_on[1] {
                        self.filter(1, f_kind[1], sh, p2, f_drive[1], sr)
                    } else {
                        p2
                    };
                    (a.0 + b.0, a.1 + b.1)
                }
            };
            let g = a * master;
            *lo += yl * g * self.out_gain[0];
            *ro += yr * g * self.out_gain[1];
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Instrument
// ---------------------------------------------------------------------------

pub struct Wavetable {
    sample_rate: f32,
    voices: [Voice; VOICES],
    counter: u64,
    rng: Rng,
    /// Pitch of the most recent note, the start point for glides.
    last_note: Option<f32>,
    /// Free-running LFO phases for voices without retrigger.
    lfo_free: [f32; 2],
    bank: &'static WavetableBank,
}

impl Wavetable {
    /// Builds the shared wavetable bank if needed, so call this off the audio
    /// thread.
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            voices: std::array::from_fn(|i| Voice::new(sample_rate, 0x9E37_79B9u32.wrapping_mul(i as u32 + 1))),
            counter: 0,
            rng: Rng::new(0x5EED_1234),
            last_note: None,
            lfo_free: [0.0; 2],
            bank: bank(),
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.amp_env.is_active()).count()
    }

    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.counter += 1;
        // Prefer a free voice, then the oldest releasing voice, then the oldest.
        let idx = self
            .voices
            .iter()
            .position(|v| !v.amp_env.is_active())
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.amp_env.is_releasing())
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
            })
            .unwrap_or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, v)| v.age)
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            });
        let start_pitch = self.last_note.unwrap_or(note as f32);
        self.last_note = Some(note as f32);
        let rng = &mut self.rng;
        let v = &mut self.voices[idx];
        let was_active = v.amp_env.is_active();
        v.note = note;
        v.velocity = velocity as f32 / 127.0;
        v.held = true;
        v.age = self.counter;
        v.pitch = start_pitch;
        v.lfo_init = true;
        v.ctrl_left = 0;
        if !was_active {
            // Randomized but deterministic unison start phases; the first
            // unison voice always starts at zero for a consistent attack.
            for osc in v.phases.iter_mut() {
                for (k, ph) in osc.iter_mut().enumerate() {
                    *ph = if k == 0 {
                        0
                    } else {
                        ((rng.next_bipolar() * 0.5 + 0.5) * u32::MAX as f32) as u32
                    };
                }
            }
            v.sub_phase = 0;
            for f in v.filters.iter_mut().flatten() {
                f.reset();
            }
            v.env2.reset();
            v.env3.reset();
            v.fresh = true;
        }
        v.amp_env.trigger();
        v.env2.trigger();
        v.env3.trigger();
    }

    pub fn note_off(&mut self, note: u8) {
        for v in &mut self.voices {
            if v.held && v.note == note {
                v.held = false;
                v.amp_env.release();
                v.env2.release();
                v.env3.release();
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.amp_env.release();
            v.env2.release();
            v.env3.release();
        }
    }

    pub fn panic(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.amp_env.reset();
            v.env2.reset();
            v.env3.reset();
        }
    }

    /// Adds the instrument's output to `left`/`right`.
    pub fn render(&mut self, p: &WavetableParams, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let len = left.len().min(right.len());
        let sh = Shared::new(p, self.lfo_free);
        let data: &[f32] = &self.bank.data;
        let master = db_to_gain(p.volume_db);
        for v in self.voices.iter_mut() {
            if !v.amp_env.is_active() {
                continue;
            }
            let mut i = 0;
            while i < len {
                if v.ctrl_left == 0 {
                    v.control_tick(p, &sh, sr);
                    v.ctrl_left = CONTROL;
                }
                let n = v.ctrl_left.min(len - i);
                let alive = v.render_span(p, &sh, data, sr, (&mut left[i..i + n], &mut right[i..i + n]), master);
                v.ctrl_left -= n;
                i += n;
                if !alive {
                    break;
                }
            }
        }
        for (x, lp) in [&p.lfo1, &p.lfo2].into_iter().enumerate() {
            let ph = self.lfo_free[x] + lp.rate.clamp(0.01, 40.0) * len as f32 / sr;
            self.lfo_free[x] = ph.fract();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn all_presets() -> Vec<WavetableParams> {
        (0..WavetableParams::PRESET_NAMES.len())
            .map(WavetableParams::preset)
            .collect()
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    /// Params with a single plain oscillator and nothing else in the way.
    fn dry(table: usize, position: f32) -> WavetableParams {
        WavetableParams {
            osc1: OscParams {
                table,
                position,
                level: 1.0,
                unison: 1,
                ..OscParams::default()
            },
            osc2: OscParams {
                enabled: false,
                ..OscParams::default()
            },
            sub_level: 0.0,
            noise_level: 0.0,
            filter1: FilterParams {
                enabled: false,
                ..FilterParams::default()
            },
            filter2: FilterParams {
                enabled: false,
                ..FilterParams::default()
            },
            amp_env: EnvParams::new(0.001, 0.1, 1.0, 0.1),
            mod_matrix: [ModSlot::default(); MOD_SLOTS],
            velocity_sens: 0.0,
            volume_db: 0.0,
            ..WavetableParams::default()
        }
    }

    #[test]
    fn produces_sound_and_releases() {
        let mut s = Wavetable::new(SR);
        let p = WavetableParams::default();
        let mut l = vec![0.0; 4800];
        let mut r = vec![0.0; 4800];
        s.note_on(60, 100);
        s.render(&p, &mut l, &mut r);
        let pk = peak(&l).max(peak(&r));
        assert!(pk > 0.05, "peak {pk}");
        assert!(pk < 2.0, "peak {pk}");
        s.note_off(60);
        for _ in 0..20 {
            l.fill(0.0);
            r.fill(0.0);
            s.render(&p, &mut l, &mut r);
        }
        assert_eq!(s.active_voices(), 0);
        l.fill(0.0);
        r.fill(0.0);
        s.render(&p, &mut l, &mut r);
        assert_eq!(peak(&l), 0.0);
    }

    #[test]
    fn voice_stealing_never_exceeds_pool() {
        let mut s = Wavetable::new(SR);
        for n in 0..40 {
            s.note_on(40 + n, 100);
        }
        assert_eq!(s.active_voices(), VOICES);
        s.panic();
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn output_bounded_for_all_presets() {
        let mut l = vec![0.0; 512];
        let mut r = vec![0.0; 512];
        for (pi, p) in all_presets().iter().enumerate() {
            for macro1 in [0.0, 1.0] {
                let p = WavetableParams { macro1, ..*p };
                let mut s = Wavetable::new(SR);
                let mut worst = 0.0f32;
                for root in (24u8..=108).step_by(12) {
                    for n in [root, root + 4, root + 7] {
                        s.note_on(n.min(127), 127);
                    }
                    for _ in 0..30 {
                        l.fill(0.0);
                        r.fill(0.0);
                        s.render(&p, &mut l, &mut r);
                        assert!(l.iter().chain(r.iter()).all(|x| x.is_finite()));
                        worst = worst.max(peak(&l)).max(peak(&r));
                    }
                    s.all_notes_off();
                }
                println!("preset {pi} macro {macro1}: peak {worst}");
                assert!(worst < 4.0, "preset {pi} peak {worst}");
                assert!(worst > 0.01, "preset {pi} silent");
            }
        }
    }

    #[test]
    fn tables_are_normalized_and_finite() {
        let b = bank();
        assert!(b.size_bytes() < 4 * 1024 * 1024, "bank is {} bytes", b.size_bytes());
        for t in 0..TABLE_COUNT {
            for f in 0..FRAMES {
                let full = peak(b.frame(t, f, 0));
                assert!((full - 1.0).abs() < 1e-4, "table {t} frame {f} peak {full}");
                for (m, &len) in MIP_LEN.iter().enumerate() {
                    let fr = b.frame(t, f, m);
                    assert_eq!(fr.len(), len);
                    assert!(fr.iter().all(|x| x.is_finite()));
                    let pk = peak(fr);
                    assert!(pk < 2.0 && pk > 0.01, "table {t} frame {f} mip {m} peak {pk}");
                }
            }
        }
        let shape = frame_samples(0, 0.5);
        assert_eq!(shape.len(), DISPLAY_POINTS);
        assert!(peak(&shape) > 0.5);
    }

    #[test]
    fn mips_have_no_harmonics_above_limit() {
        let b = bank();
        for t in 0..TABLE_COUNT {
            for f in 0..FRAMES {
                for m in 0..MIP_LEVELS {
                    let fr = b.frame(t, f, m);
                    let n = fr.len();
                    let mut re: Vec<f64> = fr.iter().map(|&x| x as f64).collect();
                    let mut im = vec![0.0; n];
                    fft(&mut re, &mut im, false);
                    let mag = |k: usize| (re[k] * re[k] + im[k] * im[k]).sqrt() / n as f64;
                    let total: f64 = (1..n / 2).map(mag).sum();
                    let above: f64 = (mip_harmonics(m) + 1..=n / 2).map(mag).sum();
                    assert!(
                        above < 1e-5 * total.max(1e-9),
                        "table {t} frame {f} mip {m}: {above} / {total}"
                    );
                }
            }
        }
    }

    #[test]
    fn fft_round_trip() {
        let n = 64;
        let orig: Vec<f64> = (0..n).map(|i| ((i * 7 % 13) as f64) - 6.0).collect();
        let mut re = orig.clone();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        fft(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(&orig) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    /// Fraction of a signal's energy that does not sit on a harmonic of `f0`.
    fn alias_ratio(x: &[f32], f0: f32) -> f64 {
        let n = x.len();
        let mut re: Vec<f64> = x
            .iter()
            .enumerate()
            .map(|(i, &v)| v as f64 * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()))
            .collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        let bin_hz = SR as f64 / n as f64;
        let (mut harm, mut alias) = (0.0, 0.0);
        for k in 1..n / 2 {
            let e = re[k] * re[k] + im[k] * im[k];
            let f = k as f64 * bin_hz;
            let h = (f / f0 as f64).round();
            if h >= 1.0 && (f - h * f0 as f64).abs() < 6.0 * bin_hz && h * (f0 as f64) < SR as f64 / 2.0 {
                harm += e;
            } else {
                alias += e;
            }
        }
        alias / (harm + alias)
    }

    #[test]
    fn high_notes_are_band_limited() {
        let saw = 10.0 / (FRAMES - 1) as f32;
        let p = dry(0, saw);
        let note = 110u8;
        let f0 = midi_to_hz(note as f32);
        let mut s = Wavetable::new(SR);
        s.note_on(note, 127);
        let mut warm_l = vec![0.0; 2048];
        let mut warm_r = vec![0.0; 2048];
        s.render(&p, &mut warm_l, &mut warm_r);
        let mut l = vec![0.0; 8192];
        let mut r = vec![0.0; 8192];
        s.render(&p, &mut l, &mut r);
        assert!(peak(&l) > 0.3);
        let bl = alias_ratio(&l, f0);

        // A naive (aliasing) saw at the same pitch for comparison.
        let naive: Vec<f32> = (0..8192)
            .map(|i| {
                let ph = (i as f32 * f0 / SR).fract();
                2.0 * ph - 1.0
            })
            .collect();
        let nv = alias_ratio(&naive, f0);
        println!("alias ratio: band-limited {bl:e}, naive {nv:e}");
        assert!(bl < 1e-4, "band-limited alias ratio {bl}");
        assert!(nv > 100.0 * bl, "naive {nv} vs band-limited {bl}");
    }

    fn render_note(p: &WavetableParams, note: u8, samples: usize) -> (Vec<f32>, Vec<f32>) {
        let mut s = Wavetable::new(SR);
        s.note_on(note, 100);
        let mut l = vec![0.0; samples];
        let mut r = vec![0.0; samples];
        // Odd block sizes exercise control ticks spanning render calls.
        let mut i = 0;
        while i < samples {
            let n = 37.min(samples - i);
            s.render(p, &mut l[i..i + n], &mut r[i..i + n]);
            i += n;
        }
        (l, r)
    }

    fn diff(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
    }

    #[test]
    fn mod_matrix_changes_output() {
        let mut base = dry(0, 0.5);
        base.lfo1 = LfoParams {
            shape: LfoShape::Sine,
            rate: 5.0,
            retrigger: true,
        };
        let (l0, r0) = render_note(&base, 60, 24000);
        let (l0b, _) = render_note(&base, 60, 24000);
        assert_eq!(diff(&l0, &l0b), 0.0, "rendering must be deterministic");
        let level = l0.iter().map(|x| x.abs()).sum::<f32>() / l0.len() as f32;

        let cases = [
            ModSlot::new(ModSource::Lfo1, ModDest::Osc1Pitch, 0.5),
            ModSlot::new(ModSource::Lfo1, ModDest::Osc1Position, 0.5),
            ModSlot::new(ModSource::Velocity, ModDest::Volume, -0.5),
            ModSlot::new(ModSource::Lfo1, ModDest::Pan, 1.0),
        ];
        for slot in cases {
            let mut p = base;
            p.mod_matrix[0] = slot;
            let (l, r) = render_note(&p, 60, 24000);
            let d = diff(&l, &l0).max(diff(&r, &r0));
            assert!(d > 0.05 * level, "{slot:?} made no difference ({d} vs level {level})");
        }

        // Env 2 → cutoff through an enabled filter.
        let mut f = base;
        f.filter1 = FilterParams {
            enabled: true,
            cutoff: 300.0,
            ..FilterParams::default()
        };
        let (a, _) = render_note(&f, 48, 24000);
        f.mod_matrix[0] = ModSlot::new(ModSource::Env2, ModDest::Filter1Cutoff, 0.8);
        let (b, _) = render_note(&f, 48, 24000);
        assert!(diff(&a, &b) > 0.01, "env2 → cutoff had no effect");
    }

    #[test]
    fn unison_spreads_stereo() {
        let mut p = dry(0, 0.6);
        p.osc1.unison = 5;
        p.osc1.unison_spread = 1.0;
        p.osc1.unison_detune = 0.4;
        let (l, r) = render_note(&p, 57, 12000);
        assert!(diff(&l, &r) > 0.01, "unison spread should decorrelate L/R");
        p.osc1.unison = 1;
        let (l, r) = render_note(&p, 57, 12000);
        assert_eq!(diff(&l, &r), 0.0);
    }

    #[test]
    fn routings_and_filter_types_are_stable() {
        for routing in FilterRouting::ALL {
            for kind in FilterType::ALL {
                let mut p = WavetableParams::growl_bass();
                p.routing = routing;
                p.filter1.kind = kind;
                p.filter1.resonance = 1.0;
                p.filter2 = FilterParams {
                    enabled: true,
                    kind,
                    cutoff: 2000.0,
                    resonance: 0.8,
                    drive: 1.0,
                };
                p.noise_level = 0.3;
                let (l, r) = render_note(&p, 45, 9600);
                let pk = peak(&l).max(peak(&r));
                assert!(pk.is_finite() && pk < 4.0, "{routing:?}/{kind:?} peak {pk}");
                assert!(pk > 1e-3, "{routing:?}/{kind:?} silent");
            }
        }
    }

    #[test]
    fn serde_round_trip() {
        for p in all_presets() {
            let json = serde_json::to_string(&p).unwrap();
            let back: WavetableParams = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
        let partial: WavetableParams = serde_json::from_str(r#"{"volume_db": -3.0, "osc1": {"table": 4}}"#).unwrap();
        assert_eq!(partial.volume_db, -3.0);
        assert_eq!(partial.osc1.table, 4);
        assert_eq!(partial.osc1.unison, OscParams::default().unison);
        assert_eq!(partial.mod_matrix, WavetableParams::default().mod_matrix);
    }

    #[test]
    fn labels_cover_all_variants() {
        assert_eq!(TABLE_NAMES.len(), TABLE_COUNT);
        assert!(ModSource::ALL.iter().all(|s| !s.label().is_empty()));
        assert!(ModDest::ALL.iter().all(|d| !d.label().is_empty()));
        assert!(LfoShape::ALL.iter().all(|d| !d.label().is_empty()));
        for (i, d) in ModDest::ALL.iter().enumerate() {
            assert_eq!(*d as usize, i);
        }
        for (i, s) in ModSource::ALL.iter().enumerate() {
            assert_eq!(*s as usize, i);
        }
    }

    /// Prints how long one second of 16 voices takes (`--nocapture` to see).
    #[test]
    fn wavetable_render_timing() {
        let p = WavetableParams::supersaw_lead();
        let mut s = Wavetable::new(SR);
        for n in 0..VOICES as u8 {
            s.note_on(40 + n * 3, 100);
        }
        let mut l = vec![0.0; 480];
        let mut r = vec![0.0; 480];
        let t = std::time::Instant::now();
        for _ in 0..100 {
            s.render(&p, &mut l, &mut r);
        }
        let el = t.elapsed();
        assert_eq!(s.active_voices(), VOICES);
        println!(
            "wavetable: 1 s of {VOICES} voices (unison 7+3, supersaw_lead) rendered in {:.1} ms",
            el.as_secs_f64() * 1000.0
        );
    }
}
