//! Second-order IIR filter sections (RBJ "Audio EQ Cookbook" designs) used by
//! the EQ, the delay's feedback filters and the reverb's input filters.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterShape {
    HighPass,
    LowPass,
    LowShelf,
    HighShelf,
    Bell,
    Notch,
}

/// Normalised biquad coefficients (a0 = 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl Default for Coefs {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Coefs {
    pub const IDENTITY: Coefs = Coefs {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// Designs a filter. `freq` is clamped below Nyquist; `gain_db` only
    /// matters for shelves and bells.
    pub fn design(shape: FilterShape, freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> Coefs {
        let sr = sample_rate as f64;
        let f = (freq as f64).clamp(5.0, sr * 0.49);
        let q = (q as f64).max(0.025);
        let w = 2.0 * PI * f / sr;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a = 10f64.powf(gain_db as f64 / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match shape {
            FilterShape::LowPass => {
                let b1 = 1.0 - cos;
                (b1 / 2.0, b1, b1 / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            FilterShape::HighPass => {
                let b1 = -(1.0 + cos);
                (-b1 / 2.0, b1, -b1 / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            FilterShape::Bell => (
                1.0 + alpha * a,
                -2.0 * cos,
                1.0 - alpha * a,
                1.0 + alpha / a,
                -2.0 * cos,
                1.0 - alpha / a,
            ),
            FilterShape::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
            FilterShape::LowShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + s),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - s),
                    (a + 1.0) + (a - 1.0) * cos + s,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - s,
                )
            }
            FilterShape::HighShelf => {
                let s = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + s),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - s),
                    (a + 1.0) - (a - 1.0) * cos + s,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - s,
                )
            }
        };
        Coefs {
            b0: (b0 / a0) as f32,
            b1: (b1 / a0) as f32,
            b2: (b2 / a0) as f32,
            a1: (a1 / a0) as f32,
            a2: (a2 / a0) as f32,
        }
    }

    /// Magnitude response at `freq` (linear gain). Used for drawing curves and
    /// in tests.
    pub fn magnitude(&self, freq: f32, sample_rate: f32) -> f32 {
        let w = 2.0 * PI * freq as f64 / sample_rate as f64;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let (b0, b1, b2) = (self.b0 as f64, self.b1 as f64, self.b2 as f64);
        let (a1, a2) = (self.a1 as f64, self.a2 as f64);
        // H(e^jw) with z^-1 = e^-jw.
        let nr = b0 + b1 * c1 + b2 * c2;
        let ni = -(b1 * s1 + b2 * s2);
        let dr = 1.0 + a1 * c1 + a2 * c2;
        let di = -(a1 * s1 + a2 * s2);
        (((nr * nr + ni * ni) / (dr * dr + di * di).max(1e-30)).sqrt()) as f32
    }
}

/// One filter section's state for one channel (transposed direct form II).
#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
    z1: f32,
    z2: f32,
}

impl Biquad {
    #[inline]
    pub fn process(&mut self, c: &Coefs, x: f32) -> f32 {
        let y = c.b0 * x + self.z1;
        self.z1 = c.b1 * x - c.a1 * y + self.z2;
        self.z2 = c.b2 * x - c.a2 * y;
        // Flush denormals so long silent tails stay cheap.
        if self.z1.abs() < 1e-20 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-20 {
            self.z2 = 0.0;
        }
        y
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// A one-pole low-pass, used for damping inside feedback loops.
#[derive(Debug, Clone, Copy, Default)]
pub struct OnePole {
    z: f32,
}

impl OnePole {
    /// `coef` is the feedback coefficient (0 = no filtering).
    #[inline]
    pub fn lowpass(&mut self, x: f32, coef: f32) -> f32 {
        self.z = x + (self.z - x) * coef;
        if self.z.abs() < 1e-20 {
            self.z = 0.0;
        }
        self.z
    }

    pub fn reset(&mut self) {
        self.z = 0.0;
    }
}

/// Coefficient for a one-pole low-pass with the given cutoff.
pub fn one_pole_coef(cutoff: f32, sample_rate: f32) -> f32 {
    (-2.0 * std::f32::consts::PI * cutoff.clamp(1.0, sample_rate * 0.49) / sample_rate).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48000.0;

    fn db(g: f32) -> f32 {
        20.0 * g.log10()
    }

    #[test]
    fn bell_has_its_gain_at_the_centre() {
        let c = Coefs::design(FilterShape::Bell, 1000.0, 1.0, 6.0, SR);
        assert!((db(c.magnitude(1000.0, SR)) - 6.0).abs() < 0.05);
        assert!(db(c.magnitude(50.0, SR)).abs() < 0.2);
        assert!(db(c.magnitude(15000.0, SR)).abs() < 0.2);
    }

    #[test]
    fn cuts_and_shelves() {
        let hp = Coefs::design(FilterShape::HighPass, 200.0, 0.7071, 0.0, SR);
        assert!((db(hp.magnitude(200.0, SR)) + 3.0).abs() < 0.1);
        assert!(db(hp.magnitude(20.0, SR)) < -35.0);
        assert!(db(hp.magnitude(5000.0, SR)).abs() < 0.1);
        let ls = Coefs::design(FilterShape::LowShelf, 300.0, 0.7071, -9.0, SR);
        assert!((db(ls.magnitude(20.0, SR)) + 9.0).abs() < 0.2);
        assert!(db(ls.magnitude(10000.0, SR)).abs() < 0.2);
        let n = Coefs::design(FilterShape::Notch, 1000.0, 2.0, 0.0, SR);
        assert!(db(n.magnitude(1000.0, SR)) < -40.0);
    }

    #[test]
    fn processing_matches_the_designed_response() {
        let c = Coefs::design(FilterShape::LowPass, 1000.0, 0.7071, 0.0, SR);
        let mut f = Biquad::default();
        let freq = 4000.0;
        let mut peak = 0.0f32;
        for i in 0..48000 {
            let x = (2.0 * std::f32::consts::PI * freq * i as f32 / SR).sin();
            let y = f.process(&c, x);
            if i > 24000 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak - c.magnitude(freq, SR)).abs() < 0.01, "{peak}");
    }
}
