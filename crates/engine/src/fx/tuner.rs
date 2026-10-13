//! Tuner: passes audio through (or mutes it while tuning) and copies the mono
//! sum into its [`scope`] tap. Pitch detection runs on the UI thread with
//! [`PitchDetector`], a YIN-style detector: a difference function computed
//! through an FFT cross-correlation, cumulative mean normalisation with an
//! absolute threshold, then parabolic refinement on the exact difference
//! function for sub-sample period accuracy.

use std::sync::Arc;

use super::fft::{Complex, RealFft};
use super::scope::{self, Scope};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::model::Id;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct TunerParams {
        reference: "Reference", 410.0, 480.0, 440.0, LIN, Unit::Hz;
        mode: "Mode", 0.0, 1.0, 0.0, ParamKind::Choice(&["Classic", "Strobe"]), Unit::Plain;
        mute: "Mute", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
    }
}

impl super::EffectParams for TunerParams {
    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            ("Strobe A440", Self { mode: 1.0, ..d }),
            ("Silent Tuning", Self { mute: 1.0, ..d }),
            ("Baroque A415", Self { reference: 415.0, ..d }),
        ]
    }
}

/// Lowest and highest detected frequency.
pub const MIN_HZ: f32 = 30.0;
pub const MAX_HZ: f32 = 2200.0;

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// Note name with octave in this project's convention (MIDI 60 = C3).
pub fn note_name(midi: i32) -> String {
    format!(
        "{}{}",
        NOTE_NAMES[midi.rem_euclid(12) as usize],
        midi.div_euclid(12) - 2
    )
}

/// Nearest MIDI note and deviation from it in cents (-50 ..= 50) for a
/// frequency, given the frequency of A (MIDI 69).
pub fn hz_to_note(hz: f32, reference: f32) -> (i32, f32) {
    let m = 69.0 + 12.0 * (hz.max(1e-3) / reference.max(1.0)).log2();
    let n = m.round();
    (n as i32, (m - n) * 100.0)
}

/// A detected pitch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pitch {
    pub hz: f32,
    /// 0..1: how periodic the signal is (1 = perfectly periodic).
    pub clarity: f32,
}

/// Reusable buffers for [`PitchDetector::detect`] (UI thread).
#[derive(Default, Clone)]
pub struct PitchDetector {
    fft: Option<RealFft>,
    x: Vec<f32>,
    pad: Vec<f32>,
    sa: Vec<Complex>,
    sx: Vec<Complex>,
    corr: Vec<f32>,
    prefix: Vec<f64>,
    cmnd: Vec<f32>,
}

const THRESHOLD: f32 = 0.15;

impl PitchDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fundamental frequency of `samples` between `min_hz` and `max_hz`, or
    /// `None` for silence and unpitched sound.
    pub fn detect(&mut self, samples: &[f32], sample_rate: f32, min_hz: f32, max_hz: f32) -> Option<Pitch> {
        let len = samples.len();
        if len < 64
            || sample_rate.is_nan()
            || sample_rate <= 0.0
            || min_hz.is_nan()
            || min_hz <= 0.0
            || max_hz.is_nan()
            || max_hz <= min_hz
        {
            return None;
        }
        let tau_max = ((sample_rate / min_hz).ceil() as usize + 2).min(len / 2);
        let tau_min = ((sample_rate / max_hz).floor() as usize).max(2);
        if tau_min + 3 >= tau_max {
            return None;
        }
        let w = len - tau_max;

        // Remove DC.
        let mean = samples.iter().map(|&v| v as f64).sum::<f64>() / len as f64;
        self.x.clear();
        self.x.extend(
            samples
                .iter()
                .map(|&v| if v.is_finite() { (v as f64 - mean) as f32 } else { 0.0 }),
        );
        let x = &self.x;

        self.prefix.clear();
        self.prefix.push(0.0);
        let mut acc = 0.0f64;
        for &v in x {
            acc += v as f64 * v as f64;
            self.prefix.push(acc);
        }
        let e1 = self.prefix[w];
        if e1 / (w as f64) < 1e-9 {
            return None;
        }

        // r(tau) = sum_{j < w} x[j] x[j + tau] by FFT. No circular wrap as
        // j + tau < w + tau_max = len <= n.
        let n = len.next_power_of_two();
        if self.fft.as_ref().map(|f| f.len()) != Some(n) {
            self.fft = Some(RealFft::new(n));
        }
        let fft = self.fft.as_ref().unwrap();
        let bins = fft.bins();
        self.pad.clear();
        self.pad.extend_from_slice(&x[..w]);
        self.pad.resize(n, 0.0);
        self.sa.resize(bins, Complex::ZERO);
        self.sx.resize(bins, Complex::ZERO);
        fft.forward(&self.pad, &mut self.sa);
        self.pad[..len].copy_from_slice(x);
        self.pad[len..].fill(0.0);
        fft.forward(&self.pad, &mut self.sx);
        for (a, b) in self.sa.iter_mut().zip(&self.sx) {
            *a = a.conj() * *b;
        }
        self.corr.resize(n, 0.0);
        fft.inverse(&mut self.sa, &mut self.corr);

        // Cumulative mean normalised difference.
        self.cmnd.clear();
        self.cmnd.resize(tau_max, 1.0);
        let mut sum = 0.0f64;
        for tau in 1..tau_max {
            let e2 = self.prefix[tau + w] - self.prefix[tau];
            let d = (e1 + e2 - 2.0 * self.corr[tau] as f64).max(0.0);
            sum += d;
            self.cmnd[tau] = if sum > 0.0 { (d * tau as f64 / sum) as f32 } else { 1.0 };
        }
        let cm = &self.cmnd;
        let mut best = None;
        let mut tau = tau_min;
        while tau < tau_max - 1 {
            if cm[tau] < THRESHOLD {
                while tau + 1 < tau_max - 1 && cm[tau + 1] < cm[tau] {
                    tau += 1;
                }
                best = Some(tau);
                break;
            }
            tau += 1;
        }
        let tau = match best {
            Some(t) => t,
            None => {
                let t = (tau_min..tau_max - 1).min_by(|&a, &b| cm[a].total_cmp(&cm[b]))?;
                if cm[t] > 0.35 {
                    return None;
                }
                t
            }
        };
        let clarity = (1.0 - cm[tau]).clamp(0.0, 1.0);
        if tau <= 1 || tau + 1 >= len - w {
            return Some(Pitch {
                hz: sample_rate / tau as f32,
                clarity,
            });
        }

        // Exact difference function around the minimum, in double precision.
        let diff = |t: usize| -> f64 {
            let mut s = 0.0f64;
            for j in 0..w {
                let v = x[j] as f64 - x[j + t] as f64;
                s += v * v;
            }
            s
        };
        let vertex = |t: usize| -> f64 {
            let (dm, d0, dp) = (diff(t - 1), diff(t), diff(t + 1));
            let den = dm - 2.0 * d0 + dp;
            let delta = if den > 1e-30 {
                (0.5 * (dm - dp) / den).clamp(-1.0, 1.0)
            } else {
                0.0
            };
            t as f64 + delta
        };
        let mut period = vertex(tau);
        // Refine on a multiple of the period: the interpolation error of the
        // minimum is then divided by the multiple (helps bright waveforms).
        let lim = (len - w - 2) as f64;
        let m = ((lim - 2.0) / period).floor().min(8.0);
        if m >= 2.0 {
            let c = (m * period).round() as usize;
            let lo = c.saturating_sub(2).max(2);
            let hi = (c + 2).min(len - w - 2);
            if lo < hi {
                let t = (lo..=hi).min_by(|&a, &b| diff(a).total_cmp(&diff(b))).unwrap_or(c);
                if t > lo && t < hi {
                    let p = vertex(t) / m;
                    if (p / period - 1.0).abs() < 0.01 {
                        period = p;
                    }
                }
            }
        }
        let hz = (sample_rate as f64 / period) as f32;
        (min_hz * 0.9..=max_hz * 1.1)
            .contains(&hz)
            .then_some(Pitch { hz, clarity })
    }
}

/// Convenience wrapper around [`PitchDetector`] for the default range.
pub fn detect_pitch(samples: &[f32], sample_rate: f32) -> Option<f32> {
    PitchDetector::new()
        .detect(samples, sample_rate, MIN_HZ, MAX_HZ)
        .map(|p| p.hz)
}

pub struct Tuner {
    sr: f32,
    scope: Option<Arc<Scope>>,
}

impl Tuner {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            scope: None,
        }
    }

    pub fn reset(&mut self) {}

    pub fn process(&mut self, p: &TunerParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        if let Some(s) = &self.scope {
            s.push_stereo(left, right, 0.5, 0.5);
        }
        if p.mute >= 0.5 {
            left.fill(0.0);
            right.fill(0.0);
        }
    }
}

impl super::EffectRuntime for Tuner {
    fn attach(&mut self, device: Id) {
        let s = scope::tap(device);
        s.set_sample_rate(self.sr);
        self.scope = Some(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::EffectRuntime;
    use std::f64::consts::TAU;

    fn cents(a: f32, b: f32) -> f32 {
        1200.0 * (a / b).log2()
    }

    fn sine(hz: f64, sr: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (0.6 * (TAU * hz * i as f64 / sr + 0.3).sin()) as f32)
            .collect()
    }

    #[test]
    fn detects_sines_within_a_cent() {
        let mut det = PitchDetector::new();
        for sr in [44100.0f32, 48000.0, 96000.0] {
            for hz in [41.2f32, 110.0, 261.63, 440.0, 1000.0, 1975.5] {
                let x = sine(hz as f64, sr as f64, 8192);
                let p = det.detect(&x, sr, MIN_HZ, MAX_HZ).expect("pitch");
                assert!(cents(p.hz, hz).abs() < 1.0, "{hz} Hz at {sr}: got {} Hz", p.hz);
                assert!(p.clarity > 0.9);
            }
        }
    }

    #[test]
    fn detects_sawtooth_fundamental() {
        let sr = 48000.0;
        for hz in [82.41f64, 196.0, 329.63] {
            let x: Vec<f32> = (0..8192)
                .map(|i| {
                    let ph = (hz * i as f64 / sr).fract();
                    (0.5 * (2.0 * ph - 1.0)) as f32
                })
                .collect();
            let f = detect_pitch(&x, sr as f32).expect("pitch");
            assert!(cents(f, hz as f32).abs() < 1.0, "saw {hz}: {f}");
        }
    }

    #[test]
    fn rejects_silence_and_noise() {
        assert_eq!(detect_pitch(&[0.0; 4096], 48000.0), None);
        let mut rng = crate::dsp::Rng::new(5);
        let noise: Vec<f32> = (0..8192).map(|_| rng.next_bipolar() * 0.5).collect();
        assert_eq!(detect_pitch(&noise, 48000.0), None);
    }

    #[test]
    fn note_names_and_cents() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(69), "A3");
        assert_eq!(note_name(0), "C-2");
        let (n, c) = hz_to_note(440.0, 440.0);
        assert_eq!((n, c.round() as i32), (69, 0));
        let (n, c) = hz_to_note(445.0, 440.0);
        assert_eq!(n, 69);
        assert!((c - 19.56).abs() < 0.1);
        let (n, _) = hz_to_note(415.3, 415.3);
        assert_eq!(n, 69);
    }

    #[test]
    fn mute_silences_output_but_still_taps() {
        let sr = 48000.0;
        let mut t = Tuner::new(sr);
        t.attach(0x7e2e_0001);
        let p = TunerParams {
            mute: 1.0,
            ..Default::default()
        };
        let mut l = sine(440.0, 48000.0, 256);
        let mut r = l.clone();
        let orig = l.clone();
        t.process(&p, &mut l, &mut r, &FxContext::new(sr, 120.0));
        assert!(l.iter().chain(&r).all(|&v| v == 0.0));
        let mut snap = Vec::new();
        scope::find(0x7e2e_0001).unwrap().snapshot_len(&mut snap, 256);
        assert_eq!(snap, orig);
    }
}
