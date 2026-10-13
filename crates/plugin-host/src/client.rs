//! Tessera's side of a hosted plug-in: the bridge process, its control
//! channel and the real-time [`Processor`] the engine calls.

use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use tessera_engine::fx::FxContext;
use tessera_engine::plugin::{base64, PluginBlock, PluginNoteKind, PluginProcessor, PluginRef};

use crate::protocol::{self, FromBridge, Line, ParamDesc, ToBridge};
use crate::shm::{self, Block, SharedBlock};
use crate::signal::{self, Wait};

/// How to start a bridge process: a program and the arguments that put it in
/// bridge mode (e.g. Tessera itself with `--plugin-bridge`).
#[derive(Debug, Clone)]
pub struct BridgeCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
}

impl BridgeCommand {
    /// The running executable with `--plugin-bridge`.
    pub fn current_exe() -> io::Result<BridgeCommand> {
        Ok(BridgeCommand {
            program: std::env::current_exe()?,
            args: vec!["--plugin-bridge".into()],
        })
    }

    /// A dedicated bridge executable (tests use `tessera-plugin-bridge`).
    pub fn program(path: impl Into<PathBuf>) -> BridgeCommand {
        BridgeCommand {
            program: path.into(),
            args: Vec::new(),
        }
    }

    pub(crate) fn command(&self) -> Command {
        let mut c = Command::new(&self.program);
        c.args(&self.args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // CREATE_NO_WINDOW: no console window per plug-in.
            c.creation_flags(0x0800_0000);
        }
        c
    }
}

/// Where an instance is in its life.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Loading,
    Ready,
    /// Could not be loaded (missing, failed to instantiate).
    Failed(String),
    /// The bridge process died; audio passes through untouched.
    Crashed,
}

/// Something the UI should know about.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Loaded,
    Failed(String),
    /// A parameter changed in the plug-in (editor, preset, automation).
    ParamChanged {
        index: u32,
        value: f64,
    },
    ParamsChanged,
    /// The plug-in's state changed (mark the project unsaved).
    Dirty,
    EditorOpened,
    EditorClosed(Option<String>),
    Crashed,
}

/// What the reader thread keeps up to date.
struct Shared {
    status: Mutex<Status>,
    name: Mutex<String>,
    params: Mutex<Vec<ParamDesc>>,
    has_editor: AtomicBool,
    editor_open: AtomicBool,
    latency: AtomicU32,
    events: Mutex<Vec<Event>>,
    states: Mutex<HashMap<u64, Option<String>>>,
    states_cv: Condvar,
    ready_cv: Condvar,
}

impl Shared {
    fn push(&self, e: Event) {
        self.events.lock().unwrap_or_else(|e| e.into_inner()).push(e);
    }
}

/// The real-time side, shared with the engine through
/// [`tessera_engine::plugin::register`].
pub struct Processor {
    block: SharedBlock,
    go: signal::Sender,
    done: signal::Receiver,
    ready: AtomicBool,
    crashed: AtomicBool,
    instrument: bool,
    /// Held while one engine (live or export) uses the block.
    busy: AtomicBool,
    /// Request number of the last request sent.
    sent: UnsafeCell<u32>,
    /// Parameter changes waiting for the next block (guarded by `busy`).
    pending: UnsafeCell<([shm::ParamChange; 256], usize)>,
    reset: AtomicBool,
    /// Blocks the plug-in did not finish in time.
    late: AtomicU64,
    processed: AtomicU64,
    #[cfg(windows)]
    process_handle: windows_sys::Win32::Foundation::HANDLE,
}

unsafe impl Send for Processor {}
unsafe impl Sync for Processor {}

impl Processor {
    fn try_lock(&self) -> bool {
        self.busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    fn unlock(&self) {
        self.busy.store(false, Ordering::Release);
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire) && !self.crashed.load(Ordering::Acquire)
    }

    /// Blocks the plug-in did not finish in time (it was skipped).
    pub fn late_blocks(&self) -> u64 {
        self.late.load(Ordering::Relaxed)
    }

    pub fn processed_blocks(&self) -> u64 {
        self.processed.load(Ordering::Relaxed)
    }

    /// Processes one chunk of at most [`shm::MAX_FRAMES`] frames.
    fn process_chunk(
        &self,
        left: &mut [f32],
        right: &mut [f32],
        notes: &[(u32, PluginNoteKind)],
        ctx: &FxContext,
        offset: usize,
    ) -> bool {
        let b: &mut Block = unsafe { &mut *self.block.block() };
        let sent = unsafe { &mut *self.sent.get() };
        let n = left.len().min(shm::MAX_FRAMES);
        if b.done.load(Ordering::Acquire) != *sent {
            // Still busy with a request that timed out: skip this block
            // rather than stall the engine again.
            return false;
        }
        b.frames = n as u32;
        let mut flags = 0;
        if self.reset.swap(false, Ordering::AcqRel) {
            flags |= shm::FLAG_RESET;
        }
        if ctx.offline {
            flags |= shm::FLAG_OFFLINE;
        }
        b.flags = flags;
        let mut k = 0;
        for &(frame, kind) in notes {
            if k == shm::MAX_NOTES {
                break;
            }
            let (kind, key, vel) = match kind {
                PluginNoteKind::On { key, velocity } => (shm::NOTE_ON, key, velocity),
                PluginNoteKind::Off { key } => (shm::NOTE_OFF, key, 0),
                PluginNoteKind::AllOff => (shm::NOTE_ALL_OFF, 0, 0),
            };
            b.notes[k] = shm::NoteEvent {
                frame,
                kind,
                key: key as u32,
                velocity: vel as u32,
            };
            k += 1;
        }
        b.n_notes = k as u32;
        let pending = unsafe { &mut *self.pending.get() };
        let np = pending.1.min(shm::MAX_PARAM_CHANGES);
        b.params[..np].copy_from_slice(&pending.0[..np]);
        b.n_params = np as u32;
        pending.1 = 0;
        let secs_per_beat = 60.0 / ctx.tempo.max(1.0);
        let beat = ctx.song_beat + offset as f64 / ctx.sample_rate as f64 / secs_per_beat;
        b.transport = shm::Transport {
            tempo: ctx.tempo,
            song_beat: beat,
            song_samples: (beat * secs_per_beat * ctx.sample_rate as f64) as i64,
            playing: ctx.playing as u32,
            beats_per_bar: ctx.beats_per_bar,
        };
        if self.instrument {
            b.input[0][..n].fill(0.0);
            b.input[1][..n].fill(0.0);
        } else {
            b.input[0][..n].copy_from_slice(&left[..n]);
            b.input[1][..n].copy_from_slice(&right[..n]);
        }
        b.status = 0;
        *sent = sent.wrapping_add(1);
        b.request.store(*sent, Ordering::Release);
        self.go.signal();

        // Give the plug-in a few blocks' worth of time in real time, and as
        // long as it needs when exporting.
        let budget = if ctx.offline {
            Duration::from_secs(30)
        } else {
            Duration::from_secs_f64((n as f64 / ctx.sample_rate as f64 * 3.0).max(0.02))
        };
        let start = Instant::now();
        loop {
            if b.done.load(Ordering::Acquire) == *sent {
                break;
            }
            let left_time = budget.saturating_sub(start.elapsed());
            if left_time.is_zero() {
                self.late.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            match self.done.wait(Some(left_time)) {
                Wait::Signalled | Wait::TimedOut => {}
                Wait::Closed => {
                    self.crashed.store(true, Ordering::Release);
                    return false;
                }
            }
        }
        if b.status != 1 {
            return false;
        }
        left[..n].copy_from_slice(&b.output[0][..n]);
        right[..n].copy_from_slice(&b.output[1][..n]);
        self.processed.fetch_add(1, Ordering::Relaxed);
        true
    }
}

impl PluginProcessor for Processor {
    fn process(&self, block: &mut PluginBlock) -> bool {
        if !self.is_ready() || !self.try_lock() {
            return false;
        }
        let frames = block.left.len().min(block.right.len());
        let mut ok = true;
        let mut pos = 0;
        // Notes for the current chunk, rebased to it. A fixed array keeps
        // this allocation-free.
        let mut chunk_notes = [(0u32, PluginNoteKind::AllOff); 256];
        while pos < frames && ok {
            let n = (frames - pos).min(shm::MAX_FRAMES);
            let mut k = 0;
            for note in block.notes {
                let f = note.frame as usize;
                if f >= pos && (f < pos + n || pos + n == frames) && k < chunk_notes.len() {
                    chunk_notes[k] = (((f - pos).min(n - 1)) as u32, note.kind);
                    k += 1;
                }
            }
            let (l, r) = (&mut block.left[pos..pos + n], &mut block.right[pos..pos + n]);
            ok = self.process_chunk(l, r, &chunk_notes[..k], block.ctx, pos);
            pos += n;
        }
        self.unlock();
        ok
    }

    fn set_param(&self, index: u32, value: f32) {
        // Spin briefly: the lock is only held for one block by the engine
        // that is also the one calling this, or by an export.
        for _ in 0..4 {
            if self.try_lock() {
                let pending = unsafe { &mut *self.pending.get() };
                // Replace an earlier change of the same parameter.
                let n = pending.1;
                let cap = pending.0.len();
                match pending.0[..n].iter_mut().find(|c| c.index == index) {
                    Some(c) => c.value = value.clamp(0.0, 1.0) as f64,
                    None if n < cap => {
                        pending.0[n] = shm::ParamChange {
                            frame: 0,
                            index,
                            value: value.clamp(0.0, 1.0) as f64,
                        };
                        pending.1 += 1;
                    }
                    None => {}
                }
                self.unlock();
                return;
            }
            std::hint::spin_loop();
        }
    }

    fn reset(&self) {
        self.reset.store(true, Ordering::Release);
    }
}

#[cfg(windows)]
impl Drop for Processor {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.process_handle) };
    }
}

/// One plug-in instance running in its own bridge process.
pub struct PluginInstance {
    pub plugin: PluginRef,
    processor: Arc<Processor>,
    shared: Arc<Shared>,
    stdin: Mutex<Option<ChildStdin>>,
    child: Mutex<Child>,
    next_request: AtomicU64,
}

impl PluginInstance {
    /// Starts a bridge and asks it to load `plugin`. Returns at once; the
    /// plug-in loads in the background (see [`PluginInstance::status`]).
    /// `state` is restored right after loading.
    pub fn spawn(
        bridge: &BridgeCommand,
        plugin: &PluginRef,
        sample_rate: f32,
        state: Option<&[u8]>,
    ) -> io::Result<PluginInstance> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tag = NEXT.fetch_add(1, Ordering::Relaxed);
        let block = SharedBlock::create(&format!("p{tag}"))?;
        let (go_tx, go_rx) = signal::channel()?;
        let (done_tx, done_rx) = signal::channel()?;

        let mut cmd = bridge.command();
        cmd.arg("host").arg(&block.name);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let (a, b) = (go_rx.raw(), done_tx.raw());
            cmd.arg(a.to_string()).arg(b.to_string());
            unsafe {
                cmd.pre_exec(move || {
                    signal::inherit(a);
                    signal::inherit(b);
                    Ok(())
                });
            }
        }
        #[cfg(windows)]
        {
            cmd.arg(&go_rx.name).arg(&done_tx.name);
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = cmd.spawn()?;
        // The bridge's ends now live in the child; closing ours lets us see
        // end-of-file when it dies.
        drop(go_rx);
        drop(done_tx);
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no bridge stdout"))?;

        #[cfg(windows)]
        let process_handle = unsafe {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
            use windows_sys::Win32::System::Threading::GetCurrentProcess;
            let mut h: HANDLE = std::ptr::null_mut();
            DuplicateHandle(
                GetCurrentProcess(),
                child.as_raw_handle() as HANDLE,
                GetCurrentProcess(),
                &mut h,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            );
            done_rx.watch_process(h);
            h
        };

        let processor = Arc::new(Processor {
            block,
            go: go_tx,
            done: done_rx,
            ready: AtomicBool::new(false),
            crashed: AtomicBool::new(false),
            instrument: plugin.instrument,
            busy: AtomicBool::new(false),
            sent: UnsafeCell::new(0),
            pending: UnsafeCell::new(([shm::ParamChange::default(); 256], 0)),
            reset: AtomicBool::new(false),
            late: AtomicU64::new(0),
            processed: AtomicU64::new(0),
            #[cfg(windows)]
            process_handle,
        });
        let shared = Arc::new(Shared {
            status: Mutex::new(Status::Loading),
            name: Mutex::new(plugin.name.clone()),
            params: Mutex::new(Vec::new()),
            has_editor: AtomicBool::new(false),
            editor_open: AtomicBool::new(false),
            latency: AtomicU32::new(0),
            events: Mutex::new(Vec::new()),
            states: Mutex::new(HashMap::new()),
            states_cv: Condvar::new(),
            ready_cv: Condvar::new(),
        });

        {
            let shared = shared.clone();
            let processor = processor.clone();
            std::thread::Builder::new()
                .name(format!("plugin-reader-{tag}"))
                .spawn(move || reader(BufReader::new(stdout), shared, processor))?;
        }

        let inst = PluginInstance {
            plugin: plugin.clone(),
            processor,
            shared,
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            next_request: AtomicU64::new(1),
        };
        inst.send(&ToBridge::Load {
            format: plugin.format,
            path: PathBuf::from(&plugin.path),
            id: plugin.id.clone(),
            sample_rate: sample_rate as f64,
            max_frames: shm::MAX_FRAMES as u32,
        });
        if let Some(s) = state {
            inst.send(&ToBridge::SetState {
                data: base64::encode(s),
            });
        }
        Ok(inst)
    }

    fn send(&self, msg: &ToBridge) -> bool {
        let mut guard = self.stdin.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(w) => protocol::write_line(w, msg, false).is_ok(),
            None => false,
        }
    }

    pub fn processor(&self) -> Arc<Processor> {
        self.processor.clone()
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Waits until the plug-in has loaded (or failed). Returns true when it
    /// is ready.
    pub fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut st = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        while *st == Status::Loading {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            st = self
                .shared
                .ready_cv
                .wait_timeout(st, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *st == Status::Ready
    }

    /// The plug-in's display name (from the plug-in once loaded).
    pub fn name(&self) -> String {
        self.shared.name.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn params(&self) -> Vec<ParamDesc> {
        self.shared.params.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Calls `f` with the parameter list without copying it.
    pub fn with_params<R>(&self, f: impl FnOnce(&[ParamDesc]) -> R) -> R {
        f(&self.shared.params.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn has_editor(&self) -> bool {
        self.shared.has_editor.load(Ordering::Relaxed)
    }

    pub fn editor_open(&self) -> bool {
        self.shared.editor_open.load(Ordering::Relaxed)
    }

    pub fn latency(&self) -> u32 {
        self.shared.latency.load(Ordering::Relaxed)
    }

    /// Changes a parameter from Tessera's UI.
    pub fn set_param(&self, index: u32, value: f64) {
        let value = value.clamp(0.0, 1.0);
        if let Some(p) = self
            .shared
            .params
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(index as usize)
        {
            p.value = value;
        }
        self.send(&ToBridge::SetParam { index, value });
    }

    pub fn show_editor(&self, title: &str) {
        self.send(&ToBridge::ShowEditor {
            title: title.to_string(),
        });
    }

    pub fn hide_editor(&self) {
        self.send(&ToBridge::HideEditor);
    }

    /// Restores a state saved with [`PluginInstance::save_state`].
    pub fn load_state(&self, data: &[u8]) {
        self.send(&ToBridge::SetState {
            data: base64::encode(data),
        });
    }

    /// Asks the plug-in for its state and waits for it.
    pub fn save_state(&self, timeout: Duration) -> Option<Vec<u8>> {
        if self.status() != Status::Ready {
            return None;
        }
        let request = self.next_request.fetch_add(1, Ordering::Relaxed);
        if !self.send(&ToBridge::GetState { request }) {
            return None;
        }
        let deadline = Instant::now() + timeout;
        let mut states = self.shared.states.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(s) = states.remove(&request) {
                return s.and_then(|s| base64::decode(&s));
            }
            let now = Instant::now();
            if now >= deadline || self.processor.crashed.load(Ordering::Acquire) {
                return None;
            }
            states = self
                .shared
                .states_cv
                .wait_timeout(states, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Events since the last call.
    pub fn take_events(&self) -> Vec<Event> {
        std::mem::take(&mut *self.shared.events.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        self.send(&ToBridge::Quit);
        // Closing stdin also tells the bridge to quit.
        self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + Duration::from_millis(1500);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    child.kill().ok();
                    child.wait().ok();
                    break;
                }
            }
        }
    }
}

fn reader(mut r: BufReader<std::process::ChildStdout>, shared: Arc<Shared>, processor: Arc<Processor>) {
    let set_status = |s: Status| {
        *shared.status.lock().unwrap_or_else(|e| e.into_inner()) = s;
        shared.ready_cv.notify_all();
    };
    loop {
        let msg = match protocol::read_line::<FromBridge>(&mut r, true) {
            Ok(Line::Message(m)) => m,
            Ok(Line::Other(text)) => {
                if !text.trim().is_empty() {
                    eprintln!("[plug-in] {text}");
                }
                continue;
            }
            Ok(Line::Eof) | Err(_) => break,
        };
        match msg {
            FromBridge::Ready => processor.block.unlink(),
            FromBridge::Loaded {
                name,
                params,
                latency,
                has_editor,
                ..
            } => {
                *shared.name.lock().unwrap_or_else(|e| e.into_inner()) = name;
                *shared.params.lock().unwrap_or_else(|e| e.into_inner()) = params;
                shared.latency.store(latency, Ordering::Relaxed);
                shared.has_editor.store(has_editor, Ordering::Relaxed);
                processor.ready.store(true, Ordering::Release);
                set_status(Status::Ready);
                shared.push(Event::Loaded);
            }
            FromBridge::Failed { error } => {
                set_status(Status::Failed(error.clone()));
                shared.push(Event::Failed(error));
            }
            FromBridge::ParamValues { values } => {
                let mut params = shared.params.lock().unwrap_or_else(|e| e.into_inner());
                for (index, value, text) in values {
                    if let Some(p) = params.get_mut(index as usize) {
                        p.value = value;
                        p.text = text;
                    }
                    shared.push(Event::ParamChanged { index, value });
                }
            }
            FromBridge::Params { params } => {
                *shared.params.lock().unwrap_or_else(|e| e.into_inner()) = params;
                shared.push(Event::ParamsChanged);
            }
            FromBridge::State { request, data } => {
                shared
                    .states
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(request, data);
                shared.states_cv.notify_all();
            }
            FromBridge::Dirty => shared.push(Event::Dirty),
            FromBridge::Latency { samples } => shared.latency.store(samples, Ordering::Relaxed),
            FromBridge::EditorOpened => {
                shared.editor_open.store(true, Ordering::Relaxed);
                shared.push(Event::EditorOpened);
            }
            FromBridge::EditorClosed { error } => {
                shared.editor_open.store(false, Ordering::Relaxed);
                shared.push(Event::EditorClosed(error));
            }
            FromBridge::Scanned { .. } => {}
        }
    }
    // The bridge is gone.
    processor.crashed.store(true, Ordering::Release);
    shared.editor_open.store(false, Ordering::Relaxed);
    let was = shared.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
    if !matches!(was, Status::Failed(_)) {
        set_status(Status::Crashed);
        shared.push(Event::Crashed);
    } else {
        shared.ready_cv.notify_all();
    }
    shared.states_cv.notify_all();
}
