//! End-to-end tests of the plug-in host against the in-repo test plug-in
//! (crates/test-plugin), packaged here as a .clap file and a .vst3 bundle.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tessera_engine::fx::chain::FxContext;
use tessera_engine::plugin::{PluginBlock, PluginFormat, PluginNote, PluginNoteKind, PluginProcessor, PluginRef};
use tessera_plugin_host::{scan, BridgeCommand, PluginInstance, Status};

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

fn bridge() -> BridgeCommand {
    BridgeCommand::program(env!("CARGO_BIN_EXE_tessera-plugin-bridge"))
}

/// Builds the test plug-in (if needed) and packages it once per run.
fn packaged() -> &'static (PathBuf, PathBuf) {
    static DIR: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    DIR.get_or_init(|| {
        let exe = PathBuf::from(env!("CARGO_BIN_EXE_tessera-plugin-bridge"));
        let target = exe.parent().unwrap().to_path_buf();
        let mut cmd = std::process::Command::new(env!("CARGO"));
        cmd.args(["build", "-p", "tessera-test-plugin"]);
        if target.ends_with("release") {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("run cargo");
        assert!(status.success(), "building the test plug-in failed");
        let lib = target.join(if cfg!(windows) {
            "tessera_test_plugin.dll"
        } else if cfg!(target_os = "macos") {
            "libtessera_test_plugin.dylib"
        } else {
            "libtessera_test_plugin.so"
        });
        assert!(lib.exists(), "{} is missing", lib.display());

        let dir = target.join("test-plugins");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clap = dir.join("TesseraTest.clap");
        std::fs::copy(&lib, &clap).unwrap();
        let vst3 = dir.join("TesseraTest.vst3");
        let (arch_dir, bin) = if cfg!(windows) {
            ("x86_64-win", "TesseraTest.vst3")
        } else if cfg!(target_os = "macos") {
            ("MacOS", "TesseraTest")
        } else if cfg!(target_arch = "aarch64") {
            ("aarch64-linux", "TesseraTest.so")
        } else {
            ("x86_64-linux", "TesseraTest.so")
        };
        let bin_dir = vst3.join("Contents").join(arch_dir);
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::copy(&lib, bin_dir.join(bin)).unwrap();
        (clap, vst3)
    })
}

fn path(format: PluginFormat) -> &'static Path {
    let (clap, vst3) = packaged();
    match format {
        PluginFormat::Clap => clap,
        PluginFormat::Vst3 => vst3,
    }
}

fn plugin(format: PluginFormat, instrument: bool) -> PluginRef {
    let entry = scan::scan_file(&bridge(), format, path(format));
    assert!(entry.error.is_none(), "scan failed: {:?}", entry.error);
    entry
        .plugins
        .iter()
        .find(|p| p.instrument == instrument)
        .expect("plug-in in scan")
        .to_ref()
}

fn start(format: PluginFormat, instrument: bool, state: Option<&[u8]>) -> PluginInstance {
    let inst = PluginInstance::spawn(&bridge(), &plugin(format, instrument), 48_000.0, state).unwrap();
    assert!(inst.wait_ready(Duration::from_secs(20)), "{:?}", inst.status());
    inst
}

/// Runs one block of constant input; returns the first output sample, or
/// None when the plug-in did not deliver.
fn block(p: &dyn PluginProcessor, input: f32, notes: &[PluginNote]) -> Option<f32> {
    let ctx = FxContext::new(48_000.0, 120.0);
    let mut l = [input; 256];
    let mut r = [input; 256];
    let mut b = PluginBlock {
        left: &mut l,
        right: &mut r,
        notes,
        ctx: &ctx,
    };
    p.process(&mut b).then(|| l.iter().fold(0.0f32, |m, s| m.max(s.abs())))
}

const BOTH: [PluginFormat; 2] = [PluginFormat::Clap, PluginFormat::Vst3];

#[test]
fn scans_both_formats() {
    for f in BOTH {
        let entry = scan::scan_file(&bridge(), f, path(f));
        assert!(entry.error.is_none(), "{f:?}: {:?}", entry.error);
        let names: Vec<&str> = entry.plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Tessera Test Gain", "Tessera Test Sine"], "{f:?}");
        assert!(!entry.plugins[0].instrument);
        assert!(entry.plugins[1].instrument);
        assert_eq!(entry.plugins[0].vendor, "Tessera");
    }
}

#[test]
fn effect_processes_and_follows_parameters() {
    for f in BOTH {
        let inst = start(f, false, None);
        let names: Vec<String> = inst.params().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Gain", "Crash", "Stall"], "{f:?}");
        let p = inst.processor();
        assert_eq!(block(&*p, 0.5, &[]), Some(0.5), "{f:?} unity gain");
        // Automation path (engine thread).
        p.set_param(0, 1.0);
        assert_eq!(block(&*p, 0.5, &[]), Some(1.0), "{f:?} automated gain");
        // UI path (through the bridge's main thread).
        inst.set_param(0, 0.25);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut out = None;
        while Instant::now() < deadline {
            out = block(&*p, 0.5, &[]);
            if out == Some(0.25) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(out, Some(0.25), "{f:?} gain set from the UI");
        assert_eq!(inst.status(), Status::Ready);
    }
}

#[test]
fn state_round_trips() {
    for f in BOTH {
        let inst = start(f, false, None);
        inst.set_param(0, 0.125);
        // The change reaches the plug-in with the next blocks.
        let p = inst.processor();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && block(&*p, 1.0, &[]) != Some(0.25) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let state = inst.save_state(Duration::from_secs(5)).expect("state");
        drop(inst);
        let again = start(f, false, Some(&state));
        // The state is applied right after loading.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && again.params().first().map(|p| p.value) != Some(0.125) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(block(&*again.processor(), 1.0, &[]), Some(0.25), "{f:?}");
    }
}

#[test]
fn instrument_plays_notes() {
    for f in BOTH {
        let inst = start(f, true, None);
        let p = inst.processor();
        assert_eq!(block(&*p, 0.0, &[]), Some(0.0), "{f:?} silent before a note");
        let on = [PluginNote {
            frame: 0,
            kind: PluginNoteKind::On { key: 69, velocity: 100 },
        }];
        let level = block(&*p, 0.0, &on).unwrap();
        assert!(level > 0.1, "{f:?} note produced {level}");
        let off = [PluginNote {
            frame: 0,
            kind: PluginNoteKind::AllOff,
        }];
        block(&*p, 0.0, &off).unwrap();
        assert_eq!(block(&*p, 0.0, &[]), Some(0.0), "{f:?} silent after all notes off");
    }
}

#[test]
fn crash_is_contained() {
    for f in BOTH {
        let inst = start(f, false, None);
        let p = inst.processor();
        assert!(block(&*p, 0.5, &[]).is_some());
        p.set_param(1, 1.0);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && inst.status() != Status::Crashed {
            assert_eq!(block(&*p, 0.5, &[]), None, "{f:?} delivered after crashing");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(inst.status(), Status::Crashed, "{f:?}");
        assert_eq!(block(&*p, 0.5, &[]), None);
    }
}

#[test]
fn stalling_plugin_does_not_block_the_engine() {
    let inst = start(PluginFormat::Clap, false, None);
    let p = inst.processor();
    p.set_param(2, 1.0);
    let started = Instant::now();
    for _ in 0..5 {
        assert_eq!(block(&*p, 0.5, &[]), None);
    }
    // Five 256-frame blocks at 48 kHz wait at most 20 ms each.
    assert!(
        started.elapsed() < Duration::from_millis(400),
        "{:?}",
        started.elapsed()
    );
    assert!(p.late_blocks() >= 1);
}

#[test]
fn processing_does_not_allocate() {
    for f in BOTH {
        let inst = start(f, true, None);
        let p = inst.processor();
        let ctx = FxContext::new(48_000.0, 120.0).with_transport(true, 1.0, 4, false);
        let mut l = vec![0.0f32; 512];
        let mut r = vec![0.0f32; 512];
        let notes = [
            PluginNote {
                frame: 3,
                kind: PluginNoteKind::On { key: 60, velocity: 90 },
            },
            PluginNote {
                frame: 300,
                kind: PluginNoteKind::Off { key: 60 },
            },
        ];
        block(&*p, 0.0, &[]);
        WATCH.with(|w| w.set(true));
        for i in 0..50 {
            let mut b = PluginBlock {
                left: &mut l,
                right: &mut r,
                notes: if i % 2 == 0 { &notes } else { &[] },
                ctx: &ctx,
            };
            assert!(p.process(&mut b));
            p.set_param(0, (i % 10) as f32 / 10.0);
        }
        WATCH.with(|w| w.set(false));
        assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "{f:?} allocated on the audio thread");
    }
}
