//! Launch modes, follow actions, clip markers and hardware MIDI input must
//! not allocate on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::engine::midi_ring;
use tessera_engine::midi::MidiMessage;
use tessera_engine::model::{Clip, ClipContent, FollowAction, FollowKind, Instrument, LaunchMode, Note, TrackKind};
use tessera_engine::time::Quantization;
use tessera_engine::{demo, Command, Engine};

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

#[test]
fn launching_and_live_midi_do_not_allocate() {
    let mut p = demo::demo_project();
    let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
    let modes = [
        LaunchMode::Trigger,
        LaunchMode::Gate,
        LaunchMode::Toggle,
        LaunchMode::Repeat,
    ];
    let follows = [
        FollowKind::Next,
        FollowKind::Any,
        FollowKind::Other,
        FollowKind::Previous,
    ];
    for (s, (mode, follow)) in modes.into_iter().zip(follows).enumerate() {
        let notes = (0..4)
            .map(|k| Note {
                pitch: 60 + k * 2,
                velocity: 90,
                start: k as f64 * 0.5,
                length: 0.4,
            })
            .collect();
        let mut c = Clip {
            id: p.alloc_id(),
            length: 2.0,
            start_marker: 0.5,
            loop_start: 0.25 * s as f64,
            content: ClipContent::Midi { notes },
            ..Default::default()
        };
        c.launch.mode = mode;
        c.launch.legato = s % 2 == 1;
        c.launch.quantization = Some(Quantization::EighthNote);
        c.launch.follow = FollowAction {
            enabled: true,
            a: follow,
            b: FollowKind::Again,
            chance_a: 0.7,
            linked: s % 2 == 0,
            time: 1.5,
        };
        p.track_mut(id).unwrap().slots[s] = Some(c);
    }
    let (mut engine, mut handle) = Engine::new(48000.0);
    let (mut tx, rx) = midi_ring();
    engine.set_midi_input(rx);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 256 * 2];
    engine.process(&mut buf);
    handle.send(Command::SetSelectedTrack(id));

    for slot in 0..4 {
        handle.send(Command::LaunchClip { track: id, slot });
        watched(|| {
            for i in 0..300 {
                if i % 7 == 0 {
                    let pitch = 40 + (i % 30) as u8;
                    tx.push(MidiMessage::note_on(1, 0, pitch, 100)).ok();
                    tx.push(MidiMessage::cc(1, 0, 64, if i % 2 == 0 { 127 } else { 0 }))
                        .ok();
                    tx.push(MidiMessage::note_off(1, 0, pitch)).ok();
                }
                engine.process(&mut buf);
            }
        });
        handle.send(Command::ReleaseClip { track: id, slot });
        handle.send(Command::LaunchClip { track: id, slot });
        watched(|| {
            for _ in 0..200 {
                engine.process(&mut buf);
            }
        });
    }
    p.track_mut(id).unwrap().mixer.arm = true;
    handle.load_project(&p, false);
    handle.send(Command::LaunchScene(1));
    watched(|| {
        for _ in 0..300 {
            tx.push(MidiMessage::note_on(1, 0, 72, 100)).ok();
            engine.process(&mut buf);
        }
    });
    // MIDI recording into a slot, then into the Arrangement.
    handle.send(Command::RecordSlot { track: id, slot: 6 });
    watched(|| {
        for i in 0..400 {
            let pitch = 50 + (i % 20) as u8;
            tx.push(MidiMessage::note_on(1, 0, pitch, 90)).ok();
            engine.process(&mut buf);
            tx.push(MidiMessage::note_off(1, 0, pitch)).ok();
        }
    });
    handle.send(Command::LaunchClip { track: id, slot: 6 });
    handle.send(Command::BackToArrangement);
    handle.send(Command::SetArrangementRecord(true));
    watched(|| {
        for i in 0..400 {
            tx.push(MidiMessage::note_on(1, 0, 60 + (i % 12) as u8, 90)).ok();
            engine.process(&mut buf);
        }
    });
    handle.send(Command::Stop);
    watched(|| {
        for _ in 0..50 {
            engine.process(&mut buf);
        }
    });
    handle.collect_garbage();
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}
