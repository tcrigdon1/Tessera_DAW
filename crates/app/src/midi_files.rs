//! Standard MIDI Files in the app: Export MIDI Clip, importing `.mid` files
//! (File menu or dropped on the window) and saving clips to the library so
//! they show up in the Browser.

use std::path::{Path, PathBuf};

use tessera_engine::chain_preset::ChainPreset;
use tessera_engine::dsp::drum_rack::DrumRackParams;
use tessera_engine::dsp::electric::ElectricParams;
use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::midifile;
use tessera_engine::model::{ArrangementClip, AudioData, Clip, ClipContent, Instrument, Note, Project, TrackKind};
use tessera_engine::render::RenderOptions;

use crate::app::{ClipRef, TesseraApp, View};
use crate::browser::DropTarget;

pub fn is_midi_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"))
        .unwrap_or(false)
}

impl TesseraApp {
    fn selected_midi_clip(&self) -> Option<&Clip> {
        self.selected_clip.and_then(|r| self.clip(r)).filter(|c| c.is_midi())
    }

    fn clip_midi_bytes(&self, clip: &Clip) -> Vec<u8> {
        let p = &self.project;
        midifile::export_clip(clip, p.tempo, p.beats_per_bar, p.beat_unit)
    }

    /// File → Export MIDI Clip… (Ctrl/Cmd + Shift + E).
    pub fn export_midi_clip_dialog(&mut self) {
        let Some(clip) = self.selected_midi_clip().cloned() else {
            self.status = "Select a MIDI clip to export".into();
            return;
        };
        let mut dialog = rfd::FileDialog::new()
            .add_filter("MIDI file", &["mid"])
            .set_file_name(format!("{}.mid", sanitize(&clip.name)));
        if let Some(dir) = self.prefs.clips_dir().filter(|d| d.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(mut path) = dialog.save_file() {
            if path.extension().is_none() {
                path.set_extension("mid");
            }
            let bytes = self.clip_midi_bytes(&clip);
            self.status = match std::fs::write(&path, bytes) {
                Ok(()) => format!("Exported {}", path.display()),
                Err(e) => format!("Could not write {}: {e}", path.display()),
            };
        }
    }

    /// Saves a clip into the library's Clips folder (shown under Clips in
    /// the Browser): MIDI clips as `.mid`, audio clips as a copy of their file.
    pub fn save_clip_to_library(&mut self, r: ClipRef) -> Option<PathBuf> {
        let clip = self.clip(r)?.clone();
        let dir = self.prefs.clips_dir()?;
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.status = format!("Could not create {}: {e}", dir.display());
            return None;
        }
        let result = match &clip.content {
            ClipContent::Midi { .. } => {
                let path = unique(&dir, &sanitize(&clip.name), "mid");
                std::fs::write(&path, self.clip_midi_bytes(&clip)).map(|_| path)
            }
            ClipContent::Audio(a) => {
                let mut src = PathBuf::from(&a.path);
                if src.is_relative() {
                    if let Some(base) = self.path.as_deref().and_then(Path::parent) {
                        src = base.join(src);
                    }
                }
                let ext = src.extension().and_then(|e| e.to_str()).unwrap_or("wav").to_string();
                let path = unique(&dir, &sanitize(&clip.name), &ext);
                std::fs::copy(&src, &path).map(|_| path)
            }
        };
        match result {
            Ok(path) => {
                self.browser.refresh();
                self.status = format!("Saved {} to the library", path.display());
                Some(path)
            }
            Err(e) => {
                self.status = format!("Could not save the clip: {e}");
                None
            }
        }
    }

    /// A `.mid` file dropped from the Browser: onto a MIDI track's slot or
    /// lane it becomes one clip there (all its tracks merged); anywhere else
    /// it is imported as new tracks.
    pub fn drop_midi_file(&mut self, path: &Path, target: DropTarget) {
        let track = match target {
            DropTarget::Slot(t, _) | DropTarget::Arrangement(t, _) | DropTarget::Track(t) | DropTarget::Device(t) => {
                Some(t)
            }
            DropTarget::NewTrack | DropTarget::Pad(..) => None,
        };
        let Some(t) = track.filter(|t| self.project.track(*t).map(|t| t.kind) == Some(TrackKind::Midi)) else {
            self.import_midi(path);
            return;
        };
        let imported = match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| midifile::import(&b))
        {
            Ok(m) => m,
            Err(e) => {
                self.status = format!("Could not import {}: {e}", path.display());
                return;
            }
        };
        let bpb = self.project.beats_per_bar as f64;
        let length = imported.tracks.iter().map(|t| t.length).fold(bpb, f64::max);
        let mut notes: Vec<_> = imported.tracks.into_iter().flat_map(|t| t.notes).collect();
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        let color = self.project.track(t).map(|t| t.color).unwrap_or([200, 200, 200]);
        let clip = Clip {
            id: self.project.alloc_id(),
            name: crate::browser::file_label(path),
            color,
            length,
            looping: true,
            gain_db: 0.0,
            content: ClipContent::Midi { notes },
            envelopes: Vec::new(),
            ..Default::default()
        };
        let r = match target {
            DropTarget::Arrangement(_, beat) => {
                let id = self.project.alloc_id();
                if let Some(track) = self.project.track_mut(t) {
                    track.arrangement.push(ArrangementClip {
                        id,
                        start: beat,
                        length,
                        clip,
                        ..Default::default()
                    });
                    track.sort_arrangement();
                }
                ClipRef::Arrangement { track: t, id }
            }
            DropTarget::Slot(_, scene) => {
                if let Some(slot) = self.project.track_mut(t).and_then(|tr| tr.slots.get_mut(scene)) {
                    *slot = Some(clip);
                }
                ClipRef::Slot { track: t, scene }
            }
            _ => {
                let Some(scene) = self.free_slot(t) else {
                    self.status = "No free slot on that track".into();
                    return;
                };
                if let Some(slot) = self.project.track_mut(t).and_then(|tr| tr.slots.get_mut(scene)) {
                    *slot = Some(clip);
                }
                ClipRef::Slot { track: t, scene }
            }
        };
        self.select_clip(r);
        self.status = format!("Imported {}", path.display());
        self.changed();
    }

    /// Plays a `.mid` file in the Browser: renders it in the background
    /// with the selected MIDI track's instrument and effects (or a Drum Rack
    /// for drum files, an Electric Piano otherwise) and plays the result
    /// through the preview output.
    pub fn preview_midi(&mut self, path: &Path) {
        let imported = match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| midifile::import(&b))
        {
            Ok(m) => m,
            Err(e) => {
                self.status = format!("Could not preview {}: {e}", path.display());
                return;
            }
        };
        let length = imported
            .tracks
            .iter()
            .map(|t| t.length)
            .fold(4.0, f64::max)
            .min(MAX_PREVIEW_BEATS);
        let mut notes: Vec<_> = imported.tracks.into_iter().flat_map(|t| t.notes).collect();
        notes.retain(|n| n.start < length);
        let drums = path
            .components()
            .any(|c| c.as_os_str().to_string_lossy().to_lowercase().contains("drum"));
        // The selected track's sound, unless it is a plug-in (offline
        // previews can't reach running plug-ins).
        let chain = self
            .selected_track
            .and_then(|t| self.project.track(t))
            .filter(|t| t.kind == TrackKind::Midi && !matches!(t.instrument, Instrument::Plugin(_) | Instrument::None))
            .filter(|t| !drums || matches!(t.instrument, Instrument::DrumRack(_) | Instrument::DrumKit(_)))
            .map(|t| t.id)
            .and_then(|t| {
                ChainPreset::capture(&self.project, t, "Preview", self.path.as_deref().and_then(Path::parent))
            });
        let tempo = self.project.tempo;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tx.send(render_preview(notes, length, chain, drums, tempo)).ok();
        });
        self.browser.midi_preview = Some((path.to_path_buf(), rx));
    }

    /// File → Import MIDI File…
    pub fn import_midi_dialog(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("MIDI file", &["mid", "midi"])
            .pick_file()
        {
            self.import_midi(&path);
        }
    }

    /// Imports a `.mid` file: each of its tracks becomes a new MIDI track
    /// with one clip, in the first Session slot (Session view) or at the
    /// playhead's bar (Arrangement view).
    pub fn import_midi(&mut self, path: &Path) {
        let imported = match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| midifile::import(&b))
        {
            Ok(m) => m,
            Err(e) => {
                self.status = format!("Could not import {}: {e}", path.display());
                return;
            }
        };
        if imported.tracks.is_empty() {
            self.status = format!("{} has no notes", path.display());
            return;
        }
        let file_name = crate::browser::file_label(path);
        let bpb = self.project.beats_per_bar as f64;
        let at = (self.song_beat() / bpb).floor() * bpb;
        let n = imported.tracks.len();
        let mut first = None;
        for t in imported.tracks {
            let id = self.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
            let name = if t.name.trim().is_empty() || n == 1 {
                file_name.clone()
            } else {
                t.name.clone()
            };
            let clip_id = self.project.alloc_id();
            let color = self.project.track(id).map(|t| t.color).unwrap_or([200, 200, 200]);
            let clip = Clip {
                id: clip_id,
                name: name.clone(),
                color,
                length: t.length.max(bpb),
                looping: true,
                gain_db: 0.0,
                content: ClipContent::Midi { notes: t.notes },
                envelopes: Vec::new(),
                ..Default::default()
            };
            let arr_id = self.project.alloc_id();
            if let Some(track) = self.project.track_mut(id) {
                track.name = name;
                match self.view {
                    View::Session => {
                        if let Some(slot) = track.slots.first_mut() {
                            *slot = Some(clip);
                        }
                    }
                    View::Arrangement => {
                        let length = clip.length;
                        track.arrangement.push(ArrangementClip {
                            id: arr_id,
                            start: at,
                            length,
                            clip,
                            ..Default::default()
                        });
                        track.sort_arrangement();
                    }
                }
            }
            first.get_or_insert(match self.view {
                View::Session => ClipRef::Slot { track: id, scene: 0 },
                View::Arrangement => ClipRef::Arrangement { track: id, id: arr_id },
            });
        }
        if let Some(r) = first {
            self.select_clip(r);
        }
        let tempo = imported
            .tempo
            .map(|t| format!(" (file tempo {t:.1} BPM)"))
            .unwrap_or_default();
        self.status = format!("Imported {n} track(s) from {file_name}{tempo}");
        self.changed();
    }
}

/// Longest stretch of a MIDI file the Browser previews.
const MAX_PREVIEW_BEATS: f64 = 32.0;

/// Renders notes through a chain (or a default instrument) to audio.
fn render_preview(notes: Vec<Note>, length: f64, chain: Option<ChainPreset>, drums: bool, tempo: f64) -> AudioData {
    const SR: u32 = 44100;
    let mut p = Project {
        tempo,
        // Far from the ids of the open project, so nothing here can pick up
        // one of its running plug-ins.
        next_id: 1 << 48,
        ..Default::default()
    };
    let fallback = if drums {
        Instrument::DrumRack(DrumRackParams::standard())
    } else {
        Instrument::Electric(ElectricParams::default())
    };
    let t = p.add_track(TrackKind::Midi, fallback);
    if let Some(c) = &chain {
        c.apply(&mut p, t, false);
    }
    let clip_id = p.alloc_id();
    let id = p.alloc_id();
    if let Some(track) = p.track_mut(t) {
        track.arrangement.push(ArrangementClip {
            id,
            start: 0.0,
            length,
            clip: Clip {
                id: clip_id,
                length,
                content: ClipContent::Midi { notes },
                ..Default::default()
            },
            ..Default::default()
        });
    }
    let mut o = RenderOptions::master(SR, 0.0, length);
    o.tail_secs = 1.5;
    let inter = tessera_engine::render::render(&p, &o);
    let (left, right) = inter.chunks_exact(2).map(|f| (f[0], f[1])).unzip();
    AudioData {
        sample_rate: SR,
        left,
        right,
    }
}

pub(crate) fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if "/\\:*?\"<>|".contains(c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() {
        "Clip".into()
    } else {
        s
    }
}

/// `dir/name.ext`, or `dir/name 2.ext` … when that exists.
pub(crate) fn unique(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let mut p = dir.join(format!("{name}.{ext}"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{name} {n}.{ext}"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_previews_are_audible() {
        for (drums, pitch) in [(false, 60), (true, 36)] {
            let notes = vec![Note {
                pitch,
                velocity: 100,
                start: 0.0,
                length: 1.0,
            }];
            let d = render_preview(notes, 4.0, None, drums, 120.0);
            assert!(d.frames() >= 44100 * 2, "two seconds plus the tail");
            let peak = d.left.iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(peak > 0.01, "drums {drums}: {peak}");
        }
    }

    #[test]
    fn midi_extensions() {
        assert!(is_midi_file(Path::new("a.mid")));
        assert!(is_midi_file(Path::new("a.MIDI")));
        assert!(!is_midi_file(Path::new("a.wav")));
    }

    #[test]
    fn unique_names() {
        let d = std::env::temp_dir().join(format!("tessera-unique-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let a = unique(&d, "Clip", "mid");
        std::fs::write(&a, b"x").unwrap();
        let b = unique(&d, "Clip", "mid");
        assert!(b.ends_with("Clip 2.mid"));
        let _ = std::fs::remove_dir_all(d);
        assert_eq!(sanitize(" a/b "), "a_b");
        assert_eq!(sanitize(""), "Clip");
    }
}
