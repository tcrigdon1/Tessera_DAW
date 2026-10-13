//! Arpeggiator: plays the held notes one at a time in a pattern, synced to the tempo.
//!
//! Steps fall on the musical grid of the clock the chain is given (session
//! beats while playing, a free-running clock while stopped). The first note
//! pressed while nothing plays starts the pattern at once, at that note's
//! frame; later steps follow every `Rate` beats from there (snapped to the
//! grid when the note is on it, which clip notes are). When the clock jumps
//! (transport start, relocation), the next step moves to the next grid point.

use super::{emit, EventKind, MidiEvent, MidiFxContext};
use crate::dsp::Rng;
use crate::fx::{device_params, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const STYLES: &[&str] = &[
    "Up",
    "Down",
    "Up Down",
    "Down Up",
    "Up & Down",
    "Down & Up",
    "Converge",
    "Diverge",
    "Pinky Up",
    "Thumb Up",
    "Played",
    "Random",
    "Chord",
];
pub const RATES: &[&str] = &["1/32", "1/16T", "1/16", "1/8T", "1/8", "1/4T", "1/4", "1/2", "1 Bar"];
/// Beats per step for each entry of [`RATES`].
pub const RATE_BEATS: [f64; 9] = [0.125, 1.0 / 6.0, 0.25, 1.0 / 3.0, 0.5, 2.0 / 3.0, 1.0, 2.0, 4.0];

device_params! {
    pub struct ArpeggiatorParams {
        style: "Style", 0.0, 12.0, 0.0, ParamKind::Choice(STYLES), Unit::Plain;
        rate: "Rate", 0.0, 8.0, 2.0, ParamKind::Choice(RATES), Unit::Plain;
        gate: "Gate", 0.01, 2.0, 0.5, LIN, Unit::Percent;
        steps: "Steps", 0.0, 8.0, 0.0, LIN, Unit::Plain;
        distance: "Distance", -24.0, 24.0, 12.0, LIN, Unit::Plain;
        hold: "Hold", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        velocity_decay: "Vel Decay", 0.0, 1.0, 0.0, LIN, Unit::Percent;
    }
}

/// Style indices into [`STYLES`].
mod style {
    pub const UP: usize = 0;
    pub const DOWN: usize = 1;
    pub const UP_DOWN: usize = 2;
    pub const DOWN_UP: usize = 3;
    pub const UP_AND_DOWN: usize = 4;
    pub const DOWN_AND_UP: usize = 5;
    pub const CONVERGE: usize = 6;
    pub const DIVERGE: usize = 7;
    pub const PINKY_UP: usize = 8;
    pub const THUMB_UP: usize = 9;
    pub const PLAYED: usize = 10;
    pub const RANDOM: usize = 11;
    pub const CHORD: usize = 12;
}

/// Held notes the arpeggiator plays (press order).
const MAX_NOTES: usize = 32;
/// Notes it can have sounding at once (gates over 100% overlap steps).
const MAX_SOUNDING: usize = 64;
/// Clock differences below this (in beats) are rounding, not jumps.
const JUMP_TOLERANCE: f64 = 1e-6;
/// Velocity lost per step at 100% Vel Decay (as a fraction).
const DECAY_PER_STEP: f32 = 0.1;

/// Length of one pass of `style` over `n` (> 0) notes.
fn pattern_len(style: usize, n: usize) -> usize {
    match style {
        style::UP_DOWN | style::DOWN_UP | style::PINKY_UP | style::THUMB_UP if n < 2 => 1,
        style::UP_DOWN | style::DOWN_UP | style::PINKY_UP | style::THUMB_UP => 2 * n - 2,
        style::UP_AND_DOWN | style::DOWN_AND_UP => 2 * n,
        style::CHORD => 1,
        _ => n,
    }
}

/// Index (into the notes sorted by pitch) of step `i` of a pass. Not used by
/// Played, Random and Chord.
fn pattern_index(style: usize, n: usize, i: usize) -> usize {
    let converge = |i: usize| if i.is_multiple_of(2) { i / 2 } else { n - 1 - i / 2 };
    let up_down = |i: usize| {
        if n < 2 {
            0
        } else if i < n {
            i
        } else {
            2 * n - 2 - i
        }
    };
    let up_and_down = |i: usize| if i < n { i } else { 2 * n - 1 - i };
    match style {
        style::UP => i,
        style::DOWN => n - 1 - i,
        style::UP_DOWN => up_down(i),
        style::DOWN_UP => n - 1 - up_down(i),
        style::UP_AND_DOWN => up_and_down(i),
        style::DOWN_AND_UP => n - 1 - up_and_down(i),
        style::CONVERGE => converge(i),
        style::DIVERGE => converge(n - 1 - i),
        style::PINKY_UP if n >= 2 => {
            if i.is_multiple_of(2) {
                i / 2
            } else {
                n - 1
            }
        }
        style::THUMB_UP if n >= 2 => {
            if i.is_multiple_of(2) {
                0
            } else {
                i / 2 + 1
            }
        }
        style::PINKY_UP | style::THUMB_UP => 0,
        _ => i,
    }
}

fn choice(v: f32, len: usize) -> usize {
    (v.round().max(0.0) as usize).min(len - 1)
}

#[derive(Clone, Copy, Default)]
struct Held {
    pitch: u8,
    velocity: u8,
}

#[derive(Clone, Copy, Default)]
struct Sounding {
    pitch: u8,
    /// Beat at which the note ends.
    off: f64,
}

pub struct Arpeggiator {
    rng: Rng,
    /// Notes being arpeggiated, in press order.
    notes: [Held; MAX_NOTES],
    n_notes: usize,
    /// Keys physically down (for Hold).
    pressed: [bool; 128],
    n_pressed: usize,
    sounding: [Sounding; MAX_SOUNDING],
    n_sounding: usize,
    running: bool,
    /// Frame of a step to play at once (the first step after a start).
    start_frame: Option<usize>,
    /// Beat of the next step and of the last one.
    next_step: f64,
    last_step: f64,
    /// Step length the schedule was made with.
    rate: f64,
    /// Position in the current pass, the pass (transposition) number and the
    /// steps since the pattern started (for velocity decay).
    pos: usize,
    pass: usize,
    decay_steps: i32,
    /// Where the clock should be at the start of the next call.
    expected_beat: Option<f64>,
}

impl Arpeggiator {
    pub fn new(seed: u32) -> Self {
        Self {
            rng: Rng::new(seed),
            notes: [Held::default(); MAX_NOTES],
            n_notes: 0,
            pressed: [false; 128],
            n_pressed: 0,
            sounding: [Sounding::default(); MAX_SOUNDING],
            n_sounding: 0,
            running: false,
            start_frame: None,
            next_step: 0.0,
            last_step: 0.0,
            rate: RATE_BEATS[2],
            pos: 0,
            pass: 0,
            decay_steps: 0,
            expected_beat: None,
        }
    }

    pub fn reset(&mut self) {
        self.clear();
        self.expected_beat = None;
    }

    /// Forgets notes and stops (without sending anything).
    fn clear(&mut self) {
        self.n_notes = 0;
        self.pressed = [false; 128];
        self.n_pressed = 0;
        self.n_sounding = 0;
        self.running = false;
        self.start_frame = None;
        self.restart_pattern();
    }

    fn restart_pattern(&mut self) {
        self.pos = 0;
        self.pass = 0;
        self.decay_steps = 0;
    }

    /// Releases the sounding notes at `frame` and stops.
    fn stop(&mut self, frame: usize, out: &mut Vec<MidiEvent>) {
        for s in &self.sounding[..self.n_sounding] {
            emit(out, frame, EventKind::Off(s.pitch));
        }
        self.n_sounding = 0;
        self.running = false;
        self.start_frame = None;
        self.restart_pattern();
    }

    fn remove_note(&mut self, i: usize) {
        self.notes.copy_within(i + 1..self.n_notes, i);
        self.n_notes -= 1;
    }

    /// Drops latched notes whose keys are up (Hold switched off).
    fn prune_released(&mut self) {
        let mut i = 0;
        while i < self.n_notes {
            if self.pressed[self.notes[i].pitch as usize & 127] {
                i += 1;
            } else {
                self.remove_note(i);
            }
        }
    }

    /// First frame at or after `beat` (may be past the segment).
    fn frame_at(beat: f64, ctx: &MidiFxContext) -> usize {
        let f = ((beat - ctx.beat0) / ctx.bpf - 1e-6).ceil();
        if f.is_nan() || f <= 0.0 {
            0
        } else {
            f.min(usize::MAX as f64 / 2.0) as usize
        }
    }

    fn note_on(&mut self, p: &ArpeggiatorParams, key: u8, vel: u8, f: usize, ctx: &MidiFxContext) {
        let hold = p.hold >= 0.5;
        if hold && self.n_pressed == 0 && self.n_notes > 0 {
            // First key after all were released replaces the latched notes.
            self.n_notes = 0;
            self.restart_pattern();
        }
        let k = key as usize & 127;
        if !self.pressed[k] {
            self.pressed[k] = true;
            self.n_pressed += 1;
        }
        match self.notes[..self.n_notes].iter_mut().find(|n| n.pitch == key) {
            Some(n) => n.velocity = vel,
            None if self.n_notes < MAX_NOTES => {
                self.notes[self.n_notes] = Held {
                    pitch: key,
                    velocity: vel,
                };
                self.n_notes += 1;
            }
            None => {}
        }
        if !self.running && self.n_notes > 0 {
            // Start at once; on the grid if the note is (within a frame).
            self.running = true;
            self.restart_pattern();
            let beat = ctx.beat0 + f as f64 * ctx.bpf;
            let grid = (beat / self.rate).round() * self.rate;
            self.next_step = if (beat - grid).abs() <= 1.5 * ctx.bpf {
                grid
            } else {
                beat
            };
            self.start_frame = Some(f);
        }
    }

    fn note_off(&mut self, p: &ArpeggiatorParams, key: u8, f: usize, out: &mut Vec<MidiEvent>) {
        let k = key as usize & 127;
        if self.pressed[k] {
            self.pressed[k] = false;
            self.n_pressed -= 1;
        }
        if p.hold < 0.5 {
            if let Some(i) = self.notes[..self.n_notes].iter().position(|n| n.pitch == key) {
                self.remove_note(i);
            }
            if self.n_notes == 0 && self.running {
                self.stop(f, out);
            }
        }
    }

    /// Starts `pitch` at `frame` until beat `off`.
    fn sound(&mut self, pitch: u8, velocity: u8, frame: usize, off: f64, out: &mut Vec<MidiEvent>) {
        // A pitch still sounding from an earlier step (long gate) ends first,
        // or its pending note-off would cut the new note.
        if let Some(i) = self.sounding[..self.n_sounding].iter().position(|s| s.pitch == pitch) {
            emit(out, frame, EventKind::Off(pitch));
            self.n_sounding -= 1;
            self.sounding[i] = self.sounding[self.n_sounding];
        }
        if self.n_sounding == MAX_SOUNDING {
            let i = (0..self.n_sounding)
                .min_by(|&a, &b| self.sounding[a].off.total_cmp(&self.sounding[b].off))
                .unwrap_or(0);
            emit(out, frame, EventKind::Off(self.sounding[i].pitch));
            self.n_sounding -= 1;
            self.sounding[i] = self.sounding[self.n_sounding];
        }
        emit(out, frame, EventKind::On(pitch, velocity));
        self.sounding[self.n_sounding] = Sounding { pitch, off };
        self.n_sounding += 1;
    }

    /// Plays the step due at `frame`.
    fn step(&mut self, p: &ArpeggiatorParams, frame: usize, ctx: &MidiFxContext, out: &mut Vec<MidiEvent>) {
        let beat = self.next_step;
        self.last_step = beat;
        self.next_step = beat + self.rate;
        let n = self.n_notes;
        if n == 0 {
            return;
        }
        let style = choice(p.style, STYLES.len());
        let len = pattern_len(style, n);
        if self.pos >= len {
            self.pos = 0;
            self.pass += 1;
            if self.pass > p.steps.round().max(0.0) as usize {
                self.pass = 0;
                self.decay_steps = 0;
            }
        }
        let i = self.pos;
        self.pos += 1;

        let transpose = self.pass as i32 * p.distance.round() as i32;
        let gain = (1.0 - DECAY_PER_STEP * p.velocity_decay.clamp(0.0, 1.0)).powi(self.decay_steps);
        self.decay_steps = self.decay_steps.saturating_add(1);
        // Notes end at least one frame after they start.
        let off = (beat + p.gate.max(0.01) as f64 * self.rate).max(ctx.beat0 + (frame + 1) as f64 * ctx.bpf);

        // Notes sorted by pitch (insertion sort on a stack copy).
        let mut sorted = [Held::default(); MAX_NOTES];
        sorted[..n].copy_from_slice(&self.notes[..n]);
        for a in 1..n {
            let mut b = a;
            while b > 0 && sorted[b - 1].pitch > sorted[b].pitch {
                sorted.swap(b - 1, b);
                b -= 1;
            }
        }

        let (from, to) = match style {
            style::CHORD => (0, n),
            style::PLAYED => {
                sorted[0] = self.notes[i % n];
                (0, 1)
            }
            style::RANDOM => {
                let r = (self.rng.next_unit() * n as f32) as usize % n;
                (r, r + 1)
            }
            _ => {
                let j = pattern_index(style, n, i).min(n - 1);
                (j, j + 1)
            }
        };
        for h in sorted[from..to].iter().copied() {
            let pitch = h.pitch as i32 + transpose;
            if !(0..=127).contains(&pitch) {
                continue;
            }
            let v = ((h.velocity as f32 * gain).round() as i32).clamp(1, 127) as u8;
            self.sound(pitch as u8, v, frame, off, out);
        }
    }

    pub fn process(
        &mut self,
        p: &ArpeggiatorParams,
        input: &[MidiEvent],
        out: &mut Vec<MidiEvent>,
        ctx: &MidiFxContext,
    ) {
        let frames = ctx.frames;
        if frames == 0 {
            return;
        }
        let clock_ok = ctx.bpf > 0.0 && ctx.bpf.is_finite() && ctx.beat0.is_finite();
        let rate = RATE_BEATS[choice(p.rate, RATE_BEATS.len())];

        // Clock jumps: keep sounding notes their remaining length and resync
        // the next step to the grid.
        if let Some(expected) = self.expected_beat {
            let delta = ctx.beat0 - expected;
            if clock_ok && delta.abs() > JUMP_TOLERANCE {
                for s in self.sounding[..self.n_sounding].iter_mut() {
                    s.off += delta;
                }
                if self.running {
                    self.next_step = ((ctx.beat0 / rate) - 1e-9).ceil() * rate;
                    self.last_step = self.next_step - rate;
                    self.rate = rate;
                }
            }
        }
        self.expected_beat = clock_ok.then(|| ctx.beat1());
        if rate != self.rate {
            if self.running && self.start_frame.is_none() {
                // Continue from the last step at the new rate, skipping
                // points already past.
                let k = ((ctx.beat0 - self.last_step) / rate - 1e-9).ceil().max(1.0);
                self.next_step = self.last_step + k * rate;
            }
            self.rate = rate;
        }
        if p.hold < 0.5 && self.n_notes > 0 {
            self.prune_released();
            if self.n_notes == 0 && self.running {
                self.stop(0, out);
            }
        }

        let mut i = 0;
        // Bounds the work per call whatever the clock does.
        let mut budget = 4 * MAX_SOUNDING + 2 * input.len() + 1024;
        while budget > 0 {
            budget -= 1;
            let ev_f = input.get(i).map_or(usize::MAX, |e| (e.frame as usize).min(frames - 1));
            let off_f = if clock_ok {
                self.sounding[..self.n_sounding]
                    .iter()
                    .map(|s| Self::frame_at(s.off, ctx))
                    .min()
                    .unwrap_or(usize::MAX)
            } else {
                usize::MAX
            };
            let step_f = match (self.running, self.start_frame) {
                (true, Some(f)) => f,
                (true, None) if clock_ok => Self::frame_at(self.next_step, ctx),
                _ => usize::MAX,
            };
            let f = ev_f.min(off_f).min(step_f);
            if f >= frames {
                break;
            }
            if ev_f == f {
                // Input first, so notes arriving with a step join it.
                match input[i].kind {
                    EventKind::On(key, vel) => self.note_on(p, key, vel, f, ctx),
                    EventKind::Off(key) => self.note_off(p, key, f, out),
                    EventKind::AllOff => {
                        self.clear();
                        emit(out, f, EventKind::AllOff);
                    }
                }
                i += 1;
            } else if off_f == f {
                let mut k = 0;
                while k < self.n_sounding {
                    if Self::frame_at(self.sounding[k].off, ctx) <= f {
                        emit(out, f, EventKind::Off(self.sounding[k].pitch));
                        self.n_sounding -= 1;
                        self.sounding[k] = self.sounding[self.n_sounding];
                    } else {
                        k += 1;
                    }
                }
            } else {
                self.start_frame = None;
                self.step(p, f, ctx, out);
            }
        }
        if budget == 0 && self.running && clock_ok {
            // Way behind (should not happen): skip to the next grid point.
            self.next_step = (ctx.beat1() / self.rate).ceil() * self.rate;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_util::*;
    use super::*;

    const BLOCK: usize = 256;
    /// Frames per sixteenth at 120 BPM / 48 kHz.
    const SIXTEENTH: u64 = 6000;

    struct Rig {
        arp: Arpeggiator,
        p: ArpeggiatorParams,
        block: u64,
        beat_offset: f64,
        log: Vec<(u64, EventKind)>,
    }

    impl Rig {
        fn new(p: ArpeggiatorParams) -> Self {
            Self {
                arp: Arpeggiator::new(1),
                p,
                block: 0,
                beat_offset: 0.0,
                log: Vec::new(),
            }
        }

        /// Runs one block with `input` (frames inside the block).
        fn run_block(&mut self, input: &[MidiEvent]) {
            let mut c = ctx(BLOCK);
            let start = self.block * BLOCK as u64;
            c.beat0 = start as f64 * c.bpf + self.beat_offset;
            let mut out = buf();
            self.arp.process(&self.p, input, &mut out, &c);
            for e in &out {
                assert!((e.frame as usize) < BLOCK, "frame {}", e.frame);
                self.log.push((start + e.frame as u64, e.kind));
            }
            self.block += 1;
        }

        fn run_until(&mut self, frame: u64) {
            while (self.block + 1) * (BLOCK as u64) <= frame {
                self.run_block(&[]);
            }
        }

        /// Feeds events at absolute frames, running blocks up to them.
        fn feed(&mut self, events: &[(u64, EventKind)]) {
            let mut k = 0;
            while k < events.len() {
                let start = self.block * BLOCK as u64;
                let end = start + BLOCK as u64;
                let mut chunk = Vec::new();
                while k < events.len() && events[k].0 < end {
                    chunk.push(MidiEvent {
                        frame: (events[k].0 - start) as u32,
                        kind: events[k].1,
                    });
                    k += 1;
                }
                self.run_block(&chunk);
            }
        }

        fn ons(&self) -> Vec<(u64, u8)> {
            self.log
                .iter()
                .filter_map(|(f, k)| match k {
                    EventKind::On(p, _) => Some((*f, *p)),
                    _ => None,
                })
                .collect()
        }

        fn pitches(&self) -> Vec<u8> {
            self.ons().into_iter().map(|(_, p)| p).collect()
        }

        /// Every note-on is matched by a later note-off and none overlap
        /// on the same pitch.
        fn assert_balanced(&self) {
            let mut on = [false; 128];
            for (_, k) in &self.log {
                match *k {
                    EventKind::On(p, _) => {
                        assert!(!on[p as usize], "pitch {p} started twice");
                        on[p as usize] = true;
                    }
                    EventKind::Off(p) => on[p as usize] = false,
                    EventKind::AllOff => on = [false; 128],
                }
            }
            assert!(on.iter().all(|o| !o), "notes left hanging");
        }
    }

    fn params(style: usize) -> ArpeggiatorParams {
        ArpeggiatorParams {
            style: style as f32,
            ..Default::default()
        }
    }

    fn chord_on(at: u64, pitches: &[u8]) -> Vec<(u64, EventKind)> {
        pitches.iter().map(|&p| (at, EventKind::On(p, 100))).collect()
    }

    fn chord_off(at: u64, pitches: &[u8]) -> Vec<(u64, EventKind)> {
        pitches.iter().map(|&p| (at, EventKind::Off(p))).collect()
    }

    /// Plays a C major chord (given in an odd press order) for `steps`
    /// sixteenths and returns the arpeggiated pitches.
    fn pattern(p: ArpeggiatorParams, steps: u64) -> Vec<u8> {
        let mut rig = Rig::new(p);
        rig.feed(&chord_on(0, &[64, 60, 67]));
        rig.run_until(steps * SIXTEENTH - 1);
        let end = rig.block * BLOCK as u64;
        rig.feed(&chord_off(end, &[64, 60, 67]));
        rig.run_until(end + 4 * SIXTEENTH);
        rig.assert_balanced();
        rig.pitches()
    }

    #[test]
    fn styles_produce_their_patterns() {
        let cases: [(usize, &[u8]); 10] = [
            (style::UP, &[60, 64, 67, 60, 64, 67]),
            (style::DOWN, &[67, 64, 60, 67, 64, 60]),
            (style::UP_DOWN, &[60, 64, 67, 64, 60, 64]),
            (style::DOWN_UP, &[67, 64, 60, 64, 67, 64]),
            (style::UP_AND_DOWN, &[60, 64, 67, 67, 64, 60]),
            (style::DOWN_AND_UP, &[67, 64, 60, 60, 64, 67]),
            (style::CONVERGE, &[60, 67, 64, 60, 67, 64]),
            (style::PINKY_UP, &[60, 67, 64, 67, 60, 67]),
            (style::THUMB_UP, &[60, 64, 60, 67, 60, 64]),
            (style::PLAYED, &[64, 60, 67, 64, 60, 67]),
        ];
        for (s, want) in cases {
            assert_eq!(pattern(params(s), 6), want, "{}", STYLES[s]);
        }
        assert_eq!(pattern(params(style::DIVERGE), 3), vec![64, 67, 60]);
    }

    #[test]
    fn steps_are_on_the_rate_grid_with_gate_length() {
        let mut rig = Rig::new(params(style::UP));
        rig.feed(&chord_on(0, &[60, 64]));
        rig.run_until(4 * SIXTEENTH - 1);
        let ons = rig.ons();
        assert_eq!(ons, vec![(0, 60), (6000, 64), (12000, 60), (18000, 64)]);
        // Gate 50%: each note lasts half a step.
        assert!(rig.log.contains(&(3000, EventKind::Off(60))));
        assert!(rig.log.contains(&(9000, EventKind::Off(64))));
    }

    #[test]
    fn first_step_plays_at_an_off_grid_note_and_continues_from_there() {
        let mut rig = Rig::new(params(style::UP));
        rig.feed(&[(1000, EventKind::On(60, 100))]);
        rig.run_until(1000 + 2 * SIXTEENTH + 300);
        let frames: Vec<u64> = rig.ons().iter().map(|(f, _)| *f).collect();
        assert_eq!(frames, vec![1000, 7000, 13000]);
    }

    #[test]
    fn chord_style_plays_all_notes_each_step() {
        let p = ArpeggiatorParams {
            style: style::CHORD as f32,
            rate: 4.0, // 1/8
            ..Default::default()
        };
        let mut rig = Rig::new(p);
        rig.feed(&chord_on(0, &[60, 64, 67]));
        rig.run_until(2 * 12000 - 1);
        let ons = rig.ons();
        assert_eq!(ons.len(), 6);
        assert!(ons[..3].iter().all(|(f, _)| *f == 0));
        assert!(ons[3..].iter().all(|(f, _)| *f == 12000));
    }

    #[test]
    fn steps_and_distance_transpose_passes() {
        let p = ArpeggiatorParams {
            steps: 2.0,
            distance: 12.0,
            ..Default::default()
        };
        assert_eq!(pattern(p, 8), vec![60, 64, 67, 72, 76, 79, 84, 88]);
        let p = ArpeggiatorParams {
            steps: 1.0,
            distance: -5.0,
            ..Default::default()
        };
        assert_eq!(pattern(p, 7), vec![60, 64, 67, 55, 59, 62, 60]);
    }

    #[test]
    fn velocity_decays_and_resets_with_the_pattern() {
        let p = ArpeggiatorParams {
            velocity_decay: 1.0,
            ..Default::default()
        };
        let mut rig = Rig::new(p);
        rig.feed(&chord_on(0, &[60, 64]));
        rig.run_until(4 * SIXTEENTH - 1);
        let vels: Vec<u8> = rig
            .log
            .iter()
            .filter_map(|(_, k)| match k {
                EventKind::On(_, v) => Some(*v),
                _ => None,
            })
            .collect();
        assert_eq!(vels, vec![100, 90, 100, 90]);
    }

    #[test]
    fn releasing_all_notes_stops_and_offs_are_consumed() {
        let mut rig = Rig::new(params(style::UP));
        rig.feed(&[(0, EventKind::On(60, 100))]);
        rig.run_until(2000);
        rig.feed(&[(2100, EventKind::Off(60))]);
        rig.run_until(10 * SIXTEENTH);
        assert_eq!(rig.ons().len(), 1);
        // The sounding note is released when the key is.
        assert_eq!(
            rig.log.iter().filter(|(_, k)| *k == EventKind::Off(60)).count(),
            1,
            "{:?}",
            rig.log
        );
        assert!(rig.log.contains(&(2100, EventKind::Off(60))));
        rig.assert_balanced();
    }

    #[test]
    fn hold_keeps_playing_until_a_new_press() {
        let p = ArpeggiatorParams {
            hold: 1.0,
            ..Default::default()
        };
        let mut rig = Rig::new(p);
        rig.feed(&chord_on(0, &[60, 64]));
        rig.feed(&chord_off(3000, &[60, 64]));
        rig.run_until(27000 - 1);
        assert_eq!(rig.pitches(), vec![60, 64, 60, 64, 60]);
        // A new key after all were released replaces the latched notes.
        rig.feed(&[(27000, EventKind::On(70, 100))]);
        rig.run_until(48000 - 1);
        assert_eq!(&rig.pitches()[5..], &[70, 70, 70]);
        // Switching Hold off with no keys down stops.
        rig.feed(&[(48005, EventKind::Off(70))]);
        rig.p.hold = 0.0;
        let n = rig.ons().len();
        rig.run_until(20 * SIXTEENTH);
        assert_eq!(rig.ons().len(), n);
        rig.assert_balanced();
    }

    #[test]
    fn long_gates_overlap_and_repeated_pitches_retrigger_cleanly() {
        let p = ArpeggiatorParams {
            gate: 1.5,
            ..Default::default()
        };
        let mut rig = Rig::new(p);
        rig.feed(&[(0, EventKind::On(60, 100))]);
        rig.run_until(4 * SIXTEENTH);
        rig.feed(&[(4 * SIXTEENTH + 300, EventKind::Off(60))]);
        rig.run_until(8 * SIXTEENTH);
        assert_eq!(rig.ons().len(), 5);
        rig.assert_balanced();

        let mut rig = Rig::new(p);
        rig.feed(&chord_on(0, &[60, 64]));
        rig.run_until(9300);
        // 60 still sounds when 64 starts.
        let i60 = rig.log.iter().position(|e| *e == (9000, EventKind::Off(60)));
        assert!(i60.is_some(), "{:?}", rig.log);
    }

    #[test]
    fn clock_jumps_resync_to_the_grid() {
        let mut rig = Rig::new(params(style::UP));
        rig.feed(&[(0, EventKind::On(60, 100))]);
        rig.run_until(SIXTEENTH * 2 - 1);
        assert_eq!(rig.ons().len(), 2);
        // Relocate the clock by 10.1 beats: the next step falls on the next
        // sixteenth of the new position, not in a burst or never.
        rig.beat_offset = 10.1;
        let jump_frame = rig.block * BLOCK as u64;
        rig.run_until(jump_frame + 2 * SIXTEENTH);
        let ons = rig.ons();
        assert!(ons.len() == 4 || ons.len() == 5, "{ons:?}");
        let c = ctx(BLOCK);
        let beat = |f: u64| f as f64 * c.bpf + 10.1;
        for (f, _) in &ons[2..] {
            let b = beat(*f);
            let off_grid = (b / 0.25 - (b / 0.25).round()).abs() * 0.25;
            assert!(off_grid <= 2.0 * c.bpf, "step at beat {b}");
        }
        rig.assert_balanced_after_off();
    }

    impl Rig {
        fn assert_balanced_after_off(&mut self) {
            let at = self.block * BLOCK as u64;
            self.feed(&[(at, EventKind::Off(60))]);
            self.run_until(at + 4 * SIXTEENTH);
            self.assert_balanced();
        }
    }

    #[test]
    fn rate_and_tempo_changes_keep_stepping() {
        let mut rig = Rig::new(params(style::UP));
        rig.feed(&chord_on(0, &[60, 64]));
        rig.run_until(2 * SIXTEENTH - 1);
        rig.p.rate = 0.0; // 1/32
        rig.run_until(4 * SIXTEENTH - 1);
        // 2 sixteenths, then 4 thirty-seconds.
        let frames: Vec<u64> = rig.ons().iter().map(|(f, _)| *f).collect();
        assert_eq!(frames, vec![0, 6000, 12000, 15000, 18000, 21000]);
        rig.assert_balanced_after_off();
    }

    #[test]
    fn all_off_is_forwarded_and_stops() {
        let mut rig = Rig::new(params(style::UP));
        rig.feed(&chord_on(0, &[60, 64]));
        rig.run_until(SIXTEENTH);
        rig.feed(&[(SIXTEENTH + 500, EventKind::AllOff)]);
        let n = rig.ons().len();
        rig.run_until(10 * SIXTEENTH);
        assert_eq!(rig.ons().len(), n);
        assert!(rig.log.contains(&(SIXTEENTH + 500, EventKind::AllOff)));

        // reset() forgets everything too.
        let mut rig = Rig::new(params(style::UP));
        rig.feed(&chord_on(0, &[60, 64]));
        rig.arp.reset();
        let n = rig.ons().len();
        rig.run_until(10 * SIXTEENTH);
        assert_eq!(rig.ons().len(), n);
    }

    #[test]
    fn random_style_plays_held_notes() {
        let mut p = params(style::RANDOM);
        p.gate = 0.3;
        let pitches = pattern(p, 64);
        assert_eq!(pitches.len(), 64);
        for want in [60, 64, 67] {
            assert!(pitches.contains(&want));
        }
        assert!(pitches.iter().all(|p| [60, 64, 67].contains(p)));
    }
}
