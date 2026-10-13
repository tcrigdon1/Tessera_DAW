//! Zero-delay-feedback state variable filter (trapezoidal integration).

#[derive(Debug, Clone, Default)]
pub struct Svf {
    ic1: f32,
    ic2: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct SvfOut {
    pub low: f32,
    pub band: f32,
    pub high: f32,
}

impl Svf {
    pub fn reset(&mut self) {
        self.ic1 = 0.0;
        self.ic2 = 0.0;
    }

    /// `cutoff` in Hz, `q` > 0.5. Returns low/band/high outputs.
    #[inline]
    pub fn process(&mut self, x: f32, cutoff: f32, q: f32, sample_rate: f32) -> SvfOut {
        let fc = cutoff.clamp(20.0, sample_rate * 0.45);
        let g = (std::f32::consts::PI * fc / sample_rate).tan();
        let k = 1.0 / q.max(0.5);
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        let v3 = x - self.ic2;
        let v1 = a1 * self.ic1 + a2 * v3;
        let v2 = self.ic2 + a2 * self.ic1 + a3 * v3;
        self.ic1 = 2.0 * v1 - self.ic1;
        self.ic2 = 2.0 * v2 - self.ic2;
        SvfOut {
            low: v2,
            band: v1,
            high: x - k * v1 - v2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_through(freq: f32, cutoff: f32) -> f32 {
        let sr = 48000.0;
        let mut f = Svf::default();
        let mut acc = 0.0;
        let n = 48000;
        for i in 0..n {
            let x = (i as f32 * freq * std::f32::consts::TAU / sr).sin();
            let y = f.process(x, cutoff, 0.707, sr).low;
            if i > n / 2 {
                acc += y * y;
            }
        }
        (acc / (n / 2) as f32).sqrt()
    }

    #[test]
    fn lowpass_attenuates_highs() {
        let pass = rms_through(100.0, 1000.0);
        let stop = rms_through(10000.0, 1000.0);
        assert!(pass > 0.6, "pass {pass}");
        assert!(stop < 0.02, "stop {stop}");
    }
}
