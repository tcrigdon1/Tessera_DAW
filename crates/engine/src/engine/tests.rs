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
        envelopes: Vec::new(),
        ..Default::default()
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
        content: ClipContent::Audio(AudioRef::new("x.wav", Some(Arc::new(data)))),
        envelopes: Vec::new(),
        ..Default::default()
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

// ----- automation -----------------------------------------------------------------

use crate::automation::{lane_mut, AutomationTarget, PAN_SPEC, VOLUME_SPEC};

/// Peak of each channel over `frames` frames.
fn run_lr(e: &mut Engine, frames: usize) -> (f32, f32) {
    let mut buf = vec![0.0; frames * 2];
    e.process(&mut buf);
    buf.chunks(2)
        .fold((0.0f32, 0.0f32), |(l, r), s| (l.max(s[0].abs()), r.max(s[1].abs())))
}

/// One synth track with a held note on the Arrangement for 16 beats.
fn arrangement_track() -> (Project, Id) {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let c = clip(&mut p, 16.0, held_note(16.0));
    let aid = p.alloc_id();
    p.track_mut(id).unwrap().arrangement.push(ArrangementClip {
        id: aid,
        start: 0.0,
        length: 16.0,
        clip: c,
    });
    (p, id)
}

#[test]
fn volume_automation_is_sample_accurate() {
    let (mut p, id) = arrangement_track();
    let env = &mut lane_mut(&mut p.track_mut(id).unwrap().automation, AutomationTarget::Volume).envelope;
    env.insert(0.0, VOLUME_SPEC.normalize(0.0));
    env.insert(2.0, VOLUME_SPEC.normalize(0.0));
    env.insert(2.0 + 1e-7, 0.0); // a vertical step to -inf at beat 2
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    // Beat 2 at 120 BPM is frame 48000.
    let before = run(&mut e, 47900);
    assert!(before > 0.01, "audible before the step, {before}");
    run(&mut e, 100 + 480); // 10 ms after the step
    let after = run(&mut e, 4800);
    assert!(after < 1e-3, "silent right after the step, {after}");
}

#[test]
fn volume_ramp_follows_the_envelope() {
    let (mut p, id) = arrangement_track();
    let env = &mut lane_mut(&mut p.track_mut(id).unwrap().automation, AutomationTarget::Volume).envelope;
    env.insert(0.0, VOLUME_SPEC.normalize(-60.0));
    env.insert(8.0, VOLUME_SPEC.normalize(0.0));
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, 24000);
    let early = run(&mut e, 4800);
    run(&mut e, 48000 * 3 - 4800);
    let late = run(&mut e, 4800);
    assert!(late > early * 20.0, "fades in: early {early}, late {late}");
    let published = h.shared.tracks[0].auto_volume.load();
    assert!(published > -12.0 && published < 0.0, "published {published} dB");
}

#[test]
fn pan_automation_moves_the_signal() {
    let (mut p, id) = arrangement_track();
    let env = &mut lane_mut(&mut p.track_mut(id).unwrap().automation, AutomationTarget::Pan).envelope;
    env.insert(0.0, PAN_SPEC.normalize(-1.0));
    env.insert(4.0, PAN_SPEC.normalize(-1.0));
    env.insert(4.0 + 1e-7, PAN_SPEC.normalize(1.0));
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, 4800);
    let (l, r) = run_lr(&mut e, 4800);
    assert!(l > 0.01 && r < 1e-4, "hard left: {l} {r}");
    run(&mut e, 96000);
    let (l, r) = run_lr(&mut e, 4800);
    assert!(r > 0.01 && l < 1e-4, "hard right: {l} {r}");
}

#[test]
fn instrument_parameters_follow_automation() {
    let (mut p, id) = arrangement_track();
    // Synth Volume (param 14): -40 dB for 4 beats, then +6 dB.
    let spec = crate::automation::SYNTH_PARAMS[14];
    let env = &mut lane_mut(
        &mut p.track_mut(id).unwrap().automation,
        AutomationTarget::Instrument(14),
    )
    .envelope;
    env.insert(4.0, spec.normalize(-40.0));
    env.insert(4.0 + 1e-7, spec.normalize(6.0));
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, 24000);
    let quiet = run(&mut e, 24000);
    run(&mut e, 48000 + 4800);
    let loud = run(&mut e, 24000);
    assert!(loud > quiet * 50.0, "quiet {quiet}, loud {loud}");
    let shown = h.shared.tracks[0].auto_params[14].load();
    assert!((shown - 6.0).abs() < 1e-3, "published {shown}");
    assert!(
        h.shared.tracks[0].auto_params[3].load().is_nan(),
        "cutoff is not automated"
    );
}

#[test]
fn newer_instruments_follow_volume_automation() {
    use crate::automation::{instrument_params, FM_PARAMS, WAVETABLE_PARAMS};
    use crate::dsp::{fm::FmParams, wavetable::WavetableParams};
    let cases = [
        (Instrument::Fm(FmParams::default()), 15u16),
        (Instrument::Wavetable(WavetableParams::default()), 39),
    ];
    assert_eq!(FM_PARAMS[15].name, "FM Volume");
    assert_eq!(WAVETABLE_PARAMS[39].name, "Wavetable Volume");
    for (instrument, idx) in cases {
        let (mut p, id) = arrangement_track();
        let t = p.track_mut(id).unwrap();
        t.instrument = instrument;
        let spec = instrument_params(&instrument)[idx as usize];
        let env = &mut lane_mut(&mut t.automation, AutomationTarget::Instrument(idx)).envelope;
        env.insert(4.0, spec.normalize(-40.0));
        env.insert(4.0 + 1e-7, spec.normalize(6.0));
        let (mut e, mut h) = setup(&p);
        h.send(Command::Play);
        run(&mut e, 24000);
        let quiet = run(&mut e, 24000);
        run(&mut e, 48000 + 4800);
        let loud = run(&mut e, 24000);
        assert!(loud > quiet * 50.0, "{}: quiet {quiet}, loud {loud}", spec.name);
    }
}

#[test]
fn overridden_lanes_are_ignored() {
    let (mut p, id) = arrangement_track();
    let lane = lane_mut(&mut p.track_mut(id).unwrap().automation, AutomationTarget::Volume);
    lane.envelope.insert(0.0, 0.0);
    lane.overridden = true;
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    assert!(run(&mut e, 9600) > 0.01);
    assert!(h.shared.tracks[0].auto_volume.load().is_nan());
}

#[test]
fn automation_holds_while_stopped_and_chases_the_playhead() {
    let (mut p, id) = arrangement_track();
    let env = &mut lane_mut(&mut p.track_mut(id).unwrap().automation, AutomationTarget::Pan).envelope;
    env.insert(0.0, 0.0);
    env.insert(8.0, 1.0);
    let (mut e, mut h) = setup(&p);
    h.send(Command::SetPosition(4.0));
    run(&mut e, 512);
    let pan = h.shared.tracks[0].auto_pan.load();
    assert!(pan.abs() < 1e-3, "pan at beat 4 is centre, got {pan}");
    h.send(Command::SetPosition(8.0));
    run(&mut e, 512);
    assert!((h.shared.tracks[0].auto_pan.load() - 1.0).abs() < 1e-3);
}

#[test]
fn session_clip_envelopes_loop_with_the_clip() {
    let (mut p, id) = one_track();
    let c = p.track_mut(id).unwrap().slots[0].as_mut().unwrap();
    // 4-beat clip: silent for the first 2 beats of every loop.
    let env = &mut lane_mut(&mut c.envelopes, AutomationTarget::Volume).envelope;
    env.insert(0.0, 0.0);
    env.insert(2.0, 0.0);
    env.insert(2.0 + 1e-7, VOLUME_SPEC.normalize(0.0));
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 480); // gain smoothing settles
    for lap in 0..2 {
        let silent = run(&mut e, 46520);
        run(&mut e, 2000);
        let loud = run(&mut e, 46000);
        run(&mut e, 1480);
        assert!(silent < 1e-3, "lap {lap}: first half silent, {silent}");
        assert!(loud > 0.01, "lap {lap}: second half audible, {loud}");
    }
    assert!(shared_slot(&h) == 0);
}

#[test]
fn arrangement_lanes_win_over_clip_envelopes() {
    let (mut p, id) = arrangement_track();
    let t = p.track_mut(id).unwrap();
    lane_mut(&mut t.arrangement[0].clip.envelopes, AutomationTarget::Volume)
        .envelope
        .insert(0.0, 0.0);
    let (mut e, mut h) = setup(&p);
    h.send(Command::Play);
    run(&mut e, 480); // gain smoothing settles
    assert!(run(&mut e, 9600) < 1e-3, "clip envelope applies without a lane");

    let t = p.track_mut(id).unwrap();
    lane_mut(&mut t.automation, AutomationTarget::Volume)
        .envelope
        .insert(0.0, VOLUME_SPEC.normalize(0.0));
    h.load_project(&p, false);
    run(&mut e, 1024);
    assert!(run(&mut e, 9600) > 0.01, "the lane wins");
    h.collect_garbage();
}

// ----- warping --------------------------------------------------------------------

/// An audio track whose clip is a click every half second (120 BPM), 2 s long.
fn click_clip_project(warp: crate::warp::WarpSettings) -> (Project, Id) {
    use crate::model::{AudioData, AudioRef};
    let mut v = vec![0.0f32; 96000];
    for k in 0..4 {
        for i in 0..240 {
            v[k * 24000 + i] = 0.8 * (1.0 - i as f32 / 240.0);
        }
    }
    let data = AudioData {
        sample_rate: 48000,
        left: v.clone(),
        right: v,
    };
    let mut p = Project {
        launch_quantization: Quantization::None,
        ..Default::default()
    };
    let id = p.add_track(TrackKind::Audio, Instrument::None);
    let mut audio = AudioRef::new("clicks.wav", Some(Arc::new(data)));
    audio.warp = warp;
    let c = Clip {
        id: p.alloc_id(),
        length: 4.0,
        content: ClipContent::Audio(audio),
        ..Default::default()
    };
    p.track_mut(id).unwrap().slots[0] = Some(c);
    (p, id)
}

/// Frames (from the launch) where clicks start.
fn click_onsets(e: &mut Engine, frames: usize) -> Vec<usize> {
    let mut buf = vec![0.0; frames * 2];
    for chunk in buf.chunks_mut(512) {
        e.process(chunk);
    }
    let mut onsets = Vec::new();
    let mut quiet = 0;
    for (i, s) in buf.chunks(2).enumerate() {
        if s[0].abs() > 0.3 {
            if quiet > 2000 || onsets.is_empty() {
                onsets.push(i);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    onsets
}

#[test]
fn warped_clips_follow_the_project_tempo() {
    use crate::warp::{WarpMode, WarpSettings};
    for mode in [WarpMode::Beats, WarpMode::RePitch, WarpMode::Complex] {
        let (p, id) = click_clip_project(WarpSettings {
            enabled: true,
            mode,
            bpm: 120.0,
            transients: vec![0.0, 0.5, 1.0, 1.5],
            ..Default::default()
        });
        let (mut e, mut h) = setup(&p);
        h.send(Command::SetTempo(90.0));
        h.send(Command::LaunchClip { track: id, slot: 0 });
        // At 90 BPM a beat lasts 32000 frames.
        let onsets = click_onsets(&mut e, 4 * 32000);
        assert_eq!(onsets.len(), 4, "{mode:?}: {onsets:?}");
        for (k, &o) in onsets.iter().enumerate() {
            let want = k * 32000;
            assert!(o.abs_diff(want) < 600, "{mode:?}: click {k} at {o}, want {want}");
        }
    }
}

#[test]
fn unwarped_clips_ignore_the_tempo() {
    let (p, id) = click_clip_project(crate::warp::WarpSettings::default());
    let (mut e, mut h) = setup(&p);
    h.send(Command::SetTempo(90.0));
    h.send(Command::LaunchClip { track: id, slot: 0 });
    let onsets = click_onsets(&mut e, 2 * 48000);
    assert_eq!(onsets.len(), 4, "{onsets:?}");
    // Clicks stay 0.5 s apart (measured between onsets, since the master
    // limiter's lookahead delays the output as a whole).
    assert!((onsets[2] - onsets[1]).abs_diff(24000) < 50, "{onsets:?}");
}
