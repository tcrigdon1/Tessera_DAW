//! Chord: adds up to six notes at fixed intervals (0 = off) to every incoming note.

use super::{emit, EventKind, MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    pub struct ChordParams {
        shift1: "Shift 1", -36.0, 36.0, 4.0, LIN, Unit::Semitones;
        shift2: "Shift 2", -36.0, 36.0, 7.0, LIN, Unit::Semitones;
        shift3: "Shift 3", -36.0, 36.0, 0.0, LIN, Unit::Semitones;
        shift4: "Shift 4", -36.0, 36.0, 0.0, LIN, Unit::Semitones;
        shift5: "Shift 5", -36.0, 36.0, 0.0, LIN, Unit::Semitones;
        shift6: "Shift 6", -36.0, 36.0, 0.0, LIN, Unit::Semitones;
        velocity: "Velocity", 0.0, 2.0, 1.0, LIN, Unit::Percent;
    }
}

/// No output note.
const NONE: u8 = u8::MAX;
/// The original note plus six shifts.
const MAX_OUT: usize = 7;

/// How many held input notes currently sound each output pitch. Devices that
/// can map several inputs onto the same output pitch (Chord, Scale, Random)
/// use it so the first release doesn't cut a note another key still holds:
/// a second note-on re-triggers the pitch and only the last note-off
/// releases it.
pub(crate) struct NoteCounts([u8; 128]);

impl NoteCounts {
    pub fn new() -> Self {
        Self([0; 128])
    }

    pub fn clear(&mut self) {
        self.0 = [0; 128];
    }

    /// Starts (or re-triggers) `pitch`.
    pub fn on(&mut self, out: &mut Vec<MidiEvent>, frame: usize, pitch: u8, velocity: u8) {
        let c = &mut self.0[pitch as usize & 127];
        if *c > 0 {
            emit(out, frame, EventKind::Off(pitch));
        }
        *c = c.saturating_add(1);
        emit(out, frame, EventKind::On(pitch, velocity));
    }

    /// Releases one holder of `pitch`; the note stops with the last one.
    pub fn off(&mut self, out: &mut Vec<MidiEvent>, frame: usize, pitch: u8) {
        let c = &mut self.0[pitch as usize & 127];
        if *c == 0 {
            return;
        }
        *c -= 1;
        if *c == 0 {
            emit(out, frame, EventKind::Off(pitch));
        }
    }
}

pub struct Chord {
    /// Output pitches started by each held input pitch (`NONE`-padded).
    held: [[u8; MAX_OUT]; 128],
    counts: NoteCounts,
}

impl Chord {
    pub fn new() -> Self {
        Self {
            held: [[NONE; MAX_OUT]; 128],
            counts: NoteCounts::new(),
        }
    }

    pub fn reset(&mut self) {
        self.held = [[NONE; MAX_OUT]; 128];
        self.counts.clear();
    }

    fn release(&mut self, key: u8, frame: usize, out: &mut Vec<MidiEvent>) {
        let slot = &mut self.held[key as usize & 127];
        for p in slot.iter_mut() {
            if *p != NONE {
                self.counts.off(out, frame, *p);
                *p = NONE;
            }
        }
    }

    pub fn process(&mut self, p: &ChordParams, input: &[MidiEvent], out: &mut Vec<MidiEvent>, _ctx: &MidiFxContext) {
        let shifts = [0.0, p.shift1, p.shift2, p.shift3, p.shift4, p.shift5, p.shift6];
        for e in input {
            let f = e.frame as usize;
            match e.kind {
                EventKind::On(key, vel) => {
                    // A repeated note-on first ends what the key played.
                    self.release(key, f, out);
                    let added_vel = ((vel as f32 * p.velocity).round() as i32).clamp(1, 127) as u8;
                    let mut n = 0;
                    for (i, s) in shifts.iter().enumerate() {
                        let s = s.round() as i32;
                        if i > 0 && s == 0 {
                            continue;
                        }
                        let to = key as i32 + s;
                        if !(0..=127).contains(&to) {
                            continue;
                        }
                        let to = to as u8;
                        // Two shifts with the same interval play one note.
                        if self.held[key as usize & 127][..n].contains(&to) {
                            continue;
                        }
                        self.held[key as usize & 127][n] = to;
                        n += 1;
                        let v = if i == 0 { vel } else { added_vel };
                        self.counts.on(out, f, to, v);
                    }
                }
                EventKind::Off(key) => self.release(key, f, out),
                EventKind::AllOff => {
                    self.reset();
                    emit(out, f, EventKind::AllOff);
                }
            }
        }
    }
}

impl Default for Chord {
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

    #[test]
    fn adds_shifted_notes_with_scaled_velocity() {
        let mut d = Chord::new();
        let p = ChordParams {
            velocity: 0.5,
            ..Default::default()
        };
        let mut out = buf();
        d.process(&p, &[on(4, 60, 100)], &mut out, &ctx(64));
        assert_eq!(
            kinds(&out),
            vec![EventKind::On(60, 100), EventKind::On(64, 50), EventKind::On(67, 50)]
        );
        assert!(out.iter().all(|e| e.frame == 4));
    }

    #[test]
    fn note_offs_release_what_the_note_on_played() {
        let mut d = Chord::new();
        let mut p = ChordParams::default();
        let mut out = buf();
        d.process(&p, &[on(0, 60, 100)], &mut out, &ctx(64));
        // Changing the shifts while held still releases 60, 64 and 67.
        p.shift1 = 3.0;
        p.shift2 = 12.0;
        out.clear();
        d.process(&p, &[off(9, 60)], &mut out, &ctx(64));
        assert_eq!(
            kinds(&out),
            vec![EventKind::Off(60), EventKind::Off(64), EventKind::Off(67)]
        );
    }

    #[test]
    fn skips_out_of_range_and_duplicate_shifts() {
        let mut d = Chord::new();
        let p = ChordParams {
            shift1: 12.0,
            shift2: 12.0,
            shift3: -24.0,
            ..Default::default()
        };
        let mut out = buf();
        d.process(&p, &[on(0, 120, 100)], &mut out, &ctx(64));
        assert_eq!(kinds(&out), vec![EventKind::On(120, 100), EventKind::On(96, 100)]);
    }

    #[test]
    fn shared_output_pitches_are_reference_counted() {
        let mut d = Chord::new();
        // C (60, 64, 67) and E (64, 68, 71) share 64.
        let p = ChordParams::default();
        let mut out = buf();
        d.process(&p, &[on(0, 60, 100), on(0, 64, 100)], &mut out, &ctx(64));
        out.clear();
        d.process(&p, &[off(0, 60)], &mut out, &ctx(64));
        assert_eq!(kinds(&out), vec![EventKind::Off(60), EventKind::Off(67)]);
        out.clear();
        d.process(&p, &[off(0, 64)], &mut out, &ctx(64));
        assert_eq!(
            kinds(&out),
            vec![EventKind::Off(64), EventKind::Off(68), EventKind::Off(71)]
        );
    }

    #[test]
    fn all_off_is_forwarded_and_clears_state() {
        let mut d = Chord::new();
        let p = ChordParams::default();
        let mut out = buf();
        d.process(&p, &[on(0, 60, 100)], &mut out, &ctx(64));
        out.clear();
        let all = MidiEvent {
            frame: 2,
            kind: EventKind::AllOff,
        };
        d.process(&p, &[all, off(3, 60)], &mut out, &ctx(64));
        assert_eq!(kinds(&out), vec![EventKind::AllOff]);
    }
}
