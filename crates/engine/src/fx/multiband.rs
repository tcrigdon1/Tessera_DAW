//! Multiband Dynamics: three bands split by fourth-order Linkwitz-Riley
//! crossovers, each with an "above" and a "below" threshold.
//!
//! Ratios follow one convention on both sides: a ratio above 1 pulls the
//! level towards the threshold (downward compression above, upward
//! compression below) and a ratio below 1 pushes it away (upward expansion
//! above, downward expansion below). With all ratios at 1 the bands sum back
//! to the input with a flat magnitude response (the low band runs through an
//! all-pass matching the upper crossover so the phases line up).

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);
const TOG: ParamKind = ParamKind::Toggle;

pub const BANDS: usize = 3;
/// Parameters per band (they come first, band by band).
pub const BAND_PARAMS: usize = 8;
pub const BAND_NAMES: [&str; BANDS] = ["Low", "Mid", "High"];

/// Per-band parameter offsets.
pub mod band {
    pub const ABOVE_THRESHOLD: usize = 0;
    pub const ABOVE_RATIO: usize = 1;
    pub const BELOW_THRESHOLD: usize = 2;
    pub const BELOW_RATIO: usize = 3;
    pub const GAIN: usize = 4;
    pub const ACTIVE: usize = 5;
    pub const SOLO: usize = 6;
    pub const MUTE: usize = 7;
}

/// Index of the first global parameter.
pub const GLOBAL: usize = BANDS * BAND_PARAMS;

/// Total gain change is limited to this range (dB).
const MAX_CUT_DB: f32 = -60.0;
const MAX_BOOST_DB: f32 = 30.0;

device_params! {
    pub struct MultibandParams {
        lo_above_thr: "Low Above", -60.0, 0.0, -12.0, LIN, Unit::Db;
        lo_above_ratio: "Low Above Ratio", 0.5, 20.0, 1.0, LOG, Unit::Ratio;
        lo_below_thr: "Low Below", -80.0, 0.0, -40.0, LIN, Unit::Db;
        lo_below_ratio: "Low Below Ratio", 0.5, 20.0, 1.0, LOG, Unit::Ratio;
        lo_gain: "Low Gain", -24.0, 24.0, 0.0, LIN, Unit::Db;
        lo_active: "Low On", 0.0, 1.0, 1.0, TOG, Unit::Plain;
        lo_solo: "Low Solo", 0.0, 1.0, 0.0, TOG, Unit::Plain;
        lo_mute: "Low Mute", 0.0, 1.0, 0.0, TOG, Unit::Plain;
        mid_above_thr: "Mid Above", -60.0, 0.0, -12.0, LIN, Unit::Db;
        mid_above_ratio: "Mid Above Ratio", 0.5, 20.0, 1.0, LOG, Unit::Ratio;
        mid_below_thr: "Mid Below", -80.0, 0.0, -40.0, LIN, Unit::Db;
        mid_below_ratio: "Mid Below Ratio", 0.5, 20.0, 1.0, LOG, Unit::Ratio;
        mid_gain: "Mid Gain", -24.0, 24.0, 0.0, LIN, Unit::Db;
        mid_active: "Mid On", 0.0, 1.0, 1.0, TOG, Unit::Plain;
        mid_solo: "Mid Solo", 0.0, 1.0, 0.0, TOG, Unit::Plain;
        mid_mute: "Mid Mute", 0.0, 1.0, 0.0, TOG, Unit::Plain;
        hi_above_thr: "High Above", -60.0, 0.0, -12.0, LIN, Unit::Db;
        hi_above_ratio: "High Above Ratio", 0.5, 20.0, 1.0, LOG, Unit::Ratio;
        hi_below_thr: "High Below", -80.0, 0.0, -40.0, LIN, Unit::Db;
        hi_below_ratio: "High Below Ratio", 0.5, 20.0, 1.0, LOG, Unit::Ratio;
        hi_gain: "High Gain", -24.0, 24.0, 0.0, LIN, Unit::Db;
        hi_active: "High On", 0.0, 1.0, 1.0, TOG, Unit::Plain;
        hi_solo: "High Solo", 0.0, 1.0, 0.0, TOG, Unit::Plain;
        hi_mute: "High Mute", 0.0, 1.0, 0.0, TOG, Unit::Plain;
        low_xover: "Low Crossover", 40.0, 1000.0, 120.0, LOG, Unit::Hz;
        high_xover: "High Crossover", 1000.0, 15000.0, 2500.0, LOG, Unit::Hz;
        attack: "Attack", 0.1, 200.0, 10.0, LOG, Unit::Ms;
        release: "Release", 5.0, 2000.0, 120.0, LOG, Unit::Ms;
        amount: "Amount", 0.0, 1.0, 1.0, LIN, Unit::Percent;
        knee: "Soft Knee", 0.0, 18.0, 6.0, LIN, Unit::Db;
        output: "Output", -24.0, 24.0, 0.0, LIN, Unit::Db;
    }
}

/// One band's settings, read out of the flat parameter table.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandSettings {
    pub above_thr: f32,
    pub above_ratio: f32,
    pub below_thr: f32,
    pub below_ratio: f32,
    pub gain: f32,
    pub active: bool,
    pub solo: bool,
    pub mute: bool,
}

/// Quadratic soft knee around a break point: 0 well below, `x` well above.
#[inline]
fn knee_curve(x: f32, knee: f32) -> f32 {
    if knee > 0.0 && x.abs() <= knee * 0.5 {
        let t = x + knee * 0.5;
        t * t / (2.0 * knee)
    } else if x > 0.0 {
        x
    } else {
        0.0
    }
}

impl BandSettings {
    /// Static gain change in dB for a band level (before Amount and limits).
    pub fn gain_db(&self, level_db: f32, knee: f32) -> f32 {
        if !self.active {
            return 0.0;
        }
        let sa = 1.0 / self.above_ratio.clamp(0.5, 20.0) - 1.0;
        let sb = 1.0 / self.below_ratio.clamp(0.5, 20.0) - 1.0;
        let above = sa * knee_curve(level_db - self.above_thr, knee);
        let below = -sb * knee_curve(self.below_thr - level_db, knee);
        above + below
    }
}

impl MultibandParams {
    pub fn index(band: usize, param: usize) -> usize {
        band * BAND_PARAMS + param
    }

    pub fn band(&self, b: usize) -> BandSettings {
        let g = |k: usize| self.get(Self::index(b, k));
        BandSettings {
            above_thr: g(band::ABOVE_THRESHOLD),
            above_ratio: g(band::ABOVE_RATIO),
            below_thr: g(band::BELOW_THRESHOLD),
            below_ratio: g(band::BELOW_RATIO),
            gain: g(band::GAIN),
            active: g(band::ACTIVE) >= 0.5,
            solo: g(band::SOLO) >= 0.5,
            mute: g(band::MUTE) >= 0.5,
        }
    }

    /// Crossover frequencies, kept in order.
    pub fn crossovers(&self) -> (f32, f32) {
        let lo = self.low_xover.clamp(20.0, 1000.0);
        (lo, self.high_xover.clamp(lo.max(1000.0), 20000.0))
    }

    /// Final gain change in dB for a band level, including Amount and limits.
    pub fn band_gain_db(&self, b: usize, level_db: f32) -> f32 {
        let mut g = self.band(b).gain_db(level_db, self.knee.max(0.0)) * self.amount.clamp(0.0, 1.0);
        if g > 0.0 {
            // Do not lift silence and the noise floor indefinitely.
            g *= ((level_db + 90.0) / 20.0).clamp(0.0, 1.0);
        }
        g.clamp(MAX_CUT_DB, MAX_BOOST_DB)
    }

    fn with_band(mut self, b: usize, vals: [(usize, f32); 4]) -> Self {
        for (k, v) in vals {
            self.set(Self::index(b, k), v);
        }
        self
    }
}

impl super::EffectParams for MultibandParams {
    fn presets() -> Vec<(&'static str, Self)> {
        use band::*;
        let d = Self::default();
        let squash = |thr_a: f32, thr_b: f32| {
            [
                (ABOVE_THRESHOLD, thr_a),
                (ABOVE_RATIO, 8.0),
                (BELOW_THRESHOLD, thr_b),
                (BELOW_RATIO, 4.0),
            ]
        };
        let mut ott = Self {
            attack: 2.0,
            release: 80.0,
            amount: 0.6,
            output: -3.0,
            ..d
        };
        ott = ott
            .with_band(0, squash(-20.0, -40.0))
            .with_band(1, squash(-24.0, -42.0))
            .with_band(2, squash(-28.0, -45.0));
        let mut gentle = Self {
            attack: 15.0,
            release: 200.0,
            knee: 10.0,
            ..d
        };
        for b in 0..BANDS {
            gentle = gentle.with_band(
                b,
                [
                    (ABOVE_THRESHOLD, -16.0),
                    (ABOVE_RATIO, 2.0),
                    (BELOW_THRESHOLD, -60.0),
                    (BELOW_RATIO, 1.0),
                ],
            );
        }
        let de_ess = Self {
            high_xover: 5000.0,
            attack: 1.0,
            release: 60.0,
            ..d
        }
        .with_band(
            2,
            [
                (ABOVE_THRESHOLD, -30.0),
                (ABOVE_RATIO, 6.0),
                (BELOW_THRESHOLD, -80.0),
                (BELOW_RATIO, 1.0),
            ],
        );
        let tighten = Self {
            low_xover: 150.0,
            attack: 20.0,
            release: 150.0,
            ..d
        }
        .with_band(
            0,
            [
                (ABOVE_THRESHOLD, -18.0),
                (ABOVE_RATIO, 4.0),
                (BELOW_THRESHOLD, -45.0),
                (BELOW_RATIO, 0.7),
            ],
        );
        vec![
            ("Over The Top", ott),
            ("Gentle Master", gentle),
            ("De-Esser", de_ess),
            ("Tight Low End", tighten),
        ]
    }
}

/// Fourth-order Linkwitz-Riley crossover state for one channel.
#[derive(Default, Clone, Copy)]
struct Splitter {
    lp1: [Biquad; 2],
    hp1: [Biquad; 2],
    lp2: [Biquad; 2],
    hp2: [Biquad; 2],
    ap: Biquad,
}

#[derive(Clone, Copy)]
struct SplitCoefs {
    lp1: Coefs,
    hp1: Coefs,
    lp2: Coefs,
    hp2: Coefs,
    ap2: Coefs,
}

/// Second-order all-pass with Butterworth Q (the sum of an LR4 pair).
pub(crate) fn allpass(freq: f32, sample_rate: f32) -> Coefs {
    let sr = sample_rate as f64;
    let f = (freq as f64).clamp(5.0, sr * 0.49);
    let w = 2.0 * std::f64::consts::PI * f / sr;
    let (sin, cos) = w.sin_cos();
    let alpha = sin / (2.0 * std::f64::consts::FRAC_1_SQRT_2);
    let a0 = 1.0 + alpha;
    Coefs {
        b0: ((1.0 - alpha) / a0) as f32,
        b1: (-2.0 * cos / a0) as f32,
        b2: 1.0,
        a1: (-2.0 * cos / a0) as f32,
        a2: ((1.0 - alpha) / a0) as f32,
    }
}

impl SplitCoefs {
    fn new(lo: f32, hi: f32, sr: f32) -> Self {
        let q = std::f32::consts::FRAC_1_SQRT_2;
        Self {
            lp1: Coefs::design(FilterShape::LowPass, lo, q, 0.0, sr),
            hp1: Coefs::design(FilterShape::HighPass, lo, q, 0.0, sr),
            lp2: Coefs::design(FilterShape::LowPass, hi, q, 0.0, sr),
            hp2: Coefs::design(FilterShape::HighPass, hi, q, 0.0, sr),
            ap2: allpass(hi, sr),
        }
    }
}

impl Splitter {
    #[inline]
    fn split(&mut self, c: &SplitCoefs, x: f32) -> [f32; BANDS] {
        let a = self.lp1[0].process(&c.lp1, x);
        let low = self.lp1[1].process(&c.lp1, a);
        let low = self.ap.process(&c.ap2, low);
        let a = self.hp1[0].process(&c.hp1, x);
        let rest = self.hp1[1].process(&c.hp1, a);
        let a = self.lp2[0].process(&c.lp2, rest);
        let mid = self.lp2[1].process(&c.lp2, a);
        let a = self.hp2[0].process(&c.hp2, rest);
        let high = self.hp2[1].process(&c.hp2, a);
        [low, mid, high]
    }
}

pub struct MultibandDynamics {
    sr: f32,
    split: [Splitter; 2],
    coefs: SplitCoefs,
    freqs: (f32, f32),
    /// Smoothed band levels (linear peak).
    level: [f32; BANDS],
    /// Smoothed output gain per band (linear, includes gain/solo/mute).
    band_gain: [f32; BANDS],
    output: f32,
    /// Gain change per band in the last block (most extreme, dB).
    pub band_gr_db: [f32; BANDS],
    /// Band levels at the end of the last block (dB).
    pub band_level_db: [f32; BANDS],
    meter_db: f32,
}

impl MultibandDynamics {
    pub fn new(sample_rate: f32) -> Self {
        let d = MultibandParams::default();
        Self {
            sr: sample_rate,
            split: [Splitter::default(); 2],
            coefs: SplitCoefs::new(d.low_xover, d.high_xover, sample_rate),
            freqs: (d.low_xover, d.high_xover),
            level: [0.0; BANDS],
            band_gain: [1.0; BANDS],
            output: 1.0,
            band_gr_db: [0.0; BANDS],
            band_level_db: [-120.0; BANDS],
            meter_db: 0.0,
        }
    }

    pub fn reset(&mut self) {
        self.split = [Splitter::default(); 2];
        self.level = [0.0; BANDS];
        self.band_gr_db = [0.0; BANDS];
        self.band_level_db = [-120.0; BANDS];
        self.meter_db = 0.0;
    }

    pub fn process(&mut self, p: &MultibandParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let sr = self.sr;
        let freqs = p.crossovers();
        if freqs != self.freqs {
            self.freqs = freqs;
            self.coefs = SplitCoefs::new(freqs.0, freqs.1, sr);
        }
        let att = super::smoothing(p.attack.clamp(0.05, 1000.0) * 0.001, sr);
        let rel = super::smoothing(p.release.clamp(1.0, 5000.0) * 0.001, sr);
        let smooth = super::smoothing(0.005, sr);
        let bands: [BandSettings; BANDS] = std::array::from_fn(|b| p.band(b));
        let any_solo = bands.iter().any(|b| b.solo);
        let target_gain: [f32; BANDS] = std::array::from_fn(|b| {
            let s = &bands[b];
            if s.mute || (any_solo && !s.solo) {
                0.0
            } else {
                super::db(s.gain.clamp(-24.0, 24.0))
            }
        });
        let out_t = super::db(p.output.clamp(-24.0, 24.0));
        let mut extreme = [0.0f32; BANDS];
        let c = self.coefs;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let bl = self.split[0].split(&c, *l);
            let br = self.split[1].split(&c, *r);
            let (mut ol, mut or) = (0.0, 0.0);
            for b in 0..BANDS {
                let peak = bl[b].abs().max(br[b].abs());
                let coef = if peak > self.level[b] { att } else { rel };
                self.level[b] = peak + (self.level[b] - peak) * coef;
                if self.level[b] < 1e-12 {
                    self.level[b] = 0.0;
                }
                let level_db = if self.level[b] > 1e-6 {
                    20.0 * self.level[b].log10()
                } else {
                    -120.0
                };
                let g_db = p.band_gain_db(b, level_db);
                if g_db.abs() > extreme[b].abs() {
                    extreme[b] = g_db;
                }
                self.band_gain[b] = target_gain[b] + (self.band_gain[b] - target_gain[b]) * smooth;
                let g = super::db(g_db) * self.band_gain[b];
                ol += bl[b] * g;
                or += br[b] * g;
            }
            self.output = out_t + (self.output - out_t) * smooth;
            *l = ol * self.output;
            *r = or * self.output;
        }
        self.band_gr_db = extreme;
        for b in 0..BANDS {
            self.band_level_db[b] = if self.level[b] > 1e-6 {
                20.0 * self.level[b].log10()
            } else {
                -120.0
            };
        }
        self.meter_db = extreme.iter().fold(0.0f32, |m, &g| m.min(g));
    }
}

impl super::EffectRuntime for MultibandDynamics {
    fn meter(&self) -> Option<f32> {
        Some(self.meter_db)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{EffectParams, EffectRuntime};
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48000.0;

    fn sine(freq: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * PI * freq * i as f32 / SR).sin()).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn run(p: &MultibandParams, input: &[f32]) -> (MultibandDynamics, Vec<f32>) {
        let ctx = FxContext::new(SR, 120.0);
        let mut m = MultibandDynamics::new(SR);
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            m.process(p, cl, cr, &ctx);
        }
        (m, l)
    }

    #[test]
    fn neutral_bands_sum_flat() {
        let p = MultibandParams::default();
        for f in [30.0, 80.0, 120.0, 400.0, 1000.0, 2500.0, 6000.0, 15000.0] {
            let x = sine(f, 0.5, 48000);
            let (_, y) = run(&p, &x);
            let ratio_db = 20.0 * (rms(&y[24000..]) / rms(&x[24000..])).log10();
            assert!(ratio_db.abs() < 0.05, "{f} Hz: {ratio_db} dB");
        }
    }

    #[test]
    fn impulse_response_is_an_allpass() {
        // Energy of the impulse response equals the impulse's.
        let p = MultibandParams::default();
        let mut x = vec![0.0f32; 48000];
        x[0] = 1.0;
        let (_, y) = run(&p, &x);
        let e: f32 = y.iter().map(|v| v * v).sum();
        assert!((e - 1.0).abs() < 0.01, "{e}");
    }

    #[test]
    fn compresses_only_the_loud_band() {
        let mut p = MultibandParams::default();
        p.set(MultibandParams::index(2, band::ABOVE_THRESHOLD), -30.0);
        p.set(MultibandParams::index(2, band::ABOVE_RATIO), 10.0);
        // Loud high tone is reduced...
        let x = sine(8000.0, 0.5, 48000);
        let (m, y) = run(&p, &x);
        assert!(rms(&y[24000..]) < rms(&x[24000..]) * 0.3);
        assert!(m.meter().unwrap() < -10.0);
        // ...a low tone is untouched.
        let x = sine(60.0, 0.5, 48000);
        let (m, y) = run(&p, &x);
        assert!((rms(&y[24000..]) / rms(&x[24000..]) - 1.0).abs() < 0.01);
        assert!(m.meter().unwrap() > -0.1);
    }

    #[test]
    fn upward_compression_lifts_quiet_signals() {
        let mut p = MultibandParams::default();
        for b in 0..BANDS {
            p.set(MultibandParams::index(b, band::BELOW_THRESHOLD), -30.0);
            p.set(MultibandParams::index(b, band::BELOW_RATIO), 4.0);
        }
        let x = sine(1000.0, 0.003, 48000); // about -50 dB
        let (_, y) = run(&p, &x);
        let gain_db = 20.0 * (rms(&y[24000..]) / rms(&x[24000..])).log10();
        assert!(gain_db > 10.0, "{gain_db}");
        // Downward expansion (ratio < 1) pushes it down instead.
        for b in 0..BANDS {
            p.set(MultibandParams::index(b, band::BELOW_RATIO), 0.5);
        }
        let (_, y) = run(&p, &x);
        let gain_db = 20.0 * (rms(&y[24000..]) / rms(&x[24000..])).log10();
        assert!(gain_db < -10.0, "{gain_db}");
    }

    #[test]
    fn upward_expansion_above_threshold() {
        let mut p = MultibandParams::default();
        p.set(MultibandParams::index(1, band::ABOVE_THRESHOLD), -20.0);
        p.set(MultibandParams::index(1, band::ABOVE_RATIO), 0.5);
        let x = sine(700.0, 0.3, 48000); // about -10 dB peak
        let (_, y) = run(&p, &x);
        assert!(rms(&y[24000..]) > rms(&x[24000..]) * 2.0);
    }

    #[test]
    fn solo_and_mute() {
        let mut p = MultibandParams::default();
        p.set(MultibandParams::index(0, band::SOLO), 1.0);
        let lo = sine(50.0, 0.5, 48000);
        let hi = sine(9000.0, 0.5, 48000);
        let (_, y) = run(&p, &lo);
        assert!(rms(&y[24000..]) > 0.3);
        let (_, y) = run(&p, &hi);
        assert!(rms(&y[24000..]) < 0.01);
        let mut p = MultibandParams::default();
        p.set(MultibandParams::index(2, band::MUTE), 1.0);
        let (_, y) = run(&p, &hi);
        assert!(rms(&y[24000..]) < 0.01);
        let (_, y) = run(&p, &lo);
        assert!(rms(&y[24000..]) > 0.3);
    }

    #[test]
    fn presets_stay_finite() {
        let mut noise = crate::dsp::Rng::new(5);
        let x: Vec<f32> = (0..20000).map(|_| noise.next_bipolar() * 2.0).collect();
        for (_, p) in MultibandParams::presets() {
            let (_, y) = run(&p, &x);
            assert!(y.iter().all(|v| v.is_finite() && v.abs() < 100.0));
        }
        let silent = vec![0.0f32; 4096];
        let (_, y) = run(&MultibandParams::presets()[0].1, &silent);
        assert!(y.iter().all(|v| *v == 0.0));
    }
}
