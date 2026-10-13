//! Stereo delay with tempo sync, ping-pong and filtered feedback.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const DIVISIONS: &[&str] = &[
    "1/32", "1/16T", "1/16", "1/16D", "1/8T", "1/8", "1/8D", "1/4T", "1/4", "1/4D", "1/2", "1/1",
];
/// Length of each division in beats.
const DIVISION_BEATS: [f64; 12] = [
    0.125,
    1.0 / 6.0,
    0.25,
    0.375,
    1.0 / 3.0,
    0.5,
    0.75,
    2.0 / 3.0,
    1.0,
    1.5,
    2.0,
    4.0,
];
const DIV: ParamKind = ParamKind::Choice(DIVISIONS);
/// Longest delay time in seconds.
pub const MAX_SECONDS: f32 = 5.0;

device_params! {
    pub struct DelayParams {
        sync: "Sync", 0.0, 1.0, 1.0, ParamKind::Toggle, Unit::Plain;
        div_l: "Left", 0.0, 11.0, 5.0, DIV, Unit::Plain;
        div_r: "Right", 0.0, 11.0, 6.0, DIV, Unit::Plain;
        time_l: "Left", 1.0, 5000.0, 250.0, LOG, Unit::Ms;
        time_r: "Right", 1.0, 5000.0, 375.0, LOG, Unit::Ms;
        feedback: "Feedback", 0.0, 0.95, 0.4, LIN, Unit::Percent;
        ping_pong: "Ping Pong", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        low_cut: "Low Cut", 20.0, 2000.0, 120.0, LOG, Unit::Hz;
        high_cut: "High Cut", 1000.0, 20000.0, 6500.0, LOG, Unit::Hz;
        mix: "Dry/Wet", 0.0, 1.0, 0.3, LIN, Unit::Percent;
    }
}

impl DelayParams {
    pub fn is_relevant(&self, index: usize) -> bool {
        let synced = self.sync >= 0.5;
        match index {
            1 | 2 => synced,
            3 | 4 => !synced,
            _ => true,
        }
    }

    /// Delay times in seconds for the left and right channel.
    pub fn times(&self, tempo: f64) -> (f32, f32) {
        if self.sync >= 0.5 {
            let spb = 60.0 / tempo.max(1.0);
            let t = |d: f32| (DIVISION_BEATS[(d.round().max(0.0) as usize).min(11)] * spb) as f32;
            (t(self.div_l), t(self.div_r))
        } else {
            (self.time_l * 0.001, self.time_r * 0.001)
        }
    }
}

pub struct Delay {
    sr: f32,
    buf: [Vec<f32>; 2],
    pos: usize,
    /// Current (gliding) delay time per channel, in samples.
    cur: [f32; 2],
    filters: [[Biquad; 2]; 2],
    cached: (f32, f32),
    lc: Coefs,
    hc: Coefs,
}

impl Delay {
    pub fn new(sample_rate: f32) -> Self {
        let n = (MAX_SECONDS * sample_rate) as usize + 4;
        Self {
            sr: sample_rate,
            buf: [vec![0.0; n], vec![0.0; n]],
            pos: 0,
            cur: [-1.0; 2],
            filters: [[Biquad::default(); 2]; 2],
            cached: (-1.0, -1.0),
            lc: Coefs::IDENTITY,
            hc: Coefs::IDENTITY,
        }
    }

    pub fn reset(&mut self) {
        for b in &mut self.buf {
            b.fill(0.0);
        }
        for ch in &mut self.filters {
            for f in ch {
                f.reset();
            }
        }
    }

    #[inline]
    fn read(buf: &[f32], pos: usize, delay: f32) -> f32 {
        let n = buf.len();
        let d = delay.clamp(1.0, (n - 2) as f32);
        let i = d as usize;
        let frac = d - i as f32;
        let a = buf[(pos + n - i) % n];
        let b = buf[(pos + n - i - 1) % n];
        a + (b - a) * frac
    }

    pub fn process(&mut self, p: &DelayParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let sr = self.sr;
        if (p.low_cut, p.high_cut) != self.cached {
            self.cached = (p.low_cut, p.high_cut);
            self.lc = Coefs::design(
                FilterShape::HighPass,
                p.low_cut,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                sr,
            );
            self.hc = Coefs::design(
                FilterShape::LowPass,
                p.high_cut,
                std::f32::consts::FRAC_1_SQRT_2,
                0.0,
                sr,
            );
        }
        let (tl, tr) = p.times(ctx.tempo);
        let target = [(tl.min(MAX_SECONDS) * sr).max(1.0), (tr.min(MAX_SECONDS) * sr).max(1.0)];
        for c in 0..2 {
            if self.cur[c] < 0.0 {
                self.cur[c] = target[c];
            }
        }
        let glide = 1.0 - super::smoothing(0.06, sr);
        let fb = p.feedback.clamp(0.0, 0.95);
        let ping = p.ping_pong >= 0.5;
        let wet = p.mix;
        let dry = 1.0 - wet;
        let n = self.buf[0].len();
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            for c in 0..2 {
                self.cur[c] += (target[c] - self.cur[c]) * glide;
            }
            let dl = Self::read(&self.buf[0], self.pos, self.cur[0]);
            let dr = Self::read(&self.buf[1], self.pos, self.cur[1]);
            let (wl, wr) = if ping {
                // Mono input enters on the left and bounces between sides.
                ((*l + *r) * 0.5 + dr * fb, dl * fb)
            } else {
                (*l + dl * fb, *r + dr * fb)
            };
            let mut w = [wl, wr];
            for c in 0..2 {
                let [hp, lp] = &mut self.filters[c];
                w[c] = lp.process(&self.hc, hp.process(&self.lc, w[c]));
                self.buf[c][self.pos] = w[c];
            }
            self.pos += 1;
            if self.pos == n {
                self.pos = 0;
            }
            *l = *l * dry + dl * wet;
            *r = *r * dry + dr * wet;
        }
    }
}

impl super::EffectParams for DelayParams {
    fn is_relevant(&self, index: usize) -> bool {
        DelayParams::is_relevant(self, index)
    }
}

impl super::EffectRuntime for Delay {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn echo_arrives_after_the_synced_time() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = DelayParams {
            div_l: 8.0, // 1/4 = 0.5 s at 120 BPM
            div_r: 8.0,
            mix: 1.0,
            feedback: 0.5,
            low_cut: 20.0,
            high_cut: 20000.0,
            ..Default::default()
        };
        let mut d = Delay::new(sr);
        let mut l = vec![0.0f32; 72000];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            d.process(&p, a, b, &ctx);
        }
        let first = l.iter().position(|x| x.abs() > 0.1).unwrap();
        assert!((first as i64 - 24000).abs() <= 2, "first echo at {first}");
        // Second echo is quieter (feedback 0.5).
        let e1 = l[23990..24010].iter().fold(0.0f32, |a, b| a.max(b.abs()));
        let e2 = l[47990..48010].iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(e2 < e1 * 0.6 && e2 > e1 * 0.3, "{e1} {e2}");
    }

    #[test]
    fn ping_pong_alternates() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = DelayParams {
            sync: 0.0,
            time_l: 100.0,
            time_r: 100.0,
            ping_pong: 1.0,
            mix: 1.0,
            feedback: 0.8,
            low_cut: 20.0,
            high_cut: 20000.0,
            ..Default::default()
        };
        let mut d = Delay::new(sr);
        let mut l = vec![0.0f32; 24000];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        d.process(&p, &mut l, &mut r, &ctx);
        let at = |b: &[f32], t: usize| b[t - 10..t + 10].iter().fold(0.0f32, |a, x| a.max(x.abs()));
        assert!(at(&l, 4800) > 0.3 && at(&r, 4800) < 0.01);
        assert!(at(&r, 9600) > 0.2 && at(&l, 9600) < 0.01);
    }

    #[test]
    fn relevance_follows_sync() {
        let mut p = DelayParams::default();
        assert!(p.is_relevant(1) && !p.is_relevant(3));
        p.sync = 0.0;
        assert!(!p.is_relevant(1) && p.is_relevant(3));
    }
}
