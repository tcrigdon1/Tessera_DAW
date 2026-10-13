//! Clip markers, launch modes, follow actions and hardware MIDI input.

use super::*;
use crate::dsp::synth::SynthParams;
use crate::model::{FollowAction, FollowKind, LaunchMode, Note};

const SR: f32 = 48000.0;
/// Frames per beat at 120 BPM.
const BEAT: usize = 24000;

fn held(len: f64) -> Vec<Note> {
    vec![Note {
        pitch: 60,
        velocity: 100,
        start: 0.0,
        length: len,
    }]
}

fn clip(p: &mut Project, length: f64) -> Clip {
    Clip {
        id: p.alloc_id(),
        length,
        content: ClipContent::Midi { notes: held(length) },
        ..Default::default()
    }
}

/// A synth track with no global launch quantization.
fn project() -> (Project, Id) {
    let mut p = Project {
        launch_quantization: Quantization::None,
        ..Default::default()
    };
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    (p, id)
}

fn setup(p: &Project) -> (Engine, EngineHandle) {
    let (mut e, mut h) = Engine::new(SR);
    h.load_project(p, true);
    e.process(&mut [0.0; 2]);
    (e, h)
}

fn run(e: &mut Engine, frames: usize) -> f32 {
    let mut buf = vec![0.0; frames * 2];
    let mut peak = 0.0f32;
    for chunk in buf.chunks_mut(512) {
        e.process(chunk);
        peak = chunk.iter().fold(peak, |a, b| a.max(b.abs()));
    }
    peak
}

fn playing(h: &EngineHandle, track: usize) -> i32 {
    h.shared.tracks[track].playing_slot.load(Ordering::Relaxed)
}

fn position(h: &EngineHandle, track: usize) -> f64 {
    h.shared.tracks[track].clip_position.load()
}

#[test]
fn follow_actions_walk_through_the_group() {
    let (mut p, id) = project();
    for s in 0..3 {
        let mut c = clip(&mut p, 1.0);
        c.launch.follow = FollowAction {
            enabled: true,
            a: FollowKind::Next,
            ..Default::default()
        };
        p.track_mut(id).unwrap().slots[s] = Some(c);
    }
    // Slot 4 is outside the group (slot 3 is empty).
    let c = clip(&mut p, 1.0);
    p.track_mut(id).unwrap().slots[4] = Some(c);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, BEAT / 2);
    assert_eq!(playing(&h, 0), 0);
    run(&mut e, BEAT);
    assert_eq!(playing(&h, 0), 1);
    run(&mut e, BEAT);
    assert_eq!(playing(&h, 0), 2);
    run(&mut e, BEAT);
    assert_eq!(playing(&h, 0), 0, "Next wraps around inside the group");
}

#[test]
fn follow_action_stop_and_chance() {
    let (mut p, id) = project();
    let mut c = clip(&mut p, 2.0);
    c.launch.follow = FollowAction {
        enabled: true,
        a: FollowKind::Again,
        b: FollowKind::Stop,
        chance_a: 0.0,
        linked: false,
        time: 1.0,
    };
    p.track_mut(id).unwrap().slots[0] = Some(c);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, BEAT / 2);
    assert_eq!(playing(&h, 0), 0);
    run(&mut e, BEAT);
    assert_eq!(playing(&h, 0), -1, "chance 0 always takes action B");
}

#[test]
fn toggle_and_gate_launch_modes() {
    let (mut p, id) = project();
    let mut t = clip(&mut p, 4.0);
    t.launch.mode = LaunchMode::Toggle;
    let mut g = clip(&mut p, 4.0);
    g.launch.mode = LaunchMode::Gate;
    p.track_mut(id).unwrap().slots[0] = Some(t);
    p.track_mut(id).unwrap().slots[1] = Some(g);
    let (mut e, mut h) = setup(&p);

    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 1024);
    assert_eq!(playing(&h, 0), 0);
    // Releasing a Toggle clip does nothing; pressing it again stops it.
    h.send(Command::ReleaseClip { track: id, slot: 0 });
    run(&mut e, 1024);
    assert_eq!(playing(&h, 0), 0);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 1024);
    assert_eq!(playing(&h, 0), -1);

    h.send(Command::LaunchClip { track: id, slot: 1 });
    run(&mut e, 1024);
    assert_eq!(playing(&h, 0), 1);
    h.send(Command::ReleaseClip { track: id, slot: 1 });
    run(&mut e, 1024);
    assert_eq!(playing(&h, 0), -1, "Gate stops on release");
}

#[test]
fn repeat_restarts_while_held() {
    let (mut p, id) = project();
    let mut c = clip(&mut p, 4.0);
    c.launch.mode = LaunchMode::Repeat;
    c.launch.quantization = Some(Quantization::QuarterNote);
    p.track_mut(id).unwrap().slots[0] = Some(c);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, BEAT * 3 / 2);
    // Restarted at beat 1, so half a beat in.
    assert!((position(&h, 0) - 0.5).abs() < 0.05, "pos {}", position(&h, 0));
    h.send(Command::ReleaseClip { track: id, slot: 0 });
    run(&mut e, BEAT);
    assert_eq!(playing(&h, 0), -1);
}

#[test]
fn clip_launch_quantization_overrides_the_global_one() {
    let (mut p, id) = project();
    p.launch_quantization = Quantization::Bar;
    let mut c = clip(&mut p, 4.0);
    c.launch.quantization = Some(Quantization::QuarterNote);
    p.track_mut(id).unwrap().slots[0] = Some(c);
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, BEAT / 4);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, BEAT);
    assert_eq!(playing(&h, 0), 0, "starts on the next beat, not the next bar");
}

#[test]
fn legato_keeps_the_position() {
    let (mut p, id) = project();
    let a = clip(&mut p, 4.0);
    let mut b = clip(&mut p, 4.0);
    b.launch.legato = true;
    p.track_mut(id).unwrap().slots[0] = Some(a);
    p.track_mut(id).unwrap().slots[1] = Some(b);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, BEAT * 3 / 2);
    h.send(Command::LaunchClip { track: id, slot: 1 });
    run(&mut e, BEAT / 2);
    assert_eq!(playing(&h, 0), 1);
    assert!((position(&h, 0) - 2.0).abs() < 0.05, "pos {}", position(&h, 0));
}

#[test]
fn clips_play_through_start_and_loop_markers() {
    let (mut p, id) = project();
    let mut c = clip(&mut p, 4.0);
    c.start_marker = 1.0;
    c.loop_start = 2.0;
    p.track_mut(id).unwrap().slots[0] = Some(c);
    let mut once = clip(&mut p, 4.0);
    once.looping = false;
    once.start_marker = 3.0;
    p.track_mut(id).unwrap().slots[1] = Some(once);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, BEAT);
    assert!((position(&h, 0) - 2.0).abs() < 0.05);
    // 1 -> 4 takes 3 beats, then the loop 2..4: after 3 beats we are at 2.
    run(&mut e, BEAT * 2);
    assert!((position(&h, 0) - 2.0).abs() < 0.05, "pos {}", position(&h, 0));
    run(&mut e, BEAT);
    assert!((position(&h, 0) - 3.0).abs() < 0.05, "pos {}", position(&h, 0));

    h.send(Command::LaunchClip { track: id, slot: 1 });
    run(&mut e, BEAT / 2);
    assert_eq!(playing(&h, 0), 1);
    run(&mut e, BEAT);
    assert_eq!(playing(&h, 0), -1, "one beat from the start marker to the end");
}

#[test]
fn clip_region_positions() {
    let r = ClipRegion::new(1.0, 2.0, 4.0, true);
    assert_eq!(r.position(0.0), Some(1.0));
    assert_eq!(r.position(3.0), Some(2.0));
    assert_eq!(r.position(4.5), Some(3.5));
    let once = ClipRegion::new(1.0, 0.0, 4.0, false);
    assert_eq!(once.position(2.9), Some(3.9));
    assert_eq!(once.position(3.0), None);
    // Markers beyond the end are pulled inside.
    let bad = ClipRegion::new(9.0, -3.0, 4.0, true);
    assert!(bad.start < 4.0 && bad.loop_start == 0.0);
}

#[test]
fn hardware_midi_plays_the_selected_or_armed_track() {
    let (mut p, a) = project();
    let b = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let (mut e, mut h) = Engine::new(SR);
    let (mut tx, rx) = midi_ring();
    e.set_midi_input(rx);
    h.load_project(&p, true);
    run(&mut e, 512);
    h.send(Command::SetSelectedTrack(a));
    tx.push(MidiMessage::note_on(7, 0, 60, 100)).unwrap();
    run(&mut e, 2048);
    assert!(h.shared.tracks[0].meter_l.load() > 0.01, "selected track plays");
    assert!(h.shared.tracks[1].meter_l.load() < 1e-6);

    // Arming another track takes the input over and releases the old notes.
    p.track_mut(b).unwrap().mixer.arm = true;
    h.load_project(&p, false);
    tx.push(MidiMessage::note_on(7, 0, 64, 100)).unwrap();
    run(&mut e, 2048);
    assert!(h.shared.tracks[1].meter_l.load() > 0.01, "armed track plays");
    run(&mut e, 96000);
    assert!(h.shared.tracks[0].meter_l.load() < 1e-3, "old track released");
}

#[test]
fn sustain_pedal_holds_notes() {
    let (p, a) = project();
    let (mut e, mut h) = Engine::new(SR);
    let (mut tx, rx) = midi_ring();
    e.set_midi_input(rx);
    h.load_project(&p, true);
    h.send(Command::SetSelectedTrack(a));
    run(&mut e, 512);
    tx.push(MidiMessage::cc(1, 0, 64, 127)).unwrap();
    tx.push(MidiMessage::note_on(1, 0, 60, 100)).unwrap();
    tx.push(MidiMessage::note_off(1, 0, 60)).unwrap();
    run(&mut e, 24000);
    assert!(run(&mut e, 4800) > 0.01, "held by the pedal");
    tx.push(MidiMessage::cc(1, 0, 64, 0)).unwrap();
    run(&mut e, 48000);
    assert!(run(&mut e, 4800) < 1e-3, "released with the pedal");
}
