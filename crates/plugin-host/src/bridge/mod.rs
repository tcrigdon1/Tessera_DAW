//! The bridge process: hosts one plug-in (or scans one file) on behalf of
//! Tessera.
//!
//! * The **main thread** owns the plug-in and handles control messages from
//!   stdin, the plug-in's timers and file descriptors (Linux GUIs need them)
//!   and its editor window. Until an editor is opened it runs a plain loop;
//!   the first editor starts a winit event loop that then drives everything.
//! * The **audio thread** waits for Tessera's signal, runs the plug-in on the
//!   shared-memory block and signals back.
//!
//! A crash here ends only this process: Tessera notices end-of-file on the
//! signal pipe and the control channel and bypasses the plug-in.

use std::ffi::{c_ulong, c_void, OsString};
use std::fs::File;
use std::io::{self, BufReader};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tessera_engine::plugin::{base64, PluginFormat};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::protocol::{self, FromBridge, Line, ParamDesc, ToBridge};
use crate::shm::{self, SharedBlock};
use crate::signal::{self, Wait};

mod clap;
pub mod module;
mod vst3;

/// How often the main thread services timers, file descriptors and
/// parameter changes.
const TICK: Duration = Duration::from_millis(10);

/// One block for the plug-in, borrowed from shared memory.
pub(crate) struct ProcessIo<'a> {
    pub frames: usize,
    pub input: [&'a [f32]; 2],
    pub output: [&'a mut [f32]; 2],
    pub notes: &'a [shm::NoteEvent],
    pub params: &'a [shm::ParamChange],
    pub transport: shm::Transport,
    pub reset: bool,
}

/// The audio-thread half of a hosted plug-in.
pub(crate) trait AudioSide: Send {
    fn process(&mut self, io: &mut ProcessIo) -> bool;
}

/// Things the plug-in reported on the main thread.
pub(crate) enum Notice {
    /// Parameters (by index) whose value changed.
    Values(Vec<u32>),
    /// The parameter list changed.
    Params,
    Dirty,
    Latency(u32),
    /// The editor wants a new size (in the window's units).
    Resize(u32, u32),
    /// The plug-in closed its own (floating) editor.
    EditorClosed,
}

/// A native parent window for an editor.
#[derive(Clone, Copy)]
pub(crate) enum Parent {
    #[allow(dead_code)]
    Win32(*mut c_void),
    #[allow(dead_code)]
    Cocoa(*mut c_void),
    #[allow(dead_code)]
    X11(c_ulong),
}

pub(crate) struct EditorSize {
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
}

/// The main-thread half of a hosted plug-in.
pub(crate) trait Hosted {
    fn name(&self) -> String;
    fn audio_io(&self) -> (u32, u32);
    /// All parameters with current values and texts.
    fn params(&mut self) -> Vec<ParamDesc>;
    fn param_value(&mut self, index: u32) -> f64;
    fn param_text(&mut self, index: u32, value: f64) -> String;
    /// A change made in Tessera's UI.
    fn set_param(&mut self, index: u32, value: f64);
    fn get_state(&mut self) -> Option<Vec<u8>>;
    fn set_state(&mut self, data: &[u8]) -> bool;
    fn latency(&mut self) -> u32;
    fn has_editor(&mut self) -> bool;
    /// Creates the editor and reports its size, before it is embedded.
    fn create_editor(&mut self, scale: f64) -> Result<EditorSize, String>;
    fn attach_editor(&mut self, parent: Parent) -> Result<(), String>;
    /// The window was resized; returns the size the editor settled on.
    fn editor_resized(&mut self, width: u32, height: u32) -> Option<(u32, u32)>;
    fn close_editor(&mut self);
    /// Main-thread housekeeping.
    fn idle(&mut self) -> Vec<Notice>;
    /// Takes the audio half (once, after loading).
    fn take_audio(&mut self) -> Option<Box<dyn AudioSide>>;
    /// Deactivates and destroys the plug-in; the audio half is gone by now.
    fn shutdown(&mut self);
}

/// Entry point for `tessera --plugin-bridge ...` and `tessera-plugin-bridge`.
/// Returns the exit code.
pub fn main(args: &[OsString]) -> i32 {
    let arg = |i: usize| args.get(i).map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let out = protocol_stdout();
    match arg(0).as_str() {
        "scan" => scan_main(out, std::path::Path::new(&args.get(1).cloned().unwrap_or_default())),
        "host" => match host_main(out, &arg(1), &arg(2), &arg(3)) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("tessera-plugin-bridge: {e}");
                1
            }
        },
        _ => {
            eprintln!("usage: --plugin-bridge scan <plug-in> | host <shm> <go> <done>");
            2
        }
    }
}

/// Moves stdout aside for the protocol and points the process's stdout at
/// stderr, so whatever plug-ins print cannot corrupt it.
fn protocol_stdout() -> Writer {
    #[cfg(unix)]
    unsafe {
        use std::os::fd::FromRawFd;
        let fd = libc::dup(1);
        if fd >= 0 && libc::dup2(2, 1) >= 0 {
            return Writer(Arc::new(Mutex::new(Box::new(File::from_raw_fd(fd)))));
        }
    }
    #[cfg(windows)]
    unsafe {
        use std::os::windows::io::FromRawHandle;
        use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
        use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        let out = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut dup: HANDLE = std::ptr::null_mut();
        if !out.is_null()
            && DuplicateHandle(
                GetCurrentProcess(),
                out,
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            ) != 0
        {
            SetStdHandle(STD_OUTPUT_HANDLE, GetStdHandle(STD_ERROR_HANDLE));
            return Writer(Arc::new(Mutex::new(Box::new(File::from_raw_handle(dup as _)))));
        }
    }
    Writer(Arc::new(Mutex::new(Box::new(io::stdout()))))
}

#[derive(Clone)]
struct Writer(Arc<Mutex<Box<dyn io::Write + Send>>>);

impl Writer {
    fn send(&self, msg: &FromBridge) {
        let mut w = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if protocol::write_line(&mut *w, msg, true).is_err() {
            // Tessera is gone.
            std::process::exit(0);
        }
    }
}

// ----- scanning ------------------------------------------------------------------

fn scan_main(out: Writer, path: &std::path::Path) -> i32 {
    let format = match path.extension().map(|e| e.to_string_lossy().to_lowercase()) {
        Some(e) if e == "clap" => PluginFormat::Clap,
        Some(e) if e == "vst3" => PluginFormat::Vst3,
        _ => {
            out.send(&FromBridge::Failed {
                error: "not a .vst3 or .clap file".into(),
            });
            return 1;
        }
    };
    let result = match format {
        PluginFormat::Clap => clap::scan(path),
        PluginFormat::Vst3 => vst3::scan(path),
    };
    match result {
        Ok(plugins) => {
            out.send(&FromBridge::Scanned { plugins });
            0
        }
        Err(error) => {
            out.send(&FromBridge::Failed { error });
            1
        }
    }
}

// ----- hosting -------------------------------------------------------------------

struct AudioSlot {
    side: Mutex<Option<Box<dyn AudioSide>>>,
}

fn host_main(out: Writer, shm_name: &str, go: &str, done: &str) -> Result<(), String> {
    let block = Arc::new(SharedBlock::open(shm_name).map_err(|e| format!("shared memory: {e}"))?);
    #[cfg(unix)]
    let (go_rx, done_tx) = unsafe {
        let g: i32 = go.parse().map_err(|_| "bad signal fd")?;
        let d: i32 = done.parse().map_err(|_| "bad signal fd")?;
        (signal::Receiver::from_raw(g), signal::Sender::from_raw(d))
    };
    #[cfg(windows)]
    let (go_rx, done_tx) = (
        signal::Receiver::open(go).map_err(|e| format!("signal: {e}"))?,
        signal::Sender::open(done).map_err(|e| format!("signal: {e}"))?,
    );

    let slot = Arc::new(AudioSlot { side: Mutex::new(None) });
    {
        let (block, slot) = (block.clone(), slot.clone());
        std::thread::Builder::new()
            .name("plugin-audio".into())
            .spawn(move || audio_thread(block, go_rx, done_tx, slot))
            .map_err(|e| e.to_string())?;
    }

    // Control messages from Tessera.
    let (tx, rx) = mpsc::channel::<ToBridge>();
    std::thread::Builder::new()
        .name("plugin-control".into())
        .spawn(move || {
            let mut r = BufReader::new(io::stdin());
            loop {
                match protocol::read_line::<ToBridge>(&mut r, false) {
                    Ok(Line::Message(m)) => {
                        if tx.send(m).is_err() {
                            break;
                        }
                    }
                    Ok(Line::Other(_)) => {}
                    Ok(Line::Eof) | Err(_) => {
                        let _ = tx.send(ToBridge::Quit);
                        break;
                    }
                }
                wake();
            }
            wake();
        })
        .map_err(|e| e.to_string())?;

    out.send(&FromBridge::Ready);
    let mut state = BridgeState {
        out,
        rx,
        slot,
        plugin: None,
        editor: None,
        quit: false,
        gui_failed: false,
        values: Vec::new(),
    };

    // Plain loop until an editor needs a window system.
    let title = loop {
        match state.rx.recv_timeout(TICK) {
            Ok(msg) => {
                if let Some(title) = state.handle(msg, None) {
                    break Some(title);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => state.quit = true,
        }
        state.tick();
        if state.quit {
            break None;
        }
    };
    if let Some(title) = title {
        run_gui(state, title);
    } else {
        state.shutdown();
    }
    Ok(())
}

fn audio_thread(block: Arc<SharedBlock>, go: signal::Receiver, done: signal::Sender, slot: Arc<AudioSlot>) {
    loop {
        if go.wait(None) == Wait::Closed {
            // Tessera is gone.
            std::process::exit(0);
        }
        let b = unsafe { &mut *block.block() };
        let request = b.request.load(Ordering::Acquire);
        if request == b.done.load(Ordering::Acquire) {
            continue;
        }
        let frames = (b.frames as usize).min(shm::MAX_FRAMES);
        let mut ok = false;
        if let Ok(mut guard) = slot.side.try_lock() {
            if let Some(side) = guard.as_mut() {
                let (inp, outp) = (&b.input, &mut b.output);
                let [out_l, out_r] = outp;
                let mut io = ProcessIo {
                    frames,
                    input: [&inp[0][..frames], &inp[1][..frames]],
                    output: [&mut out_l[..frames], &mut out_r[..frames]],
                    notes: &b.notes[..(b.n_notes as usize).min(shm::MAX_NOTES)],
                    params: &b.params[..(b.n_params as usize).min(shm::MAX_PARAM_CHANGES)],
                    transport: b.transport,
                    reset: b.flags & shm::FLAG_RESET != 0,
                };
                ok = side.process(&mut io);
            }
        }
        b.status = ok as u32;
        b.done.store(request, Ordering::Release);
        done.signal();
    }
}

static PROXY: OnceLock<Mutex<EventLoopProxy<()>>> = OnceLock::new();

/// Wakes the GUI event loop, if there is one, to handle new messages.
fn wake() {
    if let Some(p) = PROXY.get() {
        let _ = p.lock().unwrap_or_else(|e| e.into_inner()).send_event(());
    }
}

struct Editor {
    window: Window,
    /// Last size the editor and the window agreed on.
    size: (u32, u32),
}

struct BridgeState {
    out: Writer,
    rx: Receiver<ToBridge>,
    slot: Arc<AudioSlot>,
    plugin: Option<Box<dyn Hosted>>,
    editor: Option<Editor>,
    quit: bool,
    gui_failed: bool,
    /// Last values sent to Tessera, by index.
    values: Vec<f64>,
}

impl BridgeState {
    /// Handles one message. Returns an editor title when the editor must be
    /// opened but there is no event loop yet.
    fn handle(&mut self, msg: ToBridge, el: Option<&ActiveEventLoop>) -> Option<String> {
        match msg {
            ToBridge::Load {
                format,
                path,
                id,
                sample_rate,
                max_frames,
            } => {
                let loaded: Result<Box<dyn Hosted>, String> = match format {
                    PluginFormat::Clap => clap::load(&path, &id, sample_rate, max_frames).map(|p| Box::new(p) as _),
                    PluginFormat::Vst3 => vst3::load(&path, &id, sample_rate, max_frames),
                };
                match loaded {
                    Ok(mut p) => {
                        let audio = p.take_audio();
                        *self.slot.side.lock().unwrap_or_else(|e| e.into_inner()) = audio;
                        let params = p.params();
                        self.values = params.iter().map(|d| d.value).collect();
                        let (audio_inputs, audio_outputs) = p.audio_io();
                        let has_editor = p.has_editor();
                        let latency = p.latency();
                        self.out.send(&FromBridge::Loaded {
                            name: p.name(),
                            params,
                            latency,
                            has_editor,
                            audio_inputs,
                            audio_outputs,
                        });
                        self.plugin = Some(p);
                    }
                    Err(error) => self.out.send(&FromBridge::Failed { error }),
                }
            }
            ToBridge::SetParam { index, value } => {
                if let Some(p) = self.plugin.as_mut() {
                    p.set_param(index, value);
                    let text = p.param_text(index, value);
                    if let Some(v) = self.values.get_mut(index as usize) {
                        *v = value;
                    }
                    self.out.send(&FromBridge::ParamValues {
                        values: vec![(index, value, text)],
                    });
                }
            }
            ToBridge::GetState { request } => {
                let data = self
                    .plugin
                    .as_mut()
                    .and_then(|p| p.get_state())
                    .map(|d| base64::encode(&d));
                self.out.send(&FromBridge::State { request, data });
            }
            ToBridge::SetState { data } => {
                if let (Some(p), Some(bytes)) = (self.plugin.as_mut(), base64::decode(&data)) {
                    if p.set_state(&bytes) {
                        let params = p.params();
                        self.values = params.iter().map(|d| d.value).collect();
                        self.out.send(&FromBridge::Params { params });
                    }
                }
            }
            ToBridge::ShowEditor { title } => {
                if self.editor.is_some() {
                    if let Some(e) = &self.editor {
                        e.window.focus_window();
                    }
                    return None;
                }
                match el {
                    Some(el) => self.open_editor(el, &title),
                    None if self.gui_failed => self.out.send(&FromBridge::EditorClosed {
                        error: Some("no window system is available".into()),
                    }),
                    None => return Some(title),
                }
            }
            ToBridge::HideEditor => self.close_editor(),
            ToBridge::Quit => self.quit = true,
        }
        None
    }

    fn open_editor(&mut self, el: &ActiveEventLoop, title: &str) {
        let Some(p) = self.plugin.as_mut() else {
            self.out.send(&FromBridge::EditorClosed {
                error: Some("the plug-in is not loaded".into()),
            });
            return;
        };
        let scale = el
            .primary_monitor()
            .or_else(|| el.available_monitors().next())
            .map(|m| m.scale_factor())
            .unwrap_or(1.0);
        let result = (|| -> Result<Editor, String> {
            let size = p.create_editor(scale)?;
            let mut attrs = Window::default_attributes()
                .with_title(title)
                .with_resizable(size.resizable)
                .with_window_level(winit::window::WindowLevel::AlwaysOnTop);
            attrs = if cfg!(target_os = "macos") {
                attrs.with_inner_size(winit::dpi::LogicalSize::new(size.width, size.height))
            } else {
                attrs.with_inner_size(winit::dpi::PhysicalSize::new(size.width, size.height))
            };
            let window = el.create_window(attrs).map_err(|e| e.to_string())?;
            let parent = parent_of(&window)?;
            p.attach_editor(parent)?;
            Ok(Editor {
                window,
                size: (size.width, size.height),
            })
        })();
        match result {
            Ok(e) => {
                self.editor = Some(e);
                self.out.send(&FromBridge::EditorOpened);
            }
            Err(error) => {
                p.close_editor();
                self.out.send(&FromBridge::EditorClosed { error: Some(error) });
            }
        }
    }

    fn close_editor(&mut self) {
        if let Some(e) = self.editor.take() {
            if let Some(p) = self.plugin.as_mut() {
                p.close_editor();
            }
            drop(e);
            self.out.send(&FromBridge::EditorClosed { error: None });
        }
    }

    fn window_resized(&mut self, width: u32, height: u32) {
        let (Some(p), Some(e)) = (self.plugin.as_mut(), self.editor.as_mut()) else {
            return;
        };
        let (w, h) = if cfg!(target_os = "macos") {
            let s = e.window.scale_factor();
            ((width as f64 / s).round() as u32, (height as f64 / s).round() as u32)
        } else {
            (width, height)
        };
        if (w, h) == e.size || w == 0 || h == 0 {
            return;
        }
        if let Some(settled) = p.editor_resized(w, h) {
            e.size = settled;
            if settled != (w, h) {
                request_size(&e.window, settled);
            }
        }
    }

    /// Timers, file descriptors and changes the plug-in reported.
    fn tick(&mut self) {
        let Some(p) = self.plugin.as_mut() else { return };
        for n in p.idle() {
            match n {
                Notice::Values(indices) => {
                    let mut values = Vec::new();
                    for i in indices {
                        let v = p.param_value(i);
                        let old = self.values.get(i as usize).copied();
                        if old.map(|o| (o - v).abs() > 1e-9).unwrap_or(true) {
                            if let Some(o) = self.values.get_mut(i as usize) {
                                *o = v;
                            }
                            values.push((i, v, p.param_text(i, v)));
                        }
                    }
                    if !values.is_empty() {
                        self.out.send(&FromBridge::ParamValues { values });
                    }
                }
                Notice::Params => {
                    let params = p.params();
                    self.values = params.iter().map(|d| d.value).collect();
                    self.out.send(&FromBridge::Params { params });
                }
                Notice::Dirty => self.out.send(&FromBridge::Dirty),
                Notice::Latency(samples) => self.out.send(&FromBridge::Latency { samples }),
                Notice::Resize(w, h) => {
                    if let Some(e) = self.editor.as_mut() {
                        e.size = (w, h);
                        request_size(&e.window, (w, h));
                    }
                }
                Notice::EditorClosed => {
                    if let Some(e) = self.editor.take() {
                        drop(e);
                    }
                    self.out.send(&FromBridge::EditorClosed { error: None });
                }
            }
        }
    }

    fn shutdown(&mut self) {
        if let Some(e) = self.editor.take() {
            if let Some(p) = self.plugin.as_mut() {
                p.close_editor();
            }
            drop(e);
        }
        // Stop the audio half first.
        self.slot.side.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(mut p) = self.plugin.take() {
            p.shutdown();
        }
    }
}

fn request_size(window: &Window, (w, h): (u32, u32)) {
    let _ = if cfg!(target_os = "macos") {
        window.request_inner_size(winit::dpi::LogicalSize::new(w, h))
    } else {
        window.request_inner_size(winit::dpi::PhysicalSize::new(w, h))
    };
}

fn parent_of(window: &Window) -> Result<Parent, String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = window.window_handle().map_err(|e| e.to_string())?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Ok(Parent::Win32(h.hwnd.get() as *mut c_void)),
        RawWindowHandle::AppKit(h) => Ok(Parent::Cocoa(h.ns_view.as_ptr())),
        RawWindowHandle::Xlib(h) => Ok(Parent::X11(h.window as c_ulong)),
        RawWindowHandle::Xcb(h) => Ok(Parent::X11(h.window.get() as c_ulong)),
        _ => Err("plug-in editors need X11, Win32 or Cocoa windows".into()),
    }
}

struct GuiApp {
    state: BridgeState,
    pending: Option<String>,
}

impl GuiApp {
    fn drain(&mut self, el: &ActiveEventLoop) {
        while let Ok(msg) = self.state.rx.try_recv() {
            if let Some(title) = self.state.handle(msg, Some(el)) {
                self.state.open_editor(el, &title);
            }
        }
        if self.state.quit {
            self.state.shutdown();
            el.exit();
        }
    }
}

impl ApplicationHandler<()> for GuiApp {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if let Some(title) = self.pending.take() {
            self.state.open_editor(el, &title);
        }
    }

    fn user_event(&mut self, el: &ActiveEventLoop, _event: ()) {
        self.drain(el);
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => self.state.close_editor(),
            WindowEvent::Resized(size) => self.state.window_resized(size.width, size.height),
            _ => {}
        }
        if self.state.quit {
            self.state.shutdown();
            el.exit();
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.state.tick();
        self.drain(el);
        el.set_control_flow(ControlFlow::WaitUntil(Instant::now() + TICK));
    }
}

fn run_gui(mut state: BridgeState, title: String) {
    let mut builder = EventLoop::<()>::with_user_event();
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Plug-in editors embed into X11 windows (XWayland on Wayland).
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let el = match builder.build() {
        Ok(el) => el,
        Err(e) => {
            state.gui_failed = true;
            state.out.send(&FromBridge::EditorClosed {
                error: Some(format!("no window system: {e}")),
            });
            // Carry on without editors.
            loop {
                match state.rx.recv_timeout(TICK) {
                    Ok(msg) => {
                        state.handle(msg, None);
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => state.quit = true,
                }
                state.tick();
                if state.quit {
                    state.shutdown();
                    return;
                }
            }
        }
    };
    let _ = PROXY.set(Mutex::new(el.create_proxy()));
    let mut app = GuiApp {
        state,
        pending: Some(title),
    };
    if let Err(e) = el.run_app(&mut app) {
        eprintln!("tessera-plugin-bridge: event loop: {e}");
    }
    app.state.shutdown();
}
