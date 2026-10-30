//! Turns the engine's recording stream into audio clips.
//!
//! The audio thread streams raw input while anything records and marks where
//! each take starts and stops. Here, on the UI thread, takes are cut out of
//! that stream (dropping the input/output latency from the front), written to
//! 32-bit float WAV files and added to the project as clips. MIDI takes
//! arrive as the notes their track played and become MIDI clips.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use std::collections::HashMap;

use tessera_engine::engine::{EventKind, RecEvent, RecTarget};
use tessera_engine::model::{ArrangementClip, AudioData, AudioRef, Clip, ClipContent, Id, Note, TrackKind};
use tessera_engine::Command;

use crate::app::{ClipRef, TesseraApp};

#[derive(Debug, Clone, Copy)]
struct Take {
    track: Id,
    target: RecTarget,
    beat: f64,
    frame: u64,
    latency: u32,
}

#[derive(Debug, Clone, Copy)]
struct Finished {
    take: Take,
    stop_beat: f64,
    stop_frame: u64,
}

/// UI-side recording state.
#[derive(Default)]
pub struct TakeCollector {
    /// Interleaved stereo input received from the engine.
    stream: Vec<f32>,
    /// Stream frame index of `stream[0]`.
    base: u64,
    open: Vec<Take>,
    finished: Vec<Finished>,
    counter: u32,
    /// Notes played on MIDI tracks with a take, on the take's clock.
    midi: HashMap<Id, Vec<(f64, EventKind)>>,
}

/// Notes played before a quantized start by less than this still count
/// (snapped to the start), like playing slightly ahead of the beat.
const EARLY: f64 = 0.25;
/// Shortest recorded note, in beats.
const MIN_NOTE: f64 = 1.0 / 64.0;

/// Turns note-ons and offs played between `start` and `stop` into notes
/// relative to `start`.
fn notes_between(events: &[(f64, EventKind)], start: f64, stop: f64) -> Vec<Note> {
    let mut open: [Option<(f64, u8)>; 128] = [None; 128];
    let mut notes = Vec::new();
    let close = |open: &mut [Option<(f64, u8)>; 128], pitch: u8, at: f64, notes: &mut Vec<Note>| {
        if let Some((on, velocity)) = open[pitch as usize & 127].take() {
            let begin = on.max(start);
            notes.push(Note {
                pitch,
                velocity,
                start: begin - start,
                length: (at.min(stop) - begin).max(MIN_NOTE),
            });
        }
    };
    for &(beat, kind) in events {
        match kind {
            EventKind::On(pitch, velocity) => {
                close(&mut open, pitch, beat, &mut notes);
                if beat >= start - EARLY && beat < stop {
                    open[pitch as usize & 127] = Some((beat, velocity.max(1)));
                }
            }
            EventKind::Off(pitch) => close(&mut open, pitch, beat, &mut notes),
            EventKind::AllOff => {
                for p in 0..128u8 {
                    close(&mut open, p, beat, &mut notes);
                }
            }
        }
    }
    for p in 0..128u8 {
        close(&mut open, p, stop, &mut notes);
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    notes
}

impl TakeCollector {
    /// Recorded input held in memory that has not been written to disk yet,
    /// in bytes (the status bar shows it as disk activity).
    pub fn pending_bytes(&self) -> usize {
        self.stream.len() * std::mem::size_of::<f32>()
    }

    /// Whether any take is being recorded.
    pub fn is_recording(&self) -> bool {
        !self.open.is_empty()
    }

    fn received(&self) -> u64 {
        self.base + (self.stream.len() / 2) as u64
    }

    /// Cuts `[from, to)` out of the stream as left/right channels.
    fn cut(&self, from: u64, to: u64) -> (Vec<f32>, Vec<f32>) {
        let from = from.max(self.base);
        let to = to.min(self.received());
        if to <= from {
            return (Vec::new(), Vec::new());
        }
        let a = ((from - self.base) * 2) as usize;
        let b = ((to - self.base) * 2) as usize;
        let frames = &self.stream[a..b];
        let l = frames.iter().step_by(2).copied().collect();
        let r = frames.iter().skip(1).step_by(2).copied().collect();
        (l, r)
    }
}

/// Directory for temporary recordings of unsaved projects.
pub fn temp_recordings_dir() -> PathBuf {
    std::env::temp_dir().join("tessera-recordings")
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() {
        "Take".into()
    } else {
        s
    }
}

pub fn write_float_wav(path: &Path, data: &AudioData) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: data.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for (l, r) in data.left.iter().zip(data.right.iter()) {
        w.write_sample(*l)?;
        w.write_sample(*r)?;
    }
    w.finalize()
}

impl TesseraApp {
    /// Where new recordings go: next to the project once it has been saved.
    fn recordings_dir(&self) -> PathBuf {
        match self.path.as_ref().and_then(|p| p.parent()) {
            Some(dir) => dir.join("Samples").join("Recorded"),
            None => temp_recordings_dir(),
        }
    }

    pub fn arrangement_recording(&self) -> bool {
        self.engine.shared.record.arrangement.load(Ordering::Relaxed)
    }

    pub fn toggle_arrangement_record(&mut self) {
        let on = !self.arrangement_recording();
        if on && !self.project.tracks.iter().any(|t| t.mixer.arm) {
            self.status = "Arm an audio or MIDI track to record into it".into();
        }
        self.send(Command::SetArrangementRecord(on));
    }

    /// Session record button of an empty slot.
    pub fn record_slot(&mut self, track: Id, slot: usize) {
        let audio = self.project.track(track).is_some_and(|t| t.kind == TrackKind::Audio);
        if audio && !self.engine.shared.record.input_connected.load(Ordering::Relaxed) {
            self.status = "No audio input device; recording will be silent".into();
        }
        self.send(Command::RecordSlot { track, slot });
    }

    /// Drains the recording stream and finishes takes. Call once per frame.
    pub fn poll_recording(&mut self) {
        let tap = &mut self.engine.record;
        let n = tap.audio.slots();
        if n > 0 {
            if let Ok(chunk) = tap.audio.read_chunk(n) {
                let (a, b) = chunk.as_slices();
                self.takes.stream.extend_from_slice(a);
                self.takes.stream.extend_from_slice(b);
                chunk.commit_all();
            }
        }
        let mut midi_done = Vec::new();
        while let Ok(ev) = tap.events.pop() {
            match ev {
                RecEvent::Started {
                    track,
                    target,
                    beat,
                    frame,
                    latency,
                } => self.takes.open.push(Take {
                    track,
                    target,
                    beat,
                    frame,
                    latency,
                }),
                RecEvent::Note { track, beat, kind } => {
                    self.takes.midi.entry(track).or_default().push((beat, kind));
                }
                RecEvent::Stopped { track, beat, frame, .. }
                    if self.project.track(track).is_some_and(|t| t.kind == TrackKind::Midi) =>
                {
                    let events = self.takes.midi.remove(&track).unwrap_or_default();
                    if let Some(i) = self.takes.open.iter().position(|t| t.track == track) {
                        let take = self.takes.open.remove(i);
                        midi_done.push((take, beat, notes_between(&events, take.beat, beat)));
                    }
                    let _ = frame;
                }
                RecEvent::Stopped { track, beat, frame, .. } => {
                    if let Some(i) = self.takes.open.iter().position(|t| t.track == track) {
                        let take = self.takes.open.remove(i);
                        self.takes.finished.push(Finished {
                            take,
                            stop_beat: beat,
                            stop_frame: frame,
                        });
                    }
                }
                RecEvent::Overflow => {
                    self.status = "Recording fell behind; some input was lost".into();
                }
            }
        }

        for (take, stop, notes) in midi_done {
            self.finish_midi_take(take, stop, notes);
        }

        let idle = self
            .engine
            .shared
            .record
            .takes
            .iter()
            .all(|t| t.track.load(Ordering::Relaxed) == 0);
        if idle && self.takes.open.is_empty() {
            self.takes.midi.clear();
        }
        let received = self.takes.received();
        let mut ready = Vec::new();
        self.takes.finished.retain(|f| {
            let want = f.stop_frame + f.take.latency as u64;
            if received >= want || (idle && received >= f.stop_frame) {
                ready.push(*f);
                false
            } else {
                true
            }
        });
        for f in ready {
            self.finish_take(f);
        }
        if self.takes.open.is_empty() && self.takes.finished.is_empty() && !self.takes.stream.is_empty() {
            self.takes.base = self.takes.received();
            self.takes.stream.clear();
            self.takes.stream.shrink_to(48000 * 2);
        }
    }

    fn finish_take(&mut self, f: Finished) {
        let t = f.take;
        let lat = t.latency as u64;
        let (l, r) = self.takes.cut(t.frame + lat, f.stop_frame + lat);
        let length = f.stop_beat - t.beat;
        if l.is_empty() || length <= 1e-6 {
            return;
        }
        let Some(track) = self.project.track(t.track) else {
            return;
        };
        let channels = track.input.channels;
        let (left, right): (Vec<f32>, Vec<f32>) = l.iter().zip(r.iter()).map(|(a, b)| channels.route(*a, *b)).unzip();
        let data = AudioData {
            sample_rate: self.engine.sample_rate() as u32,
            left,
            right,
        };
        self.takes.counter += 1;
        let name = format!("{} {}", track.name, self.takes.counter);
        let color = track.color;
        let dir = self.recordings_dir();
        let mut path = dir.join(format!("{}.wav", sanitize(&name)));
        let mut k = 2;
        while path.exists() {
            path = dir.join(format!("{} ({k}).wav", sanitize(&name)));
            k += 1;
        }
        let written = std::fs::create_dir_all(&dir)
            .map_err(|e| e.to_string())
            .and_then(|_| write_float_wav(&path, &data).map_err(|e| e.to_string()));
        if let Err(e) = written {
            // Keep the take in memory; saving will report the missing file.
            self.status = format!("Could not write {}: {e}", path.display());
        }
        // Takes play unwarped, but carry the recording tempo and transients
        // so turning Warp on in the clip view lines them up straight away.
        let mut audio = AudioRef::new(path.to_string_lossy(), None);
        audio.warp.bpm = self.project.tempo;
        audio.warp.transients = tessera_engine::analysis::detect_transients(&data);
        audio.data = Some(Arc::new(data));
        let clip = Clip {
            id: self.project.alloc_id(),
            name,
            color,
            length,
            looping: matches!(t.target, RecTarget::Slot(_)),
            gain_db: 0.0,
            content: ClipContent::Audio(audio),
            ..Default::default()
        };
        self.place_take(t, clip);
        self.status = format!("Recorded {:.1} s", length * 60.0 / self.project.tempo);
        self.changed();
    }

    /// Puts a recorded clip into its slot (or the next free one below) or
    /// onto the Arrangement, replacing what the take covers.
    fn place_take(&mut self, t: Take, clip: Clip) {
        let length = clip.length;
        match t.target {
            RecTarget::Slot(slot) => {
                let Some(tr) = self.project.track(t.track) else {
                    return;
                };
                // With no free slot below (scenes deleted or filled while
                // recording), add a scene rather than lose the take.
                let s = match (slot..tr.slots.len()).find(|&s| tr.slots[s].is_none()) {
                    Some(s) => s,
                    None => self.project.add_scene(),
                };
                if let Some(slot) = self.project.track_mut(t.track).and_then(|tr| tr.slots.get_mut(s)) {
                    *slot = Some(clip);
                }
                self.select_clip(ClipRef::Slot {
                    track: t.track,
                    scene: s,
                });
            }
            RecTarget::Arrangement => {
                let start = t.beat;
                let rec = ArrangementClip {
                    id: self.project.alloc_id(),
                    start,
                    length,
                    clip,
                    ..Default::default()
                };
                let overdub = self.project.midi_overdub && rec.clip.is_midi();
                if let Some(id) = tessera_engine::clip_edit::record_take(&mut self.project, t.track, rec, overdub) {
                    self.select_clip(ClipRef::Arrangement { track: t.track, id });
                }
            }
        }
    }

    fn finish_midi_take(&mut self, t: Take, stop_beat: f64, mut notes: Vec<Note>) {
        let length = stop_beat - t.beat;
        let Some(track) = self.project.track(t.track) else {
            return;
        };
        if length <= 1e-6 {
            return;
        }
        if notes.is_empty() {
            self.status = format!("Nothing was played on {}", track.name);
            return;
        }
        // Record Quantization snaps to the song's grid, not the take's.
        if let Some(q) = self.prefs.record_quantize.settings() {
            tessera_engine::midi::edit::quantize_at(&mut notes, &[], &q, t.beat);
        }
        self.takes.counter += 1;
        let name = format!("{} {}", track.name, self.takes.counter);
        let color = track.color;
        let count = notes.len();
        let clip = Clip {
            id: self.project.alloc_id(),
            name,
            color,
            length,
            looping: matches!(t.target, RecTarget::Slot(_)),
            content: ClipContent::Midi { notes },
            ..Default::default()
        };
        self.place_take(t, clip);
        self.status = format!("Recorded {count} notes");
        self.changed();
    }

    /// Copies recordings of a not-yet-saved project next to the project file
    /// and points the clips at the copies (relative paths).
    pub fn collect_recordings(&mut self, project_path: &Path) {
        let Some(dir) = project_path.parent() else { return };
        let temp = temp_recordings_dir();
        let dest_dir = dir.join("Samples").join("Recorded");
        let mut failed = None;
        let mut fix = |clip: &mut Clip| {
            let ClipContent::Audio(a) = &mut clip.content else {
                return;
            };
            let src = PathBuf::from(&a.path);
            if !src.starts_with(&temp) {
                return;
            }
            let Some(file) = src.file_name() else { return };
            let dest = dest_dir.join(file);
            let ok = std::fs::create_dir_all(&dest_dir).and_then(|_| std::fs::copy(&src, &dest));
            match ok {
                Ok(_) => {
                    a.path = Path::new("Samples")
                        .join("Recorded")
                        .join(file)
                        .to_string_lossy()
                        .to_string();
                }
                Err(e) => failed = Some(format!("Could not copy {}: {e}", src.display())),
            }
        };
        for t in &mut self.project.tracks {
            for c in t.slots.iter_mut().flatten() {
                fix(c);
            }
            for a in &mut t.arrangement {
                fix(&mut a.clip);
            }
        }
        if let Some(e) = failed {
            self.status = e;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn played_notes_become_clip_notes() {
        let ev = [
            (3.9, EventKind::On(60, 100)), // slightly early: snapped to the start
            (4.5, EventKind::Off(60)),
            (5.0, EventKind::On(64, 80)),
            (5.0, EventKind::On(67, 0)),
            (6.0, EventKind::AllOff),
            (7.5, EventKind::On(72, 90)), // still held at the stop
            (2.0, EventKind::Off(50)),
        ];
        let notes = notes_between(&ev, 4.0, 8.0);
        let got: Vec<(u8, u8, f64, f64)> = notes.iter().map(|n| (n.pitch, n.velocity, n.start, n.length)).collect();
        assert_eq!(
            got,
            vec![
                (60, 100, 0.0, 0.5),
                (64, 80, 1.0, 1.0),
                (67, 1, 1.0, 1.0),
                (72, 90, 3.5, 0.5)
            ]
        );
        // Notes from long before the start are dropped.
        assert!(notes_between(&[(1.0, EventKind::On(60, 100))], 4.0, 8.0).is_empty());
    }

    #[test]
    fn cut_handles_bounds() {
        let mut c = TakeCollector {
            base: 100,
            ..Default::default()
        };
        c.stream = (0..20).map(|i| i as f32).collect();
        let (l, r) = c.cut(102, 105);
        assert_eq!(l, vec![4.0, 6.0, 8.0]);
        assert_eq!(r, vec![5.0, 7.0, 9.0]);
        let (l, _) = c.cut(90, 200);
        assert_eq!(l.len(), 10);
        assert!(c.cut(150, 160).0.is_empty());
    }

    #[test]
    fn float_wavs_round_trip() {
        let path = std::env::temp_dir().join(format!("tessera-rec-test-{}.wav", std::process::id()));
        let data = AudioData {
            sample_rate: 48000,
            left: vec![0.5, -0.25, 0.125],
            right: vec![0.0, 1.0, -1.0],
        };
        write_float_wav(&path, &data).unwrap();
        let back = AudioData::load_wav(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(back.left, data.left);
        assert_eq!(back.right, data.right);
        assert_eq!(sanitize("1 Audio/Take:2"), "1 Audio_Take_2");
    }
}
