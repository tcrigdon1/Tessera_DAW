//! Track and song editing operations on the project document: grouping,
//! duplicating and reordering tracks, locators, signature changes and
//! capturing the playing Session clips into a scene.
//!
//! These run on the UI thread and only change the [`Project`]; the UI then
//! rebuilds the engine snapshot.

use std::collections::HashMap;

use crate::automation::AutomationTarget;
use crate::model::{Id, Instrument, Locator, Project, Scene, Track, TrackKind};
use crate::timeline::{SignatureChange, TimeMap};

/// Indices of a track and everything grouped under it (recursively), in
/// track order. Members of a group always sit right after it.
pub fn track_block(p: &Project, id: Id) -> Vec<usize> {
    let Some(start) = p.track_index(id) else {
        return Vec::new();
    };
    let mut ids = vec![id];
    let mut out = vec![start];
    for (i, t) in p.tracks.iter().enumerate().skip(start + 1) {
        match t.output {
            Some(o) if ids.contains(&o) => {
                ids.push(t.id);
                out.push(i);
            }
            _ => break,
        }
    }
    out
}

/// How deep a track is nested in groups (0 = top level).
pub fn depth(p: &Project, id: Id) -> usize {
    let mut d = 0;
    let mut cur = p.track(id).and_then(|t| t.output);
    while let Some(o) = cur {
        d += 1;
        if d > p.tracks.len() {
            break;
        }
        cur = p.track(o).and_then(|t| t.output);
    }
    d
}

/// True if any group above the track is folded (the track is hidden).
pub fn hidden(p: &Project, id: Id) -> bool {
    let mut cur = p.track(id).and_then(|t| t.output);
    let mut guard = 0;
    while let Some(o) = cur {
        let Some(g) = p.track(o) else { break };
        if g.folded {
            return true;
        }
        cur = g.output;
        guard += 1;
        if guard > p.tracks.len() {
            break;
        }
    }
    false
}

/// Puts tracks into a new group track inserted where the first of them
/// was. The members move together under the group. Returns the group's id.
pub fn group_tracks(p: &mut Project, ids: &[Id]) -> Option<Id> {
    let mut indices: Vec<usize> = ids.iter().filter_map(|&id| p.track_index(id)).collect();
    if indices.is_empty() {
        return None;
    }
    indices.sort_unstable();
    indices.dedup();
    // Whole blocks move, so grouping a group takes its members along.
    let mut block: Vec<usize> = Vec::new();
    for &i in &indices {
        for j in track_block(p, p.tracks[i].id) {
            if !block.contains(&j) {
                block.push(j);
            }
        }
    }
    block.sort_unstable();
    let first = block[0];
    // The new group sits where the first member was, inside the same
    // parent group.
    let parent = p.tracks[first].output;
    let gid = p.add_track(TrackKind::Group, Instrument::None);
    let mut group = p.tracks.pop().expect("just added");
    group.output = parent;
    group.name = "Group".into();
    let top: Vec<Id> = indices.iter().map(|&i| p.tracks[i].id).collect();
    let mut moved: Vec<Track> = Vec::new();
    for &i in block.iter().rev() {
        moved.push(p.tracks.remove(i));
    }
    moved.reverse();
    for t in moved.iter_mut() {
        if top.contains(&t.id) {
            t.output = Some(gid);
        }
    }
    let at = first.min(p.tracks.len());
    p.tracks.insert(at, group);
    for (k, t) in moved.into_iter().enumerate() {
        p.tracks.insert(at + 1 + k, t);
    }
    Some(gid)
}

/// Removes a group track; its members go where the group went.
pub fn ungroup(p: &mut Project, group: Id) {
    let Some(g) = p.track(group).filter(|t| t.kind == TrackKind::Group) else {
        return;
    };
    let parent = g.output;
    for t in p.tracks.iter_mut() {
        if t.output == Some(group) {
            t.output = parent;
        }
    }
    p.remove_track(group);
}

/// Moves a track (with its group members) so it starts at `to` (an index in
/// the track list before the move). A track dropped among another group's
/// members joins that group.
pub fn move_track(p: &mut Project, id: Id, to: usize) {
    let block = track_block(p, id);
    if block.is_empty() {
        return;
    }
    let first = block[0];
    let last = *block.last().expect("not empty");
    if to >= first && to <= last + 1 {
        return;
    }
    let moved: Vec<Track> = p.tracks.drain(first..=last).collect();
    let at = if to > last { to - moved.len() } else { to };
    let at = at.min(p.tracks.len());
    // Join the group of the track it lands before, or leave groups when
    // dropped at the top level.
    let new_parent = p.tracks.get(at).and_then(|t| t.output).filter(|&o| o != id);
    let mut moved = moved;
    moved[0].output = new_parent;
    for (k, t) in moved.into_iter().enumerate() {
        p.tracks.insert(at + k, t);
    }
}

/// Duplicates a track (a group with all its members) right after it, with
/// fresh ids for tracks, clips and devices. Plug-in states are copied too.
/// Returns the new track's id.
pub fn duplicate_track(p: &mut Project, id: Id) -> Option<Id> {
    let block = track_block(p, id);
    if block.is_empty() {
        return None;
    }
    let mut map: HashMap<Id, Id> = HashMap::new();
    let mut copies: Vec<Track> = Vec::new();
    for &i in &block {
        let mut t = p.tracks[i].clone();
        let new_id = p.alloc_id();
        map.insert(t.id, new_id);
        t.id = new_id;
        t.name = format!("{} copy", t.name);
        for c in t.slots.iter_mut().flatten() {
            c.id = p.alloc_id();
        }
        for a in t.arrangement.iter_mut() {
            a.id = p.alloc_id();
            a.clip.id = p.alloc_id();
        }
        for lane in t.takes.iter_mut() {
            lane.id = p.alloc_id();
            for a in lane.clips.iter_mut() {
                a.id = p.alloc_id();
                a.clip.id = p.alloc_id();
            }
        }
        for d in t.devices.iter_mut() {
            let nd = p.alloc_id();
            map.insert(d.id, nd);
            d.id = nd;
        }
        copies.push(t);
    }
    for t in copies.iter_mut() {
        if let Some(o) = t.output {
            if let Some(&n) = map.get(&o) {
                t.output = Some(n);
            }
        }
        let remap = |target: &mut AutomationTarget| {
            if let AutomationTarget::Device { device, .. } = target {
                if let Some(&n) = map.get(device) {
                    *device = n;
                }
            }
        };
        for l in t.automation.iter_mut() {
            remap(&mut l.target);
        }
        let clips = t
            .slots
            .iter_mut()
            .flatten()
            .chain(t.arrangement.iter_mut().map(|a| &mut a.clip));
        for c in clips {
            for l in c.envelopes.iter_mut() {
                remap(&mut l.target);
            }
        }
    }
    let new_plugins: Vec<_> = p
        .plugins
        .iter()
        .filter_map(|s| {
            map.get(&s.slot).map(|&n| {
                let mut s = s.clone();
                s.slot = n;
                s
            })
        })
        .collect();
    p.plugins.extend(new_plugins);
    let at = block.last().expect("not empty") + 1;
    let new_id = copies[0].id;
    for (k, t) in copies.into_iter().enumerate() {
        p.tracks.insert(at + k, t);
    }
    Some(new_id)
}

/// Deletes a track; a group takes its members with it.
pub fn delete_track(p: &mut Project, id: Id) {
    let ids: Vec<Id> = track_block(p, id).into_iter().map(|i| p.tracks[i].id).collect();
    for id in ids {
        p.remove_track(id);
    }
    p.prune_plugins();
}

/// Adds a locator at a beat (replacing one already there) and returns its id.
pub fn add_locator(p: &mut Project, beat: f64, name: Option<String>) -> Id {
    let beat = beat.max(0.0);
    if let Some(l) = p.locators.iter().find(|l| (l.beat - beat).abs() < 1e-6) {
        return l.id;
    }
    let id = p.alloc_id();
    let name = name.unwrap_or_else(|| format!("{}", p.locators.len() + 1));
    p.locators.push(Locator { id, name, beat });
    p.locators.sort_by(|a, b| a.beat.total_cmp(&b.beat));
    id
}

/// The nearest locator after (`forward`) or before a beat.
pub fn adjacent_locator(p: &Project, beat: f64, forward: bool) -> Option<&Locator> {
    if forward {
        p.locators.iter().find(|l| l.beat > beat + 1e-6)
    } else {
        p.locators.iter().rev().find(|l| l.beat < beat - 1e-6)
    }
}

/// Inserts a time signature change on the bar line at or before `beat`.
/// A change matching the signature already in effect is not added.
pub fn add_signature(p: &mut Project, beat: f64, numerator: u32, denominator: u32) {
    let map = TimeMap::from_project(p);
    let (bar, _) = map.bar_of(beat.max(0.0));
    let at = map.bar_start(bar);
    if at <= 0.0 {
        p.beats_per_bar = numerator.clamp(1, 32);
        p.beat_unit = denominator.clamp(1, 32);
        // Bar lines after it move, so re-anchor the later changes.
        return;
    }
    p.signatures.retain(|s| (s.beat - at).abs() > 1e-6);
    let m = map.meter_at(at - 1e-6);
    if m.numerator == numerator && m.denominator == denominator {
        return;
    }
    p.signatures.push(SignatureChange {
        beat: at,
        numerator: numerator.clamp(1, 32),
        denominator: denominator.clamp(1, 32),
    });
    p.signatures.sort_by(|a, b| a.beat.total_cmp(&b.beat));
}

/// Capture and Insert Scene: a new scene below `after` holding copies of
/// the clips that are playing (`playing[i]` is the slot playing on track
/// `i`), and the same tempo. Returns the new scene's index.
pub fn capture_scene(p: &mut Project, after: usize, playing: &[Option<usize>], tempo: Option<f64>) -> usize {
    let index = (after + 1).min(p.scenes.len());
    p.insert_scene(index);
    p.scenes[index].tempo = tempo;
    for (ti, slot) in playing.iter().enumerate() {
        let Some(slot) = *slot else { continue };
        // Slots at or after the inserted row moved down by one.
        let src = if slot >= index { slot + 1 } else { slot };
        let Some(mut clip) = p.tracks.get(ti).and_then(|t| t.slots.get(src)).cloned().flatten() else {
            continue;
        };
        clip.id = p.alloc_id();
        p.tracks[ti].slots[index] = Some(clip);
    }
    index
}

/// The slot to record into on an armed track when Session Record is
/// pressed: the selected scene's slot if empty, otherwise the first empty
/// slot below it. When every slot below is full a scene is added (Ableton
/// adds a new scene). Returns the slot index.
pub fn record_slot(p: &mut Project, track: Id, selected_scene: usize) -> Option<usize> {
    let t = p.track(track)?;
    let free = (selected_scene..t.slots.len()).find(|&s| t.slots[s].is_none());
    Some(match free {
        Some(s) => s,
        None => {
            let at = p.scenes.len();
            p.insert_scene(at);
            at
        }
    })
}

/// Renames the scene with a default name.
pub fn scene_label(s: &Scene) -> String {
    let mut out = s.name.clone();
    if let Some(t) = s.tempo {
        out.push_str(&format!("  {t:.0} BPM"));
    }
    if let Some((n, d)) = s.signature {
        out.push_str(&format!("  {n}/{d}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth::SynthParams;
    use crate::fx::{Device, DeviceKind};
    use crate::model::Clip;

    fn project(n: usize) -> (Project, Vec<Id>) {
        let mut p = Project::default();
        let ids = (0..n)
            .map(|_| p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default())))
            .collect();
        (p, ids)
    }

    fn names(p: &Project) -> Vec<Id> {
        p.tracks.iter().map(|t| t.id).collect()
    }

    #[test]
    fn grouping_moves_members_under_a_new_group() {
        let (mut p, ids) = project(4);
        let g = group_tracks(&mut p, &[ids[1], ids[3]]).unwrap();
        assert_eq!(names(&p), vec![ids[0], g, ids[1], ids[3], ids[2]]);
        assert_eq!(p.track(ids[1]).unwrap().output, Some(g));
        assert_eq!(track_block(&p, g), vec![1, 2, 3]);
        assert_eq!(depth(&p, ids[3]), 1);
        // Nested group.
        let g2 = group_tracks(&mut p, &[g]).unwrap();
        assert_eq!(depth(&p, ids[3]), 2);
        assert_eq!(track_block(&p, g2).len(), 4);
        p.track_mut(g2).unwrap().folded = true;
        assert!(hidden(&p, ids[1]));
        ungroup(&mut p, g2);
        assert_eq!(p.track(g).unwrap().output, None);
        ungroup(&mut p, g);
        assert_eq!(p.track(ids[1]).unwrap().output, None);
        assert_eq!(p.tracks.len(), 4);
    }

    #[test]
    fn moving_a_group_moves_its_members() {
        let (mut p, ids) = project(4);
        let g = group_tracks(&mut p, &[ids[0], ids[1]]).unwrap();
        // [g, 0, 1, 2, 3] -> move group to the end.
        move_track(&mut p, g, 5);
        assert_eq!(names(&p), vec![ids[2], ids[3], g, ids[0], ids[1]]);
        // Drop track 2 between the members: it joins the group.
        move_track(&mut p, ids[2], 4);
        assert_eq!(names(&p), vec![ids[3], g, ids[0], ids[2], ids[1]]);
        assert_eq!(p.track(ids[2]).unwrap().output, Some(g));
        // Move it back to the top: it leaves the group.
        move_track(&mut p, ids[2], 0);
        assert_eq!(p.track(ids[2]).unwrap().output, None);
    }

    #[test]
    fn duplicates_get_fresh_ids_and_remapped_devices() {
        let (mut p, ids) = project(1);
        let dev = p.alloc_id();
        p.tracks[0].devices.push(Device::new(dev, DeviceKind::Delay));
        p.tracks[0].automation.push(crate::automation::AutomationLane::new(AutomationTarget::Device {
            device: dev,
            param: 1,
        }));
        let cid = p.alloc_id();
        p.tracks[0].slots[0] = Some(Clip {
            id: cid,
            ..Default::default()
        });
        let g = group_tracks(&mut p, &[ids[0]]).unwrap();
        let copy = duplicate_track(&mut p, g).unwrap();
        assert_eq!(p.tracks.len(), 4);
        let member = &p.tracks[3];
        assert_eq!(member.output, Some(copy));
        assert_ne!(member.id, ids[0]);
        assert_ne!(member.devices[0].id, dev);
        assert_ne!(member.slots[0].as_ref().unwrap().id, cid);
        assert_eq!(
            member.automation[0].target,
            AutomationTarget::Device {
                device: member.devices[0].id,
                param: 1
            }
        );
        delete_track(&mut p, copy);
        assert_eq!(p.tracks.len(), 2);
    }

    #[test]
    fn locators_and_signatures() {
        let (mut p, _) = project(1);
        add_locator(&mut p, 8.0, None);
        add_locator(&mut p, 4.0, Some("Verse".into()));
        add_locator(&mut p, 8.0, None);
        assert_eq!(p.locators.len(), 2);
        assert_eq!(p.locators[0].name, "Verse");
        assert_eq!(adjacent_locator(&p, 4.0, true).unwrap().beat, 8.0);
        assert_eq!(adjacent_locator(&p, 4.0, false).map(|l| l.beat), None);
        add_signature(&mut p, 9.5, 3, 4);
        assert_eq!(p.signatures[0].beat, 8.0);
        // Same as the signature in effect: ignored.
        add_signature(&mut p, 17.0, 3, 4);
        assert_eq!(p.signatures.len(), 1);
        add_signature(&mut p, 0.0, 6, 8);
        assert_eq!((p.beats_per_bar, p.beat_unit), (6, 8));
    }

    #[test]
    fn capture_scene_copies_playing_clips() {
        let (mut p, ids) = project(2);
        let c = p.alloc_id();
        p.track_mut(ids[0]).unwrap().slots[2] = Some(Clip {
            id: c,
            name: "x".into(),
            ..Default::default()
        });
        let n = p.scenes.len();
        let at = capture_scene(&mut p, 2, &[Some(2), None], Some(100.0));
        assert_eq!(at, 3);
        assert_eq!(p.scenes.len(), n + 1);
        assert_eq!(p.tracks[0].slots[3].as_ref().unwrap().name, "x");
        assert!(p.tracks[1].slots[3].is_none());
        assert_eq!(p.scenes[3].tempo, Some(100.0));
        // Session Record: the slot is taken, so the next free one is used.
        assert_eq!(record_slot(&mut p, ids[0], 2), Some(4));
    }
}
