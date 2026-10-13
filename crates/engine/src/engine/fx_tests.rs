//! Engine tests for device chains, sends/returns, input monitoring and
//! recording.

use super::*;
use crate::dsp::synth::SynthParams;
use crate::fx::{Device, DeviceKind};
use crate::model::{Clip, ClipContent, Instrument, Note, Project, TrackKind, SEND_OFF_DB};
use crate::time::Quantization;

const SR: f32 = 48000.0;

/// A synth track holding a long note in scene 0, no returns, no master
/// devices.
fn synth_project() -> (Project, Id) {
    let mut p = Project::default();
    p.returns.clear();
    p.master_devices.clear();
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let cid = p.alloc_id();
    p.track_mut(id).unwrap().slots[0] = Some(Clip {
        id: cid,
        name: "c".into(),
        color: [0, 0, 0],
        length: 4.0,
        looping: true,
        gain_db: 0.0,
        envelopes: Default::default(),
        content: ClipContent::Midi {
            notes: vec![Note {
                pitch: 57,
                velocity: 127,
                start: 0.0,
                length: 4.0,
            }],
        },
        ..Default::default()
    });
    (p, id)
}

fn setup(p: &Project) -> (Engine, EngineHandle) {
    let (mut e, mut h) = Engine::new(SR);
    h.load_project(p, true);
    let mut buf = vec![0.0; 2];
    e.process(&mut buf);
    (e, h)
}

/// Peak of the output over `frames` frames.
fn run(e: &mut Engine, frames: usize) -> f32 {
    let mut buf = vec![0.0; 512 * 2];
    let mut peak = 0.0f32;
    let mut done = 0;
    while done < frames {
        e.process(&mut buf);
        peak = buf.iter().fold(peak, |a, b| a.max(b.abs()));
        done += 512;
    }
    peak
}

fn play_clip(e: &mut Engine, h: &mut EngineHandle, track: Id) -> f32 {
    h.send(Command::SetQuantization(Quantization::None));
    h.send(Command::LaunchClip { track, slot: 0 });
    run(e, 4800);
    run(e, 9600)
}

/// Plays the clip of a fresh engine for this project and returns the peak of
/// the same window every time, so projects can be compared exactly.
fn peak_of(p: &Project, track: Id) -> f32 {
    let (mut e, mut h) = setup(p);
    play_clip(&mut e, &mut h, track)
}

#[test]
fn insert_effects_change_the_track_output() {
    let (mut p, id) = synth_project();
    let dry = peak_of(&p, id);
    assert!(dry > 0.05, "{dry}");

    // Add a utility at -24 dB.
    let dev = p.alloc_id();
    let mut d = Device::new(dev, DeviceKind::Utility);
    d.params.set(0, -24.0);
    p.track_mut(id).unwrap().devices.push(d);
    let quiet = peak_of(&p, id);
    assert!((quiet / dry - 0.0631).abs() < 0.002, "{quiet} vs {dry}");

    // Changing the chain while playing keeps going.
    let (mut e, mut h) = setup(&p);
    play_clip(&mut e, &mut h, id);

    // A parameter change takes effect without a new snapshot.
    h.send(Command::SetDeviceParam {
        bus: id,
        device: dev,
        index: 0,
        value: 0.0,
    });
    run(&mut e, 4800);
    let back = run(&mut e, 9600);
    assert!(back > quiet * 5.0, "{back} vs {quiet}");

    // Switching the device off bypasses it.
    h.send(Command::SetDeviceParam {
        bus: id,
        device: dev,
        index: 0,
        value: -36.0,
    });
    h.send(Command::SetDeviceOn {
        bus: id,
        device: dev,
        on: false,
    });
    run(&mut e, 4800);
    let bypassed = run(&mut e, 9600);
    assert!(bypassed > quiet * 5.0, "{bypassed} vs {quiet}");
    h.collect_garbage();
}

#[test]
fn sends_feed_returns_post_fader() {
    let (mut p, id) = synth_project();
    // A return with no devices: the send adds a second copy of the track.
    let ret = p.add_return(None).unwrap();
    let alone = peak_of(&p, id);
    p.track_mut(id).unwrap().sends[0] = 0.0;
    let doubled = peak_of(&p, id);
    assert!((doubled / alone - 2.0).abs() < 0.01, "{doubled} vs {alone}");
    // Return muted: back to one copy.
    p.returns[0].mixer.mute = true;
    let muted = peak_of(&p, id);
    assert!((muted / alone - 1.0).abs() < 0.01, "{muted} vs {alone}");
    p.returns[0].mixer.mute = false;

    let (mut e, mut h) = setup(&p);
    play_clip(&mut e, &mut h, id);
    assert!(h.shared.returns[0].meter_l.load() > 0.0);
    assert_eq!(h.shared.returns[0].id.load(Ordering::Relaxed), ret);
    h.send(Command::SetSend {
        track: id,
        index: 0,
        db: SEND_OFF_DB,
    });
    run(&mut e, 48000);
    assert!(h.shared.returns[0].meter_l.load() < 0.02, "send off");

    // Post-fader: muting the track silences its send too.
    h.send(Command::SetSend {
        track: id,
        index: 0,
        db: 0.0,
    });
    let tm = MixerParams {
        mute: true,
        ..Default::default()
    };
    h.send(Command::SetMixer { track: id, params: tm });
    run(&mut e, 4800);
    assert!(run(&mut e, 9600) < 1e-4);

    // Saved send levels survive a snapshot; removing the return is safe.
    p.track_mut(id).unwrap().sends[0] = SEND_OFF_DB;
    p.remove_return(ret);
    h.load_project(&p, false);
    run(&mut e, 4800);
    h.collect_garbage();
}

#[test]
fn master_limiter_holds_the_ceiling() {
    let (mut p, id) = synth_project();
    p.track_mut(id).unwrap().mixer.volume_db = 6.0;
    let dev = p.alloc_id();
    let mut lim = Device::new(dev, DeviceKind::Limiter);
    lim.params.set(0, 12.0); // gain
    lim.params.set(1, -3.0); // ceiling
    p.master_devices.push(lim);
    let (mut e, mut h) = setup(&p);
    let peak = play_clip(&mut e, &mut h, id);
    assert!(peak <= crate::fx::db(-3.0) + 1e-4, "{peak}");
    assert!(peak > crate::fx::db(-4.0), "limiter drives into the ceiling: {peak}");
    assert!(h.shared.device_meter(dev).unwrap() < -1.0);
}

/// An armed audio track with an empty slot.
fn audio_project() -> (Project, Id) {
    let mut p = Project::default();
    p.returns.clear();
    p.master_devices.clear();
    let id = p.add_track(TrackKind::Audio, Instrument::None);
    p.track_mut(id).unwrap().mixer.arm = true;
    (p, id)
}

/// Feeds a constant input level and processes `blocks` blocks of 512.
fn run_with_input(e: &mut Engine, input: &mut rtrb::Producer<f32>, level: f32, blocks: usize) -> f32 {
    let mut buf = vec![0.0; 512 * 2];
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        for _ in 0..512 {
            let _ = input.push(level);
            let _ = input.push(-level);
        }
        e.process(&mut buf);
        peak = buf.iter().fold(peak, |a, b| a.max(b.abs()));
    }
    peak
}

fn events(h: &mut EngineHandle) -> Vec<RecEvent> {
    let mut v = Vec::new();
    while let Ok(ev) = h.record.events.pop() {
        v.push(ev);
    }
    v
}

fn drain_audio(h: &mut EngineHandle) -> usize {
    let mut n = 0;
    while h.record.audio.pop().is_ok() {
        n += 1;
    }
    n / 2
}

#[test]
fn armed_audio_tracks_monitor_their_input() {
    let (mut p, id) = audio_project();
    let (tx, rx) = input_ring();
    let (mut e, mut h) = Engine::new(SR);
    e.set_input(rx);
    let mut tx = tx;
    h.load_project(&p, true);
    let peak = run_with_input(&mut e, &mut tx, 0.25, 20);
    assert!((peak - 0.25).abs() < 0.01, "{peak}");
    assert!(h.shared.record.input_peak.load() > 0.2);

    // Disarmed in Auto: silent. Monitor In: audible again.
    p.track_mut(id).unwrap().mixer.arm = false;
    h.send(Command::SetMixer {
        track: id,
        params: p.track(id).unwrap().mixer,
    });
    run_with_input(&mut e, &mut tx, 0.25, 2);
    assert!(run_with_input(&mut e, &mut tx, 0.25, 4) < 1e-4);
    let mut input = p.track(id).unwrap().input;
    input.monitor = crate::model::Monitor::In;
    input.channels = crate::model::InputChannels::Right;
    h.send(Command::SetTrackInput { track: id, input });
    run_with_input(&mut e, &mut tx, 0.25, 2);
    let mut buf = vec![0.0; 1024];
    for _ in 0..4 {
        for _ in 0..512 {
            let _ = tx.push(0.1);
            let _ = tx.push(-0.3);
        }
        e.process(&mut buf);
    }
    // Mono from input 2 on both sides.
    assert!(
        (buf[1000] + 0.3).abs() < 0.01 && (buf[1001] + 0.3).abs() < 0.01,
        "{} {}",
        buf[1000],
        buf[1001]
    );
}

#[test]
fn arrangement_recording_with_count_in() {
    let (p, id) = audio_project();
    let (tx, rx) = input_ring();
    let mut tx = tx;
    let (mut e, mut h) = Engine::new(SR);
    e.set_input(rx);
    h.load_project(&p, true);
    run_with_input(&mut e, &mut tx, 0.1, 4);
    h.send(Command::SetCountIn(1));
    h.send(Command::SetPosition(8.0));
    h.send(Command::SetArrangementRecord(true));
    // One bar at 120 BPM = 2 s = 96000 frames of count-in.
    run_with_input(&mut e, &mut tx, 0.1, 150);
    assert!(!e.is_playing());
    assert!(h.shared.record.count_in.load() > 0.0);
    assert!(events(&mut h).is_empty());
    run_with_input(&mut e, &mut tx, 0.1, 50);
    assert!(e.is_playing());
    let ev = events(&mut h);
    let RecEvent::Started {
        track,
        target,
        beat,
        frame: start,
        ..
    } = ev[0]
    else {
        panic!("{ev:?}")
    };
    assert_eq!((track, target), (id, RecTarget::Arrangement));
    assert!((beat - 8.0).abs() < 1e-9, "starts at the start marker: {beat}");
    assert!(h.shared.record.take_for(id).is_some());

    // Two seconds of recording, then stop.
    run_with_input(&mut e, &mut tx, 0.1, 188);
    h.send(Command::Stop);
    run_with_input(&mut e, &mut tx, 0.1, 2);
    let ev = events(&mut h);
    let RecEvent::Stopped { beat, frame: stop, .. } = ev[0] else {
        panic!("{ev:?}")
    };
    let secs = (stop - start) as f64 / SR as f64;
    assert!((beat - 8.0 - secs * 2.0).abs() < 1e-6, "{beat} after {secs} s");
    // The count-in ended 98048 frames in; we stopped after 392 blocks.
    assert!((stop - start).abs_diff(392 * 512 - 98048) <= 1, "{}", stop - start);
    assert!(drain_audio(&mut h) as u64 >= stop);
    assert!(!h.shared.record.arrangement.load(Ordering::Relaxed));
    assert!(h.shared.record.take_for(id).is_none());
}

#[test]
fn arrangement_takes_split_at_loop_jumps() {
    let (mut p, id) = audio_project();
    p.loop_enabled = true;
    p.loop_start = 0.0;
    p.loop_length = 4.0;
    let (tx, rx) = input_ring();
    let mut tx = tx;
    let (mut e, mut h) = Engine::new(SR);
    e.set_input(rx);
    h.load_project(&p, true);
    run_with_input(&mut e, &mut tx, 0.1, 4);
    h.send(Command::SetArrangementRecord(true));
    // 4 beats = 2 s; run a bit over one loop.
    run_with_input(&mut e, &mut tx, 0.1, 230);
    let ev = events(&mut h);
    let starts: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            RecEvent::Started { beat, track, .. } if *track == id => Some(*beat),
            _ => None,
        })
        .collect();
    let stops: Vec<_> = ev
        .iter()
        .filter_map(|e| match e {
            RecEvent::Stopped { beat, .. } => Some(*beat),
            _ => None,
        })
        .collect();
    assert_eq!(starts.len(), 2, "{ev:?}");
    assert!(starts[1].abs() < 1e-3 && (stops[0] - 4.0).abs() < 1e-9, "{ev:?}");
}

#[test]
fn session_recording_is_quantized_and_then_plays() {
    let (mut p, id) = audio_project();
    let (tx, rx) = input_ring();
    let mut tx = tx;
    let (mut e, mut h) = Engine::new(SR);
    e.set_input(rx);
    h.load_project(&p, true);
    h.send(Command::SetQuantization(Quantization::Bar));
    h.send(Command::Play);
    run_with_input(&mut e, &mut tx, 0.1, 10); // into bar 1
    h.send(Command::RecordSlot { track: id, slot: 2 });
    run_with_input(&mut e, &mut tx, 0.1, 2);
    let take = h.shared.record.take_for(id).unwrap();
    assert!(!take.recording.load(Ordering::Relaxed), "queued until the bar");
    run_with_input(&mut e, &mut tx, 0.1, 200);
    let ev = events(&mut h);
    let RecEvent::Started { beat, target, .. } = ev[0] else {
        panic!("{ev:?}")
    };
    assert_eq!(target, RecTarget::Slot(2));
    assert!((beat - 4.0).abs() < 1e-9, "starts on the next bar: {beat}");

    // Launching the recording slot ends the take at the next bar.
    h.send(Command::LaunchClip { track: id, slot: 2 });
    run_with_input(&mut e, &mut tx, 0.1, 200);
    let ev = events(&mut h);
    let RecEvent::Stopped { beat: stop, .. } = ev[0] else {
        panic!("{ev:?}")
    };
    assert!((stop - 12.0).abs() < 1e-9 || (stop - 8.0).abs() < 1e-9, "{stop}");

    // The UI adds the clip; the engine starts it from the stop beat.
    let cid = p.alloc_id();
    p.track_mut(id).unwrap().slots[2] = Some(Clip {
        id: cid,
        name: "rec".into(),
        color: [0, 0, 0],
        length: stop - 4.0,
        looping: true,
        gain_db: 0.0,
        envelopes: Default::default(),
        content: ClipContent::Audio(crate::model::AudioRef {
            path: String::new(),
            data: None,
            warp: Default::default(),
        }),
        ..Default::default()
    });
    h.load_project(&p, false);
    run_with_input(&mut e, &mut tx, 0.1, 2);
    assert_eq!(h.shared.tracks[0].playing_slot.load(Ordering::Relaxed), 2);
    h.collect_garbage();
}

#[test]
fn stopping_the_track_cancels_a_queued_recording() {
    let (p, id) = audio_project();
    let (tx, rx) = input_ring();
    let mut tx = tx;
    let (mut e, mut h) = Engine::new(SR);
    e.set_input(rx);
    h.load_project(&p, true);
    h.send(Command::SetQuantization(Quantization::Bar));
    h.send(Command::Play);
    run_with_input(&mut e, &mut tx, 0.1, 10);
    h.send(Command::RecordSlot { track: id, slot: 0 });
    h.send(Command::StopTrack(id));
    run_with_input(&mut e, &mut tx, 0.1, 200);
    assert!(events(&mut h).is_empty());
    assert_eq!(drain_audio(&mut h), 0, "nothing streamed while not recording");
}
