//! Algorithmic stereo reverb.
//!
//! Pre-delay, a low cut and four series all-pass diffusers feed an eight-line
//! feedback delay network with a Householder mixing matrix. Every line has a
//! damping low-pass and a gain set from the decay (RT60) time, so the tail
//! decays by 60 dB in `decay` seconds regardless of the size setting.

use super::biquad::{Biquad, Coefs, FilterShape, OnePole};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

device_params! {
    pub struct ReverbParams {
        predelay: "Pre-Delay", 0.0, 250.0, 15.0, LIN, Unit::Ms;
        size: "Size", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        decay: "Decay", 0.2, 20.0, 2.2, LOG, Unit::Seconds;
        damping: "High Cut", 1000.0, 20000.0, 7000.0, LOG, Unit::Hz;
        low_cut: "Low Cut", 20.0, 1000.0, 120.0, LOG, Unit::Hz;
        diffusion: "Diffusion", 0.0, 1.0, 0.75, LIN, Unit::Percent;
        width: "Width", 0.0, 1.0, 1.0, LIN, Unit::Percent;
        mix: "Dry/Wet", 0.0, 1.0, 0.3, LIN, Unit::Percent;
    }
}

const LINES: usize = 8;
/// Base line lengths in milliseconds (mutually prime-ish).
const LINE_MS: [f32; LINES] = [29.7, 37.1, 41.3, 47.9, 53.3, 59.9, 67.1, 73.1];
const DIFFUSER_MS: [f32; 4] = [4.7, 3.6, 12.7, 9.3];
const MAX_PREDELAY_MS: f32 = 250.0;
/// Size 0..1 scales the lines by 0.5..1.5.
const MAX_SCALE: f32 = 1.5;

struct Line {
    buf: Vec<f32>,
    pos: usize,
}

impl Line {
    fn new(len: usize) -> Self {
        Self {
            buf: vec![0.0; len.max(2)],
            pos: 0,
        }
    }

    /// Sample written `delay` samples ago (1 ..= capacity).
    #[inline]
    fn read(&self, delay: usize) -> f32 {
        let n = self.buf.len();
        let d = delay.clamp(1, n);
        self.buf[(self.pos + n - d) % n]
    }

    #[inline]
    fn write(&mut self, x: f32) {
        self.buf[self.pos] = x;
        self.pos += 1;
        if self.pos == self.buf.len() {
            self.pos = 0;
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

pub struct Reverb {
    sr: f32,
    pre: [Line; 2],
    diffusers: [[Line; 4]; 2],
    lines: [Line; LINES],
    damp: [OnePole; LINES],
    low_cut: [Biquad; 2],
    low_cut_coefs: Coefs,
    low_cut_freq: f32,
}

fn ms(sr: f32, v: f32) -> usize {
    (v * 0.001 * sr).round() as usize
}

impl Reverb {
    pub fn new(sample_rate: f32) -> Self {
        let sr = sample_rate;
        let pre = || Line::new(ms(sr, MAX_PREDELAY_MS) + 2);
        // The right channel's diffusers are slightly longer for decorrelation.
        let diff = |spread: f32| std::array::from_fn(|i| Line::new(ms(sr, DIFFUSER_MS[i] * spread) + 2));
        Self {
            sr,
            pre: [pre(), pre()],
            diffusers: [diff(1.0), diff(1.07)],
            lines: std::array::from_fn(|i| Line::new(ms(sr, LINE_MS[i] * MAX_SCALE) + 2)),
            damp: [OnePole::default(); LINES],
            low_cut: [Biquad::default(); 2],
            low_cut_coefs: Coefs::IDENTITY,
            low_cut_freq: -1.0,
        }
    }

    pub fn reset(&mut self) {
        for l in self.pre.iter_mut().chain(self.lines.iter_mut()) {
            l.clear();
        }
        for ch in &mut self.diffusers {
            for d in ch {
                d.clear();
            }
        }
        for d in &mut self.damp {
            d.reset();
        }
        for f in &mut self.low_cut {
            f.reset();
        }
    }

    pub fn process(&mut self, p: &ReverbParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let sr = self.sr;
        if p.low_cut != self.low_cut_freq {
            self.low_cut_freq = p.low_cut;
            self.low_cut_coefs = Coefs::design(FilterShape::HighPass, p.low_cut, 0.7071, 0.0, sr);
        }
        let scale = 0.5 + p.size.clamp(0.0, 1.0);
        let mut len = [0usize; LINES];
        let mut gain = [0.0f32; LINES];
        for i in 0..LINES {
            len[i] = ms(sr, LINE_MS[i] * scale).max(1);
            // -60 dB after `decay` seconds.
            gain[i] = 10f32.powf(-3.0 * len[i] as f32 / (p.decay.max(0.05) * sr));
        }
        let damp = super::biquad::one_pole_coef(p.damping, sr);
        let pre = ms(sr, p.predelay).max(1);
        let ap = 0.7 * p.diffusion.clamp(0.0, 1.0);
        let diff_len: [[usize; 4]; 2] = [
            std::array::from_fn(|i| self.diffusers[0][i].buf.len() - 2),
            std::array::from_fn(|i| self.diffusers[1][i].buf.len() - 2),
        ];
        let wet = p.mix;
        let dry = 1.0 - wet;
        let w = p.width.clamp(0.0, 1.0);

        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let input = [*l, *r];
            let mut x = [0.0f32; 2];
            for ch in 0..2 {
                self.pre[ch].write(input[ch]);
                let mut s = self.pre[ch].read(pre);
                s = self.low_cut[ch].process(&self.low_cut_coefs, s);
                for (d, &dl) in self.diffusers[ch].iter_mut().zip(diff_len[ch].iter()) {
                    // Schroeder all-pass: y = -g*x + z; z' = x + g*y.
                    let z = d.read(dl);
                    let v = s + ap * z;
                    let y = z - ap * v;
                    d.write(v);
                    s = y;
                }
                x[ch] = s;
            }

            // Read every line, damp, mix through the Householder matrix.
            let mut out = [0.0f32; LINES];
            let mut sum = 0.0f32;
            for i in 0..LINES {
                let v = self.damp[i].lowpass(self.lines[i].read(len[i]), damp) * gain[i];
                out[i] = v;
                sum += v;
            }
            let h = sum * (2.0 / LINES as f32);
            for i in 0..LINES {
                let inject = if i % 2 == 0 { x[0] } else { x[1] };
                self.lines[i].write(out[i] - h + inject * 0.35);
            }

            let wl = out[0] - out[2] + out[4] - out[6];
            let wr = out[1] - out[3] + out[5] - out[7];
            // Width: blend towards mono.
            let mid = (wl + wr) * 0.5;
            let wl = mid + (wl - mid) * w;
            let wr = mid + (wr - mid) * w;
            *l = input[0] * dry + wl * wet;
            *r = input[1] * dry + wr * wet;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn energy(buf: &[f32]) -> f32 {
        buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32
    }

    #[test]
    fn impulse_produces_a_decaying_stereo_tail() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = ReverbParams {
            mix: 1.0,
            decay: 1.0,
            predelay: 0.0,
            ..Default::default()
        };
        let mut rv = Reverb::new(sr);
        let secs = 3;
        let mut l = vec![0.0f32; sr as usize * secs];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            rv.process(&p, cl, cr, &ctx);
        }
        let win = sr as usize / 10;
        let early = energy(&l[win..2 * win]);
        let at_1s = energy(&l[sr as usize..sr as usize + win]);
        assert!(early > 1e-7, "tail present");
        // RT60 = 1 s: about 60 dB (1e-6 in energy) down after a second, give or take.
        let drop_db = 10.0 * (at_1s / early).log10();
        assert!(drop_db < -40.0 && drop_db > -80.0, "drop {drop_db} dB");
        assert!(l.iter().chain(r.iter()).all(|x| x.is_finite() && x.abs() < 2.0));
        // Decorrelated channels.
        assert!(l[win..2 * win] != r[win..2 * win]);
    }

    #[test]
    fn dry_passes_through() {
        let sr = 44100.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = ReverbParams {
            mix: 0.0,
            ..Default::default()
        };
        let mut rv = Reverb::new(sr);
        let mut l = [0.25f32; 64];
        let mut r = [-0.5f32; 64];
        rv.process(&p, &mut l, &mut r, &ctx);
        assert!(l.iter().all(|&x| x == 0.25) && r.iter().all(|&x| x == -0.5));
    }
}
