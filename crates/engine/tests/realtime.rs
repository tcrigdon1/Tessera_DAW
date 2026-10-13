//! Verifies that the audio thread never allocates or frees memory while
//! playing, launching clips, swapping project snapshots and handling commands.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use std::sync::Arc;

use tessera_engine::automation::{instrument_params, lane_mut, AutomationTarget};
use tessera_engine::dsp::{
    drum_rack::DrumRackParams, fm::FmParams, sampler::SamplerParams, wavetable::WavetableParams,
};
use tessera_engine::model::{
    ArrangementClip, AudioData, AudioRef, Clip, ClipContent, Instrument, Note, Project, TrackKind,
};
use tessera_engine::warp::{WarpMode, WarpSettings};
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
fn audio_thread_does_not_allocate() {
    let mut project = demo::demo_project();
    project.metronome = true;
    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&project, true);
    let mut buf = vec![0.0f32; 512 * 2];
    let ids: Vec<u64> = project.tracks.iter().map(|t| t.id).collect();

    // Warm up: first process applies the snapshot.
    engine.process(&mut buf);

    handle.send(Command::Play);
    handle.send(Command::SetLoop {
        enabled: true,
        start: 0.0,
        length: 8.0,
    });
    watched(|| {
        for _ in 0..400 {
            engine.process(&mut buf);
        }
    });
    handle.send(Command::LaunchScene(3));
    handle.send(Command::NoteOn {
        track: ids[3],
        pitch: 70,
        velocity: 100,
    });
    watched(|| {
        for _ in 0..400 {
            engine.process(&mut buf);
        }
    });
    // Snapshot swap while playing.
    project.tracks[0].name = "changed".into();
    handle.load_project(&project, false);
    handle.send(Command::StopTrack(ids[1]));
    handle.send(Command::SetTempo(140.0));
    watched(|| {
        for _ in 0..400 {
            engine.process(&mut buf);
        }
    });
    handle.send(Command::BackToArrangement);
    handle.send(Command::Stop);
    watched(|| {
        for _ in 0..100 {
            engine.process(&mut buf);
        }
    });
    handle.collect_garbage();
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}

/// The demo song plus an audio track with a clip in every warp mode, and
/// automation on the mixer and instruments.
fn automated_project() -> Project {
    let mut p = demo::demo_project();
    // Every parameter of the newer instruments swept by lanes, under notes.
    let instruments = [
        Instrument::Sampler(SamplerParams::default()),
        Instrument::DrumRack(DrumRackParams::default()),
        Instrument::Fm(FmParams::default()),
        Instrument::Wavetable(WavetableParams::default()),
    ];
    for instrument in instruments {
        let id = p.add_track(TrackKind::Midi, instrument);
        let notes = (0..8)
            .map(|k| Note {
                pitch: 36 + k * 3,
                velocity: 100,
                start: k as f64 * 0.5,
                length: 0.4,
            })
            .collect();
        let clip = Clip {
            id: p.alloc_id(),
            length: 4.0,
            content: ClipContent::Midi { notes },
            ..Default::default()
        };
        let arr = ArrangementClip {
            id: p.alloc_id(),
            start: 0.0,
            length: 8.0,
            clip,
            ..Default::default()
        };
        let t = p.track_mut(id).unwrap();
        t.arrangement.push(arr);
        for idx in 0..instrument_params(&instrument).len() as u16 {
            let env = &mut lane_mut(&mut t.automation, AutomationTarget::Instrument(idx)).envelope;
            env.insert(0.0, 0.2);
            env.insert(4.0, 0.9);
        }
    }
    let audio = p.add_track(TrackKind::Audio, Instrument::None);
    let data = Arc::new(AudioData {
        sample_rate: 44100,
        left: (0..88200).map(|i| ((i as f32) * 0.03).sin() * 0.3).collect(),
        right: (0..88200).map(|i| ((i as f32) * 0.02).sin() * 0.3).collect(),
    });
    let modes = [WarpMode::Beats, WarpMode::Tones, WarpMode::Complex, WarpMode::RePitch];
    for (scene, mode) in modes.into_iter().enumerate() {
        let mut a = AudioRef::new("loop.wav", Some(data.clone()));
        a.warp = WarpSettings {
            enabled: true,
            mode,
            bpm: 100.0,
            transpose: if scene == 1 { 3.0 } else { 0.0 },
            transients: vec![0.0, 0.3, 0.6, 0.9, 1.2],
            ..Default::default()
        };
        a.warp.add_marker(0.0);
        a.warp.add_marker(2.0);
        a.warp.move_marker(1, 2.5);
        let mut clip = Clip {
            id: p.alloc_id(),
            length: 4.0,
            content: ClipContent::Audio(a),
            ..Default::default()
        };
        let env = &mut lane_mut(&mut clip.envelopes, AutomationTarget::Pan).envelope;
        env.insert(0.0, 0.0);
        env.insert(4.0, 1.0);
        p.track_mut(audio).unwrap().slots[scene] = Some(clip);
    }
    for t in p.tracks.iter_mut() {
        let vol = &mut lane_mut(&mut t.automation, AutomationTarget::Volume).envelope;
        vol.insert(0.0, 0.5);
        vol.insert(6.0, 0.9);
        if !matches!(t.instrument, Instrument::None) {
            let env = &mut lane_mut(&mut t.automation, AutomationTarget::Instrument(1)).envelope;
            env.insert(0.0, 0.1);
            env.insert(3.0, 0.8);
        }
    }
    p
}

#[test]
fn automation_and_warping_do_not_allocate() {
    let mut project = automated_project();
    let audio = project.tracks.last().unwrap().id;
    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&project, true);
    let mut buf = vec![0.0f32; 256 * 2];
    engine.process(&mut buf);

    handle.send(Command::SetLoop {
        enabled: true,
        start: 0.0,
        length: 4.0,
    });
    handle.send(Command::Play);
    watched(|| {
        for _ in 0..600 {
            engine.process(&mut buf);
        }
    });
    for scene in 0..4 {
        handle.send(Command::LaunchClip {
            track: audio,
            slot: scene,
        });
        handle.send(Command::SetTempo(90.0 + scene as f64 * 20.0));
        watched(|| {
            for _ in 0..500 {
                engine.process(&mut buf);
            }
        });
    }
    handle.send(Command::LaunchScene(2));
    watched(|| {
        for _ in 0..500 {
            engine.process(&mut buf);
        }
    });
    // Snapshot swap with changed automation while playing.
    project.tracks[0].automation[0].overridden = true;
    handle.load_project(&project, false);
    handle.send(Command::BackToArrangement);
    handle.send(Command::SetPosition(2.0));
    watched(|| {
        for _ in 0..300 {
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
