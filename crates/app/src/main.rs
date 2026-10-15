//! Tessera: an original DAW inspired by Ableton Live.

mod app;
mod audio;
mod automation;
mod browser;
mod chain_templates;
mod device_edit;
mod devices;
mod icon;
mod midi;
mod midi_devices;
mod midi_files;
mod plugins;
mod prefs;
mod project_files;
mod racks;
mod recording;
mod ui;
#[cfg(windows)]
mod wasapi_exclusive;
mod workflow;

use std::path::{Path, PathBuf};

use tessera_engine::{demo, export, Project};

fn usage() {
    eprintln!(
        "usage:\n  tessera [--play] [--arrangement] [project.tessera]\n  tessera render <out.wav|.aif|.flac|.mp3> [beats] [project.tessera]"
    );
}

/// Started from a shortcut or Explorer, Windows gives Tessera a console
/// window of its own; close it. Started from a terminal (`cargo run`), the
/// console is shared and stays, so log output still shows there.
#[cfg(windows)]
fn close_own_console() {
    use windows::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0u32; 2];
    // SAFETY: plain Win32 calls on a local buffer.
    unsafe {
        if GetConsoleProcessList(&mut pids) == 1 {
            let _ = FreeConsole();
        }
    }
}

fn run_render(args: &[String]) -> Result<(), String> {
    let out = args.first().ok_or("missing output path")?;
    let project = match args.get(2) {
        Some(p) => Project::load_from(Path::new(p)).map_err(|e| format!("cannot open {p}: {e}"))?,
        None => demo::demo_project(),
    };
    let beats: f64 = match args.get(1) {
        Some(b) => b.parse().map_err(|_| format!("invalid beat count {b}"))?,
        None => project.arrangement_length().max(project.beats_per_bar as f64),
    };
    let sr = 44100;
    let instances = plugins::headless_instances(&project, sr as f32);
    let samples = plugins::render_with_plugins(&project, instances, sr, beats);
    // The format follows the file extension (.wav, .aif/.aiff, .flac, .mp3).
    let ext = Path::new(out)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("wav")
        .to_ascii_lowercase();
    let format = match ext.as_str() {
        "aif" | "aiff" => export::AudioFormat::Aiff,
        "flac" => export::AudioFormat::Flac,
        "mp3" => export::AudioFormat::Mp3,
        _ => export::AudioFormat::Wav,
    };
    let settings = export::EncodeSettings {
        format,
        sample_rate: sr,
        bit_depth: export::BitDepth::Int16,
        ..Default::default()
    };
    export::encode_file(Path::new(out), &samples, &settings).map_err(|e| e.to_string())?;
    println!("Rendered {beats} beats of \"{}\" to {out}", project.name);
    Ok(())
}

fn main() {
    // MP3 export uses Tessera's own encoder.
    let _ = tessera_engine::export::MP3_ENCODER.set(tessera_mp3::encode);
    // Tessera doubles as the sandbox process for plug-ins.
    let raw: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    if raw.first().map(|a| a == "--plugin-bridge").unwrap_or(false) {
        std::process::exit(tessera_plugin_host::bridge::main(&raw[1..]));
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(|s| s.as_str()) {
        Some("render") => {
            if let Err(e) = run_render(&args[1..]) {
                eprintln!("tessera: {e}");
                usage();
                std::process::exit(1);
            }
            return;
        }
        Some("-h") | Some("--help") => {
            usage();
            return;
        }
        _ => {}
    }
    project_files::install_crash_logger();
    #[cfg(windows)]
    close_own_console();
    let play = args.iter().any(|a| a == "--play");
    let view_arrangement = args.iter().any(|a| a == "--arrangement");
    let open = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Tessera")
            .with_icon(icon::icon())
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([900.0, 560.0])
            .with_drag_and_drop(true),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    if let Err(e) = eframe::run_native(
        "Tessera",
        options,
        Box::new(move |cc| {
            let mut app = app::TesseraApp::new(cc, open);
            if view_arrangement {
                app.view = app::View::Arrangement;
            }
            if play {
                app.send(tessera_engine::Command::Play);
            }
            Ok(Box::new(app))
        }),
    ) {
        eprintln!("tessera: {e}");
        std::process::exit(1);
    }
}
