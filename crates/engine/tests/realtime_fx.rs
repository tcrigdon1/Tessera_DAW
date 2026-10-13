//! Verifies that device chains, sends/returns, input monitoring and recording
//! never allocate or free memory on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use tessera_engine::engine::input_ring;
use tessera_engine::fx::{hybrid_reverb, Device, DeviceKind};
use tessera_engine::model::{Instrument, TrackKind, MASTER_BUS};
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
fn effects_and_recording_do_not_allocate() {
    let mut project = demo::demo_project();
    // Every effect kind somewhere: on a track, on the returns and the master.
    let lead = project.tracks[3].id;
    for kind in DeviceKind::ALL {
        let id = project.alloc_id();
        project.track_mut(lead).unwrap().devices.push(Device::new(id, kind));
    }
    let id = project.alloc_id();
    project.master_devices.push(Device::new(id, DeviceKind::Eq));
    let delay_dev = project.track(lead).unwrap().devices[4].id;
    let reverb_dev = project.track(lead).unwrap().devices[DeviceKind::ALL
        .iter()
        .position(|k| *k == DeviceKind::HybridReverb)
        .unwrap()]
    .id;
    for t in &mut project.tracks {
        t.sends = vec![-6.0; project.returns.len()];
    }
    let audio = project.add_track(TrackKind::Audio, Instrument::None);
    project.track_mut(audio).unwrap().mixer.arm = true;
    project.count_in_bars = 1;

    let (mut engine, mut handle) = Engine::new(48000.0);
    let (mut tx, rx) = input_ring();
    engine.set_input(rx);
    handle.load_project(&project, true);
    let mut buf = vec![0.0f32; 512 * 2];
    let mut feed = |engine: &mut Engine, blocks: usize| {
        for _ in 0..blocks {
            for i in 0..1024 {
                let _ = tx.push(((i as f32) * 0.01).sin() * 0.3);
            }
            engine.process(&mut buf);
        }
    };
    // Warm up (applies the snapshot).
    feed(&mut engine, 2);

    handle.send(Command::LaunchScene(3));
    watched(|| feed(&mut engine, 300));

    // Parameter changes, toggles and send moves.
    handle.send(Command::SetDeviceParam {
        bus: lead,
        device: delay_dev,
        index: 0,
        value: 0.0,
    });
    handle.send(Command::SetDeviceOn {
        bus: lead,
        device: delay_dev,
        on: false,
    });
    handle.send(Command::SetDeviceOn {
        bus: lead,
        device: delay_dev,
        on: true,
    });
    handle.send(Command::SetDeviceParam {
        bus: MASTER_BUS,
        device: id,
        index: 3,
        value: 6.0,
    });
    handle.send(Command::SetSend {
        track: lead,
        index: 1,
        db: 0.0,
    });
    watched(|| feed(&mut engine, 200));

    // Hybrid Reverb IR Size rebuilds its stretched IR on the audio thread.
    for value in [1.6, 0.7, 1.0, 2.0] {
        handle.send(Command::SetDeviceParam {
            bus: lead,
            device: reverb_dev,
            index: hybrid_reverb::idx::IR_SIZE,
            value,
        });
        watched(|| feed(&mut engine, 60));
    }

    // Snapshot swap while effects have tails.
    project.tracks[0].name = "changed".into();
    handle.load_project(&project, false);
    watched(|| feed(&mut engine, 200));

    // Session recording into a slot, then stop it and launch it.
    handle.send(Command::RecordSlot { track: audio, slot: 5 });
    watched(|| feed(&mut engine, 300));
    handle.send(Command::LaunchClip { track: audio, slot: 5 });
    watched(|| feed(&mut engine, 300));

    // Arrangement recording with count-in from a stopped transport.
    handle.send(Command::Stop);
    watched(|| feed(&mut engine, 10));
    handle.send(Command::SetArrangementRecord(true));
    watched(|| feed(&mut engine, 400));
    handle.send(Command::SetArrangementRecord(false));
    handle.send(Command::Panic);
    handle.send(Command::Stop);
    watched(|| feed(&mut engine, 50));

    handle.collect_garbage();
    let mut events = 0;
    while handle.record.events.pop().is_ok() {
        events += 1;
    }
    assert!(events >= 4, "session and arrangement takes ran: {events}");
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}
