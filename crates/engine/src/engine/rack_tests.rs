//! Engine-level tests for Instrument Racks and Drum Rack pad chains.

use super::*;
use crate::dsp::drum_rack::DrumRackParams;
use crate::dsp::synth::SynthParams;
use crate::fx::rack::{MacroTarget, Rack, RackChain, RackParams, Zone};
use crate::fx::{Device, DeviceKind};
use crate::model::{Instrument, Project, TrackKind};

const SR: f32 = 48000.0;

fn setup(p: &Project) -> (Engine, EngineHandle) {
    let (mut e, mut h) = Engine::new(SR);
    h.load_project(p, true);
    let mut buf = vec![0.0; 2];
    e.process(&mut buf);
    (e, h)
}

fn run(e: &mut Engine, frames: usize) -> f32 {
    let mut buf = vec![0.0; frames * 2];
    let mut peak = 0.0f32;
    for chunk in buf.chunks_mut(1024) {
        e.process(chunk);
        peak = chunk.iter().fold(peak, |a, b| a.max(b.abs()));
    }
    peak
}

fn note(h: &mut EngineHandle, track: Id, pitch: u8, velocity: u8) {
    h.send(Command::NoteOn { track, pitch, velocity });
}

fn off(h: &mut EngineHandle, track: Id, pitch: u8) {
    h.send(Command::NoteOff { track, pitch });
}

/// An Instrument Rack with a low chain (keys 0..59) and a high chain
/// (60..127), each a Poly Synth.
fn split_rack(p: &mut Project) -> (Id, Id, Id) {
    let low = p.alloc_id();
    let high = p.alloc_id();
    let mut a = RackChain::new(low, "Low").with_instrument(Instrument::Synth(SynthParams::bass()));
    a.zones.key = Zone::range(0, 59);
    let mut b = RackChain::new(high, "High").with_instrument(Instrument::Synth(SynthParams::default()));
    b.zones.key = Zone::range(60, 127);
    let mut rack = Rack::with_chain(a);
    rack.chains.push(b);
    let track = p.add_track(TrackKind::Midi, Instrument::Rack(RackParams::default()));
    p.track_mut(track).unwrap().rack = Some(rack);
    (track, low, high)
}

#[test]
fn instrument_rack_routes_notes_by_key_zone() {
    let mut p = Project::default();
    let (track, _low, high) = split_rack(&mut p);
    let (mut e, mut h) = setup(&p);
    note(&mut h, track, 72, 110);
    assert!(run(&mut e, 4800) > 0.02, "high chain plays");
    off(&mut h, track, 72);
    run(&mut e, 48000 * 3);
    assert!(run(&mut e, 4800) < 1e-4, "released");

    // Muting the high chain silences high notes but not low ones.
    let mixer = crate::fx::rack::ChainMixer {
        mute: true,
        ..Default::default()
    };
    h.send(Command::SetChainMixer {
        bus: track,
        chain: high,
        mixer,
    });
    note(&mut h, track, 72, 110);
    assert!(run(&mut e, 4800) < 1e-4, "muted chain is silent");
    off(&mut h, track, 72);
    note(&mut h, track, 40, 110);
    assert!(run(&mut e, 4800) > 0.02, "low chain still plays");
}

#[test]
fn instrument_rack_selector_and_macros_reach_the_chains() {
    let mut p = Project::default();
    let (track, low, high) = split_rack(&mut p);
    {
        let rack = p.track_mut(track).unwrap().rack.as_mut().unwrap();
        // Both chains cover all keys; the selector picks one.
        for c in rack.chains.iter_mut() {
            c.zones.key = Zone::FULL;
        }
        rack.chains[0].zones.select = Zone::range(0, 63);
        rack.chains[1].zones.select = Zone::range(64, 127);
        // Macro 1 controls the high chain's volume.
        rack.map(0, MacroTarget::ChainVolume(high));
    }
    let _ = low;
    let mut rp = RackParams {
        chain_select: 100.0,
        ..Default::default()
    };
    // Macro at 0: high chain at -inf.
    p.track_mut(track).unwrap().instrument = Instrument::Rack(rp);
    let (mut e, mut h) = setup(&p);
    note(&mut h, track, 60, 110);
    assert!(run(&mut e, 4800) < 1e-4, "selected chain is muted by the macro");
    off(&mut h, track, 60);
    run(&mut e, 48000 * 3);
    rp.macro1 = 0.75;
    h.send(Command::SetInstrument {
        track,
        instrument: Instrument::Rack(rp),
    });
    note(&mut h, track, 60, 110);
    assert!(run(&mut e, 4800) > 0.02, "macro raised the chain volume");
}

#[test]
fn rack_chain_effects_and_state_survive_rebuilds() {
    let mut p = Project::default();
    let (track, low, _high) = split_rack(&mut p);
    let util = p.alloc_id();
    {
        let rack = p.track_mut(track).unwrap().rack.as_mut().unwrap();
        let c = rack.chain_mut(low).unwrap();
        c.devices.push(Device::new(util, DeviceKind::Utility));
    }
    let (mut e, mut h) = setup(&p);
    note(&mut h, track, 40, 110);
    let loud = run(&mut e, 4800);
    assert!(loud > 0.02);
    // Turn the utility's gain all the way down through the device command.
    h.send(Command::SetDeviceParam {
        bus: track,
        device: util,
        index: 0,
        value: -70.0,
    });
    run(&mut e, 4800);
    let quiet = run(&mut e, 4800);
    assert!(quiet < loud * 0.1, "{quiet} vs {loud}");
    // A rebuilt snapshot keeps the held note playing (voices adopted).
    h.send(Command::SetDeviceParam {
        bus: track,
        device: util,
        index: 0,
        value: 0.0,
    });
    p.track_mut(track).unwrap().rack.as_mut().unwrap().chains[0].name = "Renamed".into();
    h.load_project(&p, false);
    run(&mut e, 2048);
    assert!(run(&mut e, 4800) > 0.02, "voice survived the rebuild");
    h.collect_garbage();
}

#[test]
fn drum_rack_pads_run_through_their_chains_and_sends() {
    let mut p = Project::default();
    // No master limiter, and soft hits, so a doubled kick doesn't clip.
    p.master_devices.clear();
    let track = p.add_track(TrackKind::Midi, Instrument::DrumRack(DrumRackParams::standard()));
    let plain = setup(&p);
    let (mut e, mut h) = plain;
    note(&mut h, track, 36, 60);
    let kick = run(&mut e, 4800);
    assert!(kick > 0.05 && kick < 0.4, "{kick}");

    let mut rack = Rack::default();
    let util = p.alloc_id();
    for pad in 0..16 {
        rack.chains
            .push(RackChain::new(p.alloc_id(), format!("Pad {}", pad + 1)));
    }
    // The kick pad gets a utility turned all the way down.
    let mut d = Device::new(util, DeviceKind::Utility);
    d.params.set(0, -36.0);
    rack.chains[0].devices.push(d);
    let ret = p.alloc_id();
    rack.returns.push(RackChain::new(ret, "A"));
    let kick_chain = rack.chains[0].id;
    p.track_mut(track).unwrap().rack = Some(rack);
    let (mut e, mut h) = setup(&p);
    note(&mut h, track, 36, 60);
    let quiet = run(&mut e, 4800);
    assert!(
        quiet < kick * 0.05,
        "kick turned down by its pad chain: {quiet} vs {kick}"
    );
    run(&mut e, 48000 * 2);
    // The snare's chain is empty, so it plays normally.
    note(&mut h, track, 38, 127);
    assert!(run(&mut e, 4800) > 0.05, "snare plays");
    run(&mut e, 48000 * 2);

    // Utility back to unity: the kick plays as before.
    run(&mut e, 4800);
    h.send(Command::SetDeviceParam {
        bus: track,
        device: util,
        index: 0,
        value: 0.0,
    });
    run(&mut e, 4800);
    note(&mut h, track, 36, 60);
    let direct = run(&mut e, 4800);
    assert!((direct - kick).abs() < kick * 0.1, "{direct} vs {kick}");
    run(&mut e, 48000 * 2);
    // A send to the (empty) return adds a second copy.
    h.send(Command::SetChainSend {
        track,
        chain: kick_chain,
        index: 0,
        db: 0.0,
    });
    note(&mut h, track, 36, 60);
    let with_send = run(&mut e, 4800);
    assert!(with_send > direct * 1.5, "{with_send} vs {direct}");
    run(&mut e, 48000 * 2);
    // Pad chain fader down: nothing reaches the output or the return.
    let mixer = crate::fx::rack::ChainMixer {
        volume_db: -70.0,
        ..Default::default()
    };
    h.send(Command::SetChainMixer {
        bus: track,
        chain: kick_chain,
        mixer,
    });
    run(&mut e, 4800);
    note(&mut h, track, 36, 60);
    assert!(run(&mut e, 4800) < 1e-3);
}
