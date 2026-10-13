//! The audio thread stays allocation-free with group tracks, track-to-track
//! routing, freeze, fades, the crossfader, cue output, Song Tempo
//! automation, signature changes and scene follow actions.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tessera_engine::model::{
    AudioData, CrossfadeAssign, FreezeData, InputSource, Instrument, Monitor, SceneFollow, SceneFollowAction, TrackKind,
};
use tessera_engine::timeline::{SignatureChange, TempoLane};
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
fn workflow_features_do_not_allocate() {
    let mut p = demo::demo_project();
    p.metronome = true;
    // Group the first two tracks.
    let group = p.add_track(TrackKind::Group, Instrument::None);
    let ids: Vec<u64> = p.tracks.iter().map(|t| t.id).collect();
    p.tracks[0].output = Some(group);
    p.tracks[1].output = Some(group);
    p.tracks[1].mixer.solo = true;
    p.tracks[0].crossfade = CrossfadeAssign::A;
    p.tracks[1].delay_ms = 12.0;
    // An audio track resampling track 2.
    let listener = p.add_track(TrackKind::Audio, Instrument::None);
    {
        let t = p.track_mut(listener).unwrap();
        t.input.source = InputSource::Track(ids[2]);
        t.input.monitor = Monitor::In;
    }
    // A frozen track.
    let frozen = p.add_track(TrackKind::Audio, Instrument::None);
    let data = Arc::new(AudioData {
        sample_rate: 48000,
        left: vec![0.2; 48000],
        right: vec![0.2; 48000],
    });
    {
        let t = p.track_mut(frozen).unwrap();
        t.frozen = true;
        t.freeze = Some(Arc::new(FreezeData {
            arrangement: Some(data.clone()),
            arrangement_beats: 2.0,
            slots: vec![Some((data, 2.0))],
            tempo: 120.0,
        }));
    }
    for t in p.tracks.iter_mut() {
        for a in t.arrangement.iter_mut() {
            a.fade_in = 0.5;
            a.fade_out = 0.5;
        }
    }
    let mut lane = TempoLane::default();
    lane.envelope.insert(0.0, lane.normalize(110.0));
    lane.envelope.insert(16.0, lane.normalize(140.0));
    p.tempo_lane = lane;
    p.signatures.push(SignatureChange {
        beat: 8.0,
        numerator: 7,
        denominator: 8,
    });
    for s in p.scenes.iter_mut() {
        s.follow = SceneFollow {
            enabled: true,
            action: SceneFollowAction::Any,
            beats: 2.0,
        };
    }
    p.scenes[2].signature = Some((5, 4));
    p.cue.cue_mode = true;

    let (mut engine, mut handle) = Engine::new(48000.0);
    engine.set_cue_output(true);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    let mut cue = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);

    handle.send(Command::Play);
    watched(|| {
        for _ in 0..600 {
            engine.process_with_cue(&mut buf, Some(&mut cue));
        }
    });
    handle.send(Command::LaunchScene(2));
    handle.send(Command::SetCrossfader(0.7));
    handle.send(Command::SetCue {
        mode: true,
        volume_db: -3.0,
    });
    watched(|| {
        for _ in 0..600 {
            engine.process_with_cue(&mut buf, Some(&mut cue));
        }
    });
    p.tracks[0].name = "changed".into();
    handle.load_project(&p, false);
    handle.send(Command::SetTempo(100.0));
    handle.send(Command::SetSignature(3, 4));
    watched(|| {
        for _ in 0..300 {
            engine.process(&mut buf);
        }
    });
    handle.send(Command::FollowTempoAutomation);
    handle.send(Command::StopAllClips);
    handle.send(Command::BackToArrangement);
    watched(|| {
        for _ in 0..300 {
            engine.process(&mut buf);
        }
    });
    handle.collect_garbage();
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}
