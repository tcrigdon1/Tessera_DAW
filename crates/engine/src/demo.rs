//! A small built-in demo song so a fresh install makes sound immediately.

use crate::dsp::drums::{self, DrumKitParams};
use crate::dsp::synth::SynthParams;
use crate::fx::{Device, DeviceKind, DeviceParams};
use crate::model::{ArrangementClip, Clip, ClipContent, Instrument, Note, Project, TrackKind};

fn note(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
    Note {
        pitch,
        velocity,
        start,
        length,
    }
}

fn midi_clip(p: &mut Project, name: &str, color: [u8; 3], length: f64, notes: Vec<Note>) -> Clip {
    Clip {
        id: p.alloc_id(),
        name: name.into(),
        color,
        length,
        looping: true,
        gain_db: 0.0,
        content: ClipContent::Midi { notes },
    }
}

fn beat_basic() -> Vec<Note> {
    let mut n = Vec::new();
    for bar in 0..2 {
        let b = bar as f64 * 4.0;
        for k in [0.0, 1.0, 2.0, 3.0] {
            n.push(note(drums::KICK, b + k, 0.25, 118));
        }
        n.push(note(drums::SNARE, b + 1.0, 0.25, 100));
        n.push(note(drums::SNARE, b + 3.0, 0.25, 104));
        for i in 0..8 {
            let v = if i % 2 == 0 { 70 } else { 96 };
            n.push(note(drums::CLOSED_HAT, b + i as f64 * 0.5, 0.25, v));
        }
    }
    n
}

fn beat_busy() -> Vec<Note> {
    let mut n = beat_basic();
    n.retain(|x| x.pitch != drums::CLOSED_HAT);
    for bar in 0..2 {
        let b = bar as f64 * 4.0;
        for i in 0..16 {
            let v = [90, 50, 70, 50][i % 4];
            n.push(note(drums::CLOSED_HAT, b + i as f64 * 0.25, 0.125, v));
        }
        n.push(note(drums::OPEN_HAT, b + 3.5, 0.5, 80));
        n.push(note(drums::CLAP, b + 3.0, 0.25, 90));
    }
    n.push(note(drums::KICK, 6.5, 0.25, 90));
    n
}

fn bass_line(root: u8) -> Vec<Note> {
    let pattern = [(0.0, 0), (0.75, 0), (1.5, 12), (2.0, 0), (2.75, 7), (3.5, 10)];
    let mut n = Vec::new();
    for (bar, transpose) in [(0usize, 0i8), (1, -2)] {
        for (t, iv) in pattern {
            let pitch = (root as i8 + transpose + iv as i8) as u8;
            n.push(note(pitch, bar as f64 * 4.0 + t, 0.4, 105));
        }
    }
    n
}

fn chords(voicings: &[[u8; 3]]) -> Vec<Note> {
    let mut n = Vec::new();
    for (bar, v) in voicings.iter().enumerate() {
        for &p in v {
            n.push(note(p, bar as f64 * 4.0, 3.75, 80));
        }
    }
    n
}

fn lead() -> Vec<Note> {
    let line = [
        (0.0, 72, 0.5),
        (0.5, 75, 0.5),
        (1.0, 79, 1.0),
        (2.5, 77, 0.5),
        (3.0, 75, 1.0),
        (4.0, 74, 0.5),
        (4.5, 75, 0.5),
        (5.0, 77, 1.5),
        (6.5, 74, 0.5),
        (7.0, 70, 1.0),
    ];
    line.iter().map(|&(s, p, l)| note(p, s, l, 96)).collect()
}

/// Four tracks, four scenes and a short arrangement.
pub fn demo_project() -> Project {
    let mut p = Project {
        name: "Demo Song".into(),
        tempo: 122.0,
        ..Project::default()
    };
    let drums_id = p.add_track(TrackKind::Midi, Instrument::DrumKit(DrumKitParams::default()));
    let bass_id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::bass()));
    let keys_id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::pad()));
    let lead_id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let names = ["Drums", "Bass", "Pad", "Lead"];
    for (i, id) in [drums_id, bass_id, keys_id, lead_id].into_iter().enumerate() {
        p.track_mut(id).unwrap().name = names[i].into();
    }
    // Leave headroom on the master.
    for (id, db) in [(drums_id, -4.0), (bass_id, -5.0), (keys_id, -4.0), (lead_id, -8.0)] {
        p.track_mut(id).unwrap().mixer.volume_db = db;
    }
    // Effects: glue the drums, warm the bass, send pad and lead to the returns
    // (A = reverb, B = delay).
    let fx = |p: &mut Project, id, kind: DeviceKind, edit: &dyn Fn(&mut DeviceParams)| {
        let dev = p.alloc_id();
        let mut d = Device::new(dev, kind);
        edit(&mut d.params);
        p.track_mut(id).unwrap().devices.push(d);
    };
    fx(&mut p, drums_id, DeviceKind::Compressor, &|d| {
        if let DeviceParams::Compressor(c) = d {
            c.threshold = -14.0;
            c.ratio = 3.0;
            c.attack = 10.0;
            c.makeup = 2.0;
        }
    });
    fx(&mut p, bass_id, DeviceKind::Saturator, &|d| {
        if let DeviceParams::Saturator(s) = d {
            s.drive = 4.0;
            s.curve = 2.0;
            s.output = -4.0;
        }
    });
    fx(&mut p, lead_id, DeviceKind::Chorus, &|d| {
        if let DeviceParams::Chorus(c) = d {
            c.mix = 0.35;
        }
    });
    for (id, sends) in [
        (keys_id, [-9.0, -70.0]),
        (lead_id, [-12.0, -10.0]),
        (drums_id, [-22.0, -70.0]),
    ] {
        p.track_mut(id).unwrap().sends = sends.to_vec();
    }
    let scene_names = ["Intro", "Groove", "Build", "Drop"];
    for (i, n) in scene_names.iter().enumerate() {
        p.scenes[i].name = (*n).into();
    }

    let c_drum = p.track(drums_id).unwrap().color;
    let c_bass = p.track(bass_id).unwrap().color;
    let c_keys = p.track(keys_id).unwrap().color;
    let c_lead = p.track(lead_id).unwrap().color;

    let d1 = midi_clip(&mut p, "Beat", c_drum, 8.0, beat_basic());
    let d2 = midi_clip(&mut p, "Beat Busy", c_drum, 8.0, beat_busy());
    let b1 = midi_clip(&mut p, "Bass C", c_bass, 8.0, bass_line(36));
    let k1 = midi_clip(&mut p, "Chords", c_keys, 8.0, chords(&[[60, 63, 67], [58, 62, 65]]));
    let l1 = midi_clip(&mut p, "Lead", c_lead, 8.0, lead());

    let set = |p: &mut Project, id, scene: usize, c: &Clip| {
        let mut c = c.clone();
        c.id = p.alloc_id();
        p.track_mut(id).unwrap().slots[scene] = Some(c);
    };
    set(&mut p, keys_id, 0, &k1);
    set(&mut p, drums_id, 1, &d1);
    set(&mut p, bass_id, 1, &b1);
    set(&mut p, keys_id, 1, &k1);
    set(&mut p, drums_id, 2, &d2);
    set(&mut p, keys_id, 2, &k1);
    set(&mut p, lead_id, 2, &l1);
    set(&mut p, drums_id, 3, &d2);
    set(&mut p, bass_id, 3, &b1);
    set(&mut p, keys_id, 3, &k1);
    set(&mut p, lead_id, 3, &l1);

    // Arrangement: 4 bars intro, 8 bars groove, 8 bars with lead.
    let place = |p: &mut Project, id, start: f64, length: f64, c: &Clip| {
        let mut c = c.clone();
        c.id = p.alloc_id();
        let aid = p.alloc_id();
        let t = p.track_mut(id).unwrap();
        t.arrangement.push(ArrangementClip {
            id: aid,
            start,
            length,
            clip: c,
        });
        t.sort_arrangement();
    };
    place(&mut p, keys_id, 0.0, 80.0, &k1);
    place(&mut p, drums_id, 16.0, 32.0, &d1);
    place(&mut p, drums_id, 48.0, 32.0, &d2);
    place(&mut p, bass_id, 16.0, 64.0, &b1);
    place(&mut p, lead_id, 48.0, 32.0, &l1);
    p.loop_start = 16.0;
    p.loop_length = 32.0;
    p
}
