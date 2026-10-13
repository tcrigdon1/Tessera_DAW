//! Plug-in slots in the engine: instruments get their notes, effects their
//! parameter changes, exports can use their own processors, and none of it
//! allocates on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use tessera_engine::fx::{Device, DeviceKind};
use tessera_engine::model::{Instrument, TrackKind};
use tessera_engine::plugin::{self, PluginBlock, PluginInstrument, PluginNoteKind, PluginProcessor};
use tessera_engine::{demo, render, Command, Engine};

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

/// Outputs a constant while any note is held.
#[derive(Default)]
struct FakeSynth {
    held: AtomicUsize,
    note_ons: AtomicUsize,
}

impl PluginProcessor for FakeSynth {
    fn process(&self, b: &mut PluginBlock) -> bool {
        for n in b.notes {
            match n.kind {
                PluginNoteKind::On { .. } => {
                    self.held.fetch_add(1, Ordering::SeqCst);
                    self.note_ons.fetch_add(1, Ordering::SeqCst);
                }
                PluginNoteKind::Off { .. } => {
                    let _ = self
                        .held
                        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |h| h.checked_sub(1));
                }
                PluginNoteKind::AllOff => self.held.store(0, Ordering::SeqCst),
            }
        }
        let v = if self.held.load(Ordering::SeqCst) > 0 {
            0.25
        } else {
            0.0
        };
        b.left.fill(v);
        b.right.fill(v);
        true
    }
    fn set_param(&self, _index: u32, _value: f32) {}
}

/// Multiplies by its parameter 0.
struct FakeGain(AtomicU32);

impl PluginProcessor for FakeGain {
    fn process(&self, b: &mut PluginBlock) -> bool {
        let g = f32::from_bits(self.0.load(Ordering::SeqCst));
        for s in b.left.iter_mut().chain(b.right.iter_mut()) {
            *s *= g;
        }
        true
    }
    fn set_param(&self, index: u32, value: f32) {
        if index == 0 {
            self.0.store(value.to_bits(), Ordering::SeqCst);
        }
    }
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn plugin_slots_play_and_do_not_allocate() {
    let mut project = demo::demo_project();
    for t in &mut project.tracks {
        t.mixer.mute = true;
    }
    project.returns.clear();
    for t in &mut project.tracks {
        t.sends.clear();
    }
    let track = project.add_track(TrackKind::Midi, Instrument::Plugin(PluginInstrument::default()));
    let fx = project.alloc_id();
    project
        .track_mut(track)
        .unwrap()
        .devices
        .push(Device::new(fx, DeviceKind::Plugin));
    assert_eq!(project.plugin_slots_in_use(), vec![fx, track]);

    let synth = Arc::new(FakeSynth::default());
    let gain = Arc::new(FakeGain(AtomicU32::new(1.0f32.to_bits())));
    plugin::register(track, synth.clone());
    plugin::register(fx, gain.clone());

    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&project, true);
    let mut buf = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);

    let watch = |engine: &mut Engine, buf: &mut [f32], blocks: usize| {
        WATCH.with(|w| w.set(true));
        for _ in 0..blocks {
            engine.process(buf);
        }
        WATCH.with(|w| w.set(false));
    };

    handle.send(Command::NoteOn {
        track,
        pitch: 60,
        velocity: 100,
    });
    watch(&mut engine, &mut buf, 20);
    assert_eq!(synth.note_ons.load(Ordering::SeqCst), 1);
    let loud = peak(&buf);
    assert!(loud > 0.05, "instrument plug-in reached the output ({loud})");

    handle.send(Command::SetDeviceParam {
        bus: track,
        device: fx,
        index: 0,
        value: 0.0,
    });
    watch(&mut engine, &mut buf, 20);
    assert!(peak(&buf) < 1e-6, "effect plug-in got its parameter");

    handle.send(Command::Panic);
    watch(&mut engine, &mut buf, 5);
    assert_eq!(synth.held.load(Ordering::SeqCst), 0, "panic releases plug-in notes");
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");

    plugin::unregister(track);
    plugin::unregister(fx);
}

#[test]
fn exports_can_use_their_own_processors() {
    let mut project = demo::demo_project();
    // Tests share the registry; keep this slot apart from the other test's.
    project.next_id += 10_000;
    let master = project.alloc_id();
    project.master_devices.push(Device::new(master, DeviceKind::Plugin));
    let live = Arc::new(FakeGain(AtomicU32::new(1.0f32.to_bits())));
    plugin::register(master, live);
    let silent: Arc<dyn PluginProcessor> = Arc::new(FakeGain(AtomicU32::new(0.0f32.to_bits())));
    let out = plugin::with_processors(HashMap::from([(master, silent)]), || {
        render::render_arrangement(&project, 22050, 4.0)
    });
    assert!(peak(&out) < 1e-6, "export used the override");
    assert!(plugin::lookup(master).is_some(), "registry untouched");
    plugin::unregister(master);
}

#[test]
fn automation_lanes_drive_plugin_parameters() {
    use tessera_engine::automation::{AutomationLane, AutomationTarget};
    let mut project = demo::demo_project();
    project.next_id += 20_000;
    for t in &mut project.tracks {
        t.mixer.mute = true;
    }
    let track = project.add_track(TrackKind::Midi, Instrument::Plugin(PluginInstrument::default()));
    let fx = project.alloc_id();
    let t = project.track_mut(track).unwrap();
    t.devices.push(Device::new(fx, DeviceKind::Plugin));
    let mut lane = AutomationLane::new(AutomationTarget::Device { device: fx, param: 0 });
    lane.envelope.insert(0.0, 0.25);
    t.automation.push(lane);

    let synth = Arc::new(FakeSynth::default());
    let gain = Arc::new(FakeGain(AtomicU32::new(1.0f32.to_bits())));
    plugin::register(track, synth.clone());
    plugin::register(fx, gain.clone());
    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&project, true);
    let mut buf = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);
    handle.send(Command::Play);
    WATCH.with(|w| w.set(true));
    for _ in 0..10 {
        engine.process(&mut buf);
    }
    WATCH.with(|w| w.set(false));
    assert_eq!(f32::from_bits(gain.0.load(Ordering::SeqCst)), 0.25);
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
    plugin::unregister(track);
    plugin::unregister(fx);
}
