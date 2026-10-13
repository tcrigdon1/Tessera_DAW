//! Beat Repeat: tempo-synced stutters.
//!
//! Once per *interval* (offset by a number of sixteenths) the device rolls
//! its *chance*; on success it starts capturing the incoming audio. The first
//! *grid* length passes through untouched while it is recorded, then that
//! slice repeats until the *gate* runs out. Repeats can be pitched down by
//! resampling (with a further drop per repeat), band-pass filtered and faded
//! out repeat by repeat. *Variation* picks a random subdivision of the grid,
//! either once per trigger or continuously every 1/4, 1/8 or 1/16.
//!
//! Triggers follow the song position (`ctx.song_beat`), so the stutters lock
//! to the bar; while the transport is stopped the device passes audio
//! through. The *Repeat* toggle forces repeating for as long as it is on.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::dsp::Rng;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);
const INTERVALS: ParamKind = ParamKind::Choice(&["1/32", "1/16", "1/8", "1/4", "1/2", "1 Bar", "2 Bars", "4 Bars"]);
const GRIDS: ParamKind = ParamKind::Choice(&["1/4", "1/8", "1/16", "1/32"]);
const VAR_MODES: ParamKind = ParamKind::Choice(&["Trigger", "1/4", "1/8", "1/16"]);
const MIX_MODES: ParamKind = ParamKind::Choice(&["Mix", "Insert", "Gate"]);

device_params! {
    pub struct BeatRepeatParams {
        interval: "Interval", 0.0, 7.0, 5.0, INTERVALS, Unit::Plain;
        offset: "Offset", 0.0, 15.0, 8.0, LIN, Unit::Integer;
        chance: "Chance", 0.0, 1.0, 1.0, LIN, Unit::Percent;
        gate: "Gate", 1.0, 64.0, 8.0, LIN, Unit::Integer;
        grid: "Grid", 0.0, 3.0, 2.0, GRIDS, Unit::Plain;
        triplet: "Triplet", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        variation: "Variation", 0.0, 10.0, 0.0, LIN, Unit::Plain;
        var_mode: "Var. Mode", 0.0, 3.0, 0.0, VAR_MODES, Unit::Plain;
        pitch: "Pitch", -48.0, 0.0, 0.0, LIN, Unit::Semitones;
        pitch_decay: "Pitch Decay", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        filter: "Filter", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        filter_freq: "Filter Freq", 50.0, 18000.0, 1000.0, LOG, Unit::Hz;
        filter_width: "Filter Width", 0.5, 9.0, 4.0, LIN, Unit::Plain;
        decay: "Decay", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        mix_mode: "Mix Mode", 0.0, 2.0, 1.0, MIX_MODES, Unit::Plain;
        volume: "Volume", -36.0, 6.0, 0.0, LIN, Unit::Db;
        repeat: "Repeat", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
    }
}

/// Interval lengths in beats (the bar values are scaled by the meter).
const INTERVAL_BEATS: [f64; 8] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
/// Grid lengths in beats.
const GRID_BEATS: [f64; 4] = [1.0, 0.5, 0.25, 0.125];
/// Period of automatic re-variation in beats (index 0 = only on trigger).
const VAR_BEATS: [f64; 4] = [0.0, 1.0, 0.5, 0.25];
/// Longest slice the device can capture.
const MAX_CAPTURE_SECONDS: f32 = 8.0;

fn choice(v: f32, n: usize) -> usize {
    (v.round().max(0.0) as usize).min(n - 1)
}

impl super::EffectParams for BeatRepeatParams {
    fn is_relevant(&self, index: usize) -> bool {
        match index {
            7 => self.variation > 0.0,
            9 => self.pitch < 0.0,
            11 | 12 => self.filter >= 0.5,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Last 16th Stutter",
                Self {
                    interval: 5.0,
                    offset: 12.0,
                    gate: 4.0,
                    grid: 3.0,
                    ..d
                },
            ),
            (
                "Tape Stop Fill",
                Self {
                    interval: 6.0,
                    offset: 12.0,
                    gate: 4.0,
                    grid: 2.0,
                    pitch: -12.0,
                    pitch_decay: 0.6,
                    decay: 0.2,
                    ..d
                },
            ),
            (
                "Random Glitch",
                Self {
                    interval: 3.0,
                    offset: 0.0,
                    chance: 0.3,
                    gate: 2.0,
                    grid: 2.0,
                    variation: 4.0,
                    var_mode: 3.0,
                    ..d
                },
            ),
            (
                "Filtered Echo Repeats",
                Self {
                    interval: 5.0,
                    offset: 4.0,
                    gate: 12.0,
                    grid: 1.0,
                    filter: 1.0,
                    filter_freq: 1500.0,
                    filter_width: 2.0,
                    decay: 0.3,
                    mix_mode: 0.0,
                    ..d
                },
            ),
        ]
    }
}

pub struct BeatRepeat {
    sr: f32,
    /// Captured slice, one buffer per channel.
    cap: [Vec<f32>; 2],
    rng: Rng,
    /// Interval index of the last trigger check (`None` right after start).
    last_idx: Option<i64>,
    active: bool,
    /// Samples since the repeat started (drives the gate and variation).
    elapsed: u64,
    gate_len: u64,
    /// Samples captured so far.
    captured: usize,
    /// Length of the current repeat in samples.
    slice_len: usize,
    /// Position inside the current repeat.
    slice_pos: usize,
    /// Index of the current repeat (0 = the pass that is being captured).
    repeat_n: u32,
    rate: f32,
    gain: f32,
    /// Smoothed amount of repeat signal in the output (declicks starts/stops).
    wet_amt: f32,
    vol: f32,
    hp: [Biquad; 2],
    lp: [Biquad; 2],
    filt_key: (f32, f32),
    hp_c: Coefs,
    lp_c: Coefs,
    was_manual: bool,
}

impl BeatRepeat {
    pub fn new(sample_rate: f32) -> Self {
        let n = (MAX_CAPTURE_SECONDS * sample_rate) as usize + 4;
        Self {
            sr: sample_rate,
            cap: [vec![0.0; n], vec![0.0; n]],
            rng: Rng::new(0x5eed_b347),
            last_idx: None,
            active: false,
            elapsed: 0,
            gate_len: 0,
            captured: 0,
            slice_len: 16,
            slice_pos: 0,
            repeat_n: 0,
            rate: 1.0,
            gain: 1.0,
            wet_amt: 0.0,
            vol: 1.0,
            hp: Default::default(),
            lp: Default::default(),
            filt_key: (-1.0, -1.0),
            hp_c: Coefs::IDENTITY,
            lp_c: Coefs::IDENTITY,
            was_manual: false,
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.last_idx = None;
        self.wet_amt = 0.0;
        self.was_manual = false;
        for f in self.hp.iter_mut().chain(self.lp.iter_mut()) {
            f.reset();
        }
    }

    fn uniform(&mut self) -> f32 {
        (self.rng.next_bipolar() * 0.5 + 0.5).clamp(0.0, 0.999_999)
    }

    /// A slice length for the grid, randomised by the variation amount.
    fn pick_len(&mut self, p: &BeatRepeatParams, spb: f64, vary: bool) -> usize {
        let mut beats = GRID_BEATS[choice(p.grid, 4)];
        if p.triplet >= 0.5 {
            beats *= 2.0 / 3.0;
        }
        let var = p.variation.clamp(0.0, 10.0);
        if vary && var > 0.0 {
            let steps = var.floor() as u32 + 1;
            let j = (self.uniform() * steps as f32) as u32;
            beats /= (1 + j) as f64;
        }
        let max = self.cap[0].len() - 2;
        ((beats * spb) as usize).clamp(16, max)
    }

    fn start(&mut self, p: &BeatRepeatParams, spb: f64) {
        self.active = true;
        self.elapsed = 0;
        self.gate_len = (p.gate.clamp(1.0, 64.0) as f64 * 0.25 * spb) as u64;
        self.captured = 0;
        self.slice_pos = 0;
        self.repeat_n = 0;
        // Automatic variation re-picks later; its first capture is a whole grid.
        let vary = choice(p.var_mode, 4) == 0;
        self.slice_len = self.pick_len(p, spb, vary);
        self.rate = 1.0;
        self.gain = 1.0;
    }

    /// Sets pitch and gain for the repeat that starts now.
    fn enter_repeat(&mut self, p: &BeatRepeatParams) {
        let n = self.repeat_n.max(1) as f32;
        let st = (p.pitch.clamp(-48.0, 0.0) - p.pitch_decay.clamp(0.0, 1.0) * 3.0 * (n - 1.0)).max(-48.0);
        self.rate = 2f32.powf(st / 12.0);
        self.gain = (1.0 - p.decay.clamp(0.0, 1.0)).powf(n);
    }

    fn update_filter(&mut self, p: &BeatRepeatParams) {
        let key = (p.filter_freq, p.filter_width);
        if key != self.filt_key {
            self.filt_key = key;
            let half = 2f32.powf(p.filter_width.clamp(0.5, 9.0) * 0.5);
            let f = p.filter_freq.clamp(20.0, 20000.0);
            self.hp_c = Coefs::design(FilterShape::HighPass, f / half, 0.707, 0.0, self.sr);
            self.lp_c = Coefs::design(FilterShape::LowPass, f * half, 0.707, 0.0, self.sr);
        }
    }

    pub fn process(&mut self, p: &BeatRepeatParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let tempo = ctx.tempo.clamp(1.0, 999.0);
        let spb = 60.0 / tempo * self.sr as f64;
        let coef = super::smoothing(0.003, self.sr);
        if !ctx.playing {
            // Stopped: the last repeat fades out, then audio passes through.
            self.active = false;
            self.last_idx = None;
            self.was_manual = false;
        }
        self.update_filter(p);
        let filter_on = p.filter >= 0.5;
        let target_vol = super::db(p.volume.clamp(-36.0, 6.0));
        let mix_mode = choice(p.mix_mode, 3);
        let bpb = ctx.beats_per_bar.max(1) as f64;
        let mut interval = INTERVAL_BEATS[choice(p.interval, 8)];
        if interval >= 4.0 {
            interval = interval / 4.0 * bpb;
        }
        let offset = (p.offset.round().clamp(0.0, 15.0) as f64 * 0.25) % interval;
        let beat_inc = 1.0 / spb;
        let var_period = (VAR_BEATS[choice(p.var_mode, 4)] * spb) as u64;
        let manual = p.repeat >= 0.5;
        let fade = (0.0015 * self.sr) as usize;
        let chance = p.chance.clamp(0.0, 1.0);

        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let x = [*l, *r];
            if ctx.playing {
                let beat = ctx.song_beat + i as f64 * beat_inc;
                let rel = beat - offset;
                let idx = (rel / interval).floor() as i64;
                if self.last_idx != Some(idx) {
                    let frac = rel - idx as f64 * interval;
                    // Only fire on an actual crossing, not after a relocation.
                    if frac < beat_inc * 1.5 && !manual && self.uniform() < chance {
                        self.start(p, spb);
                    }
                    self.last_idx = Some(idx);
                }
                if manual && !self.was_manual {
                    self.start(p, spb);
                    self.gate_len = u64::MAX;
                } else if !manual && self.was_manual {
                    self.active = false;
                }
                self.was_manual = manual;
            }

            let mut wet = [0.0f32; 2];
            if self.active && self.elapsed >= self.gate_len {
                self.active = false;
            }
            if self.active {
                if var_period > 0 && self.repeat_n > 0 && self.elapsed > 0 && self.elapsed.is_multiple_of(var_period) {
                    self.slice_len = self.pick_len(p, spb, true).min(self.captured.max(16));
                    self.slice_pos = 0;
                    self.repeat_n += 1;
                    self.enter_repeat(p);
                }
                if self.slice_pos >= self.slice_len {
                    self.slice_pos = 0;
                    self.repeat_n += 1;
                    self.enter_repeat(p);
                }
                if self.repeat_n == 0 {
                    for c in 0..2 {
                        self.cap[c][self.slice_pos] = x[c];
                        wet[c] = x[c];
                    }
                    self.captured = self.slice_pos + 1;
                } else {
                    let last = self.captured.saturating_sub(1);
                    let rp = self.slice_pos as f32 * self.rate;
                    let i0 = (rp as usize).min(last);
                    let i1 = (i0 + 1).min(last);
                    let fr = (rp - i0 as f32).clamp(0.0, 1.0);
                    // Short fades at both ends of every repeat avoid clicks.
                    let edge = self.slice_pos.min(self.slice_len - self.slice_pos);
                    let env = if edge < fade { edge as f32 / fade as f32 } else { 1.0 };
                    for c in 0..2 {
                        let a = self.cap[c][i0];
                        let b = self.cap[c][i1];
                        wet[c] = (a + (b - a) * fr) * env * self.gain;
                    }
                }
                self.slice_pos += 1;
                self.elapsed += 1;
            }
            if filter_on {
                for c in 0..2 {
                    let y = self.hp[c].process(&self.hp_c, wet[c]);
                    let y = self.lp[c].process(&self.lp_c, y);
                    // The captured pass stays clean; the filter colours the repeats.
                    if self.repeat_n > 0 {
                        wet[c] = y;
                    }
                }
            }
            let target = if self.active { 1.0 } else { 0.0 };
            self.wet_amt = target + (self.wet_amt - target) * coef;
            self.vol = target_vol + (self.vol - target_vol) * coef;
            let (dry_g, wet_g) = match mix_mode {
                0 => (1.0, self.wet_amt),
                1 => (1.0 - self.wet_amt, self.wet_amt),
                _ => (0.0, self.wet_amt),
            };
            *l = x[0] * dry_g + wet[0] * wet_g * self.vol;
            *r = x[1] * dry_g + wet[1] * wet_g * self.vol;
        }
    }
}

impl super::EffectRuntime for BeatRepeat {}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn run(p: &BeatRepeatParams, input: &[f32], playing: bool) -> Vec<f32> {
        let mut br = BeatRepeat::new(SR);
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        let spb = 60.0 / 120.0 * SR as f64;
        let block = 256;
        for (k, (a, b)) in l.chunks_mut(block).zip(r.chunks_mut(block)).enumerate() {
            let beat = (k * block) as f64 / spb;
            let ctx = FxContext::new(SR, 120.0).with_transport(playing, beat, 4, false);
            br.process(p, a, b, &ctx);
        }
        assert!(l.iter().all(|x| x.is_finite()));
        l
    }

    fn signal(n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| ((i as f32) * 0.0137).sin() * 0.5 + (i as f32 * 0.00071).sin() * 0.3)
            .collect()
    }

    #[test]
    fn repeats_captured_slice_on_grid() {
        // 120 BPM: a sixteenth is 6000 samples; the trigger is at beat 0.
        let p = BeatRepeatParams {
            offset: 0.0,
            gate: 8.0,
            grid: 2.0,
            ..Default::default()
        };
        let input = signal(120000);
        let out = run(&p, &input, true);
        // The first pass is the input itself.
        for i in 100..5900 {
            assert!((out[i] - input[i]).abs() < 1e-5, "first pass {i}");
        }
        // Then the slice repeats every 6000 samples for the 2-beat gate.
        for rep in 1..8 {
            for k in (200..5800).step_by(7) {
                let o = out[rep * 6000 + k];
                assert!((o - input[k]).abs() < 1e-4, "repeat {rep} at {k}: {o} vs {}", input[k]);
            }
        }
        // After the gate (48000 samples) the dry signal returns.
        for i in 49000..90000 {
            assert!((out[i] - input[i]).abs() < 1e-3, "after gate {i}");
        }
        // Next bar (96000) triggers again: its second sixteenth repeats the first.
        for k in (200..5800).step_by(11) {
            assert!((out[96000 + 6000 + k] - input[96000 + k]).abs() < 1e-4);
        }
    }

    #[test]
    fn offset_moves_the_trigger() {
        // Offset 4 sixteenths = beat 1 = sample 24000.
        let p = BeatRepeatParams {
            offset: 4.0,
            gate: 4.0,
            grid: 2.0,
            ..Default::default()
        };
        let input = signal(60000);
        let out = run(&p, &input, true);
        for i in 0..24000 {
            assert!((out[i] - input[i]).abs() < 1e-6);
        }
        for k in (200..5800).step_by(5) {
            assert!((out[30000 + k] - input[24000 + k]).abs() < 1e-4);
        }
    }

    #[test]
    fn stopped_or_no_chance_passes_through() {
        let input = signal(30000);
        let p = BeatRepeatParams {
            offset: 0.0,
            ..Default::default()
        };
        assert_eq!(run(&p, &input, false), input);
        let p = BeatRepeatParams {
            offset: 0.0,
            chance: 0.0,
            ..Default::default()
        };
        assert_eq!(run(&p, &input, true), input);
    }

    #[test]
    fn decay_and_pitch_shape_repeats() {
        let input: Vec<f32> = (0..60000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let p = BeatRepeatParams {
            offset: 0.0,
            gate: 8.0,
            decay: 0.5,
            pitch: -12.0,
            pitch_decay: 0.5,
            filter: 1.0,
            ..Default::default()
        };
        let out = run(&p, &input, true);
        let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
        let r1 = rms(&out[6000..12000]);
        let r4 = rms(&out[24000..30000]);
        assert!(r1 > 0.05, "{r1}");
        assert!(r4 < r1 * 0.3, "decay: {r1} {r4}");
        assert!(out.iter().all(|x| x.abs() < 2.0));
    }

    #[test]
    fn extreme_settings_stay_finite() {
        let input = signal(48000);
        let p = BeatRepeatParams {
            interval: 0.0,
            offset: 15.0,
            gate: 64.0,
            grid: 0.0,
            triplet: 1.0,
            variation: 10.0,
            var_mode: 3.0,
            pitch: -48.0,
            pitch_decay: 1.0,
            filter: 1.0,
            filter_freq: 18000.0,
            filter_width: 9.0,
            mix_mode: 2.0,
            volume: 6.0,
            ..Default::default()
        };
        let mut br = BeatRepeat::new(SR);
        let mut l = input.clone();
        let mut r = input.clone();
        for (k, (a, b)) in l.chunks_mut(1).zip(r.chunks_mut(1)).enumerate().take(20000) {
            let ctx = FxContext::new(SR, 20.0).with_transport(true, k as f64 / (3.0 * SR as f64), 4, false);
            br.process(&p, a, b, &ctx);
        }
        let p = BeatRepeatParams { repeat: 1.0, ..p };
        for (k, (a, b)) in l.chunks_mut(64).zip(r.chunks_mut(64)).enumerate() {
            let ctx = FxContext::new(SR, 300.0).with_transport(true, k as f64 * 0.4, 4, false);
            br.process(&p, a, b, &ctx);
        }
        assert!(l.iter().chain(r.iter()).all(|x| x.is_finite() && x.abs() < 4.0));
    }
}
