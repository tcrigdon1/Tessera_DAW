//! Shifter: pitch shifting, frequency shifting and ring modulation.
//!
//! - Pitch: a delay-line shifter with two crossfaded read heads half a window
//!   apart; each head's delay ramps at a speed set by the pitch ratio and the
//!   sin^2 windows sum to one, so the output keeps a constant level.
//! - Frequency: the input is split into an analytic pair (two chains of
//!   first-order allpass filters whose outputs stay 90 degrees apart from
//!   20 Hz to 20 kHz) and multiplied by a quadrature carrier, moving every
//!   partial by the same number of Hz (single sideband).
//! - Ring: the (optionally driven) input times a sine carrier.
//!
//! An LFO modulates the pitch or the frequency; feedback, a tone control and
//! dry/wet follow.

use super::auto_filter::{
    read_cubic, soft_clip, wrap, LfoClock, LfoVoice, SvfCoefs, SvfState, LFO_SHAPES, SHAPE_RANDOM, SYNC_QUARTER,
    SYNC_RATES,
};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const MODES: &[&str] = &["Pitch", "Frequency", "Ring"];
pub const MODE_PITCH: usize = 0;
pub const MODE_FREQ: usize = 1;
pub const MODE_RING: usize = 2;
const DIRECTIONS: &[&str] = &["Up", "Down", "Wide"];
const MAX_WINDOW_MS: f32 = 200.0;

/// Hilbert pair design: break frequencies of the first-order allpass
/// sections, as natural logs of multiples of the warped 20 Hz frequency.
/// Even entries form the in-phase chain, odd entries the quadrature chain.
/// Designed (minimax on the phase difference over 20 Hz..20 kHz at 44.1 kHz,
/// a 1:5000 warped band) for this project; maximum error about 0.7 degrees,
/// so the unwanted sideband sits around -44 dB.
const HILBERT_POLES: [f32; 16] = [
    -0.915_969, 0.263_156, 0.823_417, 1.316_578, 1.979_481, 2.688_455, 3.315_228, 3.931_202, 4.584_244, 5.198_727,
    5.824_608, 6.531_878, 7.188_291, 7.676_895, 8.242_437, 9.427_657,
];
const HILBERT_SECTIONS: usize = 8;

device_params! {
    pub struct ShifterParams {
        mode: "Mode", 0.0, 2.0, 0.0, ParamKind::Choice(MODES), Unit::Plain;
        pitch: "Pitch", -24.0, 24.0, 0.0, LIN, Unit::Semitones;
        fine: "Fine", -50.0, 50.0, 0.0, LIN, Unit::Integer;
        window: "Window", 10.0, 200.0, 60.0, LOG, Unit::Ms;
        freq: "Frequency", 0.5, 5000.0, 120.0, LOG, Unit::Hz;
        direction: "Direction", 0.0, 2.0, 0.0, ParamKind::Choice(DIRECTIONS), Unit::Plain;
        drive: "Drive", 0.0, 24.0, 0.0, LIN, Unit::Db;
        lfo_amount: "LFO Amount", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        lfo_sync: "LFO Sync", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        lfo_rate: "LFO Rate", 0.01, 40.0, 0.5, LOG, Unit::Hz;
        lfo_div: "LFO Rate", 0.0, 16.0, SYNC_QUARTER, ParamKind::Choice(SYNC_RATES), Unit::Plain;
        lfo_shape: "LFO Shape", 0.0, 6.0, 0.0, ParamKind::Choice(LFO_SHAPES), Unit::Plain;
        feedback: "Feedback", 0.0, 0.95, 0.0, LIN, Unit::Percent;
        tone: "Tone", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl ShifterParams {
    pub fn mode_index(&self) -> usize {
        (self.mode.round().max(0.0) as usize).min(MODE_RING)
    }
}

impl super::EffectParams for ShifterParams {
    fn is_relevant(&self, index: usize) -> bool {
        let mode = self.mode_index();
        let lfo = self.lfo_amount > 0.0;
        let synced = self.lfo_sync >= 0.5;
        match index {
            1..=3 => mode == MODE_PITCH,
            4 => mode != MODE_PITCH,
            5 => mode == MODE_FREQ,
            6 => mode == MODE_RING,
            8 | 11 => lfo,
            9 => lfo && !synced,
            10 => lfo && synced,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Octave Up",
                Self {
                    pitch: 12.0,
                    window: 40.0,
                    mix: 0.5,
                    ..d
                },
            ),
            (
                "Detune Thickener",
                Self {
                    pitch: 0.0,
                    fine: 12.0,
                    window: 80.0,
                    mix: 0.5,
                    ..d
                },
            ),
            (
                "Barber Pole",
                Self {
                    mode: MODE_FREQ as f32,
                    freq: 3.0,
                    feedback: 0.6,
                    mix: 0.6,
                    ..d
                },
            ),
            (
                "Ring Bell",
                Self {
                    mode: MODE_RING as f32,
                    freq: 440.0,
                    drive: 6.0,
                    lfo_amount: 0.05,
                    lfo_rate: 5.0,
                    mix: 0.7,
                    ..d
                },
            ),
        ]
    }
}

pub struct Shifter {
    sr: f32,
    clock: LfoClock,
    voice: LfoVoice,
    buf: [Vec<f32>; 2],
    pos: usize,
    /// Read-head phase of the pitch shifter (0..1 of a window).
    head: f32,
    /// Allpass coefficients and states of the Hilbert pair.
    hil_coef: [f32; 16],
    hil_state: [[f32; 16]; 2],
    /// Carrier phase in cycles.
    carrier: f64,
    freq: f32,
    last: [f32; 2],
    tone: [SvfState; 2],
    mix: f32,
    started: bool,
}

impl Shifter {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let n = (MAX_WINDOW_MS * 0.001 * sr) as usize + 16;
        let w_lo = (std::f32::consts::PI * 20.0 / sr).tan();
        let mut hil_coef = [0.0; 16];
        for (c, x) in hil_coef.iter_mut().zip(HILBERT_POLES) {
            let t = w_lo * x.exp();
            *c = (t - 1.0) / (t + 1.0);
        }
        Self {
            sr,
            clock: LfoClock::default(),
            voice: LfoVoice::new(0x5eed_f00d),
            buf: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            head: 0.0,
            hil_coef,
            hil_state: [[0.0; 16]; 2],
            carrier: 0.0,
            freq: 0.0,
            last: [0.0; 2],
            tone: [SvfState::default(); 2],
            mix: 1.0,
            started: false,
        }
    }

    pub fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
        self.hil_state = [[0.0; 16]; 2];
        self.last = [0.0; 2];
        for t in &mut self.tone {
            t.reset();
        }
        self.clock.reset();
    }

    /// Splits `x` into (in-phase, quadrature) outputs of the Hilbert pair.
    #[inline]
    fn hilbert(coef: &[f32; 16], state: &mut [f32; 16], x: f32) -> (f32, f32) {
        let mut i = x;
        let mut q = x;
        for s in 0..HILBERT_SECTIONS {
            // Quadrature chain: even entries.
            let k = 2 * s;
            let y = coef[k] * q + state[k];
            state[k] = q - coef[k] * y;
            q = y;
            // In-phase chain: odd entries.
            let k = 2 * s + 1;
            let y = coef[k] * i + state[k];
            state[k] = i - coef[k] * y;
            i = y;
        }
        if state[0].abs() < 1e-20 && state[1].abs() < 1e-20 {
            *state = [0.0; 16];
        }
        (i, q)
    }

    pub fn process(&mut self, p: &ShifterParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let sr = self.sr;
        let frames = left.len().min(right.len());
        let mode = p.mode_index();
        let target_mix = p.mix.clamp(0.0, 1.0);
        let target_freq = p.freq.clamp(0.0, 20000.0);
        if !self.started {
            self.started = true;
            self.mix = target_mix;
            self.freq = target_freq;
        }
        let smooth = 1.0 - super::smoothing(0.02, sr);
        let (start, inc) = self.clock.block(p.lfo_sync >= 0.5, p.lfo_rate, p.lfo_div, ctx, frames);
        let shape = (p.lfo_shape.round().max(0.0) as usize).min(SHAPE_RANDOM);
        let lfo_amt = p.lfo_amount.clamp(0.0, 1.0);
        let fb = p.feedback.clamp(0.0, 0.95);
        let base_st = p.pitch.clamp(-24.0, 24.0) + p.fine.clamp(-50.0, 50.0) * 0.01;
        let n = self.buf[0].len();
        let window = (p.window.clamp(5.0, MAX_WINDOW_MS) * 0.001 * sr)
            .min((n - 8) as f32)
            .max(8.0);
        let direction = p.direction.round() as i32;
        let drive = super::db(p.drive.clamp(0.0, 36.0));
        let drive_comp = 1.0 / drive.sqrt();
        let tone = p.tone.clamp(-1.0, 1.0);
        let tone_coefs = if tone < 0.0 {
            SvfCoefs::new(20000.0 * (0.01f32).powf(-tone), 0.6, sr)
        } else {
            SvfCoefs::new(20.0 * (250.0f32).powf(tone), 0.6, sr)
        };
        let use_tone = tone.abs() > 0.005;
        for k in 0..frames {
            self.mix += (target_mix - self.mix) * smooth;
            self.freq += (target_freq - self.freq) * smooth;
            let lfo = if lfo_amt > 0.0 {
                self.voice.value(shape, wrap((start + inc * k as f64) as f32))
            } else {
                0.0
            };
            let x = [left[k], right[k]];
            let input = [x[0] + soft_clip(self.last[0] * fb), x[1] + soft_clip(self.last[1] * fb)];
            let mut wet = [0.0f32; 2];
            match mode {
                MODE_PITCH => {
                    let st = base_st + lfo * lfo_amt * 12.0;
                    let ratio = (st / 12.0).exp2().clamp(0.0625, 16.0);
                    for c in 0..2 {
                        self.buf[c][self.pos] = input[c];
                    }
                    let h0 = self.head;
                    let h1 = wrap(h0 + 0.5);
                    let g0 = (std::f32::consts::PI * h0).sin().powi(2);
                    let g1 = 1.0 - g0;
                    for c in 0..2 {
                        let a = read_cubic(&self.buf[c], self.pos, 2.0 + h0 * window);
                        let b = read_cubic(&self.buf[c], self.pos, 2.0 + h1 * window);
                        wet[c] = a * g0 + b * g1;
                    }
                    self.head = wrap(self.head + (1.0 - ratio) / window);
                    self.pos += 1;
                    if self.pos == n {
                        self.pos = 0;
                    }
                }
                MODE_FREQ => {
                    let f = self.freq * (1.0 + lfo_amt * lfo);
                    let (s, co) = ((self.carrier * std::f64::consts::TAU) as f32).sin_cos();
                    for c in 0..2 {
                        let (i, q) = Self::hilbert(&self.hil_coef, &mut self.hil_state[c], input[c]);
                        let down = match direction {
                            1 => true,
                            2 => c == 1,
                            _ => false,
                        };
                        wet[c] = if down { i * co + q * s } else { i * co - q * s };
                    }
                    self.carrier = (self.carrier + f as f64 / sr as f64).rem_euclid(1.0);
                }
                _ => {
                    let f = self.freq * (1.0 + lfo_amt * lfo);
                    let s = ((self.carrier * std::f64::consts::TAU) as f32).sin();
                    for c in 0..2 {
                        let v = if p.drive > 0.01 {
                            soft_clip(input[c] * drive) * drive_comp
                        } else {
                            input[c]
                        };
                        wet[c] = v * s;
                    }
                    self.carrier = (self.carrier + f as f64 / sr as f64).rem_euclid(1.0);
                }
            }
            for c in 0..2 {
                if use_tone {
                    let (lo, _, hi) = self.tone[c].tick(&tone_coefs, wet[c]);
                    wet[c] = if tone < 0.0 { lo } else { hi };
                }
                self.last[c] = if wet[c].abs() < 1e-20 { 0.0 } else { wet[c] };
            }
            let m = self.mix;
            left[k] = x[0] + (wet[0] - x[0]) * m;
            right[k] = x[1] + (wet[1] - x[1]) * m;
        }
    }
}

impl super::EffectRuntime for Shifter {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Amplitude of the `freq` component (Goertzel-style correlation).
    fn level(x: &[f32], freq: f32, sr: f32) -> f32 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (i, v) in x.iter().enumerate() {
            let ph = i as f64 * freq as f64 * std::f64::consts::TAU / sr as f64;
            re += *v as f64 * ph.cos();
            im += *v as f64 * ph.sin();
        }
        (2.0 * (re * re + im * im).sqrt() / x.len() as f64) as f32
    }

    fn run(p: &ShifterParams, freq: f32, n: usize) -> (Vec<f32>, Vec<f32>) {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let mut s = Shifter::new(sr);
        let mut l: Vec<f32> = (0..n)
            .map(|i| (i as f32 * freq * std::f32::consts::TAU / sr).sin() * 0.5)
            .collect();
        let mut r = l.clone();
        for (a, b) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            s.process(p, a, b, &ctx);
        }
        (l, r)
    }

    #[test]
    fn hilbert_pair_is_in_quadrature() {
        let sr = 48000.0;
        let s = Shifter::new(sr);
        for f in [40.0f32, 200.0, 1000.0, 5000.0, 15000.0] {
            let mut st = [0.0f32; 16];
            let n = 48000;
            let (mut ii, mut qq, mut iq) = (0.0f64, 0.0f64, 0.0f64);
            for k in 0..n {
                let x = (k as f32 * f * std::f32::consts::TAU / sr).sin();
                let (i, q) = Shifter::hilbert(&s.hil_coef, &mut st, x);
                if k > n / 2 {
                    ii += (i * i) as f64;
                    qq += (q * q) as f64;
                    iq += (i * q) as f64;
                }
            }
            // Equal level and uncorrelated (90 degrees apart).
            assert!((ii / qq - 1.0).abs() < 0.01, "{f}");
            let corr = iq / (ii * qq).sqrt();
            assert!(corr.abs() < 0.02, "{f}: {corr}");
        }
    }

    #[test]
    fn frequency_shift_moves_partials() {
        let sr = 48000.0;
        let p = ShifterParams {
            mode: MODE_FREQ as f32,
            freq: 200.0,
            ..Default::default()
        };
        let (l, _) = run(&p, 1000.0, 48000);
        let tail = &l[24000..];
        let up = level(tail, 1200.0, sr);
        let mirror = level(tail, 800.0, sr);
        let orig = level(tail, 1000.0, sr);
        assert!(up > 0.4, "{up}");
        assert!(mirror < up / 50.0 && orig < up / 50.0, "{up} {mirror} {orig}");
        let down = ShifterParams { direction: 1.0, ..p };
        let (l, _) = run(&down, 1000.0, 48000);
        assert!(level(&l[24000..], 800.0, sr) > 0.4);
        // Wide: left up, right down.
        let wide = ShifterParams { direction: 2.0, ..p };
        let (l, r) = run(&wide, 1000.0, 48000);
        assert!(level(&l[24000..], 1200.0, sr) > 0.4 && level(&r[24000..], 800.0, sr) > 0.4);
    }

    #[test]
    fn pitch_shift_up_an_octave() {
        let sr = 48000.0;
        let p = ShifterParams {
            pitch: 12.0,
            window: 50.0,
            ..Default::default()
        };
        let (l, _) = run(&p, 440.0, 48000);
        let tail = &l[12000..];
        let oct = level(tail, 880.0, sr);
        let orig = level(tail, 440.0, sr);
        assert!(oct > 0.3 && oct > orig * 5.0, "{oct} {orig}");
        let down = ShifterParams { pitch: -12.0, ..p };
        let (l, _) = run(&down, 880.0, 48000);
        let tail = &l[12000..];
        assert!(level(tail, 440.0, sr) > level(tail, 880.0, sr) * 5.0);
    }

    #[test]
    fn unity_pitch_is_nearly_transparent() {
        let p = ShifterParams::default();
        let (l, _) = run(&p, 440.0, 24000);
        let lv = level(&l[12000..], 440.0, 48000.0);
        assert!((lv - 0.5).abs() < 0.03, "{lv}");
    }

    #[test]
    fn ring_modulation_makes_sidebands() {
        let sr = 48000.0;
        let p = ShifterParams {
            mode: MODE_RING as f32,
            freq: 300.0,
            ..Default::default()
        };
        let (l, _) = run(&p, 1000.0, 48000);
        let t = &l[4800..];
        assert!(level(t, 700.0, sr) > 0.2 && level(t, 1300.0, sr) > 0.2);
        assert!(level(t, 1000.0, sr) < 0.01);
    }

    #[test]
    fn extremes_stay_finite() {
        let sr = 44100.0;
        let ctx = FxContext::new(sr, 120.0);
        for mode in 0..3 {
            let p = ShifterParams {
                mode: mode as f32,
                pitch: 24.0,
                fine: 50.0,
                window: 10.0,
                freq: 5000.0,
                drive: 24.0,
                lfo_amount: 1.0,
                lfo_rate: 40.0,
                feedback: 0.95,
                tone: 1.0,
                ..Default::default()
            };
            let mut s = Shifter::new(sr);
            let mut l: Vec<f32> = (0..20000).map(|i| if i % 40 < 20 { 1.0 } else { -1.0 }).collect();
            let mut r = l.clone();
            s.process(&p, &mut l[..1], &mut r[..1], &ctx);
            s.process(&p, &mut l[1..], &mut r[1..], &ctx);
            let p2 = ShifterParams {
                pitch: -24.0,
                tone: -1.0,
                ..p
            };
            s.process(&p2, &mut l, &mut r, &ctx);
            assert!(l.iter().chain(&r).all(|x| x.is_finite() && x.abs() < 20.0), "{mode}");
        }
    }
}
