//! Velocity: shapes (drive), limits (out low/high) and randomizes note velocities.

use super::{emit, EventKind, MidiEvent, MidiFxContext};
use crate::dsp::Rng;
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct VelocityParams {
        drive: "Drive", -1.0, 1.0, 0.0, LIN, Unit::Plain;
        out_low: "Out Low", 1.0, 127.0, 1.0, LIN, Unit::Integer;
        out_high: "Out Hi", 1.0, 127.0, 127.0, LIN, Unit::Integer;
        random: "Random", 0.0, 64.0, 0.0, LIN, Unit::Integer;
    }
}

impl VelocityParams {
    /// The velocity before randomization, as a float in `out_low ..= out_high`.
    pub fn shape(&self, velocity: u8) -> f32 {
        let x = (velocity.clamp(1, 127) as f32 - 1.0) / 126.0;
        // Drive bends the curve: x^(4^-drive) runs from x^4 (drive -1,
        // velocities pushed down) through linear to x^0.25 (drive +1).
        let y = x.powf(4f32.powf(-self.drive.clamp(-1.0, 1.0)));
        self.out_low + y * (self.out_high - self.out_low)
    }
}

pub struct Velocity {
    rng: Rng,
}

impl Velocity {
    pub fn new(seed: u32) -> Self {
        Self { rng: Rng::new(seed) }
    }

    /// No per-note state; the random generator keeps running.
    pub fn reset(&mut self) {}

    pub fn process(&mut self, p: &VelocityParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        for e in input {
            let kind = match e.kind {
                EventKind::On(key, vel) => {
                    let mut v = p.shape(vel);
                    if p.random > 0.0 {
                        v += self.rng.next_bipolar() * p.random;
                    }
                    EventKind::On(key, v.round().clamp(1.0, 127.0) as u8)
                }
                k => k,
            };
            emit(out, e.frame as usize, kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    fn velocity_of(d: &mut Velocity, p: &VelocityParams, v: u8) -> u8 {
        let mut out = buf();
        d.process(p, &[on(0, 60, v)], &mut out, &ctx(64));
        match out[0].kind {
            EventKind::On(60, v) => v,
            k => panic!("{k:?}"),
        }
    }

    #[test]
    fn default_is_identity() {
        let mut d = Velocity::new(1);
        let p = VelocityParams::default();
        for v in 1..=127 {
            assert_eq!(velocity_of(&mut d, &p, v), v);
        }
    }

    #[test]
    fn drive_bends_the_curve_and_range_limits() {
        let mut d = Velocity::new(1);
        let up = VelocityParams {
            drive: 1.0,
            ..Default::default()
        };
        let down = VelocityParams {
            drive: -1.0,
            ..Default::default()
        };
        assert!(velocity_of(&mut d, &up, 64) > 100);
        assert!(velocity_of(&mut d, &down, 64) < 20);
        // Ends stay put.
        assert_eq!(velocity_of(&mut d, &up, 127), 127);
        assert_eq!(velocity_of(&mut d, &down, 1), 1);

        let range = VelocityParams {
            out_low: 40.0,
            out_high: 80.0,
            ..Default::default()
        };
        assert_eq!(velocity_of(&mut d, &range, 1), 40);
        assert_eq!(velocity_of(&mut d, &range, 127), 80);
        assert_eq!(velocity_of(&mut d, &range, 64), 60);
    }

    #[test]
    fn random_stays_within_bounds_and_varies() {
        let mut d = Velocity::new(7);
        let p = VelocityParams {
            random: 10.0,
            ..Default::default()
        };
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let v = velocity_of(&mut d, &p, 64);
            assert!((54..=74).contains(&v), "{v}");
            seen.insert(v);
        }
        assert!(seen.len() > 5);
        // Clamped to 1..=127.
        let wide = VelocityParams {
            random: 64.0,
            ..Default::default()
        };
        for _ in 0..200 {
            let v = velocity_of(&mut d, &wide, 127);
            assert!((1..=127).contains(&v));
        }
    }

    #[test]
    fn note_offs_and_all_off_pass_through() {
        let mut d = Velocity::new(1);
        let p = VelocityParams {
            drive: 0.5,
            ..Default::default()
        };
        let all = MidiEvent {
            frame: 1,
            kind: EventKind::AllOff,
        };
        let mut out = buf();
        d.process(&p, &[off(0, 60), all], &mut out, &ctx(64));
        assert_eq!(out, vec![off(0, 60), all]);
    }
}
