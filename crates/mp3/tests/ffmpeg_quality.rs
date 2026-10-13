//! End-to-end quality checks against an independent decoder (ffmpeg).
//!
//! These tests are ignored by default because they need `ffmpeg` on PATH:
//!
//! ```text
//! cargo test -p tessera-mp3 --release -- --ignored --nocapture
//! ```

use std::f64::consts::PI;
use std::process::Command;

use tessera_mp3::{encode, encode_testing_tables, frame_count, DECODER_DELAY, FRAME_SAMPLES};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

fn sine(n: usize, sr: u32, freq: f64, amp: f64) -> Vec<f32> {
    (0..n)
        .flat_map(|i| {
            let v = (amp * (2.0 * PI * freq * i as f64 / sr as f64).sin()) as f32;
            [v, v]
        })
        .collect()
}

fn chord(n: usize, sr: u32) -> Vec<f32> {
    // A minor seventh voicing with a few harmonics, slightly different per channel.
    let notes = [220.0, 261.63, 329.63, 392.0, 880.0, 1318.5];
    (0..n)
        .flat_map(|i| {
            let t = i as f64 / sr as f64;
            let mut l = 0.0;
            let mut r = 0.0;
            for (k, f) in notes.iter().enumerate() {
                for h in 1..=4 {
                    let a = 0.08 / h as f64;
                    let s = (2.0 * PI * f * h as f64 * t + k as f64).sin();
                    l += a * s * if k % 2 == 0 { 1.0 } else { 0.6 };
                    r += a * s * if k % 2 == 0 { 0.6 } else { 1.0 };
                }
            }
            [l as f32, r as f32]
        })
        .collect()
}

fn noise(n: usize, amp: f64, seed: u64) -> Vec<f32> {
    let mut rng = Rng(seed);
    (0..2 * n).map(|_| (amp * rng.next()) as f32).collect()
}

/// Decodes MP3 bytes with ffmpeg; returns interleaved stereo f32 and stderr.
fn ffmpeg_decode(mp3: &[u8], name: &str) -> (Vec<f32>, String) {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let path = dir.join(format!("tessera-mp3-{}-{name}.mp3", std::process::id()));
    std::fs::write(&path, mp3).unwrap();
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&path)
        .args(["-f", "f32le", "-ac", "2", "-"])
        .output()
        .expect("ffmpeg must be installed for this test");
    let pcm = out
        .stdout
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    (pcm, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// SNR (dB) of `dec` against `input` (both interleaved stereo) over input
/// frames `range` at a given lag.
fn snr_at(input: &[f32], dec: &[f32], range: std::ops::Range<usize>, lag: usize) -> f64 {
    let (mut sig, mut err) = (0.0f64, 0.0f64);
    for i in range {
        if 2 * (i + lag) + 1 >= dec.len() {
            break;
        }
        for c in 0..2 {
            let x = input[2 * i + c] as f64;
            let y = dec[2 * (i + lag) + c] as f64;
            sig += x * x;
            err += (x - y) * (x - y);
        }
    }
    10.0 * (sig / err.max(1e-30)).log10()
}

/// SNR (dB) of `dec` against `input` over input frames `range`, at the best
/// lag (the encoder/decoder delay) in `0..2 * FRAME_SAMPLES`.
fn snr_db(input: &[f32], dec: &[f32], range: std::ops::Range<usize>) -> (f64, usize) {
    let mut best = (f64::MIN, 0);
    for lag in 0..2 * FRAME_SAMPLES {
        let snr = snr_at(input, dec, range.clone(), lag);
        if snr > best.0 {
            best = (snr, lag);
        }
    }
    best
}

/// Frequency estimate of the left channel from zero crossings.
fn zero_crossing_freq(dec: &[f32], range: std::ops::Range<usize>, sr: u32) -> f64 {
    let left: Vec<f32> = range.clone().map(|i| dec[2 * i]).collect();
    let crossings: Vec<f64> = left
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + (-w[0] / (w[1] - w[0])) as f64)
        .collect();
    let periods = (crossings.len() - 1) as f64;
    periods / ((crossings[crossings.len() - 1] - crossings[0]) / sr as f64)
}

#[test]
#[ignore = "requires ffmpeg"]
fn ffmpeg_decodes_sine_chord_and_noise() {
    for &sr in &[44100u32, 48000, 32000] {
        let sec = sr as usize;
        // 2 s sine, 2 s chord, 2 s white noise.
        let mut input = sine(2 * sec, sr, 1000.0, 0.5);
        input.extend(chord(2 * sec, sr));
        input.extend(noise(2 * sec, 0.3, 99));
        let n = input.len() / 2;
        for &br in &[128u32, 160, 192, 256, 320] {
            let mp3 = encode(&input, sr, br).unwrap();
            let (dec, err) = ffmpeg_decode(&mp3, &format!("mix-{sr}-{br}"));
            assert!(err.trim().is_empty(), "ffmpeg reported errors: {err}");
            let frames = frame_count(n);
            assert_eq!(dec.len() / 2, frames * FRAME_SAMPLES, "decoded length");
            assert!((dec.len() / 2).abs_diff(n + DECODER_DELAY) < FRAME_SAMPLES);
            // A pure sine is periodic, so find the lag on the non-periodic
            // material (sine-to-chord transition and noise) first.
            let (_, lag) = snr_db(&input, &dec, 2 * sec - sec / 4..2 * sec + sec / 4);
            let sine_range = sec / 4..2 * sec - sec / 4;
            let s_snr = snr_at(&input, &dec, sine_range.clone(), lag);
            let freq = zero_crossing_freq(&dec, sine_range.start + lag..sine_range.end + lag, sr);
            let c_snr = snr_at(&input, &dec, 2 * sec + sec / 4..4 * sec - sec / 4, lag);
            let n_snr = snr_at(&input, &dec, 4 * sec + sec / 4..6 * sec - sec / 4, lag);
            println!(
                "{sr} Hz {br:3} kbps: sine SNR {s_snr:5.1} dB (lag {lag}, {freq:.2} Hz)  chord SNR {c_snr:5.1} dB  noise SNR {n_snr:5.1} dB"
            );
            assert_eq!(lag, DECODER_DELAY);
            assert!((freq - 1000.0).abs() < 0.5, "sine frequency {freq}");
            assert!(s_snr > 20.0, "sine SNR {s_snr:.1} dB at {br} kbps");
            assert!(c_snr > 12.0, "chord SNR {c_snr:.1} dB at {br} kbps");
        }
    }
}

#[test]
#[ignore = "requires ffmpeg"]
fn ffmpeg_sine_sweep_across_subband_edges() {
    // Tones at and between subband edges exercise the filterbank's alias
    // cancellation against the decoder's synthesis window.
    let sr = 44100;
    let mut worst = f64::MAX;
    for k in [1.0, 1.5, 2.0, 3.0, 4.25, 8.0, 12.5, 20.0, 23.75] {
        let freq = k * sr as f64 / 64.0;
        let input = sine(sr as usize, sr, freq, 0.4);
        let mp3 = encode(&input, sr, 320).unwrap();
        let (dec, err) = ffmpeg_decode(&mp3, &format!("sweep-{k}"));
        assert!(err.trim().is_empty(), "ffmpeg reported errors: {err}");
        let (snr, lag) = snr_db(&input, &dec, 4000..40000);
        println!("{freq:8.1} Hz: SNR {snr:5.1} dB (lag {lag})");
        worst = worst.min(snr);
    }
    assert!(worst > 45.0, "worst SNR {worst:.1} dB");
}

#[test]
#[ignore = "requires ffmpeg"]
fn ffmpeg_every_huffman_table_decodes() {
    // Broadband material at rising levels plus loud tones (escape values), so
    // every table sees many different codewords. Forcing a table wherever it
    // can represent a region changes only the Huffman coding, never the
    // quantised values, so the decoded audio must be bit-identical.
    let sr = 44100;
    let sec = sr as usize;
    let mut input = noise(sec, 0.3, 7);
    let tone = chord(sec, sr);
    let loud = sine(sec, sr, 3000.0, 0.6);
    for (i, v) in input.iter_mut().enumerate() {
        let ramp = i as f32 / (2 * sec) as f32;
        *v = *v * ramp * ramp + tone[i] + if i > sec { loud[i] } else { 0.0 };
    }
    for &quant in &[128u32, 160] {
        let reference = encode_testing_tables(&input, sr, 320, quant, None).unwrap();
        let (ref_dec, err) = ffmpeg_decode(&reference, "tables-ref");
        assert!(err.trim().is_empty(), "ffmpeg reported errors: {err}");
        let (ref_snr, _) = snr_db(&input, &ref_dec, 2000..sec - 2000);
        for t in (1..=31).filter(|t| *t != 4 && *t != 14) {
            let mp3 = encode_testing_tables(&input, sr, 320, quant, Some(t)).unwrap();
            assert_ne!(mp3, reference, "table {t} was never used");
            let (dec, err) = ffmpeg_decode(&mp3, &format!("table-{t}"));
            assert!(err.trim().is_empty(), "table {t}: ffmpeg reported errors: {err}");
            assert_eq!(dec.len(), ref_dec.len());
            let diff = dec
                .iter()
                .zip(ref_dec.iter())
                .map(|(a, b)| (a - b).abs())
                .fold(0.0f32, f32::max);
            println!("quant {quant} kbps, table {t:2}: max deviation {diff:e} (reference SNR {ref_snr:.1} dB)");
            assert!(diff < 1e-6, "table {t} decodes differently (max deviation {diff})");
        }
    }
}
