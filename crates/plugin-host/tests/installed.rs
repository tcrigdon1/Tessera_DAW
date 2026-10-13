//! Smoke tests against plug-ins installed on this computer (the DISTRHO
//! ports in /usr/lib/clap and /usr/lib/vst3 on Debian/Ubuntu). Each test
//! skips quietly when its plug-in isn't there, so CI only runs `bridge.rs`.

use std::path::Path;
use std::time::Duration;

use tessera_engine::fx::chain::FxContext;
use tessera_engine::plugin::{PluginBlock, PluginFormat, PluginNote, PluginNoteKind, PluginProcessor, PluginRef};
use tessera_plugin_host::{scan, BridgeCommand, PluginInstance, Status};

fn bridge() -> BridgeCommand {
    BridgeCommand::program(env!("CARGO_BIN_EXE_tessera-plugin-bridge"))
}

fn find(format: PluginFormat, path: &str) -> Option<PluginRef> {
    if !Path::new(path).exists() {
        eprintln!("skipping: {path} is not installed");
        return None;
    }
    let entry = scan::scan_file(&bridge(), format, Path::new(path));
    assert!(entry.error.is_none(), "scan failed: {:?}", entry.error);
    Some(entry.plugins.first().expect("one plug-in").to_ref())
}

fn render(p: &dyn PluginProcessor, notes: &[PluginNote], blocks: usize) -> f32 {
    let ctx = FxContext::new(48_000.0, 120.0).with_transport(true, 0.0, 4, false);
    let mut peak = 0.0f32;
    for b in 0..blocks {
        let mut l = vec![0.0f32; 256];
        let mut r = vec![0.0f32; 256];
        let n: &[PluginNote] = if b == 0 { notes } else { &[] };
        let mut block = PluginBlock {
            left: &mut l,
            right: &mut r,
            notes: n,
            ctx: &ctx,
        };
        assert!(p.process(&mut block), "block {b} failed");
        peak = l.iter().chain(r.iter()).fold(peak, |m, s| m.max(s.abs()));
    }
    peak
}

fn instrument(format: PluginFormat, path: &str) {
    let Some(plugin) = find(format, path) else { return };
    assert!(plugin.instrument);
    let inst = PluginInstance::spawn(&bridge(), &plugin, 48_000.0, None).unwrap();
    assert!(inst.wait_ready(Duration::from_secs(20)), "{:?}", inst.status());
    assert!(!inst.params().is_empty());
    let on = [PluginNote {
        frame: 0,
        kind: PluginNoteKind::On { key: 60, velocity: 110 },
    }];
    let peak = render(&*inst.processor(), &on, 40);
    assert!(peak > 0.001, "{} made no sound (peak {peak})", plugin.name);
    let state = inst.save_state(Duration::from_secs(5)).expect("state");
    assert!(!state.is_empty());
    assert_eq!(inst.status(), Status::Ready);
}

#[test]
fn nekobi_clap_plays() {
    instrument(PluginFormat::Clap, "/usr/lib/clap/Nekobi.clap");
}

#[test]
fn nekobi_vst3_plays() {
    instrument(PluginFormat::Vst3, "/usr/lib/vst3/Nekobi.vst3");
}

#[test]
fn kars_vst3_plays() {
    instrument(PluginFormat::Vst3, "/usr/lib/vst3/Kars.vst3");
}

fn effect(format: PluginFormat, path: &str) {
    let Some(plugin) = find(format, path) else { return };
    let inst = PluginInstance::spawn(&bridge(), &plugin, 48_000.0, None).unwrap();
    assert!(inst.wait_ready(Duration::from_secs(20)), "{:?}", inst.status());
    let p = inst.processor();
    let ctx = FxContext::new(48_000.0, 120.0);
    let mut out = 0.0f32;
    for b in 0..40 {
        let mut l = vec![if b == 0 { 1.0 } else { 0.0 }; 256];
        let mut r = l.clone();
        let mut block = PluginBlock {
            left: &mut l,
            right: &mut r,
            notes: &[],
            ctx: &ctx,
        };
        assert!(p.process(&mut block));
        if b > 2 {
            out = l.iter().fold(out, |m, s| m.max(s.abs()));
        }
    }
    assert!(out > 0.0001, "{} produced no reverb tail", plugin.name);
}

#[test]
fn mverb_clap_processes() {
    effect(PluginFormat::Clap, "/usr/lib/clap/MVerb.clap");
}

#[test]
fn dragonfly_vst3_processes() {
    effect(PluginFormat::Vst3, "/usr/lib/vst3/DragonflyRoomReverb.vst3");
}
