//! Audio device input and output.
//!
//! Opens the default output device with cpal and runs the engine inside the
//! device callback. If no device can be opened, a "null" device thread drives
//! the engine in real time so the transport and UI keep working silently.
//!
//! The default input device, when there is one at the output's sample rate,
//! feeds the engine through a lock-free ring for monitoring and recording.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tessera_engine::engine::input_ring;
use tessera_engine::{Engine, EngineHandle, SharedState, MAX_BLOCK};

pub struct AudioOutput {
    _stream: Option<cpal::Stream>,
    _input: Option<cpal::Stream>,
    pub input_name: Option<String>,
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
    let (mut engine, handle) = Engine::new(sample_rate as f32);
    let out_latency = Arc::new(AtomicU32::new(0));
    let input = match start_input(&host, sample_rate, &handle.shared, &out_latency) {
        Ok((stream, name, consumer)) => {
            engine.set_input(consumer);
            Some((stream, name))
        }
        Err(e) => {
            eprintln!("tessera: no audio input ({e})");
            None
        }
    };
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, stream_config, engine, out_latency),
        SampleFormat::I16 => build::<i16>(&device, stream_config, engine, out_latency),
        SampleFormat::U16 => build::<u16>(&device, stream_config, engine, out_latency),
        SampleFormat::I32 => build::<i32>(&device, stream_config, engine, out_latency),
        SampleFormat::F64 => build::<f64>(&device, stream_config, engine, out_latency),
        other => return Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    let (input_stream, input_name) = match input {
        Some((s, n)) => {
            if let Err(e) = s.play() {
                eprintln!("tessera: could not start audio input: {e}");
            }
            (Some(s), Some(n))
        }
        None => (None, None),
    };
    Ok((
        AudioOutput {
            _stream: Some(stream),
            _input: input_stream,
            input_name,
            null_thread: None,
            device_name: name,
            sample_rate,
        },
        handle,
    ))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut engine: Engine,
    out_latency: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate as f64;
    let mut scratch = vec![0.0f32; MAX_BLOCK * 2];
    // Channels 3 and 4 carry the cue output when the device has them.
    let mut cue = vec![0.0f32; MAX_BLOCK * 2];
    engine.set_cue_output(channels >= 4);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let ts = info.timestamp();
                let lat = ts.playback.duration_since(ts.callback).as_secs_f64() * rate;
                out_latency.store(lat as u32, Ordering::Relaxed);
                let frames_total = data.len() / channels.max(1);
                let mut done = 0;
                while done < frames_total {
                    let n = (frames_total - done).min(MAX_BLOCK);
                    let buf = &mut scratch[..n * 2];
                    let cue = &mut cue[..n * 2];
                    if channels >= 4 {
                        engine.process_with_cue(buf, Some(cue));
                    } else {
                        engine.process(buf);
                    }
                    for f in 0..n {
                        let frame = &mut data[(done + f) * channels..(done + f + 1) * channels];
                        for (c, s) in frame.iter_mut().enumerate() {
                            let v = match c {
                                0 => buf[f * 2],
                                1 => buf[f * 2 + 1],
                                2 if channels >= 4 => cue[f * 2],
                                3 if channels >= 4 => cue[f * 2 + 1],
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

/// Opens the default input device at `sample_rate` and returns the stream,
/// the device name and the ring the engine reads from.
fn start_input(
    host: &cpal::Host,
    sample_rate: u32,
    shared: &Arc<SharedState>,
    out_latency: &Arc<AtomicU32>,
) -> Result<(cpal::Stream, String, rtrb::Consumer<f32>), String> {
    if std::env::var_os("TESSERA_NO_INPUT").is_some() {
        return Err("TESSERA_NO_INPUT is set".into());
    }
    let device = host.default_input_device().ok_or("no default input device")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Default input".into());
    // Prefer float formats, then anything, at the output's sample rate.
    let mut configs: Vec<_> = device
        .supported_input_configs()
        .map_err(|e| e.to_string())?
        .filter(|c| c.min_sample_rate() <= sample_rate && sample_rate <= c.max_sample_rate())
        .collect();
    configs.sort_by_key(|c| {
        (
            c.sample_format() != SampleFormat::F32,
            c.channels() > 2,
            c.channels() < 2,
        )
    });
    let supported = configs
        .into_iter()
        .next()
        .ok_or_else(|| format!("input does not support {sample_rate} Hz"))?
        .with_sample_rate(sample_rate);
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let (producer, consumer) = input_ring();
    let shared = shared.clone();
    let out_latency = out_latency.clone();
    let stream = match format {
        SampleFormat::F32 => build_input::<f32>(&device, config, producer, shared, out_latency),
        SampleFormat::I16 => build_input::<i16>(&device, config, producer, shared, out_latency),
        SampleFormat::U16 => build_input::<u16>(&device, config, producer, shared, out_latency),
        SampleFormat::I32 => build_input::<i32>(&device, config, producer, shared, out_latency),
        SampleFormat::F64 => build_input::<f64>(&device, config, producer, shared, out_latency),
        other => return Err(format!("unsupported input sample format {other}")),
    }?;
    Ok((stream, name, consumer))
}

fn build_input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut producer: rtrb::Producer<f32>,
    shared: Arc<SharedState>,
    out_latency: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = (config.channels as usize).max(1);
    let rate = config.sample_rate as f64;
    device
        .build_input_stream(
            config,
            move |data: &[T], info: &cpal::InputCallbackInfo| {
                let ts = info.timestamp();
                let lat = ts.callback.duration_since(ts.capture).as_secs_f64() * rate;
                shared
                    .record
                    .io_latency
                    .store(lat as u32 + out_latency.load(Ordering::Relaxed), Ordering::Relaxed);
                for frame in data.chunks(channels) {
                    let l = <f32 as FromSample<T>>::from_sample_(frame[0]);
                    let r = if channels > 1 {
                        <f32 as FromSample<T>>::from_sample_(frame[1])
                    } else {
                        l
                    };
                    // When the engine is not reading (ring full) input is dropped.
                    if producer.slots() >= 2 {
                        let _ = producer.push(l);
                        let _ = producer.push(r);
                    }
                }
            },
            |err| eprintln!("tessera: audio input error: {err}"),
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
            _input: None,
            input_name: None,
            null_thread: Some((stop, thread)),
            device_name: "No audio device (silent)".into(),
            sample_rate,
        },
        handle,
    )
}
