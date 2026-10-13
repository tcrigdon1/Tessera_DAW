//! Pitch: transposes notes and lets only a range of pitches through.

use super::{emit, EventKind, MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct PitchParams {
        pitch: "Pitch", -48.0, 48.0, 0.0, LIN, Unit::Semitones;
        lowest: "Lowest", 0.0, 127.0, 0.0, LIN, Unit::Note;
        range: "Range", 0.0, 127.0, 127.0, LIN, Unit::Integer;
    }
}

/// No output note for an input pitch.
const NONE: u8 = u8::MAX;

pub struct Pitch {
    /// Output pitch of each held input pitch, so a note-off matches its
    /// note-on even if the transposition changed in between.
    held: [u8; 128],
}

impl Pitch {
    pub fn new() -> Self {
        Self { held: [NONE; 128] }
    }

    pub fn reset(&mut self) {
        self.held = [NONE; 128];
    }

    pub fn process(&mut self, p: &PitchParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        let lo = p.lowest.round() as i32;
        let hi = lo + p.range.round() as i32;
        let shift = p.pitch.round() as i32;
        for e in input {
            let f = e.frame as usize;
            match e.kind {
                EventKind::On(key, vel) => {
                    let k = key as i32;
                    let to = k + shift;
                    if k < lo || k > hi || !(0..=127).contains(&to) {
                        continue;
                    }
                    let slot = &mut self.held[key as usize & 127];
                    if *slot != NONE {
                        emit(out, f, EventKind::Off(*slot));
                    }
                    *slot = to as u8;
                    emit(out, f, EventKind::On(to as u8, vel));
                }
                EventKind::Off(key) => {
                    let slot = &mut self.held[key as usize & 127];
                    if *slot != NONE {
                        emit(out, f, EventKind::Off(*slot));
                        *slot = NONE;
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

impl Default for Pitch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    #[test]
    fn transposes_and_matches_note_offs() {
        let mut d = Pitch::new();
        let mut p = PitchParams {
            pitch: 7.0,
            ..Default::default()
        };
        let mut out = buf();
        d.process(&p, &[on(0, 60, 90)], &mut out, &ctx(64));
        assert_eq!(out[0].kind, EventKind::On(67, 90));
        // Changing the shift while the note is held still releases 67.
        p.pitch = -12.0;
        out.clear();
        d.process(&p, &[off(3, 60)], &mut out, &ctx(64));
        assert_eq!(out[0].kind, EventKind::Off(67));
        assert_eq!(out[0].frame, 3);
    }

    #[test]
    fn filters_the_range() {
        let mut d = Pitch::new();
        let p = PitchParams {
            lowest: 48.0,
            range: 12.0,
            ..Default::default()
        };
        let mut out = buf();
        d.process(
            &p,
            &[on(0, 47, 90), on(0, 48, 90), on(0, 60, 90), on(0, 61, 90)],
            &mut out,
            &ctx(64),
        );
        let keys: Vec<_> = out.iter().map(|e| e.kind).collect();
        assert_eq!(keys, vec![EventKind::On(48, 90), EventKind::On(60, 90)]);
        out.clear();
        d.process(&p, &[off(0, 47)], &mut out, &ctx(64));
        assert!(out.is_empty(), "filtered notes have no note-off");
    }
}
