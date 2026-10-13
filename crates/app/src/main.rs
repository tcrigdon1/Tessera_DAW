//! Tessera desktop application (UI in progress).

use tessera_engine::{demo, render};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("render") {
        let out = args.get(2).cloned().unwrap_or_else(|| "tessera-render.wav".into());
        let beats: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(32.0);
        let project = demo::demo_project();
        let samples = render::render_arrangement(&project, 44100, beats);
        render::write_wav(std::path::Path::new(&out), &samples, 44100).expect("write wav");
        println!("Rendered {beats} beats to {out}");
    } else {
        eprintln!("The Tessera UI is under construction. Try: tessera render out.wav");
    }
}
