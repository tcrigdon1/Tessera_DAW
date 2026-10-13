//! Chorus / flanger: a short delay modulated by a sine LFO, with the left
//! and right LFOs offset in phase for stereo width.

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

device_params! {
    pub struct ChorusParams {
        rate: "Rate", 0.05, 10.0, 0.6, LOG, Unit::Hz;
        depth: "Depth", 0.0, 8.0, 2.5, LIN, Unit::Ms;
        delay: "Delay", 0.5, 30.0, 7.0, LOG, Unit::Ms;
        feedback: "Feedback", -0.95, 0.95, 0.0, LIN, Unit::Percent;
        spread: "Spread", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        mix: "Dry/Wet", 0.0, 1.0, 0.5, LIN, Unit::Percent;
    }
}

const MAX_MS: f32 = 40.0;

pub struct Chorus {
    sr: f32,
    buf: [Vec<f32>; 2],
    pos: usize,
    phase: f32,
}

impl Chorus {
    pub fn new(sample_rate: f32) -> Self {
        let n = (MAX_MS * 0.001 * sample_rate) as usize + 4;
        Self {
            sr: sample_rate,
            buf: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            phase: 0.0,
        }
    }

    pub fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
    }

    pub fn process(&mut self, p: &ChorusParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let n = self.buf[0].len();
        let ms = self.sr * 0.001;
        let base = p.delay * ms;
        let depth = p.depth * ms * 0.5;
        let inc = p.rate / self.sr;
        let offset = p.spread.clamp(0.0, 1.0) * 0.5;
        let fb = p.feedback.clamp(-0.95, 0.95);
        let wet = p.mix;
        let dry = 1.0 - wet * 0.5;
        let max_d = (n - 3) as f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let input = [*l, *r];
            let mut out = [0.0f32; 2];
            for c in 0..2 {
                let ph = self.phase + if c == 1 { offset } else { 0.0 };
                let lfo = (ph * std::f32::consts::TAU).sin();
                let d = (base + depth * (1.0 + lfo)).clamp(1.0, max_d);
                let i = d as usize;
                let frac = d - i as f32;
                let b = &self.buf[c];
                let a = b[(self.pos + n - i) % n];
                let bb = b[(self.pos + n - i - 1) % n];
                let v = a + (bb - a) * frac;
                out[c] = v;
                self.buf[c][self.pos] = input[c] + v * fb;
            }
            self.pos += 1;
            if self.pos == n {
                self.pos = 0;
            }
            self.phase += inc;
            if self.phase >= 1.0 {
                self.phase -= 1.0;
            }
            *l = input[0] * dry + out[0] * wet;
            *r = input[1] * dry + out[1] * wet;
        }
    }
}

impl super::EffectParams for ChorusParams {}

impl super::EffectRuntime for Chorus {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_modulated_and_stereo() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = ChorusParams {
            mix: 1.0,
            rate: 2.0,
            ..Default::default()
        };
        let mut c = Chorus::new(sr);
        let mut l: Vec<f32> = (0..48000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let mut r = l.clone();
        for (a, b) in l.chunks_mut(300).zip(r.chunks_mut(300)) {
            c.process(&p, a, b, &ctx);
        }
        assert!(l.iter().all(|x| x.is_finite() && x.abs() < 1.5));
        let diff: f32 = l.iter().zip(r.iter()).map(|(a, b)| (a - b).abs()).sum();
        assert!(diff > 100.0, "left and right differ: {diff}");
    }
}
