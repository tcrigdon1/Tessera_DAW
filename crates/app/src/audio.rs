//! Audio device output.
//!
//! Opens the default output device with cpal and runs the engine inside the
//! device callback. If no device can be opened, a "null" device thread drives
//! the engine in real time so the transport and UI keep working silently.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tessera_engine::{Engine, EngineHandle, MAX_BLOCK};

pub struct AudioOutput {
    _stream: Option<cpal::Stream>,
    null_thread: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    pub device_name: String,
    pub sample_rate: u32,
}

impl Drop for AudioOutput {
    fn drop(&mut self) {
        if let Some((stop, handle)) = self.null_thread.take() {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    }
}

/// Starts audio output and returns it with the engine handle for the UI.
pub fn start() -> (AudioOutput, EngineHandle) {
    match start_device() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("tessera: no audio output ({e}); using the silent null device");
            start_null(48000)
        }
    }
}

fn start_device() -> Result<(AudioOutput, EngineHandle), String> {
    if std::env::var_os("TESSERA_NULL_AUDIO").is_some() {
        return Err("TESSERA_NULL_AUDIO is set".into());
    }
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or("no default output device")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .or_else(|_| device.id().map(|id| format!("{id:?}")))
        .unwrap_or_else(|_| "Default output".into());
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();
    let sample_rate = stream_config.sample_rate;
    let (engine, handle) = Engine::new(sample_rate as f32);
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, stream_config, engine),
        SampleFormat::I16 => build::<i16>(&device, stream_config, engine),
        SampleFormat::U16 => build::<u16>(&device, stream_config, engine),
        SampleFormat::I32 => build::<i32>(&device, stream_config, engine),
        SampleFormat::F64 => build::<f64>(&device, stream_config, engine),
        other => return Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((
        AudioOutput {
            _stream: Some(stream),
            null_thread: None,
            device_name: name,
            sample_rate,
        },
        handle,
    ))
}

fn build<T>(device: &cpal::Device, config: cpal::StreamConfig, mut engine: Engine) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let mut scratch = vec![0.0f32; MAX_BLOCK * 2];
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                let frames_total = data.len() / channels.max(1);
                let mut done = 0;
                while done < frames_total {
                    let n = (frames_total - done).min(MAX_BLOCK);
                    let buf = &mut scratch[..n * 2];
                    engine.process(buf);
                    for f in 0..n {
                        let frame = &mut data[(done + f) * channels..(done + f + 1) * channels];
                        for (c, s) in frame.iter_mut().enumerate() {
                            let v = match c {
                                0 => buf[f * 2],
                                1 => buf[f * 2 + 1],
                                _ => 0.0,
                            };
                            *s = T::from_sample(v);
                        }
                        if channels == 1 {
                            frame[0] = T::from_sample((buf[f * 2] + buf[f * 2 + 1]) * 0.5);
                        }
                    }
                    done += n;
                }
            },
            |err| eprintln!("tessera: audio stream error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}

fn start_null(sample_rate: u32) -> (AudioOutput, EngineHandle) {
    let (mut engine, handle) = Engine::new(sample_rate as f32);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let thread = std::thread::Builder::new()
        .name("tessera-null-audio".into())
        .spawn(move || {
            let block = 512usize;
            let period = Duration::from_secs_f64(block as f64 / sample_rate as f64);
            let mut buf = vec![0.0f32; block * 2];
            let mut next = Instant::now();
            while !stop2.load(Ordering::Relaxed) {
                engine.process(&mut buf);
                next += period;
                let now = Instant::now();
                if next > now {
                    std::thread::sleep(next - now);
                } else {
                    next = now;
                }
            }
        })
        .expect("spawn null audio thread");
    (
        AudioOutput {
            _stream: None,
            null_thread: Some((stop, thread)),
            device_name: "No audio device (silent)".into(),
            sample_rate,
        },
        handle,
    )
}
