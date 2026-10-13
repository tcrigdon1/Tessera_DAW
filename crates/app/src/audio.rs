//! Audio device input and output.
//!
//! Opens the default output device with cpal and runs the engine inside the
//! device callback. If no device can be opened, a "null" device thread drives
//! the engine in real time so the transport and UI keep working silently.
//!
//! The input device, when there is one at the output's sample rate, feeds
//! the engine through a lock-free ring for monitoring and recording.
//!
//! Which driver, devices, sample rate, buffer size and channels are used
//! comes from [`AudioConfig`] (Preferences → Audio); anything unset or
//! unavailable falls back to the system default.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tessera_engine::engine::input_ring;
use tessera_engine::{Engine, EngineHandle, SharedState, MAX_BLOCK};

use crate::prefs::AudioConfig;

/// Sample rates offered in Preferences → Audio (when the device supports them).
pub const SAMPLE_RATES: [u32; 7] = [22050, 32000, 44100, 48000, 88200, 96000, 192000];
/// Buffer sizes offered in Preferences → Audio.
pub const BUFFER_SIZES: [u32; 9] = [32, 64, 128, 256, 512, 1024, 2048, 4096, 8192];

pub struct AudioOutput {
    _stream: Option<cpal::Stream>,
    _input: Option<cpal::Stream>,
    pub input_name: Option<String>,
    null_thread: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    pub driver: String,
    pub device_name: String,
    pub sample_rate: u32,
    /// Frames per buffer when a fixed size was requested.
    pub buffer_size: Option<u32>,
    pub output_channels: u16,
    pub input_channels: u16,
    /// Output latency the device reports, in frames.
    pub output_latency: Arc<AtomicU32>,
    /// Why the configured device couldn't be used, if it couldn't.
    pub fallback_reason: Option<String>,
}

impl AudioOutput {
    /// No device at all (while switching devices).
    pub fn closed() -> AudioOutput {
        AudioOutput {
            _stream: None,
            _input: None,
            input_name: None,
            null_thread: None,
            driver: String::new(),
            device_name: String::new(),
            sample_rate: 48000,
            buffer_size: None,
            output_channels: 0,
            input_channels: 0,
            output_latency: Arc::new(AtomicU32::new(0)),
            fallback_reason: None,
        }
    }

    /// Output latency in milliseconds: the buffer plus what the device reports.
    pub fn output_latency_ms(&self) -> f64 {
        let frames = self.output_latency.load(Ordering::Relaxed) as f64;
        let buffer = self.buffer_size.unwrap_or(0) as f64;
        (frames.max(buffer)) * 1000.0 / self.sample_rate as f64
    }
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
pub fn start(cfg: &AudioConfig) -> (AudioOutput, EngineHandle) {
    match start_device(cfg) {
        Ok(r) => r,
        Err(e) if *cfg != AudioConfig::default() && std::env::var_os("TESSERA_NULL_AUDIO").is_none() => {
            // The chosen device is gone or refused the settings: try the
            // system default before giving up on sound.
            eprintln!("tessera: audio settings failed ({e}); trying the default device");
            match start_device(&AudioConfig::default()) {
                Ok((mut out, h)) => {
                    out.fallback_reason = Some(e);
                    (out, h)
                }
                Err(e2) => {
                    eprintln!("tessera: no audio output ({e2}); using the silent null device");
                    let (mut out, h) = start_null(48000);
                    out.fallback_reason = Some(e);
                    (out, h)
                }
            }
        }
        Err(e) => {
            eprintln!("tessera: no audio output ({e}); using the silent null device");
            start_null(48000)
        }
    }
}

/// The audio drivers (cpal hosts) on this system.
pub fn drivers() -> Vec<String> {
    cpal::available_hosts().iter().map(|h| h.name().to_string()).collect()
}

fn host_for(driver: Option<&str>) -> Result<cpal::Host, String> {
    match driver {
        None => Ok(cpal::default_host()),
        Some(name) => {
            let id = cpal::available_hosts()
                .into_iter()
                .find(|h| h.name() == name)
                .ok_or_else(|| format!("audio driver {name} is not available"))?;
            cpal::host_from_id(id).map_err(|e| e.to_string())
        }
    }
}

fn device_name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_string())
        .or_else(|_| device.id().map(|id| format!("{id:?}")))
        .unwrap_or_else(|_| "Audio device".into())
}

/// What Preferences → Audio can offer for a driver.
#[derive(Debug, Clone, Default)]
pub struct DeviceList {
    pub outputs: Vec<String>,
    pub inputs: Vec<String>,
}

/// Lists the output and input devices of a driver (slow on some systems:
/// call it when the Audio tab opens or on Refresh, not every frame).
pub fn list_devices(driver: Option<&str>) -> DeviceList {
    let Ok(host) = host_for(driver) else {
        return DeviceList::default();
    };
    let names = |it: Result<_, _>| -> Vec<String> {
        match it {
            Ok(devs) => {
                let devs: Vec<cpal::Device> = devs;
                devs.iter().map(device_name).collect()
            }
            Err(_) => Vec::new(),
        }
    };
    DeviceList {
        outputs: names(host.output_devices().map(|d| d.collect())),
        inputs: names(host.input_devices().map(|d| d.collect())),
    }
}

/// Sample rates from [`SAMPLE_RATES`] an output device supports, and its
/// buffer size range.
pub fn output_capabilities(cfg: &AudioConfig) -> (Vec<u32>, Option<(u32, u32)>) {
    let Ok(host) = host_for(cfg.driver.as_deref()) else {
        return (Vec::new(), None);
    };
    let Ok(device) = find_output(&host, cfg.output_device.as_deref()) else {
        return (Vec::new(), None);
    };
    let Ok(configs) = device.supported_output_configs() else {
        return (Vec::new(), None);
    };
    let configs: Vec<_> = configs.collect();
    let rates = SAMPLE_RATES
        .into_iter()
        .filter(|r| {
            configs
                .iter()
                .any(|c| c.min_sample_rate() <= *r && *r <= c.max_sample_rate())
        })
        .collect();
    let buffers = configs.iter().find_map(|c| match c.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => Some((*min, *max)),
        cpal::SupportedBufferSize::Unknown => None,
    });
    (rates, buffers)
}

fn find_output(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device, String> {
    match name {
        None => host
            .default_output_device()
            .ok_or_else(|| "no default output device".into()),
        Some(n) => host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| device_name(d) == n)
            .ok_or_else(|| format!("output device {n} not found")),
    }
}

fn find_input(host: &cpal::Host, name: Option<&str>) -> Result<cpal::Device, String> {
    match name {
        None => host
            .default_input_device()
            .ok_or_else(|| "no default input device".into()),
        Some(n) => host
            .input_devices()
            .map_err(|e| e.to_string())?
            .find(|d| device_name(d) == n)
            .ok_or_else(|| format!("input device {n} not found")),
    }
}

fn start_device(cfg: &AudioConfig) -> Result<(AudioOutput, EngineHandle), String> {
    if std::env::var_os("TESSERA_NULL_AUDIO").is_some() {
        return Err("TESSERA_NULL_AUDIO is set".into());
    }
    let host = host_for(cfg.driver.as_deref())?;
    let device = find_output(&host, cfg.output_device.as_deref())?;
    let name = device_name(&device);
    let config = match cfg.sample_rate {
        None => device.default_output_config().map_err(|e| e.to_string())?,
        Some(rate) => {
            let mut configs: Vec<_> = device
                .supported_output_configs()
                .map_err(|e| e.to_string())?
                .filter(|c| c.min_sample_rate() <= rate && rate <= c.max_sample_rate())
                .collect();
            configs.sort_by_key(|c| (c.sample_format() != SampleFormat::F32, c.channels() < 2));
            configs
                .into_iter()
                .next()
                .ok_or_else(|| format!("{name} does not support {rate} Hz"))?
                .with_sample_rate(rate)
        }
    };
    let format = config.sample_format();
    let mut stream_config: cpal::StreamConfig = config.into();
    if let Some(frames) = cfg.buffer_size {
        stream_config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let output_channels = stream_config.channels;
    let offset = (cfg.output_offset as usize).min(output_channels.saturating_sub(2) as usize);
    let sample_rate = stream_config.sample_rate;
    let (mut engine, handle) = Engine::new(sample_rate as f32);
    let out_latency = Arc::new(AtomicU32::new(0));
    let input = if cfg.input_off {
        Err("input switched off in Preferences".to_string())
    } else {
        start_input(&host, cfg, sample_rate, &handle.shared, &out_latency)
    };
    let input = match input {
        Ok((stream, name, consumer, channels)) => {
            engine.set_input(consumer);
            Some((stream, name, channels))
        }
        Err(e) => {
            eprintln!("tessera: no audio input ({e})");
            None
        }
    };
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, stream_config, engine, out_latency.clone(), offset),
        SampleFormat::I16 => build::<i16>(&device, stream_config, engine, out_latency.clone(), offset),
        SampleFormat::U16 => build::<u16>(&device, stream_config, engine, out_latency.clone(), offset),
        SampleFormat::I32 => build::<i32>(&device, stream_config, engine, out_latency.clone(), offset),
        SampleFormat::F64 => build::<f64>(&device, stream_config, engine, out_latency.clone(), offset),
        other => return Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    let (input_stream, input_name, input_channels) = match input {
        Some((s, n, c)) => {
            if let Err(e) = s.play() {
                eprintln!("tessera: could not start audio input: {e}");
            }
            (Some(s), Some(n), c)
        }
        None => (None, None, 0),
    };
    Ok((
        AudioOutput {
            _stream: Some(stream),
            _input: input_stream,
            input_name,
            null_thread: None,
            driver: host.id().name().to_string(),
            device_name: name,
            sample_rate,
            buffer_size: cfg.buffer_size,
            output_channels,
            input_channels,
            output_latency: out_latency,
            fallback_reason: None,
        },
        handle,
    ))
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut engine: Engine,
    out_latency: Arc<AtomicU32>,
    offset: usize,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate as f64;
    let mut scratch = vec![0.0f32; MAX_BLOCK * 2];
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
                    engine.process(buf);
                    for f in 0..n {
                        let frame = &mut data[(done + f) * channels..(done + f + 1) * channels];
                        for (c, s) in frame.iter_mut().enumerate() {
                            // The master goes to the chosen output pair.
                            let v = match c.wrapping_sub(offset) {
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

/// Opens the input device at `sample_rate` and returns the stream, the
/// device name, the ring the engine reads from and the channel count.
fn start_input(
    host: &cpal::Host,
    cfg: &AudioConfig,
    sample_rate: u32,
    shared: &Arc<SharedState>,
    out_latency: &Arc<AtomicU32>,
) -> Result<(cpal::Stream, String, rtrb::Consumer<f32>, u16), String> {
    if std::env::var_os("TESSERA_NO_INPUT").is_some() {
        return Err("TESSERA_NO_INPUT is set".into());
    }
    let device = find_input(host, cfg.input_device.as_deref())?;
    let name = device_name(&device);
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
    let mut config: cpal::StreamConfig = supported.into();
    if let Some(frames) = cfg.buffer_size {
        config.buffer_size = cpal::BufferSize::Fixed(frames);
    }
    let channels = config.channels;
    let offset = (cfg.input_offset as usize).min(channels.saturating_sub(1) as usize);
    let (producer, consumer) = input_ring();
    let shared = shared.clone();
    let out_latency = out_latency.clone();
    let stream = match format {
        SampleFormat::F32 => build_input::<f32>(&device, config, producer, shared, out_latency, offset),
        SampleFormat::I16 => build_input::<i16>(&device, config, producer, shared, out_latency, offset),
        SampleFormat::U16 => build_input::<u16>(&device, config, producer, shared, out_latency, offset),
        SampleFormat::I32 => build_input::<i32>(&device, config, producer, shared, out_latency, offset),
        SampleFormat::F64 => build_input::<f64>(&device, config, producer, shared, out_latency, offset),
        other => return Err(format!("unsupported input sample format {other}")),
    }?;
    Ok((stream, name, consumer, channels))
}

fn build_input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut producer: rtrb::Producer<f32>,
    shared: Arc<SharedState>,
    out_latency: Arc<AtomicU32>,
    offset: usize,
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
                    // The chosen input pair (or the last channel, mono).
                    let l = <f32 as FromSample<T>>::from_sample_(frame[offset.min(frame.len() - 1)]);
                    let r = match frame.get(offset + 1) {
                        Some(s) => <f32 as FromSample<T>>::from_sample_(*s),
                        None => l,
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
            driver: "None".into(),
            device_name: "No audio device (silent)".into(),
            sample_rate,
            buffer_size: Some(512),
            output_channels: 2,
            input_channels: 0,
            output_latency: Arc::new(AtomicU32::new(0)),
            fallback_reason: None,
        },
        handle,
    )
}
