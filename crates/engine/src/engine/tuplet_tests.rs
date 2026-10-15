//! Tuplets and nested tuplets play on their exact sample.

use super::*;
use crate::model::{Clip, ClipContent, Instrument, Note, Project, TrackKind};
use crate::rhythm::{self, PolyLayer, SpanUnit, Tuplet, TupletGrid};

const SR: f64 = 48000.0;

/// Note-on frames (from the song start) of a looping clip played from
/// beat 0 for `beats`, in blocks of `block` frames.
fn note_on_frames(notes: Vec<Note>, length: f64, tempo: f64, beats: f64, block: usize) -> Vec<(u64, u8)> {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::None);
    let clip = Clip {
        length,
        content: ClipContent::Midi { notes },
        ..Default::default()
    };
    let track = p.track(id).unwrap().clone();
    let automation = TrackAutomation::new(&track);
    let ec = EngineClip::from_model(&clip, &automation, (&[], 1.0));
    let mut rt = TrackRuntime::new(&Instrument::None, SR as f32, automation, id);
    let bpf = tempo / 60.0 / SR;
    let total = (beats / bpf).round() as u64;
    let mut out = Vec::new();
    let mut done = 0u64;
    let mut local0 = 0.0;
    while done < total {
        let n = block.min((total - done) as usize);
        rt.events.clear();
        // Like the engine: the song position advances block by block.
        let local1 = local0 + n as f64 * bpf;
        render_clip(&mut rt, &ec, 0, n, (local0, local1), bpf, tempo, SR);
        local0 = local1;
        for e in &rt.events {
            if let EventKind::On(p, _) = e.kind {
                out.push((done + e.frame as u64, p));
            }
        }
        done += n as u64;
    }
    out
}

fn note(pitch: u8, start: f64) -> Note {
    Note {
        pitch,
        velocity: 100,
        start,
        length: 1.0 / 64.0,
    }
}

#[test]
fn nested_tuplets_land_on_their_exact_sample() {
    // Quintuplets inside eighth triplets (4/15 beat) and septuplet
    // sixteenths, at 120 BPM where a beat is 24 000 samples.
    let nested = TupletGrid::new(0.5, Tuplet::new(3, 2), Tuplet::new(5, 4));
    let sept = TupletGrid::new(0.25, Tuplet::new(7, 4), Tuplet::STRAIGHT);
    let mut notes: Vec<Note> = (0..15).map(|k| note(60, nested.point(k))).collect();
    notes.extend((0..28).map(|k| note(62, sept.point(k))));
    for block in [64, 333, 512] {
        let got = note_on_frames(notes.clone(), 4.0, 120.0, 16.0, block);
        // Four loops of the clip.
        assert_eq!(got.len(), 4 * notes.len(), "block {block}");
        for (frame, pitch) in got {
            let (num, den) = if pitch == 60 { (4u64, 15u64) } else { (1, 7) };
            // Exact position in samples: k · num/den beats · 24 000.
            let k = (frame as f64 / 24000.0 * den as f64 / num as f64).round() as u64;
            let exact = k as f64 * num as f64 * 24000.0 / den as f64;
            assert!(
                (frame as f64 - exact).abs() <= 0.5,
                "pitch {pitch} at {frame}, expected {exact} (block {block})"
            );
        }
    }
}

#[test]
fn polymeter_clip_cycles_against_the_bar() {
    // A 7/8 clip (3.5 beats) with a hit on its downbeat, over 7 bars of 4/4:
    // it comes back to a bar line only after 7 of its cycles (28 beats).
    let len = rhythm::bar_beats(7, 8, 4);
    let got = note_on_frames(vec![note(36, 0.0)], len, 120.0, 28.0, 512);
    assert_eq!(got.len(), 8);
    let on_bar: Vec<u64> = got.iter().map(|(f, _)| *f).filter(|f| f % (4 * 24000) == 0).collect();
    assert_eq!(on_bar, vec![0]);
    assert_eq!(got[7].0, 7 * 84000);
}

#[test]
fn generated_polyrhythm_plays_exactly() {
    let layers = [
        PolyLayer {
            pitch: 36,
            pulses: 5,
            across: 1,
            unit: SpanUnit::Bar,
            ..PolyLayer::default()
        },
        PolyLayer {
            pitch: 38,
            pulses: 7,
            across: 1,
            unit: SpanUnit::Bar,
            ..PolyLayer::default()
        },
    ];
    let notes = rhythm::generate(&layers, 4.0, 4.0);
    let got = note_on_frames(notes, 4.0, 97.0, 4.0, 256);
    let spb = 60.0 / 97.0 * SR;
    for (frame, pitch) in got {
        let pulses = if pitch == 36 { 5.0 } else { 7.0 };
        let step = 4.0 * spb / pulses;
        let exact = (frame as f64 / step).round() * step;
        assert!((frame as f64 - exact).abs() <= 0.5, "{pitch} at {frame}, exact {exact}");
    }
}
