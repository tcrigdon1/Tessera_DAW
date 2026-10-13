//! Hybrid Reverb: a convolution reverb and an algorithmic reverb, in series
//! or in parallel.
//!
//! Convolution: there are no impulse response files, so the IRs are
//! synthesised (shaped, filtered, decorrelated noise with early reflections;
//! a dispersive spring; a reversed hall) once per sample rate on the UI
//! thread, partitioned and transformed into an [`IrBank`] shared by every
//! instance. The convolver is uniformly partitioned overlap-save: each block
//! of `B` input samples is transformed once into a frequency-domain delay
//! line and multiplied with every IR partition, so the cost per sample is
//! independent of the block size and nothing is allocated while running.
//! IR decay and attack are envelopes applied per partition, so they change
//! without recomputing the IR. Switching IR crossfades over one block.
//!
//! Algorithm: an original eight-line feedback delay network with a Hadamard
//! mixing matrix, input allpass diffusion, per-line damping and modulated
//! delays, and an octave-up pitch shifter in the feedback path for Shimmer.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, PI};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use super::biquad::{Biquad, Coefs, FilterShape, OnePole};
use super::fft::{Complex, RealFft};
use super::{device_params, smoothing, FxContext, ParamKind, Scale, Unit};
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const ROUTINGS: [&str; 4] = ["Serial", "Parallel", "Algorithm", "Convolution"];
pub const IR_NAMES: [&str; 7] = [
    "Small Room",
    "Large Hall",
    "Plate",
    "Chamber",
    "Spring",
    "Cathedral",
    "Reverse",
];
pub const ALGORITHMS: [&str; 5] = ["Dark Hall", "Quartz", "Shimmer", "Tides", "Prism"];
const SYNC_DIVS: [&str; 8] = ["1/64", "1/32", "1/16T", "1/16", "1/8T", "1/8", "1/8D", "1/4"];
const SYNC_BEATS: [f64; 8] = [1.0 / 16.0, 1.0 / 8.0, 1.0 / 6.0, 0.25, 1.0 / 3.0, 0.5, 0.75, 1.0];

device_params! {
    pub struct HybridReverbParams {
        routing: "Routing", 0.0, 3.0, 1.0, ParamKind::Choice(&ROUTINGS), Unit::Plain;
        ir: "IR", 0.0, 6.0, 1.0, ParamKind::Choice(&IR_NAMES), Unit::Plain;
        ir_decay: "IR Decay", 0.05, 1.0, 1.0, LIN, Unit::Percent;
        ir_attack: "IR Attack", 0.0, 500.0, 0.0, LIN, Unit::Ms;
        predelay: "Pre-Delay", 0.0, 500.0, 10.0, LIN, Unit::Ms;
        sync: "Sync", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        sync_div: "Pre-Delay", 0.0, 7.0, 3.0, ParamKind::Choice(&SYNC_DIVS), Unit::Plain;
        algorithm: "Algorithm", 0.0, 4.0, 0.0, ParamKind::Choice(&ALGORITHMS), Unit::Plain;
        decay: "Decay", 0.2, 30.0, 3.0, LOG, Unit::Seconds;
        size: "Size", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        damping: "Damping", 0.0, 1.0, 0.4, LIN, Unit::Percent;
        diffusion: "Diffusion", 0.0, 1.0, 0.7, LIN, Unit::Percent;
        modulation: "Modulation", 0.0, 1.0, 0.25, LIN, Unit::Percent;
        shimmer: "Shimmer", 0.0, 1.0, 0.4, LIN, Unit::Percent;
        blend: "Blend", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        low_cut: "Low Cut", 20.0, 2000.0, 80.0, LOG, Unit::Hz;
        high_cut: "High Cut", 1000.0, 20000.0, 14000.0, LOG, Unit::Hz;
        width: "Width", 0.0, 2.0, 1.0, LIN, Unit::Percent;
        freeze: "Freeze", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        mix: "Dry/Wet", 0.0, 1.0, 0.35, LIN, Unit::Percent;
    }
}

/// Parameter indices.
pub mod idx {
    pub const ROUTING: usize = 0;
    pub const IR: usize = 1;
    pub const IR_DECAY: usize = 2;
    pub const IR_ATTACK: usize = 3;
    pub const PREDELAY: usize = 4;
    pub const SYNC: usize = 5;
    pub const SYNC_DIV: usize = 6;
    pub const ALGORITHM: usize = 7;
    pub const DECAY: usize = 8;
    pub const SIZE: usize = 9;
    pub const DAMPING: usize = 10;
    pub const DIFFUSION: usize = 11;
    pub const MODULATION: usize = 12;
    pub const SHIMMER: usize = 13;
    pub const BLEND: usize = 14;
    pub const LOW_CUT: usize = 15;
    pub const HIGH_CUT: usize = 16;
    pub const WIDTH: usize = 17;
    pub const FREEZE: usize = 18;
    pub const MIX: usize = 19;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    Serial,
    Parallel,
    Algorithm,
    Convolution,
}

impl HybridReverbParams {
    pub fn routing(&self) -> Routing {
        match self.routing.round() as i32 {
            0 => Routing::Serial,
            2 => Routing::Algorithm,
            3 => Routing::Convolution,
            _ => Routing::Parallel,
        }
    }

    pub fn ir_index(&self) -> usize {
        (self.ir.round().max(0.0) as usize).min(IR_NAMES.len() - 1)
    }

    fn algo_index(&self) -> usize {
        (self.algorithm.round().max(0.0) as usize).min(ALGORITHMS.len() - 1)
    }

    fn uses_conv(&self) -> bool {
        self.routing() != Routing::Algorithm
    }

    fn uses_algo(&self) -> bool {
        self.routing() != Routing::Convolution
    }
}

impl super::EffectParams for HybridReverbParams {
    fn is_relevant(&self, index: usize) -> bool {
        use idx::*;
        let sync = self.sync >= 0.5;
        match index {
            IR | IR_DECAY | IR_ATTACK => self.uses_conv(),
            ALGORITHM | DECAY | SIZE | DAMPING | DIFFUSION | MODULATION => self.uses_algo(),
            SHIMMER => self.uses_algo() && self.algo_index() == 2,
            BLEND => matches!(self.routing(), Routing::Serial | Routing::Parallel),
            PREDELAY => !sync,
            SYNC_DIV => sync,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Vocal Plate + Hall",
                Self {
                    routing: 1.0,
                    ir: 2.0,
                    algorithm: 0.0,
                    decay: 2.4,
                    predelay: 25.0,
                    blend: 0.4,
                    low_cut: 180.0,
                    high_cut: 11000.0,
                    mix: 0.3,
                    ..d
                },
            ),
            (
                "Shimmer Cathedral",
                Self {
                    routing: 0.0,
                    ir: 5.0,
                    algorithm: 2.0,
                    decay: 9.0,
                    size: 0.9,
                    shimmer: 0.6,
                    blend: 0.65,
                    predelay: 40.0,
                    low_cut: 200.0,
                    mix: 0.45,
                    ..d
                },
            ),
            (
                "Drum Room",
                Self {
                    routing: 3.0,
                    ir: 0.0,
                    ir_decay: 0.7,
                    predelay: 0.0,
                    low_cut: 120.0,
                    width: 1.2,
                    mix: 0.25,
                    ..d
                },
            ),
            (
                "Tidal Pad Wash",
                Self {
                    routing: 2.0,
                    algorithm: 3.0,
                    decay: 14.0,
                    size: 1.0,
                    modulation: 0.8,
                    damping: 0.55,
                    width: 1.5,
                    mix: 0.5,
                    ..d
                },
            ),
        ]
    }
}

// ---------------------------------------------------------------------------
// Impulse responses

/// Convolution block (partition) size for a sample rate: 256 at 44.1/48 kHz,
/// growing with the rate so the partition count (and CPU) stays similar.
pub fn block_size(sample_rate: f32) -> usize {
    ((256.0 * sample_rate / 48000.0).round().max(1.0) as usize)
        .next_power_of_two()
        .clamp(256, 1024)
}

#[derive(Clone, Copy)]
enum Shape {
    Space,
    Spring,
    Reverse,
}

struct IrSpec {
    len_s: f32,
    rt60: f32,
    /// Early reflections: count and time span.
    er_count: usize,
    er_ms: f32,
    /// Onset of the diffuse tail.
    tail_ms: f32,
    /// Low-pass cutoff at the start and end of the tail.
    lp_start: f32,
    lp_end: f32,
    hp: f32,
    shape: Shape,
}

const IR_SPECS: [IrSpec; 7] = [
    // Small Room
    IrSpec {
        len_s: 0.9,
        rt60: 0.55,
        er_count: 14,
        er_ms: 22.0,
        tail_ms: 4.0,
        lp_start: 10000.0,
        lp_end: 2500.0,
        hp: 90.0,
        shape: Shape::Space,
    },
    // Large Hall
    IrSpec {
        len_s: 3.6,
        rt60: 3.0,
        er_count: 18,
        er_ms: 80.0,
        tail_ms: 25.0,
        lp_start: 8000.0,
        lp_end: 1400.0,
        hp: 60.0,
        shape: Shape::Space,
    },
    // Plate
    IrSpec {
        len_s: 2.6,
        rt60: 2.2,
        er_count: 0,
        er_ms: 0.0,
        tail_ms: 0.5,
        lp_start: 15000.0,
        lp_end: 5000.0,
        hp: 180.0,
        shape: Shape::Space,
    },
    // Chamber
    IrSpec {
        len_s: 1.8,
        rt60: 1.4,
        er_count: 16,
        er_ms: 40.0,
        tail_ms: 10.0,
        lp_start: 7000.0,
        lp_end: 2200.0,
        hp: 80.0,
        shape: Shape::Space,
    },
    // Spring
    IrSpec {
        len_s: 2.2,
        rt60: 1.8,
        er_count: 0,
        er_ms: 0.0,
        tail_ms: 0.0,
        lp_start: 5000.0,
        lp_end: 2500.0,
        hp: 150.0,
        shape: Shape::Spring,
    },
    // Cathedral (a long tail truncated to the length cap with a fade)
    IrSpec {
        len_s: 4.0,
        rt60: 5.5,
        er_count: 10,
        er_ms: 120.0,
        tail_ms: 45.0,
        lp_start: 6000.0,
        lp_end: 900.0,
        hp: 45.0,
        shape: Shape::Space,
    },
    // Reverse
    IrSpec {
        len_s: 1.6,
        rt60: 1.6,
        er_count: 0,
        er_ms: 0.0,
        tail_ms: 0.0,
        lp_start: 9000.0,
        lp_end: 2500.0,
        hp: 100.0,
        shape: Shape::Reverse,
    },
];

/// Synthesises IR `choice` at `sample_rate` (left, right). UI thread.
pub fn generate_ir(choice: usize, sample_rate: f32) -> [Vec<f32>; 2] {
    let spec = &IR_SPECS[choice.min(IR_SPECS.len() - 1)];
    let sr = sample_rate.max(1000.0);
    let n = ((spec.len_s * sr) as usize).max(16);
    let mut out = [vec![0.0f32; n], vec![0.0f32; n]];
    for (ch, buf) in out.iter_mut().enumerate() {
        let mut rng = Rng::new(0x9e37_79b9 ^ ((choice as u32 + 1) * 7919) ^ ((ch as u32 + 1) * 104_729));
        match spec.shape {
            Shape::Space | Shape::Reverse => space_ir(spec, sr, ch, &mut rng, buf),
            Shape::Spring => spring_ir(spec, sr, ch, &mut rng, buf),
        }
        // High-pass (two one-pole stages) for the low end.
        let a = super::biquad::one_pole_coef(spec.hp, sr);
        for _ in 0..2 {
            let mut lp = OnePole::default();
            for v in buf.iter_mut() {
                *v -= lp.lowpass(*v, a);
            }
        }
        if let Shape::Reverse = spec.shape {
            buf.reverse();
        }
        // Fade out the end so the truncation does not click.
        let fade = match spec.shape {
            Shape::Reverse => (0.006 * sr) as usize,
            _ => n / 7,
        }
        .clamp(1, n);
        for i in 0..fade {
            let t = (i as f32 + 0.5) / fade as f32;
            buf[n - fade + i] *= 0.5 + 0.5 * (PI * t).cos();
        }
    }
    // Energy 0.25 per channel: white noise comes out 6 dB below the input.
    let energy: f64 = out
        .iter()
        .flat_map(|b| b.iter())
        .map(|&v| v as f64 * v as f64)
        .sum::<f64>()
        / 2.0;
    let g = if energy > 0.0 {
        (0.5 / energy.sqrt()) as f32
    } else {
        0.0
    };
    for b in &mut out {
        for v in b.iter_mut() {
            *v *= g;
        }
    }
    out
}

fn space_ir(spec: &IrSpec, sr: f32, ch: usize, rng: &mut Rng, buf: &mut [f32]) {
    let n = buf.len();
    let len_s = n as f32 / sr;
    let tail0 = spec.tail_ms * 0.001;
    let k = 6.9078 / spec.rt60;
    // Diffuse tail: noise under an exponential decay with a soft onset,
    // through a low-pass whose cutoff falls over time (highs decay faster).
    let mut lp = [OnePole::default(); 2];
    let onset = (tail0 * 1.5).max(0.002);
    for (i, v) in buf.iter_mut().enumerate() {
        let t = i as f32 / sr;
        if t < tail0 * 0.5 {
            continue;
        }
        let rise = ((t - tail0 * 0.5) / onset).min(1.0);
        let env = (-k * t).exp() * rise * rise;
        let frac = (t / len_s).min(1.0);
        let cutoff = spec.lp_start * (spec.lp_end / spec.lp_start).powf(frac);
        let c = super::biquad::one_pole_coef(cutoff, sr);
        let x = rng.next_bipolar() * env;
        let y = lp[0].lowpass(x, c);
        *v = lp[1].lowpass(y, c);
    }
    // Early reflections: sparse taps with decreasing level, different per
    // channel (the seed differs).
    if spec.er_count > 0 {
        let span = spec.er_ms * 0.001;
        let lp_c = super::biquad::one_pole_coef(spec.lp_start, sr);
        for r in 0..spec.er_count {
            let pos = (r as f32 + 0.5 + 0.45 * rng.next_bipolar()) / spec.er_count as f32;
            let t = 0.0015 + span * pos * pos.sqrt() + if ch == 1 { 0.0007 } else { 0.0 };
            let at = (t * sr) as usize;
            let amp = 0.9 / (1.0 + t * 60.0) * (0.6 + 0.4 * rng.next_bipolar().abs());
            let sign = if rng.next_bipolar() >= 0.0 { 1.0 } else { -1.0 };
            // A short smeared tap (a few samples of filtered energy).
            let mut f = OnePole::default();
            for j in 0..((0.0012 * sr) as usize).max(2) {
                if at + j < n {
                    let x = if j == 0 { amp * sign * 4.0 } else { 0.0 };
                    buf[at + j] += f.lowpass(x, lp_c) * 0.8;
                }
            }
        }
    }
}

fn spring_ir(spec: &IrSpec, sr: f32, ch: usize, rng: &mut Rng, buf: &mut [f32]) {
    // Repeating dispersive chirps (high frequencies arrive first), the
    // characteristic "boing" of a spring tank.
    let n = buf.len();
    let period = if ch == 0 { 0.0287 } else { 0.0331 };
    let chirp_len = 0.022;
    let k = 6.9078 / spec.rt60;
    let mut echo = 0;
    loop {
        let start = 0.003 + echo as f32 * period;
        if start >= n as f32 / sr {
            break;
        }
        let amp = (-k * start).exp() * if echo % 2 == 1 { -0.8 } else { 1.0 };
        let mut phase = rng.next_bipolar().abs();
        let len = (chirp_len * sr) as usize;
        let s0 = (start * sr) as usize;
        for j in 0..len {
            let i = s0 + j;
            if i >= n {
                break;
            }
            let u = j as f32 / len as f32;
            let f = 4200.0 * (350.0f32 / 4200.0).powf(u);
            phase += f / sr;
            let w = (PI * u).sin();
            buf[i] += amp * w * (phase * 2.0 * PI).sin() * 0.6;
        }
        echo += 1;
    }
    // A little diffuse noise in between, and a low-pass for the tank tone.
    let mut lp = OnePole::default();
    let c = super::biquad::one_pole_coef(spec.lp_start, sr);
    for (i, v) in buf.iter_mut().enumerate() {
        let t = i as f32 / sr;
        let x = *v + rng.next_bipolar() * 0.08 * (-k * t).exp();
        *v = lp.lowpass(x, c);
    }
}

/// One IR, partitioned and transformed.
struct IrSet {
    parts: usize,
    /// Spectra of the partitions, `parts * bins` per channel.
    h: [Vec<Complex>; 2],
    /// IR length in seconds.
    len_s: f32,
    /// Coarse amplitude envelope (0..1) for displays.
    envelope: Vec<f32>,
}

/// Every IR at one sample rate, shared by all instances at that rate.
pub struct IrBank {
    block: usize,
    bins: usize,
    max_parts: usize,
    irs: Vec<IrSet>,
}

const ENVELOPE_POINTS: usize = 96;

impl IrBank {
    fn build(sample_rate: f32) -> Self {
        let block = block_size(sample_rate);
        let fft = RealFft::new(2 * block);
        let bins = fft.bins();
        let mut frame = vec![0.0f32; 2 * block];
        let irs: Vec<IrSet> = (0..IR_NAMES.len())
            .map(|choice| {
                let ir = generate_ir(choice, sample_rate);
                let n = ir[0].len();
                let parts = n.div_ceil(block);
                let h = std::array::from_fn(|c| {
                    let mut spec = vec![Complex::ZERO; parts * bins];
                    for p in 0..parts {
                        frame.fill(0.0);
                        let seg = &ir[c][p * block..((p + 1) * block).min(n)];
                        frame[..seg.len()].copy_from_slice(seg);
                        fft.forward(&frame, &mut spec[p * bins..(p + 1) * bins]);
                    }
                    spec
                });
                let mut envelope: Vec<f32> = (0..ENVELOPE_POINTS)
                    .map(|i| {
                        let a = i * n / ENVELOPE_POINTS;
                        let b = ((i + 1) * n / ENVELOPE_POINTS).max(a + 1).min(n);
                        let e: f32 = (a..b).map(|j| ir[0][j] * ir[0][j] + ir[1][j] * ir[1][j]).sum();
                        (e / (b - a) as f32).sqrt()
                    })
                    .collect();
                let peak = envelope.iter().copied().fold(1e-12, f32::max);
                for v in &mut envelope {
                    *v /= peak;
                }
                IrSet {
                    parts,
                    h,
                    len_s: n as f32 / sample_rate,
                    envelope,
                }
            })
            .collect();
        let max_parts = irs.iter().map(|s| s.parts).max().unwrap_or(1);
        Self {
            block,
            bins,
            max_parts,
            irs,
        }
    }

    /// Memory used by the IR spectra in bytes.
    pub fn bytes(&self) -> usize {
        self.irs
            .iter()
            .map(|s| s.h[0].len() * 2 * std::mem::size_of::<Complex>())
            .sum()
    }
}

fn banks() -> &'static Mutex<HashMap<u32, Weak<IrBank>>> {
    static BANKS: OnceLock<Mutex<HashMap<u32, Weak<IrBank>>>> = OnceLock::new();
    BANKS.get_or_init(Default::default)
}

/// The IR bank for a sample rate, built if no instance holds one. UI thread.
fn bank(sample_rate: f32) -> Arc<IrBank> {
    let mut map = banks().lock().unwrap_or_else(|e| e.into_inner());
    let key = sample_rate.to_bits();
    if let Some(b) = map.get(&key).and_then(Weak::upgrade) {
        return b;
    }
    map.retain(|_, w| w.strong_count() > 0);
    let b = Arc::new(IrBank::build(sample_rate));
    map.insert(key, Arc::downgrade(&b));
    b
}

/// Display envelope (0..1, oldest first) and length in seconds of an IR, if
/// an instance at `sample_rate` is alive. Never builds the bank. UI thread.
pub fn ir_envelope(sample_rate: f32, ir: usize) -> Option<(Vec<f32>, f32)> {
    let map = banks().lock().unwrap_or_else(|e| e.into_inner());
    let b = map.get(&sample_rate.to_bits()).and_then(Weak::upgrade)?;
    let s = b.irs.get(ir)?;
    Some((s.envelope.clone(), s.len_s))
}

/// Gain of the IR at time `t` (seconds) given the decay and attack
/// parameters, for an IR of length `len_s`.
pub fn ir_gain(t: f32, len_s: f32, decay: f32, attack_ms: f32) -> f32 {
    let d = decay.clamp(0.05, 1.0);
    let mut g = if d < 0.999 {
        (-6.9078 * t * (1.0 / (d * len_s) - 1.0 / len_s)).exp()
    } else {
        1.0
    };
    if attack_ms > 0.5 {
        let a = (t / (attack_ms * 0.001)).min(1.0);
        g *= a * a * (3.0 - 2.0 * a);
    }
    g
}

// ---------------------------------------------------------------------------
// Convolver

struct Convolver {
    bank: Arc<IrBank>,
    fft: RealFft,
    block: usize,
    bins: usize,
    parts: usize,
    /// Input frames (previous block followed by the current one).
    frame: [Vec<f32>; 2],
    /// Frequency-domain delay line of input block spectra.
    fdl: [Vec<Complex>; 2],
    head: usize,
    acc: Vec<Complex>,
    time: Vec<f32>,
    /// Output of the last block.
    out: [Vec<f32>; 2],
    /// Old-IR output during a crossfade.
    old: Vec<f32>,
    fill: usize,
    gains: Vec<f32>,
    gain_key: (usize, f32, f32),
    active: usize,
    ir: usize,
    prev_ir: Option<usize>,
    prev_active: usize,
}

impl Convolver {
    fn new(sample_rate: f32) -> Self {
        let bank = bank(sample_rate);
        let block = bank.block;
        let bins = bank.bins;
        let parts = bank.max_parts;
        Self {
            fft: RealFft::new(2 * block),
            block,
            bins,
            parts,
            frame: [vec![0.0; 2 * block], vec![0.0; 2 * block]],
            fdl: [vec![Complex::ZERO; parts * bins], vec![Complex::ZERO; parts * bins]],
            head: 0,
            acc: vec![Complex::ZERO; bins],
            time: vec![0.0; 2 * block],
            out: [vec![0.0; block], vec![0.0; block]],
            old: vec![0.0; block],
            fill: 0,
            gains: vec![1.0; parts],
            gain_key: (usize::MAX, -1.0, -1.0),
            active: 0,
            ir: 1,
            prev_ir: None,
            prev_active: 0,
            bank,
        }
    }

    fn reset(&mut self) {
        for c in 0..2 {
            self.frame[c].fill(0.0);
            self.fdl[c].fill(Complex::ZERO);
            self.out[c].fill(0.0);
        }
        self.fill = 0;
        self.head = 0;
        self.prev_ir = None;
    }

    /// Updates the IR choice and envelope (once per block or call).
    fn configure(&mut self, ir: usize, decay: f32, attack: f32, sr: f32) {
        let ir = ir.min(self.bank.irs.len() - 1);
        if ir != self.ir {
            if self.prev_ir.is_none() {
                self.prev_ir = Some(self.ir);
                self.prev_active = self.active;
            }
            self.ir = ir;
        }
        let key = (ir, decay, attack);
        if key != self.gain_key {
            self.gain_key = key;
            let set = &self.bank.irs[ir];
            let bt = self.block as f32 / sr;
            let mut active = 0;
            for k in 0..self.parts {
                let g = if k < set.parts {
                    ir_gain((k as f32 + 0.5) * bt, set.len_s, decay, attack)
                } else {
                    0.0
                };
                self.gains[k] = g;
                if g > 1e-4 {
                    active = k + 1;
                }
            }
            self.active = active;
        }
    }

    /// Feeds one stereo sample and returns one output sample (delayed by one
    /// block).
    #[inline]
    fn tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let b = self.block;
        self.frame[0][b + self.fill] = l;
        self.frame[1][b + self.fill] = r;
        let y = (self.out[0][self.fill], self.out[1][self.fill]);
        self.fill += 1;
        if self.fill == b {
            self.fill = 0;
            self.run_block();
        }
        y
    }

    fn accumulate(&mut self, c: usize, ir: usize, active: usize) {
        let bins = self.bins;
        let set = &self.bank.irs[ir];
        let h = &set.h[c];
        self.acc.fill(Complex::ZERO);
        let n = active.min(set.parts).min(self.parts);
        for k in 0..n {
            let g = self.gains[k];
            if g <= 1e-4 {
                continue;
            }
            let slot = (self.head + self.parts - k) % self.parts;
            let x = &self.fdl[c][slot * bins..(slot + 1) * bins];
            let hk = &h[k * bins..(k + 1) * bins];
            for ((a, x), h) in self.acc.iter_mut().zip(x).zip(hk) {
                a.re += g * (x.re * h.re - x.im * h.im);
                a.im += g * (x.re * h.im + x.im * h.re);
            }
        }
        self.fft.inverse(&mut self.acc, &mut self.time);
    }

    fn run_block(&mut self) {
        let b = self.block;
        let bins = self.bins;
        for c in 0..2 {
            let slot = self.head;
            self.fft
                .forward(&self.frame[c], &mut self.fdl[c][slot * bins..(slot + 1) * bins]);
            if let Some(prev) = self.prev_ir {
                self.accumulate(c, prev, self.prev_active);
                self.old.copy_from_slice(&self.time[b..]);
            }
            self.accumulate(c, self.ir, self.active);
            let fading = self.prev_ir.is_some();
            for i in 0..b {
                let y = self.time[b + i];
                self.out[c][i] = if fading {
                    let f = (i as f32 + 1.0) / b as f32;
                    self.old[i] + (y - self.old[i]) * f
                } else {
                    y
                };
            }
            self.frame[c].copy_within(b.., 0);
        }
        self.prev_ir = None;
        self.head = (self.head + 1) % self.parts;
    }
}

// ---------------------------------------------------------------------------
// Algorithmic reverb

const LINES: usize = 8;
const LINE_MS: [f32; LINES] = [31.3, 37.9, 43.1, 50.3, 57.7, 64.9, 71.3, 79.7];
const MAX_SIZE: f32 = 1.7;
const MAX_MOD_MS: f32 = 4.0;
const DIFF_MS: [f32; 4] = [3.1, 4.7, 7.3, 11.9];
const SHIFT_MS: f32 = 60.0;

struct Algo {
    size_mul: f32,
    damp_lo: f32,
    damp_hi: f32,
    diff_mul: f32,
    mod_ms: f32,
    mod_rate: f32,
    shimmer: bool,
}

const ALGOS: [Algo; 5] = [
    // Dark Hall
    Algo {
        size_mul: 1.3,
        damp_lo: 500.0,
        damp_hi: 7000.0,
        diff_mul: 1.0,
        mod_ms: 0.6,
        mod_rate: 0.35,
        shimmer: false,
    },
    // Quartz: sparse and clear
    Algo {
        size_mul: 0.9,
        damp_lo: 2500.0,
        damp_hi: 18000.0,
        diff_mul: 0.45,
        mod_ms: 0.15,
        mod_rate: 0.2,
        shimmer: false,
    },
    // Shimmer
    Algo {
        size_mul: 1.2,
        damp_lo: 1500.0,
        damp_hi: 12000.0,
        diff_mul: 1.0,
        mod_ms: 0.4,
        mod_rate: 0.3,
        shimmer: true,
    },
    // Tides: deep slow modulation
    Algo {
        size_mul: 1.0,
        damp_lo: 1200.0,
        damp_hi: 10000.0,
        diff_mul: 0.9,
        mod_ms: 3.2,
        mod_rate: 0.12,
        shimmer: false,
    },
    // Prism: bright
    Algo {
        size_mul: 0.8,
        damp_lo: 5000.0,
        damp_hi: 20000.0,
        diff_mul: 0.8,
        mod_ms: 0.3,
        mod_rate: 0.5,
        shimmer: false,
    },
];

struct Delay {
    buf: Vec<f32>,
    pos: usize,
}

impl Delay {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(4)],
            pos: 0,
        }
    }

    #[inline]
    fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
    }

    /// Sample `d` (>= 1) samples ago, linearly interpolated.
    #[inline]
    fn read_frac(&self, d: f32) -> f32 {
        let n = self.buf.len();
        let d = d.clamp(1.0, (n - 2) as f32);
        let i = d as usize;
        let f = d - i as f32;
        let a = self.buf[(self.pos + n - i) % n];
        let b = self.buf[(self.pos + n - i - 1) % n];
        a + (b - a) * f
    }

    #[inline]
    fn read(&self, d: usize) -> f32 {
        let n = self.buf.len();
        self.buf[(self.pos + n - d.clamp(1, n)) % n]
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// Schroeder allpass on a delay line.
#[inline]
fn allpass(d: &mut Delay, len: usize, g: f32, x: f32) -> f32 {
    let z = d.read(len);
    let v = x + g * z;
    d.write(v);
    z - g * v
}

/// In-place 8-point fast Walsh-Hadamard transform, normalised.
#[inline]
fn hadamard(v: &mut [f32; LINES]) {
    let mut h = 1;
    while h < LINES {
        for i in (0..LINES).step_by(h * 2) {
            for j in i..i + h {
                let (a, b) = (v[j], v[j + h]);
                v[j] = a + b;
                v[j + h] = a - b;
            }
        }
        h *= 2;
    }
    let s = 1.0 / (LINES as f32).sqrt();
    for x in v.iter_mut() {
        *x *= s;
    }
}

struct Fdn {
    sr: f32,
    lines: [Delay; LINES],
    damp: [OnePole; LINES],
    diff: [[Delay; 4]; 2],
    diff_len: [[usize; 4]; 2],
    phase: [f32; LINES],
    size: f32,
    shift: Delay,
    shift_phase: f32,
    shift_hp: OnePole,
    shimmer_fb: f32,
}

impl Fdn {
    fn new(sr: f32) -> Self {
        let max_line = ((LINE_MS[LINES - 1] * MAX_SIZE * 1.3 + MAX_MOD_MS * 2.0) * 0.001 * sr) as usize + 8;
        let diff_len: [[usize; 4]; 2] = std::array::from_fn(|c| {
            std::array::from_fn(|i| (DIFF_MS[i] * (1.0 + 0.09 * c as f32) * 0.001 * sr) as usize + 1)
        });
        Self {
            sr,
            lines: std::array::from_fn(|_| Delay::new(max_line)),
            damp: [OnePole::default(); LINES],
            diff: std::array::from_fn(|c| std::array::from_fn(|i| Delay::new(diff_len[c][i] + 2))),
            diff_len,
            phase: std::array::from_fn(|i| i as f32 / LINES as f32),
            size: -1.0,
            shift: Delay::new((SHIFT_MS * 0.001 * sr) as usize + 8),
            shift_phase: 0.0,
            shift_hp: OnePole::default(),
            shimmer_fb: 0.0,
        }
    }

    fn reset(&mut self) {
        for l in &mut self.lines {
            l.clear();
        }
        for d in &mut self.damp {
            d.reset();
        }
        for ch in &mut self.diff {
            for d in ch {
                d.clear();
            }
        }
        self.shift.clear();
        self.shift_hp.reset();
        self.shimmer_fb = 0.0;
    }

    /// Processes `n` samples from `inp` into `out` (both stereo).
    #[allow(clippy::too_many_arguments)]
    fn process(&mut self, p: &HybridReverbParams, inl: &[f32], inr: &[f32], outl: &mut [f32], outr: &mut [f32]) {
        let sr = self.sr;
        let algo = &ALGOS[p.algo_index()];
        let freeze = p.freeze >= 0.5;
        let target_size = (0.25 + 1.0 * p.size.clamp(0.0, 1.0)) * algo.size_mul;
        if self.size < 0.0 {
            self.size = target_size;
        }
        let size_k = smoothing(0.15, sr);
        let cutoff = algo.damp_hi * (algo.damp_lo / algo.damp_hi).powf(p.damping.clamp(0.0, 1.0));
        let damp_c = if freeze {
            0.0
        } else {
            super::biquad::one_pole_coef(cutoff, sr)
        };
        let g_diff = (0.72 * p.diffusion.clamp(0.0, 1.0) * algo.diff_mul).min(0.75);
        let rt60 = p.decay.clamp(0.05, 60.0);
        let mod_depth = algo.mod_ms * 0.001 * sr * p.modulation.clamp(0.0, 1.0);
        let base_inc = algo.mod_rate / sr;
        let shimmer = if algo.shimmer && !freeze {
            p.shimmer.clamp(0.0, 1.0) * 0.55
        } else {
            0.0
        };
        let shift_len = (SHIFT_MS * 0.001 * sr).min((self.shift.buf.len() - 4) as f32);
        let shift_hp_c = super::biquad::one_pole_coef(300.0, sr);
        // Quieter for longer decays so the level stays comparable.
        let out_gain = 1.4 / (1.0 + 0.35 * rt60).sqrt();
        let in_gain = if freeze { 0.0 } else { 1.0 };

        let n = inl.len().min(inr.len()).min(outl.len()).min(outr.len());
        let mut start = 0;
        while start < n {
            let end = (start + 32).min(n);
            self.size += (target_size - self.size) * (1.0 - size_k.powi((end - start) as i32));
            let mut delays = [0.0f32; LINES];
            let mut gains = [1.0f32; LINES];
            for i in 0..LINES {
                delays[i] = LINE_MS[i] * self.size * 0.001 * sr;
                gains[i] = if freeze {
                    1.0
                } else {
                    10f32.powf(-3.0 * (delays[i] / sr) / rt60)
                };
            }
            for s in start..end {
                // Input diffusion.
                let mut x = [inl[s] * in_gain, inr[s] * in_gain];
                for c in 0..2 {
                    for k in 0..4 {
                        x[c] = allpass(&mut self.diff[c][k], self.diff_len[c][k], g_diff, x[c]);
                    }
                }
                let mut y = [0.0f32; LINES];
                for i in 0..LINES {
                    let lfo = (self.phase[i] * 2.0 * PI).sin();
                    let d = delays[i] + mod_depth * (1.0 + lfo) + 1.0;
                    let v = self.lines[i].read_frac(d);
                    y[i] = self.damp[i].lowpass(v, damp_c) * gains[i];
                    self.phase[i] += base_inc * (1.0 + 0.13 * i as f32);
                    if self.phase[i] >= 1.0 {
                        self.phase[i] -= 1.0;
                    }
                }
                let ol = y[0] - y[2] + y[4] - y[6] + 0.5 * (y[1] - y[7]);
                let or = y[1] - y[3] + y[5] - y[7] + 0.5 * (y[0] - y[6]);
                let mut m = y;
                hadamard(&mut m);
                // Shimmer: octave-up pitch shift of the tail fed back in.
                let mut sh = 0.0;
                if shimmer > 0.0 {
                    self.shift.write(0.5 * (ol + or));
                    // Two taps sweeping the delay down by one sample per
                    // sample (ratio 2) with complementary sine windows.
                    let ph = self.shift_phase;
                    let ph2 = (ph + 0.5).fract();
                    let a = self.shift.read_frac(1.0 + (1.0 - ph) * shift_len);
                    let b = self.shift.read_frac(1.0 + (1.0 - ph2) * shift_len);
                    let v = a * (PI * ph).sin() + b * (PI * ph2).sin();
                    self.shift_phase += 1.0 / shift_len;
                    if self.shift_phase >= 1.0 {
                        self.shift_phase -= 1.0;
                    }
                    let v = v - self.shift_hp.lowpass(v, shift_hp_c);
                    self.shimmer_fb = v * shimmer;
                    sh = self.shimmer_fb;
                }
                for i in 0..LINES {
                    let inj = if i % 2 == 0 { x[0] } else { x[1] };
                    let w = m[i] + inj * 0.5 + sh * if i < 4 { 0.5 } else { -0.5 };
                    self.lines[i].write(w.clamp(-8.0, 8.0));
                }
                outl[s] = ol * out_gain;
                outr[s] = or * out_gain;
            }
            start = end;
        }
    }
}

// ---------------------------------------------------------------------------
// Device

const MAX_PREDELAY_S: f32 = 2.0;

pub struct HybridReverb {
    sr: f32,
    conv: Convolver,
    fdn: Fdn,
    pre: [Delay; 2],
    pre_d: f32,
    conv_on: bool,
    algo_on: bool,
    /// Scratch for one chunk.
    conv_out: [Vec<f32>; 2],
    algo_in: [Vec<f32>; 2],
    algo_out: [Vec<f32>; 2],
    low: [Biquad; 2],
    high: [Biquad; 2],
    low_c: Coefs,
    high_c: Coefs,
    eq_key: (f32, f32),
    mix: f32,
    blend: f32,
    width: f32,
    k: f32,
}

const CHUNK: usize = 64;

impl HybridReverb {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate;
        let conv = Convolver::new(sr);
        let pre_len = (MAX_PREDELAY_S * sr) as usize + conv.block + 8;
        Self {
            sr,
            conv,
            fdn: Fdn::new(sr),
            pre: [Delay::new(pre_len), Delay::new(pre_len)],
            pre_d: -1.0,
            conv_on: false,
            algo_on: false,
            conv_out: [vec![0.0; CHUNK], vec![0.0; CHUNK]],
            algo_in: [vec![0.0; CHUNK], vec![0.0; CHUNK]],
            algo_out: [vec![0.0; CHUNK], vec![0.0; CHUNK]],
            low: [Biquad::default(); 2],
            high: [Biquad::default(); 2],
            low_c: Coefs::IDENTITY,
            high_c: Coefs::IDENTITY,
            eq_key: (-1.0, -1.0),
            mix: -1.0,
            blend: -1.0,
            width: -1.0,
            k: smoothing(0.02, sr),
        }
    }

    pub fn reset(&mut self) {
        self.conv.reset();
        self.fdn.reset();
        for d in &mut self.pre {
            d.clear();
        }
        for f in self.low.iter_mut().chain(self.high.iter_mut()) {
            f.reset();
        }
    }

    /// Latency of the convolution path in samples (compensated by the
    /// pre-delay when it is at least this long).
    pub fn conv_latency(&self) -> usize {
        self.conv.block
    }

    fn predelay_samples(&self, p: &HybridReverbParams, ctx: &FxContext) -> f32 {
        let s = if p.sync >= 0.5 {
            let i = (p.sync_div.round().max(0.0) as usize).min(SYNC_BEATS.len() - 1);
            let tempo = if ctx.tempo > 1.0 { ctx.tempo } else { 120.0 };
            (SYNC_BEATS[i] * 60.0 / tempo) as f32
        } else {
            p.predelay.max(0.0) * 0.001
        };
        (s.min(MAX_PREDELAY_S) * self.sr).max(0.0)
    }

    pub fn process(&mut self, p: &HybridReverbParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let sr = self.sr;
        let routing = p.routing();
        let use_conv = p.uses_conv();
        let use_algo = p.uses_algo();
        if use_conv && !self.conv_on {
            self.conv.reset();
        }
        if use_algo && !self.algo_on {
            self.fdn.reset();
        }
        self.conv_on = use_conv;
        self.algo_on = use_algo;
        if use_conv {
            self.conv.configure(
                p.ir_index(),
                p.ir_decay.clamp(0.05, 1.0),
                p.ir_attack.clamp(0.0, 500.0),
                sr,
            );
        }
        let key = (p.low_cut.clamp(20.0, 2000.0), p.high_cut.clamp(1000.0, 20000.0));
        if key != self.eq_key {
            self.eq_key = key;
            self.low_c = Coefs::design(FilterShape::HighPass, key.0, 0.707, 0.0, sr);
            self.high_c = Coefs::design(FilterShape::LowPass, key.1, 0.707, 0.0, sr);
        }
        let target_pre = self.predelay_samples(p, ctx);
        if self.pre_d < 0.0 {
            self.pre_d = target_pre;
        }
        let (t_mix, t_blend, t_width) = (p.mix.clamp(0.0, 1.0), p.blend.clamp(0.0, 1.0), p.width.clamp(0.0, 2.0));
        if self.mix < 0.0 {
            self.mix = t_mix;
            self.blend = t_blend;
            self.width = t_width;
        }
        let freeze = p.freeze >= 0.5;
        let lat = self.conv.block as f32;
        let k = self.k;
        let pre_k = smoothing(0.05, sr);

        let n = left.len().min(right.len());
        let mut start = 0;
        while start < n {
            let end = (start + CHUNK).min(n);
            let len = end - start;
            // Pre-delay and convolution, sample by sample.
            for j in 0..len {
                let s = start + j;
                self.pre_d = target_pre + (self.pre_d - target_pre) * pre_k;
                let (dl, dr) = (left[s], right[s]);
                self.pre[0].write(dl);
                self.pre[1].write(dr);
                let d_algo = self.pre_d;
                let al = if d_algo < 1.0 {
                    dl
                } else {
                    self.pre[0].read_frac(d_algo)
                };
                let ar = if d_algo < 1.0 {
                    dr
                } else {
                    self.pre[1].read_frac(d_algo)
                };
                let (cl, cr) = if use_conv {
                    let d_conv = d_algo - lat;
                    let (il, ir) = if d_conv < 1.0 {
                        (dl, dr)
                    } else {
                        (self.pre[0].read_frac(d_conv), self.pre[1].read_frac(d_conv))
                    };
                    let g = if freeze { 0.0 } else { 1.0 };
                    self.conv.tick(il * g, ir * g)
                } else {
                    (0.0, 0.0)
                };
                self.conv_out[0][j] = cl;
                self.conv_out[1][j] = cr;
                let serial = routing == Routing::Serial;
                self.algo_in[0][j] = if serial { cl } else { al };
                self.algo_in[1][j] = if serial { cr } else { ar };
            }
            if use_algo {
                let [ol, or] = &mut self.algo_out;
                self.fdn.process(
                    p,
                    &self.algo_in[0][..len],
                    &self.algo_in[1][..len],
                    &mut ol[..len],
                    &mut or[..len],
                );
            } else {
                self.algo_out[0][..len].fill(0.0);
                self.algo_out[1][..len].fill(0.0);
            }
            for j in 0..len {
                let s = start + j;
                self.mix = t_mix + (self.mix - t_mix) * k;
                self.blend = t_blend + (self.blend - t_blend) * k;
                self.width = t_width + (self.width - t_width) * k;
                let (gc, ga) = match routing {
                    Routing::Algorithm => (0.0, 1.0),
                    Routing::Convolution => (1.0, 0.0),
                    _ => ((self.blend * FRAC_PI_2).cos(), (self.blend * FRAC_PI_2).sin()),
                };
                let mut wl = gc * self.conv_out[0][j] + ga * self.algo_out[0][j];
                let mut wr = gc * self.conv_out[1][j] + ga * self.algo_out[1][j];
                wl = self.high[0].process(&self.high_c, self.low[0].process(&self.low_c, wl));
                wr = self.high[1].process(&self.high_c, self.low[1].process(&self.low_c, wr));
                let mid = 0.5 * (wl + wr);
                let side = 0.5 * (wl - wr) * self.width;
                wl = (mid + side).clamp(-8.0, 8.0);
                wr = (mid - side).clamp(-8.0, 8.0);
                if !wl.is_finite() || !wr.is_finite() {
                    wl = 0.0;
                    wr = 0.0;
                }
                let dry = 1.0 - self.mix;
                left[s] = left[s] * dry + wl * self.mix;
                right[s] = right[s] * dry + wr * self.mix;
            }
            start = end;
        }
    }
}

impl super::EffectRuntime for HybridReverb {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const SR: f32 = 48000.0;

    fn run(p: &HybridReverbParams, input: &[f32], block: usize) -> (Vec<f32>, Vec<f32>) {
        let mut rv = HybridReverb::new(SR);
        let ctx = FxContext::new(SR, 120.0);
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        for (a, b) in l.chunks_mut(block).zip(r.chunks_mut(block)) {
            rv.process(p, a, b, &ctx);
        }
        (l, r)
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    fn impulse(n: usize) -> Vec<f32> {
        let mut x = vec![0.0; n];
        x[0] = 1.0;
        x
    }

    #[test]
    fn convolution_matches_direct_convolution() {
        // Compare the partitioned convolver with a direct convolution of the
        // generated IR (wet only, no EQ effect to speak of).
        let mut c = Convolver::new(SR);
        c.configure(0, 1.0, 0.0, SR);
        let ir = generate_ir(0, SR);
        let mut rng = Rng::new(3);
        let x: Vec<f32> = (0..3000).map(|_| rng.next_bipolar()).collect();
        let lat = c.block;
        let mut y = Vec::new();
        for i in 0..(3000 + lat) {
            let v = x.get(i).copied().unwrap_or(0.0);
            y.push(c.tick(v, v).0);
        }
        for t in [0usize, 10, 255, 256, 700, 2999] {
            let want: f32 = (0..=t).map(|j| x[j] * ir[0].get(t - j).copied().unwrap_or(0.0)).sum();
            let got = y[t + lat];
            assert!((want - got).abs() < 1e-3, "t={t}: {want} vs {got}");
        }
    }

    #[test]
    fn convolution_tail_has_ir_length() {
        let p = HybridReverbParams {
            routing: 3.0,
            ir: 0.0,
            predelay: 0.0,
            mix: 1.0,
            low_cut: 20.0,
            high_cut: 20000.0,
            ..Default::default()
        };
        let (l, r) = run(&p, &impulse(72000), 128);
        let len_s = IR_SPECS[0].len_s;
        let a = (0.05 * SR) as usize;
        assert!(rms(&l[a..a + 4800]) > 1e-3, "tail present");
        assert!(rms(&r[a..a + 4800]) > 1e-3);
        // Nothing after the IR (plus block latency and filter ring).
        let after = ((len_s + 0.03) * SR) as usize;
        let late = l[after..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(late < 1e-4, "late {late}");
        // Decorrelated channels.
        let diff: f32 = l.iter().zip(&r).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 0.1);
    }

    #[test]
    fn ir_decay_shortens_the_tail() {
        let mut p = HybridReverbParams {
            routing: 3.0,
            ir: 1.0,
            predelay: 0.0,
            mix: 1.0,
            ..Default::default()
        };
        let (full, _) = run(&p, &impulse(SR as usize * 3), 256);
        p.ir_decay = 0.2;
        let (short, _) = run(&p, &impulse(SR as usize * 3), 256);
        let w = (1.5 * SR) as usize..(2.0 * SR) as usize;
        assert!(rms(&short[w.clone()]) < rms(&full[w]) * 0.05);
    }

    #[test]
    fn algorithm_decays_by_rt60() {
        let p = HybridReverbParams {
            routing: 2.0,
            decay: 1.0,
            predelay: 0.0,
            mix: 1.0,
            modulation: 0.0,
            ..Default::default()
        };
        let (l, _) = run(&p, &impulse(SR as usize * 2), 512);
        let early = rms(&l[(0.1 * SR) as usize..(0.2 * SR) as usize]);
        let late = rms(&l[(1.1 * SR) as usize..(1.2 * SR) as usize]);
        let db = 20.0 * (early / late.max(1e-12)).log10();
        assert!((45.0..80.0).contains(&db), "decay over 1 s: {db} dB");
        assert!(early > 1e-4);
    }

    #[test]
    fn predelay_delays_the_onset() {
        for routing in [2.0, 3.0] {
            let p = HybridReverbParams {
                routing,
                predelay: 100.0,
                mix: 1.0,
                ir: 2.0,
                ..Default::default()
            };
            let (l, _) = run(&p, &impulse(12000), 64);
            let before = l[..(0.09 * SR) as usize].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            let after = l[(0.1 * SR) as usize..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
            assert!(before < 1e-6 && after > 1e-4, "routing {routing}: {before} {after}");
        }
    }

    #[test]
    fn freeze_sustains_and_all_modes_stay_bounded() {
        let mut rng = Rng::new(11);
        let noise: Vec<f32> = (0..24000).map(|_| rng.next_bipolar() * 0.5).collect();
        for routing in 0..4 {
            for algo in 0..5 {
                let p = HybridReverbParams {
                    routing: routing as f32,
                    algorithm: algo as f32,
                    ir: ((routing + algo) % 7) as f32,
                    decay: 30.0,
                    size: 1.0,
                    diffusion: 1.0,
                    modulation: 1.0,
                    shimmer: 1.0,
                    width: 2.0,
                    mix: 1.0,
                    ..Default::default()
                };
                let (l, r) = run(&p, &noise, 1000);
                assert!(l.iter().chain(&r).all(|v| v.is_finite() && v.abs() <= 8.0));
            }
        }
        // Freeze: feed noise, then freeze and silence; the tail holds.
        let mut rv = HybridReverb::new(SR);
        let ctx = FxContext::new(SR, 120.0);
        let mut p = HybridReverbParams {
            routing: 2.0,
            decay: 1.0,
            mix: 1.0,
            ..Default::default()
        };
        let mut l = noise.clone();
        let mut r = noise.clone();
        rv.process(&p, &mut l, &mut r, &ctx);
        p.freeze = 1.0;
        let mut l = vec![0.0; 96000];
        let mut r = vec![0.0; 96000];
        for (a, b) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            rv.process(&p, a, b, &ctx);
        }
        let early = rms(&l[4800..14400]);
        let late = rms(&l[86400..]);
        assert!(late > early * 0.5, "frozen tail {early} -> {late}");
    }

    #[test]
    fn ir_switch_and_tiny_blocks_are_safe() {
        let mut rv = HybridReverb::new(SR);
        let ctx = FxContext::new(SR, 120.0);
        let mut p = HybridReverbParams {
            mix: 1.0,
            ..Default::default()
        };
        let mut rng = Rng::new(2);
        for i in 0..4000 {
            if i % 700 == 0 {
                p.ir = ((i / 700) % 7) as f32;
            }
            let mut l = [rng.next_bipolar() * 0.3];
            let mut r = [rng.next_bipolar() * 0.3];
            rv.process(&p, &mut l, &mut r, &ctx);
            assert!(l[0].is_finite() && r[0].is_finite());
        }
        rv.reset();
        let mut l = [0.0f32; 300];
        let mut r = [0.0f32; 300];
        rv.process(&p, &mut l, &mut r, &ctx);
        assert!(l.iter().all(|v| v.abs() < 1e-6), "reset clears tails");
    }

    #[test]
    fn levels_are_comparable_between_paths() {
        let mut rng = Rng::new(5);
        let noise: Vec<f32> = (0..96000).map(|_| rng.next_bipolar() * 0.3).collect();
        let base = HybridReverbParams {
            mix: 1.0,
            ..Default::default()
        };
        let conv = HybridReverbParams { routing: 3.0, ..base };
        let algo = HybridReverbParams { routing: 2.0, ..base };
        let (c, _) = run(&conv, &noise, 512);
        let (a, _) = run(&algo, &noise, 512);
        let (rc, ra) = (rms(&c[48000..]), rms(&a[48000..]));
        let input = rms(&noise);
        eprintln!("conv {rc} algo {ra} input {input}");
        let db = 20.0 * (rc / ra).log10();
        assert!(db.abs() < 6.0, "conv vs algo {db} dB");
        assert!(rc < input * 3.0 && ra < input * 3.0);
    }

    #[test]
    fn memory_and_cpu_are_reasonable() {
        let b = bank(SR);
        let mb = b.bytes() as f32 / 1e6;
        assert!(mb < 100.0, "IR bank {mb} MB");
        let p = HybridReverbParams {
            routing: 1.0,
            ir: 5.0,
            algorithm: 2.0,
            ..Default::default()
        };
        let mut rv = HybridReverb::new(SR);
        let ctx = FxContext::new(SR, 120.0);
        let mut rng = Rng::new(1);
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        let t = Instant::now();
        for _ in 0..(10 * 48000 / 512) {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                *a = rng.next_bipolar() * 0.2;
                *b = *a;
            }
            rv.process(&p, &mut l, &mut r, &ctx);
        }
        let secs = t.elapsed().as_secs_f32();
        eprintln!("IR bank {mb:.1} MB, 10 s of Cathedral+Shimmer in {secs:.2} s");
        assert!(secs < 30.0, "too slow: {secs} s for 10 s of audio");
    }
}
