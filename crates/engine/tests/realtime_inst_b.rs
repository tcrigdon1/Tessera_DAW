//! Verifies that the Mallet Synth and the Granular Synth never allocate or
//! free on the audio thread, in every resonator type and grain mode, and that
//! they produce sound.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tessera_engine::dsp::granular::GranularParams;
use tessera_engine::dsp::mallet::{MalletParams, RES_TYPES};
use tessera_engine::model::{AudioData, Clip, ClipContent, Instrument, Note, Project, SampleRef, TrackKind};
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

fn sample(freq: f32, frames: usize) -> Option<SampleRef> {
    let v: Vec<f32> = (0..frames)
        .map(|i| (i as f32 * freq / 44100.0 * std::f32::consts::TAU).sin() * 0.5)
        .collect();
    Some(SampleRef {
        path: format!("{freq}.wav"),
        data: Some(Arc::new(AudioData {
            sample_rate: 44100,
            right: v.clone(),
            left: v,
        })),
    })
}

fn clip(p: &mut Project, notes: Vec<Note>) -> Clip {
    Clip {
        id: p.alloc_id(),
        name: "c".into(),
        color: [0, 0, 0],
        length: 4.0,
        looping: true,
        gain_db: 0.0,
        envelopes: Default::default(),
        content: ClipContent::Midi { notes },
    }
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |a, b| a.max(b.abs()))
}

#[test]
fn mallet_and_granular_do_not_allocate() {
    let mut p = Project::default();
    let mallet = p.add_track(TrackKind::Midi, Instrument::Mallet(MalletParams::default()));
    let granular = p.add_track(TrackKind::Midi, Instrument::Granular(GranularParams::default()));
    p.track_mut(granular).unwrap().set_sample(0, sample(330.0, 44100));
    let melody: Vec<Note> = (0..16)
        .map(|i| Note {
            pitch: 48 + (i * 7 % 24) as u8,
            velocity: 100,
            start: i as f64 * 0.25,
            length: 0.6,
        })
        .collect();
    for t in [mallet, granular] {
        let c = clip(&mut p, melody.clone());
        p.track_mut(t).unwrap().slots[0] = Some(c);
    }

    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);
    handle.send(Command::LaunchScene(0));

    let mut loudest = 0.0f32;
    watched(|| {
        for _ in 0..300 {
            engine.process(&mut buf);
            loudest = loudest.max(peak(&buf));
        }
    });
    assert!(loudest > 0.01, "no output: {loudest}");

    // Every resonator type in both structures, with the noise exciter.
    for kind in 0..RES_TYPES.len() {
        let params = MalletParams {
            a_type: kind as f32,
            b_on: 1.0,
            b_type: ((kind + 3) % RES_TYPES.len()) as f32,
            structure: (kind % 2) as f32,
            noise_on: 1.0,
            noise_sustain: 0.5,
            lfo_pitch: 0.5,
            lfo_decay: 0.5,
            ..MalletParams::default()
        };
        handle.send(Command::SetInstrument {
            track: mallet,
            instrument: Instrument::Mallet(params),
        });
        watched(|| {
            for _ in 0..60 {
                engine.process(&mut buf);
            }
        });
    }

    // Every grain mode, window and filter.
    for mode in 0..3 {
        let params = GranularParams {
            mode: mode as f32,
            shape: mode as f32,
            filter_type: (mode + 1) as f32,
            density: 150.0,
            size: 400.0,
            reverse: 0.5,
            rand_pitch: 2.0,
            scan: 1.0,
            lfo_pos: 0.5,
            lfo_size: 0.5,
            lfo_pitch: 1.0,
            ..GranularParams::default()
        };
        handle.send(Command::SetInstrument {
            track: granular,
            instrument: Instrument::Granular(params),
        });
        watched(|| {
            for _ in 0..80 {
                engine.process(&mut buf);
            }
        });
    }

    // Swap the sample, then remove it.
    p.track_mut(granular).unwrap().set_sample(0, sample(550.0, 3000));
    handle.load_project(&p, false);
    watched(|| {
        for _ in 0..100 {
            engine.process(&mut buf);
        }
    });
    p.track_mut(granular).unwrap().set_sample(0, None);
    handle.load_project(&p, false);
    watched(|| {
        for _ in 0..100 {
            engine.process(&mut buf);
        }
    });
    handle.send(Command::Panic);
    watched(|| {
        for _ in 0..50 {
            engine.process(&mut buf);
        }
    });
    handle.collect_garbage();
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}

#[test]
fn granular_sounds_with_a_sample_on_its_track() {
    let mut p = Project::default();
    let granular = p.add_track(TrackKind::Midi, Instrument::Granular(GranularParams::default()));
    p.track_mut(granular).unwrap().set_sample(0, sample(220.0, 44100));
    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);
    handle.send(Command::NoteOn {
        track: granular,
        pitch: 60,
        velocity: 110,
    });
    let mut loudest = 0.0f32;
    for _ in 0..100 {
        engine.process(&mut buf);
        loudest = loudest.max(peak(&buf));
    }
    assert!(loudest > 0.05, "granular silent: {loudest}");
}
