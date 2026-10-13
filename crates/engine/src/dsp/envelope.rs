//! Linear-attack, exponential decay/release ADSR envelope.

#[derive(Debug, Clone, Copy, PartialEq)]
enum Stage {
    Idle,
    Attack,
    Decay,
    Sustain,
    Release,
}

#[derive(Debug, Clone)]
pub struct Adsr {
    stage: Stage,
    value: f32,
    sample_rate: f32,
}

impl Adsr {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            stage: Stage::Idle,
            value: 0.0,
            sample_rate,
        }
    }

    pub fn trigger(&mut self) {
        self.stage = Stage::Attack;
    }

    pub fn release(&mut self) {
        if self.stage != Stage::Idle {
            self.stage = Stage::Release;
        }
    }

    pub fn reset(&mut self) {
        self.stage = Stage::Idle;
        self.value = 0.0;
    }

    pub fn is_active(&self) -> bool {
        self.stage != Stage::Idle
    }

    pub fn is_releasing(&self) -> bool {
        self.stage == Stage::Release
    }

    pub fn value(&self) -> f32 {
        self.value
    }

    /// Advances one sample. Times are in seconds, sustain is 0..1.
    #[inline]
    pub fn next(&mut self, attack: f32, decay: f32, sustain: f32, release: f32) -> f32 {
        let sr = self.sample_rate;
        match self.stage {
            Stage::Idle => {}
            Stage::Attack => {
                let step = 1.0 / (attack.max(0.0005) * sr);
                self.value += step;
                if self.value >= 1.0 {
                    self.value = 1.0;
                    self.stage = Stage::Decay;
                }
            }
            Stage::Decay => {
                let coef = exp_coef(decay, sr);
                self.value = sustain + (self.value - sustain) * coef;
                if (self.value - sustain).abs() < 1e-4 {
                    self.value = sustain;
                    self.stage = Stage::Sustain;
                }
            }
            Stage::Sustain => {
                self.value = sustain;
            }
            Stage::Release => {
                let coef = exp_coef(release, sr);
                self.value *= coef;
                if self.value < 1e-4 {
                    self.value = 0.0;
                    self.stage = Stage::Idle;
                }
            }
        }
        self.value
    }
}

/// One-pole coefficient that reaches ~-60 dB after `time` seconds.
#[inline]
pub fn exp_coef(time: f32, sample_rate: f32) -> f32 {
    (-6.9 / (time.max(0.001) * sample_rate)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_cycle() {
        let mut e = Adsr::new(1000.0);
        e.trigger();
        let mut peak: f32 = 0.0;
        for _ in 0..200 {
            peak = peak.max(e.next(0.01, 0.05, 0.5, 0.05));
        }
        assert!((peak - 1.0).abs() < 1e-6);
        assert!((e.value() - 0.5).abs() < 0.01);
        e.release();
        for _ in 0..1000 {
            e.next(0.01, 0.05, 0.5, 0.05);
        }
        assert!(!e.is_active());
    }
}
