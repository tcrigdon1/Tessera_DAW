//! Track-to-track routing: group tracks, "Audio To" another track and
//! "Audio From" another track.
//!
//! The render order is worked out on the UI thread when a snapshot is
//! compiled, so every track renders after the tracks that feed it. The audio
//! thread only follows the order and the precomputed indices.

use crate::model::{InputSource, Project, TrackKind};
use crate::MAX_TRACKS;

/// Routing of one track, by track index in the snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Route {
    /// Track whose input this track's output feeds (a group), or `None`
    /// for the master.
    pub output: Option<usize>,
    /// Track whose output this track listens to ("Audio From").
    pub source: Option<usize>,
    /// Another track listens to this one, so its output is kept.
    pub tapped: bool,
}

/// Works out each track's routing and an order in which every track renders
/// after its inputs. Routes that would form a loop are dropped (the track
/// goes to the master, or listens to the audio interface).
pub fn plan(p: &Project) -> (Vec<Route>, Vec<usize>) {
    let tracks = &p.tracks[..p.tracks.len().min(MAX_TRACKS)];
    let index = |id| tracks.iter().position(|t| t.id == id);
    let mut routes: Vec<Route> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| Route {
            output: t
                .output
                .and_then(index)
                .filter(|&o| o != i && tracks[o].kind == TrackKind::Group),
            source: match t.input.source {
                InputSource::Track(id) if t.kind == TrackKind::Audio => index(id).filter(|&s| s != i),
                _ => None,
            },
            tapped: false,
        })
        .collect();

    // Depth-first topological order over "renders before" edges:
    // a member before its group, a source before its listener.
    let n = routes.len();
    let mut order = Vec::with_capacity(n);
    // 0 = unvisited, 1 = on the stack, 2 = done.
    let mut state = vec![0u8; n];
    fn visit(i: usize, routes: &mut [Route], state: &mut [u8], order: &mut Vec<usize>) {
        if state[i] != 0 {
            return;
        }
        state[i] = 1;
        // Inputs of `i`: its members and its source.
        let members: Vec<usize> = (0..routes.len()).filter(|&j| routes[j].output == Some(i)).collect();
        for j in members {
            if state[j] == 1 {
                // A loop: cut the member's route.
                routes[j].output = None;
            } else {
                visit(j, routes, state, order);
            }
        }
        if let Some(s) = routes[i].source {
            if state[s] == 1 {
                routes[i].source = None;
            } else {
                visit(s, routes, state, order);
            }
        }
        state[i] = 2;
        order.push(i);
    }
    for i in 0..n {
        visit(i, &mut routes, &mut state, &mut order);
    }
    // A cut route may leave a group member after its group; check and
    // send any such member to the master rather than render late.
    let pos: Vec<usize> = {
        let mut pos = vec![0; n];
        for (k, &i) in order.iter().enumerate() {
            pos[i] = k;
        }
        pos
    };
    for i in 0..n {
        if let Some(o) = routes[i].output {
            if pos[o] < pos[i] {
                routes[i].output = None;
            }
        }
        if let Some(s) = routes[i].source {
            if pos[s] > pos[i] {
                routes[i].source = None;
            }
        }
    }
    for i in 0..n {
        if let Some(s) = routes[i].source {
            routes[s].tapped = true;
        }
    }
    (routes, order)
}

/// Which tracks are heard when some tracks are soloed: a soloed track, the
/// groups it plays through, and the members of a soloed group.
/// Allocation-free.
pub fn audible_by_solo(solo: &[bool], outputs: &[Option<usize>], out: &mut [bool; MAX_TRACKS]) {
    let n = solo.len().min(MAX_TRACKS).min(outputs.len());
    out[..n].fill(false);
    for i in 0..n {
        // Soloed itself or through a group above it.
        let mut cur = Some(i);
        let mut depth = 0;
        while let Some(c) = cur {
            if solo[c] {
                out[i] = true;
                break;
            }
            cur = outputs[c];
            depth += 1;
            if depth > n {
                break;
            }
        }
        // A soloed track keeps the groups above it open.
        if solo[i] {
            let mut cur = outputs[i];
            let mut depth = 0;
            while let Some(c) = cur {
                out[c] = true;
                cur = outputs[c];
                depth += 1;
                if depth > n {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth::SynthParams;
    use crate::model::Instrument;

    #[test]
    fn members_render_before_their_group() {
        let mut p = Project::default();
        let g = p.add_track(TrackKind::Group, Instrument::None);
        let a = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        let b = p.add_track(TrackKind::Audio, Instrument::None);
        p.track_mut(a).unwrap().output = Some(g);
        p.track_mut(b).unwrap().output = Some(g);
        let (routes, order) = plan(&p);
        assert_eq!(routes[1].output, Some(0));
        let pos = |i| order.iter().position(|&x| x == i).unwrap();
        assert!(pos(1) < pos(0) && pos(2) < pos(0));
    }

    #[test]
    fn audio_from_renders_the_source_first_and_loops_are_cut() {
        let mut p = Project::default();
        let a = p.add_track(TrackKind::Audio, Instrument::None);
        let b = p.add_track(TrackKind::Audio, Instrument::None);
        p.track_mut(a).unwrap().input.source = InputSource::Track(b);
        let (routes, order) = plan(&p);
        assert_eq!(routes[0].source, Some(1));
        assert!(routes[1].tapped);
        assert_eq!(order, vec![1, 0]);
        // b listens to a too: one of the two routes is dropped.
        p.track_mut(b).unwrap().input.source = InputSource::Track(a);
        let (routes, _) = plan(&p);
        assert!(routes[0].source.is_none() || routes[1].source.is_none());
    }

    #[test]
    fn output_to_a_non_group_goes_to_master() {
        let mut p = Project::default();
        let a = p.add_track(TrackKind::Audio, Instrument::None);
        let b = p.add_track(TrackKind::Audio, Instrument::None);
        p.track_mut(a).unwrap().output = Some(b);
        let (routes, _) = plan(&p);
        assert_eq!(routes[0].output, None);
    }

    #[test]
    fn solo_opens_groups_and_members() {
        // 0 = group, 1 and 2 in it, 3 outside.
        let outputs = [None, Some(0), Some(0), None];
        let mut out = [false; MAX_TRACKS];
        audible_by_solo(&[false, true, false, false], &outputs, &mut out);
        assert_eq!(&out[..4], &[true, true, false, false]);
        audible_by_solo(&[true, false, false, false], &outputs, &mut out);
        assert_eq!(&out[..4], &[true, true, true, false]);
    }
}
