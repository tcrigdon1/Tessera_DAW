//! Verifies that MIDI effects never allocate or free on the audio thread
//! (while playing, looping, changing tempo, stopping and starting, swapping
//! snapshots and taking live notes on the free-running clock), and that the
//! arpeggiator audibly plays.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::midi_fx::arpeggiator::ArpeggiatorParams;
use tessera_engine::midi_fx::{MidiDevice, MidiDeviceKind, MidiDeviceParams};
use tessera_engine::model::{ArrangementClip, Clip, ClipContent, Id, Instrument, Note, Project, TrackKind};
use tessera_engine::{Command, Engine};

struct CountingAlloc;

thread_local! {
    static WATCH: Cell<bool> = const { Cell::new(false) };
}
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCH.with(|w| w.get()) {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if WATCH.with(|w| w.get()) {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

fn watched<R>(f: impl FnOnce() -> R) -> R {
    WATCH.with(|w| w.set(true));
    let r = f();
    WATCH.with(|w| w.set(false));
    r
}

fn clip(p: &mut Project, notes: Vec<Note>) -> Clip {
    Clip {
        id: p.alloc_id(),
        name: "c".into(),
        length: 4.0,
        looping: true,
        content: ClipContent::Midi { notes },
        ..Default::default()
    }
}

/// Chords of three notes on every beat, plus fast sixteenths.
fn busy_notes() -> Vec<Note> {
    let mut notes = Vec::new();
    for beat in 0..4 {
        for (k, pitch) in [48u8, 52, 55].into_iter().enumerate() {
            notes.push(Note {
                pitch: pitch + beat as u8 * 2,
                velocity: 60 + 20 * k as u8,
                start: beat as f64,
                length: 0.9,
            });
        }
    }
    for i in 0..16 {
        notes.push(Note {
            pitch: 60 + (i * 7 % 12) as u8,
            velocity: 100,
            start: i as f64 * 0.25,
            length: 0.2,
        });
    }
    notes
}

fn add_device(p: &mut Project, track: Id, kind: MidiDeviceKind) -> Id {
    let id = p.alloc_id();
    p.track_mut(track).unwrap().midi_devices.push(MidiDevice::new(id, kind));
    id
}

#[test]
fn midi_effects_do_not_allocate() {
    let mut p = Project::default();
    // Every MIDI effect in one chain, arpeggiator last so its output
    // (one note at a time) isn't the only thing the others see.
    let all = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let mut all_devices = Vec::new();
    for kind in [
        MidiDeviceKind::Chord,
        MidiDeviceKind::Scale,
        MidiDeviceKind::Random,
        MidiDeviceKind::Velocity,
        MidiDeviceKind::Pitch,
        MidiDeviceKind::NoteLength,
        MidiDeviceKind::Arpeggiator,
    ] {
        all_devices.push(add_device(&mut p, all, kind));
    }
    // An arpeggiator on its own track, plus a held chord through Chord +
    // Note Length (note-off trigger) on another.
    let arp_track = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let arp = add_device(&mut p, arp_track, MidiDeviceKind::Arpeggiator);
    let other = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    add_device(&mut p, other, MidiDeviceKind::Chord);
    let nl = add_device(&mut p, other, MidiDeviceKind::NoteLength);

    for t in [all, arp_track, other] {
        let c = clip(&mut p, busy_notes());
        p.track_mut(t).unwrap().slots[0] = Some(c.clone());
        let arr = ArrangementClip {
            id: p.alloc_id(),
            start: 0.0,
            length: 16.0,
            clip: c,
            ..Default::default()
        };
        p.track_mut(t).unwrap().arrangement.push(arr);
    }

    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);
    let mut run = |engine: &mut Engine, blocks: usize| {
        watched(|| {
            for _ in 0..blocks {
                engine.process(&mut buf);
            }
        })
    };

    // Arrangement playback with a short loop (blocks split at the loop end).
    handle.send(Command::SetLoop {
        enabled: true,
        start: 1.0,
        length: 2.5,
    });
    handle.send(Command::Play);
    run(&mut engine, 300);

    // Every arpeggiator style and rate, with steps, hold and decay.
    for style in 0..13 {
        let params = ArpeggiatorParams {
            style: style as f32,
            rate: (style % 9) as f32,
            gate: if style % 2 == 0 { 1.7 } else { 0.3 },
            steps: (style % 4) as f32,
            distance: if style % 3 == 0 { -7.0 } else { 12.0 },
            hold: (style % 2) as f32,
            velocity_decay: 0.5,
        };
        for (track, device) in [(arp_track, arp), (all, all_devices[6])] {
            handle.send(Command::SetMidiDeviceParams {
                track,
                device,
                params: MidiDeviceParams::Arpeggiator(params),
            });
        }
        run(&mut engine, 25);
    }

    // Tweak the other devices live.
    for (i, &device) in all_devices.iter().enumerate().take(6) {
        handle.send(Command::SetMidiDeviceParam {
            track: all,
            device,
            index: 0,
            value: if i % 2 == 0 { 1.0 } else { 0.5 },
        });
    }
    handle.send(Command::SetMidiDeviceParam {
        track: other,
        device: nl,
        index: 4,
        value: 1.0,
    });
    // Tempo changes mid-play.
    for tempo in [90.0, 174.0, 61.0, 140.0] {
        handle.send(Command::SetTempo(tempo));
        run(&mut engine, 40);
    }

    // Session clips, stop and start, devices switched off and on.
    handle.send(Command::LaunchScene(0));
    run(&mut engine, 100);
    handle.send(Command::Stop);
    run(&mut engine, 20);
    handle.send(Command::Play);
    run(&mut engine, 60);
    handle.send(Command::SetMidiDeviceOn {
        track: all,
        device: all_devices[0],
        on: false,
    });
    run(&mut engine, 30);
    handle.send(Command::SetMidiDeviceOn {
        track: all,
        device: all_devices[0],
        on: true,
    });
    run(&mut engine, 30);

    // A new snapshot with a changed chain (devices keep their state by id).
    p.track_mut(all).unwrap().midi_devices.swap(0, 1);
    let extra = p.alloc_id();
    p.track_mut(arp_track)
        .unwrap()
        .midi_devices
        .insert(0, MidiDevice::new(extra, MidiDeviceKind::Random));
    handle.load_project(&p, false);
    run(&mut engine, 150);

    // Live notes while stopped: the arpeggiators run on the free clock.
    handle.send(Command::Stop);
    run(&mut engine, 10);
    for (track, pitch) in [(arp_track, 60), (arp_track, 64), (all, 67), (other, 50)] {
        handle.send(Command::NoteOn {
            track,
            pitch,
            velocity: 100,
        });
    }
    run(&mut engine, 200);
    for (track, pitch) in [(arp_track, 60), (all, 67), (other, 50)] {
        handle.send(Command::NoteOff { track, pitch });
    }
    run(&mut engine, 50);
    // Start the transport under a held note (clock jump), then stop again.
    handle.send(Command::Play);
    run(&mut engine, 100);
    handle.send(Command::NoteOff {
        track: arp_track,
        pitch: 64,
    });
    handle.send(Command::Stop);
    handle.send(Command::Panic);
    run(&mut engine, 50);

    handle.collect_garbage();
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}

/// Peak of each 10 ms window over `windows` windows.
fn window_peaks(engine: &mut Engine, windows: usize) -> Vec<f32> {
    let mut buf = vec![0.0f32; 480 * 2];
    (0..windows)
        .map(|_| {
            engine.process(&mut buf);
            buf.iter().fold(0.0f32, |a, b| a.max(b.abs()))
        })
        .collect()
}

#[test]
fn arpeggiator_plays_held_notes_rhythmically() {
    let mut p = Project::default();
    assert_eq!(p.tempo, 120.0);
    let t = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let arp = p.alloc_id();
    let mut dev = MidiDevice::new(arp, MidiDeviceKind::Arpeggiator);
    // One step per bar (2 s at 120 BPM), each 10% long (0.2 s).
    dev.params = MidiDeviceParams::Arpeggiator(ArpeggiatorParams {
        rate: 8.0,
        gate: 0.1,
        ..Default::default()
    });
    p.track_mut(t).unwrap().midi_devices.push(dev);

    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&p, true);
    let mut warm = vec![0.0f32; 4];
    engine.process(&mut warm);

    // A held key while stopped: without the arpeggiator it would sustain.
    handle.send(Command::NoteOn {
        track: t,
        pitch: 57,
        velocity: 110,
    });
    let peaks = window_peaks(&mut engine, 260); // 2.6 s
    let max = |r: std::ops::Range<usize>| peaks[r].iter().fold(0.0f32, |a, b| a.max(*b));
    assert!(max(0..15) > 0.02, "first step sounds: {}", max(0..15));
    assert!(max(120..190) < 0.002, "silent between steps: {}", max(120..190));
    assert!(max(200..215) > 0.02, "second step sounds: {}", max(200..215));

    // Chord style plays the whole held chord on each step.
    handle.send(Command::SetMidiDeviceParams {
        track: t,
        device: arp,
        params: MidiDeviceParams::Arpeggiator(ArpeggiatorParams {
            style: 12.0,
            rate: 2.0,
            gate: 0.8,
            ..Default::default()
        }),
    });
    for pitch in [60, 64] {
        handle.send(Command::NoteOn {
            track: t,
            pitch,
            velocity: 110,
        });
    }
    let peaks = window_peaks(&mut engine, 100);
    assert!(peaks.iter().fold(0.0f32, |a, b| a.max(*b)) > 0.02);

    // Releasing every key stops the arpeggio.
    for pitch in [57, 60, 64] {
        handle.send(Command::NoteOff { track: t, pitch });
    }
    let peaks = window_peaks(&mut engine, 150);
    assert!(peaks[100..].iter().all(|p| *p < 0.002), "{:?}", &peaks[100..]);
}
