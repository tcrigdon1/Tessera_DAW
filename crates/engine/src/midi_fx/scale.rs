//! Scale: moves every note onto the nearest note of a scale.

use super::chord::NoteCounts;
use super::{emit, EventKind, MidiEvent, MidiFxContext};
use crate::fx::{self, device_params, ParamKind, Unit};
use crate::midi::scale::{MusicalScale, ScaleKind, ROOT_NAMES};

const LIN: ParamKind = ParamKind::Continuous(fx::Scale::Linear);

device_params! {
    pub struct ScaleParams {
        base: "Base", 0.0, 11.0, 0.0, ParamKind::Choice(&ROOT_NAMES), Unit::Plain;
        scale: "Scale", 0.0, 13.0, 0.0, ParamKind::Choice(&ScaleKind::LABELS), Unit::Plain;
        transpose: "Transpose", -36.0, 36.0, 0.0, LIN, Unit::Plain;
    }
}

/// No output note for an input pitch.
const NONE: u8 = u8::MAX;

pub struct Scale {
    /// Output pitch of each held input pitch.
    held: [u8; 128],
    /// Several keys can snap to the same pitch.
    counts: NoteCounts,
}

impl Scale {
    pub fn new() -> Self {
        Self {
            held: [NONE; 128],
            counts: NoteCounts::new(),
        }
    }

    pub fn reset(&mut self) {
        self.held = [NONE; 128];
        self.counts.clear();
    }

    pub fn process(&mut self, p: &ScaleParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        let scale = MusicalScale::new(
            p.base.round().clamp(0.0, 11.0) as u8,
            ScaleKind::from_index(p.scale.round().max(0.0) as usize),
        );
        let transpose = p.transpose.round() as i32;
        for e in input {
            let f = e.frame as usize;
            match e.kind {
                EventKind::On(key, vel) => {
                    let slot = key as usize & 127;
                    if self.held[slot] != NONE {
                        self.counts.off(out, f, self.held[slot]);
                        self.held[slot] = NONE;
                    }
                    let to = scale.snap(key) as i32 + transpose;
                    if !(0..=127).contains(&to) {
                        continue;
                    }
                    self.held[slot] = to as u8;
                    self.counts.on(out, f, to as u8, vel);
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

impl Default for Scale {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    fn kinds(out: &[MidiEvent]) -> Vec<EventKind> {
        out.iter().map(|e| e.kind).collect()
    }

    fn major(base: u8) -> ScaleParams {
        ScaleParams {
            base: base as f32,
            scale: ScaleKind::Major.index() as f32,
            ..Default::default()
        }
    }

    #[test]
    fn snaps_to_the_scale_and_transposes() {
        let mut d = Scale::new();
        let mut p = major(0);
        p.transpose = 12.0;
        let mut out = buf();
        // C# snaps to C (ties go down), D stays.
        d.process(&p, &[on(0, 61, 90), on(1, 62, 80)], &mut out, &ctx(64));
        assert_eq!(kinds(&out), vec![EventKind::On(72, 90), EventKind::On(74, 80)]);
    }

    #[test]
    fn note_offs_match_even_after_changes() {
        let mut d = Scale::new();
        let mut p = major(0);
        let mut out = buf();
        d.process(&p, &[on(0, 66, 90)], &mut out, &ctx(64));
        let played = out[0].kind;
        p.base = 6.0;
        p.transpose = 5.0;
        out.clear();
        d.process(&p, &[off(7, 66)], &mut out, &ctx(64));
        let EventKind::On(pitch, _) = played else { panic!() };
        assert_eq!(kinds(&out), vec![EventKind::Off(pitch)]);
        assert_eq!(out[0].frame, 7);
    }

    #[test]
    fn keys_snapping_to_one_pitch_release_it_last() {
        let mut d = Scale::new();
        let p = major(0);
        let mut out = buf();
        d.process(&p, &[on(0, 60, 90), on(0, 61, 90)], &mut out, &ctx(64));
        out.clear();
        d.process(&p, &[off(0, 61)], &mut out, &ctx(64));
        assert!(out.is_empty(), "60 is still held");
        d.process(&p, &[off(0, 60)], &mut out, &ctx(64));
        assert_eq!(kinds(&out), vec![EventKind::Off(60)]);
    }

    #[test]
    fn all_off_is_forwarded_and_clears_state() {
        let mut d = Scale::new();
        let p = major(0);
        let mut out = buf();
        d.process(&p, &[on(0, 60, 90)], &mut out, &ctx(64));
        out.clear();
        let all = MidiEvent {
            frame: 0,
            kind: EventKind::AllOff,
        };
        d.process(&p, &[all, off(1, 60)], &mut out, &ctx(64));
        assert_eq!(kinds(&out), vec![EventKind::AllOff]);
    }
}
