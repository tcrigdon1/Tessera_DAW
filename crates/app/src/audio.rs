//! Audio device input and output.
//!
//! Opens the default output device with cpal and runs the engine inside the
//! device callback. If no device can be opened, a "null" device thread drives
//! the engine in real time so the transport and UI keep working silently.
//!
//! The input device feeds the engine through a lock-free ring for monitoring
//! and recording. It runs at whatever rate, channel count and sample format
//! it supports; its callback converts to stereo f32 and resamples to the
//! engine's rate when the two differ, so an interface that cannot match the
//! output's rate still works.
//!
//! Which driver, devices, sample rate, buffer size and channels are used
//! comes from [`AudioConfig`] (Preferences → Audio); anything unset or
//! unavailable falls back to the system default.
//!
//! Drivers are cpal hosts (CoreAudio, ALSA, JACK, ASIO when built with the
//! `asio` feature), except that Windows WASAPI is offered twice: shared mode
//! through cpal, and exclusive mode through [`crate::wasapi_exclusive`].

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tessera_engine::engine::input_ring;
use tessera_engine::midi::MidiMessage;
use tessera_engine::resample::StreamResampler;
use tessera_engine::{Engine, EngineHandle, SharedState, MAX_BLOCK};

use crate::prefs::AudioConfig;

/// Hardware MIDI input handed to whichever engine gets created.
type MidiIn = Option<rtrb::Consumer<MidiMessage>>;

/// Sample rates offered in Preferences → Audio (when the device supports them).
pub const SAMPLE_RATES: [u32; 7] = [22050, 32000, 44100, 48000, 88200, 96000, 192000];
/// Buffer sizes offered in Preferences → Audio.
pub const BUFFER_SIZES: [u32; 9] = [32, 64, 128, 256, 512, 1024, 2048, 4096, 8192];

/// Driver names shown for Windows WASAPI.
pub const WASAPI_SHARED: &str = "WASAPI Shared";
pub const WASAPI_EXCLUSIVE: &str = "WASAPI Exclusive";

/// A running device stream, held to keep it running.
#[allow(dead_code)]
enum StreamHandle {
    Cpal(cpal::Stream),
    #[cfg(windows)]
    Exclusive(crate::wasapi_exclusive::Stream),
}

pub struct AudioOutput {
    _stream: Option<StreamHandle>,
    _input: Option<StreamHandle>,
    pub input_name: Option<String>,
    null_thread: Option<(Arc<AtomicBool>, JoinHandle<()>)>,
    pub driver: String,
    pub device_name: String,
    pub sample_rate: u32,
    /// Frames per buffer when a fixed size was requested.
    pub buffer_size: Option<u32>,
    pub output_channels: u16,
    pub input_channels: u16,
    /// The input device's own sample rate (0 without input).
    pub input_rate: u32,
    /// The input device's sample format, e.g. "f32" (empty without input).
    pub input_format: String,
    /// Why there is no audio input, when the input could not be opened.
    pub input_error: Option<String>,
    /// Output latency the device reports, in frames.
    pub output_latency: Arc<AtomicU32>,
    /// Frames the output callback last asked for (the real buffer size).
    pub callback_frames: Arc<AtomicU32>,
    /// Frames the input callback last delivered, at the input's own rate.
    pub input_frames: Arc<AtomicU32>,
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
            input_rate: 0,
            input_format: String::new(),
            input_error: None,
            output_latency: Arc::new(AtomicU32::new(0)),
            callback_frames: Arc::new(AtomicU32::new(0)),
            input_frames: Arc::new(AtomicU32::new(0)),
            fallback_reason: None,
        }
    }

    /// Frames per buffer the device actually runs at (the requested size
    /// until the first callback reports it).
    pub fn running_buffer(&self) -> Option<u32> {
        match self.callback_frames.load(Ordering::Relaxed) {
            0 => self.buffer_size,
            n => Some(n),
        }
    }

    /// Output latency in milliseconds: the buffer plus what the device reports.
    pub fn output_latency_ms(&self) -> f64 {
        let frames = self.output_latency.load(Ordering::Relaxed) as f64;
        let buffer = self.running_buffer().unwrap_or(0) as f64;
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
/// `midi` is the hardware MIDI input ring the engine reads.
pub fn start(cfg: &AudioConfig, midi: rtrb::Consumer<MidiMessage>) -> (AudioOutput, EngineHandle) {
    let mut midi = Some(midi);
    match start_device(cfg, &mut midi) {
        Ok(r) => r,
        Err(e) if *cfg != AudioConfig::default() && std::env::var_os("TESSERA_NULL_AUDIO").is_none() => {
            // The chosen device is gone or refused the settings: try the
            // system default before giving up on sound.
            eprintln!("tessera: audio settings failed ({e}); trying the default device");
            match start_device(&AudioConfig::default(), &mut midi) {
                Ok((mut out, h)) => {
                    out.fallback_reason = Some(e);
                    (out, h)
                }
                Err(e2) => {
                    eprintln!("tessera: no audio output ({e2}); using the silent null device");
                    let (mut out, h) = start_null(48000, &mut midi);
                    out.fallback_reason = Some(e);
                    (out, h)
                }
            }
        }
        Err(e) => {
            eprintln!("tessera: no audio output ({e}); using the silent null device");
            start_null(48000, &mut midi)
        }
    }
}

/// The audio drivers on this system, for the Driver Type menu.
pub fn drivers() -> Vec<String> {
    let mut out = Vec::new();
    for h in cpal::available_hosts() {
        match h.name() {
            "WASAPI" => {
                out.push(WASAPI_SHARED.to_string());
                if cfg!(windows) {
                    out.push(WASAPI_EXCLUSIVE.to_string());
                }
            }
            name => out.push(name.to_string()),
        }
    }
    out
}

/// Whether this build can use ASIO drivers (the `asio` Cargo feature).
pub const ASIO_BUILT: bool = cfg!(feature = "asio");

/// Whether a driver choice means WASAPI exclusive mode.
pub fn is_exclusive(driver: Option<&str>) -> bool {
    cfg!(windows) && driver == Some(WASAPI_EXCLUSIVE)
}

/// The cpal host behind a driver name ("WASAPI" also accepts the name
/// saved by earlier versions).
fn host_name(driver: &str) -> &str {
    match driver {
        WASAPI_SHARED | WASAPI_EXCLUSIVE => "WASAPI",
        d => d,
    }
}

/// The driver name shown for a cpal host.
fn driver_label(host: &cpal::Host) -> String {
    match host.id().name() {
        "WASAPI" => WASAPI_SHARED.to_string(),
        name => name.to_string(),
    }
}

fn host_for(driver: Option<&str>) -> Result<cpal::Host, String> {
    match driver {
        None => Ok(cpal::default_host()),
        Some(name) => {
            let id = cpal::available_hosts()
                .into_iter()
                .find(|h| h.name() == host_name(name))
                .ok_or_else(|| match name {
                    "ASIO" if !ASIO_BUILT => "ASIO needs a Tessera build with the asio feature".to_string(),
                    _ => format!("audio driver {name} is not available"),
                })?;
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
    #[cfg(windows)]
    if is_exclusive(cfg.driver.as_deref()) {
        use crate::wasapi_exclusive as ex;
        return match ex::capabilities(ex::Flow::Output, cfg.output_device.as_deref(), &SAMPLE_RATES) {
            Ok(c) => (c.rates, c.buffers),
            Err(e) => {
                eprintln!("tessera: exclusive mode: {e}");
                (Vec::new(), None)
            }
        };
    }
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

/// Buffer sizes to offer for a device's buffer range: the usual sizes that
/// fit, plus the range's own ends (WASAPI shared mode reports a single fixed
/// period such as 480, which matches none of the usual sizes).
pub fn buffer_choices(range: Option<(u32, u32)>) -> Vec<u32> {
    let Some((lo, hi)) = range else {
        return BUFFER_SIZES.to_vec();
    };
    let mut out: Vec<u32> = BUFFER_SIZES.into_iter().filter(|b| (lo..=hi).contains(b)).collect();
    for end in [lo, hi] {
        if end > 0 && end <= BUFFER_SIZES[BUFFER_SIZES.len() - 1] && !out.contains(&end) {
            out.push(end);
        }
    }
    out.sort_unstable();
    out
}

/// What an input device offers, for Preferences → Audio.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InputCapabilities {
    /// Sample rates it reports (any rate works: others are resampled).
    pub rates: Vec<u32>,
    pub max_channels: u16,
    /// Sample formats, e.g. "f32", "i16".
    pub formats: Vec<String>,
    /// Why the device could not be queried.
    pub error: Option<String>,
}

/// Queries the configured input device (slow on some systems, like
/// [`list_devices`]).
pub fn input_capabilities(cfg: &AudioConfig) -> InputCapabilities {
    let query = || -> Result<InputCapabilities, String> {
        let host = host_for(cfg.driver.as_deref())?;
        let device = find_input(&host, cfg.input_device.as_deref())?;
        let mut ranges: Vec<_> = device.supported_input_configs().map_err(|e| e.to_string())?.collect();
        if let Ok(d) = device.default_input_config() {
            let r = d.sample_rate();
            ranges.push(cpal::SupportedStreamConfigRange::new(
                d.channels(),
                r,
                r,
                *d.buffer_size(),
                d.sample_format(),
            ));
        }
        Ok(summarize_input(&ranges))
    };
    query().unwrap_or_else(|e| InputCapabilities {
        error: Some(e),
        ..Default::default()
    })
}

fn summarize_input(ranges: &[cpal::SupportedStreamConfigRange]) -> InputCapabilities {
    let mut rates: Vec<u32> = SAMPLE_RATES
        .into_iter()
        .filter(|r| {
            ranges
                .iter()
                .any(|c| c.min_sample_rate() <= *r && *r <= c.max_sample_rate())
        })
        .collect();
    // Odd fixed rates (16 kHz headsets and the like).
    for c in ranges {
        if c.min_sample_rate() == c.max_sample_rate() && !rates.contains(&c.min_sample_rate()) {
            rates.push(c.min_sample_rate());
        }
    }
    rates.sort_unstable();
    let mut formats: Vec<SampleFormat> = Vec::new();
    for c in ranges {
        if !formats.contains(&c.sample_format()) {
            formats.push(c.sample_format());
        }
    }
    formats.sort_by_key(|f| format_rank(*f));
    InputCapabilities {
        rates,
        max_channels: ranges.iter().map(|c| c.channels()).max().unwrap_or(0),
        formats: formats.iter().map(|f| f.to_string()).collect(),
        error: if ranges.is_empty() {
            Some("the device reports no input formats".into())
        } else {
            None
        },
    }
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

fn start_device(cfg: &AudioConfig, midi: &mut MidiIn) -> Result<(AudioOutput, EngineHandle), String> {
    if std::env::var_os("TESSERA_NULL_AUDIO").is_some() {
        return Err("TESSERA_NULL_AUDIO is set".into());
    }
    #[cfg(windows)]
    if is_exclusive(cfg.driver.as_deref()) {
        return start_exclusive(cfg, midi);
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
    let sample_rate = stream_config.sample_rate;
    let (mut engine, handle) = Engine::new(sample_rate as f32);
    if let Some(m) = midi.take() {
        engine.set_midi_input(m);
    }
    let meters = Meters::default();
    // An ASIO driver is one device for both directions: the input follows
    // the output unless another input was chosen.
    let input_cfg = AudioConfig {
        input_device: cfg
            .input_device
            .clone()
            .or_else(|| (host.id().name() == "ASIO").then(|| name.clone())),
        ..cfg.clone()
    };
    let (input, input_error) = open_input(Some(&host), &input_cfg, sample_rate, &handle.shared, &meters);
    let input = input.map(|mut open| {
        if let Some(c) = open.consumer.take() {
            engine.set_input(c);
        }
        open
    });
    let renderer = Renderer::new(
        engine,
        output_channels,
        cfg.output_offset,
        meters.callback_frames.clone(),
    );
    let lat = meters.output_latency.clone();
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, stream_config, renderer, lat),
        SampleFormat::I16 => build::<i16>(&device, stream_config, renderer, lat),
        SampleFormat::U16 => build::<u16>(&device, stream_config, renderer, lat),
        SampleFormat::I32 => build::<i32>(&device, stream_config, renderer, lat),
        SampleFormat::F64 => build::<f64>(&device, stream_config, renderer, lat),
        other => return Err(format!("unsupported sample format {other}")),
    }?;
    stream.play().map_err(|e| e.to_string())?;
    let running = Running {
        driver: driver_label(&host),
        device_name: name,
        sample_rate,
        buffer_size: cfg.buffer_size,
        output_channels,
    };
    Ok((
        finish(StreamHandle::Cpal(stream), running, input, input_error, meters),
        handle,
    ))
}

/// Opens the output in WASAPI exclusive mode; the input follows in
/// exclusive mode too when it can, else in shared mode.
#[cfg(windows)]
fn start_exclusive(cfg: &AudioConfig, midi: &mut MidiIn) -> Result<(AudioOutput, EngineHandle), String> {
    use crate::wasapi_exclusive as ex;
    let pending = ex::open(
        ex::Flow::Output,
        cfg.output_device.as_deref(),
        cfg.sample_rate,
        cfg.buffer_size,
    )?;
    let info = pending.info.clone();
    let sample_rate = info.rate;
    let (mut engine, handle) = Engine::new(sample_rate as f32);
    if let Some(m) = midi.take() {
        engine.set_midi_input(m);
    }
    let meters = Meters::default();
    meters.output_latency.store(info.latency_frames, Ordering::Relaxed);
    let host = host_for(cfg.driver.as_deref()).ok();
    let (input, input_error) = open_input(host.as_ref(), cfg, sample_rate, &handle.shared, &meters);
    let input = input.map(|mut open| {
        if let Some(c) = open.consumer.take() {
            engine.set_input(c);
        }
        open
    });
    let mut renderer = Renderer::new(engine, info.channels, cfg.output_offset, meters.callback_frames.clone());
    let stream = pending.start_output(Box::new(move |data, _channels| renderer.render(data)))?;
    let running = Running {
        driver: WASAPI_EXCLUSIVE.to_string(),
        device_name: info.name,
        sample_rate,
        buffer_size: Some(info.frames),
        output_channels: info.channels,
    };
    Ok((
        finish(StreamHandle::Exclusive(stream), running, input, input_error, meters),
        handle,
    ))
}

/// What the output runs at, for [`AudioOutput`].
struct Running {
    driver: String,
    device_name: String,
    sample_rate: u32,
    buffer_size: Option<u32>,
    output_channels: u16,
}

/// Latency and buffer readings the callbacks keep up to date.
#[derive(Default)]
struct Meters {
    output_latency: Arc<AtomicU32>,
    callback_frames: Arc<AtomicU32>,
    input_frames: Arc<AtomicU32>,
}

/// Starts the input (after the output, so its ring doesn't fill with audio
/// the engine never reads) and assembles the [`AudioOutput`].
fn finish(
    output: StreamHandle,
    running: Running,
    input: Option<OpenInput>,
    mut input_error: Option<String>,
    meters: Meters,
) -> AudioOutput {
    let (input_stream, input_name, input_channels, input_rate, input_format) = match input {
        Some(open) => {
            let name = open.name;
            match open.stream.play() {
                Ok(s) => (Some(s), Some(name), open.channels, open.rate, open.format),
                Err(e) => {
                    eprintln!("tessera: could not start audio input: {e}");
                    input_error = Some(format!("{name} could not be started: {e}"));
                    (None, None, 0, 0, String::new())
                }
            }
        }
        None => (None, None, 0, 0, String::new()),
    };
    AudioOutput {
        _stream: Some(output),
        _input: input_stream,
        input_name,
        null_thread: None,
        driver: running.driver,
        device_name: running.device_name,
        sample_rate: running.sample_rate,
        buffer_size: running.buffer_size,
        output_channels: running.output_channels,
        input_channels,
        input_rate,
        input_format,
        input_error,
        output_latency: meters.output_latency,
        callback_frames: meters.callback_frames,
        input_frames: meters.input_frames,
        fallback_reason: None,
    }
}

/// Opens the input for an output running at `engine_rate`, or says why
/// there is none. `host` is the cpal host for shared-mode inputs.
fn open_input(
    host: Option<&cpal::Host>,
    cfg: &AudioConfig,
    engine_rate: u32,
    shared: &Arc<SharedState>,
    meters: &Meters,
) -> (Option<OpenInput>, Option<String>) {
    if cfg.input_off {
        return (None, None);
    }
    let ctx = || InputContext {
        shared: shared.clone(),
        out_latency: meters.output_latency.clone(),
        frames: meters.input_frames.clone(),
        offset: cfg.input_offset as usize,
        engine_rate,
    };
    #[cfg(windows)]
    let exclusive_error = if is_exclusive(cfg.driver.as_deref()) {
        match start_input_exclusive(cfg, ctx()) {
            Ok(open) => return (Some(open), None),
            Err(e) => {
                eprintln!("tessera: exclusive-mode input unavailable ({e}); using shared mode");
                Some(e)
            }
        }
    } else {
        None
    };
    #[cfg(not(windows))]
    let exclusive_error: Option<String> = None;
    let result = match host {
        Some(host) => start_input(host, cfg, ctx),
        None => Err("no audio driver for the input".into()),
    };
    match result {
        Ok(open) => {
            if open.rate != engine_rate {
                eprintln!(
                    "tessera: audio input {} runs at {} Hz, resampled to {engine_rate} Hz",
                    open.name, open.rate
                );
            }
            (Some(open), None)
        }
        Err(e) => {
            eprintln!("tessera: no audio input ({e})");
            let e = match exclusive_error {
                Some(x) => format!("exclusive mode: {x}; shared mode: {e}"),
                None => e,
            };
            (None, Some(e))
        }
    }
}

/// Mixes the engine into the device's channels: the master goes to the
/// chosen output pair, the cue to the next pair when the device has four or
/// more channels.
struct Renderer {
    engine: Engine,
    scratch: Vec<f32>,
    cue: Vec<f32>,
    channels: usize,
    offset: usize,
    frames: Arc<AtomicU32>,
}

impl Renderer {
    fn new(mut engine: Engine, channels: u16, output_offset: u16, frames: Arc<AtomicU32>) -> Renderer {
        let offset = (output_offset as usize).min(channels.saturating_sub(2) as usize);
        let channels = channels as usize;
        // Channels 3 and 4 carry the cue output when the device has them.
        engine.set_cue_output(channels >= 4);
        Renderer {
            engine,
            scratch: vec![0.0f32; MAX_BLOCK * 2],
            cue: vec![0.0f32; MAX_BLOCK * 2],
            channels,
            offset,
            frames,
        }
    }

    fn render<T: SizedSample + FromSample<f32>>(&mut self, data: &mut [T]) {
        let channels = self.channels.max(1);
        let frames_total = data.len() / channels;
        self.frames.store(frames_total as u32, Ordering::Relaxed);
        let mut done = 0;
        while done < frames_total {
            let n = (frames_total - done).min(MAX_BLOCK);
            let buf = &mut self.scratch[..n * 2];
            let cue = &mut self.cue[..n * 2];
            if channels >= 4 {
                self.engine.process_with_cue(buf, Some(cue));
            } else {
                self.engine.process(buf);
            }
            for f in 0..n {
                let frame = &mut data[(done + f) * channels..(done + f + 1) * channels];
                for (c, s) in frame.iter_mut().enumerate() {
                    let v = match c.wrapping_sub(self.offset) {
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
    }
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut renderer: Renderer,
    out_latency: Arc<AtomicU32>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = (config.channels as usize).max(1);
    let rate = config.sample_rate as f64;
    device
        .build_output_stream(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                let ts = info.timestamp();
                let lat = ts.playback.duration_since(ts.callback).as_secs_f64() * rate;
                // A driver that reports no latency still has its buffer.
                let frames = (data.len() / channels) as f64;
                out_latency.store(lat.max(frames) as u32, Ordering::Relaxed);
                renderer.render(data);
            },
            |err| eprintln!("tessera: audio stream error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// An input stream opened but not started yet.
enum InputStream {
    Cpal(cpal::Stream),
    #[cfg(windows)]
    Exclusive(Box<(crate::wasapi_exclusive::Pending, InputFeed)>),
}

impl InputStream {
    fn play(self) -> Result<StreamHandle, String> {
        match self {
            InputStream::Cpal(s) => s.play().map(|()| StreamHandle::Cpal(s)).map_err(|e| e.to_string()),
            #[cfg(windows)]
            InputStream::Exclusive(b) => {
                let (pending, mut feed) = *b;
                let latency = pending.info.latency_frames as f64 / pending.info.rate as f64;
                pending
                    .start_input(Box::new(move |data, _channels| feed.feed(data, latency)))
                    .map(StreamHandle::Exclusive)
            }
        }
    }
}

/// An opened input stream, ready to play.
struct OpenInput {
    stream: InputStream,
    name: String,
    /// The ring the engine reads (taken when the engine is connected).
    consumer: Option<rtrb::Consumer<f32>>,
    channels: u16,
    /// The device's own sample rate (resampled to the engine's when it differs).
    rate: u32,
    format: String,
}

/// Opens the input in WASAPI exclusive mode at the engine's rate (or the
/// device's own, then resampled).
#[cfg(windows)]
fn start_input_exclusive(cfg: &AudioConfig, ctx: InputContext) -> Result<OpenInput, String> {
    if std::env::var_os("TESSERA_NO_INPUT").is_some() {
        return Err("TESSERA_NO_INPUT is set".into());
    }
    use crate::wasapi_exclusive as ex;
    let pending = ex::open(
        ex::Flow::Input,
        cfg.input_device.as_deref(),
        Some(ctx.engine_rate),
        cfg.buffer_size,
    )?;
    let info = pending.info.clone();
    let (producer, consumer) = input_ring();
    let feed = InputFeed::new(producer, ctx, info.channels, info.rate);
    Ok(OpenInput {
        stream: InputStream::Exclusive(Box::new((pending, feed))),
        name: info.name,
        consumer: Some(consumer),
        channels: info.channels,
        rate: info.rate,
        format: info.format.to_string(),
    })
}

/// Opens the input device and returns the stream and the ring the engine
/// reads from. The device runs at whatever rate, channel count and sample
/// format it supports that best fits the engine's rate; the callback converts
/// to stereo f32 at the engine's rate.
fn start_input(host: &cpal::Host, cfg: &AudioConfig, ctx: impl Fn() -> InputContext) -> Result<OpenInput, String> {
    if std::env::var_os("TESSERA_NO_INPUT").is_some() {
        return Err("TESSERA_NO_INPUT is set".into());
    }
    let engine_rate = ctx().engine_rate;
    let device = find_input(host, cfg.input_device.as_deref())?;
    let name = device_name(&device);
    let ranges: Vec<_> = match device.supported_input_configs() {
        Ok(c) => c.collect(),
        Err(e) => {
            eprintln!("tessera: {name} did not list its input formats ({e})");
            Vec::new()
        }
    };
    let default = device.default_input_config().ok();
    let candidates = input_candidates(&ranges, default.as_ref(), engine_rate, cfg.input_offset);
    if candidates.is_empty() {
        return Err(format!("{name} reports no usable input format"));
    }
    let mut errors = Vec::new();
    for supported in candidates {
        let format = supported.sample_format();
        let base: cpal::StreamConfig = supported.into();
        // A fixed buffer size only makes sense at the engine's own rate;
        // fall back to the device's default size if it is refused.
        let mut buffers = vec![cpal::BufferSize::Default];
        if let (Some(frames), true) = (cfg.buffer_size, base.sample_rate == engine_rate) {
            buffers.insert(0, cpal::BufferSize::Fixed(frames));
        }
        for buffer_size in buffers {
            let config = cpal::StreamConfig { buffer_size, ..base };
            let channels = config.channels;
            let rate = config.sample_rate;
            let (producer, consumer) = input_ring();
            let feed = InputFeed::new(producer, ctx(), channels, rate);
            let built = match format {
                SampleFormat::F32 => build_input::<f32>(&device, config, feed),
                SampleFormat::F64 => build_input::<f64>(&device, config, feed),
                SampleFormat::I8 => build_input::<i8>(&device, config, feed),
                SampleFormat::I16 => build_input::<i16>(&device, config, feed),
                SampleFormat::I24 => build_input::<cpal::I24>(&device, config, feed),
                SampleFormat::I32 => build_input::<i32>(&device, config, feed),
                SampleFormat::I64 => build_input::<i64>(&device, config, feed),
                SampleFormat::U8 => build_input::<u8>(&device, config, feed),
                SampleFormat::U16 => build_input::<u16>(&device, config, feed),
                SampleFormat::U24 => build_input::<cpal::U24>(&device, config, feed),
                SampleFormat::U32 => build_input::<u32>(&device, config, feed),
                SampleFormat::U64 => build_input::<u64>(&device, config, feed),
                other => Err(format!("unsupported sample format {other}")),
            };
            match built {
                Ok(stream) => {
                    return Ok(OpenInput {
                        stream: InputStream::Cpal(stream),
                        name,
                        consumer: Some(consumer),
                        channels,
                        rate,
                        format: format.to_string(),
                    })
                }
                Err(e) => errors.push(format!("{rate} Hz {channels} ch {format}: {e}")),
            }
        }
    }
    Err(format!("{name} could not be opened ({})", errors.join("; ")))
}

/// Rank of a sample format: lower is preferred.
fn format_rank(f: SampleFormat) -> u8 {
    match f {
        SampleFormat::F32 => 0,
        SampleFormat::I32 => 1,
        SampleFormat::I24 => 2,
        SampleFormat::I16 => 3,
        SampleFormat::F64 => 4,
        SampleFormat::U16 | SampleFormat::U24 | SampleFormat::U32 => 5,
        _ => 6,
    }
}

/// The input formats to try, best first. Each range is tried at the engine
/// rate if it covers it, else at its closest rate (then resampled). Formats
/// at the engine rate come first, then the device's own default (what
/// Windows shared mode and most drivers actually run at), then the rest.
/// Float and wide integer formats beat narrow ones, and formats with enough
/// channels for the chosen input pair beat narrower ones.
fn input_candidates(
    ranges: &[cpal::SupportedStreamConfigRange],
    default: Option<&cpal::SupportedStreamConfig>,
    engine_rate: u32,
    input_offset: u16,
) -> Vec<cpal::SupportedStreamConfig> {
    let want = input_offset as u32 + 2;
    let mut all: Vec<(bool, cpal::SupportedStreamConfig)> = ranges
        .iter()
        .filter(|r| r.channels() > 0 && r.min_sample_rate() <= r.max_sample_rate())
        .map(|r| {
            let rate = engine_rate.clamp(r.min_sample_rate(), r.max_sample_rate());
            (false, r.with_sample_rate(rate))
        })
        .collect();
    if let Some(d) = default {
        if d.channels() > 0 {
            all.push((true, *d));
        }
    }
    all.sort_by_key(|(is_default, c)| {
        (
            c.sample_rate() != engine_rate,
            !is_default,
            (c.sample_rate() as i64 - engine_rate as i64).unsigned_abs(),
            format_rank(c.sample_format()),
            (c.channels() as u32) < want,
            c.channels(),
        )
    });
    let mut out: Vec<cpal::SupportedStreamConfig> = Vec::new();
    for (_, c) in all {
        let same = out.iter().any(|o| {
            o.sample_rate() == c.sample_rate() && o.channels() == c.channels() && o.sample_format() == c.sample_format()
        });
        if !same {
            out.push(c);
        }
    }
    out
}

/// What every input callback shares with the engine.
struct InputContext {
    shared: Arc<SharedState>,
    out_latency: Arc<AtomicU32>,
    frames: Arc<AtomicU32>,
    /// First channel of the chosen input pair.
    offset: usize,
    engine_rate: u32,
}

/// Converts device input to stereo f32 at the engine's rate and feeds the
/// engine's input ring. Owned by the input callback; never allocates.
struct InputFeed {
    producer: rtrb::Producer<f32>,
    resampler: StreamResampler,
    ctx: InputContext,
    channels: usize,
    rate: f64,
}

impl InputFeed {
    fn new(producer: rtrb::Producer<f32>, ctx: InputContext, channels: u16, rate: u32) -> InputFeed {
        let channels = (channels as usize).max(1);
        let ctx = InputContext {
            offset: ctx.offset.min(channels - 1),
            ..ctx
        };
        InputFeed {
            producer,
            resampler: StreamResampler::new(rate, ctx.engine_rate),
            ctx,
            channels,
            rate: rate as f64,
        }
    }

    /// `latency` is how old the first sample is, in seconds (at least the
    /// buffer itself).
    fn feed<T>(&mut self, data: &[T], latency: f64)
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        let frames = data.len() / self.channels;
        self.ctx.frames.store(frames as u32, Ordering::Relaxed);
        let lat = latency.max(frames as f64 / self.rate) * self.ctx.engine_rate as f64;
        self.ctx.shared.record.io_latency.store(
            lat as u32 + self.ctx.out_latency.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        let offset = self.ctx.offset;
        let producer = &mut self.producer;
        for frame in data.chunks(self.channels) {
            // The chosen input pair (or the last channel, mono).
            let l = <f32 as FromSample<T>>::from_sample_(frame[offset.min(frame.len() - 1)]);
            let r = match frame.get(offset + 1) {
                Some(s) => <f32 as FromSample<T>>::from_sample_(*s),
                None => l,
            };
            self.resampler.push(l, r, |l, r| {
                // When the engine is not reading (ring full) input is dropped.
                if producer.slots() >= 2 {
                    let _ = producer.push(l);
                    let _ = producer.push(r);
                }
            });
        }
    }
}

fn build_input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    mut feed: InputFeed,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], info: &cpal::InputCallbackInfo| {
                let ts = info.timestamp();
                feed.feed(data, ts.callback.duration_since(ts.capture).as_secs_f64());
            },
            |err| eprintln!("tessera: audio input error: {err}"),
            None,
        )
        .map_err(|e| e.to_string())
}

fn start_null(sample_rate: u32, midi: &mut MidiIn) -> (AudioOutput, EngineHandle) {
    let (mut engine, handle) = Engine::new(sample_rate as f32);
    if let Some(m) = midi.take() {
        engine.set_midi_input(m);
    }
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
            input_rate: 0,
            input_format: String::new(),
            input_error: None,
            output_latency: Arc::new(AtomicU32::new(0)),
            callback_frames: Arc::new(AtomicU32::new(0)),
            input_frames: Arc::new(AtomicU32::new(0)),
            fallback_reason: None,
        },
        handle,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::{SupportedBufferSize, SupportedStreamConfigRange as Range};

    fn range(ch: u16, min: u32, max: u32, f: SampleFormat) -> Range {
        Range::new(ch, min, max, SupportedBufferSize::Unknown, f)
    }

    #[test]
    fn shared_mode_device_at_another_rate_is_still_used() {
        // A Windows shared-mode microphone that only runs at 48 kHz.
        let ranges = [range(2, 48000, 48000, SampleFormat::F32)];
        let c = input_candidates(&ranges, None, 44100, 0);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].sample_rate(), 48000);
    }

    #[test]
    fn the_engine_rate_wins_when_offered() {
        let ranges = [
            range(2, 48000, 48000, SampleFormat::F32),
            range(2, 44100, 44100, SampleFormat::I16),
        ];
        let default = range(2, 48000, 48000, SampleFormat::F32).with_sample_rate(48000);
        let c = input_candidates(&ranges, Some(&default), 44100, 0);
        assert_eq!((c[0].sample_rate(), c[0].sample_format()), (44100, SampleFormat::I16));
        // The device default comes next, without duplicates.
        assert_eq!(c[1].sample_rate(), 48000);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn default_beats_other_mismatched_rates() {
        let ranges = [
            range(1, 16000, 16000, SampleFormat::I16),
            range(2, 96000, 96000, SampleFormat::I32),
        ];
        let default = range(2, 96000, 96000, SampleFormat::I32).with_sample_rate(96000);
        let c = input_candidates(&ranges, Some(&default), 44100, 0);
        assert_eq!(c[0].sample_rate(), 96000);
    }

    #[test]
    fn prefers_float_then_enough_channels() {
        let ranges = [
            range(2, 8000, 192000, SampleFormat::I16),
            range(2, 8000, 192000, SampleFormat::F32),
            range(8, 8000, 192000, SampleFormat::F32),
        ];
        let c = input_candidates(&ranges, None, 48000, 0);
        assert_eq!((c[0].sample_format(), c[0].channels()), (SampleFormat::F32, 2));
        // Inputs 5/6 need the 8-channel format.
        let c = input_candidates(&ranges, None, 48000, 4);
        assert_eq!((c[0].sample_format(), c[0].channels()), (SampleFormat::F32, 8));
    }

    #[test]
    fn closest_rate_is_picked_inside_a_range() {
        let ranges = [range(2, 8000, 32000, SampleFormat::I16)];
        let c = input_candidates(&ranges, None, 48000, 0);
        assert_eq!(c[0].sample_rate(), 32000);
    }

    #[test]
    fn a_fixed_period_is_still_offered() {
        // WASAPI shared mode: one 10 ms period at 48 kHz.
        assert_eq!(buffer_choices(Some((480, 480))), vec![480]);
        assert_eq!(buffer_choices(Some((100, 300))), vec![100, 128, 256, 300]);
        assert_eq!(buffer_choices(None), BUFFER_SIZES.to_vec());
    }

    #[test]
    fn wasapi_modes_share_one_host() {
        assert_eq!(host_name(WASAPI_SHARED), "WASAPI");
        assert_eq!(host_name(WASAPI_EXCLUSIVE), "WASAPI");
        // Settings saved before the split still open shared mode.
        assert_eq!(host_name("WASAPI"), "WASAPI");
        assert_eq!(host_name("ASIO"), "ASIO");
        assert!(!is_exclusive(Some(WASAPI_SHARED)));
        assert_eq!(is_exclusive(Some(WASAPI_EXCLUSIVE)), cfg!(windows));
        assert!(!drivers().iter().any(|d| d == "WASAPI"));
    }

    #[test]
    fn summary_lists_rates_and_formats() {
        let caps = summarize_input(&[
            range(2, 44100, 96000, SampleFormat::I16),
            range(1, 16000, 16000, SampleFormat::F32),
        ]);
        assert_eq!(caps.rates, vec![16000, 44100, 48000, 88200, 96000]);
        assert_eq!(caps.max_channels, 2);
        assert_eq!(caps.formats, vec!["f32".to_string(), "i16".to_string()]);
        assert!(summarize_input(&[]).error.is_some());
    }
}
