//! Look-ahead brickwall limiter.
//!
//! The required gain for every incoming sample is known `L` samples before
//! that sample leaves the delay line. A sliding-window minimum over `L + 1`
//! samples followed by a moving average of the same length gives a smooth
//! gain curve that is guaranteed to have reached the required reduction by
//! the time the peak is output, so the ceiling is never exceeded (a final
//! clamp only catches rounding).

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const LOOKAHEADS: &[&str] = &["1.5 ms", "3 ms", "6 ms"];
const LOOKAHEAD_MS: [f32; 3] = [1.5, 3.0, 6.0];

device_params! {
    pub struct LimiterParams {
        gain: "Gain", 0.0, 24.0, 0.0, LIN, Unit::Db;
        ceiling: "Ceiling", -12.0, 0.0, -0.3, LIN, Unit::Db;
        release: "Release", 1.0, 1000.0, 60.0, LOG, Unit::Ms;
        lookahead: "Lookahead", 0.0, 2.0, 0.0, ParamKind::Choice(LOOKAHEADS), Unit::Plain;
    }
}

pub struct Limiter {
    sr: f32,
    /// Look-ahead in samples currently in use.
    la: usize,
    delay: [Vec<f32>; 2],
    pos: usize,
    // Monotonic deque of (required gain, sample index) for the sliding minimum.
    dq_val: Vec<f32>,
    dq_idx: Vec<u64>,
    dq_head: usize,
    dq_len: usize,
    // Moving average of the minimum.
    avg_buf: Vec<f32>,
    avg_pos: usize,
    avg_sum: f64,
    t: u64,
    env: f32,
    pub meter_db: f32,
}

impl Limiter {
    pub fn new(sample_rate: f32) -> Self {
        let max = (LOOKAHEAD_MS[2] * 0.001 * sample_rate).ceil() as usize + 2;
        let mut l = Self {
            sr: sample_rate,
            la: 0,
            delay: [vec![0.0; max + 1], vec![0.0; max + 1]],
            pos: 0,
            dq_val: vec![0.0; max + 2],
            dq_idx: vec![0; max + 2],
            dq_head: 0,
            dq_len: 0,
            avg_buf: vec![1.0; max + 1],
            avg_pos: 0,
            avg_sum: 0.0,
            t: 0,
            env: 1.0,
            meter_db: 0.0,
        };
        l.configure(0);
        l
    }

    fn configure(&mut self, choice: usize) {
        let la = ((LOOKAHEAD_MS[choice.min(2)] * 0.001 * self.sr).round() as usize).clamp(1, self.avg_buf.len() - 1);
        self.la = la;
        for d in &mut self.delay {
            d.fill(0.0);
        }
        self.pos = 0;
        self.dq_head = 0;
        self.dq_len = 0;
        self.avg_buf.fill(1.0);
        self.avg_pos = 0;
        self.avg_sum = (la + 1) as f64;
        self.env = 1.0;
    }

    pub fn reset(&mut self) {
        let choice = LOOKAHEAD_MS
            .iter()
            .position(|ms| ((ms * 0.001 * self.sr).round() as usize).max(1) == self.la)
            .unwrap_or(0);
        self.configure(choice);
    }

    /// Latency in samples.
    pub fn latency(&self) -> usize {
        self.la
    }

    pub fn process(&mut self, p: &LimiterParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let choice = p.lookahead.round().max(0.0) as usize;
        let want = ((LOOKAHEAD_MS[choice.min(2)] * 0.001 * self.sr).round() as usize).clamp(1, self.avg_buf.len() - 1);
        if want != self.la {
            self.configure(choice);
        }
        let la = self.la;
        let win = la + 1;
        let gain = super::db(p.gain);
        let ceiling = super::db(p.ceiling);
        let rel = super::smoothing(p.release * 0.001, self.sr);
        let dlen = self.delay[0].len();
        let cap = self.dq_val.len();
        let mut lowest = 1.0f32;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let xl = *l * gain;
            let xr = *r * gain;
            let peak = xl.abs().max(xr.abs());
            let need = if peak > ceiling { ceiling / peak } else { 1.0 };

            // Sliding minimum over the last `win` samples.
            let t = self.t;
            while self.dq_len > 0 {
                let back = (self.dq_head + self.dq_len - 1) % cap;
                if self.dq_val[back] >= need {
                    self.dq_len -= 1;
                } else {
                    break;
                }
            }
            let slot = (self.dq_head + self.dq_len) % cap;
            self.dq_val[slot] = need;
            self.dq_idx[slot] = t;
            self.dq_len += 1;
            while self.dq_idx[self.dq_head] + (win as u64) <= t {
                self.dq_head = (self.dq_head + 1) % cap;
                self.dq_len -= 1;
            }
            let hold = self.dq_val[self.dq_head];

            // Moving average over the same window.
            self.avg_sum += hold as f64 - self.avg_buf[self.avg_pos] as f64;
            self.avg_buf[self.avg_pos] = hold;
            self.avg_pos += 1;
            if self.avg_pos == win {
                self.avg_pos = 0;
            }
            let smooth = (self.avg_sum / win as f64) as f32;
            self.env = if smooth < self.env {
                smooth
            } else {
                smooth + (self.env - smooth) * rel
            };
            lowest = lowest.min(self.env);

            // Delay the audio by the look-ahead.
            self.delay[0][self.pos] = xl;
            self.delay[1][self.pos] = xr;
            let read = (self.pos + dlen - la) % dlen;
            self.pos = (self.pos + 1) % dlen;
            *l = (self.delay[0][read] * self.env).clamp(-ceiling, ceiling);
            *r = (self.delay[1][read] * self.env).clamp(-ceiling, ceiling);
            self.t += 1;
        }
        self.meter_db = 20.0 * lowest.max(1e-6).log10();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_exceeds_the_ceiling() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = LimiterParams {
            gain: 12.0,
            ceiling: -1.0,
            ..Default::default()
        };
        let ceiling = super::super::db(-1.0);
        let mut lim = Limiter::new(sr);
        let mut rng = crate::dsp::Rng::new(5);
        let mut max_out = 0.0f32;
        let mut max_in = 0.0f32;
        for block in 0..200 {
            let mut l = [0.0f32; 333];
            let mut r = [0.0f32; 333];
            for i in 0..333 {
                // Bursts of loud noise with sudden transients.
                let amp = if (block / 10) % 2 == 0 { 0.9 } else { 0.05 };
                l[i] = rng.next_bipolar() * amp;
                r[i] = rng.next_bipolar() * amp;
            }
            max_in = l.iter().fold(max_in, |a, b| a.max(b.abs()));
            lim.process(&p, &mut l, &mut r, &ctx);
            max_out = l.iter().chain(r.iter()).fold(max_out, |a, b| a.max(b.abs()));
        }
        assert!(max_out <= ceiling + 1e-6, "{max_out}");
        // The smoothing, not the safety clamp, does the work.
        assert!(max_in * 3.98 > ceiling);
    }

    #[test]
    fn smooth_gain_reaches_the_target_before_the_peak() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = LimiterParams {
            ceiling: -6.0,
            ..Default::default()
        };
        let mut lim = Limiter::new(sr);
        let la = lim.latency();
        // A single full-scale step; the output must ramp, not jump.
        let mut l = vec![0.1f32; 2000];
        for x in &mut l[1000..] {
            *x = 1.0;
        }
        let mut r = l.clone();
        lim.process(&p, &mut l, &mut r, &ctx);
        let c = super::super::db(-6.0);
        for (i, x) in l.iter().enumerate() {
            assert!(*x <= c + 1e-6, "sample {i}: {x}");
        }
        // Before the step reaches the output, gain is already falling smoothly.
        assert!(l[1000 + la - 1] < 0.1 * 0.9);
        assert!((l[1999] - c).abs() < 1e-3);
        assert!(lim.meter_db < -5.9);
    }

    #[test]
    fn quiet_signals_pass_unchanged_but_delayed() {
        let sr = 44100.0;
        let ctx = FxContext::new(sr, 120.0);
        let p = LimiterParams::default();
        let mut lim = Limiter::new(sr);
        let la = lim.latency();
        let mut l: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        let orig = l.clone();
        let mut r = l.clone();
        lim.process(&p, &mut l, &mut r, &ctx);
        for i in la..1000 {
            assert!((l[i] - orig[i - la]).abs() < 1e-6);
        }
    }
}
