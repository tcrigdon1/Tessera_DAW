//! Note Length: gives every note a fixed length in time or synced to the tempo, triggered on note-on or note-off.

use super::{emit, EventKind, MidiEvent, MidiFxContext};
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const MODES: &[&str] = &["Time", "Sync"];
pub const TRIGGERS: &[&str] = &["Note On", "Note Off"];
pub const SYNC_LENGTHS: &[&str] = &["1/32", "1/16", "1/8", "1/4", "1/2", "1 Bar", "2 Bars", "4 Bars"];
/// Beats for each entry of [`SYNC_LENGTHS`] (bars assume 4/4).
pub const SYNC_BEATS: [f64; 8] = [0.125, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];

device_params! {
    pub struct NoteLengthParams {
        mode: "Mode", 0.0, 1.0, 0.0, ParamKind::Choice(MODES), Unit::Plain;
        time: "Length", 1.0, 10000.0, 250.0, ParamKind::Continuous(Scale::Log), Unit::Ms;
        sync: "Sync", 0.0, 7.0, 3.0, ParamKind::Choice(SYNC_LENGTHS), Unit::Plain;
        gate: "Gate", 0.01, 2.0, 1.0, LIN, Unit::Percent;
        trigger: "Trigger", 0.0, 1.0, 0.0, ParamKind::Choice(TRIGGERS), Unit::Plain;
    }
}

impl NoteLengthParams {
    /// The millisecond length is hidden in Sync mode and vice versa.
    pub fn is_relevant(&self, index: usize) -> bool {
        match index {
            1 => self.mode < 0.5,
            2 => self.mode >= 0.5,
            _ => true,
        }
    }

    /// Note length in samples (at least one).
    fn length_samples(&self, sample_rate: f32, bpf: f64) -> u64 {
        let frames = if self.mode >= 0.5 {
            let i = (self.sync.round().max(0.0) as usize).min(SYNC_BEATS.len() - 1);
            if bpf > 0.0 {
                SYNC_BEATS[i] / bpf
            } else {
                0.0
            }
        } else {
            self.time as f64 * sample_rate as f64 / 1000.0
        };
        ((frames * self.gate as f64).round() as u64).max(1)
    }
}

/// Notes waiting for their note-off.
const MAX_PENDING: usize = 64;

#[derive(Clone, Copy, Default)]
struct Pending {
    pitch: u8,
    /// Absolute sample position of the note-off.
    at: u64,
}

pub struct NoteLength {
    sample_rate: f32,
    /// Absolute sample position of the current segment's first frame.
    now: u64,
    pending: [Pending; MAX_PENDING],
    n_pending: usize,
    /// Velocity of each key's note-on while it is down (0 = up), for the
    /// Note Off trigger.
    down: [u8; 128],
}

impl NoteLength {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            now: 0,
            pending: [Pending::default(); MAX_PENDING],
            n_pending: 0,
            down: [0; 128],
        }
    }

    pub fn reset(&mut self) {
        self.n_pending = 0;
        self.down = [0; 128];
    }

    /// Sends the note-offs due at or before absolute sample `upto`.
    fn flush(&mut self, upto: u64, out: &mut Vec<MidiEvent>) {
        let mut i = 0;
        while i < self.n_pending {
            let pd = self.pending[i];
            if pd.at <= upto {
                emit(out, pd.at.saturating_sub(self.now) as usize, EventKind::Off(pd.pitch));
                self.n_pending -= 1;
                self.pending[i] = self.pending[self.n_pending];
            } else {
                i += 1;
            }
        }
    }

    /// Ends the pending note on `pitch` (if any) at `frame`.
    fn end(&mut self, pitch: u8, frame: usize, out: &mut Vec<MidiEvent>) {
        if let Some(i) = self.pending[..self.n_pending].iter().position(|pd| pd.pitch == pitch) {
            emit(out, frame, EventKind::Off(pitch));
            self.n_pending -= 1;
            self.pending[i] = self.pending[self.n_pending];
        }
    }

    fn start(&mut self, pitch: u8, vel: u8, frame: usize, len: u64, out: &mut Vec<MidiEvent>) {
        // A retriggered note ends the old one first.
        self.end(pitch, frame, out);
        if self.n_pending == MAX_PENDING {
            // Full: end the note closest to its end now.
            let (i, oldest) = self.pending[..self.n_pending]
                .iter()
                .enumerate()
                .min_by_key(|(_, pd)| pd.at)
                .map(|(i, pd)| (i, *pd))
                .unwrap_or_default();
            emit(out, frame, EventKind::Off(oldest.pitch));
            self.n_pending -= 1;
            self.pending[i] = self.pending[self.n_pending];
        }
        emit(out, frame, EventKind::On(pitch, vel));
        self.pending[self.n_pending] = Pending {
            pitch,
            at: self.now + frame as u64 + len,
        };
        self.n_pending += 1;
    }

    pub fn process(
        &mut self,
        p: &NoteLengthParams,
        input: &[MidiEvent],
        out: &mut Vec<MidiEvent>,
        ctx: &MidiFxContext,
    ) {
        if ctx.frames == 0 {
            return;
        }
        let len = p.length_samples(self.sample_rate, ctx.bpf);
        let on_release = p.trigger >= 0.5;
        let last = ctx.frames - 1;
        for e in input {
            let f = (e.frame as usize).min(last);
            self.flush(self.now + f as u64, out);
            match e.kind {
                EventKind::On(key, vel) => {
                    self.down[key as usize & 127] = vel.max(1);
                    if !on_release {
                        self.start(key, vel, f, len, out);
                    }
                }
                EventKind::Off(key) => {
                    let vel = std::mem::take(&mut self.down[key as usize & 127]);
                    if on_release && vel > 0 {
                        self.start(key, vel, f, len, out);
                    }
                }
                EventKind::AllOff => {
                    self.reset();
                    emit(out, f, EventKind::AllOff);
                }
            }
        }
        self.flush(self.now + last as u64, out);
        self.now += ctx.frames as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const BLOCK: usize = 480;

    /// Runs `blocks` blocks, feeding `input` in the first, and returns the
    /// output with absolute frames.
    fn run(d: &mut NoteLength, p: &NoteLengthParams, input: &[MidiEvent], blocks: usize) -> Vec<(u64, EventKind)> {
        let mut all = Vec::new();
        for b in 0..blocks {
            let mut out = buf();
            let inp = if b == 0 { input } else { &[] };
            d.process(p, inp, &mut out, &ctx(BLOCK));
            for e in &out {
                assert!((e.frame as usize) < BLOCK);
                all.push(((b * BLOCK) as u64 + e.frame as u64, e.kind));
            }
        }
        all
    }

    #[test]
    fn time_mode_sets_the_length_and_ignores_note_offs() {
        let mut d = NoteLength::new(48000.0);
        let p = NoteLengthParams {
            time: 10.0,
            gate: 0.5,
            ..Default::default()
        };
        // 10 ms * 0.5 = 240 samples.
        let got = run(&mut d, &p, &[on(100, 60, 90), off(110, 60)], 3);
        assert_eq!(got, vec![(100, EventKind::On(60, 90)), (340, EventKind::Off(60))]);
    }

    #[test]
    fn sync_mode_follows_the_tempo_across_blocks() {
        let mut d = NoteLength::new(48000.0);
        let p = NoteLengthParams {
            mode: 1.0,
            sync: 1.0, // 1/16 = 0.25 beat = 6000 samples at 120 BPM.
            ..Default::default()
        };
        let got = run(&mut d, &p, &[on(5, 64, 100)], 20);
        assert_eq!(got, vec![(5, EventKind::On(64, 100)), (6005, EventKind::Off(64))]);
    }

    #[test]
    fn note_off_trigger_plays_on_release() {
        let mut d = NoteLength::new(48000.0);
        let p = NoteLengthParams {
            time: 5.0,
            trigger: 1.0,
            ..Default::default()
        };
        let mut out = buf();
        d.process(&p, &[on(0, 60, 77)], &mut out, &ctx(BLOCK));
        assert!(out.is_empty());
        let got = run(&mut d, &p, &[off(10, 60)], 2);
        assert_eq!(got, vec![(10, EventKind::On(60, 77)), (250, EventKind::Off(60))]);
        // A release without a press plays nothing.
        assert!(run(&mut d, &p, &[off(10, 61)], 2).is_empty());
    }

    #[test]
    fn retrigger_ends_the_old_note_first() {
        let mut d = NoteLength::new(48000.0);
        let p = NoteLengthParams {
            time: 100.0,
            ..Default::default()
        };
        let mut out = buf();
        d.process(&p, &[on(0, 60, 90), on(50, 60, 80)], &mut out, &ctx(BLOCK));
        let kinds: Vec<_> = out.iter().map(|e| (e.frame, e.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (0, EventKind::On(60, 90)),
                (50, EventKind::Off(60)),
                (50, EventKind::On(60, 80))
            ]
        );
        // Only one note-off follows.
        let rest = run(&mut d, &p, &[], 20);
        assert_eq!(rest.iter().filter(|(_, k)| *k == EventKind::Off(60)).count(), 1);
    }

    #[test]
    fn all_off_clears_pending_notes() {
        let mut d = NoteLength::new(48000.0);
        let p = NoteLengthParams::default();
        let mut out = buf();
        d.process(&p, &[on(0, 60, 90)], &mut out, &ctx(BLOCK));
        out.clear();
        let all = MidiEvent {
            frame: 1,
            kind: EventKind::AllOff,
        };
        d.process(&p, &[all], &mut out, &ctx(BLOCK));
        assert_eq!(out, vec![all]);
        assert!(run(&mut d, &p, &[], 40).is_empty());
    }

    #[test]
    fn many_notes_never_overflow() {
        let mut d = NoteLength::new(48000.0);
        let p = NoteLengthParams {
            time: 1000.0,
            ..Default::default()
        };
        let input: Vec<_> = (0..100u32).map(|i| on(i, i as u8, 90)).collect();
        let got = run(&mut d, &p, &input, 200);
        let ons = got.iter().filter(|(_, k)| matches!(k, EventKind::On(..))).count();
        let offs = got.iter().filter(|(_, k)| matches!(k, EventKind::Off(..))).count();
        assert_eq!((ons, offs), (100, 100));
    }
}
