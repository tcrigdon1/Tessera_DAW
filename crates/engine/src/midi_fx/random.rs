//! Random: with some chance, moves each note by a random number of steps.

use super::chord::NoteCounts;
use super::{emit, EventKind, MidiEvent, MidiFxContext};
use crate::dsp::Rng;
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const SIGNS: &[&str] = &["Add", "Sub", "Bi"];
pub const MODES: &[&str] = &["Random", "Alt"];

device_params! {
    pub struct RandomParams {
        chance: "Chance", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        choices: "Choices", 1.0, 24.0, 1.0, LIN, Unit::Integer;
        scale: "Scale", 1.0, 24.0, 12.0, LIN, Unit::Semitones;
        sign: "Sign", 0.0, 2.0, 0.0, ParamKind::Choice(SIGNS), Unit::Plain;
        mode: "Mode", 0.0, 1.0, 0.0, ParamKind::Choice(MODES), Unit::Plain;
    }
}

/// No output note for an input pitch.
const NONE: u8 = u8::MAX;

pub struct Random {
    rng: Rng,
    /// Output pitch of each held input pitch.
    held: [u8; 128],
    /// Two keys can land on the same pitch.
    counts: NoteCounts,
    /// Next choice in Alt mode (0-based).
    alt: u32,
}

impl Random {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: Rng::new(seed),
            held: [NONE; 128],
            counts: NoteCounts::new(),
            alt: 0,
        }
    }

    pub fn reset(&mut self) {
        self.held = [NONE; 128];
        self.counts.clear();
        self.alt = 0;
    }

    /// The pitch a new note on `key` plays.
    fn pick(&mut self, p: &RandomParams, key: u8) -> u8 {
        if p.chance <= 0.0 || self.rng.next_unit() >= p.chance {
            return key;
        }
        let choices = (p.choices.round() as u32).clamp(1, 24);
        let k = if p.mode >= 0.5 {
            let k = self.alt % choices + 1;
            self.alt = (self.alt + 1) % choices;
            k
        } else {
            (self.rng.next_unit() * choices as f32) as u32 % choices + 1
        };
        let sign = match p.sign.round() as i32 {
            0 => 1,
            1 => -1,
            _ => {
                if self.rng.next_bipolar() < 0.0 {
                    -1
                } else {
                    1
                }
            }
        };
        let to = key as i32 + sign * k as i32 * p.scale.round().max(1.0) as i32;
        if (0..=127).contains(&to) {
            to as u8
        } else {
            key
        }
    }

    pub fn process(&mut self, p: &RandomParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        for e in input {
            let f = e.frame as usize;
            match e.kind {
                EventKind::On(key, vel) => {
                    let slot = key as usize & 127;
                    if self.held[slot] != NONE {
                        self.counts.off(out, f, self.held[slot]);
                    }
                    let to = self.pick(p, key);
                    self.held[slot] = to;
                    self.counts.on(out, f, to, vel);
                }
                EventKind::Off(key) => {
                    let slot = key as usize & 127;
                    if self.held[slot] != NONE {
                        self.counts.off(out, f, self.held[slot]);
                        self.held[slot] = NONE;
                    }
                }
                EventKind::AllOff => {
                    self.reset();
                    emit(out, f, EventKind::AllOff);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    fn played(d: &mut Random, p: &RandomParams, key: u8) -> u8 {
        let mut out = buf();
        d.process(p, &[on(0, key, 100)], &mut out, &ctx(64));
        let pitch = match out.last().unwrap().kind {
            EventKind::On(pitch, 100) => pitch,
            k => panic!("{k:?}"),
        };
        out.clear();
        d.process(p, &[off(0, key)], &mut out, &ctx(64));
        assert_eq!(out[0].kind, EventKind::Off(pitch), "note-off matches");
        pitch
    }

    #[test]
    fn chance_zero_passes_and_one_always_moves() {
        let mut d = Random::new(3);
        let never = RandomParams {
            chance: 0.0,
            ..Default::default()
        };
        let always = RandomParams {
            chance: 1.0,
            ..Default::default()
        };
        for _ in 0..50 {
            assert_eq!(played(&mut d, &never, 60), 60);
            assert_eq!(played(&mut d, &always, 60), 72);
        }
    }

    #[test]
    fn random_choices_cover_the_range_with_signs() {
        let mut d = Random::new(11);
        let p = RandomParams {
            chance: 1.0,
            choices: 3.0,
            scale: 2.0,
            sign: 2.0,
            ..Default::default()
        };
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..400 {
            seen.insert(played(&mut d, &p, 60));
        }
        assert_eq!(seen.into_iter().collect::<Vec<_>>(), vec![54, 56, 58, 62, 64, 66]);
    }

    #[test]
    fn alt_mode_cycles_and_sub_goes_down() {
        let mut d = Random::new(5);
        let p = RandomParams {
            chance: 1.0,
            choices: 3.0,
            scale: 1.0,
            sign: 1.0,
            mode: 1.0,
        };
        let got: Vec<u8> = (0..6).map(|_| played(&mut d, &p, 60)).collect();
        assert_eq!(got, vec![59, 58, 57, 59, 58, 57]);
    }

    #[test]
    fn out_of_range_falls_back_to_the_original() {
        let mut d = Random::new(5);
        let p = RandomParams {
            chance: 1.0,
            scale: 24.0,
            ..Default::default()
        };
        assert_eq!(played(&mut d, &p, 120), 120);
    }

    #[test]
    fn note_off_matches_after_param_change_and_all_off_clears() {
        let mut d = Random::new(5);
        let mut p = RandomParams {
            chance: 1.0,
            ..Default::default()
        };
        let mut out = buf();
        d.process(&p, &[on(0, 60, 100)], &mut out, &ctx(64));
        assert_eq!(out[0].kind, EventKind::On(72, 100));
        p.chance = 0.0;
        out.clear();
        d.process(&p, &[off(2, 60)], &mut out, &ctx(64));
        assert_eq!(out, vec![off(2, 72)]);

        out.clear();
        d.process(&p, &[on(0, 61, 100)], &mut out, &ctx(64));
        out.clear();
        let all = MidiEvent {
            frame: 3,
            kind: EventKind::AllOff,
        };
        d.process(&p, &[all, off(4, 61)], &mut out, &ctx(64));
        assert_eq!(out, vec![all]);
    }
}
