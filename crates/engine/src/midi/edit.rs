//! Editing operations on MIDI notes, used by the piano roll. Each works on a
//! clip's notes and a selection (indices into them).

use crate::model::Note;

/// Shortest note the editor creates, in beats (1/128 note).
pub const MIN_LENGTH: f64 = 1.0 / 32.0;

/// Quantize settings (Ableton's Ctrl+Shift+U dialog).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quantize {
    /// Grid in beats (0.25 = 1/16).
    pub grid: f64,
    /// 0 ..= 1: how far notes move towards the grid.
    pub strength: f64,
    /// 0 ..= 1: delays every other grid point by up to two thirds of a grid
    /// step (0.5 is a triplet-like shuffle).
    pub swing: f64,
    /// Also quantize note ends.
    pub ends: bool,
}

impl Default for Quantize {
    fn default() -> Self {
        Self {
            grid: 0.25,
            strength: 1.0,
            swing: 0.0,
            ends: false,
        }
    }
}

impl Quantize {
    /// The grid point (with swing) nearest to `beat`.
    pub fn target(&self, beat: f64) -> f64 {
        let g = self.grid.max(1e-6);
        let base = (beat / g).floor();
        // Candidates around the beat: the two neighbouring grid points with
        // swing applied to odd ones.
        let point = |n: f64| {
            let odd = (n as i64).rem_euclid(2) == 1;
            n * g
                + if odd {
                    self.swing.clamp(0.0, 1.0) * g * 2.0 / 3.0
                } else {
                    0.0
                }
        };
        [base - 1.0, base, base + 1.0, base + 2.0]
            .into_iter()
            .map(point)
            .min_by(|a, b| (a - beat).abs().total_cmp(&(b - beat).abs()))
            .unwrap_or(beat)
    }

    fn apply(&self, beat: f64) -> f64 {
        beat + (self.target(beat) - beat) * self.strength.clamp(0.0, 1.0)
    }
}

/// Quantizes the selected notes (all notes when `selection` is empty).
pub fn quantize(notes: &mut [Note], selection: &[usize], q: &Quantize) {
    for i in targets(notes.len(), selection) {
        let n = &mut notes[i];
        let end = n.start + n.length;
        n.start = q.apply(n.start).max(0.0);
        // Quantizing only starts keeps each note's length.
        if q.ends {
            n.length = (q.apply(end) - n.start).max(MIN_LENGTH);
        }
    }
}

/// Moves the selected notes by `semitones`. Does nothing (and returns false)
/// if any note would leave the MIDI range.
pub fn transpose(notes: &mut [Note], selection: &[usize], semitones: i32) -> bool {
    let idx: Vec<usize> = targets(notes.len(), selection).collect();
    if idx
        .iter()
        .any(|&i| !(0..=127).contains(&(notes[i].pitch as i32 + semitones)))
    {
        return false;
    }
    for i in idx {
        notes[i].pitch = (notes[i].pitch as i32 + semitones) as u8;
    }
    true
}

/// Moves the selected notes in time by `beats`, keeping them inside
/// `0 .. clip_len`. The whole selection moves together.
pub fn shift(notes: &mut [Note], selection: &[usize], beats: f64, clip_len: f64) {
    let idx: Vec<usize> = targets(notes.len(), selection).collect();
    let Some((first, last)) = bounds(notes, &idx) else {
        return;
    };
    let lo = -first;
    let hi = (clip_len - MIN_LENGTH - (last - first)).max(0.0) - first;
    let d = beats.clamp(lo.min(0.0), hi.max(0.0));
    for i in idx {
        notes[i].start += d;
    }
}

/// Changes the length of the selected notes by `beats`.
pub fn resize(notes: &mut [Note], selection: &[usize], beats: f64) {
    for i in targets(notes.len(), selection) {
        notes[i].length = (notes[i].length + beats).max(MIN_LENGTH);
    }
}

/// Earliest start and latest end of the given notes.
pub fn bounds(notes: &[Note], idx: &[usize]) -> Option<(f64, f64)> {
    idx.iter().filter_map(|&i| notes.get(i)).fold(None, |acc, n| {
        let (s, e) = (n.start, n.start + n.length);
        Some(match acc {
            None => (s, e),
            Some((a, b)) => (f64::min(a, s), f64::max(b, e)),
        })
    })
}

/// Extends each selected note to the start of the next note (in time) in
/// the selection; the last one keeps its length. With one or no selected
/// notes, every note is made legato.
pub fn legato(notes: &mut [Note], selection: &[usize]) {
    let mut idx: Vec<usize> = if selection.len() > 1 {
        selection.to_vec()
    } else {
        (0..notes.len()).collect()
    };
    idx.retain(|&i| i < notes.len());
    idx.sort_by(|&a, &b| notes[a].start.total_cmp(&notes[b].start));
    for w in 0..idx.len() {
        let start = notes[idx[w]].start;
        // The next note that starts later (chords share a start).
        if let Some(&next) = idx[w + 1..].iter().find(|&&j| notes[j].start > start + 1e-9) {
            notes[idx[w]].length = (notes[next].start - start).max(MIN_LENGTH);
        }
    }
}

/// Scales the selected notes' positions and lengths around `anchor` by
/// `factor` (note stretch).
pub fn stretch(notes: &mut [Note], selection: &[usize], anchor: f64, factor: f64) {
    let f = factor.max(0.01);
    for i in targets(notes.len(), selection) {
        let n = &mut notes[i];
        n.start = (anchor + (n.start - anchor) * f).max(0.0);
        n.length = (n.length * f).max(MIN_LENGTH);
    }
}

/// Copies the selected notes and places the copies right after the
/// selection (rounded up to `grid`). Returns the indices of the copies.
pub fn duplicate(notes: &mut Vec<Note>, selection: &[usize], grid: f64) -> Vec<usize> {
    let idx: Vec<usize> = selection.iter().copied().filter(|&i| i < notes.len()).collect();
    let Some((first, last)) = bounds(notes, &idx) else {
        return Vec::new();
    };
    let g = grid.max(1e-6);
    let span = (((last - first) / g) - 1e-9).ceil().max(1.0) * g;
    let start = notes.len();
    for i in idx {
        let n = notes[i];
        notes.push(Note {
            start: n.start + span,
            ..n
        });
    }
    (start..notes.len()).collect()
}

/// Sets the velocity of the selected notes.
pub fn set_velocity(notes: &mut [Note], selection: &[usize], velocity: u8) {
    for i in targets(notes.len(), selection) {
        notes[i].velocity = velocity.clamp(1, 127);
    }
}

/// Changes the velocity of the selected notes by `delta`.
pub fn add_velocity(notes: &mut [Note], selection: &[usize], delta: i32) {
    for i in targets(notes.len(), selection) {
        notes[i].velocity = (notes[i].velocity as i32 + delta).clamp(1, 127) as u8;
    }
}

/// Reverses the selected notes in time within their span.
pub fn reverse(notes: &mut [Note], selection: &[usize]) {
    let idx: Vec<usize> = targets(notes.len(), selection).collect();
    let Some((first, last)) = bounds(notes, &idx) else {
        return;
    };
    for i in idx {
        let n = &mut notes[i];
        n.start = first + (last - (n.start + n.length));
    }
}

/// Mirrors the selected notes' pitches between their lowest and highest.
pub fn invert(notes: &mut [Note], selection: &[usize]) {
    let idx: Vec<usize> = targets(notes.len(), selection).collect();
    let lo = idx.iter().map(|&i| notes[i].pitch).min();
    let hi = idx.iter().map(|&i| notes[i].pitch).max();
    if let (Some(lo), Some(hi)) = (lo, hi) {
        for i in idx {
            notes[i].pitch = hi - (notes[i].pitch - lo);
        }
    }
}

/// Removes the given notes and returns how many were removed.
pub fn delete(notes: &mut Vec<Note>, selection: &[usize]) -> usize {
    let mut idx: Vec<usize> = selection.iter().copied().filter(|&i| i < notes.len()).collect();
    idx.sort_unstable();
    idx.dedup();
    for &i in idx.iter().rev() {
        notes.remove(i);
    }
    idx.len()
}

/// Pitches used by the notes, highest first (the piano roll's Fold).
pub fn used_pitches(notes: &[Note]) -> Vec<u8> {
    let mut used = [false; 128];
    for n in notes {
        used[n.pitch as usize & 127] = true;
    }
    (0..128u8).rev().filter(|&p| used[p as usize]).collect()
}

/// Indices of notes that start inside `start .. end` (beats) with a pitch
/// in `lo ..= hi`, or overlap that time range (box selection).
pub fn in_box(notes: &[Note], start: f64, end: f64, lo: u8, hi: u8) -> Vec<usize> {
    let (s, e) = (start.min(end), start.max(end));
    notes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.pitch >= lo && n.pitch <= hi && n.start < e && n.start + n.length > s)
        .map(|(i, _)| i)
        .collect()
}

/// Notes cut or moved to fit inside a clip of `clip_len` beats: notes
/// starting at or after the end are dropped, notes running past it are
/// shortened.
pub fn fit_to_length(notes: &mut Vec<Note>, clip_len: f64) {
    notes.retain(|n| n.start < clip_len - 1e-9);
    for n in notes.iter_mut() {
        n.length = n.length.min(clip_len - n.start).max(MIN_LENGTH);
    }
}

/// Keeps the selected notes (all when `selection` is empty) inside a clip
/// of `clip_len` beats: notes starting at or after the end are removed and
/// the others are shortened to end by it. Returns the selection without the
/// removed notes and with the remaining indices shifted down.
pub fn keep_inside(notes: &mut Vec<Note>, selection: &[usize], clip_len: f64) -> Vec<usize> {
    let idx: Vec<usize> = targets(notes.len(), selection).collect();
    let outside: Vec<usize> = idx
        .iter()
        .copied()
        .filter(|&i| notes[i].start >= clip_len - 1e-9)
        .collect();
    for &i in &idx {
        let n = &mut notes[i];
        if n.start < clip_len - 1e-9 {
            n.length = n.length.min(clip_len - n.start).max(MIN_LENGTH);
        }
    }
    let len = notes.len();
    delete(notes, &outside);
    selection
        .iter()
        .copied()
        .filter(|i| *i < len && !outside.contains(i))
        .map(|i| i - outside.iter().filter(|&&o| o < i).count())
        .collect()
}

/// The selected notes relative to the earliest one, and the span they cover
/// rounded up to `grid` (for copy and paste).
pub fn copy(notes: &[Note], selection: &[usize], grid: f64) -> Option<(Vec<Note>, f64)> {
    let idx: Vec<usize> = selection.iter().copied().filter(|&i| i < notes.len()).collect();
    let (first, last) = bounds(notes, &idx)?;
    let g = grid.max(1e-6);
    let span = (((last - first) / g) - 1e-9).ceil().max(1.0) * g;
    let copied = idx
        .iter()
        .map(|&i| Note {
            start: notes[i].start - first,
            ..notes[i]
        })
        .collect();
    Some((copied, span))
}

/// Adds copied notes at `at` beats, kept inside `clip_len`. Returns the
/// indices of the pasted notes.
pub fn paste(notes: &mut Vec<Note>, copied: &[Note], at: f64, clip_len: f64) -> Vec<usize> {
    let first = notes.len();
    notes.extend(copied.iter().map(|n| Note {
        start: (n.start + at).max(0.0),
        ..*n
    }));
    let pasted: Vec<usize> = (first..notes.len()).collect();
    if pasted.is_empty() {
        return pasted;
    }
    keep_inside(notes, &pasted, clip_len)
}

/// Doubles a clip's loop: the notes starting inside `loop_start .. end` are
/// copied right after it. Returns the new end.
pub fn double_loop(notes: &mut Vec<Note>, loop_start: f64, end: f64) -> f64 {
    let span = (end - loop_start).max(0.0);
    let copies: Vec<Note> = notes
        .iter()
        .filter(|n| n.start >= loop_start - 1e-9 && n.start < end - 1e-9)
        .map(|n| Note {
            start: n.start + span,
            ..*n
        })
        .collect();
    notes.extend(copies);
    end + span
}

fn targets(len: usize, selection: &[usize]) -> impl Iterator<Item = usize> + '_ {
    let all = selection.is_empty();
    (0..len).filter(move |i| all || selection.contains(i))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(pitch: u8, start: f64, length: f64) -> Note {
        Note {
            pitch,
            velocity: 100,
            start,
            length,
        }
    }

    #[test]
    fn quantize_strength_and_swing() {
        let mut v = vec![n(60, 0.1, 0.5), n(62, 0.27, 0.2)];
        quantize(&mut v, &[], &Quantize::default());
        assert!((v[0].start - 0.0).abs() < 1e-9);
        assert!((v[0].length - 0.5).abs() < 1e-9, "length kept");
        assert!((v[1].start - 0.25).abs() < 1e-9);

        let mut v = vec![n(60, 0.2, 0.5)];
        let half = Quantize {
            strength: 0.5,
            ..Default::default()
        };
        quantize(&mut v, &[], &half);
        assert!((v[0].start - 0.225).abs() < 1e-9);

        // Full swing pushes odd 16ths two thirds of a step late.
        let swung = Quantize {
            swing: 1.0,
            ..Default::default()
        };
        assert!((swung.target(0.4) - (0.25 + 0.25 * 2.0 / 3.0)).abs() < 1e-9);
        assert!((swung.target(0.52) - 0.5).abs() < 1e-9);

        let mut v = vec![n(60, 0.1, 0.3)];
        let ends = Quantize {
            ends: true,
            ..Default::default()
        };
        quantize(&mut v, &[], &ends);
        assert!((v[0].start).abs() < 1e-9 && (v[0].length - 0.5).abs() < 1e-9);
    }

    #[test]
    fn transpose_and_shift_respect_bounds() {
        let mut v = vec![n(126, 0.0, 1.0), n(60, 1.0, 1.0)];
        assert!(!transpose(&mut v, &[], 2));
        assert_eq!(v[0].pitch, 126);
        assert!(transpose(&mut v, &[1], 12));
        assert_eq!(v[1].pitch, 72);
        shift(&mut v, &[0, 1], -3.0, 4.0);
        assert_eq!(v[0].start, 0.0);
        shift(&mut v, &[1], 10.0, 4.0);
        assert!(v[1].start <= 4.0 - MIN_LENGTH + 1e-9);
    }

    #[test]
    fn legato_fills_gaps_and_keeps_chords() {
        let mut v = vec![n(60, 0.0, 0.25), n(64, 0.0, 0.25), n(67, 1.0, 0.25), n(72, 2.5, 0.25)];
        legato(&mut v, &[]);
        assert_eq!(v[0].length, 1.0);
        assert_eq!(v[1].length, 1.0);
        assert_eq!(v[2].length, 1.5);
        assert_eq!(v[3].length, 0.25);
    }

    #[test]
    fn stretch_duplicate_reverse_invert() {
        let mut v = vec![n(60, 1.0, 1.0), n(67, 2.0, 1.0)];
        stretch(&mut v, &[], 1.0, 2.0);
        assert_eq!((v[0].start, v[0].length), (1.0, 2.0));
        assert_eq!((v[1].start, v[1].length), (3.0, 2.0));

        let mut v = vec![n(60, 0.0, 0.5), n(62, 0.5, 0.5)];
        let copies = duplicate(&mut v, &[0, 1], 0.25);
        assert_eq!(copies, vec![2, 3]);
        assert_eq!(v[2].start, 1.0);
        assert_eq!(v[3].start, 1.5);

        let mut v = vec![n(60, 0.0, 0.5), n(64, 1.0, 1.0)];
        reverse(&mut v, &[]);
        assert_eq!(v[0].start, 1.5);
        assert_eq!(v[1].start, 0.0);
        invert(&mut v, &[]);
        assert_eq!((v[0].pitch, v[1].pitch), (64, 60));
    }

    #[test]
    fn selection_helpers() {
        let mut v = vec![n(60, 0.0, 1.0), n(64, 1.0, 1.0), n(60, 3.5, 1.0)];
        assert_eq!(used_pitches(&v), vec![64, 60]);
        assert_eq!(in_box(&v, 0.5, 2.0, 59, 70), vec![0, 1]);
        assert_eq!(in_box(&v, 0.5, 2.0, 61, 70), vec![1]);
        add_velocity(&mut v, &[0], 50);
        assert_eq!(v[0].velocity, 127);
        set_velocity(&mut v, &[1], 0);
        assert_eq!(v[1].velocity, 1);
        fit_to_length(&mut v, 4.0);
        assert_eq!(v[2].length, 0.5);
        assert_eq!(delete(&mut v, &[2, 0, 2]), 2);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].pitch, 64);
    }

    #[test]
    fn keep_inside_remaps_the_selection() {
        let mut v = vec![n(60, 0.0, 1.0), n(62, 4.0, 1.0), n(64, 3.5, 1.0), n(65, 5.0, 1.0)];
        let sel = keep_inside(&mut v, &[1, 2, 3], 4.0);
        assert_eq!(v.len(), 2);
        assert_eq!(sel, vec![1]);
        assert_eq!((v[1].pitch, v[1].length), (64, 0.5));
        // An empty selection means every note.
        let mut v = vec![n(60, 4.5, 1.0), n(62, 1.0, 9.0)];
        assert!(keep_inside(&mut v, &[], 4.0).is_empty());
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].length, 3.0);
    }

    #[test]
    fn copy_paste_and_double_loop() {
        let mut v = vec![n(60, 1.0, 0.5), n(64, 1.5, 0.25), n(67, 3.0, 1.0)];
        let (copied, span) = copy(&v, &[1, 0], 0.25).unwrap();
        assert_eq!(span, 0.75);
        assert_eq!(copied.iter().map(|c| c.start).collect::<Vec<_>>(), vec![0.5, 0.0]);
        assert!(copy(&v, &[], 0.25).is_none());
        let pasted = paste(&mut v, &copied, 3.5, 4.0);
        assert_eq!(pasted, vec![3]);
        assert_eq!((v[3].pitch, v[3].start, v[3].length), (60, 3.5, 0.5));
        assert_eq!(v.len(), 4, "the copy that would start at the end is dropped");

        let mut v = vec![n(60, 0.0, 1.0), n(62, 2.0, 1.0), n(64, 3.0, 1.0)];
        assert_eq!(double_loop(&mut v, 2.0, 4.0), 6.0);
        assert_eq!(v.len(), 5);
        assert_eq!((v[3].start, v[4].start), (4.0, 5.0));
    }
}
