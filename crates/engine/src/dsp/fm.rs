//! "FM Synth": a four-operator phase-modulation polyphonic synthesizer.
//!
//! Four operators (A, B, C, D) are wired by one of [`ALGORITHMS`] fixed
//! topologies. Every operator is a wavetable oscillator with its own ADSR,
//! frequency ratio (or fixed frequency), level, velocity sensitivity and key
//! scaling. Modulators are applied as *phase* modulation, as in classic
//! DX-style synths:
//!
//! ```text
//! out = wave(phase + MOD_DEPTH * sum(modulator outputs))
//! ```
//!
//! where phases are measured in cycles. A modulator at level 1.0 and full
//! envelope therefore shifts its destination's phase by up to
//! [`MOD_DEPTH`] = 2 cycles, i.e. a modulation index of 4π radians. A level of
//! 0.1 gives an index of ~1.26, 0.25 gives ~π.
//!
//! Operator D has self-feedback (averaged over its last two output samples to
//! stay stable). On top of the operators sit a per-voice LFO, a pitch envelope,
//! an optional state-variable filter with its own envelope, glide, a stereo
//! spread (the carriers are rendered twice with a small opposite detune for
//! left and right), transpose and output volume.
//!
//! Like the rest of `dsp`, nothing here allocates, locks or frees after
//! [`FmSynth::new`].

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::envelope::Adsr;
use super::filter::Svf;
use super::{lfo_hz, Performance, Rng, SYNC_RATE_DEFAULT};
use crate::model::db_to_gain;

pub const VOICES: usize = 16;
/// Number of operators.
pub const OPS: usize = 4;
/// Display names of the operators, index 0..4.
pub const OP_NAMES: [&str; OPS] = ["A", "B", "C", "D"];
/// Number of algorithms (operator routings).
pub const ALGORITHMS: usize = 11;
/// Phase deviation in cycles produced by a modulator at level 1.0 and full
/// envelope: 2 cycles = a modulation index of 4π radians.
pub const MOD_DEPTH: f32 = 2.0;
/// Phase deviation in cycles produced by operator D's self-feedback at
/// `feedback = 1.0`: 0.28 cycles (~1.76 radians) gives a bright, near-sawtooth
/// tone; beyond ~0.35 cycles the two-sample-averaged loop turns chaotic
/// (noise), so the range stops short of that.
pub const FEEDBACK_DEPTH: f32 = 0.28;

/// Samples between control-rate updates (pitch, LFO, glide, filter cutoff).
const CONTROL_RATE: usize = 16;
/// Wavetable length (a guard sample is appended for interpolation).
const TABLE_LEN: usize = 2048;
const TABLE_STRIDE: usize = TABLE_LEN + 1;
/// Maximum harmonic count of each band-limited mip level.
const MIP_HARMONICS: [usize; 7] = [64, 32, 16, 8, 4, 2, 1];
const MIPS: usize = MIP_HARMONICS.len();

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

/// Operator waveform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpWaveform {
    Sine,
    /// Sine quantized to 16 levels: a deliberately lo-fi, buzzy sine.
    Sine4Bit,
    Triangle,
    Saw,
    Square,
}

impl OpWaveform {
    pub const ALL: [OpWaveform; 5] = [
        OpWaveform::Sine,
        OpWaveform::Sine4Bit,
        OpWaveform::Triangle,
        OpWaveform::Saw,
        OpWaveform::Square,
    ];
    pub fn label(self) -> &'static str {
        match self {
            OpWaveform::Sine => "Sine",
            OpWaveform::Sine4Bit => "Sine 4-bit",
            OpWaveform::Triangle => "Tri",
            OpWaveform::Saw => "Saw",
            OpWaveform::Square => "Square",
        }
    }
    fn index(self) -> usize {
        match self {
            OpWaveform::Sine => 0,
            OpWaveform::Sine4Bit => 1,
            OpWaveform::Triangle => 2,
            OpWaveform::Saw => 3,
            OpWaveform::Square => 4,
        }
    }
}

const WAVES: usize = OpWaveform::ALL.len();

/// LFO waveform. All shapes are bipolar (-1..1).
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

/// What the LFO modulates. At `lfo_amount = 1.0`: pitch ±12 semitones,
/// operator levels ±100 % (modulators only, i.e. the FM amount), filter cutoff
/// ±4 octaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LfoDestination {
    Pitch,
    OpLevels,
    FilterCutoff,
}

impl LfoDestination {
    pub const ALL: [LfoDestination; 3] = [
        LfoDestination::Pitch,
        LfoDestination::OpLevels,
        LfoDestination::FilterCutoff,
    ];
    pub fn label(self) -> &'static str {
        match self {
            LfoDestination::Pitch => "Pitch",
            LfoDestination::OpLevels => "FM Amount",
            LfoDestination::FilterCutoff => "Filter",
        }
    }
}

/// Filter response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FilterMode {
    Lowpass,
    Highpass,
    Bandpass,
}

impl FilterMode {
    pub const ALL: [FilterMode; 3] = [FilterMode::Lowpass, FilterMode::Highpass, FilterMode::Bandpass];
    pub fn label(self) -> &'static str {
        match self {
            FilterMode::Lowpass => "LP",
            FilterMode::Highpass => "HP",
            FilterMode::Bandpass => "BP",
        }
    }
}

// ---------------------------------------------------------------------------
// Algorithms
// ---------------------------------------------------------------------------

/// One operator routing. Operators are indexed 0..4 = A..D.
///
/// `modulators[dst][src]` is true when operator `src` phase-modulates operator
/// `dst`; `carriers[op]` is true when `op` is heard at the output. In every
/// algorithm a modulator always has a higher index than its destination, so
/// the operators are evaluated D, C, B, A.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlgorithmRouting {
    pub name: &'static str,
    pub modulators: [[bool; OPS]; OPS],
    pub carriers: [bool; OPS],
}

const fn routing(name: &'static str, edges: &[(usize, usize)], carriers: [bool; OPS]) -> AlgorithmRouting {
    let mut modulators = [[false; OPS]; OPS];
    let mut i = 0;
    while i < edges.len() {
        let (src, dst) = edges[i];
        modulators[dst][src] = true;
        i += 1;
    }
    AlgorithmRouting {
        name,
        modulators,
        carriers,
    }
}

const A: usize = 0;
const B: usize = 1;
const C: usize = 2;
const D: usize = 3;

/// The algorithm table. Edges are `(source, destination)`.
pub const ALGORITHM_TABLE: [AlgorithmRouting; ALGORITHMS] = [
    // 1: D -> C -> B -> A
    routing("Stack", &[(D, C), (C, B), (B, A)], [true, false, false, false]),
    // 2: (C + D) -> B -> A
    routing("Y Stack", &[(D, B), (C, B), (B, A)], [true, false, false, false]),
    // 3: (B + (D -> C)) -> A
    routing("Branch", &[(D, C), (C, A), (B, A)], [true, false, false, false]),
    // 4: D -> B, D -> C, (B + C) -> A
    routing(
        "Diamond",
        &[(D, B), (D, C), (B, A), (C, A)],
        [true, false, false, false],
    ),
    // 5: (B + C + D) -> A
    routing("Three to One", &[(B, A), (C, A), (D, A)], [true, false, false, false]),
    // 6: D -> C -> B, plus A
    routing("Stack + Sine", &[(D, C), (C, B)], [true, true, false, false]),
    // 7: D -> C, B -> A
    routing("Two Pairs", &[(D, C), (B, A)], [true, false, true, false]),
    // 8: D -> C, C -> A, C -> B
    routing("Shared Pair", &[(D, C), (C, A), (C, B)], [true, true, false, false]),
    // 9: D -> A, D -> B, D -> C
    routing("One to Three", &[(D, A), (D, B), (D, C)], [true, true, true, false]),
    // 10: D -> C, plus A and B
    routing("Pair + Two", &[(D, C)], [true, true, true, false]),
    // 11: all carriers (additive)
    routing("Additive", &[], [true, true, true, true]),
];

/// Routing of algorithm `alg` (clamped to the valid range).
pub fn algorithm_routing(alg: usize) -> AlgorithmRouting {
    ALGORITHM_TABLE[alg.min(ALGORITHMS - 1)]
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

/// One operator's settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpParams {
    pub on: bool,
    pub wave: OpWaveform,
    /// Coarse frequency ratio: 0 means 0.5, otherwise 1..=32.
    pub coarse: u8,
    /// Fine ratio offset in per-mille, 0..=1000, added to the coarse ratio
    /// (coarse 3, fine 500 = ratio 3.5).
    pub fine: u16,
    /// Ignore the played note and run at `fixed_hz`.
    pub fixed: bool,
    pub fixed_hz: f32,
    /// 0..1. Output level for carriers, modulation index for modulators
    /// (1.0 = 4π radians, see [`MOD_DEPTH`]).
    pub level: f32,
    pub attack: f32,
    pub decay: f32,
    pub sustain: f32,
    pub release: f32,
    /// 0 = velocity ignored, 1 = level fully follows velocity.
    pub vel_sens: f32,
    /// 0..1. Level change per octave away from middle C (C4, note 60): at 1.0
    /// the level halves every two octaves up and doubles every two octaves
    /// down (clamped to 0..2x).
    pub key_scale: f32,
}

impl Default for OpParams {
    fn default() -> Self {
        Self {
            on: true,
            wave: OpWaveform::Sine,
            coarse: 1,
            fine: 0,
            fixed: false,
            fixed_hz: 440.0,
            level: 1.0,
            attack: 0.002,
            decay: 0.5,
            sustain: 0.8,
            release: 0.3,
            vel_sens: 0.5,
            key_scale: 0.0,
        }
    }
}

impl OpParams {
    /// The frequency ratio from `coarse` and `fine`.
    pub fn ratio(&self) -> f32 {
        let coarse = if self.coarse == 0 {
            0.5
        } else {
            self.coarse.min(32) as f32
        };
        coarse + self.fine.min(1000) as f32 / 1000.0
    }

    #[allow(clippy::too_many_arguments)]
    fn with(
        on: bool,
        coarse: u8,
        fine: u16,
        level: f32,
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        vel_sens: f32,
    ) -> Self {
        Self {
            on,
            coarse,
            fine,
            level,
            attack,
            decay,
            sustain,
            release,
            vel_sens,
            ..Self::default()
        }
    }

    fn off() -> Self {
        Self {
            on: false,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FmParams {
    /// 0..ALGORITHMS, see [`ALGORITHM_TABLE`].
    pub algorithm: u8,
    /// Operators A, B, C, D.
    pub ops: [OpParams; OPS],
    /// Operator D self-feedback, 0..1.
    pub feedback: f32,
    pub lfo_rate: f32,
    pub lfo_shape: LfoShape,
    pub lfo_dest: LfoDestination,
    /// 0..1, see [`LfoDestination`] for the depth at 1.0.
    pub lfo_amount: f32,
    /// Initial pitch offset in semitones, decaying to 0.
    pub pitch_env_amount: f32,
    /// Seconds for the pitch envelope to settle.
    pub pitch_env_decay: f32,
    pub filter_on: bool,
    pub filter_mode: FilterMode,
    pub cutoff: f32,
    pub resonance: f32,
    /// Filter envelope depth in octaves.
    pub filter_env: f32,
    pub f_attack: f32,
    pub f_decay: f32,
    pub f_sustain: f32,
    pub f_release: f32,
    /// Glide (portamento) time in seconds; 0 = off.
    pub glide: f32,
    /// Stereo detune of the carriers in cents (left down, right up by half).
    pub spread: f32,
    /// Transpose in semitones.
    pub transpose: f32,
    pub volume_db: f32,
    /// LFO rate follows the song tempo instead of `lfo_rate`.
    pub lfo_sync: bool,
    /// Index into [`super::SYNC_RATE_NAMES`].
    pub lfo_sync_rate: u8,
}

impl Default for FmParams {
    fn default() -> Self {
        let mut b = OpParams::with(true, 1, 0, 0.2, 0.002, 0.6, 0.3, 0.3, 0.6);
        b.key_scale = 0.3;
        Self {
            algorithm: 0,
            ops: [
                OpParams::with(true, 1, 0, 1.0, 0.002, 0.8, 0.7, 0.3, 0.5),
                b,
                OpParams::off(),
                OpParams::off(),
            ],
            feedback: 0.0,
            lfo_rate: 5.0,
            lfo_sync: false,
            lfo_sync_rate: SYNC_RATE_DEFAULT as u8,
            lfo_shape: LfoShape::Sine,
            lfo_dest: LfoDestination::Pitch,
            lfo_amount: 0.0,
            pitch_env_amount: 0.0,
            pitch_env_decay: 0.1,
            filter_on: false,
            filter_mode: FilterMode::Lowpass,
            cutoff: 8000.0,
            resonance: 0.3,
            filter_env: 0.0,
            f_attack: 0.002,
            f_decay: 0.4,
            f_sustain: 0.5,
            f_release: 0.3,
            glide: 0.0,
            spread: 0.0,
            transpose: 0.0,
            volume_db: -12.0,
        }
    }
}

impl FmParams {
    /// Tine electric piano: two 1:1 pairs, one with a 14:1 "tine" transient.
    pub fn electric_piano() -> Self {
        let mut b = OpParams::with(true, 1, 0, 0.16, 0.001, 1.2, 0.08, 0.4, 0.8);
        b.key_scale = 0.5;
        let mut d = OpParams::with(true, 14, 0, 0.07, 0.001, 0.12, 0.0, 0.1, 0.9);
        d.key_scale = 0.6;
        Self {
            algorithm: 6,
            ops: [
                OpParams::with(true, 1, 0, 0.9, 0.001, 2.2, 0.15, 0.5, 0.5),
                b,
                OpParams::with(true, 1, 0, 0.6, 0.001, 1.0, 0.0, 0.3, 0.6),
                d,
            ],
            spread: 6.0,
            volume_db: -10.0,
            lfo_rate: 4.5,
            lfo_dest: LfoDestination::OpLevels,
            lfo_amount: 0.15,
            ..Self::default()
        }
    }

    /// Inharmonic struck bell: 1:3.5 and 2:7.13 pairs with long decays.
    pub fn bell() -> Self {
        Self {
            algorithm: 6,
            ops: [
                OpParams::with(true, 1, 0, 0.8, 0.001, 5.0, 0.0, 3.0, 0.5),
                OpParams::with(true, 3, 500, 0.35, 0.001, 3.5, 0.0, 2.5, 0.7),
                OpParams::with(true, 2, 0, 0.45, 0.001, 2.5, 0.0, 2.0, 0.5),
                OpParams::with(true, 7, 130, 0.18, 0.001, 1.5, 0.0, 1.5, 0.7),
            ],
            spread: 8.0,
            volume_db: -10.0,
            ..Self::default()
        }
    }

    /// Punchy FM bass: serial stack with a fast-decaying 1:1 modulator and a
    /// 3:1 click, through a low-pass.
    pub fn bass() -> Self {
        Self {
            algorithm: 0,
            ops: [
                OpParams::with(true, 1, 0, 1.0, 0.001, 0.6, 0.6, 0.08, 0.3),
                OpParams::with(true, 1, 0, 0.3, 0.001, 0.25, 0.12, 0.08, 0.7),
                OpParams::with(true, 3, 0, 0.08, 0.001, 0.05, 0.0, 0.05, 0.8),
                OpParams::off(),
            ],
            filter_on: true,
            cutoff: 2500.0,
            resonance: 0.3,
            filter_env: 1.0,
            f_attack: 0.001,
            f_decay: 0.2,
            f_sustain: 0.0,
            f_release: 0.1,
            volume_db: -8.0,
            ..Self::default()
        }
    }

    /// Brass: 1:1 modulation whose index swells with a slower attack, a slight
    /// pitch scoop and delayed vibrato-like LFO.
    pub fn brass() -> Self {
        Self {
            algorithm: 4,
            ops: [
                OpParams::with(true, 1, 0, 1.0, 0.04, 0.4, 0.8, 0.15, 0.4),
                OpParams::with(true, 1, 0, 0.3, 0.07, 0.5, 0.55, 0.15, 0.7),
                OpParams::with(true, 2, 0, 0.05, 0.05, 0.3, 0.3, 0.15, 0.7),
                OpParams::off(),
            ],
            feedback: 0.0,
            pitch_env_amount: -0.6,
            pitch_env_decay: 0.08,
            lfo_rate: 5.5,
            lfo_amount: 0.006,
            filter_on: true,
            cutoff: 5000.0,
            resonance: 0.2,
            filter_env: 0.8,
            f_attack: 0.06,
            f_decay: 0.5,
            f_sustain: 0.6,
            f_release: 0.2,
            spread: 5.0,
            volume_db: -11.0,
            ..Self::default()
        }
    }

    /// Drawbar-style organ: four parallel sines at 0.5, 1, 2, 3 with a touch
    /// of feedback on D for grit.
    pub fn organ() -> Self {
        let drawbar = |coarse: u8, level: f32| OpParams::with(true, coarse, 0, level, 0.004, 0.1, 1.0, 0.04, 0.0);
        Self {
            algorithm: 10,
            ops: [drawbar(0, 0.8), drawbar(1, 0.8), drawbar(2, 0.5), drawbar(3, 0.4)],
            feedback: 0.15,
            lfo_rate: 6.2,
            lfo_amount: 0.004,
            spread: 4.0,
            volume_db: -14.0,
            ..Self::default()
        }
    }

    /// Plucked "harp/koto": 1:2 and 1:4 modulators decaying faster than the
    /// carrier.
    pub fn pluck() -> Self {
        Self {
            algorithm: 2,
            ops: [
                OpParams::with(true, 1, 0, 1.0, 0.001, 0.9, 0.0, 0.4, 0.5),
                OpParams::with(true, 2, 0, 0.2, 0.001, 0.3, 0.0, 0.2, 0.8),
                OpParams::with(true, 4, 0, 0.12, 0.001, 0.15, 0.0, 0.1, 0.8),
                OpParams::with(true, 1, 0, 0.1, 0.001, 0.2, 0.0, 0.1, 0.8),
            ],
            feedback: 0.3,
            volume_db: -10.0,
            ..Self::default()
        }
    }

    /// All built-in presets with display names, starting with the init patch.
    pub fn presets() -> [(&'static str, FmParams); 7] {
        [
            ("Init", Self::default()),
            ("Electric Piano", Self::electric_piano()),
            ("Bell", Self::bell()),
            ("Bass", Self::bass()),
            ("Brass", Self::brass()),
            ("Organ", Self::organ()),
            ("Pluck", Self::pluck()),
        ]
    }
}

// ---------------------------------------------------------------------------
// Wavetables
// ---------------------------------------------------------------------------

static TABLES: OnceLock<Vec<f32>> = OnceLock::new();

/// Builds the band-limited tables: `WAVES * MIPS` tables of `TABLE_STRIDE`
/// samples, each normalised to a peak of 1.
fn build_tables() -> Vec<f32> {
    use std::f64::consts::TAU;
    let mut data = vec![0.0f32; WAVES * MIPS * TABLE_STRIDE];
    let mut tmp = vec![0.0f64; TABLE_LEN];
    for (w, wave) in OpWaveform::ALL.iter().enumerate() {
        for (m, &harmonics) in MIP_HARMONICS.iter().enumerate() {
            for (i, t) in tmp.iter_mut().enumerate() {
                let x = TAU * i as f64 / TABLE_LEN as f64;
                *t = match wave {
                    OpWaveform::Sine => x.sin(),
                    OpWaveform::Sine4Bit => (x.sin() * 7.5).round() / 7.5,
                    OpWaveform::Saw => (1..=harmonics).map(|k| (k as f64 * x).sin() / k as f64).sum::<f64>(),
                    OpWaveform::Square => (1..=harmonics)
                        .step_by(2)
                        .map(|k| (k as f64 * x).sin() / k as f64)
                        .sum::<f64>(),
                    OpWaveform::Triangle => (1..=harmonics)
                        .step_by(2)
                        .map(|k| {
                            let sign = if (k / 2) % 2 == 0 { 1.0 } else { -1.0 };
                            sign * (k as f64 * x).sin() / (k * k) as f64
                        })
                        .sum::<f64>(),
                };
            }
            let peak = tmp.iter().fold(0.0f64, |a, b| a.max(b.abs())).max(1e-9);
            let base = (w * MIPS + m) * TABLE_STRIDE;
            for i in 0..TABLE_LEN {
                data[base + i] = (tmp[i] / peak) as f32;
            }
            data[base + TABLE_LEN] = data[base];
        }
    }
    data
}

fn tables() -> &'static [f32] {
    TABLES.get_or_init(build_tables)
}

/// Picks the richest mip level whose top harmonic stays below ~0.45·sr.
#[inline]
fn mip_for(freq: f32, sample_rate: f32) -> usize {
    let limit = sample_rate * 0.45;
    MIP_HARMONICS
        .iter()
        .position(|&h| h as f32 * freq.abs() <= limit)
        .unwrap_or(MIPS - 1)
}

/// Linear-interpolated lookup; `phase` is in cycles and may be any value.
#[inline]
fn lookup(table: &[f32], phase: f32) -> f32 {
    let p = phase - phase.floor();
    let x = p * TABLE_LEN as f32;
    let i = (x as usize).min(TABLE_LEN - 1);
    let f = x - i as f32;
    let a = table[i];
    let b = table[i + 1];
    a + (b - a) * f
}

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct OpState {
    phase: f32,
    /// Right-channel phase, used for carriers when spread is on.
    phase_r: f32,
    env: Adsr,
}

#[derive(Debug, Clone)]
struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    active: bool,
    age: u64,
    ops: [OpState; OPS],
    fb1: f32,
    fb2: f32,
    /// Current (gliding) pitch in semitones, before transpose/modulation.
    pitch: f32,
    /// Pitch envelope level, 1 → 0.
    pitch_env: f32,
    lfo_phase: f32,
    lfo_held: f32,
    rng: Rng,
    filt_env: Adsr,
    filt_l: Svf,
    filt_r: Svf,
}

impl Voice {
    fn new(sr: f32, seed: u32) -> Self {
        Self {
            note: 0,
            velocity: 0.0,
            held: false,
            active: false,
            age: 0,
            ops: std::array::from_fn(|_| OpState {
                phase: 0.0,
                phase_r: 0.0,
                env: Adsr::new(sr),
            }),
            fb1: 0.0,
            fb2: 0.0,
            pitch: 60.0,
            pitch_env: 0.0,
            lfo_phase: 0.0,
            lfo_held: 0.0,
            rng: Rng::new(seed),
            filt_env: Adsr::new(sr),
            filt_l: Svf::default(),
            filt_r: Svf::default(),
        }
    }

    fn stop(&mut self) {
        self.active = false;
        self.held = false;
        for op in &mut self.ops {
            op.env.reset();
        }
        self.filt_env.reset();
    }

    /// Advances the LFO by `samples` and returns its bipolar value.
    fn lfo(&mut self, p: &FmParams, rate: f32, samples: usize, sr: f32) -> f32 {
        let ph = self.lfo_phase;
        let v = match p.lfo_shape {
            LfoShape::Sine => (ph * std::f32::consts::TAU).sin(),
            LfoShape::Triangle => 1.0 - 4.0 * (ph - 0.5).abs(),
            LfoShape::Saw => 2.0 * ph - 1.0,
            LfoShape::Square => {
                if ph < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            LfoShape::SampleHold => self.lfo_held,
        };
        self.lfo_phase += rate.clamp(0.0, 1000.0) * samples as f32 / sr;
        if self.lfo_phase >= 1.0 {
            self.lfo_phase -= self.lfo_phase.floor();
            self.lfo_held = self.rng.next_bipolar();
        }
        v
    }
}

pub struct FmSynth {
    sample_rate: f32,
    pub perf: Performance,
    tables: &'static [f32],
    voices: [Voice; VOICES],
    counter: u64,
    last_note: Option<u8>,
}

impl FmSynth {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            perf: Performance::default(),
            sample_rate,
            tables: tables(),
            voices: std::array::from_fn(|i| Voice::new(sample_rate, 0x9E37_79B9u32.wrapping_mul(i as u32 + 1))),
            counter: 0,
            last_note: None,
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    pub fn note_on(&mut self, note: u8, velocity: u8) {
        self.counter += 1;
        // Prefer a free voice, then the oldest releasing voice, then the oldest.
        let idx = self
            .voices
            .iter()
            .position(|v| !v.active)
            .or_else(|| {
                self.voices
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| !v.held)
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
        let glide_from = self.last_note.unwrap_or(note);
        self.last_note = Some(note);
        let v = &mut self.voices[idx];
        let was_active = v.active;
        v.note = note;
        v.velocity = velocity as f32 / 127.0;
        v.held = true;
        v.active = true;
        v.age = self.counter;
        v.pitch = glide_from as f32;
        v.pitch_env = 1.0;
        if !was_active {
            for op in &mut v.ops {
                op.phase = 0.0;
                op.phase_r = 0.0;
            }
            v.fb1 = 0.0;
            v.fb2 = 0.0;
            v.lfo_phase = 0.0;
            v.lfo_held = v.rng.next_bipolar();
            v.filt_l.reset();
            v.filt_r.reset();
        }
        for op in &mut v.ops {
            op.env.trigger();
        }
        v.filt_env.trigger();
    }

    pub fn note_off(&mut self, note: u8) {
        for v in &mut self.voices {
            if v.held && v.note == note {
                v.held = false;
                for op in &mut v.ops {
                    op.env.release();
                }
                v.filt_env.release();
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            for op in &mut v.ops {
                op.env.release();
            }
            v.filt_env.release();
        }
    }

    pub fn panic(&mut self) {
        for v in &mut self.voices {
            v.stop();
        }
    }

    /// Renders all voices and ADDS the result into `left`/`right`.
    pub fn render(&mut self, p: &FmParams, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let n = left.len().min(right.len());
        let tables = self.tables;
        let routing = algorithm_routing(p.algorithm as usize);
        let on: [bool; OPS] = std::array::from_fn(|k| p.ops[k].on);
        let carrier: [bool; OPS] = std::array::from_fn(|k| routing.carriers[k] && on[k]);
        let modulator: [bool; OPS] = std::array::from_fn(|k| !routing.carriers[k]);
        let n_carriers = carrier.iter().filter(|c| **c).count();
        let out_gain = db_to_gain(p.volume_db) / (n_carriers.max(1) as f32).sqrt();
        let fb_amt = p.feedback.clamp(0.0, 1.0) * FEEDBACK_DEPTH;
        let spread_on = p.spread.abs() > 1e-3;
        let detune_l = (-p.spread / 2400.0).exp2();
        let detune_r = (p.spread / 2400.0).exp2();
        let q = 0.5 + p.resonance.max(0.0) * 2.0;
        let lfo_amt = p.lfo_amount.clamp(0.0, 1.0);
        let lfo_rate = lfo_hz(p.lfo_rate, p.lfo_sync, p.lfo_sync_rate as f32, self.perf.tempo);
        let bend = self.perf.bend;
        let wave_base: [usize; OPS] = std::array::from_fn(|k| p.ops[k].wave.index() * MIPS * TABLE_STRIDE);

        for v in self.voices.iter_mut() {
            if !v.active {
                continue;
            }
            let target = v.note as f32;
            let vel_gain: [f32; OPS] = std::array::from_fn(|k| {
                let s = p.ops[k].vel_sens.clamp(0.0, 1.0);
                1.0 - s * (1.0 - v.velocity)
            });
            let key_gain: [f32; OPS] =
                std::array::from_fn(|k| (-p.ops[k].key_scale * (target - 60.0) / 24.0).exp2().clamp(0.0, 2.0));
            let mut i = 0;
            'blocks: while i < n {
                let end = (i + CONTROL_RATE).min(n);
                let len = end - i;

                // ---- control rate ----
                if p.glide > 1e-4 {
                    let coef = (-6.9 * len as f32 / (p.glide * sr)).exp();
                    v.pitch = target + (v.pitch - target) * coef;
                } else {
                    v.pitch = target;
                }
                let lfo = if lfo_amt > 0.0 {
                    v.lfo(p, lfo_rate, len, sr) * lfo_amt
                } else {
                    0.0
                };
                let mut semis = v.pitch + p.transpose + bend + p.pitch_env_amount * v.pitch_env;
                v.pitch_env *= (-6.9 * len as f32 / (p.pitch_env_decay.max(0.001) * sr)).exp();
                if p.lfo_dest == LfoDestination::Pitch {
                    semis += lfo * 12.0;
                }
                let base_hz = 440.0 * ((semis - 69.0) / 12.0).exp2();
                let pitch_mult = base_hz / (440.0 * ((target + p.transpose - 69.0) / 12.0).exp2());
                let level_lfo = if p.lfo_dest == LfoDestination::OpLevels {
                    (1.0 + lfo).max(0.0)
                } else {
                    1.0
                };
                let mut inc = [0.0f32; OPS];
                let mut inc_r = [0.0f32; OPS];
                let mut amp = [0.0f32; OPS];
                let mut table: [&[f32]; OPS] = [&[]; OPS];
                for k in 0..OPS {
                    let op = &p.ops[k];
                    let f = if op.fixed {
                        op.fixed_hz.max(0.0) * pitch_mult
                    } else {
                        base_hz * op.ratio()
                    };
                    let (fl, fr) = if carrier[k] && spread_on {
                        (f * detune_l, f * detune_r)
                    } else {
                        (f, f)
                    };
                    inc[k] = fl / sr;
                    inc_r[k] = fr / sr;
                    let start = wave_base[k] + mip_for(f, sr) * TABLE_STRIDE;
                    table[k] = &tables[start..start + TABLE_STRIDE];
                    let lvl = op.level.clamp(0.0, 1.0) * vel_gain[k] * key_gain[k];
                    amp[k] = if modulator[k] { lvl * level_lfo } else { lvl };
                }
                let cutoff = if p.filter_on {
                    let mut oct = p.filter_env * v.filt_env.value();
                    if p.lfo_dest == LfoDestination::FilterCutoff {
                        oct += lfo * 4.0;
                    }
                    p.cutoff * oct.exp2()
                } else {
                    0.0
                };

                // ---- audio rate ----
                for s in i..end {
                    let mut env = [0.0f32; OPS];
                    let mut alive = false;
                    for k in 0..OPS {
                        if on[k] {
                            let op = &p.ops[k];
                            let st = &mut v.ops[k];
                            env[k] = st.env.next(op.attack, op.decay, op.sustain.clamp(0.0, 1.0), op.release);
                            alive |= carrier[k] && st.env.is_active();
                        }
                    }
                    if !alive {
                        v.stop();
                        break 'blocks;
                    }
                    v.filt_env.next(p.f_attack, p.f_decay, p.f_sustain, p.f_release);

                    let mut out = [0.0f32; OPS];
                    let mut l = 0.0;
                    let mut r = 0.0;
                    for k in (0..OPS).rev() {
                        if !on[k] {
                            continue;
                        }
                        let mut m = 0.0;
                        for (src, o) in out.iter().enumerate().skip(k + 1) {
                            if routing.modulators[k][src] {
                                m += *o;
                            }
                        }
                        m *= MOD_DEPTH;
                        if k == D {
                            m += fb_amt * 0.5 * (v.fb1 + v.fb2);
                        }
                        let st = &mut v.ops[k];
                        let g = amp[k] * env[k];
                        let y = lookup(table[k], st.phase + m) * g;
                        out[k] = y;
                        st.phase += inc[k];
                        st.phase -= st.phase.floor();
                        if carrier[k] {
                            l += y;
                            if spread_on {
                                r += lookup(table[k], st.phase_r + m) * g;
                                st.phase_r += inc_r[k];
                                st.phase_r -= st.phase_r.floor();
                            }
                        }
                    }
                    if on[D] {
                        v.fb2 = v.fb1;
                        v.fb1 = out[D];
                    }
                    if !spread_on {
                        r = l;
                    }
                    if p.filter_on {
                        let fl = v.filt_l.process(l, cutoff, q, sr);
                        l = pick(p.filter_mode, fl.low, fl.band, fl.high);
                        if spread_on {
                            let fr = v.filt_r.process(r, cutoff, q, sr);
                            r = pick(p.filter_mode, fr.low, fr.band, fr.high);
                        } else {
                            r = l;
                        }
                    }
                    left[s] += l * out_gain;
                    right[s] += r * out_gain;
                }
                i = end;
            }
        }
    }
}

#[inline]
fn pick(mode: FilterMode, low: f32, band: f32, high: f32) -> f32 {
    match mode {
        FilterMode::Lowpass => low,
        FilterMode::Highpass => high,
        FilterMode::Bandpass => band,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn render_note(p: &FmParams, note: u8, len: usize) -> (Vec<f32>, Vec<f32>) {
        let mut s = FmSynth::new(SR);
        let mut l = vec![0.0; len];
        let mut r = vec![0.0; len];
        s.note_on(note, 100);
        for (lc, rc) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            s.render(p, lc, rc);
        }
        (l, r)
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count()
    }

    /// Ratio of first-difference energy to signal energy: grows with
    /// high-frequency content.
    fn brightness(x: &[f32]) -> f32 {
        let e: f32 = x.iter().map(|v| v * v).sum();
        let d: f32 = x.windows(2).map(|w| (w[1] - w[0]) * (w[1] - w[0])).sum();
        d / e.max(1e-12)
    }

    fn pure_sine_params() -> FmParams {
        let mut p = FmParams {
            algorithm: 10,
            ..FmParams::default()
        };
        p.ops[0] = OpParams::with(true, 1, 0, 1.0, 0.001, 1.0, 1.0, 0.1, 0.0);
        for op in &mut p.ops[1..] {
            op.on = false;
        }
        p
    }

    #[test]
    fn produces_sound_and_releases() {
        let mut s = FmSynth::new(SR);
        let p = FmParams::default();
        let mut l = vec![0.0; 4800];
        let mut r = vec![0.0; 4800];
        s.note_on(60, 100);
        s.render(&p, &mut l, &mut r);
        let pk = peak(&l);
        assert!(pk > 0.05, "peak {pk}");
        assert!(pk < 2.0);
        s.note_off(60);
        for _ in 0..20 {
            l.fill(0.0);
            r.fill(0.0);
            s.render(&p, &mut l, &mut r);
        }
        assert_eq!(s.active_voices(), 0);
        assert!(peak(&l) < 1e-3);
    }

    #[test]
    fn voice_stealing_never_exceeds_pool() {
        let mut s = FmSynth::new(SR);
        for n in 0..40 {
            s.note_on(40 + n, 100);
        }
        assert_eq!(s.active_voices(), VOICES);
        s.panic();
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn algorithms_feed_forward_only() {
        for alg in 0..ALGORITHMS {
            let r = algorithm_routing(alg);
            assert!(r.carriers.iter().any(|c| *c), "alg {alg} has no carrier");
            for dst in 0..OPS {
                for src in 0..OPS {
                    if r.modulators[dst][src] {
                        assert!(src > dst, "alg {alg}: {src} -> {dst}");
                        assert!(!r.carriers[src], "alg {alg}: carrier {src} also modulates");
                    }
                }
            }
        }
    }

    #[test]
    fn every_algorithm_and_preset_is_bounded() {
        let mut patches: Vec<FmParams> = FmParams::presets().iter().map(|(_, p)| *p).collect();
        for alg in 0..ALGORITHMS {
            let mut p = FmParams {
                algorithm: alg as u8,
                ..FmParams::default()
            };
            for (k, op) in p.ops.iter_mut().enumerate() {
                *op = OpParams::with(true, 1 + k as u8, 0, 1.0, 0.001, 0.3, 0.7, 0.1, 0.5);
            }
            p.feedback = 1.0;
            p.spread = 10.0;
            patches.push(p);
        }
        for p in &patches {
            for note in (24..=108).step_by(6) {
                let (l, r) = render_note(p, note, 9600);
                assert!(l.iter().chain(&r).all(|x| x.is_finite()));
                let pk = peak(&l).max(peak(&r));
                assert!(pk < 4.0, "alg {} note {note} peak {pk}", p.algorithm);
                assert!(pk > 1e-3, "alg {} note {note} silent", p.algorithm);
            }
        }
    }

    #[test]
    fn single_carrier_is_a_pure_sine() {
        let p = pure_sine_params();
        let (l, r) = render_note(&p, 69, 48000);
        // Skip the attack, count one second minus that.
        let body = &l[4800..];
        let secs = body.len() as f32 / SR;
        let freq = zero_crossings(body) as f32 / 2.0 / secs;
        assert!((freq - 440.0).abs() < 2.0, "freq {freq}");
        assert_eq!(l, r);
        // A pure sine's first-difference energy ratio is (2 sin(πf/sr))².
        let expected = (2.0 * (std::f32::consts::PI * 440.0 / SR).sin()).powi(2);
        let b = brightness(body);
        assert!((b / expected - 1.0).abs() < 0.05, "brightness {b} vs {expected}");
    }

    #[test]
    fn modulator_level_adds_high_frequencies() {
        let mut p = FmParams {
            algorithm: 0,
            ..FmParams::default()
        };
        p.ops[0] = OpParams::with(true, 1, 0, 1.0, 0.001, 1.0, 1.0, 0.1, 0.0);
        p.ops[1] = OpParams::with(true, 1, 0, 0.0, 0.001, 1.0, 1.0, 0.1, 0.0);
        let mut last = 0.0;
        for level in [0.0, 0.1, 0.3] {
            p.ops[1].level = level;
            let (l, _) = render_note(&p, 57, 24000);
            let b = brightness(&l[4800..]);
            assert!(b > last * 1.5, "level {level}: brightness {b} not above {last}");
            last = b;
        }
    }

    #[test]
    fn feedback_is_stable_at_maximum() {
        let mut p = pure_sine_params();
        p.ops[0].on = false;
        p.ops[3] = OpParams::with(true, 1, 0, 1.0, 0.001, 1.0, 1.0, 0.1, 0.0);
        p.feedback = 1.0;
        for note in [24u8, 60, 108] {
            let (l, _) = render_note(&p, note, 48000);
            assert!(l.iter().all(|x| x.is_finite()));
            let pk = peak(&l);
            assert!(pk < 2.0 && pk > 0.05, "note {note} peak {pk}");
            // Still periodic-ish rather than noise: brightness well below white noise (~2).
            let b = brightness(&l[4800..]);
            assert!(b < 1.0, "note {note}: feedback turned to noise ({b})");
        }
    }

    #[test]
    fn glide_and_spread_render() {
        let p = FmParams {
            glide: 0.2,
            spread: 20.0,
            ..FmParams::default()
        };
        let mut s = FmSynth::new(SR);
        let mut l = vec![0.0; 9600];
        let mut r = vec![0.0; 9600];
        s.note_on(48, 100);
        s.note_on(72, 100);
        s.render(&p, &mut l, &mut r);
        assert!(l.iter().zip(&r).any(|(a, b)| (a - b).abs() > 1e-3));
        assert!(peak(&l) < 4.0);
    }

    #[test]
    fn serde_round_trip() {
        for (_, p) in FmParams::presets() {
            let json = serde_json::to_string(&p).unwrap();
            let back: FmParams = serde_json::from_str(&json).unwrap();
            assert_eq!(p, back);
        }
        // Missing fields fall back to defaults.
        let partial: FmParams = serde_json::from_str(r#"{"algorithm":3}"#).unwrap();
        assert_eq!(partial.algorithm, 3);
        assert_eq!(partial.ops, FmParams::default().ops);
    }
}
