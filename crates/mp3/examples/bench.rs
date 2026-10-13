//! Encodes three minutes of synthetic music-like stereo audio and reports the
//! encoding speed. Optionally writes the 128 kbps result to the path given as
//! the first argument.
//!
//! ```text
//! cargo run --release -p tessera-mp3 --example bench [-- out.mp3]
//! ```

use std::f64::consts::PI;
use std::time::Instant;

fn main() {
    let sr = 44100u32;
    let n = sr as usize * 180;
    let mut samples = Vec::with_capacity(2 * n);
    let mut seed = 1u32;
    let notes = [110.0, 220.0, 277.18, 329.63, 440.0, 659.25];
    for i in 0..n {
        let t = i as f64 / sr as f64;
        let beat = (t * 2.0).fract();
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = ((seed >> 8) as f64 / (1u32 << 24) as f64 - 0.5) * (1.0 - beat).powi(8) * 0.6;
        let mut l = noise;
        let mut r = noise * 0.8;
        for (k, f) in notes.iter().enumerate() {
            let s = (2.0 * PI * f * t).sin() + 0.3 * (4.0 * PI * f * t).sin();
            l += 0.06 * s * (1.0 + 0.3 * (k as f64 * 0.7 + t).sin());
            r += 0.06 * s * (1.0 - 0.3 * (k as f64 * 0.7 + t).sin());
        }
        samples.push(l as f32);
        samples.push(r as f32);
    }
    for br in [128, 192, 320] {
        let start = Instant::now();
        let mp3 = tessera_mp3::encode(&samples, sr, br).expect("encode");
        let secs = start.elapsed().as_secs_f64();
        println!(
            "{br} kbps: 180 s of stereo audio encoded in {secs:.2} s ({:.0}x real time), {} bytes",
            180.0 / secs,
            mp3.len()
        );
        if br == 128 {
            if let Some(path) = std::env::args().nth(1) {
                std::fs::write(&path, &mp3).expect("write output");
                println!("wrote {path}");
            }
        }
    }
}
