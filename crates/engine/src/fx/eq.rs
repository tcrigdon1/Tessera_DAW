//! Eight-band parametric EQ.
//!
//! Each band can be a low or high cut (12 or 48 dB/octave), a shelf, a bell
//! or a notch. Coefficients are recomputed on the audio thread only when a
//! band's settings change.

use serde::{Deserialize, Serialize};

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{FxContext, ParamInfo, ParamKind, Scale, Unit};

pub const BANDS: usize = 8;
const FIELDS: usize = 5;
const MAX_STAGES: usize = 4;

pub const SHAPES: &[&str] = &[
    "Low Cut 48",
    "Low Cut 12",
    "Low Shelf",
    "Bell",
    "Notch",
    "High Shelf",
    "High Cut 12",
    "High Cut 48",
];

/// Band shape indices into [`SHAPES`].
pub mod shape {
    pub const LOW_CUT_48: f32 = 0.0;
    pub const LOW_CUT_12: f32 = 1.0;
    pub const LOW_SHELF: f32 = 2.0;
    pub const BELL: f32 = 3.0;
    pub const NOTCH: f32 = 4.0;
    pub const HIGH_SHELF: f32 = 5.0;
    pub const HIGH_CUT_12: f32 = 6.0;
    pub const HIGH_CUT_48: f32 = 7.0;
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqBand {
    pub on: f32,
    pub shape: f32,
    pub freq: f32,
    pub gain: f32,
    pub q: f32,
}

impl Default for EqBand {
    fn default() -> Self {
        DEFAULT_BANDS[3]
    }
}

impl EqBand {
    const fn new(on: f32, shape: f32, freq: f32) -> Self {
        Self {
            on,
            shape,
            freq,
            gain: 0.0,
            q: 0.71,
        }
    }

    pub fn is_on(&self) -> bool {
        self.on >= 0.5
    }

    /// Whether the gain parameter affects this band's shape.
    pub fn uses_gain(&self) -> bool {
        matches!(self.shape as i32, 2 | 3 | 5)
    }

    /// Filter sections for this band.
    pub fn coefs(&self, sample_rate: f32) -> ([Coefs; MAX_STAGES], usize) {
        let mut c = [Coefs::IDENTITY; MAX_STAGES];
        let resonance = self.q / std::f32::consts::FRAC_1_SQRT_2;
        // Butterworth pole Qs for an 8th-order (48 dB/oct) response.
        const BW8: [f32; 4] = [0.5098, 0.6013, 0.9000, 2.5629];
        let single = |s: FilterShape| Coefs::design(s, self.freq, self.q, self.gain, sample_rate);
        let n = match self.shape.round() as i32 {
            0 | 7 => {
                let s = if self.shape < 1.0 {
                    FilterShape::HighPass
                } else {
                    FilterShape::LowPass
                };
                for (i, q) in BW8.iter().enumerate() {
                    let q = if i == 3 { q * resonance.max(0.2) } else { *q };
                    c[i] = Coefs::design(s, self.freq, q, 0.0, sample_rate);
                }
                4
            }
            1 => {
                c[0] = single(FilterShape::HighPass);
                1
            }
            2 => {
                c[0] = single(FilterShape::LowShelf);
                1
            }
            4 => {
                c[0] = single(FilterShape::Notch);
                1
            }
            5 => {
                c[0] = single(FilterShape::HighShelf);
                1
            }
            6 => {
                c[0] = single(FilterShape::LowPass);
                1
            }
            _ => {
                c[0] = single(FilterShape::Bell);
                1
            }
        };
        (c, n)
    }
}

const DEFAULT_BANDS: [EqBand; BANDS] = [
    EqBand::new(0.0, shape::LOW_CUT_48, 30.0),
    EqBand::new(1.0, shape::LOW_SHELF, 100.0),
    EqBand::new(1.0, shape::BELL, 250.0),
    EqBand::new(1.0, shape::BELL, 600.0),
    EqBand::new(1.0, shape::BELL, 1500.0),
    EqBand::new(1.0, shape::BELL, 3500.0),
    EqBand::new(1.0, shape::HIGH_SHELF, 8000.0),
    EqBand::new(0.0, shape::HIGH_CUT_12, 18000.0),
];

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqParams {
    pub bands: [EqBand; BANDS],
    /// Output gain in dB.
    pub output: f32,
}

impl Default for EqParams {
    fn default() -> Self {
        Self {
            bands: DEFAULT_BANDS,
            output: 0.0,
        }
    }
}

const fn build_info() -> [ParamInfo; BANDS * FIELDS + 1] {
    let mut out = [ParamInfo::new(
        "Output",
        -24.0,
        24.0,
        0.0,
        ParamKind::Continuous(Scale::Linear),
        Unit::Db,
    ); BANDS * FIELDS + 1];
    let mut b = 0;
    while b < BANDS {
        let d = DEFAULT_BANDS[b];
        out[b * FIELDS] = ParamInfo::new("On", 0.0, 1.0, d.on, ParamKind::Toggle, Unit::Plain);
        out[b * FIELDS + 1] = ParamInfo::new("Type", 0.0, 7.0, d.shape, ParamKind::Choice(SHAPES), Unit::Plain);
        out[b * FIELDS + 2] = ParamInfo::new(
            "Freq",
            10.0,
            22000.0,
            d.freq,
            ParamKind::Continuous(Scale::Log),
            Unit::Hz,
        );
        out[b * FIELDS + 3] = ParamInfo::new(
            "Gain",
            -15.0,
            15.0,
            d.gain,
            ParamKind::Continuous(Scale::Linear),
            Unit::Db,
        );
        out[b * FIELDS + 4] = ParamInfo::new("Q", 0.1, 18.0, d.q, ParamKind::Continuous(Scale::Log), Unit::Plain);
        b += 1;
    }
    out
}

const INFO_TABLE: [ParamInfo; BANDS * FIELDS + 1] = build_info();

impl EqParams {
    pub const INFO: &'static [ParamInfo] = &INFO_TABLE;

    /// Parameter index of a band field (0 = on, 1 = type, 2 = freq, 3 = gain, 4 = q).
    pub const fn index(band: usize, field: usize) -> usize {
        band * FIELDS + field
    }

    pub const OUTPUT: usize = BANDS * FIELDS;

    pub fn get(&self, index: usize) -> f32 {
        if index == Self::OUTPUT {
            return self.output;
        }
        let Some(b) = self.bands.get(index / FIELDS) else {
            return 0.0;
        };
        match index % FIELDS {
            0 => b.on,
            1 => b.shape,
            2 => b.freq,
            3 => b.gain,
            _ => b.q,
        }
    }

    pub fn set(&mut self, index: usize, value: f32) {
        let Some(info) = Self::INFO.get(index) else { return };
        let v = info.clamp(value);
        if index == Self::OUTPUT {
            self.output = v;
            return;
        }
        let b = &mut self.bands[index / FIELDS];
        match index % FIELDS {
            0 => b.on = v,
            1 => b.shape = v,
            2 => b.freq = v,
            3 => b.gain = v,
            _ => b.q = v,
        }
    }

    /// Overall magnitude response in dB at `freq` (for drawing the curve).
    pub fn response_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let mut g = 1.0f32;
        for b in self.bands.iter().filter(|b| b.is_on()) {
            let (c, n) = b.coefs(sample_rate);
            for s in &c[..n] {
                g *= s.magnitude(freq, sample_rate);
            }
        }
        20.0 * g.max(1e-9).log10() + self.output
    }
}

struct BandState {
    cached: Option<EqBand>,
    coefs: [Coefs; MAX_STAGES],
    stages: usize,
    filters: [[Biquad; MAX_STAGES]; 2],
}

pub struct Eq {
    sr: f32,
    bands: [BandState; BANDS],
    gain: f32,
}

impl Eq {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            bands: std::array::from_fn(|_| BandState {
                cached: None,
                coefs: [Coefs::IDENTITY; MAX_STAGES],
                stages: 0,
                filters: [[Biquad::default(); MAX_STAGES]; 2],
            }),
            gain: 1.0,
        }
    }

    pub fn reset(&mut self) {
        for b in &mut self.bands {
            for ch in &mut b.filters {
                for f in ch {
                    f.reset();
                }
            }
        }
    }

    pub fn process(&mut self, p: &EqParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        for (state, band) in self.bands.iter_mut().zip(p.bands.iter()) {
            if !band.is_on() {
                if state.cached.is_some() {
                    state.cached = None;
                    for ch in &mut state.filters {
                        for f in ch {
                            f.reset();
                        }
                    }
                }
                continue;
            }
            if state.cached != Some(*band) {
                let (c, n) = band.coefs(self.sr);
                state.coefs = c;
                state.stages = n;
                state.cached = Some(*band);
            }
            let n = state.stages;
            let [fl, fr] = &mut state.filters;
            for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                for s in 0..n {
                    *l = fl[s].process(&state.coefs[s], *l);
                    *r = fr[s].process(&state.coefs[s], *r);
                }
            }
        }
        let target = super::db(p.output);
        let k = 1.0 - super::smoothing(0.01, self.sr);
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            self.gain += (target - self.gain) * k;
            *l *= self.gain;
            *r *= self.gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn sine_gain(eq: &mut Eq, p: &EqParams, freq: f32) -> f32 {
        let ctx = FxContext::new(SR, 120.0);
        let n = 4800;
        let mut peak_in = 0.0f32;
        let mut peak_out = 0.0f32;
        for block in 0..20 {
            let mut l = [0.0f32; 480];
            let mut r = [0.0f32; 480];
            for (i, s) in l.iter_mut().enumerate() {
                *s = (2.0 * std::f32::consts::PI * freq * (block * 480 + i) as f32 / SR).sin() * 0.5;
            }
            r.copy_from_slice(&l);
            eq.process(p, &mut l, &mut r, &ctx);
            if block * 480 > n {
                peak_in = 0.5;
                peak_out = l.iter().fold(peak_out, |a, b| a.max(b.abs()));
            }
        }
        20.0 * (peak_out / peak_in).log10()
    }

    #[test]
    fn flat_by_default() {
        let p = EqParams::default();
        for f in [40.0, 500.0, 5000.0] {
            let mut eq = Eq::new(SR);
            assert!(sine_gain(&mut eq, &p, f).abs() < 0.1, "{f}");
            assert!(p.response_db(f, SR).abs() < 0.01);
        }
    }

    #[test]
    fn bell_boost_and_low_cut() {
        let mut p = EqParams::default();
        p.set(EqParams::index(4, 3), 9.0); // band 5 bell at 1.5 kHz
        let mut eq = Eq::new(SR);
        let g = sine_gain(&mut eq, &p, 1500.0);
        assert!((g - 9.0).abs() < 0.3, "{g}");
        assert!((p.response_db(1500.0, SR) - 9.0).abs() < 0.1);

        let mut p = EqParams::default();
        p.set(EqParams::index(0, 0), 1.0);
        p.set(EqParams::index(0, 2), 200.0);
        let mut eq = Eq::new(SR);
        assert!(sine_gain(&mut eq, &p, 50.0) < -40.0, "48 dB/oct low cut");
        let mut eq = Eq::new(SR);
        assert!(sine_gain(&mut eq, &p, 2000.0).abs() < 0.3);
    }

    #[test]
    fn index_accessors() {
        let mut p = EqParams::default();
        p.set(EqParams::index(2, 2), 440.0);
        assert_eq!(p.bands[2].freq, 440.0);
        assert_eq!(p.get(EqParams::index(2, 2)), 440.0);
        p.set(EqParams::OUTPUT, -3.0);
        assert_eq!(p.output, -3.0);
        assert_eq!(EqParams::INFO.len(), 41);
    }
}
