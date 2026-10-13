//! Verifies that the Sampler, Drum Rack and browser preview never allocate
//! or free on the audio thread, including while samples are swapped in new
//! project snapshots.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tessera_engine::dsp::drum_rack::{DrumRackParams, PadSource};
use tessera_engine::dsp::sampler::{PlayMode, SamplerParams};
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

fn tone(freq: f32, frames: usize) -> Arc<AudioData> {
    let v: Vec<f32> = (0..frames)
        .map(|i| (i as f32 * freq / 48000.0 * std::f32::consts::TAU).sin() * 0.5)
        .collect();
    Arc::new(AudioData {
        sample_rate: 44100,
        right: v.clone(),
        left: v,
    })
}

fn sample(freq: f32, frames: usize) -> Option<SampleRef> {
    Some(SampleRef {
        path: format!("{freq}.wav"),
        data: Some(tone(freq, frames)),
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

#[test]
fn sample_instruments_do_not_allocate() {
    let mut p = Project::default();
    let sampler = p.add_track(TrackKind::Midi, Instrument::Sampler(SamplerParams::default()));
    let mut kit = DrumRackParams::standard();
    kit.pads[2].source = PadSource::Sample;
    kit.pads[3].source = PadSource::Sample;
    kit.pads[3].reverse = true;
    let rack = p.add_track(TrackKind::Midi, Instrument::DrumRack(kit));
    p.track_mut(sampler).unwrap().set_sample(0, sample(330.0, 30000));
    p.track_mut(rack).unwrap().set_sample(2, sample(800.0, 8000));
    p.track_mut(rack).unwrap().set_sample(3, sample(200.0, 8000));
    let melody: Vec<Note> = (0..16)
        .map(|i| Note {
            pitch: 48 + (i * 5 % 24) as u8,
            velocity: 100,
            start: i as f64 * 0.25,
            length: 0.4,
        })
        .collect();
    let beat: Vec<Note> = (0..16)
        .map(|i| Note {
            pitch: 36 + (i % 16) as u8,
            velocity: 110,
            start: i as f64 * 0.25,
            length: 0.1,
        })
        .collect();
    let c = clip(&mut p, melody);
    p.track_mut(sampler).unwrap().slots[0] = Some(c);
    let c = clip(&mut p, beat);
    p.track_mut(rack).unwrap().slots[0] = Some(c);

    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);
    let preview = tone(500.0, 20000);

    handle.send(Command::LaunchScene(0));
    handle.send(Command::PreviewSample(preview.clone()));
    watched(|| {
        for _ in 0..300 {
            engine.process(&mut buf);
        }
    });

    // Change the instrument settings live (all modes) and swap samples.
    for mode in PlayMode::ALL {
        let params = SamplerParams {
            mode,
            loop_on: true,
            reverse: mode == PlayMode::Slice,
            filter_on: true,
            voices: 3,
            ..SamplerParams::default()
        };
        handle.send(Command::SetInstrument {
            track: sampler,
            instrument: Instrument::Sampler(params),
        });
        watched(|| {
            for _ in 0..150 {
                engine.process(&mut buf);
            }
        });
    }
    p.track_mut(sampler).unwrap().set_sample(0, sample(660.0, 5000));
    p.track_mut(rack).unwrap().set_sample(2, None);
    handle.load_project(&p, false);
    handle.send(Command::PreviewSample(tone(250.0, 4000)));
    handle.send(Command::NoteOn {
        track: rack,
        pitch: 39,
        velocity: 127,
    });
    watched(|| {
        for _ in 0..300 {
            engine.process(&mut buf);
        }
    });
    handle.send(Command::StopPreview);
    handle.send(Command::Panic);
    watched(|| {
        for _ in 0..100 {
            engine.process(&mut buf);
        }
    });
    handle.collect_garbage();
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}
