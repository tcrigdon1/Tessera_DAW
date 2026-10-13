//! WASAPI exclusive mode (Windows).
//!
//! cpal only opens WASAPI in shared mode, where Windows mixes every app
//! through its audio engine and adds its own buffer. Exclusive mode hands the
//! device to Tessera alone at the buffer size it asks for, which is the
//! low-latency choice for interfaces without an ASIO driver.
//!
//! Each stream runs on its own thread: it opens the device there (COM objects
//! stay on the thread that made them), reports the format it negotiated, then
//! waits for the callback, starts the device and services it from the device
//! event. The loop converts between the device's sample format and
//! interleaved f32 in a buffer allocated before the stream starts, so it
//! never allocates.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

use windows::core::{w, HRESULT};
use windows::Win32::Devices::Properties::DEVPKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, HANDLE, PROPERTYKEY, WAIT_OBJECT_0};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioCaptureClient, IAudioClient, IAudioRenderClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED,
    AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED, AUDCLNT_SHAREMODE_EXCLUSIVE,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVEFORMATEXTENSIBLE_0,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW, CreateEventW, WaitForSingleObject,
};

/// Which way the audio goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Output,
    Input,
}

impl Flow {
    fn data_flow(self) -> EDataFlow {
        match self {
            Flow::Output => eRender,
            Flow::Input => eCapture,
        }
    }
}

/// Sample formats tried in exclusive mode, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sample {
    F32,
    I32,
    /// 24-bit samples in a 32-bit container.
    I24In32,
    /// Packed 24-bit samples.
    I24,
    I16,
}

impl Sample {
    const ALL: [Sample; 5] = [Sample::F32, Sample::I32, Sample::I24In32, Sample::I24, Sample::I16];

    fn bytes(self) -> usize {
        match self {
            Sample::F32 | Sample::I32 | Sample::I24In32 => 4,
            Sample::I24 => 3,
            Sample::I16 => 2,
        }
    }

    fn valid_bits(self) -> u16 {
        match self {
            Sample::F32 | Sample::I32 => 32,
            Sample::I24In32 | Sample::I24 => 24,
            Sample::I16 => 16,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Sample::F32 => "f32",
            Sample::I32 => "i32",
            Sample::I24In32 => "i24 (32-bit)",
            Sample::I24 => "i24",
            Sample::I16 => "i16",
        }
    }

    /// Writes one sample in the device format.
    fn write(self, v: f32, out: &mut [u8]) {
        let v = v.clamp(-1.0, 1.0);
        match self {
            Sample::F32 => out.copy_from_slice(&v.to_le_bytes()),
            Sample::I32 => out.copy_from_slice(&((v as f64 * i32::MAX as f64) as i32).to_le_bytes()),
            Sample::I24In32 => out.copy_from_slice(&(((v * 8_388_607.0) as i32) << 8).to_le_bytes()),
            Sample::I24 => out.copy_from_slice(&((v * 8_388_607.0) as i32).to_le_bytes()[..3]),
            Sample::I16 => out.copy_from_slice(&((v * i16::MAX as f32) as i16).to_le_bytes()),
        }
    }

    /// Reads one sample in the device format.
    fn read(self, b: &[u8]) -> f32 {
        match self {
            Sample::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            Sample::I32 | Sample::I24In32 => {
                (i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 / 2_147_483_648.0) as f32
            }
            Sample::I24 => (i32::from_le_bytes([0, b[0], b[1], b[2]]) as f64 / 2_147_483_648.0) as f32,
            Sample::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
        }
    }
}

fn wave_format(sample: Sample, rate: u32, channels: u16, mask: u32) -> WAVEFORMATEXTENSIBLE {
    let bytes = sample.bytes() as u16;
    let block = channels * bytes;
    WAVEFORMATEXTENSIBLE {
        Format: WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_EXTENSIBLE as u16,
            nChannels: channels,
            nSamplesPerSec: rate,
            nAvgBytesPerSec: rate * block as u32,
            nBlockAlign: block,
            wBitsPerSample: bytes * 8,
            cbSize: (std::mem::size_of::<WAVEFORMATEXTENSIBLE>() - std::mem::size_of::<WAVEFORMATEX>()) as u16,
        },
        Samples: WAVEFORMATEXTENSIBLE_0 {
            wValidBitsPerSample: sample.valid_bits(),
        },
        dwChannelMask: mask,
        SubFormat: if sample == Sample::F32 {
            KSDATAFORMAT_SUBTYPE_IEEE_FLOAT
        } else {
            KSDATAFORMAT_SUBTYPE_PCM
        },
    }
}

/// Speaker mask for a channel count: front left/right for stereo, none
/// (direct out) otherwise.
fn channel_mask(channels: u16, mix_channels: u16, mix_mask: u32) -> u32 {
    if channels == mix_channels && mix_mask != 0 {
        mix_mask
    } else if channels == 2 {
        0x3
    } else {
        0
    }
}

fn frames_to_hns(frames: u32, rate: u32) -> i64 {
    (10_000_000.0 * frames as f64 / rate as f64).round() as i64
}

fn hns_to_frames(hns: i64, rate: u32) -> u32 {
    (hns.max(0) as f64 * rate as f64 / 10_000_000.0).round() as u32
}

/// COM for the current thread, released when dropped.
struct Com(bool);

impl Com {
    fn init() -> Com {
        Com(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn describe(e: &windows::core::Error) -> String {
    describe_hr(e.code(), &e.message())
}

fn describe_hr(code: HRESULT, message: &str) -> String {
    if code == AUDCLNT_E_DEVICE_IN_USE {
        "the device is in use by another app in exclusive mode".into()
    } else if code == AUDCLNT_E_EXCLUSIVE_MODE_NOT_ALLOWED {
        "exclusive mode is switched off for this device in Windows Sound settings \
         (device Properties → Advanced → \"Allow applications to take exclusive control\")"
            .into()
    } else if message.is_empty() {
        format!("error {:#010x}", code.0)
    } else {
        message.trim().to_string()
    }
}

/// The device's friendly name, the same name cpal lists.
unsafe fn friendly_name(device: &IMMDevice) -> Option<String> {
    let store = device.OpenPropertyStore(STGM_READ).ok()?;
    let key = &DEVPKEY_Device_FriendlyName as *const _ as *const PROPERTYKEY;
    let mut value = store.GetValue(key).ok()?;
    let text = PropVariantToStringAlloc(&value).ok();
    let _ = PropVariantClear(&mut value);
    let text = text?;
    let name = text.to_string().ok();
    CoTaskMemFree(Some(text.0 as *const _));
    name
}

unsafe fn find_device(flow: Flow, name: Option<&str>) -> Result<(IMMDevice, String), String> {
    let enumerator: IMMDeviceEnumerator =
        CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| describe(&e))?;
    let device = match name {
        None => enumerator
            .GetDefaultAudioEndpoint(flow.data_flow(), eConsole)
            .map_err(|_| "no default device".to_string())?,
        Some(n) => {
            let all = enumerator
                .EnumAudioEndpoints(flow.data_flow(), DEVICE_STATE_ACTIVE)
                .map_err(|e| describe(&e))?;
            let count = all.GetCount().map_err(|e| describe(&e))?;
            (0..count)
                .filter_map(|i| all.Item(i).ok())
                .find(|d| friendly_name(d).as_deref() == Some(n))
                .ok_or_else(|| format!("device {n} not found"))?
        }
    };
    let name = friendly_name(&device).unwrap_or_else(|| "Audio device".into());
    Ok((device, name))
}

/// The shared-mode format and periods of a device.
struct DeviceFormat {
    rate: u32,
    channels: u16,
    mask: u32,
    /// Default and minimum device periods (100 ns units).
    default_period: i64,
    min_period: i64,
}

unsafe fn device_format(client: &IAudioClient) -> Result<DeviceFormat, String> {
    let mix = client.GetMixFormat().map_err(|e| describe(&e))?;
    let base: WAVEFORMATEX = std::ptr::read_unaligned(mix);
    let tag = base.wFormatTag as u32;
    let mask = if tag == WAVE_FORMAT_EXTENSIBLE {
        let ext: WAVEFORMATEXTENSIBLE = std::ptr::read_unaligned(mix as *const WAVEFORMATEXTENSIBLE);
        ext.dwChannelMask
    } else {
        0
    };
    CoTaskMemFree(Some(mix as *const _));
    let mut default_period = 0i64;
    let mut min_period = 0i64;
    client
        .GetDevicePeriod(Some(&mut default_period), Some(&mut min_period))
        .map_err(|e| describe(&e))?;
    Ok(DeviceFormat {
        rate: base.nSamplesPerSec,
        channels: base.nChannels,
        mask,
        default_period,
        min_period,
    })
}

unsafe fn supported(client: &IAudioClient, fmt: &WAVEFORMATEXTENSIBLE) -> bool {
    client
        .IsFormatSupported(AUDCLNT_SHAREMODE_EXCLUSIVE, &fmt.Format, None)
        .0
        == 0
}

/// What exclusive mode offers on a device, for Preferences → Audio.
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    pub rates: Vec<u32>,
    /// Smallest and largest buffer, in frames at the device's own rate.
    pub buffers: Option<(u32, u32)>,
}

/// Queries a device on a short-lived thread (COM stays off the UI thread).
pub fn capabilities(flow: Flow, name: Option<&str>, rates: &[u32]) -> Result<Capabilities, String> {
    let name = name.map(str::to_string);
    let rates = rates.to_vec();
    std::thread::spawn(move || {
        let _com = Com::init();
        unsafe {
            let (device, _) = find_device(flow, name.as_deref())?;
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| describe(&e))?;
            let f = device_format(&client)?;
            let mut channels = vec![f.channels];
            if f.channels != 2 {
                channels.push(2);
            }
            let ok = |rate: u32| {
                channels.iter().any(|&ch| {
                    Sample::ALL
                        .iter()
                        .any(|&s| supported(&client, &wave_format(s, rate, ch, channel_mask(ch, f.channels, f.mask))))
                })
            };
            let rates: Vec<u32> = rates.into_iter().filter(|&r| ok(r)).collect();
            if rates.is_empty() {
                return Err("the device offers no exclusive-mode format".into());
            }
            // Exclusive-mode event buffers top out around half a second.
            let min = hns_to_frames(f.min_period, f.rate).max(16);
            Ok(Capabilities {
                rates,
                buffers: Some((min, hns_to_frames(5_000_000, f.rate).max(min))),
            })
        }
    })
    .join()
    .unwrap_or_else(|_| Err("the exclusive-mode query failed".into()))
}

/// What an opened stream runs at.
#[derive(Debug, Clone)]
pub struct Info {
    pub name: String,
    pub rate: u32,
    pub channels: u16,
    /// Frames per buffer (one device period).
    pub frames: u32,
    pub format: &'static str,
    /// Buffer plus the latency the device reports, in frames.
    pub latency_frames: u32,
}

/// Fills an interleaved output buffer of `channels` channels.
pub type RenderFn = Box<dyn FnMut(&mut [f32], usize) + Send>;
/// Receives an interleaved input buffer of `channels` channels.
pub type CaptureFn = Box<dyn FnMut(&[f32], usize) + Send>;

enum Callback {
    Render(RenderFn),
    Capture(CaptureFn),
}

enum Msg {
    Ready(Info),
    Started,
    Failed(String),
}

/// A running exclusive-mode stream; dropping it stops the device.
pub struct Stream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A device opened in exclusive mode, waiting for its callback.
pub struct Pending {
    // Field order matters: the sender goes first so a stream never started
    // sees it closed and exits before `stream` joins it.
    callback: SyncSender<Callback>,
    messages: Receiver<Msg>,
    pub info: Info,
    stream: Stream,
}

impl Pending {
    pub fn start_output(self, render: RenderFn) -> Result<Stream, String> {
        self.start(Callback::Render(render))
    }

    pub fn start_input(self, capture: CaptureFn) -> Result<Stream, String> {
        self.start(Callback::Capture(capture))
    }

    fn start(self, callback: Callback) -> Result<Stream, String> {
        let Pending {
            callback: tx,
            messages,
            info: _,
            stream,
        } = self;
        tx.send(callback)
            .map_err(|_| "the exclusive-mode stream ended".to_string())?;
        match messages.recv() {
            Ok(Msg::Started) => Ok(stream),
            Ok(Msg::Failed(e)) => Err(e),
            _ => Err("the exclusive-mode stream ended".into()),
        }
    }
}

/// Opens a device in exclusive mode at `rate` (else its own rate) with
/// `buffer` frames per period (else its default period).
pub fn open(flow: Flow, name: Option<&str>, rate: Option<u32>, buffer: Option<u32>) -> Result<Pending, String> {
    let name = name.map(str::to_string);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let (msg_tx, msg_rx) = sync_channel(2);
    let (cb_tx, cb_rx) = sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("tessera-wasapi-exclusive".into())
        .spawn(move || {
            let _com = Com::init();
            unsafe { run(flow, name.as_deref(), rate, buffer, &stop2, &msg_tx, &cb_rx) };
        })
        .map_err(|e| e.to_string())?;
    let stream = Stream {
        stop,
        thread: Some(thread),
    };
    match msg_rx.recv() {
        Ok(Msg::Ready(info)) => Ok(Pending {
            callback: cb_tx,
            messages: msg_rx,
            info,
            stream,
        }),
        Ok(Msg::Failed(e)) => Err(e),
        _ => Err("the exclusive-mode stream ended".into()),
    }
}

struct Opened {
    client: IAudioClient,
    sample: Sample,
    rate: u32,
    channels: u16,
    frames: u32,
}

/// Finds a format the device takes exclusively and initializes it.
unsafe fn negotiate(device: &IMMDevice, name: &str, rate: Option<u32>, buffer: Option<u32>) -> Result<Opened, String> {
    let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| describe(&e))?;
    let f = device_format(&client)?;
    let mut rates = vec![rate.unwrap_or(f.rate)];
    if !rates.contains(&f.rate) {
        rates.push(f.rate);
    }
    let mut channels = vec![f.channels];
    if f.channels != 2 {
        channels.push(2);
    }
    let mut chosen = None;
    'search: for &r in &rates {
        for &ch in &channels {
            for s in Sample::ALL {
                let fmt = wave_format(s, r, ch, channel_mask(ch, f.channels, f.mask));
                if supported(&client, &fmt) {
                    chosen = Some((s, r, ch, fmt));
                    break 'search;
                }
            }
        }
    }
    let Some((sample, rate, channels, fmt)) = chosen else {
        return Err(format!(
            "{name} has no exclusive-mode format at {} Hz (another app may be holding it, \
             or exclusive mode is switched off in Windows Sound settings)",
            rates[0]
        ));
    };
    let frames = buffer.unwrap_or_else(|| hns_to_frames(f.default_period, rate));
    let mut period = frames_to_hns(frames, rate).max(f.min_period);
    let init = |client: &IAudioClient, period: i64| {
        client.Initialize(
            AUDCLNT_SHAREMODE_EXCLUSIVE,
            AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
            period,
            period,
            &fmt.Format,
            None,
        )
    };
    let client = match init(&client, period) {
        Ok(()) => client,
        Err(e) if e.code() == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED => {
            // The device wants a period it can align to: retry at the size it
            // proposes, on a fresh client (a failed Initialize can't be redone).
            let aligned = client.GetBufferSize().map_err(|e| describe(&e))?;
            drop(client);
            let client: IAudioClient = device.Activate(CLSCTX_ALL, None).map_err(|e| describe(&e))?;
            period = frames_to_hns(aligned, rate);
            init(&client, period).map_err(|e| format!("{name}: {}", describe(&e)))?;
            client
        }
        Err(e) => return Err(format!("{name}: {}", describe(&e))),
    };
    let frames = client.GetBufferSize().map_err(|e| describe(&e))?;
    Ok(Opened {
        client,
        sample,
        rate,
        channels,
        frames,
    })
}

/// The stream thread: open, report, wait for the callback, run.
unsafe fn run(
    flow: Flow,
    name: Option<&str>,
    rate: Option<u32>,
    buffer: Option<u32>,
    stop: &AtomicBool,
    messages: &SyncSender<Msg>,
    callbacks: &Receiver<Callback>,
) {
    let opened = find_device(flow, name).and_then(|(device, name)| {
        let o = negotiate(&device, &name, rate, buffer)?;
        let latency = o
            .client
            .GetStreamLatency()
            .map(|l| hns_to_frames(l, o.rate))
            .unwrap_or(0);
        Ok((o, name, latency))
    });
    let (o, name, latency) = match opened {
        Ok(x) => x,
        Err(e) => {
            let _ = messages.send(Msg::Failed(e));
            return;
        }
    };
    let info = Info {
        name,
        rate: o.rate,
        channels: o.channels,
        frames: o.frames,
        format: o.sample.label(),
        latency_frames: o.frames + latency,
    };
    if messages.send(Msg::Ready(info)).is_err() {
        return;
    }
    // Wait for the callback (none if the caller gave up on this stream).
    let Ok(callback) = callbacks.recv() else {
        return;
    };
    let event = match CreateEventW(None, false, false, None) {
        Ok(e) => e,
        Err(e) => {
            let _ = messages.send(Msg::Failed(describe(&e)));
            return;
        }
    };
    let result = service(&o, event, callback, stop, messages);
    let _ = CloseHandle(event);
    if let Err(e) = result {
        let _ = messages.send(Msg::Failed(e.clone()));
        eprintln!("tessera: WASAPI exclusive stream stopped: {e}");
    }
}

unsafe fn service(
    o: &Opened,
    event: HANDLE,
    callback: Callback,
    stop: &AtomicBool,
    messages: &SyncSender<Msg>,
) -> Result<(), String> {
    o.client.SetEventHandle(event).map_err(|e| describe(&e))?;
    let channels = o.channels as usize;
    let frames = o.frames as usize;
    let bytes = o.sample.bytes();
    let block = bytes * channels;
    let mut buf = vec![0.0f32; frames * channels];
    let mut task_index = 0u32;
    let task = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index).ok();
    let result = match callback {
        Callback::Render(mut render) => {
            let client: IAudioRenderClient = o.client.GetService().map_err(|e| describe(&e))?;
            // Start from silence so the first period isn't garbage.
            client.GetBuffer(o.frames).map_err(|e| describe(&e))?;
            client
                .ReleaseBuffer(o.frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
                .map_err(|e| describe(&e))?;
            o.client.Start().map_err(|e| describe(&e))?;
            let _ = messages.send(Msg::Started);
            let mut result = Ok(());
            while !stop.load(Ordering::Relaxed) {
                if WaitForSingleObject(event, 2000) != WAIT_OBJECT_0 {
                    result = Err("the device stopped responding".to_string());
                    break;
                }
                let ptr = match client.GetBuffer(o.frames) {
                    Ok(p) => p,
                    Err(e) => {
                        result = Err(describe(&e));
                        break;
                    }
                };
                render(&mut buf, channels);
                let out = std::slice::from_raw_parts_mut(ptr, frames * block);
                for (v, dst) in buf.iter().zip(out.chunks_exact_mut(bytes)) {
                    o.sample.write(*v, dst);
                }
                if let Err(e) = client.ReleaseBuffer(o.frames, 0) {
                    result = Err(describe(&e));
                    break;
                }
            }
            result
        }
        Callback::Capture(mut capture) => {
            let client: IAudioCaptureClient = o.client.GetService().map_err(|e| describe(&e))?;
            o.client.Start().map_err(|e| describe(&e))?;
            let _ = messages.send(Msg::Started);
            let mut result = Ok(());
            'run: while !stop.load(Ordering::Relaxed) {
                if WaitForSingleObject(event, 2000) != WAIT_OBJECT_0 {
                    result = Err("the device stopped responding".to_string());
                    break;
                }
                loop {
                    match client.GetNextPacketSize() {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(e) => {
                            result = Err(describe(&e));
                            break 'run;
                        }
                    }
                    let mut ptr = std::ptr::null_mut();
                    let mut n = 0u32;
                    let mut flags = 0u32;
                    if let Err(e) = client.GetBuffer(&mut ptr, &mut n, &mut flags, None, None) {
                        result = Err(describe(&e));
                        break 'run;
                    }
                    let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                    let data = std::slice::from_raw_parts(ptr as *const u8, n as usize * block);
                    for chunk in data.chunks(frames * block) {
                        let len = chunk.len() / bytes;
                        for (dst, src) in buf[..len].iter_mut().zip(chunk.chunks_exact(bytes)) {
                            *dst = if silent { 0.0 } else { o.sample.read(src) };
                        }
                        capture(&buf[..len], channels);
                    }
                    if let Err(e) = client.ReleaseBuffer(n) {
                        result = Err(describe(&e));
                        break 'run;
                    }
                }
            }
            result
        }
    };
    let _ = o.client.Stop();
    if let Some(t) = task {
        let _ = AvRevertMmThreadCharacteristics(t);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_trip() {
        for s in Sample::ALL {
            let mut b = [0u8; 4];
            for v in [0.0f32, 0.5, -0.5, 0.99] {
                s.write(v, &mut b[..s.bytes()]);
                let back = s.read(&b[..s.bytes()]);
                assert!((back - v).abs() < 1e-3, "{s:?} {v} -> {back}");
            }
        }
    }

    #[test]
    fn periods_convert() {
        assert_eq!(frames_to_hns(480, 48000), 100_000);
        assert_eq!(hns_to_frames(100_000, 48000), 480);
    }
}
