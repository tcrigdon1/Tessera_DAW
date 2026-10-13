//! Verifies that the audio thread never allocates or frees memory while
//! playing, launching clips, swapping project snapshots and handling commands.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

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
