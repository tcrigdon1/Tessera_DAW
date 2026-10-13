//! Verifies that Instrument Racks, Audio Effect Racks (nested too) and Drum
//! Rack pad chains never allocate or free memory on the audio thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use tessera_engine::dsp::drum_rack::DrumRackParams;
use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::fx::rack::{ChainMixer, MacroTarget, Rack, RackChain, RackParams, Zone};
use tessera_engine::fx::{Device, DeviceKind};
use tessera_engine::model::{Clip, ClipContent, Instrument, Note, Project, TrackKind};
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
        content: ClipContent::Midi { notes },
        ..Clip::default()
    }
}

fn notes(base: u8, step: usize) -> Vec<Note> {
    (0..16)
        .map(|i| Note {
            pitch: base + (i * step % 24) as u8,
            velocity: 30 + (i * 37 % 97) as u8,
            start: i as f64 * 0.25,
            length: 0.3,
        })
        .collect()
}

#[test]
fn racks_do_not_allocate() {
    let mut p = Project::default();

    // Instrument Rack: two synth chains split by key and velocity, a delay
    // on one, macros on volume, an instrument parameter and the delay.
    let (a, b, delay) = (p.alloc_id(), p.alloc_id(), p.alloc_id());
    let mut ca = RackChain::new(a, "Low").with_instrument(Instrument::Synth(SynthParams::bass()));
    ca.zones.key = Zone::range(0, 59);
    ca.devices.push(Device::new(delay, DeviceKind::Delay));
    let mut cb = RackChain::new(b, "High").with_instrument(Instrument::Synth(SynthParams::pad()));
    cb.zones.velocity = Zone {
        lo: 20,
        hi: 127,
        fade_lo: 40,
        fade_hi: 0,
    };
    let mut rack = Rack::with_chain(ca);
    rack.chains.push(cb);
    rack.map(0, MacroTarget::ChainVolume(b));
    rack.map(1, MacroTarget::Instrument { chain: a, param: 3 });
    rack.map(
        2,
        MacroTarget::Device {
            device: delay,
            param: 6,
        },
    );
    let inst = p.add_track(TrackKind::Midi, Instrument::Rack(RackParams::default()));
    p.track_mut(inst).unwrap().rack = Some(rack);
    let c = clip(&mut p, notes(40, 7));
    p.track_mut(inst).unwrap().slots[0] = Some(c);

    // Audio Effect Rack with a nested rack, on the instrument track.
    let (fx_rack, c1, c2, inner, c3) = (p.alloc_id(), p.alloc_id(), p.alloc_id(), p.alloc_id(), p.alloc_id());
    let mut chain1 = RackChain::new(c1, "Dry");
    chain1.devices.push(Device::new(p.alloc_id(), DeviceKind::Compressor));
    let mut chain3 = RackChain::new(c3, "Inner");
    chain3.devices.push(Device::new(p.alloc_id(), DeviceKind::Reverb));
    let mut chain2 = RackChain::new(c2, "Wet");
    chain2.devices.push(Device::new_rack(inner, Rack::with_chain(chain3)));
    chain2.zones.select = Zone::range(64, 127);
    let mut fxr = Rack::with_chain(chain1);
    fxr.chains.push(chain2);
    p.track_mut(inst).unwrap().devices.push(Device::new_rack(fx_rack, fxr));

    // Drum Rack with pad chains, sends and two returns.
    let drums = p.add_track(TrackKind::Midi, Instrument::DrumRack(DrumRackParams::standard()));
    let mut dr = Rack::default();
    for pad in 0..16 {
        let mut c = RackChain::new(p.alloc_id(), format!("Pad {}", pad + 1));
        if pad % 3 == 0 {
            c.devices.push(Device::new(p.alloc_id(), DeviceKind::Saturator));
        }
        c.sends = vec![-12.0, if pad % 2 == 0 { 0.0 } else { -70.0 }];
        dr.chains.push(c);
    }
    let mut r1 = RackChain::new(p.alloc_id(), "A");
    r1.devices.push(Device::new(p.alloc_id(), DeviceKind::Reverb));
    dr.returns.push(r1);
    dr.returns.push(RackChain::new(p.alloc_id(), "B"));
    let pad0 = dr.chains[0].id;
    p.track_mut(drums).unwrap().rack = Some(dr);
    let c = clip(&mut p, notes(36, 1));
    p.track_mut(drums).unwrap().slots[0] = Some(c);

    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    let mut peak = 0.0f32;
    let mut feed = |engine: &mut Engine, blocks: usize| {
        for _ in 0..blocks {
            engine.process(&mut buf);
        }
        buf.iter().fold(0.0f32, |a, b| a.max(b.abs()))
    };
    feed(&mut engine, 2);
    handle.send(Command::LaunchScene(0));
    peak = peak.max(watched(|| feed(&mut engine, 400)));

    // Macros, the selector, chain mixers and zones, sends and device
    // parameters inside the racks.
    for v in [0.2f32, 0.9, 0.5] {
        let rp = RackParams {
            macro1: v,
            macro2: 1.0 - v,
            macro3: v,
            chain_select: v * 127.0,
            ..Default::default()
        };
        handle.send(Command::SetInstrument {
            track: inst,
            instrument: Instrument::Rack(rp),
        });
        handle.send(Command::SetDeviceParam {
            bus: inst,
            device: fx_rack,
            index: 8,
            value: v * 127.0,
        });
        handle.send(Command::SetDeviceParam {
            bus: inst,
            device: delay,
            index: 1,
            value: 0.5,
        });
        handle.send(Command::SetChainMixer {
            bus: inst,
            chain: c3,
            mixer: ChainMixer {
                volume_db: -6.0 * v,
                ..Default::default()
            },
        });
        handle.send(Command::SetChainMixer {
            bus: drums,
            chain: pad0,
            mixer: ChainMixer {
                solo: v > 0.8,
                ..Default::default()
            },
        });
        handle.send(Command::SetChainSend {
            track: drums,
            chain: pad0,
            index: 1,
            db: -6.0,
        });
        peak = peak.max(watched(|| feed(&mut engine, 100)));
    }

    // Snapshot swaps keep voices and tails without allocating.
    p.tracks[0].name = "changed".into();
    handle.load_project(&p, false);
    peak = peak.max(watched(|| feed(&mut engine, 200)));
    handle.send(Command::Panic);
    watched(|| feed(&mut engine, 20));

    handle.collect_garbage();
    assert!(peak > 0.05, "racks played: {peak}");
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}
