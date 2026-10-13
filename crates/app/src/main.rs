//! Tessera: an original DAW inspired by Ableton Live.

mod app;
mod audio;
mod automation;
mod browser;
mod devices;
mod plugins;
mod recording;
mod ui;

use std::path::{Path, PathBuf};

use tessera_engine::{demo, render, Project};

fn usage() {
    eprintln!(
        "usage:\n  tessera [--play] [--arrangement] [project.tessera]\n  tessera render <out.wav> [beats] [project.tessera]"
    );
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
    render::write_wav(Path::new(out), &samples, sr).map_err(|e| e.to_string())?;
    println!("Rendered {beats} beats of \"{}\" to {out}", project.name);
    Ok(())
}

fn main() {
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
    let play = args.iter().any(|a| a == "--play");
    let view_arrangement = args.iter().any(|a| a == "--arrangement");
    let open = args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Tessera")
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
