//! Arrangement and Session workflow on the app side: undo history, track
//! operations (group, freeze, flatten, duplicate, reorder), clip editing
//! (split, consolidate, reverse, crossfades, comping), locators and song
//! time, the Groove Pool, audio-to-MIDI, Capture MIDI and Session helpers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;

use tessera_engine::audio_to_midi::{self, ConvertMode};
use tessera_engine::capture::CaptureBuffer;
use tessera_engine::dsp::electric::ElectricParams;
use tessera_engine::dsp::sampler::{SamplerParams, MAX_SLICES, SLICE_BASE_NOTE};
use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::groove::{self, Groove};
use tessera_engine::history::History;
use tessera_engine::model::{
    ArrangementClip, AudioData, AudioRef, Clip, ClipContent, FreezeData, Id, Instrument, Note, Project, SampleRef,
    TrackKind,
};
use tessera_engine::timeline::TimeMap;
use tessera_engine::{arrange, clip_edit, Command};

use crate::app::{ClipRef, TesseraApp, View};
use crate::recording::{temp_recordings_dir, write_float_wav};

/// Edits of the same kind this close together (seconds) become one undo
/// step (typing a name, nudging a value).
const MERGE_SECS: f64 = 1.0;

/// Audio-to-MIDI analysis running on its own thread, so long clips don't
/// freeze the UI.
struct ConvertJob {
    from: ClipRef,
    mode: ConvertMode,
    /// The source clip as it was when the conversion started.
    clip: Clip,
    thread: JoinHandle<Vec<Note>>,
}

/// A time range selected on one Arrangement track (or in a take lane).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimeSelection {
    pub track: Id,
    pub from: f64,
    pub to: f64,
    /// Selected in this take lane rather than the main lane.
    pub take: Option<Id>,
}

/// A clip being dragged between the Session and the Arrangement (egui
/// drag-and-drop payload).
#[derive(Clone, Copy, Debug)]
pub enum ClipDrag {
    Slot { track: Id, scene: usize },
    Arrangement { track: Id, id: Id },
}

/// Workflow state kept by the app.
#[derive(Default)]
pub struct WorkflowState {
    pub history: History<Project>,
    /// The project as of the last undo step.
    committed: Option<Project>,
    /// Label of an edit not yet recorded as an undo step.
    pending: Option<String>,
    /// Label for the next edit (set just before making it).
    next_label: Option<String>,
    last_commit: Option<(String, f64)>,
    pub show_history: bool,
    pub show_grooves: bool,
    /// Tracks selected together (Ctrl/Cmd+click), for grouping.
    pub selected_tracks: Vec<Id>,
    /// An audio-to-MIDI conversion running in the background.
    convert: Option<ConvertJob>,
    /// Scene selected in the Session view (Session Record, Capture).
    pub selected_scene: usize,
    pub selection: Option<TimeSelection>,
    /// Notes played per track, for Capture MIDI.
    pub capture: HashMap<Id, CaptureBuffer>,
    /// Song tempo lane open in the Arrangement.
    pub show_tempo_lane: bool,
}

impl WorkflowState {
    /// Starts a fresh history for a newly loaded project.
    pub fn reset(&mut self, p: &Project) {
        self.history.clear();
        self.committed = Some(p.clone());
        self.pending = None;
        self.next_label = None;
        self.last_commit = None;
        self.selected_tracks.clear();
        self.selected_scene = 0;
        self.selection = None;
        self.capture.clear();
    }
}

impl TesseraApp {
    // ----- undo ------------------------------------------------------------------

    /// Notes that the project changed; it becomes an undo step once the
    /// mouse is released.
    pub fn touch(&mut self) {
        self.dirty = true;
        let label = self.work.next_label.take();
        match (&mut self.work.pending, label) {
            (p @ None, l) => *p = Some(l.unwrap_or_else(|| "Edit".into())),
            (Some(p), Some(l)) => *p = l,
            _ => {}
        }
    }

    /// Names the next edit in the History panel.
    pub fn label_edit(&mut self, label: &str) {
        self.work.next_label = Some(label.to_string());
    }

    /// Records the pending edit as an undo step. Called at the end of every
    /// frame; waits while a mouse button is held so a drag is one step.
    pub fn commit_history(&mut self, pointer_down: bool, now: f64) {
        if pointer_down || self.work.pending.is_none() {
            return;
        }
        let label = self.work.pending.take().unwrap_or_default();
        let current = self.project.clone();
        let Some(before) = self.work.committed.replace(current) else {
            return;
        };
        let merge = matches!(&self.work.last_commit, Some((l, t)) if *l == label && now - t < MERGE_SECS)
            && self.work.history.undo_label() == Some(label.as_str())
            && !self.work.history.can_redo();
        if !merge {
            self.work.history.record(label.clone(), before);
        }
        self.work.last_commit = Some((label, now));
    }

    pub fn undo(&mut self) {
        self.commit_history(false, 0.0);
        let label = self.work.history.undo_label().map(str::to_string);
        if let Some(p) = self.work.history.undo(self.project.clone()) {
            self.restore(p);
            self.status = format!("Undo {}", label.unwrap_or_default());
        }
    }

    pub fn redo(&mut self) {
        self.commit_history(false, 0.0);
        let label = self.work.history.redo_label().map(str::to_string);
        if let Some(p) = self.work.history.redo(self.project.clone()) {
            self.restore(p);
            self.status = format!("Redo {}", label.unwrap_or_default());
        }
    }

    /// Jumps to a point in the history (History panel).
    pub fn history_jump(&mut self, position: usize) {
        self.commit_history(false, 0.0);
        if let Some(p) = self.work.history.jump_to(position, self.project.clone()) {
            self.restore(p);
        }
    }

    /// Puts a project state from the history back, keeping the transport.
    fn restore(&mut self, p: Project) {
        self.project = p;
        self.work.committed = Some(self.project.clone());
        self.work.last_commit = None;
        self.dirty = true;
        self.needs_sync = true;
        // Settings the snapshot doesn't carry while playing.
        let p = &self.project;
        let cmds = [
            Command::SetTempo(p.tempo),
            Command::SetSignature(p.beats_per_bar, p.beat_unit),
            Command::SetLoop {
                enabled: p.loop_enabled,
                start: p.loop_start,
                length: p.loop_length,
            },
            Command::SetMaster(p.master),
            Command::SetMetronome(p.metronome),
            Command::SetQuantization(p.launch_quantization),
        ];
        for c in cmds {
            self.send(c);
        }
        if let Some(r) = self.selected_clip {
            if self.clip(r).is_none() {
                self.selected_clip = None;
            }
        }
        if let Some(t) = self.selected_track {
            if self.project.track(t).is_none() && !self.project.returns.iter().any(|r| r.id == t) && t != 0 {
                self.selected_track = self.project.tracks.first().map(|t| t.id);
            }
        }
        self.work.selected_tracks.retain(|t| self.project.track(*t).is_some());
        self.work.selection = None;
    }

    // ----- tracks ----------------------------------------------------------------

    /// The tracks to act on: the multi-selection, or the selected track.
    pub fn target_tracks(&self) -> Vec<Id> {
        if !self.work.selected_tracks.is_empty() {
            return self.work.selected_tracks.clone();
        }
        self.selected_track
            .filter(|t| self.project.track(*t).is_some())
            .into_iter()
            .collect()
    }

    /// Ctrl/Cmd+click on a track header.
    pub fn toggle_track_selection(&mut self, id: Id) {
        if self.work.selected_tracks.is_empty() {
            if let Some(t) = self.selected_track.filter(|t| self.project.track(*t).is_some()) {
                self.work.selected_tracks.push(t);
            }
        }
        if let Some(i) = self.work.selected_tracks.iter().position(|t| *t == id) {
            self.work.selected_tracks.remove(i);
        } else {
            self.work.selected_tracks.push(id);
        }
        self.selected_track = Some(id);
    }

    /// Plain click on a track header.
    pub fn select_track(&mut self, id: Id) {
        self.work.selected_tracks.clear();
        self.selected_track = Some(id);
    }

    /// Groups the selected tracks (Ctrl/Cmd+G).
    pub fn group_selected(&mut self) {
        let ids = self.target_tracks();
        if ids.is_empty() {
            return;
        }
        self.label_edit("Group Tracks");
        if let Some(g) = arrange::group_tracks(&mut self.project, &ids) {
            self.select_track(g);
            self.changed();
        }
    }

    pub fn ungroup(&mut self, group: Id) {
        self.label_edit("Ungroup Tracks");
        arrange::ungroup(&mut self.project, group);
        self.select_track(self.project.tracks.first().map(|t| t.id).unwrap_or(0));
        self.changed();
    }

    pub fn add_group_track(&mut self) -> Id {
        self.label_edit("Insert Group Track");
        let id = self.add_track(TrackKind::Group, Instrument::None);
        if let Some(t) = self.project.track_mut(id) {
            t.name = "Group".into();
        }
        id
    }

    pub fn toggle_fold(&mut self, group: Id) {
        if let Some(t) = self.project.track_mut(group) {
            t.folded = !t.folded;
        }
        self.dirty = true;
    }

    /// Duplicates the selected tracks (a group with its members).
    pub fn duplicate_tracks(&mut self) {
        self.label_edit("Duplicate Track");
        // Take the running plug-ins' current state along.
        self.plugins.store_states(&mut self.project);
        let ids = self.target_tracks();
        let mut last = None;
        for id in ids {
            last = arrange::duplicate_track(&mut self.project, id).or(last);
        }
        if let Some(id) = last {
            self.select_track(id);
            self.changed();
        }
    }

    /// Deletes the selected tracks (a group with its members).
    pub fn delete_tracks(&mut self) {
        let ids = self.target_tracks();
        if ids.is_empty() {
            return;
        }
        self.label_edit("Delete Track");
        for id in ids {
            arrange::delete_track(&mut self.project, id);
            if self.selected_clip.map(|c| c.track()) == Some(id) {
                self.selected_clip = None;
            }
        }
        self.work.selected_tracks.clear();
        self.selected_track = self.project.tracks.first().map(|t| t.id);
        self.changed();
    }

    /// Moves a track (with its group) to a new position in the track list.
    pub fn move_track(&mut self, id: Id, to: usize) {
        self.label_edit("Move Track");
        arrange::move_track(&mut self.project, id, to);
        self.changed();
    }

    /// Moves a track one place left/up (`-1`) or right/down (`1`), skipping
    /// over whole groups.
    pub fn nudge_track(&mut self, id: Id, dir: i32) {
        let block = arrange::track_block(&self.project, id);
        let (Some(&first), Some(&last)) = (block.first(), block.last()) else {
            return;
        };
        let to = if dir < 0 {
            if first == 0 {
                return;
            }
            // The track above, or the start of its group block.
            let above = self.project.tracks[first - 1].id;
            let mut top = first - 1;
            let mut cur = self.project.track(above).and_then(|t| t.output);
            while let Some(g) = cur {
                if Some(g) == self.project.tracks[first].output {
                    break;
                }
                match self.project.track_index(g) {
                    Some(i) => top = i,
                    None => break,
                }
                cur = self.project.track(g).and_then(|t| t.output);
            }
            top
        } else {
            if last + 1 >= self.project.tracks.len() {
                return;
            }
            let below = self.project.tracks[last + 1].id;
            arrange::track_block(&self.project, below)
                .last()
                .copied()
                .unwrap_or(last + 1)
                + 1
        };
        self.move_track(id, to);
    }

    pub fn set_track_color(&mut self, id: Id, color: [u8; 3]) {
        self.label_edit("Track Color");
        if let Some(t) = self.project.track_mut(id) {
            t.color = color;
            // Clips follow the track colour, like Ableton's default.
            for c in t.slots.iter_mut().flatten() {
                c.color = color;
            }
            for a in t.arrangement.iter_mut() {
                a.clip.color = color;
            }
        }
        self.touch();
    }

    pub fn set_track_delay(&mut self, id: Id, ms: f32) {
        self.label_edit("Track Delay");
        if let Some(t) = self.project.track_mut(id) {
            t.delay_ms = ms.clamp(-1000.0, 1000.0);
        }
        self.changed();
    }

    pub fn set_crossfade_assign(&mut self, id: Id, assign: tessera_engine::model::CrossfadeAssign) {
        self.label_edit("Crossfade Assign");
        if let Some(t) = self.project.track_mut(id) {
            t.crossfade = assign;
        }
        self.changed();
    }

    pub fn set_crossfader(&mut self, x: f32) {
        self.project.crossfader = x.clamp(-1.0, 1.0);
        self.send(Command::SetCrossfader(self.project.crossfader));
        self.touch();
    }

    pub fn send_cue(&mut self) {
        let c = self.project.cue;
        self.send(Command::SetCue {
            mode: c.cue_mode,
            volume_db: c.volume_db,
        });
        self.touch();
    }

    // ----- freeze and flatten ------------------------------------------------------

    /// A copy of the project with only `track` (and, for a group, its
    /// members) left, at unity gain, for rendering it on its own.
    fn solo_project(&self, track: Id) -> Option<Project> {
        let mut p = self.project.clone();
        let keep: Vec<Id> = arrange::track_block(&p, track)
            .into_iter()
            .map(|i| p.tracks[i].id)
            .collect();
        p.tracks.retain(|t| keep.contains(&t.id));
        let t = p.track_mut(track)?;
        t.output = None;
        t.frozen = false;
        t.freeze = None;
        t.delay_ms = 0.0;
        t.crossfade = Default::default();
        t.mixer = Default::default();
        t.automation.retain(|l| {
            !matches!(
                l.target,
                tessera_engine::automation::AutomationTarget::Volume
                    | tessera_engine::automation::AutomationTarget::Pan
            )
        });
        t.input.monitor = tessera_engine::model::Monitor::Off;
        for t in p.tracks.iter_mut() {
            t.sends.iter_mut().for_each(|s| *s = tessera_engine::model::SEND_OFF_DB);
        }
        p.returns.clear();
        p.fix_sends();
        p.master_devices.clear();
        p.master = Default::default();
        p.loop_enabled = false;
        p.metronome = false;
        p.count_in_bars = 0;
        Some(p)
    }

    fn render_project(&self, p: &Project, beats: f64) -> AudioData {
        let sr = self.engine.sample_rate() as u32;
        let instances = self.plugins.export_instances(p, sr as f32);
        let samples = crate::plugins::render_with_plugins(p, instances, sr, beats);
        let frames = samples.len() / 2;
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for f in 0..frames {
            left.push(samples[f * 2]);
            right.push(samples[f * 2 + 1]);
        }
        AudioData {
            sample_rate: sr,
            left,
            right,
        }
    }

    /// Renders a track's Arrangement and Session clips to audio.
    fn render_freeze(&self, track: Id) -> Option<FreezeData> {
        let base = self.solo_project(track)?;
        let t = base.track(track)?;
        let block: Vec<&tessera_engine::model::Track> = base
            .tracks
            .iter()
            .filter(|x| x.id == t.id || x.output.is_some())
            .collect();
        let arr_end = block
            .iter()
            .flat_map(|x| x.arrangement.iter())
            .map(|a| a.end())
            .fold(0.0, f64::max);
        let arrangement = (arr_end > 0.0).then(|| Arc::new(self.render_project(&base, arr_end)));
        let mut slots = Vec::new();
        for s in 0..t.slots.len() {
            let Some(clip) = t.slots[s].clone() else {
                slots.push(None);
                continue;
            };
            // Each Session clip on its own, as an Arrangement clip at 0.
            let mut p = base.clone();
            p.tempo_lane = Default::default();
            p.signatures.clear();
            let len = clip.region().first_pass().max(clip.length.min(64.0)).max(0.25);
            for x in p.tracks.iter_mut() {
                x.arrangement.clear();
                x.automation.clear();
            }
            let id = p.alloc_id();
            if let Some(x) = p.track_mut(track) {
                x.arrangement.push(ArrangementClip {
                    id,
                    start: 0.0,
                    length: len,
                    clip,
                    ..Default::default()
                });
            }
            slots.push(Some((Arc::new(self.render_project(&p, len)), len)));
        }
        Some(FreezeData {
            arrangement,
            arrangement_beats: arr_end,
            slots,
            tempo: self.project.tempo,
        })
    }

    pub fn freeze_track(&mut self, track: Id) {
        let Some(t) = self.project.track(track) else { return };
        if t.kind == TrackKind::Group || t.frozen {
            return;
        }
        let name = t.name.clone();
        let Some(data) = self.render_freeze(track) else { return };
        self.label_edit("Freeze Track");
        if let Some(t) = self.project.track_mut(track) {
            t.frozen = true;
            t.freeze = Some(Arc::new(data));
        }
        self.status = format!("Froze {name}");
        self.changed();
    }

    pub fn unfreeze_track(&mut self, track: Id) {
        self.label_edit("Unfreeze Track");
        if let Some(t) = self.project.track_mut(track) {
            t.frozen = false;
            t.freeze = None;
        }
        self.changed();
    }

    /// Renders frozen tracks again after a project was opened (the frozen
    /// audio isn't saved).
    pub fn refreeze_after_load(&mut self) {
        let ids: Vec<Id> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.frozen && t.freeze.is_none())
            .map(|t| t.id)
            .collect();
        for id in ids {
            if let Some(d) = self.render_freeze(id) {
                if let Some(t) = self.project.track_mut(id) {
                    t.freeze = Some(Arc::new(d));
                }
            }
        }
        if let Some(c) = self.work.committed.as_mut() {
            // The history shouldn't see this as an edit.
            *c = self.project.clone();
        }
        self.needs_sync = true;
    }

    /// Saves processed audio to the recordings folder (collected next to the
    /// project when it is saved) and returns a clip reference to it.
    fn save_audio(&mut self, data: AudioData, name: &str) -> Option<AudioRef> {
        let dir = temp_recordings_dir();
        let _ = std::fs::create_dir_all(&dir);
        let stem: String = name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == ' ' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let id = self.project.alloc_id();
        let path: PathBuf = dir.join(format!("{stem} {id}.wav"));
        if let Err(e) = write_float_wav(&path, &data) {
            self.status = format!("Could not write {}: {e}", path.display());
            return None;
        }
        Some(AudioRef::new(path.to_string_lossy(), Some(Arc::new(data))))
    }

    /// Flatten: the frozen audio replaces the track's instrument, devices and
    /// clips, and the track becomes an audio track.
    pub fn flatten_track(&mut self, track: Id) {
        if !self.project.track(track).is_some_and(|t| t.frozen) {
            self.freeze_track(track);
        }
        let Some(t) = self.project.track(track) else { return };
        let Some(f) = t.freeze.clone() else { return };
        let (name, color) = (t.name.clone(), t.color);
        let looping: Vec<bool> = t.slots.iter().map(|c| c.as_ref().is_none_or(|c| c.looping)).collect();
        self.label_edit("Flatten Track");
        let mut arrangement = Vec::new();
        if let Some(data) = &f.arrangement {
            if let Some(audio) = self.save_audio(AudioData::clone(data), &name) {
                let id = self.project.alloc_id();
                let cid = self.project.alloc_id();
                arrangement.push(ArrangementClip {
                    id,
                    start: 0.0,
                    length: f.arrangement_beats,
                    clip: Clip {
                        id: cid,
                        name: format!("{name} Flattened"),
                        color,
                        length: f.arrangement_beats,
                        looping: false,
                        content: ClipContent::Audio(audio),
                        ..Default::default()
                    },
                    ..Default::default()
                });
            }
        }
        let mut slots = Vec::new();
        for (s, entry) in f.slots.iter().enumerate() {
            let clip = match entry {
                Some((data, len)) => self.save_audio(AudioData::clone(data), &name).map(|audio| Clip {
                    id: self.project.alloc_id(),
                    name: format!("{name} {}", s + 1),
                    color,
                    length: *len,
                    looping: looping.get(s).copied().unwrap_or(true),
                    content: ClipContent::Audio(audio),
                    ..Default::default()
                }),
                None => None,
            };
            slots.push(clip);
        }
        if let Some(t) = self.project.track_mut(track) {
            t.kind = TrackKind::Audio;
            t.instrument = Instrument::None;
            t.devices.clear();
            t.midi_devices.clear();
            t.samples.clear();
            t.automation
                .retain(|l| !matches!(l.target, tessera_engine::automation::AutomationTarget::Instrument(_)));
            t.automation
                .retain(|l| !matches!(l.target, tessera_engine::automation::AutomationTarget::Device { .. }));
            t.arrangement = arrangement;
            slots.resize(t.slots.len(), None);
            t.slots = slots;
            t.frozen = false;
            t.freeze = None;
        }
        self.project.prune_plugins();
        self.status = format!("Flattened {name}");
        self.changed();
    }

    // ----- arrangement editing -----------------------------------------------------------

    /// Where edits like Split act: the time selection, else the playhead on
    /// the selected track.
    fn edit_range(&self) -> Option<(Id, f64, f64)> {
        if let Some(s) = self.work.selection.filter(|s| s.take.is_none()) {
            return Some((s.track, s.from, s.to));
        }
        let t = self.selected_track.filter(|t| self.project.track(*t).is_some())?;
        let b = self.song_beat();
        Some((t, b, b))
    }

    /// Split (Ctrl/Cmd+E): at the selection edges, or at the playhead.
    pub fn split(&mut self) {
        let Some((track, from, to)) = self.edit_range() else {
            return;
        };
        self.label_edit("Split");
        let mut any = clip_edit::split_at(&mut self.project, track, from).is_some();
        if to > from + 1e-9 {
            any |= clip_edit::split_at(&mut self.project, track, to).is_some();
        }
        if any {
            self.changed();
        }
    }

    /// Deletes the selected time range on its track.
    pub fn delete_selection(&mut self) -> bool {
        let Some(s) = self.work.selection.filter(|s| s.to > s.from) else {
            return false;
        };
        self.label_edit("Delete Time Selection");
        let mut ids: Vec<Id> = (0..4).map(|_| self.project.alloc_id()).collect();
        let mut alloc = || ids.pop().unwrap_or(0);
        let Some(t) = self.project.track_mut(s.track) else {
            return false;
        };
        match s.take {
            Some(take) => {
                if let Some(l) = t.takes.iter_mut().find(|l| l.id == take) {
                    clip_edit::cut_range(&mut l.clips, s.from, s.to, &mut alloc);
                }
            }
            None => clip_edit::cut_range(&mut t.arrangement, s.from, s.to, &mut alloc),
        }
        self.work.selection = None;
        self.changed();
        true
    }

    /// Consolidate (Ctrl/Cmd+J): the time selection, or the selected clip,
    /// becomes one clip. MIDI is merged; audio is rendered.
    pub fn consolidate(&mut self) {
        let range = match (self.work.selection, self.selected_clip) {
            (Some(s), _) if s.to > s.from && s.take.is_none() => Some((s.track, s.from, s.to)),
            (_, Some(ClipRef::Arrangement { track, id })) => self
                .project
                .track(track)
                .and_then(|t| t.arrangement.iter().find(|a| a.id == id))
                .map(|a| (track, a.start, a.end())),
            _ => None,
        };
        let Some((track, from, to)) = range else {
            self.status = "Select a time range or an Arrangement clip to consolidate".into();
            return;
        };
        let Some(kind) = self.project.track(track).map(|t| t.kind) else {
            return;
        };
        self.label_edit("Consolidate");
        let new = if kind == TrackKind::Audio {
            self.consolidate_audio(track, from, to)
        } else {
            clip_edit::consolidate_midi(&mut self.project, track, from, to)
        };
        if let Some(id) = new {
            self.select_clip(ClipRef::Arrangement { track, id });
            self.work.selection = None;
            self.changed();
        }
    }

    fn consolidate_audio(&mut self, track: Id, from: f64, to: f64) -> Option<Id> {
        let t = self.project.track(track)?;
        let mut next: Id = 1_000_000_000;
        let mut alloc = || {
            next += 1;
            next
        };
        let mut pieces = clip_edit::copy_range(&t.arrangement, from, to, &mut alloc);
        if pieces.is_empty() {
            return None;
        }
        let (name, color) = (pieces[0].clip.name.clone(), pieces[0].clip.color);
        for a in pieces.iter_mut() {
            a.start -= from;
        }
        // Render just these clips: no devices, unity gain, constant tempo.
        let mut p = self.solo_project(track)?;
        p.tempo_lane = Default::default();
        p.signatures.clear();
        p.tempo = TimeMap::from_project(&self.project).tempo_at(from);
        if let Some(x) = p.track_mut(track) {
            x.arrangement = pieces;
            x.devices.clear();
            x.automation.clear();
        }
        let data = self.render_project(&p, to - from);
        let mut audio = self.save_audio(data, &name)?;
        audio.warp.enabled = true;
        audio.warp.bpm = p.tempo;
        audio.warp.mode = tessera_engine::warp::WarpMode::Complex;
        let id = self.project.alloc_id();
        let cid = self.project.alloc_id();
        let mut ids: Vec<Id> = (0..4).map(|_| self.project.alloc_id()).collect();
        let mut alloc = || ids.pop().unwrap_or(0);
        let lane = &mut self.project.track_mut(track)?.arrangement;
        clip_edit::cut_range(lane, from, to, &mut alloc);
        lane.push(ArrangementClip {
            id,
            start: from,
            length: to - from,
            clip: Clip {
                id: cid,
                name,
                color,
                length: to - from,
                looping: true,
                content: ClipContent::Audio(audio),
                ..Default::default()
            },
            ..Default::default()
        });
        lane.sort_by(|a, b| a.start.total_cmp(&b.start));
        Some(id)
    }

    /// Reverses the selected clip (Session or Arrangement).
    pub fn reverse_clip(&mut self, r: ClipRef) {
        self.label_edit("Reverse Clip");
        let name = self.clip(r).map(|c| c.name.clone()).unwrap_or_default();
        let Some(clip) = self.clip_mut(r) else { return };
        if let Some(rev) = clip_edit::reverse(clip) {
            if let Some(audio) = self.save_audio(rev, &format!("{name} Reversed")) {
                if let Some(ClipContent::Audio(a)) = self.clip_mut(r).map(|c| &mut c.content) {
                    a.path = audio.path;
                    a.data = audio.data;
                }
            }
        }
        self.changed();
    }

    /// Crossfades an Arrangement clip into the clip right after it.
    pub fn crossfade_next(&mut self, track: Id, id: Id, beats: f64) {
        self.label_edit("Create Crossfade");
        if clip_edit::crossfade(&mut self.project, track, id, beats) {
            self.changed();
        } else {
            self.status = "Crossfades need a clip starting where this one ends".into();
        }
    }

    pub fn set_fades(&mut self, track: Id, id: Id, fade_in: f64, fade_out: f64) {
        self.label_edit("Clip Fade");
        if let Some(a) = self
            .project
            .track_mut(track)
            .and_then(|t| t.arrangement.iter_mut().find(|a| a.id == id))
        {
            a.fade_in = fade_in.clamp(0.0, a.length);
            a.fade_out = fade_out.clamp(0.0, a.length - a.fade_in);
        }
        self.changed();
    }

    /// Comping: copies the selected range of a take lane to the main lane.
    pub fn comp(&mut self, track: Id, take: Id, from: f64, to: f64) {
        self.label_edit("Comp Take");
        if clip_edit::comp(&mut self.project, track, take, from, to) {
            self.changed();
        }
    }

    pub fn toggle_takes(&mut self, track: Id) {
        if let Some(t) = self.project.track_mut(track) {
            t.show_takes = !t.show_takes;
        }
        self.dirty = true;
    }

    pub fn delete_take(&mut self, track: Id, take: Id) {
        self.label_edit("Delete Take Lane");
        if let Some(t) = self.project.track_mut(track) {
            t.takes.retain(|l| l.id != take);
        }
        self.touch();
    }

    /// Copies a Session clip onto the Arrangement at a beat.
    pub fn slot_to_arrangement_at(&mut self, track: Id, scene: usize, target: Id, beat: f64) {
        let Some(mut clip) = self.clip(ClipRef::Slot { track, scene }).cloned() else {
            return;
        };
        let (src_kind, dst_kind) = (
            self.project.track(track).map(|t| t.kind),
            self.project.track(target).map(|t| t.kind),
        );
        if src_kind != dst_kind {
            self.status = "Clips go onto a track of the same kind".into();
            return;
        }
        self.label_edit("Add Clip to Arrangement");
        clip.id = self.project.alloc_id();
        let id = self.project.alloc_id();
        let length = clip.region().first_pass().max(clip.length);
        let piece = ArrangementClip {
            id,
            start: beat.max(0.0),
            length,
            clip,
            ..Default::default()
        };
        let mut ids: Vec<Id> = (0..4).map(|_| self.project.alloc_id()).collect();
        let mut alloc = || ids.pop().unwrap_or(0);
        if let Some(t) = self.project.track_mut(target) {
            clip_edit::paste(&mut t.arrangement, vec![piece], &mut alloc);
        }
        self.select_clip(ClipRef::Arrangement { track: target, id });
        self.changed();
    }

    /// Copies an Arrangement clip into a Session slot (the first free one at
    /// or below `scene`).
    pub fn arrangement_to_slot(&mut self, track: Id, id: Id, target: Id, scene: usize) {
        let Some(a) = self
            .project
            .track(track)
            .and_then(|t| t.arrangement.iter().find(|a| a.id == id))
            .cloned()
        else {
            return;
        };
        if self.project.track(track).map(|t| t.kind) != self.project.track(target).map(|t| t.kind) {
            self.status = "Clips go onto a track of the same kind".into();
            return;
        }
        self.label_edit("Copy Clip to Session");
        let mut clip = a.clip;
        clip.id = self.project.alloc_id();
        let Some(t) = self.project.track(target) else { return };
        let slot = (scene..t.slots.len()).find(|&s| t.slots[s].is_none());
        let slot = match slot {
            Some(s) => s,
            None => self.project.add_scene(),
        };
        if let Some(t) = self.project.track_mut(target) {
            t.slots[slot] = Some(clip);
        }
        self.select_clip(ClipRef::Slot {
            track: target,
            scene: slot,
        });
        self.changed();
    }

    // ----- locators and song time ---------------------------------------------------

    pub fn time_map(&self) -> TimeMap {
        TimeMap::from_project(&self.project)
    }

    pub fn add_locator_at(&mut self, beat: f64) {
        self.label_edit("Add Locator");
        arrange::add_locator(&mut self.project, beat, None);
        self.touch();
    }

    pub fn delete_locator(&mut self, id: Id) {
        self.label_edit("Delete Locator");
        self.project.locators.retain(|l| l.id != id);
        self.touch();
    }

    /// Jumps to the previous or next locator.
    pub fn jump_locator(&mut self, forward: bool) {
        let b = self.song_beat();
        if let Some(l) = arrange::adjacent_locator(&self.project, b, forward) {
            let beat = l.beat;
            self.send(Command::SetPosition(beat));
        }
    }

    pub fn add_signature_at(&mut self, beat: f64, numerator: u32, denominator: u32) {
        self.label_edit("Time Signature Change");
        arrange::add_signature(&mut self.project, beat, numerator, denominator);
        let (n, d) = (self.project.beats_per_bar, self.project.beat_unit);
        self.send(Command::SetSignature(n, d));
        self.changed();
    }

    pub fn delete_signature(&mut self, index: usize) {
        if index < self.project.signatures.len() {
            self.label_edit("Delete Time Signature Change");
            self.project.signatures.remove(index);
            self.changed();
        }
    }

    pub fn set_punch(&mut self, punch_in: bool, punch_out: bool) {
        self.project.punch_in = punch_in;
        self.project.punch_out = punch_out;
        self.changed();
    }

    /// Whether the tempo was changed by hand while the Song Tempo lane is
    /// automated (lights the Re-Enable button).
    pub fn tempo_overridden(&self) -> bool {
        self.project.tempo_lane.is_active()
            && (self.time_map().tempo_at(self.song_beat()) - self.engine.shared.tempo.load()).abs() > 0.5
    }

    /// Re-Enable the Song Tempo automation after a manual tempo change.
    pub fn follow_tempo_automation(&mut self) {
        self.send(Command::FollowTempoAutomation);
    }

    // ----- session ----------------------------------------------------------------

    /// Capture and Insert Scene (Ctrl/Cmd+Shift+I): the clips playing now
    /// are copied into a new scene below the selected one.
    pub fn capture_scene(&mut self) {
        let playing: Vec<Option<usize>> = self
            .project
            .tracks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                crate::ui::session::track_state(self, i, t.id)
                    .map(|s| s.playing_slot.load(Ordering::Relaxed))
                    .filter(|&s| s >= 0)
                    .map(|s| s as usize)
            })
            .collect();
        self.label_edit("Capture and Insert Scene");
        let tempo = Some(self.project.tempo);
        let after = self
            .work
            .selected_scene
            .min(self.project.scenes.len().saturating_sub(1));
        let at = arrange::capture_scene(&mut self.project, after, &playing, tempo);
        self.work.selected_scene = at;
        self.changed();
    }

    /// Session Record: armed tracks record into the selected scene, or the
    /// next free slot below (adding a scene when needed).
    pub fn session_record(&mut self) {
        let armed: Vec<Id> = self
            .project
            .tracks
            .iter()
            .filter(|t| t.mixer.arm && t.kind != TrackKind::Group)
            .map(|t| t.id)
            .collect();
        if armed.is_empty() {
            self.status = "Arm a track to record into the Session".into();
            return;
        }
        let scene = self.work.selected_scene;
        let mut targets = Vec::new();
        for t in armed {
            if let Some(slot) = arrange::record_slot(&mut self.project, t, scene) {
                targets.push((t, slot));
            }
        }
        self.changed();
        self.flush();
        for (t, slot) in targets {
            self.record_slot(t, slot);
        }
    }

    /// Adds or removes the stop button of an empty slot.
    pub fn toggle_stop_button(&mut self, track: Id, scene: usize) {
        self.label_edit("Stop Button");
        if let Some(t) = self.project.track_mut(track) {
            if let Some(i) = t.no_stop_slots.iter().position(|&s| s == scene) {
                t.no_stop_slots.remove(i);
            } else {
                t.no_stop_slots.push(scene);
            }
        }
        self.changed();
    }

    /// Launches the clips of a group's members in a scene (the group's own
    /// slot button).
    pub fn launch_group_slot(&mut self, group: Id, scene: usize) {
        let members: Vec<(Id, bool)> = arrange::track_block(&self.project, group)
            .into_iter()
            .skip(1)
            .map(|i| &self.project.tracks[i])
            .filter(|t| t.kind != TrackKind::Group)
            .map(|t| (t.id, t.slots.get(scene).is_some_and(|s| s.is_some())))
            .collect();
        for (t, has) in members {
            if has {
                self.send(Command::LaunchClip { track: t, slot: scene });
                self.send(Command::ReleaseClip { track: t, slot: scene });
            } else {
                self.send(Command::StopTrack(t));
            }
        }
    }

    pub fn stop_group(&mut self, group: Id) {
        let members: Vec<Id> = arrange::track_block(&self.project, group)
            .into_iter()
            .skip(1)
            .map(|i| self.project.tracks[i].id)
            .collect();
        for t in members {
            self.send(Command::StopTrack(t));
        }
    }

    // ----- grooves ----------------------------------------------------------------

    pub fn add_groove(&mut self, mut g: Groove) -> Id {
        self.label_edit("Add Groove");
        g.id = self.project.alloc_id();
        let id = g.id;
        self.project.grooves.push(g);
        self.touch();
        id
    }

    /// Extracts a groove from a clip (MIDI notes, or an audio clip's
    /// transients) into the Groove Pool.
    pub fn extract_groove(&mut self, r: ClipRef) {
        let Some(c) = self.clip(r) else { return };
        let name = format!("{} Groove", c.name);
        let g = match &c.content {
            ClipContent::Midi { notes } => groove::extract_from_notes(&name, notes, c.length, 0.25),
            ClipContent::Audio(a) => {
                let hits: Vec<(f64, f32)> = a
                    .warp
                    .transients
                    .iter()
                    .map(|&s| {
                        let b = if a.warp.enabled {
                            a.warp.secs_to_beat(s)
                        } else {
                            s * self.project.tempo / 60.0
                        };
                        (b, 1.0)
                    })
                    .collect();
                groove::extract_from_hits(&name, &hits, c.length, 0.25)
            }
        };
        self.add_groove(g);
        self.status = format!("Extracted {name}");
        self.work.show_grooves = true;
    }

    pub fn set_clip_groove(&mut self, r: ClipRef, groove: Option<Id>) {
        self.label_edit("Clip Groove");
        if let Some(c) = self.clip_mut(r) {
            c.groove = groove;
        }
        self.changed();
    }

    /// Commit: bakes the clip's groove into its notes.
    pub fn commit_groove(&mut self, r: ClipRef) {
        let amount = self.project.groove_amount;
        let Some(c) = self.clip(r) else { return };
        let Some(g) = c
            .groove
            .and_then(|id| self.project.grooves.iter().find(|g| g.id == id))
            .cloned()
        else {
            return;
        };
        let seed = c.id;
        self.label_edit("Commit Groove");
        if let Some(c) = self.clip_mut(r) {
            if let Some(notes) = c.notes_mut() {
                *notes = groove::commit(&g, notes, amount, seed);
            }
            c.groove = None;
        }
        self.changed();
    }

    pub fn remove_groove(&mut self, id: Id) {
        self.label_edit("Delete Groove");
        self.project.grooves.retain(|g| g.id != id);
        for t in self.project.tracks.iter_mut() {
            let clips = t
                .slots
                .iter_mut()
                .flatten()
                .chain(t.arrangement.iter_mut().map(|a| &mut a.clip));
            for c in clips {
                if c.groove == Some(id) {
                    c.groove = None;
                }
            }
        }
        self.changed();
    }

    // ----- audio to MIDI ------------------------------------------------------------

    /// Maps file seconds to clip beats for an audio clip.
    fn clip_secs_to_beat(&self, a: &AudioRef) -> impl Fn(f64) -> f64 + use<> {
        let warp = a.warp.clone();
        let tempo = self.project.tempo;
        move |s: f64| {
            if warp.enabled {
                warp.secs_to_beat(s)
            } else {
                s * tempo / 60.0
            }
        }
    }

    fn clip_beat_to_secs(&self, a: &AudioRef) -> impl Fn(f64) -> f64 + use<> {
        let warp = a.warp.clone();
        let tempo = self.project.tempo;
        move |b: f64| {
            if warp.enabled {
                warp.beat_to_secs(b)
            } else {
                b * 60.0 / tempo
            }
        }
    }

    /// Puts a new MIDI clip on a new MIDI track where `r` was (same slot or
    /// the same Arrangement position).
    fn place_on_new_track(&mut self, r: ClipRef, clip: Clip, track: Id) {
        match r {
            ClipRef::Slot { scene, .. } => {
                if let Some(t) = self.project.track_mut(track) {
                    if let Some(s) = t.slots.get_mut(scene) {
                        *s = Some(clip);
                    }
                }
                self.select_clip(ClipRef::Slot { track, scene });
            }
            ClipRef::Arrangement { track: src, id } => {
                let pos = self
                    .project
                    .track(src)
                    .and_then(|t| t.arrangement.iter().find(|a| a.id == id))
                    .map(|a| (a.start, a.length))
                    .unwrap_or((0.0, clip.length));
                let aid = self.project.alloc_id();
                if let Some(t) = self.project.track_mut(track) {
                    t.arrangement.push(ArrangementClip {
                        id: aid,
                        start: pos.0,
                        length: pos.1,
                        clip,
                        ..Default::default()
                    });
                }
                self.select_clip(ClipRef::Arrangement { track, id: aid });
            }
        }
    }

    /// Convert Melody / Harmony / Drums to New MIDI Track. The analysis runs
    /// in the background; [`Self::poll_conversion`] adds the track when done.
    pub fn convert_to_midi(&mut self, r: ClipRef, mode: ConvertMode) {
        if self.work.convert.is_some() {
            self.status = "Already converting a clip to MIDI".into();
            return;
        }
        let Some(c) = self.clip(r).cloned() else { return };
        let ClipContent::Audio(a) = &c.content else { return };
        let Some(data) = a.data.clone() else {
            self.status = "The clip's audio file is missing".into();
            return;
        };
        let to_beat = self.clip_secs_to_beat(a);
        let thread = std::thread::spawn(move || audio_to_midi::convert(&data, mode, &to_beat));
        self.status = format!("Converting {} to MIDI…", mode_name(mode));
        self.work.convert = Some(ConvertJob {
            from: r,
            mode,
            clip: c,
            thread,
        });
    }

    /// Whether an audio-to-MIDI conversion is running.
    pub fn converting(&self) -> bool {
        self.work.convert.is_some()
    }

    /// Picks up a finished audio-to-MIDI conversion: a new MIDI track with the
    /// notes, where the source clip was.
    pub fn poll_conversion(&mut self) {
        if !self.work.convert.as_ref().is_some_and(|j| j.thread.is_finished()) {
            return;
        }
        let Some(job) = self.work.convert.take() else { return };
        let ConvertJob {
            from,
            mode,
            clip: c,
            thread,
        } = job;
        let Ok(found) = thread.join() else {
            self.status = "Audio to MIDI conversion failed".into();
            return;
        };
        let mut notes: Vec<Note> = found.into_iter().filter(|n| n.start < c.length).collect();
        for n in notes.iter_mut() {
            n.length = n.length.min(c.length - n.start);
        }
        if notes.is_empty() {
            self.status = "No notes found in the audio".into();
            return;
        }
        self.label_edit(match mode {
            ConvertMode::Melody => "Convert Melody to MIDI",
            ConvertMode::Harmony => "Convert Harmony to MIDI",
            ConvertMode::Drums => "Convert Drums to MIDI",
        });
        let (instrument, label) = match mode {
            ConvertMode::Drums => (
                Instrument::DrumRack(tessera_engine::dsp::drum_rack::DrumRackParams::standard()),
                "Drums",
            ),
            ConvertMode::Harmony => (Instrument::Electric(ElectricParams::default()), "Harmony"),
            ConvertMode::Melody => (Instrument::Synth(SynthParams::default()), "Melody"),
        };
        let count = notes.len();
        let track = self.add_track(TrackKind::Midi, instrument);
        let n = self.project.tracks.len();
        if let Some(t) = self.project.track_mut(track) {
            t.name = format!("{n} {} {label}", c.name);
        }
        let color = self.project.track(track).map(|t| t.color).unwrap_or(c.color);
        let clip = Clip {
            id: self.project.alloc_id(),
            name: format!("{} {label}", c.name),
            color,
            length: c.length,
            looping: c.looping,
            content: ClipContent::Midi { notes },
            ..Default::default()
        };
        self.place_on_new_track(from, clip, track);
        self.status = format!("Converted {count} notes");
        self.changed();
    }

    /// Slice to New MIDI Track: a Sampler in Slice mode holding the clip's
    /// audio, with a clip playing the slices in order. `division` is a beat
    /// length, or `None` to slice at transients.
    pub fn slice_to_midi(&mut self, r: ClipRef, division: Option<f64>) {
        let Some(c) = self.clip(r).cloned() else { return };
        let ClipContent::Audio(a) = &c.content else { return };
        let Some(data) = a.data.clone() else {
            self.status = "The clip's audio file is missing".into();
            return;
        };
        let to_beat = self.clip_secs_to_beat(a);
        let to_secs = self.clip_beat_to_secs(a);
        let dur = data.duration_secs();
        // Only the part of the file the clip plays.
        let clip_end_secs = to_secs(c.length).min(dur);
        let points: Vec<f64> = audio_to_midi::slice_points(&data, division, MAX_SLICES, &to_beat, &to_secs)
            .into_iter()
            .filter(|&s| s < clip_end_secs)
            .collect();
        if points.is_empty() || dur <= 0.0 {
            return;
        }
        self.label_edit("Slice to New MIDI Track");
        let mut params = SamplerParams::sliced(points.len());
        let fractions: Vec<f32> = points.iter().map(|&s| (s / dur) as f32).collect();
        params.set_slices(&fractions);
        let track = self.add_track(TrackKind::Midi, Instrument::Sampler(params));
        let n = self.project.tracks.len();
        if let Some(t) = self.project.track_mut(track) {
            t.name = format!("{n} {} Slices", c.name);
            t.set_sample(
                0,
                Some(SampleRef {
                    path: a.path.clone(),
                    data: Some(data.clone()),
                }),
            );
        }
        let mut notes = Vec::new();
        for (i, &s) in points.iter().enumerate() {
            let start = to_beat(s).max(0.0);
            let end = points.get(i + 1).map(|&e| to_beat(e)).unwrap_or(c.length).min(c.length);
            if end <= start {
                continue;
            }
            notes.push(Note {
                pitch: SLICE_BASE_NOTE + i as u8,
                velocity: 100,
                start,
                length: end - start,
            });
        }
        let color = self.project.track(track).map(|t| t.color).unwrap_or(c.color);
        let clip = Clip {
            id: self.project.alloc_id(),
            name: format!("{} Slices", c.name),
            color,
            length: c.length,
            looping: c.looping,
            content: ClipContent::Midi { notes },
            ..Default::default()
        };
        self.place_on_new_track(r, clip, track);
        self.status = format!("Sliced into {} parts", points.len());
        self.changed();
    }

    // ----- Capture MIDI ----------------------------------------------------------------

    /// The MIDI track live notes go to: the armed one, else the selected.
    pub fn capture_target(&self) -> Option<Id> {
        let armed = self
            .project
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Midi && t.mixer.arm)
            .map(|t| t.id);
        armed.or(self
            .selected_track
            .filter(|t| self.project.track(*t).is_some_and(|t| t.kind == TrackKind::Midi)))
    }

    pub fn capture_note(&mut self, track: Id, pitch: u8, velocity: u8, on: bool, now: f64) {
        let beat = self.is_playing().then(|| self.song_beat());
        let buf = self.work.capture.entry(track).or_default();
        if on {
            buf.note_on(pitch, velocity, now, beat);
        } else {
            buf.note_off(pitch, now, beat);
        }
    }

    /// Capture MIDI: turns what was just played into a clip.
    pub fn capture_midi(&mut self, now: f64) {
        let Some(track) = self.capture_target() else {
            self.status = "Select or arm a MIDI track to capture".into();
            return;
        };
        let bpb = self.time_map().meter_at(self.song_beat()).bar_beats();
        let Some(cap) = self
            .work
            .capture
            .get(&track)
            .and_then(|b| b.capture(now, self.project.tempo, bpb))
        else {
            self.status = "Nothing was played to capture".into();
            return;
        };
        self.label_edit("Capture MIDI");
        let color = self.project.track(track).map(|t| t.color).unwrap_or([200, 200, 200]);
        let mut clip = self.new_midi_clip(cap.length);
        clip.name = "Captured".into();
        clip.color = color;
        if let Some(n) = clip.notes_mut() {
            *n = cap.notes.clone();
        }
        // Played freely with nothing running: the tempo follows the playing.
        let anything_playing = self.is_playing();
        if let (Some(t), false) = (cap.tempo, anything_playing) {
            self.set_tempo(t.round());
        }
        match (self.view, cap.start_beat) {
            (View::Arrangement, start) => {
                let start = start.unwrap_or_else(|| self.time_map().next_bar(self.song_beat()));
                let id = self.project.alloc_id();
                let mut ids: Vec<Id> = (0..4).map(|_| self.project.alloc_id()).collect();
                let mut alloc = || ids.pop().unwrap_or(0);
                let piece = ArrangementClip {
                    id,
                    start,
                    length: cap.length,
                    clip,
                    ..Default::default()
                };
                if let Some(t) = self.project.track_mut(track) {
                    clip_edit::paste(&mut t.arrangement, vec![piece], &mut alloc);
                }
                self.select_clip(ClipRef::Arrangement { track, id });
            }
            (View::Session, _) => {
                let scene = self.work.selected_scene;
                let Some(slot) = arrange::record_slot(&mut self.project, track, scene) else {
                    return;
                };
                if let Some(t) = self.project.track_mut(track) {
                    t.slots[slot] = Some(clip);
                }
                self.select_clip(ClipRef::Slot { track, scene: slot });
                if anything_playing {
                    self.send(Command::LaunchClip { track, slot });
                    self.send(Command::ReleaseClip { track, slot });
                }
            }
        }
        if let Some(b) = self.work.capture.get_mut(&track) {
            b.clear();
        }
        self.status = format!("Captured {} notes", cap.notes.len());
        self.changed();
    }
}

fn mode_name(mode: ConvertMode) -> &'static str {
    match mode {
        ConvertMode::Melody => "Melody",
        ConvertMode::Harmony => "Harmony",
        ConvertMode::Drums => "Drums",
    }
}
