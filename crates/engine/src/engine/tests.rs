use super::*;
use crate::dsp::drums::DrumKitParams;
use crate::dsp::synth::SynthParams;
use crate::model::{ArrangementClip, Clip, ClipContent, Instrument, Note, Project, TrackKind};

const SR: f32 = 48000.0;

fn clip(p: &mut Project, length: f64, notes: Vec<Note>) -> Clip {
    Clip {
        id: p.alloc_id(),
        name: "c".into(),
        color: [0, 0, 0],
        length,
        looping: true,
        gain_db: 0.0,
        content: ClipContent::Midi { notes },
    }
}

fn held_note(len: f64) -> Vec<Note> {
    vec![Note {
        pitch: 60,
        velocity: 100,
        start: 0.0,
        length: len,
    }]
}

/// Project with one synth track holding a clip in scene 0.
fn one_track() -> (Project, Id) {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let c = clip(&mut p, 4.0, held_note(4.0));
    p.track_mut(id).unwrap().slots[0] = Some(c);
    (p, id)
}

fn setup(p: &Project) -> (Engine, EngineHandle) {
    let (mut e, mut h) = Engine::new(SR);
    h.load_project(p, true);
    let mut buf = vec![0.0; 2];
    e.process(&mut buf);
    (e, h)
}

/// Processes `frames` frames and returns the peak absolute sample.
fn run(e: &mut Engine, frames: usize) -> f32 {
    let mut buf = vec![0.0; frames * 2];
    let mut peak = 0.0f32;
    for chunk in buf.chunks_mut(1024) {
        e.process(chunk);
        peak = chunk.iter().fold(peak, |a, b| a.max(b.abs()));
    }
    peak
}

fn shared_slot(h: &EngineHandle) -> i32 {
    h.shared.tracks[0].playing_slot.load(Ordering::Relaxed)
}

#[test]
fn transport_advances_at_tempo() {
    let (p, _) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, 48000); // one second at 120 bpm = 2 beats
    assert!((e.song_beat() - 2.0).abs() < 1e-6, "beat {}", e.song_beat());
    h.send(Command::Stop);
    run(&mut e, 128);
    assert!(!e.is_playing());
    assert_eq!(e.song_beat(), 0.0, "stop returns to start marker");
}

#[test]
fn tempo_change_changes_speed() {
    let (p, _) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::SetTempo(60.0));
    h.send(Command::Play);
    run(&mut e, 48000);
    assert!((e.song_beat() - 1.0).abs() < 1e-6);
}

#[test]
fn clip_launch_is_quantized_to_the_bar() {
    let (p, id) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, 12000); // half a beat
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 1024);
    assert_eq!(shared_slot(&h), -1, "should wait for the next bar");
    assert_eq!(h.shared.tracks[0].queued_slot.load(Ordering::Relaxed), 0);
    // Bar 2 starts at beat 4 = 2 seconds.
    run(&mut e, 96000);
    assert_eq!(shared_slot(&h), 0);
    assert!(h.shared.tracks[0].session_active.load(Ordering::Relaxed));
}

#[test]
fn launching_while_stopped_starts_immediately() {
    let (p, id) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    let peak = run(&mut e, 4800);
    assert!(e.is_playing());
    assert_eq!(shared_slot(&h), 0);
    assert!(peak > 0.01, "clip should sound, peak {peak}");
}

#[test]
fn mute_and_solo() {
    let (mut p, id) = one_track();
    let other = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    assert!(run(&mut e, 4800) > 0.01);

    let mut m = p.track(id).unwrap().mixer;
    m.mute = true;
    h.send(Command::SetMixer { track: id, params: m });
    run(&mut e, 1024); // gain ramp
    assert!(run(&mut e, 4800) < 1e-6, "muted track is silent");

    m.mute = false;
    h.send(Command::SetMixer { track: id, params: m });
    let mut s = p.track(other).unwrap().mixer;
    s.solo = true;
    h.send(Command::SetMixer {
        track: other,
        params: s,
    });
    run(&mut e, 1024);
    assert!(run(&mut e, 4800) < 1e-6, "soloing another track silences this one");
}

#[test]
fn stop_track_is_quantized_and_stops() {
    let (p, id) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 4800);
    h.send(Command::StopTrack(id));
    run(&mut e, 1024);
    assert_eq!(shared_slot(&h), 0, "stop waits for the bar");
    run(&mut e, 96000);
    assert_eq!(shared_slot(&h), -1);
    // Synth release tail dies out.
    run(&mut e, 48000);
    assert!(run(&mut e, 4800) < 1e-4);
}

#[test]
fn scene_launch_stops_tracks_with_empty_slots() {
    let (mut p, id) = one_track();
    let c = clip(&mut p, 4.0, held_note(1.0));
    p.track_mut(id).unwrap().slots[1] = Some(c);
    let id2 = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let c2 = clip(&mut p, 4.0, held_note(1.0));
    p.track_mut(id2).unwrap().slots[0] = Some(c2);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchScene(0));
    run(&mut e, 1024);
    assert_eq!(h.shared.tracks[0].playing_slot.load(Ordering::Relaxed), 0);
    assert_eq!(h.shared.tracks[1].playing_slot.load(Ordering::Relaxed), 0);
    h.send(Command::LaunchScene(1));
    run(&mut e, 96000 + 1024);
    assert_eq!(h.shared.tracks[0].playing_slot.load(Ordering::Relaxed), 1);
    assert_eq!(h.shared.tracks[1].playing_slot.load(Ordering::Relaxed), -1);
}

#[test]
fn scene_tempo_is_applied() {
    let (mut p, _) = one_track();
    p.scenes[0].tempo = Some(90.0);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchScene(0));
    run(&mut e, 128);
    assert_eq!(e.tempo(), 90.0);
}

#[test]
fn loop_wraps_song_position() {
    let (p, _) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::SetLoop {
        enabled: true,
        start: 0.0,
        length: 4.0,
    });
    h.send(Command::Play);
    run(&mut e, 48000 * 3); // 6 beats
    assert!((e.song_beat() - 2.0).abs() < 0.01, "beat {}", e.song_beat());
}

#[test]
fn arrangement_clip_only_sounds_inside_its_range() {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::DrumKit(DrumKitParams::default()));
    let notes = (0..8)
        .map(|i| Note {
            pitch: 36,
            velocity: 120,
            start: i as f64 * 0.5,
            length: 0.25,
        })
        .collect();
    let c = clip(&mut p, 4.0, notes);
    let aid = p.alloc_id();
    p.track_mut(id).unwrap().arrangement.push(ArrangementClip {
        id: aid,
        start: 4.0,
        length: 4.0,
        clip: c,
    });
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    assert!(run(&mut e, 90000) < 1e-6, "silent before the clip");
    assert!(run(&mut e, 96000) > 0.05, "plays inside the clip");
    run(&mut e, 48000); // tails
    assert!(run(&mut e, 48000) < 1e-4, "silent after the clip");
}

#[test]
fn session_clip_overrides_arrangement_until_back_to_arrangement() {
    let (mut p, id) = one_track();
    let c = clip(&mut p, 4.0, held_note(4.0));
    let aid = p.alloc_id();
    p.track_mut(id).unwrap().arrangement.push(ArrangementClip {
        id: aid,
        start: 0.0,
        length: 64.0,
        clip: c,
    });
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 1024);
    h.send(Command::StopTrack(id));
    run(&mut e, 96000 + 48000 * 2);
    assert!(run(&mut e, 4800) < 1e-4, "stopped session clip keeps the track silent");
    h.send(Command::BackToArrangement);
    run(&mut e, 1024);
    assert!(!h.shared.tracks[0].session_active.load(Ordering::Relaxed));
    // Notes are not chased, so the arrangement is heard from the next note start.
    assert!(run(&mut e, 48000 * 3) > 0.01, "arrangement resumes");
}

#[test]
fn reloading_a_snapshot_keeps_the_playing_clip() {
    let (mut p, id) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 4800);
    p.track_mut(id).unwrap().name = "renamed".into();
    h.load_project(&p, false);
    run(&mut e, 1024);
    h.collect_garbage();
    assert_eq!(shared_slot(&h), 0);
    assert!(run(&mut e, 4800) > 0.01);
}

#[test]
fn non_looping_clip_stops_at_its_end() {
    let (mut p, id) = one_track();
    if let Some(c) = p.track_mut(id).unwrap().slots[0].as_mut() {
        c.looping = false;
    }
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 48000 * 3); // 6 beats > 4 beat clip
    assert_eq!(shared_slot(&h), -1);
}

#[test]
fn preview_notes_sound_without_transport() {
    let (p, id) = one_track();
    let (mut e, mut h) = setup(&p);
    h.send(Command::NoteOn {
        track: id,
        pitch: 64,
        velocity: 100,
    });
    assert!(run(&mut e, 4800) > 0.01);
    assert!(!e.is_playing());
}

#[test]
fn audio_clip_plays_samples() {
    use crate::model::{AudioData, AudioRef};
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Audio, Instrument::None);
    let data = AudioData {
        sample_rate: 48000,
        left: (0..48000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect(),
        right: (0..48000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect(),
    };
    let c = Clip {
        id: p.alloc_id(),
        name: "a".into(),
        color: [0; 3],
        length: 2.0,
        looping: true,
        gain_db: 0.0,
        content: ClipContent::Audio(AudioRef {
            path: "x.wav".into(),
            data: Some(Arc::new(data)),
        }),
    };
    p.track_mut(id).unwrap().slots[0] = Some(c);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    let peak = run(&mut e, 9600);
    assert!(peak > 0.4 && peak <= 0.51, "peak {peak}");
}

#[test]
fn demo_renders_offline() {
    let p = crate::demo::demo_project();
    let out = crate::render::render_arrangement(&p, 44100, 24.0);
    let peak = out.iter().fold(0.0f32, |a, b| a.max(b.abs()));
    assert!(peak > 0.05 && peak <= 1.0, "peak {peak}");
}
