//! Capture MIDI: everything played on a MIDI track is remembered, recording
//! or not, so it can be turned into a clip afterwards.

use crate::model::Note;

/// How long played notes are kept, in seconds.
pub const CAPTURE_SECS: f64 = 300.0;
/// A pause this long (seconds) separates phrases: Capture keeps only the
/// last phrase.
pub const PHRASE_GAP_SECS: f64 = 4.0;

/// A note that was played: times in seconds (wall clock) and, when the
/// transport was running, in song beats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayedNote {
    pub pitch: u8,
    pub velocity: u8,
    pub on_secs: f64,
    pub off_secs: Option<f64>,
    pub on_beat: Option<f64>,
    pub off_beat: Option<f64>,
}

/// The result of a capture.
#[derive(Debug, Clone, PartialEq)]
pub struct Captured {
    /// Notes relative to the clip start, in beats.
    pub notes: Vec<Note>,
    /// Clip length in beats (whole bars).
    pub length: f64,
    /// Tempo detected when the transport was stopped while playing.
    pub tempo: Option<f64>,
    /// Song beat the clip belongs at when played with the transport
    /// running (bar aligned).
    pub start_beat: Option<f64>,
}

/// Remembers played notes per track (the caller keeps one per track).
#[derive(Debug, Clone, Default)]
pub struct CaptureBuffer {
    notes: Vec<PlayedNote>,
}

impl CaptureBuffer {
    /// A note started. `beat` is the song position if the transport plays.
    pub fn note_on(&mut self, pitch: u8, velocity: u8, secs: f64, beat: Option<f64>) {
        self.notes.retain(|n| secs - n.on_secs < CAPTURE_SECS);
        self.notes.push(PlayedNote {
            pitch,
            velocity: velocity.max(1),
            on_secs: secs,
            off_secs: None,
            on_beat: beat,
            off_beat: None,
        });
    }

    /// A note ended.
    pub fn note_off(&mut self, pitch: u8, secs: f64, beat: Option<f64>) {
        if let Some(n) = self
            .notes
            .iter_mut()
            .rev()
            .find(|n| n.pitch == pitch && n.off_secs.is_none())
        {
            n.off_secs = Some(secs);
            n.off_beat = beat;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn clear(&mut self) {
        self.notes.clear();
    }

    /// The last phrase (notes after the last long pause).
    fn phrase(&self) -> &[PlayedNote] {
        let mut start = 0;
        for i in 1..self.notes.len() {
            let prev_end = self.notes[..i]
                .iter()
                .map(|n| n.off_secs.unwrap_or(n.on_secs))
                .fold(f64::MIN, f64::max);
            if self.notes[i].on_secs - prev_end > PHRASE_GAP_SECS {
                start = i;
            }
        }
        &self.notes[start..]
    }

    /// Turns the last phrase into a clip. With the transport running the
    /// notes keep their song positions (the clip starts on the bar before
    /// the first note); played while stopped, the tempo is estimated from
    /// the notes and the first note starts the clip. `now` closes notes
    /// still held. Returns `None` when nothing was played.
    pub fn capture(&self, now: f64, project_tempo: f64, beats_per_bar: f64) -> Option<Captured> {
        let phrase = self.phrase();
        if phrase.is_empty() {
            return None;
        }
        let bpb = beats_per_bar.max(1.0);
        let synced = phrase.iter().all(|n| n.on_beat.is_some());
        let (mut notes, tempo, start_beat) = if synced {
            let first = phrase.iter().filter_map(|n| n.on_beat).fold(f64::MAX, f64::min);
            let start = (first / bpb).floor() * bpb;
            let spb = 60.0 / project_tempo;
            let notes = phrase
                .iter()
                .map(|n| {
                    let on = n.on_beat.unwrap_or(0.0);
                    let off = n
                        .off_beat
                        .unwrap_or_else(|| on + (n.off_secs.unwrap_or(now) - n.on_secs) / spb);
                    Note {
                        pitch: n.pitch,
                        velocity: n.velocity,
                        start: on - start,
                        length: (off - on).max(1.0 / 64.0),
                    }
                })
                .collect::<Vec<_>>();
            (notes, None, Some(start))
        } else {
            let first = phrase[0].on_secs;
            let tempo = estimate_tempo(phrase).unwrap_or(project_tempo);
            let bps = tempo / 60.0;
            let notes = phrase
                .iter()
                .map(|n| Note {
                    pitch: n.pitch,
                    velocity: n.velocity,
                    start: (n.on_secs - first) * bps,
                    length: ((n.off_secs.unwrap_or(now) - n.on_secs) * bps).max(1.0 / 64.0),
                })
                .collect::<Vec<_>>();
            (notes, Some(tempo), None)
        };
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        let end = notes.iter().map(|n| n.start + n.length).fold(0.0, f64::max);
        // Unsynced phrases end on the bar the last note started in when they
        // only spill over a little.
        let last_start = notes.last().map(|n| n.start).unwrap_or(0.0);
        let bars = if synced {
            (end / bpb).ceil()
        } else {
            ((last_start + 1e-6) / bpb).floor() + 1.0
        };
        let length = bars.max(1.0) * bpb;
        Some(Captured {
            notes,
            length,
            tempo,
            start_beat,
        })
    }
}

/// Estimates a tempo (70..=160 BPM) from note onsets played freely: the
/// beat length that best fits the gaps between onsets.
pub fn estimate_tempo(notes: &[PlayedNote]) -> Option<f64> {
    let mut onsets: Vec<f64> = notes.iter().map(|n| n.on_secs).collect();
    onsets.sort_by(f64::total_cmp);
    onsets.dedup_by(|a, b| (*a - *b).abs() < 0.03);
    if onsets.len() < 3 {
        return None;
    }
    let mut best = (f64::MAX, None);
    let mut bpm = 70.0;
    while bpm <= 160.0 {
        // Score: how far each onset lies from a 1/8-note grid starting at
        // the first onset (normalised), slightly favouring slower tempos
        // to avoid doubling.
        let grid = 30.0 / bpm;
        let err: f64 = onsets
            .iter()
            .map(|t| {
                let x = (t - onsets[0]) / grid;
                (x - x.round()).abs()
            })
            .sum::<f64>()
            / onsets.len() as f64;
        let score = err + bpm * 1e-4;
        if score < best.0 {
            best = (score, Some(bpm));
        }
        bpm += 0.5;
    }
    best.1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synced_notes_keep_song_positions() {
        let mut c = CaptureBuffer::default();
        // Two notes in bar 2 at 120 BPM.
        c.note_on(60, 100, 10.0, Some(5.0));
        c.note_off(60, 10.25, Some(5.5));
        c.note_on(64, 90, 10.5, Some(6.0));
        c.note_off(64, 11.0, Some(7.0));
        let cap = c.capture(12.0, 120.0, 4.0).unwrap();
        assert_eq!(cap.start_beat, Some(4.0));
        assert_eq!(cap.tempo, None);
        assert_eq!(cap.length, 4.0);
        assert_eq!(cap.notes[0].start, 1.0);
        assert_eq!(cap.notes[0].length, 0.5);
        assert_eq!(cap.notes[1].pitch, 64);
    }

    #[test]
    fn free_playing_detects_tempo() {
        let mut c = CaptureBuffer::default();
        // Quarter notes at 100 BPM (0.6 s apart).
        for i in 0..8 {
            let t = 1.0 + i as f64 * 0.6;
            c.note_on(60 + i as u8, 100, t, None);
            c.note_off(60 + i as u8, t + 0.3, None);
        }
        let cap = c.capture(10.0, 120.0, 4.0).unwrap();
        let tempo = cap.tempo.unwrap();
        // 100 BPM (or a grid-compatible equivalent like 150 would put
        // quarters on dotted grid points; 100 must win).
        assert!((tempo - 100.0).abs() < 1.0, "{tempo}");
        assert!((cap.notes[1].start - 1.0).abs() < 0.02);
        assert_eq!(cap.length, 8.0);
    }

    #[test]
    fn only_the_last_phrase_is_captured() {
        let mut c = CaptureBuffer::default();
        c.note_on(40, 100, 0.0, None);
        c.note_off(40, 0.5, None);
        c.note_on(60, 100, 20.0, None);
        c.note_off(60, 20.5, None);
        let cap = c.capture(21.0, 120.0, 4.0).unwrap();
        assert_eq!(cap.notes.len(), 1);
        assert_eq!(cap.notes[0].pitch, 60);
        assert!(CaptureBuffer::default().capture(0.0, 120.0, 4.0).is_none());
    }
}
