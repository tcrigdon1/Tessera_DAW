//! Noise gate with hysteresis, hold, a floor, flip mode and lookahead.
//!
//! The detector is a stereo-linked peak follower on the undelayed input;
//! with lookahead the audio is delayed so the gate is already open when a
//! transient arrives. The gate opens when the level rises above Threshold and
//! closes once it falls below Threshold minus Return and the hold time has
//! elapsed. Flip inverts the action: only material below the threshold
//! passes.

use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const LOOKAHEADS: &[&str] = &["0 ms", "1.5 ms", "10 ms"];
const LOOKAHEAD_MS: [f32; 3] = [0.0, 1.5, 10.0];
const MAX_LOOKAHEAD_S: f32 = 0.0105;

device_params! {
    pub struct GateParams {
        threshold: "Threshold", -80.0, 0.0, -40.0, LIN, Unit::Db;
        return_db: "Return", 0.0, 24.0, 3.0, LIN, Unit::Db;
        attack: "Attack", 0.02, 100.0, 0.5, LOG, Unit::Ms;
        hold: "Hold", 0.0, 1000.0, 20.0, LIN, Unit::Ms;
        release: "Release", 1.0, 3000.0, 120.0, LOG, Unit::Ms;
        floor: "Floor", -80.0, 0.0, -80.0, LIN, Unit::Db;
        flip: "Flip", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        lookahead: "Lookahead", 0.0, 2.0, 0.0, ParamKind::Choice(LOOKAHEADS), Unit::Plain;
    }
}

impl GateParams {
    pub fn lookahead_ms(&self) -> f32 {
        LOOKAHEAD_MS[super::glue::choice(self.lookahead, LOOKAHEAD_MS.len())]
    }

    /// Floor as a linear gain (-80 dB means fully closed).
    pub fn floor_gain(&self) -> f32 {
        if self.floor <= -79.9 {
            0.0
        } else {
            super::db(self.floor.min(0.0))
        }
    }
}

impl super::EffectParams for GateParams {
    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Tight Drums",
                Self {
                    threshold: -30.0,
                    return_db: 4.0,
                    attack: 0.05,
                    hold: 15.0,
                    release: 60.0,
                    lookahead: 1.0,
                    ..d
                },
            ),
            (
                "Vocal Cleanup",
                Self {
                    threshold: -50.0,
                    return_db: 6.0,
                    attack: 2.0,
                    hold: 80.0,
                    release: 250.0,
                    floor: -18.0,
                    ..d
                },
            ),
            (
                "Gated Reverb",
                Self {
                    threshold: -24.0,
                    return_db: 2.0,
                    attack: 0.1,
                    hold: 250.0,
                    release: 15.0,
                    lookahead: 2.0,
                    ..d
                },
            ),
            (
                "Flip Tails",
                Self {
                    threshold: -20.0,
                    flip: 1.0,
                    attack: 1.0,
                    hold: 30.0,
                    release: 80.0,
                    ..d
                },
            ),
        ]
    }
}

pub struct Gate {
    sr: f32,
    /// Lookahead delay lines (one per channel) and the write position.
    delay: [Vec<f32>; 2],
    pos: usize,
    /// Peak follower on the detector input (linear).
    level: f32,
    open: bool,
    hold_left: u32,
    /// Gate envelope: 0 = closed, 1 = open.
    env: f32,
    meter_db: f32,
}

impl Gate {
    pub fn new(sample_rate: f32) -> Self {
        let len = (MAX_LOOKAHEAD_S * sample_rate.max(1.0)).ceil() as usize + 2;
        Self {
            sr: sample_rate,
            delay: [vec![0.0; len], vec![0.0; len]],
            pos: 0,
            level: 0.0,
            open: false,
            hold_left: 0,
            env: 0.0,
            meter_db: 0.0,
        }
    }

    pub fn reset(&mut self) {
        for d in &mut self.delay {
            d.iter_mut().for_each(|v| *v = 0.0);
        }
        self.level = 0.0;
        self.open = false;
        self.hold_left = 0;
        self.env = 0.0;
        self.meter_db = 0.0;
    }

    /// Current gain applied to the audio (linear).
    fn gain(&self, flip: bool, floor: f32) -> f32 {
        if flip {
            1.0 - self.env * (1.0 - floor)
        } else {
            floor + (1.0 - floor) * self.env
        }
    }

    pub fn process(&mut self, p: &GateParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let sr = self.sr;
        let len = self.delay[0].len();
        let la = ((p.lookahead_ms() * 0.001 * sr).round() as usize).min(len - 1);
        let open_lin = super::db(p.threshold.clamp(-80.0, 0.0));
        let close_lin = super::db((p.threshold - p.return_db.max(0.0)).clamp(-120.0, 0.0));
        let att = super::smoothing(p.attack.max(0.01) * 0.001, sr);
        let rel = super::smoothing(p.release.max(0.5) * 0.001, sr);
        // The peak follower decays fast enough to track the gate but slowly
        // enough to bridge zero crossings of low notes.
        let peak_decay = super::smoothing(0.008, sr);
        let hold = (p.hold.max(0.0) * 0.001 * sr) as u32;
        let floor = p.floor_gain();
        let flip = p.flip >= 0.5;
        for (l, r) in left.iter_mut().zip(right.iter_mut()) {
            let peak = l.abs().max(r.abs());
            self.level = if peak > self.level {
                peak
            } else {
                self.level * peak_decay
            };
            if self.level >= open_lin {
                self.open = true;
                self.hold_left = hold;
            } else if self.open && self.level < close_lin {
                if self.hold_left > 0 {
                    self.hold_left -= 1;
                } else {
                    self.open = false;
                }
            } else if self.open {
                // Between the two thresholds: stay open and keep the hold.
                self.hold_left = hold;
            }
            let target = if self.open { 1.0 } else { 0.0 };
            let coef = if target > self.env { att } else { rel };
            self.env = target + (self.env - target) * coef;
            if self.env < 1e-6 {
                self.env = 0.0;
            }
            // Delay the audio by the lookahead.
            self.delay[0][self.pos] = *l;
            self.delay[1][self.pos] = *r;
            let read = (self.pos + len - la) % len;
            let g = self.gain(flip, floor);
            *l = self.delay[0][read] * g;
            *r = self.delay[1][read] * g;
            self.pos = (self.pos + 1) % len;
        }
        let g = self.gain(flip, floor);
        self.meter_db = if g > 1e-4 { (20.0 * g.log10()).max(-80.0) } else { -80.0 };
    }
}

impl super::EffectRuntime for Gate {
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

    fn run(p: &GateParams, input: &[f32]) -> (Gate, Vec<f32>) {
        let ctx = FxContext::new(SR, 120.0);
        let mut g = Gate::new(SR);
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        for (cl, cr) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
            g.process(p, cl, cr, &ctx);
        }
        (g, l)
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
    }

    #[test]
    fn quiet_noise_is_gated_and_loud_signal_passes() {
        let p = GateParams::default(); // -40 dB threshold
        let mut rng = crate::dsp::Rng::new(1);
        let noise: Vec<f32> = (0..48000).map(|_| rng.next_bipolar() * 0.001).collect(); // -60 dB
        let (g, y) = run(&p, &noise);
        assert!(rms(&y[4800..]) < 1e-6);
        assert!(g.meter().unwrap() <= -79.0);
        let tone: Vec<f32> = (0..48000)
            .map(|i| 0.5 * (2.0 * PI * 100.0 * i as f32 / SR).sin())
            .collect();
        let (g, y) = run(&p, &tone);
        assert!((rms(&y[4800..]) / rms(&tone[4800..]) - 1.0).abs() < 0.01);
        assert!(g.meter().unwrap() > -0.1);
    }

    #[test]
    fn floor_limits_attenuation() {
        let p = GateParams {
            floor: -20.0,
            ..Default::default()
        };
        let x = vec![0.001f32; 48000];
        let (g, y) = run(&p, &x);
        assert!((y[47999] / 0.001 - 0.1).abs() < 1e-3);
        assert!((g.meter().unwrap() + 20.0).abs() < 0.1);
    }

    #[test]
    fn hold_and_hysteresis_keep_it_open() {
        let p = GateParams {
            threshold: -20.0,
            return_db: 10.0,
            hold: 100.0,
            release: 1.0,
            ..Default::default()
        };
        // Loud, then a level between close and open thresholds, then silence.
        let mut x = vec![0.5f32; 4800];
        x.extend(vec![0.05f32; 9600]); // -26 dB: above -30, below -20
        x.extend(vec![0.001f32; 9600]);
        let (_, y) = run(&p, &x);
        assert!((y[4800 + 9000] - 0.05).abs() < 1e-4, "between thresholds stays open");
        // Hold keeps it open ~100 ms (4800 samples) into the quiet part.
        assert!((y[14400 + 2400] - 0.001).abs() < 1e-5);
        assert!(y[14400 + 9000].abs() < 1e-5);
    }

    #[test]
    fn flip_passes_only_quiet_material() {
        let p = GateParams {
            flip: 1.0,
            ..Default::default()
        };
        let (_, y) = run(&p, &vec![0.001f32; 24000]);
        assert!((y[23999] - 0.001).abs() < 1e-6);
        let (_, y) = run(&p, &vec![0.5f32; 24000]);
        assert!(y[23999].abs() < 1e-5);
    }

    #[test]
    fn lookahead_opens_before_the_transient() {
        let p = GateParams {
            lookahead: 2.0,
            attack: 1.0,
            ..Default::default()
        };
        let mut x = vec![0.0f32; 4800];
        x[2400..].iter_mut().for_each(|v| *v = 0.5);
        let (_, y) = run(&p, &x);
        let delay = 480; // 10 ms
        assert_eq!(y[2400 + delay - 1], 0.0);
        // The first delayed sample of the burst is already almost fully open.
        assert!(y[2400 + delay] > 0.45, "{}", y[2400 + delay]);
        let p0 = GateParams { attack: 1.0, ..p };
        let p0 = GateParams { lookahead: 0.0, ..p0 };
        let (_, y0) = run(&p0, &x);
        assert!(y0[2400] < 0.1);
    }

    #[test]
    fn presets_stay_finite_at_all_rates() {
        let ctx = FxContext::new(192000.0, 120.0);
        for (_, p) in GateParams::presets() {
            let mut g = Gate::new(192000.0);
            let mut l: Vec<f32> = (0..2048).map(|i| ((i % 64) as f32 - 32.0) * 0.1).collect();
            let mut r = l.clone();
            g.process(&p, &mut l, &mut r, &ctx);
            g.process(&p, &mut l[..1], &mut r[..1], &ctx);
            assert!(l.iter().all(|v| v.is_finite()));
        }
    }
}
