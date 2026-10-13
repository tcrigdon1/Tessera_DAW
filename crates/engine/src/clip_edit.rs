//! Arrangement clip editing: split, cut and copy time ranges, consolidate,
//! reverse, crossfades and comping from take lanes.
//!
//! Arrangement clips play their content through the clip's start and loop
//! markers (see [`crate::model::ClipRegion`]), so cutting a clip never
//! touches its notes or audio: the right part simply starts later.

use std::sync::Arc;

use crate::model::{ArrangementClip, AudioData, Clip, ClipContent, Id, Note, Project, TakeLane};

const EPS: f64 = 1e-9;
/// Shortest clip an edit leaves behind, in beats.
pub const MIN_CLIP: f64 = 1.0 / 64.0;

/// The part of `a` from song beat `from` to `to` (clamped to the clip), or
/// `None` if they don't overlap or the clip has already ended there. The
/// copy keeps `a`'s id; callers give it a new one.
pub fn slice(a: &ArrangementClip, from: f64, to: f64) -> Option<ArrangementClip> {
    let s = from.max(a.start);
    let e = to.min(a.end());
    if e - s < MIN_CLIP - EPS {
        return None;
    }
    let mut out = a.clone();
    let offset = s - a.start;
    if offset > EPS {
        let marker = a.clip.region().position(offset)?;
        out.clip.start_marker = marker;
        out.fade_in = 0.0;
    }
    if e < a.end() - EPS {
        out.fade_out = 0.0;
    }
    out.start = s;
    out.length = e - s;
    out.fade_in = out.fade_in.min(out.length);
    out.fade_out = out.fade_out.min(out.length);
    Some(out)
}

/// Removes song beats `from..to` from a lane of clips, trimming and
/// splitting the clips that cross the edges. `alloc` gives ids to new
/// pieces.
pub fn cut_range(clips: &mut Vec<ArrangementClip>, from: f64, to: f64, alloc: &mut impl FnMut() -> Id) {
    if to <= from {
        return;
    }
    let mut out = Vec::with_capacity(clips.len() + 1);
    for a in clips.drain(..) {
        if a.end() <= from + EPS || a.start >= to - EPS {
            out.push(a);
            continue;
        }
        if let Some(left) = slice(&a, a.start, from) {
            out.push(left);
        }
        if let Some(mut right) = slice(&a, to, a.end()) {
            if a.start < from {
                // The original id stays with the left part.
                right.id = alloc();
                right.clip.id = alloc();
            }
            out.push(right);
        }
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    *clips = out;
}

/// Copies of the parts of `clips` inside `from..to`, with fresh ids.
pub fn copy_range(
    clips: &[ArrangementClip],
    from: f64,
    to: f64,
    alloc: &mut impl FnMut() -> Id,
) -> Vec<ArrangementClip> {
    clips
        .iter()
        .filter_map(|a| slice(a, from, to))
        .map(|mut a| {
            a.id = alloc();
            a.clip.id = alloc();
            a
        })
        .collect()
}

/// Puts `pieces` into a lane, replacing whatever they cover.
pub fn paste(clips: &mut Vec<ArrangementClip>, pieces: Vec<ArrangementClip>, alloc: &mut impl FnMut() -> Id) {
    for p in &pieces {
        cut_range(clips, p.start, p.end(), alloc);
    }
    clips.extend(pieces);
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
}

/// Splits the clip of `track` that crosses `beat` in two. Returns the id of
/// the new right-hand clip.
pub fn split_at(p: &mut Project, track: Id, beat: f64) -> Option<Id> {
    let ti = p.track_index(track)?;
    let a = p.tracks[ti]
        .arrangement
        .iter()
        .find(|a| a.start + MIN_CLIP < beat && beat < a.end() - MIN_CLIP)?
        .clone();
    let left = slice(&a, a.start, beat)?;
    let mut right = slice(&a, beat, a.end())?;
    right.id = p.alloc_id();
    right.clip.id = p.alloc_id();
    let new_id = right.id;
    let lane = &mut p.tracks[ti].arrangement;
    if let Some(slot) = lane.iter_mut().find(|x| x.id == a.id) {
        *slot = left;
    }
    lane.push(right);
    lane.sort_by(|a, b| a.start.total_cmp(&b.start));
    Some(new_id)
}

/// The notes a MIDI clip plays between song beats `from` and `to`, in song
/// beats (loops unrolled, notes cut at the range and clip ends).
pub fn played_notes(a: &ArrangementClip, from: f64, to: f64) -> Vec<Note> {
    let ClipContent::Midi { notes } = &a.clip.content else {
        return Vec::new();
    };
    let region = a.clip.region();
    let s = from.max(a.start);
    let e = to.min(a.end());
    let mut out = Vec::new();
    let mut t = 0.0;
    // Walk the clip's runs from its start (a run ends where the loop wraps).
    while a.start + t < e - EPS {
        let Some(pos) = region.position(t) else { break };
        let run = (region.end - pos).max(MIN_CLIP);
        let run_start = a.start + t;
        for n in notes {
            if n.start < pos - EPS || n.start >= region.end - EPS {
                continue;
            }
            let on = run_start + (n.start - pos);
            if on >= run_start + run - EPS {
                continue;
            }
            let off = (on + n.length).min(run_start + run).min(e);
            if on < s - EPS || on >= e - EPS || off <= on {
                continue;
            }
            out.push(Note {
                start: on,
                length: off - on,
                ..*n
            });
        }
        t += run;
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    out
}

/// Consolidates the MIDI clips of a track between `from` and `to` into one
/// new clip (Ctrl+J). Returns its id, or `None` if there are no MIDI clips
/// there (audio is consolidated by rendering, see the app).
pub fn consolidate_midi(p: &mut Project, track: Id, from: f64, to: f64) -> Option<Id> {
    let ti = p.track_index(track)?;
    let lane = &p.tracks[ti].arrangement;
    let inside: Vec<&ArrangementClip> = lane
        .iter()
        .filter(|a| a.clip.is_midi() && a.end() > from + EPS && a.start < to - EPS)
        .collect();
    let first = inside.first()?;
    let (name, color) = (first.clip.name.clone(), first.clip.color);
    let mut notes: Vec<Note> = inside.iter().flat_map(|a| played_notes(a, from, to)).collect();
    for n in notes.iter_mut() {
        n.start -= from;
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start));
    let id = p.alloc_id();
    let clip_id = p.alloc_id();
    let clip = Clip {
        id: clip_id,
        name,
        color,
        length: to - from,
        looping: true,
        content: ClipContent::Midi { notes },
        ..Default::default()
    };
    let mut alloc_ids: Vec<Id> = (0..4).map(|_| p.alloc_id()).collect();
    let mut alloc = || alloc_ids.pop().unwrap_or(0);
    let lane = &mut p.tracks[ti].arrangement;
    cut_range(lane, from, to, &mut alloc);
    lane.push(ArrangementClip {
        id,
        start: from,
        length: to - from,
        clip,
        ..Default::default()
    });
    lane.sort_by(|a, b| a.start.total_cmp(&b.start));
    Some(id)
}

/// Reverses a clip's content within its length: MIDI notes are mirrored,
/// audio is returned reversed (the caller saves it and points the clip at
/// the new file); warp markers and transients are mirrored with it.
pub fn reverse(clip: &mut Clip) -> Option<AudioData> {
    let len = clip.length;
    clip.start_marker = 0.0;
    clip.loop_start = 0.0;
    match &mut clip.content {
        ClipContent::Midi { notes } => {
            for n in notes.iter_mut() {
                let end = (n.start + n.length).min(len);
                n.start = (len - end).max(0.0);
            }
            notes.sort_by(|a, b| a.start.total_cmp(&b.start));
            None
        }
        ClipContent::Audio(a) => {
            let data = a.data.as_ref()?;
            let dur = data.duration_secs();
            let mut rev = AudioData::clone(data);
            rev.left.reverse();
            rev.right.reverse();
            let total = a.warp.secs_to_beat(dur);
            for m in a.warp.markers.iter_mut() {
                m.beat = total - m.beat;
                m.secs = dur - m.secs;
            }
            a.warp.markers.reverse();
            for t in a.warp.transients.iter_mut() {
                *t = dur - *t;
            }
            a.warp.transients.reverse();
            // The audio that played at the clip's start now sits at the
            // file's end; pin the clip start back to it.
            let start_secs = a.warp.beat_to_secs(total - len.min(total));
            if a.warp.markers.is_empty() && start_secs > 1e-6 {
                a.warp.markers.push(crate::warp::WarpMarker {
                    beat: 0.0,
                    secs: start_secs,
                });
                a.warp.markers.push(crate::warp::WarpMarker { beat: len, secs: dur });
            }
            a.data = Some(Arc::new(rev.clone()));
            Some(rev)
        }
    }
}

/// Creates a crossfade of `length` beats where clip `left` ends and the next
/// clip on the track starts: both clips are extended into each other by
/// half the length (when their content allows) and faded with equal-power
/// curves.
pub fn crossfade(p: &mut Project, track: Id, left: Id, length: f64) -> bool {
    let Some(ti) = p.track_index(track) else {
        return false;
    };
    let lane = &mut p.tracks[ti].arrangement;
    let Some(li) = lane.iter().position(|a| a.id == left) else {
        return false;
    };
    let boundary = lane[li].end();
    let Some(ri) = lane
        .iter()
        .position(|a| a.id != left && (a.start - boundary).abs() < 1e-3)
    else {
        return false;
    };
    let h = (length / 2.0).max(MIN_CLIP);
    // Extend the right clip backwards if its start marker leaves room.
    let r = &mut lane[ri];
    let back = h.min(r.clip.start_marker);
    if back > 0.0 {
        r.start -= back;
        r.length += back;
        r.clip.start_marker -= back;
    }
    r.fade_in = (back + h).min(r.length);
    // Extend the left clip forwards: looping content continues, and a
    // non-looping clip grows into the rest of its material.
    let l = &mut lane[li];
    let fwd = if l.clip.looping {
        h
    } else {
        h.min(l.clip.length - l.clip.region().position(l.length - MIN_CLIP).unwrap_or(l.clip.length))
    };
    let fwd = fwd.max(0.0);
    if fwd > 0.0 && !l.clip.looping {
        l.clip.length += fwd;
    }
    l.length += fwd;
    l.fade_out = (fwd + h).min(l.length);
    lane.sort_by(|a, b| a.start.total_cmp(&b.start));
    true
}

/// Adds a recorded clip as a new take lane of the track.
pub fn add_take(p: &mut Project, track: Id, clip: ArrangementClip) -> Option<Id> {
    let id = p.alloc_id();
    let t = p.track_mut(track)?;
    let n = t.takes.len() + 1;
    t.takes.push(TakeLane {
        id,
        name: format!("Take {n}"),
        clips: vec![clip],
    });
    Some(id)
}

/// Places an Arrangement recording: it replaces what it covers on the main
/// lane (or, for MIDI with `overdub`, is merged with the notes there) and
/// is also kept as a new take lane for comping. Returns the main-lane id.
pub fn record_take(p: &mut Project, track: Id, mut rec: ArrangementClip, overdub: bool) -> Option<Id> {
    let ti = p.track_index(track)?;
    let (from, to) = (rec.start, rec.end());
    let mut take = rec.clone();
    take.id = p.alloc_id();
    take.clip.id = p.alloc_id();
    if overdub {
        if let ClipContent::Midi { notes } = &mut rec.clip.content {
            for a in p.tracks[ti].arrangement.iter().filter(|a| a.clip.is_midi()) {
                notes.extend(played_notes(a, from, to).into_iter().map(|n| Note {
                    start: n.start - from,
                    ..n
                }));
            }
            notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
        }
    }
    let id = rec.id;
    let mut ids: Vec<Id> = (0..8).map(|_| p.alloc_id()).collect();
    let mut alloc = || ids.pop().unwrap_or(0);
    paste(&mut p.tracks[ti].arrangement, vec![rec], &mut alloc);
    add_take(p, track, take);
    Some(id)
}

/// Comping: copies song beats `from..to` of a take lane onto the track's
/// main lane, replacing what was there.
pub fn comp(p: &mut Project, track: Id, take: Id, from: f64, to: f64) -> bool {
    let Some(ti) = p.track_index(track) else {
        return false;
    };
    let Some(lane) = p.tracks[ti].takes.iter().find(|l| l.id == take).cloned() else {
        return false;
    };
    let mut ids: Vec<Id> = (0..(lane.clips.len() * 2 + 8)).map(|_| p.alloc_id()).collect();
    let mut alloc = || ids.pop().unwrap_or(0);
    let pieces = copy_range(&lane.clips, from, to, &mut alloc);
    if pieces.is_empty() {
        return false;
    }
    paste(&mut p.tracks[ti].arrangement, pieces, &mut alloc);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_takes_replace_or_overdub_and_keep_a_take_lane() {
        let mut p = Project::default();
        let t = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        let mk = |p: &mut Project, start: f64, len: f64, notes: Vec<Note>| ArrangementClip {
            id: p.alloc_id(),
            start,
            length: len,
            clip: Clip {
                id: p.alloc_id(),
                length: len,
                looping: false,
                content: ClipContent::Midi { notes },
                ..Default::default()
            },
            ..Default::default()
        };
        let base = mk(&mut p, 0.0, 8.0, vec![note(60, 1.0, 0.5), note(62, 5.0, 0.5)]);
        p.track_mut(t).unwrap().arrangement.push(base);
        // Replace: beats 4..8 lose the old note, 0..4 keep theirs.
        let rec = mk(&mut p, 4.0, 4.0, vec![note(70, 0.5, 0.5)]);
        record_take(&mut p, t, rec, false).unwrap();
        let tr = p.track(t).unwrap();
        let all: Vec<Note> = tr.arrangement.iter().flat_map(|a| played_notes(a, 0.0, 8.0)).collect();
        let pitches: Vec<u8> = all.iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, vec![60, 70]);
        assert_eq!(tr.takes.len(), 1);
        // Overdub keeps what's there.
        let rec = mk(&mut p, 0.0, 4.0, vec![note(72, 2.0, 0.5)]);
        record_take(&mut p, t, rec, true).unwrap();
        let tr = p.track(t).unwrap();
        let mut all: Vec<Note> = tr.arrangement.iter().flat_map(|a| played_notes(a, 0.0, 8.0)).collect();
        all.sort_by(|a, b| a.start.total_cmp(&b.start));
        let pitches: Vec<u8> = all.iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, vec![60, 72, 70]);
        assert_eq!(tr.takes.len(), 2);
    }
    use crate::dsp::synth::SynthParams;
    use crate::model::{AudioRef, Instrument, TrackKind};

    fn note(pitch: u8, start: f64, length: f64) -> Note {
        Note {
            pitch,
            velocity: 100,
            start,
            length,
        }
    }

    fn midi_project(notes: Vec<Note>, clip_len: f64, arr_len: f64) -> (Project, Id) {
        let mut p = Project::default();
        let t = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        let id = p.alloc_id();
        let cid = p.alloc_id();
        p.track_mut(t).unwrap().arrangement.push(ArrangementClip {
            id,
            start: 4.0,
            length: arr_len,
            clip: Clip {
                id: cid,
                length: clip_len,
                looping: true,
                content: ClipContent::Midi { notes },
                ..Default::default()
            },
            ..Default::default()
        });
        (p, t)
    }

    #[test]
    fn split_keeps_what_plays() {
        let (mut p, t) = midi_project(vec![note(60, 0.0, 1.0), note(62, 2.0, 1.0)], 4.0, 8.0);
        let before = played_notes(&p.tracks[0].arrangement[0], 0.0, 100.0);
        let right = split_at(&mut p, t, 7.0).unwrap();
        let lane = &p.tracks[0].arrangement;
        assert_eq!(lane.len(), 2);
        assert_eq!(lane[0].length, 3.0);
        assert_eq!(lane[1].id, right);
        assert_eq!(lane[1].start, 7.0);
        assert_eq!(lane[1].clip.start_marker, 3.0);
        let after: Vec<Note> = lane.iter().flat_map(|a| played_notes(a, 0.0, 100.0)).collect();
        assert_eq!(before, after);
        // Splitting at an edge does nothing.
        assert!(split_at(&mut p, t, 4.0).is_none());
    }

    #[test]
    fn played_notes_unroll_loops() {
        let (p, _) = midi_project(vec![note(60, 0.0, 0.5), note(64, 1.5, 1.0)], 2.0, 5.0);
        let n = played_notes(&p.tracks[0].arrangement[0], 0.0, 100.0);
        let starts: Vec<f64> = n.iter().map(|n| n.start).collect();
        assert_eq!(starts, vec![4.0, 5.5, 6.0, 7.5, 8.0]);
        // The note at 7.5 is cut at the loop end (8.0).
        assert_eq!(n[3].length, 0.5);
    }

    #[test]
    fn consolidate_merges_clips_into_one() {
        let (mut p, t) = midi_project(vec![note(60, 0.0, 1.0)], 2.0, 4.0);
        let id = p.alloc_id();
        let cid = p.alloc_id();
        p.tracks[0].arrangement.push(ArrangementClip {
            id,
            start: 10.0,
            length: 2.0,
            clip: Clip {
                id: cid,
                length: 2.0,
                content: ClipContent::Midi {
                    notes: vec![note(67, 0.5, 0.5)],
                },
                ..Default::default()
            },
            ..Default::default()
        });
        let new = consolidate_midi(&mut p, t, 4.0, 12.0).unwrap();
        let lane = &p.tracks[0].arrangement;
        assert_eq!(lane.len(), 1);
        assert_eq!(lane[0].id, new);
        assert_eq!(lane[0].length, 8.0);
        let starts: Vec<(u8, f64)> = lane[0].clip.notes().iter().map(|n| (n.pitch, n.start)).collect();
        assert_eq!(starts, vec![(60, 0.0), (60, 2.0), (67, 6.5)]);
    }

    #[test]
    fn cut_and_paste_ranges() {
        let (p, _) = midi_project(vec![note(60, 0.0, 1.0)], 4.0, 8.0);
        let mut lane = p.tracks[0].arrangement.clone();
        let mut next = 1000;
        let mut alloc = || {
            next += 1;
            next
        };
        cut_range(&mut lane, 6.0, 8.0, &mut alloc);
        assert_eq!(lane.len(), 2);
        assert_eq!((lane[0].start, lane[0].length), (4.0, 2.0));
        assert_eq!(
            (lane[1].start, lane[1].length, lane[1].clip.start_marker),
            (8.0, 4.0, 0.0)
        );
        let pieces = copy_range(&p.tracks[0].arrangement, 6.0, 7.0, &mut alloc);
        assert_eq!(pieces[0].clip.start_marker, 2.0);
        paste(&mut lane, pieces, &mut alloc);
        assert_eq!(lane.len(), 3);
        assert_eq!(lane[1].start, 6.0);
    }

    #[test]
    fn reverse_mirrors_notes_and_audio() {
        let mut c = Clip {
            length: 4.0,
            content: ClipContent::Midi {
                notes: vec![note(60, 0.0, 1.0), note(62, 3.0, 0.5)],
            },
            ..Default::default()
        };
        assert!(reverse(&mut c).is_none());
        let n: Vec<(u8, f64)> = c.notes().iter().map(|n| (n.pitch, n.start)).collect();
        assert_eq!(n, vec![(62, 0.5), (60, 3.0)]);

        let data = Arc::new(AudioData {
            sample_rate: 4,
            left: vec![1.0, 2.0, 3.0, 4.0],
            right: vec![1.0, 2.0, 3.0, 4.0],
        });
        let mut c = Clip {
            length: 2.0,
            content: ClipContent::Audio(AudioRef::new("a.wav", Some(data))),
            ..Default::default()
        };
        let rev = reverse(&mut c).unwrap();
        assert_eq!(rev.left, vec![4.0, 3.0, 2.0, 1.0]);
    }

    #[test]
    fn crossfades_overlap_and_fade_both_clips() {
        let mut p = Project::default();
        let t = p.add_track(TrackKind::Audio, Instrument::None);
        let data = Arc::new(AudioData {
            sample_rate: 100,
            left: vec![0.5; 1000],
            right: vec![0.5; 1000],
        });
        for (k, start) in [0.0, 4.0].into_iter().enumerate() {
            let id = p.alloc_id();
            let mut clip = Clip {
                length: 4.0,
                looping: true,
                content: ClipContent::Audio(AudioRef::new("a.wav", Some(data.clone()))),
                ..Default::default()
            };
            if k == 1 {
                clip.start_marker = 1.0;
            }
            p.track_mut(t).unwrap().arrangement.push(ArrangementClip {
                id,
                start,
                length: 4.0,
                clip,
                ..Default::default()
            });
        }
        let left = p.tracks[0].arrangement[0].id;
        assert!(crossfade(&mut p, t, left, 1.0));
        let lane = &p.tracks[0].arrangement;
        assert_eq!(lane[0].end(), 4.5);
        assert_eq!(lane[0].fade_out, 1.0);
        assert_eq!(lane[1].start, 3.5);
        assert_eq!(lane[1].fade_in, 1.0);
        assert_eq!(lane[1].clip.start_marker, 0.5);
    }

    #[test]
    fn comping_copies_a_take_region() {
        let (mut p, t) = midi_project(vec![note(60, 0.0, 1.0)], 4.0, 8.0);
        let take_clip = ArrangementClip {
            id: 900,
            start: 4.0,
            length: 8.0,
            clip: Clip {
                id: 901,
                length: 8.0,
                content: ClipContent::Midi {
                    notes: vec![note(72, 2.0, 1.0)],
                },
                ..Default::default()
            },
            ..Default::default()
        };
        let take = add_take(&mut p, t, take_clip).unwrap();
        assert!(comp(&mut p, t, take, 5.0, 7.0));
        let lane = &p.tracks[0].arrangement;
        assert_eq!(lane.len(), 3);
        let notes: Vec<(u8, f64)> = lane
            .iter()
            .flat_map(|a| played_notes(a, 0.0, 100.0))
            .map(|n| (n.pitch, n.start))
            .collect();
        assert_eq!(notes, vec![(60, 4.0), (72, 6.0), (60, 8.0)]);
    }
}
