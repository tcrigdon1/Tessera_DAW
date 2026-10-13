//! Engine tests for groups, routing, crossfader, cue, track delay, freeze,
//! fades, Song Tempo automation, signature changes and scene follow actions.

use super::*;
use crate::dsp::synth::SynthParams;
use crate::model::{
    ArrangementClip, AudioRef, Clip, ClipContent, FreezeData, InputSource, Instrument, Note, Project, Scene,
    SceneFollow, TrackKind,
};
use crate::timeline::{SignatureChange, TempoLane};

const SR: f32 = 48000.0;

/// One second of constant 0.5 audio.
fn dc(seconds: f64) -> Arc<AudioData> {
    let n = (seconds * SR as f64) as usize;
    Arc::new(AudioData {
        sample_rate: SR as u32,
        left: vec![0.5; n],
        right: vec![0.5; n],
    })
}

fn audio_clip(p: &mut Project, data: Arc<AudioData>, length: f64) -> Clip {
    Clip {
        id: p.alloc_id(),
        length,
        looping: true,
        content: ClipContent::Audio(AudioRef::new("dc.wav", Some(data))),
        ..Default::default()
    }
}

fn midi_clip(p: &mut Project, length: f64) -> Clip {
    Clip {
        id: p.alloc_id(),
        length,
        looping: true,
        content: ClipContent::Midi {
            notes: vec![Note {
                pitch: 60,
                velocity: 100,
                start: 0.0,
                length,
            }],
        },
        ..Default::default()
    }
}

/// An audio track playing constant 0.5 on the Arrangement for 16 beats.
fn dc_track(p: &mut Project) -> Id {
    let id = p.add_track(TrackKind::Audio, Instrument::None);
    let c = audio_clip(p, dc(10.0), 16.0);
    let aid = p.alloc_id();
    p.track_mut(id).unwrap().arrangement.push(ArrangementClip {
        id: aid,
        start: 0.0,
        length: 16.0,
        clip: c,
        ..Default::default()
    });
    id
}

fn setup(p: &Project) -> (Engine, EngineHandle) {
    // No master limiter, so levels can be checked exactly.
    let mut p = p.clone();
    p.master_devices.clear();
    let p = &p;
    let (mut e, mut h) = Engine::new(SR);
    h.load_project(p, true);
    let mut buf = vec![0.0; 2];
    e.process(&mut buf);
    (e, h)
}

/// Processes `frames` frames and returns (peak, last left sample).
fn run(e: &mut Engine, frames: usize) -> (f32, f32) {
    let mut buf = vec![0.0; frames * 2];
    let mut peak = 0.0f32;
    for chunk in buf.chunks_mut(512) {
        e.process(chunk);
        peak = chunk.iter().fold(peak, |a, b| a.max(b.abs()));
    }
    (peak, buf[buf.len() - 2])
}

#[test]
fn group_tracks_sum_their_members_through_their_mixer() {
    let mut p = Project::default();
    let g = p.add_track(TrackKind::Group, Instrument::None);
    let a = dc_track(&mut p);
    let b = dc_track(&mut p);
    for t in [a, b] {
        p.track_mut(t).unwrap().output = Some(g);
    }
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    let (_, v) = run(&mut e, 4800);
    assert!((v - 1.0).abs() < 1e-3, "two members of 0.5 sum to 1.0, got {v}");
    // The group fader at -6 dB scales both.
    let mut m = p.track(g).unwrap().mixer;
    m.volume_db = -6.0206;
    h.send(Command::SetMixer { track: g, params: m });
    let (_, v) = run(&mut e, 4800);
    assert!((v - 0.5).abs() < 1e-3, "{v}");
    // Muting the group silences the members.
    m.mute = true;
    h.send(Command::SetMixer { track: g, params: m });
    let (_, v) = run(&mut e, 4800);
    assert!(v.abs() < 1e-4, "{v}");
}

#[test]
fn soloing_a_member_keeps_its_group_audible() {
    let mut p = Project::default();
    let g = p.add_track(TrackKind::Group, Instrument::None);
    let a = dc_track(&mut p);
    let b = dc_track(&mut p);
    let c = dc_track(&mut p);
    p.track_mut(a).unwrap().output = Some(g);
    p.track_mut(b).unwrap().output = Some(g);
    let _ = c;
    p.track_mut(a).unwrap().mixer.solo = true;
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    let (_, v) = run(&mut e, 4800);
    assert!((v - 0.5).abs() < 1e-3, "only the soloed member plays, got {v}");
}

#[test]
fn audio_from_another_track_and_cue_mode() {
    let mut p = Project::default();
    let src = dc_track(&mut p);
    let rec = p.add_track(TrackKind::Audio, Instrument::None);
    {
        let t = p.track_mut(rec).unwrap();
        t.input.source = InputSource::Track(src);
        t.input.monitor = crate::model::Monitor::In;
    }
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    let (_, v) = run(&mut e, 4800);
    // The source plays to the master and the listening track plays it again.
    assert!((v - 1.0).abs() < 1e-3, "{v}");
    // Mute the source: its output (post-fader) is silent for the listener too.
    let mut m = p.track(src).unwrap().mixer;
    m.mute = true;
    h.send(Command::SetMixer { track: src, params: m });
    let (_, v) = run(&mut e, 4800);
    assert!(v.abs() < 1e-3, "{v}");
}

#[test]
fn cue_output_carries_cued_tracks_without_muting_others() {
    let mut p = Project::default();
    let a = dc_track(&mut p);
    let _b = dc_track(&mut p);
    p.track_mut(a).unwrap().mixer.solo = true;
    p.cue.cue_mode = true;
    let (mut e, mut h) = setup(&p);
    e.set_cue_output(true);
    h.send(Command::Play);
    let mut out = vec![0.0; 1024];
    let mut cue = vec![0.0; 1024];
    for _ in 0..10 {
        e.process_with_cue(&mut out, Some(&mut cue));
    }
    assert!(
        (out[1000] - 1.0).abs() < 1e-3,
        "both tracks on the master: {}",
        out[1000]
    );
    assert!((cue[1000] - 0.5).abs() < 1e-3, "cued track on the cue: {}", cue[1000]);
    h.send(Command::SetCue {
        mode: true,
        volume_db: -6.0206,
    });
    e.process_with_cue(&mut out, Some(&mut cue));
    assert!((cue[1000] - 0.25).abs() < 1e-3, "{}", cue[1000]);
}

#[test]
fn crossfader_fades_a_and_b() {
    let mut p = Project::default();
    let a = dc_track(&mut p);
    let b = dc_track(&mut p);
    p.track_mut(a).unwrap().crossfade = CrossfadeAssign::A;
    p.track_mut(b).unwrap().crossfade = CrossfadeAssign::B;
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    let (_, v) = run(&mut e, 4800);
    assert!((v - 1.0).abs() < 1e-3, "centre plays both: {v}");
    h.send(Command::SetCrossfader(-1.0));
    run(&mut e, 512);
    let (_, v) = run(&mut e, 4800);
    assert!((v - 0.5).abs() < 1e-3, "full A: {v}");
    h.send(Command::SetCrossfader(1.0));
    run(&mut e, 512);
    let (_, v) = run(&mut e, 4800);
    assert!((v - 0.5).abs() < 1e-3, "full B: {v}");
}

#[test]
fn track_delay_shifts_playback() {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Audio, Instrument::None);
    let c = audio_clip(&mut p, dc(10.0), 4.0);
    let aid = p.alloc_id();
    // Clip from beat 1 (0.5 s at 120 BPM).
    p.track_mut(id).unwrap().arrangement.push(ArrangementClip {
        id: aid,
        start: 1.0,
        length: 4.0,
        clip: c,
        ..Default::default()
    });
    p.track_mut(id).unwrap().delay_ms = 100.0;
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    // Up to 0.55 s: still silent (0.5 s + 100 ms delay).
    let (peak, _) = run(&mut e, (0.58 * SR as f64) as usize);
    assert!(peak < 1e-4, "delayed clip started early: {peak}");
    let (peak, _) = run(&mut e, 4800);
    assert!(peak > 0.4, "{peak}");
}

#[test]
fn frozen_tracks_play_their_rendered_audio() {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let c = midi_clip(&mut p, 4.0);
    p.track_mut(id).unwrap().slots[0] = Some(c);
    let t = p.track_mut(id).unwrap();
    t.frozen = true;
    t.freeze = Some(Arc::new(FreezeData {
        arrangement: Some(dc(10.0)),
        arrangement_beats: 8.0,
        slots: vec![Some((dc(10.0), 4.0))],
        tempo: 120.0,
    }));
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    let (_, v) = run(&mut e, 4800);
    assert!((v - 0.5).abs() < 1e-3, "frozen arrangement: {v}");
    // After the frozen arrangement ends the track is silent.
    run(&mut e, (4.5 * SR as f64) as usize);
    let (peak, _) = run(&mut e, 4800);
    assert!(peak < 1e-4, "{peak}");
}

#[test]
fn arrangement_fades_ramp_audio_clips() {
    let mut p = Project::default();
    let id = dc_track(&mut p);
    {
        let a = &mut p.track_mut(id).unwrap().arrangement[0];
        a.fade_in = 2.0;
        a.fade_out = 2.0;
    }
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    // Half way into the fade-in (beat 1 = 0.5 s): sin(pi/4) * 0.5.
    let (_, v) = run(&mut e, 24000);
    let expect = 0.5 * std::f32::consts::FRAC_1_SQRT_2;
    assert!((v - expect).abs() < 0.01, "{v} vs {expect}");
    // Middle of the clip: full level.
    let (_, v) = run(&mut e, 72000);
    assert!((v - 0.5).abs() < 1e-3, "{v}");
}

#[test]
fn tempo_automation_drives_the_transport() {
    let mut p = Project::default();
    p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let mut lane = TempoLane::default();
    let n60 = lane.normalize(60.0);
    lane.envelope.insert(0.0, n60);
    p.tempo_lane = lane;
    p.tempo = 120.0;
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, 48000);
    assert!(
        (e.song_beat() - 1.0).abs() < 0.01,
        "one second at 60 BPM: {}",
        e.song_beat()
    );
    assert!((h.shared.tempo.load() - 60.0).abs() < 1e-6);
    // A manual tempo change overrides the automation until re-enabled.
    h.send(Command::SetTempo(120.0));
    run(&mut e, 48000);
    assert!((e.song_beat() - 3.0).abs() < 0.01, "{}", e.song_beat());
    h.send(Command::FollowTempoAutomation);
    run(&mut e, 48000);
    assert!((e.song_beat() - 4.0).abs() < 0.01, "{}", e.song_beat());
}

#[test]
fn signature_changes_move_launch_quantization() {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let c = midi_clip(&mut p, 4.0);
    p.track_mut(id).unwrap().slots[0] = Some(c);
    // 3/4 from the second bar (beat 4).
    p.signatures.push(SignatureChange {
        beat: 4.0,
        numerator: 3,
        denominator: 4,
    });
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, (2.25 * SR as f64) as usize); // beat 4.5
    h.send(Command::LaunchClip { track: id, slot: 0 });
    // Launches on the next 3/4 bar line: beat 7 (3.5 s), not beat 8.
    run(&mut e, (1.15 * SR as f64) as usize);
    assert_eq!(h.shared.tracks[0].playing_slot.load(Ordering::Relaxed), -1);
    run(&mut e, (0.2 * SR as f64) as usize);
    assert_eq!(h.shared.tracks[0].playing_slot.load(Ordering::Relaxed), 0);
}

#[test]
fn scene_follow_actions_launch_the_next_scene() {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    for s in 0..3 {
        let c = midi_clip(&mut p, 4.0);
        p.track_mut(id).unwrap().slots[s] = Some(c);
    }
    p.scenes[0].follow = SceneFollow {
        enabled: true,
        action: SceneFollowAction::Next,
        beats: 4.0,
    };
    p.scenes[1].follow = SceneFollow {
        enabled: true,
        action: SceneFollowAction::First,
        beats: 8.0,
    };
    let (mut e, mut h) = setup(&p);
    let slot = |h: &EngineHandle| h.shared.tracks[0].playing_slot.load(Ordering::Relaxed);
    h.send(Command::LaunchScene(0));
    run(&mut e, 4800);
    assert_eq!(slot(&h), 0);
    // After 4 beats (2 s): scene 2.
    run(&mut e, (2.0 * SR as f64) as usize);
    assert_eq!(slot(&h), 1);
    // 8 beats later: back to the first.
    run(&mut e, (4.0 * SR as f64) as usize);
    assert_eq!(slot(&h), 0);
    // Stop All Clips cancels the follow action.
    h.send(Command::StopAllClips);
    run(&mut e, (4.0 * SR as f64) as usize);
    assert_eq!(slot(&h), -1);
}

#[test]
fn scene_signature_and_stop_buttons() {
    let mut p = Project::default();
    let a = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let b = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    for t in [a, b] {
        let c = midi_clip(&mut p, 4.0);
        p.track_mut(t).unwrap().slots[0] = Some(c);
    }
    // Scene 1 has an empty slot on both tracks; track b's has no stop button.
    p.track_mut(b).unwrap().no_stop_slots.push(1);
    p.scenes[1] = Scene {
        name: "2".into(),
        signature: Some((6, 8)),
        ..Default::default()
    };
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchScene(0));
    run(&mut e, 4800);
    h.send(Command::LaunchScene(1));
    run(&mut e, (2.1 * SR as f64) as usize);
    let slot = |i: usize| h.shared.tracks[i].playing_slot.load(Ordering::Relaxed);
    assert_eq!(slot(0), -1, "stop button stops track a");
    assert_eq!(slot(1), 0, "no stop button: track b keeps playing");
    assert_eq!(e.beats_per_bar, 6);
    assert!((e.bar_beats - 3.0).abs() < 1e-9);
}
