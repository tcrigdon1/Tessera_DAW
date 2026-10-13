//! Offline rendering (export) without an audio device.

use std::path::Path;

use crate::engine::{Command, Engine};
use crate::model::Project;

/// Renders the Arrangement from the start for `beats` beats and returns
/// interleaved stereo samples.
pub fn render_arrangement(project: &Project, sample_rate: u32, beats: f64) -> Vec<f32> {
    let (mut engine, mut handle) = Engine::new(sample_rate as f32);
    engine.set_offline(true);
    handle.load_project(project, true);
    handle.send(Command::Play);
    let total_frames = (beats * 60.0 / project.tempo * sample_rate as f64).ceil() as usize;
    let block = 512;
    let mut out = vec![0.0f32; total_frames * 2];
    let mut done = 0;
    while done < total_frames {
        let n = block.min(total_frames - done);
        engine.process(&mut out[done * 2..(done + n) * 2]);
        handle.collect_garbage();
        done += n;
    }
    out
}

pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    w.finalize()
}
