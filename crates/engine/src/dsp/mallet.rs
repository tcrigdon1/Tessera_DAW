//! "Mallet Synth": a physically modelled percussion instrument.
//!
//! Each voice has two **excitators** and two **resonators**:
//!
//! * **Mallet** – a raised-cosine force pulse of unit area whose width follows
//!   the mallet stiffness (a soft mallet stays in contact longer and so
//!   excites fewer high partials), with an optional burst of coloured noise.
//! * **Noise** – white noise through a state-variable filter (LP/BP/HP) shaped
//!   by its own ADSR, for bowed, blown or brushed excitation.
//!
//! The excitation drives two resonators, either in parallel (`1 + 2`) or in
//! series (`1 > 2`, resonator 2 is driven by resonator 1). Resonator types:
//!
//! * **Beam, Marimba, Membrane, Plate** – banks of up to [`PARTIALS`] tuned
//!   two-pole modal filters. The mode frequency ratios are derived when the
//!   synth is created (see `ModeTables`): free-free beam modes from the
//!   approximate roots `βₙ ≈ (n + ½)π`, a marimba bar tuned to 1 : 4 : 10,
//!   membrane modes from asymptotic zeros of the Bessel functions, and plate
//!   modes of a simply supported rectangular plate.
//! * **String, Pipe, Tube** – a delay-line loop (Karplus-Strong-like) with a
//!   loss filter and a dispersion all-pass. The pipe inverts at the reflection
//!   (closed end) so only odd harmonics sound.
//!
//! Per resonator: decay (T60 of the fundamental), material (how much faster
//! the high partials die), radius (brightness), inharmonics (stretches the
//! partial series or adds dispersion), listening positions for the left and
//! right outputs, hit position, coarse/fine tune, key tracking, volume and
//! pan. A global LFO modulates resonator decay and pitch.
//!
//! Nothing here allocates after [`MalletSynth::new`].

use super::envelope::Adsr;
use super::filter::Svf;
use super::{lfo_hz, midi_to_hz, Performance, Rng, SYNC_RATES, SYNC_RATE_DEFAULT};
use crate::automation::{specs_from_info, ParamSpec};
use crate::fx::{device_params, ParamKind, Scale, Unit};
use crate::model::{db_to_gain, pan_gains};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);
const TOGGLE: ParamKind = ParamKind::Toggle;

/// Resonator types, in parameter order.
pub const RES_TYPES: &[&str] = &["Beam", "Marimba", "String", "Membrane", "Plate", "Pipe", "Tube"];
pub const FILTER_TYPES: &[&str] = &["LP", "BP", "HP"];
pub const STRUCTURES: &[&str] = &["1 > 2", "1 + 2"];
pub const LFO_SHAPES: &[&str] = &["Sine", "Tri", "Square", "S&H"];

/// Most voices that sound at once.
pub const VOICES: usize = 16;
/// Modal partials per resonator.
pub const PARTIALS: usize = 20;
/// Samples between coefficient updates.
const CONTROL: usize = 32;
/// Lowest frequency a delay-line resonator can reach (sizes its buffer).
const MIN_DELAY_HZ: f32 = 20.0;

device_params! {
    pub struct MalletParams {
        mallet_on: "Mallet", 0.0, 1.0, 1.0, TOGGLE, Unit::Plain;
        mallet_volume: "Mallet Vol", -40.0, 6.0, 0.0, LIN, Unit::Db;
        mallet_vel: "Mallet Vel", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        stiffness: "Stiffness", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        stiff_vel: "Stiff Vel", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        mallet_noise: "Noise Amt", 0.0, 1.0, 0.1, LIN, Unit::Percent;
        noise_color: "Noise Color", 0.0, 1.0, 0.5, LIN, Unit::Percent;

        noise_on: "Noise", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        noise_volume: "Noise Vol", -40.0, 6.0, -6.0, LIN, Unit::Db;
        noise_vel: "Noise Vel", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        noise_filter: "Noise Filter", 0.0, 2.0, 1.0, ParamKind::Choice(FILTER_TYPES), Unit::Plain;
        noise_freq: "Noise Freq", 40.0, 18000.0, 3000.0, LOG, Unit::Hz;
        noise_q: "Noise Q", 0.5, 12.0, 1.0, LOG, Unit::Plain;
        noise_attack: "N Attack", 0.5, 2000.0, 1.0, LOG, Unit::Ms;
        noise_decay: "N Decay", 1.0, 5000.0, 120.0, LOG, Unit::Ms;
        noise_sustain: "N Sustain", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        noise_release: "N Release", 1.0, 5000.0, 60.0, LOG, Unit::Ms;

        a_on: "Res 1", 0.0, 1.0, 1.0, TOGGLE, Unit::Plain;
        a_type: "1 Type", 0.0, 6.0, 0.0, ParamKind::Choice(RES_TYPES), Unit::Plain;
        a_decay: "1 Decay", 0.01, 30.0, 1.2, LOG, Unit::Seconds;
        a_material: "1 Material", 0.0, 1.0, 0.4, LIN, Unit::Percent;
        a_radius: "1 Radius", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        a_inharm: "1 Inharm", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        a_listen_l: "1 Listen L", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        a_listen_r: "1 Listen R", 0.0, 1.0, 0.7, LIN, Unit::Percent;
        a_hit: "1 Hit", 0.0, 1.0, 0.25, LIN, Unit::Percent;
        a_coarse: "1 Coarse", -48.0, 48.0, 0.0, LIN, Unit::Semitones;
        a_fine: "1 Fine", -100.0, 100.0, 0.0, LIN, Unit::Integer;
        a_key: "1 Key Trk", 0.0, 1.0, 1.0, LIN, Unit::Percent;
        a_volume: "1 Volume", -40.0, 6.0, 0.0, LIN, Unit::Db;
        a_pan: "1 Pan", -1.0, 1.0, 0.0, LIN, Unit::Plain;

        b_on: "Res 2", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        b_type: "2 Type", 0.0, 6.0, 4.0, ParamKind::Choice(RES_TYPES), Unit::Plain;
        b_decay: "2 Decay", 0.01, 30.0, 1.5, LOG, Unit::Seconds;
        b_material: "2 Material", 0.0, 1.0, 0.3, LIN, Unit::Percent;
        b_radius: "2 Radius", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        b_inharm: "2 Inharm", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        b_listen_l: "2 Listen L", 0.0, 1.0, 0.4, LIN, Unit::Percent;
        b_listen_r: "2 Listen R", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        b_hit: "2 Hit", 0.0, 1.0, 0.35, LIN, Unit::Percent;
        b_coarse: "2 Coarse", -48.0, 48.0, 12.0, LIN, Unit::Semitones;
        b_fine: "2 Fine", -100.0, 100.0, 0.0, LIN, Unit::Integer;
        b_key: "2 Key Trk", 0.0, 1.0, 1.0, LIN, Unit::Percent;
        b_volume: "2 Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
        b_pan: "2 Pan", -1.0, 1.0, 0.0, LIN, Unit::Plain;

        structure: "Structure", 0.0, 1.0, 1.0, ParamKind::Choice(STRUCTURES), Unit::Plain;
        lfo_shape: "LFO Shape", 0.0, 3.0, 0.0, ParamKind::Choice(LFO_SHAPES), Unit::Plain;
        lfo_rate: "LFO Rate", 0.05, 20.0, 4.0, LOG, Unit::Hz;
        lfo_decay: "LFO>Decay", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        lfo_pitch: "LFO>Pitch", -12.0, 12.0, 0.0, LIN, Unit::Plain;
        release: "Release", 0.01, 30.0, 3.0, LOG, Unit::Seconds;
        transpose: "Transpose", -24.0, 24.0, 0.0, LIN, Unit::Semitones;
        voices: "Voices", 1.0, 16.0, 12.0, LIN, Unit::Integer;
        volume_db: "Volume", -40.0, 6.0, -6.0, LIN, Unit::Db;
        lfo_sync: "LFO Sync", 0.0, 1.0, 0.0, TOGGLE, Unit::Plain;
        lfo_sync_rate: "LFO Sync Rate", 0.0, 10.0, SYNC_RATE_DEFAULT, SYNC_RATES, Unit::Plain;
    }
}

/// Automatable parameters, in [`MalletParams::INFO`] order (append only).
pub static PARAMS: [ParamSpec; MalletParams::COUNT] = specs_from_info(MalletParams::INFO);

/// Index of the first parameter of resonator 1 and 2. Each resonator has
/// [`RES_PARAMS`] parameters in the order on, type, decay, material, radius,
/// inharm, listen L, listen R, hit, coarse, fine, key, volume, pan.
pub const RES_A_FIRST: usize = 17;
pub const RES_B_FIRST: usize = 31;
pub const RES_PARAMS: usize = 14;
/// Index of the LFO rate and of the LFO sync toggle (its synced rate
/// follows it).
pub const LFO_RATE: usize = 47;
pub const LFO_SYNC: usize = 54;

/// The parameters of one resonator, gathered from the flat struct.
#[derive(Debug, Clone, Copy)]
struct ResSettings {
    on: bool,
    kind: usize,
    decay: f32,
    material: f32,
    radius: f32,
    inharm: f32,
    listen_l: f32,
    listen_r: f32,
    hit: f32,
    tune: f32,
    key: f32,
    gain_l: f32,
    gain_r: f32,
}

impl MalletParams {
    fn res(&self, which: usize) -> ResSettings {
        let first = if which == 0 { RES_A_FIRST } else { RES_B_FIRST };
        let g = |i: usize| self.get(first + i);
        let (pl, pr) = pan_gains(g(13));
        let vol = db_to_gain(g(12));
        ResSettings {
            on: g(0) >= 0.5,
            kind: (g(1).round().max(0.0) as usize).min(RES_TYPES.len() - 1),
            decay: g(2).clamp(0.01, 30.0),
            material: g(3).clamp(0.0, 1.0),
            radius: g(4).clamp(0.0, 1.0),
            inharm: g(5).clamp(-1.0, 1.0),
            listen_l: g(6).clamp(0.0, 1.0),
            listen_r: g(7).clamp(0.0, 1.0),
            hit: g(8).clamp(0.0, 1.0),
            tune: g(9).clamp(-48.0, 48.0) + g(10).clamp(-100.0, 100.0) / 100.0,
            key: g(11).clamp(0.0, 1.0),
            gain_l: vol * pl,
            gain_r: vol * pr,
        }
    }
}

/// Factory presets: (browser category, name, settings).
pub fn presets() -> Vec<(&'static str, &'static str, MalletParams)> {
    let d = MalletParams::default();
    vec![
        (
            "Mallets",
            "Rosewood Marimba",
            MalletParams {
                a_type: 1.0,
                a_decay: 0.9,
                a_material: 0.65,
                a_radius: 0.45,
                a_hit: 0.15,
                stiffness: 0.45,
                mallet_noise: 0.05,
                b_on: 1.0,
                b_type: 6.0,
                b_decay: 0.35,
                b_coarse: 0.0,
                b_material: 0.5,
                b_radius: 0.3,
                b_volume: -14.0,
                ..d
            },
        ),
        (
            "Mallets",
            "Soft Vibes",
            MalletParams {
                a_type: 0.0,
                a_decay: 4.5,
                a_material: 0.12,
                a_radius: 0.35,
                a_hit: 0.1,
                stiffness: 0.3,
                mallet_noise: 0.02,
                lfo_rate: 5.5,
                lfo_pitch: 0.08,
                lfo_decay: 0.2,
                release: 1.2,
                ..d
            },
        ),
        (
            "Mallets",
            "Glass Glockenspiel",
            MalletParams {
                a_type: 0.0,
                a_decay: 3.0,
                a_material: 0.05,
                a_radius: 0.8,
                a_hit: 0.3,
                stiffness: 0.9,
                transpose: 24.0,
                volume_db: -9.0,
                release: 6.0,
                ..d
            },
        ),
        (
            "Percussion",
            "Tight Tom",
            MalletParams {
                a_type: 3.0,
                a_decay: 0.45,
                a_material: 0.55,
                a_radius: 0.4,
                a_hit: 0.35,
                a_key: 0.5,
                stiffness: 0.6,
                mallet_noise: 0.3,
                noise_on: 1.0,
                noise_volume: -14.0,
                noise_filter: 0.0,
                noise_freq: 2500.0,
                noise_decay: 40.0,
                transpose: -12.0,
                ..d
            },
        ),
        (
            "Percussion",
            "Steel Plate",
            MalletParams {
                a_type: 4.0,
                a_decay: 3.5,
                a_material: 0.1,
                a_radius: 0.7,
                a_inharm: 0.2,
                a_hit: 0.6,
                stiffness: 0.75,
                volume_db: -9.0,
                release: 8.0,
                ..d
            },
        ),
        (
            "Keys",
            "Struck String Keys",
            MalletParams {
                a_type: 2.0,
                a_decay: 3.0,
                a_material: 0.35,
                a_radius: 0.6,
                a_inharm: 0.15,
                a_hit: 0.12,
                stiffness: 0.55,
                b_on: 1.0,
                b_type: 0.0,
                b_decay: 1.2,
                b_coarse: 24.0,
                b_volume: -18.0,
                release: 0.4,
                ..d
            },
        ),
    ]
}

/// For displays: the partials of resonator `which` (0 or 1) as
/// (frequency ratio to the fundamental, relative amplitude 0..1). Runs on the
/// UI thread (allocates).
pub fn partial_preview(p: &MalletParams, which: usize) -> Vec<(f32, f32)> {
    let s = p.res(which.min(1));
    let tilt = (1.0 - s.radius) * 1.3 + 0.2;
    if is_delay_type(s.kind) {
        let pipe = s.kind == 5;
        let hit = 0.05 + 0.45 * s.hit;
        (1..=PARTIALS)
            .map(|k| {
                let h = if pipe { 2 * k - 1 } else { k };
                let ph = std::f32::consts::TAU * h as f32 * if pipe { hit * 0.5 } else { hit };
                let comb = ((1.0 - 0.7 * ph.cos()).powi(2) + (0.7 * ph.sin()).powi(2)).sqrt() / 1.7;
                let damp = (h as f32).powf(-s.material * 0.8);
                (h as f32, (h as f32).powf(-tilt) * comb * damp)
            })
            .collect()
    } else {
        let tables = ModeTables::new();
        let stretch = 1.0 + 0.25 * s.inharm;
        tables
            .ratios(s.kind)
            .iter()
            .enumerate()
            .map(|(k, r)| {
                let n = k + 1;
                let ratio = r.powf(stretch);
                let damp = ratio.powf(-s.material * 0.8);
                (ratio, ((n as f32).powf(-tilt) * mode_shape(n, s.hit)).abs() * damp)
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Mode tables
// ---------------------------------------------------------------------------

/// Partial frequency ratios (relative to the fundamental) of the modal
/// resonator types, derived once at construction.
struct ModeTables {
    beam: [f32; PARTIALS],
    marimba: [f32; PARTIALS],
    membrane: [f32; PARTIALS],
    plate: [f32; PARTIALS],
}

impl ModeTables {
    fn new() -> Self {
        use std::f64::consts::PI;
        // Free-free beam: f_n ∝ β_n², with β₁ ≈ 4.7300 and β_n ≈ (n + ½)π
        // for the higher modes.
        let beta = |n: usize| -> f64 {
            if n == 1 {
                4.730_04
            } else {
                (n as f64 + 0.5) * PI
            }
        };
        let beam: [f32; PARTIALS] = std::array::from_fn(|k| ((beta(k + 1) / beta(1)).powi(2)) as f32);
        // Marimba bar: the undercut tunes the first three modes to 1 : 4 : 10;
        // above that the series continues like a beam.
        let marimba: [f32; PARTIALS] = std::array::from_fn(|k| match k {
            0 => 1.0,
            1 => 3.98,
            2 => 9.92,
            _ => beam[k] * 9.92 / beam[2],
        });
        // Circular membrane: zeros j(m, n) of the Bessel function J_m, from
        // McMahon's asymptotic expansion, relative to j(0, 1).
        let mut zeros = Vec::with_capacity(64);
        for m in 0..8 {
            for n in 1..9 {
                let b = (n as f64 + m as f64 / 2.0 - 0.25) * PI;
                let mu = 4.0 * (m * m) as f64;
                let e = 8.0 * b;
                let j = b - (mu - 1.0) / e - 4.0 * (mu - 1.0) * (7.0 * mu - 31.0) / (3.0 * e * e * e);
                zeros.push(j);
            }
        }
        zeros.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let membrane: [f32; PARTIALS] = std::array::from_fn(|k| (zeros[k] / zeros[0]) as f32);
        // Simply supported rectangular plate with side ratio 1 : 0.7:
        // f(m, n) ∝ m² + (n / 0.7)².
        let aspect: f64 = 0.7;
        let mut plate_modes = Vec::with_capacity(64);
        for m in 1..9 {
            for n in 1..9 {
                plate_modes.push((m * m) as f64 + (n as f64 / aspect).powi(2));
            }
        }
        plate_modes.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let plate: [f32; PARTIALS] = std::array::from_fn(|k| (plate_modes[k] / plate_modes[0]) as f32);
        Self {
            beam,
            marimba,
            membrane,
            plate,
        }
    }

    fn ratios(&self, kind: usize) -> &[f32; PARTIALS] {
        match kind {
            1 => &self.marimba,
            3 => &self.membrane,
            4 => &self.plate,
            _ => &self.beam,
        }
    }
}

/// True for the resonator types modelled with a delay line.
pub fn is_delay_type(kind: usize) -> bool {
    matches!(kind, 2 | 5 | 6)
}

/// Spatial weight of mode `n` (1-based) at position `x` (0 = centre,
/// 1 = near the edge).
#[inline]
fn mode_shape(n: usize, x: f32) -> f32 {
    let p = 0.5 + 0.47 * x;
    (n as f32 * std::f32::consts::PI * p).sin()
}

// ---------------------------------------------------------------------------
// Resonators
// ---------------------------------------------------------------------------

struct Resonator {
    // Modal bank.
    y1: [f32; PARTIALS],
    y2: [f32; PARTIALS],
    a1: [f32; PARTIALS],
    a2: [f32; PARTIALS],
    gin: [f32; PARTIALS],
    wl: [f32; PARTIALS],
    wr: [f32; PARTIALS],
    // Delay-line loop.
    buf: Vec<f32>,
    ebuf: Vec<f32>,
    w: usize,
    delay: f32,
    hit_delay: usize,
    tap_l: usize,
    tap_r: usize,
    loss: f32,
    prev: f32,
    ap: f32,
    ap_x1: f32,
    ap_y1: f32,
    feedback: f32,
    /// Input scale of the delay loop (a pulse of unit area spread over one
    /// period would otherwise be very quiet).
    in_scale: f32,
    modal: bool,
    /// Kind the state was last configured for (switching clears the state).
    kind: usize,
}

// The modal bank keeps its state in parallel arrays indexed together.
#[allow(clippy::needless_range_loop)]
impl Resonator {
    fn new(sample_rate: f32) -> Self {
        let len = (sample_rate / MIN_DELAY_HZ) as usize + 8;
        Self {
            y1: [0.0; PARTIALS],
            y2: [0.0; PARTIALS],
            a1: [0.0; PARTIALS],
            a2: [0.0; PARTIALS],
            gin: [0.0; PARTIALS],
            wl: [0.0; PARTIALS],
            wr: [0.0; PARTIALS],
            buf: vec![0.0; len],
            ebuf: vec![0.0; len],
            w: 0,
            delay: 100.0,
            hit_delay: 1,
            tap_l: 1,
            tap_r: 1,
            loss: 0.2,
            prev: 0.0,
            ap: 0.0,
            ap_x1: 0.0,
            ap_y1: 0.0,
            feedback: 0.0,
            in_scale: 1.0,
            modal: true,
            kind: 0,
        }
    }

    fn clear(&mut self) {
        self.y1 = [0.0; PARTIALS];
        self.y2 = [0.0; PARTIALS];
        self.buf.fill(0.0);
        self.ebuf.fill(0.0);
        self.prev = 0.0;
        self.ap_x1 = 0.0;
        self.ap_y1 = 0.0;
    }

    /// Recomputes the coefficients for fundamental `freq` (Hz) and T60
    /// `decay` (seconds).
    fn configure(&mut self, s: &ResSettings, tables: &ModeTables, freq: f32, decay: f32, sr: f32) {
        if s.kind != self.kind {
            self.kind = s.kind;
            self.clear();
        }
        self.modal = !is_delay_type(s.kind);
        let decay = decay.clamp(0.005, 60.0);
        if self.modal {
            let ratios = tables.ratios(s.kind);
            let stretch = 1.0 + 0.25 * s.inharm;
            let tilt = (1.0 - s.radius) * 1.3 + 0.2;
            let mut energy = 0.0f32;
            for k in 0..PARTIALS {
                let n = k + 1;
                let ratio = ratios[k].powf(stretch);
                let f = freq * ratio;
                let w = std::f32::consts::TAU * f / sr;
                if w >= std::f32::consts::PI * 0.9 || w <= 0.0 {
                    self.a1[k] = 0.0;
                    self.a2[k] = 0.0;
                    self.gin[k] = 0.0;
                    self.y1[k] = 0.0;
                    self.y2[k] = 0.0;
                    continue;
                }
                // Higher partials of softer materials die faster.
                let t60 = decay / ratio.powf(s.material * 1.6);
                let r = (-6.9 / (t60.max(0.002) * sr)).exp();
                self.a1[k] = 2.0 * r * w.cos();
                self.a2[k] = r * r;
                let amp = (n as f32).powf(-tilt) * mode_shape(n, s.hit);
                energy += amp * amp;
                self.gin[k] = amp * w.sin();
                self.wl[k] = mode_shape(n, s.listen_l);
                self.wr[k] = mode_shape(n, s.listen_r);
            }
            let norm = 1.2 / (energy.sqrt() + 0.05);
            for g in &mut self.gin {
                *g *= norm;
            }
        } else {
            let pipe = s.kind == 5;
            let tube = s.kind == 6;
            let f = freq.clamp(MIN_DELAY_HZ, sr * 0.25);
            // Round-trip length of the loop; the inverting pipe needs two.
            let period = if pipe { sr / (2.0 * f) } else { sr / f };
            let darkness = s.material * 0.6 + (1.0 - s.radius) * 0.4;
            let loss = if tube {
                0.2 + 0.3 * darkness
            } else {
                0.02 + 0.48 * darkness
            };
            let ap = if s.kind == 2 { s.inharm * 0.7 } else { s.inharm * 0.3 };
            let w0 = std::f32::consts::TAU * f / sr;
            // Phase delays of the loss filter and all-pass at the fundamental.
            let lp_delay = loss_phase_delay(loss, w0);
            let ap_delay = allpass_phase_delay(ap, w0);
            let max = (self.buf.len() - 4) as f32;
            self.delay = (period - lp_delay - ap_delay).clamp(1.0, max);
            self.loss = loss;
            self.ap = ap;
            // Loop gain from the decay, compensating the filter's loss at f0.
            let lp_mag = ((1.0 - loss) * (1.0 - loss) + loss * loss + 2.0 * loss * (1.0 - loss) * w0.cos())
                .sqrt()
                .max(0.05);
            let per_period = 10f32.powf(-3.0 * period / (decay * sr));
            self.feedback = (per_period / lp_mag).min(0.9995) * if pipe { -1.0 } else { 1.0 };
            self.hit_delay = ((0.05 + 0.45 * s.hit) * period).clamp(1.0, max) as usize;
            self.tap_l = ((0.05 + 0.45 * s.listen_l) * period).clamp(1.0, max) as usize;
            self.tap_r = ((0.05 + 0.45 * s.listen_r) * period).clamp(1.0, max) as usize;
            self.in_scale = 3.0 * period.sqrt();
        }
    }

    /// Processes one input sample; returns the (left, right) output.
    #[inline]
    fn tick(&mut self, x: f32) -> (f32, f32) {
        if self.modal {
            let mut l = 0.0;
            let mut r = 0.0;
            for k in 0..PARTIALS {
                let y = self.a1[k] * self.y1[k] - self.a2[k] * self.y2[k] + self.gin[k] * x;
                self.y2[k] = self.y1[k];
                self.y1[k] = y;
                l += self.wl[k] * y;
                r += self.wr[k] * y;
            }
            (l, r)
        } else {
            let len = self.buf.len();
            // Excitation with a comb for the hit position.
            self.ebuf[self.w] = x;
            let xe = x - 0.7 * self.ebuf[(self.w + len - self.hit_delay) % len];
            // Fractional read of the loop.
            let pos = self.w as f32 + len as f32 - self.delay;
            let i = pos as usize;
            let frac = pos - i as f32;
            let a = self.buf[i % len];
            let b = self.buf[(i + 1) % len];
            let d = a + (b - a) * frac;
            let f = (1.0 - self.loss) * d + self.loss * self.prev;
            self.prev = d;
            let y = self.ap * f + self.ap_x1 - self.ap * self.ap_y1;
            self.ap_x1 = f;
            self.ap_y1 = y;
            let v = self.feedback * y + xe * self.in_scale;
            self.buf[self.w] = v;
            let l = 0.5 * (v - 0.7 * self.buf[(self.w + len - self.tap_l) % len]);
            let r = 0.5 * (v - 0.7 * self.buf[(self.w + len - self.tap_r) % len]);
            self.w = (self.w + 1) % len;
            (l, r)
        }
    }

    fn flush_denormals(&mut self) {
        for k in 0..PARTIALS {
            if self.y1[k].abs() < 1e-15 {
                self.y1[k] = 0.0;
            }
            if self.y2[k].abs() < 1e-15 {
                self.y2[k] = 0.0;
            }
        }
        if self.prev.abs() < 1e-15 {
            self.prev = 0.0;
        }
        if self.ap_y1.abs() < 1e-15 {
            self.ap_y1 = 0.0;
        }
    }
}

/// Phase delay (samples) of the two-tap loss filter `(1-s) + s·z⁻¹` at `w`.
fn loss_phase_delay(s: f32, w: f32) -> f32 {
    let re = (1.0 - s) + s * w.cos();
    let im = -s * w.sin();
    -im.atan2(re) / w.max(1e-6)
}

/// Phase delay (samples) of the all-pass `(a + z⁻¹) / (1 + a·z⁻¹)` at `w`.
fn allpass_phase_delay(a: f32, w: f32) -> f32 {
    let num = (-w.sin()).atan2(a + w.cos());
    let den = (-a * w.sin()).atan2(1.0 + a * w.cos());
    -(num - den) / w.max(1e-6)
}

// ---------------------------------------------------------------------------
// Voices
// ---------------------------------------------------------------------------

struct Voice {
    note: u8,
    velocity: f32,
    held: bool,
    active: bool,
    age: u64,
    /// Mallet pulse: position and length in samples (pos >= len = done).
    pulse_pos: usize,
    pulse_len: usize,
    noise_env: Adsr,
    noise_filter: Svf,
    color_state: f32,
    res: [Resonator; 2],
    /// Consecutive quiet control blocks.
    quiet: u32,
}

impl Voice {
    fn new(sr: f32) -> Self {
        Self {
            note: 60,
            velocity: 0.0,
            held: false,
            active: false,
            age: 0,
            pulse_pos: 0,
            pulse_len: 0,
            noise_env: Adsr::new(sr),
            noise_filter: Svf::default(),
            color_state: 0.0,
            res: [Resonator::new(sr), Resonator::new(sr)],
            quiet: 0,
        }
    }
}

pub struct MalletSynth {
    sample_rate: f32,
    pub perf: Performance,
    voices: Vec<Voice>,
    tables: ModeTables,
    counter: u64,
    max_voices: usize,
    rng: Rng,
    lfo_phase: f32,
    lfo_hold: f32,
    /// Stiffness settings seen on the last render (note-on uses them to set
    /// the mallet's contact time).
    stiffness: f32,
    stiff_vel: f32,
    transpose: f32,
}

impl MalletSynth {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1000.0);
        let d = MalletParams::default();
        Self {
            perf: Performance::default(),
            sample_rate: sr,
            voices: (0..VOICES).map(|_| Voice::new(sr)).collect(),
            tables: ModeTables::new(),
            counter: 0,
            max_voices: d.voices as usize,
            rng: Rng::new(0x5eed_1234),
            lfo_phase: 0.0,
            lfo_hold: 0.0,
            stiffness: d.stiffness,
            stiff_vel: d.stiff_vel,
            transpose: d.transpose,
        }
    }

    pub fn active_voices(&self) -> usize {
        self.voices.iter().filter(|v| v.active).count()
    }

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        self.counter += 1;
        let limit = self.max_voices.clamp(1, VOICES);
        // A note that is still ringing is struck again on the same voice.
        let same = self.voices.iter().position(|v| v.active && v.note == pitch);
        let idx = same.unwrap_or_else(|| {
            let active = self.active_voices();
            let free = self.voices.iter().position(|v| !v.active);
            match free {
                Some(i) if active < limit => i,
                _ => {
                    // Steal the oldest released voice, else the oldest.
                    let released = self
                        .voices
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.active && !v.held)
                        .min_by_key(|(_, v)| v.age)
                        .map(|(i, _)| i);
                    released.unwrap_or_else(|| {
                        self.voices
                            .iter()
                            .enumerate()
                            .filter(|(_, v)| v.active)
                            .min_by_key(|(_, v)| v.age)
                            .map(|(i, _)| i)
                            .unwrap_or(0)
                    })
                }
            }
        });
        let sr = self.sample_rate;
        let vel = velocity as f32 / 127.0;
        let stiff = (self.stiffness + self.stiff_vel * (vel - 0.5)).clamp(0.0, 1.0);
        // Contact time from 8 ms (soft) to 0.25 ms (hard), exponentially.
        let contact = 0.008 * (0.000_25f32 / 0.008).powf(stiff);
        // Never longer than about half a period of the note, so high notes
        // keep their level with soft mallets.
        let contact = contact.min(0.6 / midi_to_hz((pitch as f32 + self.transpose).clamp(0.0, 140.0)));
        let v = &mut self.voices[idx];
        if same.is_none() {
            v.res[0].clear();
            v.res[1].clear();
            v.noise_filter.reset();
            v.color_state = 0.0;
        }
        v.note = pitch;
        v.velocity = vel;
        v.held = true;
        v.active = true;
        v.age = self.counter;
        v.pulse_pos = 0;
        v.pulse_len = ((contact * sr) as usize).max(2);
        v.quiet = 0;
        v.noise_env.reset();
        v.noise_env.trigger();
    }

    pub fn note_off(&mut self, pitch: u8) {
        for v in &mut self.voices {
            if v.held && v.note == pitch {
                v.held = false;
                v.noise_env.release();
            }
        }
    }

    pub fn all_notes_off(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.noise_env.release();
        }
    }

    pub fn panic(&mut self) {
        for v in &mut self.voices {
            v.held = false;
            v.active = false;
            v.noise_env.reset();
            v.res[0].clear();
            v.res[1].clear();
        }
    }

    fn lfo_value(&self, shape: usize) -> f32 {
        let ph = self.lfo_phase;
        match shape {
            1 => 1.0 - 4.0 * (ph - 0.5).abs(),
            2 => {
                if ph < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
            3 => self.lfo_hold,
            _ => (ph * std::f32::consts::TAU).sin(),
        }
    }

    pub fn render(&mut self, p: &MalletParams, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let n = left.len().min(right.len());
        self.max_voices = (p.voices.round() as usize).clamp(1, VOICES);
        self.stiffness = p.stiffness.clamp(0.0, 1.0);
        self.stiff_vel = p.stiff_vel.clamp(0.0, 1.0);
        self.transpose = p.transpose.clamp(-48.0, 48.0);
        let res_set = [p.res(0), p.res(1)];
        let serial = p.structure < 0.5;
        let mallet_on = p.mallet_on >= 0.5;
        let noise_on = p.noise_on >= 0.5;
        let mallet_gain = db_to_gain(p.mallet_volume);
        let noise_gain = db_to_gain(p.noise_volume) * 0.25;
        let mallet_vel = p.mallet_vel.clamp(0.0, 1.0);
        let noise_vel = p.noise_vel.clamp(0.0, 1.0);
        let mallet_noise = p.mallet_noise.clamp(0.0, 1.0);
        // Mallet noise colour: one-pole low-pass from ~500 Hz to open.
        let color_hz = 500.0 * 40f32.powf(p.noise_color.clamp(0.0, 1.0));
        let color_c = 1.0 - (-std::f32::consts::TAU * color_hz / sr).exp();
        let nf_type = p.noise_filter.round().clamp(0.0, 2.0) as usize;
        let nf_freq = p.noise_freq.clamp(20.0, 20000.0);
        let nf_q = p.noise_q.clamp(0.5, 20.0);
        let (na, nd, ns, nr) = (
            p.noise_attack.max(0.1) / 1000.0,
            p.noise_decay.max(0.5) / 1000.0,
            p.noise_sustain.clamp(0.0, 1.0),
            p.noise_release.max(0.5) / 1000.0,
        );
        let out_gain = db_to_gain(p.volume_db);
        let lfo_shape = (p.lfo_shape.round().max(0.0) as usize).min(3);
        let lfo_inc = lfo_hz(p.lfo_rate, p.lfo_sync >= 0.5, p.lfo_sync_rate, self.perf.tempo).clamp(0.01, 50.0) / sr;
        let release = p.release.clamp(0.01, 30.0);
        let transpose = p.transpose.clamp(-48.0, 48.0) + self.perf.bend;

        let mut start = 0;
        while start < n {
            let len = CONTROL.min(n - start);
            // Control-rate LFO.
            let lfo = self.lfo_value(lfo_shape);
            let new_phase = self.lfo_phase + lfo_inc * len as f32;
            if new_phase >= 1.0 {
                self.lfo_hold = self.rng.next_bipolar();
            }
            self.lfo_phase = new_phase.fract();
            let decay_mul = 2f32.powf(2.0 * p.lfo_decay.clamp(-1.0, 1.0) * lfo);
            let pitch_mod = p.lfo_pitch.clamp(-12.0, 12.0) * lfo;

            for v in self.voices.iter_mut() {
                if !v.active {
                    continue;
                }
                for (r, s) in v.res.iter_mut().zip(res_set.iter()) {
                    if !s.on {
                        continue;
                    }
                    let note = 60.0 + (v.note as f32 - 60.0) * s.key + transpose + s.tune + pitch_mod;
                    let freq = midi_to_hz(note.clamp(-24.0, 150.0));
                    let mut decay = s.decay * decay_mul;
                    if !v.held {
                        decay = decay.min(release);
                    }
                    r.configure(s, &self.tables, freq, decay, sr);
                }
                let vel = v.velocity;
                let m_amp = mallet_gain * (1.0 - mallet_vel + mallet_vel * vel);
                let n_amp = noise_gain * (1.0 - noise_vel + noise_vel * vel);
                let mut peak = 0.0f32;
                for i in start..start + len {
                    // Excitation.
                    let mut x = 0.0;
                    if v.pulse_pos < v.pulse_len {
                        let pl = v.pulse_len as f32;
                        let shape = (1.0 - (std::f32::consts::TAU * v.pulse_pos as f32 / pl).cos()) / pl;
                        v.pulse_pos += 1;
                        if mallet_on {
                            let noise = self.rng.next_bipolar();
                            v.color_state += color_c * (noise - v.color_state);
                            x += m_amp * shape * ((1.0 - mallet_noise) + mallet_noise * 3.0 * v.color_state);
                        }
                    }
                    let env = v.noise_env.next(na, nd, ns, nr);
                    if noise_on && env > 0.0 {
                        let w = self.rng.next_bipolar();
                        let o = v.noise_filter.process(w, nf_freq, nf_q, sr);
                        let f = match nf_type {
                            0 => o.low,
                            2 => o.high,
                            _ => o.band,
                        };
                        x += f * env * n_amp;
                    }
                    // Resonators.
                    let (mut l, mut r) = (0.0, 0.0);
                    let mut a_mono = 0.0;
                    if res_set[0].on {
                        let (al, ar) = v.res[0].tick(x);
                        l += al * res_set[0].gain_l;
                        r += ar * res_set[0].gain_r;
                        a_mono = 0.5 * (al + ar);
                    }
                    if res_set[1].on {
                        let input = if serial && res_set[0].on { a_mono * 0.05 } else { x };
                        let (bl, br) = v.res[1].tick(input);
                        l += bl * res_set[1].gain_l;
                        r += br * res_set[1].gain_r;
                    }
                    let l = soft_clip(l * out_gain);
                    let r = soft_clip(r * out_gain);
                    peak = peak.max(l.abs()).max(r.abs());
                    left[i] += l;
                    right[i] += r;
                }
                v.res[0].flush_denormals();
                v.res[1].flush_denormals();
                let exciting = v.pulse_pos < v.pulse_len || (noise_on && v.noise_env.is_active());
                if !exciting && peak < 2e-5 {
                    v.quiet += 1;
                    if v.quiet > 4 {
                        v.active = false;
                        v.held = false;
                    }
                } else {
                    v.quiet = 0;
                }
            }
            start += len;
        }
    }
}

/// Smooth saturation that leaves normal levels almost untouched and keeps
/// the output bounded (|y| < 2).
#[inline]
fn soft_clip(x: f32) -> f32 {
    if !x.is_finite() {
        return 0.0;
    }
    x / (1.0 + 0.25 * x * x).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn render(s: &mut MalletSynth, p: &MalletParams, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let mut l = vec![0.0; frames];
        let mut r = vec![0.0; frames];
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            s.render(p, cl, cr);
        }
        (l, r)
    }

    fn peak(v: &[f32]) -> f32 {
        v.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|x| x * x).sum::<f32>() / v.len().max(1) as f32).sqrt()
    }

    /// Magnitude of the frequency `f` in `x` (Goertzel).
    fn goertzel(x: &[f32], f: f32) -> f32 {
        let w = std::f32::consts::TAU * f / SR;
        let c = 2.0 * w.cos();
        let (mut s1, mut s2) = (0.0f32, 0.0f32);
        for &v in x {
            let s = v + c * s1 - s2;
            s2 = s1;
            s1 = s;
        }
        (s1 * s1 + s2 * s2 - c * s1 * s2).max(0.0).sqrt()
    }

    /// The strongest frequency between 40 Hz and 4 kHz.
    fn strongest(x: &[f32]) -> f32 {
        let mut best = (0.0, 0.0);
        let mut f = 40.0;
        while f < 4000.0 {
            let m = goertzel(x, f);
            if m > best.1 {
                best = (f, m);
            }
            f *= 1.002;
        }
        best.0
    }

    #[test]
    fn mode_tables_are_ascending_and_plausible() {
        let t = ModeTables::new();
        assert!((t.beam[1] - 2.756).abs() < 0.02, "{:?}", t.beam);
        assert!((t.beam[2] - 5.404).abs() < 0.03);
        assert!((t.membrane[1] - 1.594).abs() < 0.02, "{:?}", t.membrane);
        assert!((t.membrane[2] - 2.136).abs() < 0.03);
        for table in [t.beam, t.marimba, t.membrane, t.plate] {
            assert_eq!(table[0], 1.0);
            for k in 1..PARTIALS {
                assert!(table[k] >= table[k - 1], "{table:?}");
            }
        }
    }

    #[test]
    fn modal_fundamental_is_at_note_pitch() {
        for kind in [0.0, 1.0, 3.0, 4.0] {
            let p = MalletParams {
                a_type: kind,
                a_radius: 0.0,
                a_hit: 0.0,
                a_listen_l: 0.0,
                a_listen_r: 0.0,
                ..MalletParams::default()
            };
            let mut s = MalletSynth::new(SR);
            s.note_on(57, 100); // A3 = 220 Hz
            let (l, _) = render(&mut s, &p, 24000);
            let f = strongest(&l[2000..]);
            assert!((f - 220.0).abs() < 3.0, "type {kind}: strongest {f}");
        }
    }

    #[test]
    fn delay_resonators_are_in_tune() {
        for kind in [2.0, 5.0, 6.0] {
            let p = MalletParams {
                a_type: kind,
                a_decay: 4.0,
                a_hit: 1.0,
                a_listen_l: 1.0,
                a_listen_r: 1.0,
                ..MalletParams::default()
            };
            let mut s = MalletSynth::new(SR);
            s.note_on(57, 100);
            let (l, _) = render(&mut s, &p, 24000);
            let f = strongest(&l[4000..]);
            assert!((f - 220.0).abs() < 3.0, "type {kind}: strongest {f}");
            assert!(peak(&l) > 0.02 && peak(&l) < 2.0, "type {kind}: peak {}", peak(&l));
        }
    }

    #[test]
    fn coarse_tune_shifts_pitch() {
        let p = MalletParams {
            a_coarse: 12.0,
            a_radius: 0.0,
            a_hit: 0.0,
            a_listen_l: 0.0,
            a_listen_r: 0.0,
            ..MalletParams::default()
        };
        let mut s = MalletSynth::new(SR);
        s.note_on(57, 100);
        let (l, _) = render(&mut s, &p, 24000);
        let f = strongest(&l[2000..]);
        assert!((f - 440.0).abs() < 5.0, "strongest {f}");
    }

    #[test]
    fn notes_decay_with_the_decay_setting() {
        let level = |decay: f32| {
            let p = MalletParams {
                a_decay: decay,
                release: 30.0,
                ..MalletParams::default()
            };
            let mut s = MalletSynth::new(SR);
            s.note_on(60, 100);
            let (l, _) = render(&mut s, &p, 48000);
            (rms(&l[0..4800]), rms(&l[43200..48000]))
        };
        let (start_short, end_short) = level(0.2);
        let (start_long, end_long) = level(4.0);
        assert!(start_short > 0.01 && start_long > 0.01);
        assert!(end_short < start_short * 0.001, "short {start_short} -> {end_short}");
        assert!(end_long > start_long * 0.2, "long {start_long} -> {end_long}");
    }

    #[test]
    fn level_is_reasonable_and_voices_free_up() {
        let p = MalletParams {
            a_decay: 0.5,
            ..MalletParams::default()
        };
        let mut s = MalletSynth::new(SR);
        s.note_on(60, 110);
        let (l, r) = render(&mut s, &p, 9600);
        let pk = peak(&l).max(peak(&r));
        assert!(pk > 0.1 && pk < 1.2, "peak {pk}");
        s.note_off(60);
        render(&mut s, &p, 96000);
        assert_eq!(s.active_voices(), 0);
    }

    #[test]
    fn note_off_damps_with_release() {
        let p = MalletParams {
            a_decay: 20.0,
            release: 0.05,
            ..MalletParams::default()
        };
        let mut s = MalletSynth::new(SR);
        s.note_on(60, 100);
        render(&mut s, &p, 4800);
        s.note_off(60);
        let (l, _) = render(&mut s, &p, 24000);
        assert!(rms(&l[19200..]) < 1e-4, "{}", rms(&l[19200..]));
    }

    #[test]
    fn panic_silences() {
        let p = MalletParams::default();
        let mut s = MalletSynth::new(SR);
        s.note_on(60, 100);
        s.note_on(64, 100);
        render(&mut s, &p, 2000);
        s.panic();
        let (l, r) = render(&mut s, &p, 2000);
        assert_eq!(peak(&l), 0.0);
        assert_eq!(peak(&r), 0.0);
    }

    #[test]
    fn polyphony_is_limited() {
        let p = MalletParams {
            voices: 4.0,
            ..MalletParams::default()
        };
        let mut s = MalletSynth::new(SR);
        render(&mut s, &p, 64);
        for n in 0..10 {
            s.note_on(48 + n, 100);
        }
        assert!(s.active_voices() <= 4);
        render(&mut s, &MalletParams::default(), 64);
        for n in 0..30 {
            s.note_on(30 + n, 100);
        }
        assert!(s.active_voices() <= VOICES);
    }

    #[test]
    fn extreme_settings_stay_finite_and_bounded() {
        let mut rng = Rng::new(99);
        for trial in 0..60 {
            let mut p = MalletParams::default();
            for (i, info) in MalletParams::INFO.iter().enumerate() {
                let u = rng.next_bipolar() * 0.5 + 0.5;
                let v = match trial % 3 {
                    0 => info.min,
                    1 => info.max,
                    _ => info.min + (info.max - info.min) * u,
                };
                p.set(i, v);
            }
            p.voices = 4.0;
            let mut s = MalletSynth::new(SR);
            render(&mut s, &p, 64);
            for n in [0u8, 24, 60, 100, 127] {
                s.note_on(n, 127);
            }
            let (l, r) = render(&mut s, &p, 6000);
            for x in l.iter().chain(r.iter()) {
                assert!(x.is_finite() && x.abs() <= 8.0, "trial {trial}: {x}");
            }
        }
    }

    #[test]
    fn serial_and_noise_modes_sound() {
        let p = MalletParams {
            b_on: 1.0,
            structure: 0.0,
            noise_on: 1.0,
            noise_sustain: 0.6,
            b_type: 2.0,
            ..MalletParams::default()
        };
        let mut s = MalletSynth::new(SR);
        s.note_on(60, 100);
        let (l, r) = render(&mut s, &p, 24000);
        let pk = peak(&l).max(peak(&r));
        assert!(pk > 0.02 && pk < 2.0, "peak {pk}");
    }

    #[test]
    fn presets_and_defaults_are_in_range() {
        for (_, name, p) in presets() {
            for (i, info) in MalletParams::INFO.iter().enumerate() {
                let v = p.get(i);
                assert!(v >= info.min && v <= info.max, "{name}: {} = {v}", info.name);
            }
            let mut s = MalletSynth::new(SR);
            s.note_on(60, 100);
            let (l, r) = render(&mut s, &p, 12000);
            let pk = peak(&l).max(peak(&r));
            assert!(pk > 0.05 && pk < 1.5, "{name}: peak {pk}");
        }
        assert_eq!(MalletParams::INFO[RES_A_FIRST].name, "Res 1");
        assert_eq!(MalletParams::INFO[RES_B_FIRST].name, "Res 2");
        assert_eq!(MalletParams::INFO[RES_B_FIRST + RES_PARAMS - 1].name, "2 Pan");
    }
}
