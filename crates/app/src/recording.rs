//! Turns the engine's recording stream into audio clips.
//!
//! The audio thread streams raw input while anything records and marks where
//! each take starts and stops. Here, on the UI thread, takes are cut out of
//! that stream (dropping the input/output latency from the front), written to
//! 32-bit float WAV files and added to the project as clips.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tessera_engine::engine::{RecEvent, RecTarget};
use tessera_engine::model::{ArrangementClip, AudioData, AudioRef, Clip, ClipContent, Id};
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
            self.status = "Arm an audio track to record into it".into();
        }
        self.send(Command::SetArrangementRecord(on));
    }

    /// Session record button of an empty slot.
    pub fn record_slot(&mut self, track: Id, slot: usize) {
        if !self.engine.shared.record.input_connected.load(Ordering::Relaxed) {
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

        let idle = self
            .engine
            .shared
            .record
            .takes
            .iter()
            .all(|t| t.track.load(Ordering::Relaxed) == 0);
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
        match t.target {
            RecTarget::Slot(slot) => {
                let Some(tr) = self.project.track_mut(t.track) else {
                    return;
                };
                let target = (slot..tr.slots.len()).find(|&s| tr.slots[s].is_none());
                if let Some(s) = target {
                    tr.slots[s] = Some(clip);
                    self.select_clip(ClipRef::Slot {
                        track: t.track,
                        scene: s,
                    });
                }
            }
            RecTarget::Arrangement => {
                let start = t.beat;
                let end = f.stop_beat;
                let aid = self.project.alloc_id();
                let Some(tr) = self.project.track_mut(t.track) else {
                    return;
                };
                // The new take replaces what it covers.
                tr.arrangement.retain(|a| !(a.start >= start && a.end() <= end));
                for a in tr.arrangement.iter_mut() {
                    if a.start < start && a.end() > start {
                        a.length = start - a.start;
                    }
                }
                tr.arrangement.push(ArrangementClip {
                    id: aid,
                    start,
                    length,
                    clip,
                });
                tr.sort_arrangement();
            }
        }
        self.status = format!("Recorded {:.1} s", length * 60.0 / self.project.tempo);
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
