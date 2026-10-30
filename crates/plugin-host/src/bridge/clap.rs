//! Hosting CLAP plug-ins.

use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use clap_sys::audio_buffer::clap_audio_buffer;
use clap_sys::entry::clap_plugin_entry;
use clap_sys::events::*;
use clap_sys::ext::audio_ports::*;
use clap_sys::ext::gui::*;
use clap_sys::ext::latency::*;
use clap_sys::ext::log::*;
use clap_sys::ext::note_ports::*;
use clap_sys::ext::params::*;
use clap_sys::ext::posix_fd_support::*;
use clap_sys::ext::state::*;
use clap_sys::ext::thread_check::*;
use clap_sys::ext::timer_support::*;
use clap_sys::factory::plugin_factory::*;
use clap_sys::fixedpoint::{CLAP_BEATTIME_FACTOR, CLAP_SECTIME_FACTOR};
use clap_sys::host::clap_host;
use clap_sys::id::clap_id;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::process::*;
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::version::{clap_version_is_compatible, CLAP_VERSION};
use tessera_engine::plugin::PluginFormat;

use super::module::Module;
use super::{AudioSide, EditorSize, Hosted, Notice, Parent, ProcessIo};
use crate::protocol::{ParamDesc, PluginInfo};
use crate::shm;

unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().to_string()
    }
}

fn chars(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).to_string()
}

/// A loaded CLAP module with its plug-in factory.
struct Entry {
    module: Module,
    entry: *const clap_plugin_entry,
    factory: *const clap_plugin_factory,
}

impl Entry {
    fn open(path: &Path) -> Result<Entry, String> {
        let module = Module::load(PluginFormat::Clap, path)?;
        unsafe {
            let entry: *const clap_plugin_entry = module
                .symbol::<*const clap_plugin_entry>(b"clap_entry\0")
                .ok_or("not a CLAP plug-in (no clap_entry)")?;
            if entry.is_null() || !clap_version_is_compatible((*entry).clap_version) {
                return Err("incompatible CLAP version".into());
            }
            let p = CString::new(path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
            let init = (*entry).init.ok_or("clap_entry has no init")?;
            if !init(p.as_ptr()) {
                return Err("the plug-in failed to initialise".into());
            }
            let factory = (*entry).get_factory.ok_or("clap_entry has no get_factory")?(CLAP_PLUGIN_FACTORY_ID.as_ptr())
                as *const clap_plugin_factory;
            if factory.is_null() {
                if let Some(deinit) = (*entry).deinit {
                    deinit();
                }
                return Err("the plug-in has no plug-in factory".into());
            }
            Ok(Entry { module, entry, factory })
        }
    }

    fn descriptors(&self) -> Vec<*const clap_plugin_descriptor> {
        unsafe {
            let f = &*self.factory;
            let n = f.get_plugin_count.map(|c| c(self.factory)).unwrap_or(0);
            (0..n)
                .filter_map(|i| f.get_plugin_descriptor.map(|g| g(self.factory, i)))
                .filter(|d| !d.is_null())
                .collect()
        }
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        unsafe {
            if let Some(deinit) = (*self.entry).deinit {
                deinit();
            }
        }
        let _ = &self.module;
    }
}

unsafe fn features(d: &clap_plugin_descriptor) -> Vec<String> {
    let mut out = Vec::new();
    let mut p = d.features;
    if p.is_null() {
        return out;
    }
    while !(*p).is_null() {
        out.push(cstr(*p));
        p = p.add(1);
    }
    out
}

pub fn scan(path: &Path) -> Result<Vec<PluginInfo>, String> {
    let entry = Entry::open(path)?;
    let mut v = Vec::new();
    for d in entry.descriptors() {
        unsafe {
            let d = &*d;
            let feats = features(d);
            let instrument = feats.iter().any(|f| f == "instrument");
            let effect = feats.iter().any(|f| f == "audio-effect" || f == "analyzer");
            if !instrument && !effect {
                // Note effects and the like are not supported yet.
                continue;
            }
            v.push(PluginInfo {
                format: PluginFormat::Clap,
                path: path.to_path_buf(),
                id: cstr(d.id),
                name: cstr(d.name),
                vendor: cstr(d.vendor),
                version: cstr(d.version),
                instrument,
                category: feats.join("|"),
            });
        }
    }
    Ok(v)
}

// ----- the host side the plug-in sees ----------------------------------------------

struct Timer {
    id: clap_id,
    period: Duration,
    next: Instant,
}

/// What `clap_host::host_data` points to.
struct HostData {
    host: clap_host,
    main_thread: ThreadId,
    audio_thread: OnceLock<ThreadId>,
    callback: AtomicBool,
    restart: AtomicBool,
    rescan: AtomicU32,
    dirty: AtomicBool,
    latency_changed: AtomicBool,
    resize: Mutex<Option<(u32, u32)>>,
    gui_closed: AtomicBool,
    timers: Mutex<Vec<Timer>>,
    next_timer: AtomicU32,
    fds: Mutex<Vec<(i32, u32)>>,
}

unsafe impl Send for HostData {}
unsafe impl Sync for HostData {}

unsafe fn data<'a>(host: *const clap_host) -> &'a HostData {
    &*((*host).host_data as *const HostData)
}

static HOST_NAME: &CStr = c"Tessera";
static HOST_VENDOR: &CStr = c"Tessera";
static HOST_URL: &CStr = c"https://github.com/tcrigdon1/Tessera_DAW";
static HOST_VERSION: &CStr = c"0.1.0";

unsafe extern "C" fn host_get_extension(_host: *const clap_host, id: *const c_char) -> *const c_void {
    if id.is_null() {
        return std::ptr::null();
    }
    let id = CStr::from_ptr(id);
    if id == CLAP_EXT_LOG {
        &HOST_LOG as *const _ as *const c_void
    } else if id == CLAP_EXT_THREAD_CHECK {
        &HOST_THREAD_CHECK as *const _ as *const c_void
    } else if id == CLAP_EXT_PARAMS {
        &HOST_PARAMS as *const _ as *const c_void
    } else if id == CLAP_EXT_STATE {
        &HOST_STATE as *const _ as *const c_void
    } else if id == CLAP_EXT_GUI {
        &HOST_GUI as *const _ as *const c_void
    } else if id == CLAP_EXT_LATENCY {
        &HOST_LATENCY as *const _ as *const c_void
    } else if id == CLAP_EXT_TIMER_SUPPORT {
        &HOST_TIMER as *const _ as *const c_void
    } else if id == CLAP_EXT_POSIX_FD_SUPPORT && cfg!(unix) {
        &HOST_FD as *const _ as *const c_void
    } else if id == CLAP_EXT_AUDIO_PORTS {
        &HOST_AUDIO_PORTS as *const _ as *const c_void
    } else if id == CLAP_EXT_NOTE_PORTS {
        &HOST_NOTE_PORTS as *const _ as *const c_void
    } else {
        std::ptr::null()
    }
}

unsafe extern "C" fn host_request_restart(host: *const clap_host) {
    data(host).restart.store(true, Ordering::Release);
}

unsafe extern "C" fn host_request_process(_host: *const clap_host) {
    // Tessera processes continuously.
}

unsafe extern "C" fn host_request_callback(host: *const clap_host) {
    data(host).callback.store(true, Ordering::Release);
}

unsafe extern "C" fn log_log(_host: *const clap_host, severity: clap_log_severity, msg: *const c_char) {
    if severity >= CLAP_LOG_WARNING {
        eprintln!("[clap] {}", cstr(msg));
    }
}
static HOST_LOG: clap_host_log = clap_host_log { log: Some(log_log) };

unsafe extern "C" fn tc_main(host: *const clap_host) -> bool {
    std::thread::current().id() == data(host).main_thread
}
unsafe extern "C" fn tc_audio(host: *const clap_host) -> bool {
    data(host).audio_thread.get() == Some(&std::thread::current().id())
}
static HOST_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(tc_main),
    is_audio_thread: Some(tc_audio),
};

unsafe extern "C" fn params_rescan(host: *const clap_host, flags: clap_param_rescan_flags) {
    data(host).rescan.fetch_or(flags | 0x8000_0000, Ordering::AcqRel);
}
unsafe extern "C" fn params_clear(_host: *const clap_host, _id: clap_id, _flags: clap_param_clear_flags) {}
unsafe extern "C" fn params_request_flush(_host: *const clap_host) {
    // Process runs continuously, which flushes parameters.
}
static HOST_PARAMS: clap_host_params = clap_host_params {
    rescan: Some(params_rescan),
    clear: Some(params_clear),
    request_flush: Some(params_request_flush),
};

unsafe extern "C" fn state_mark_dirty(host: *const clap_host) {
    data(host).dirty.store(true, Ordering::Release);
}
static HOST_STATE: clap_host_state = clap_host_state {
    mark_dirty: Some(state_mark_dirty),
};

unsafe extern "C" fn gui_resize_hints_changed(_host: *const clap_host) {}
unsafe extern "C" fn gui_request_resize(host: *const clap_host, w: u32, h: u32) -> bool {
    *data(host).resize.lock().unwrap_or_else(|e| e.into_inner()) = Some((w, h));
    true
}
unsafe extern "C" fn gui_request_show(_host: *const clap_host) -> bool {
    false
}
unsafe extern "C" fn gui_request_hide(_host: *const clap_host) -> bool {
    false
}
unsafe extern "C" fn gui_closed(host: *const clap_host, _was_destroyed: bool) {
    data(host).gui_closed.store(true, Ordering::Release);
}
static HOST_GUI: clap_host_gui = clap_host_gui {
    resize_hints_changed: Some(gui_resize_hints_changed),
    request_resize: Some(gui_request_resize),
    request_show: Some(gui_request_show),
    request_hide: Some(gui_request_hide),
    closed: Some(gui_closed),
};

unsafe extern "C" fn latency_changed(host: *const clap_host) {
    data(host).latency_changed.store(true, Ordering::Release);
}
static HOST_LATENCY: clap_host_latency = clap_host_latency {
    changed: Some(latency_changed),
};

unsafe extern "C" fn timer_register(host: *const clap_host, period_ms: u32, id: *mut clap_id) -> bool {
    let d = data(host);
    let tid = d.next_timer.fetch_add(1, Ordering::Relaxed);
    let period = Duration::from_millis(period_ms.max(5) as u64);
    d.timers.lock().unwrap_or_else(|e| e.into_inner()).push(Timer {
        id: tid,
        period,
        next: Instant::now() + period,
    });
    if !id.is_null() {
        *id = tid;
    }
    true
}
unsafe extern "C" fn timer_unregister(host: *const clap_host, id: clap_id) -> bool {
    let mut t = data(host).timers.lock().unwrap_or_else(|e| e.into_inner());
    let n = t.len();
    t.retain(|t| t.id != id);
    t.len() != n
}
static HOST_TIMER: clap_host_timer_support = clap_host_timer_support {
    register_timer: Some(timer_register),
    unregister_timer: Some(timer_unregister),
};

unsafe extern "C" fn fd_register(host: *const clap_host, fd: i32, flags: clap_posix_fd_flags) -> bool {
    let mut f = data(host).fds.lock().unwrap_or_else(|e| e.into_inner());
    f.retain(|(x, _)| *x != fd);
    f.push((fd, flags));
    true
}
unsafe extern "C" fn fd_modify(host: *const clap_host, fd: i32, flags: clap_posix_fd_flags) -> bool {
    fd_register(host, fd, flags)
}
unsafe extern "C" fn fd_unregister(host: *const clap_host, fd: i32) -> bool {
    data(host)
        .fds
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(x, _)| *x != fd);
    true
}
static HOST_FD: clap_host_posix_fd_support = clap_host_posix_fd_support {
    register_fd: Some(fd_register),
    modify_fd: Some(fd_modify),
    unregister_fd: Some(fd_unregister),
};

unsafe extern "C" fn ports_is_rescan_flag_supported(_host: *const clap_host, _flag: u32) -> bool {
    false
}
unsafe extern "C" fn ports_rescan(_host: *const clap_host, _flags: u32) {}
static HOST_AUDIO_PORTS: clap_host_audio_ports = clap_host_audio_ports {
    is_rescan_flag_supported: Some(ports_is_rescan_flag_supported),
    rescan: Some(ports_rescan),
};

unsafe extern "C" fn notes_dialects(_host: *const clap_host) -> clap_note_dialect {
    CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI
}
unsafe extern "C" fn notes_rescan(_host: *const clap_host, _flags: u32) {}
static HOST_NOTE_PORTS: clap_host_note_ports = clap_host_note_ports {
    supported_dialects: Some(notes_dialects),
    rescan: Some(notes_rescan),
};

// ----- streams --------------------------------------------------------------------

unsafe extern "C" fn ostream_write(s: *const clap_ostream, buf: *const c_void, size: u64) -> i64 {
    if buf.is_null() || size == 0 {
        // A null pointer is never a valid slice, even an empty one.
        return if size == 0 { 0 } else { -1 };
    }
    let v = &mut *((*s).ctx as *mut Vec<u8>);
    v.extend_from_slice(std::slice::from_raw_parts(buf as *const u8, size as usize));
    size as i64
}

struct ReadCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

unsafe extern "C" fn istream_read(s: *const clap_istream, buf: *mut c_void, size: u64) -> i64 {
    if buf.is_null() {
        return if size == 0 { 0 } else { -1 };
    }
    let c = &mut *((*s).ctx as *mut ReadCursor);
    let n = (c.data.len() - c.pos).min(size as usize);
    std::ptr::copy_nonoverlapping(c.data.as_ptr().add(c.pos), buf as *mut u8, n);
    c.pos += n;
    n as i64
}

// ----- the plug-in -------------------------------------------------------------------

#[derive(Clone, Copy)]
struct ParamMeta {
    id: clap_id,
    cookie: *mut c_void,
    min: f64,
    max: f64,
    flags: u32,
}

unsafe impl Send for ParamMeta {}

impl ParamMeta {
    fn to_plain(self, n: f64) -> f64 {
        let v = self.min + n.clamp(0.0, 1.0) * (self.max - self.min);
        if self.flags & CLAP_PARAM_IS_STEPPED != 0 {
            v.round()
        } else {
            v
        }
    }

    fn to_norm(self, v: f64) -> f64 {
        if self.max > self.min {
            ((v - self.min) / (self.max - self.min)).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

struct Ext {
    params: *const clap_plugin_params,
    state: *const clap_plugin_state,
    gui: *const clap_plugin_gui,
    latency: *const clap_plugin_latency,
    timer: *const clap_plugin_timer_support,
    #[cfg_attr(not(unix), allow(dead_code))]
    fd: *const clap_plugin_posix_fd_support,
    audio_ports: *const clap_plugin_audio_ports,
    note_ports: *const clap_plugin_note_ports,
}

unsafe fn ext<T>(plugin: *const clap_plugin, id: &CStr) -> *const T {
    match (*plugin).get_extension {
        Some(g) => g(plugin, id.as_ptr()) as *const T,
        None => std::ptr::null(),
    }
}

/// Parameter table shared with the audio half (it only `try_lock`s).
type Table = Arc<Mutex<Vec<ParamMeta>>>;

/// Flags between the halves for restarts.
struct Control {
    pause: AtomicBool,
    paused: AtomicBool,
}

pub struct ClapHosted {
    plugin: *const clap_plugin,
    ext: Ext,
    host: Box<HostData>,
    name: String,
    table: Table,
    to_audio: rtrb::Producer<(u32, f64)>,
    from_audio: rtrb::Consumer<u32>,
    audio: Option<Box<ClapAudio>>,
    control: Arc<Control>,
    sample_rate: f64,
    max_frames: u32,
    gui_created: bool,
    gui_api: &'static CStr,
    io: (u32, u32),
    // Keep the module loaded for as long as the plug-in exists.
    _entry: Entry,
}

fn port_channels(plugin: *const clap_plugin, ports: *const clap_plugin_audio_ports, input: bool) -> Vec<u32> {
    unsafe {
        if ports.is_null() {
            // No audio-ports extension: assume stereo out, stereo in for
            // effects is not guaranteed; use none.
            return if input { Vec::new() } else { vec![2] };
        }
        let p = &*ports;
        let n = p.count.map(|c| c(plugin, input)).unwrap_or(0);
        (0..n)
            .map(|i| {
                let mut info: clap_audio_port_info = std::mem::zeroed();
                if p.get.map(|g| g(plugin, i, input, &mut info)).unwrap_or(false) {
                    info.channel_count
                } else {
                    2
                }
            })
            .collect()
    }
}

pub fn load(path: &Path, id: &str, sample_rate: f64, max_frames: u32) -> Result<ClapHosted, String> {
    let entry = Entry::open(path)?;
    let desc = entry
        .descriptors()
        .into_iter()
        .find(|d| unsafe { cstr((**d).id) } == id)
        .ok_or_else(|| format!("{} no longer contains the plug-in {id}", path.display()))?;
    let name = unsafe { cstr((*desc).name) };
    let mut host = Box::new(HostData {
        host: clap_host {
            clap_version: CLAP_VERSION,
            host_data: std::ptr::null_mut(),
            name: HOST_NAME.as_ptr(),
            vendor: HOST_VENDOR.as_ptr(),
            url: HOST_URL.as_ptr(),
            version: HOST_VERSION.as_ptr(),
            get_extension: Some(host_get_extension),
            request_restart: Some(host_request_restart),
            request_process: Some(host_request_process),
            request_callback: Some(host_request_callback),
        },
        main_thread: std::thread::current().id(),
        audio_thread: OnceLock::new(),
        callback: AtomicBool::new(false),
        restart: AtomicBool::new(false),
        rescan: AtomicU32::new(0),
        dirty: AtomicBool::new(false),
        latency_changed: AtomicBool::new(false),
        resize: Mutex::new(None),
        gui_closed: AtomicBool::new(false),
        timers: Mutex::new(Vec::new()),
        next_timer: AtomicU32::new(1),
        fds: Mutex::new(Vec::new()),
    });
    host.host.host_data = &*host as *const HostData as *mut c_void;

    let c_id = CString::new(id).map_err(|e| e.to_string())?;
    let plugin = unsafe {
        let create = (*entry.factory)
            .create_plugin
            .ok_or("the factory cannot create plug-ins")?;
        create(entry.factory, &host.host, c_id.as_ptr())
    };
    if plugin.is_null() {
        return Err(format!("the plug-in {name} could not be created"));
    }
    unsafe {
        if !(*plugin).init.map(|f| f(plugin)).unwrap_or(false) {
            if let Some(d) = (*plugin).destroy {
                d(plugin);
            }
            return Err(format!("{name} failed to initialise"));
        }
    }
    let ext = unsafe {
        Ext {
            params: ext(plugin, CLAP_EXT_PARAMS),
            state: ext(plugin, CLAP_EXT_STATE),
            gui: ext(plugin, CLAP_EXT_GUI),
            latency: ext(plugin, CLAP_EXT_LATENCY),
            timer: ext(plugin, CLAP_EXT_TIMER_SUPPORT),
            fd: ext(plugin, CLAP_EXT_POSIX_FD_SUPPORT),
            audio_ports: ext(plugin, CLAP_EXT_AUDIO_PORTS),
            note_ports: ext(plugin, CLAP_EXT_NOTE_PORTS),
        }
    };
    let ins = port_channels(plugin, ext.audio_ports, true);
    let outs = port_channels(plugin, ext.audio_ports, false);
    let midi_only = unsafe {
        if ext.note_ports.is_null() {
            false
        } else {
            let np = &*ext.note_ports;
            let mut info: clap_note_port_info = std::mem::zeroed();
            if np.count.map(|c| c(plugin, true)).unwrap_or(0) > 0
                && np.get.map(|g| g(plugin, 0, true, &mut info)).unwrap_or(false)
            {
                info.supported_dialects & CLAP_NOTE_DIALECT_CLAP == 0
            } else {
                false
            }
        }
    };
    unsafe {
        if !(*plugin)
            .activate
            .map(|a| a(plugin, sample_rate, 1, max_frames))
            .unwrap_or(false)
        {
            if let Some(d) = (*plugin).destroy {
                d(plugin);
            }
            return Err(format!("{name} could not be activated"));
        }
    }
    let (to_audio, from_main) = rtrb::RingBuffer::new(1024);
    let (to_main, from_audio) = rtrb::RingBuffer::new(1024);
    let control = Arc::new(Control {
        pause: AtomicBool::new(false),
        paused: AtomicBool::new(false),
    });
    let mut hosted = ClapHosted {
        plugin,
        ext,
        host,
        name,
        table: Arc::new(Mutex::new(Vec::new())),
        to_audio,
        from_audio,
        audio: None,
        control: control.clone(),
        sample_rate,
        max_frames,
        gui_created: false,
        gui_api: if cfg!(target_os = "windows") {
            CLAP_WINDOW_API_WIN32
        } else if cfg!(target_os = "macos") {
            CLAP_WINDOW_API_COCOA
        } else {
            CLAP_WINDOW_API_X11
        },
        io: (ins.first().copied().unwrap_or(0), outs.first().copied().unwrap_or(0)),
        _entry: entry,
    };
    hosted.refresh_table();
    let frames = max_frames as usize;
    let mk = |chans: &[u32]| -> Vec<Vec<Vec<f32>>> {
        chans
            .iter()
            .map(|&c| (0..c.max(1)).map(|_| vec![0.0f32; frames]).collect())
            .collect()
    };
    let in_bufs = mk(&ins);
    let out_bufs = mk(&outs);
    let ptrs = |b: &mut Vec<Vec<Vec<f32>>>| -> Vec<Vec<*mut f32>> {
        b.iter_mut()
            .map(|p| p.iter_mut().map(|c| c.as_mut_ptr()).collect())
            .collect()
    };
    let mut in_bufs = in_bufs;
    let mut out_bufs = out_bufs;
    let in_ptrs = ptrs(&mut in_bufs);
    let out_ptrs = ptrs(&mut out_bufs);
    hosted.audio = Some(Box::new(ClapAudio {
        plugin,
        table: hosted.table.clone(),
        from_main,
        to_main,
        control,
        started: false,
        host: &*hosted.host as *const HostData,
        midi_only,
        in_bufs,
        out_bufs,
        in_ptrs,
        out_ptrs,
        in_ports: Vec::with_capacity(ins.len()),
        out_ports: Vec::with_capacity(outs.len()),
        events: Vec::with_capacity(shm::MAX_NOTES + shm::MAX_PARAM_CHANGES + 1024),
        order: Vec::with_capacity(shm::MAX_NOTES + shm::MAX_PARAM_CHANGES + 1024),
        steady: 0,
        held: [false; 128],
    }));
    Ok(hosted)
}

impl ClapHosted {
    fn refresh_table(&mut self) {
        let mut t = Vec::new();
        unsafe {
            if !self.ext.params.is_null() {
                let p = &*self.ext.params;
                let n = p.count.map(|c| c(self.plugin)).unwrap_or(0);
                for i in 0..n {
                    let mut info: clap_param_info = std::mem::zeroed();
                    if p.get_info.map(|g| g(self.plugin, i, &mut info)).unwrap_or(false) {
                        t.push(ParamMeta {
                            id: info.id,
                            cookie: info.cookie,
                            min: info.min_value,
                            max: info.max_value,
                            flags: info.flags,
                        });
                    }
                }
            }
        }
        *self.table.lock().unwrap_or_else(|e| e.into_inner()) = t;
    }

    fn meta(&self, index: u32) -> Option<ParamMeta> {
        self.table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(index as usize)
            .copied()
    }

    fn info(&self, index: u32) -> Option<clap_param_info> {
        unsafe {
            if self.ext.params.is_null() {
                return None;
            }
            let p = &*self.ext.params;
            let mut info: clap_param_info = std::mem::zeroed();
            p.get_info
                .map(|g| g(self.plugin, index, &mut info))
                .unwrap_or(false)
                .then_some(info)
        }
    }

    /// Pauses the audio half (it stops processing) and waits for it.
    fn pause_audio(&self) {
        self.control.paused.store(false, Ordering::Release);
        self.control.pause.store(true, Ordering::Release);
        let start = Instant::now();
        while !self.control.paused.load(Ordering::Acquire) && start.elapsed() < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn resume_audio(&self) {
        self.control.pause.store(false, Ordering::Release);
    }
}

impl Hosted for ClapHosted {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn audio_io(&self) -> (u32, u32) {
        self.io
    }

    fn params(&mut self) -> Vec<ParamDesc> {
        let n = self.table.lock().unwrap_or_else(|e| e.into_inner()).len() as u32;
        (0..n)
            .filter_map(|i| {
                let info = self.info(i)?;
                let meta = self.meta(i)?;
                let value = self.param_value(i);
                let steps = if info.flags & CLAP_PARAM_IS_STEPPED != 0 {
                    (info.max_value - info.min_value).round().max(0.0) as u32
                } else {
                    0
                };
                Some(ParamDesc {
                    id: info.id,
                    name: chars(&info.name),
                    group: chars(&info.module),
                    default: meta.to_norm(info.default_value),
                    steps,
                    automatable: info.flags & CLAP_PARAM_IS_AUTOMATABLE != 0,
                    hidden: info.flags & CLAP_PARAM_IS_HIDDEN != 0,
                    read_only: info.flags & CLAP_PARAM_IS_READONLY != 0,
                    value,
                    text: self.param_text(i, value),
                })
            })
            .collect()
    }

    fn param_value(&mut self, index: u32) -> f64 {
        let Some(meta) = self.meta(index) else { return 0.0 };
        unsafe {
            let p = &*self.ext.params;
            let mut v = 0.0;
            if p.get_value.map(|g| g(self.plugin, meta.id, &mut v)).unwrap_or(false) {
                meta.to_norm(v)
            } else {
                0.0
            }
        }
    }

    fn param_text(&mut self, index: u32, value: f64) -> String {
        let Some(meta) = self.meta(index) else {
            return String::new();
        };
        let plain = meta.to_plain(value);
        unsafe {
            let p = &*self.ext.params;
            let mut buf = [0 as c_char; 128];
            if p.value_to_text
                .map(|f| f(self.plugin, meta.id, plain, buf.as_mut_ptr(), buf.len() as u32))
                .unwrap_or(false)
            {
                chars(&buf)
            } else {
                format!("{plain:.2}")
            }
        }
    }

    fn set_param(&mut self, index: u32, value: f64) {
        let _ = self.to_audio.push((index, value));
    }

    fn get_state(&mut self) -> Option<Vec<u8>> {
        if self.ext.state.is_null() {
            return None;
        }
        let mut data: Vec<u8> = Vec::new();
        let stream = clap_ostream {
            ctx: &mut data as *mut Vec<u8> as *mut c_void,
            write: Some(ostream_write),
        };
        unsafe {
            let s = &*self.ext.state;
            s.save.map(|f| f(self.plugin, &stream)).unwrap_or(false).then_some(data)
        }
    }

    fn set_state(&mut self, data: &[u8]) -> bool {
        if self.ext.state.is_null() {
            return false;
        }
        let mut cursor = ReadCursor { data, pos: 0 };
        let stream = clap_istream {
            ctx: &mut cursor as *mut ReadCursor as *mut c_void,
            read: Some(istream_read),
        };
        let ok = unsafe {
            let s = &*self.ext.state;
            s.load.map(|f| f(self.plugin, &stream)).unwrap_or(false)
        };
        if ok {
            self.refresh_table();
        }
        ok
    }

    fn latency(&mut self) -> u32 {
        unsafe {
            if self.ext.latency.is_null() {
                0
            } else {
                (*self.ext.latency).get.map(|g| g(self.plugin)).unwrap_or(0)
            }
        }
    }

    fn has_editor(&mut self) -> bool {
        unsafe {
            !self.ext.gui.is_null()
                && (*self.ext.gui)
                    .is_api_supported
                    .map(|f| f(self.plugin, self.gui_api.as_ptr(), false))
                    .unwrap_or(false)
        }
    }

    fn create_editor(&mut self, scale: f64) -> Result<EditorSize, String> {
        if !self.has_editor() {
            return Err("this plug-in has no editor that can be embedded".into());
        }
        unsafe {
            let g = &*self.ext.gui;
            if !g
                .create
                .map(|f| f(self.plugin, self.gui_api.as_ptr(), false))
                .unwrap_or(false)
            {
                return Err("the plug-in could not create its editor".into());
            }
            self.gui_created = true;
            if self.gui_api != CLAP_WINDOW_API_COCOA {
                if let Some(f) = g.set_scale {
                    f(self.plugin, scale);
                }
            }
            let (mut w, mut h) = (0u32, 0u32);
            if !g.get_size.map(|f| f(self.plugin, &mut w, &mut h)).unwrap_or(false) {
                w = 640;
                h = 400;
            }
            let resizable = g.can_resize.map(|f| f(self.plugin)).unwrap_or(false);
            Ok(EditorSize {
                width: w.max(1),
                height: h.max(1),
                resizable,
            })
        }
    }

    fn attach_editor(&mut self, parent: Parent) -> Result<(), String> {
        let window = clap_window {
            api: self.gui_api.as_ptr(),
            specific: match parent {
                Parent::Win32(h) => clap_window_handle { win32: h },
                Parent::Cocoa(v) => clap_window_handle { cocoa: v },
                Parent::X11(w) => clap_window_handle { x11: w },
            },
        };
        unsafe {
            let g = &*self.ext.gui;
            if !g.set_parent.map(|f| f(self.plugin, &window)).unwrap_or(false) {
                return Err("the plug-in could not embed its editor".into());
            }
            if let Some(show) = g.show {
                show(self.plugin);
            }
        }
        Ok(())
    }

    fn editor_resized(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        if !self.gui_created {
            return None;
        }
        unsafe {
            let g = &*self.ext.gui;
            let (mut w, mut h) = (width, height);
            if g.can_resize.map(|f| f(self.plugin)).unwrap_or(false) {
                if let Some(adjust) = g.adjust_size {
                    adjust(self.plugin, &mut w, &mut h);
                }
                if let Some(set) = g.set_size {
                    set(self.plugin, w, h);
                }
                Some((w, h))
            } else {
                let (mut cw, mut ch) = (width, height);
                g.get_size.map(|f| f(self.plugin, &mut cw, &mut ch));
                Some((cw, ch))
            }
        }
    }

    fn close_editor(&mut self) {
        if !self.gui_created {
            return;
        }
        unsafe {
            let g = &*self.ext.gui;
            if let Some(hide) = g.hide {
                hide(self.plugin);
            }
            if let Some(destroy) = g.destroy {
                destroy(self.plugin);
            }
        }
        self.gui_created = false;
    }

    fn idle(&mut self) -> Vec<Notice> {
        let mut out = Vec::new();
        // The host data is boxed and outlives this call; borrowing it through
        // a pointer leaves `self` free for the helpers below.
        let d: &HostData = unsafe { &*(&*self.host as *const HostData) };
        unsafe {
            if d.callback.swap(false, Ordering::AcqRel) {
                if let Some(f) = (*self.plugin).on_main_thread {
                    f(self.plugin);
                }
            }
            if d.restart.swap(false, Ordering::AcqRel) {
                self.pause_audio();
                if let Some(f) = (*self.plugin).deactivate {
                    f(self.plugin);
                }
                if let Some(f) = (*self.plugin).activate {
                    f(self.plugin, self.sample_rate, 1, self.max_frames);
                }
                self.resume_audio();
                out.push(Notice::Latency(self.latency()));
            }
            // Timers.
            if !self.ext.timer.is_null() {
                let now = Instant::now();
                let due: Vec<clap_id> = {
                    let mut timers = d.timers.lock().unwrap_or_else(|e| e.into_inner());
                    timers
                        .iter_mut()
                        .filter(|t| t.next <= now)
                        .map(|t| {
                            t.next = now + t.period;
                            t.id
                        })
                        .collect()
                };
                if let Some(f) = (*self.ext.timer).on_timer {
                    for id in due {
                        f(self.plugin, id);
                    }
                }
            }
            // File descriptors (X11 connections of editors on Linux).
            #[cfg(unix)]
            if !self.ext.fd.is_null() {
                let fds = d.fds.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if !fds.is_empty() {
                    let mut pfds: Vec<libc::pollfd> = fds
                        .iter()
                        .map(|&(fd, flags)| libc::pollfd {
                            fd,
                            events: (if flags & CLAP_POSIX_FD_READ != 0 {
                                libc::POLLIN
                            } else {
                                0
                            }) | (if flags & CLAP_POSIX_FD_WRITE != 0 {
                                libc::POLLOUT
                            } else {
                                0
                            }),
                            revents: 0,
                        })
                        .collect();
                    if libc::poll(pfds.as_mut_ptr(), pfds.len() as _, 0) > 0 {
                        for p in pfds {
                            let mut flags = 0;
                            if p.revents & libc::POLLIN != 0 {
                                flags |= CLAP_POSIX_FD_READ;
                            }
                            if p.revents & libc::POLLOUT != 0 {
                                flags |= CLAP_POSIX_FD_WRITE;
                            }
                            if p.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
                                flags |= CLAP_POSIX_FD_ERROR;
                            }
                            if flags != 0 {
                                if let Some(f) = (*self.ext.fd).on_fd {
                                    f(self.plugin, p.fd, flags);
                                }
                            }
                        }
                    }
                }
            }
        }
        let rescan = d.rescan.swap(0, Ordering::AcqRel);
        if rescan != 0 {
            if rescan & (CLAP_PARAM_RESCAN_INFO | CLAP_PARAM_RESCAN_ALL) != 0 {
                self.pause_audio();
                self.refresh_table();
                self.resume_audio();
                out.push(Notice::Params);
            } else {
                let n = self.table.lock().unwrap_or_else(|e| e.into_inner()).len() as u32;
                out.push(Notice::Values((0..n).collect()));
            }
        }
        let mut changed = Vec::new();
        while let Ok(i) = self.from_audio.pop() {
            if !changed.contains(&i) {
                changed.push(i);
            }
        }
        if !changed.is_empty() {
            out.push(Notice::Values(changed));
        }
        if d.dirty.swap(false, Ordering::AcqRel) {
            out.push(Notice::Dirty);
        }
        if d.latency_changed.swap(false, Ordering::AcqRel) {
            out.push(Notice::Latency(self.latency()));
        }
        if let Some((w, h)) = d.resize.lock().unwrap_or_else(|e| e.into_inner()).take() {
            out.push(Notice::Resize(w, h));
        }
        if d.gui_closed.swap(false, Ordering::AcqRel) {
            self.close_editor();
            out.push(Notice::EditorClosed);
        }
        out
    }

    fn take_audio(&mut self) -> Option<Box<dyn AudioSide>> {
        self.audio.take().map(|a| a as Box<dyn AudioSide>)
    }

    fn shutdown(&mut self) {
        self.close_editor();
        unsafe {
            if let Some(f) = (*self.plugin).deactivate {
                f(self.plugin);
            }
            if let Some(f) = (*self.plugin).destroy {
                f(self.plugin);
            }
        }
        self.plugin = std::ptr::null();
    }
}

impl Drop for ClapHosted {
    fn drop(&mut self) {
        if !self.plugin.is_null() {
            self.shutdown();
        }
    }
}

// ----- audio half --------------------------------------------------------------------

/// Storage big enough for any event we send.
#[repr(C)]
#[derive(Clone, Copy)]
union EventStore {
    header: clap_event_header,
    note: clap_event_note,
    param: clap_event_param_value,
    midi: clap_event_midi,
}

struct ClapAudio {
    plugin: *const clap_plugin,
    table: Table,
    from_main: rtrb::Consumer<(u32, f64)>,
    to_main: rtrb::Producer<u32>,
    control: Arc<Control>,
    started: bool,
    host: *const HostData,
    midi_only: bool,
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    in_ptrs: Vec<Vec<*mut f32>>,
    out_ptrs: Vec<Vec<*mut f32>>,
    in_ports: Vec<clap_audio_buffer>,
    out_ports: Vec<clap_audio_buffer>,
    events: Vec<EventStore>,
    /// Sort keys (time, insertion order) parallel to `events`.
    order: Vec<(u32, u32)>,
    steady: i64,
    held: [bool; 128],
}

unsafe impl Send for ClapAudio {}

struct OutCtx<'a> {
    table: &'a [ParamMeta],
    to_main: &'a mut rtrb::Producer<u32>,
}

unsafe extern "C" fn in_size(list: *const clap_input_events) -> u32 {
    let a = &*((*list).ctx as *const ClapAudio);
    a.events.len() as u32
}

unsafe extern "C" fn in_get(list: *const clap_input_events, index: u32) -> *const clap_event_header {
    let a = &*((*list).ctx as *const ClapAudio);
    match a.order.get(index as usize) {
        Some(&(_, i)) => &a.events[i as usize].header,
        None => std::ptr::null(),
    }
}

unsafe extern "C" fn out_try_push(list: *const clap_output_events, ev: *const clap_event_header) -> bool {
    let c = &mut *((*list).ctx as *mut OutCtx);
    if (*ev).space_id == CLAP_CORE_EVENT_SPACE_ID && (*ev).type_ == CLAP_EVENT_PARAM_VALUE {
        let pv = &*(ev as *const clap_event_param_value);
        if let Some(i) = c.table.iter().position(|m| m.id == pv.param_id) {
            let _ = c.to_main.push(i as u32);
        }
    }
    true
}

impl ClapAudio {
    fn push(&mut self, time: u32, e: EventStore) {
        if self.events.len() < self.events.capacity() {
            let n = self.events.len() as u32;
            self.events.push(e);
            self.order.push((time, n));
        }
    }

    fn note(&mut self, time: u32, on: bool, key: u8, velocity: u8) {
        if self.midi_only {
            let status = if on { 0x90 } else { 0x80 };
            self.push(
                time,
                EventStore {
                    midi: clap_event_midi {
                        header: header::<clap_event_midi>(time, CLAP_EVENT_MIDI),
                        port_index: 0,
                        data: [status, key, velocity],
                    },
                },
            );
        } else {
            self.push(
                time,
                EventStore {
                    note: clap_event_note {
                        header: header::<clap_event_note>(
                            time,
                            if on { CLAP_EVENT_NOTE_ON } else { CLAP_EVENT_NOTE_OFF },
                        ),
                        note_id: -1,
                        port_index: 0,
                        channel: 0,
                        key: key as i16,
                        velocity: velocity as f64 / 127.0,
                    },
                },
            );
        }
    }
}

fn header<T>(time: u32, type_: u16) -> clap_event_header {
    clap_event_header {
        size: std::mem::size_of::<T>() as u32,
        time,
        space_id: CLAP_CORE_EVENT_SPACE_ID,
        type_,
        flags: 0,
    }
}

impl AudioSide for ClapAudio {
    fn process(&mut self, io: &mut ProcessIo) -> bool {
        let plugin = self.plugin;
        unsafe {
            if self.control.pause.load(Ordering::Acquire) {
                if self.started {
                    if let Some(f) = (*plugin).stop_processing {
                        f(plugin);
                    }
                    self.started = false;
                }
                self.control.paused.store(true, Ordering::Release);
                return false;
            }
            if !self.started {
                let _ = (*self.host).audio_thread.set(std::thread::current().id());
                if let Some(f) = (*plugin).start_processing {
                    if !f(plugin) {
                        return false;
                    }
                }
                self.started = true;
            }
            if io.reset {
                if let Some(f) = (*plugin).reset {
                    f(plugin);
                }
            }
        }
        // The main thread only holds this lock while rescanning parameters.
        let table_lock = self.table.clone();
        let Ok(table) = table_lock.try_lock() else {
            return false;
        };
        let frames = io.frames;
        self.events.clear();
        self.order.clear();
        // Parameter changes from Tessera's UI, then automation.
        while let Ok((index, value)) = self.from_main.pop() {
            if let Some(m) = table.get(index as usize) {
                self.push(
                    0,
                    EventStore {
                        param: param_event(0, m, m.to_plain(value)),
                    },
                );
            }
        }
        for c in io.params {
            if let Some(m) = table.get(c.index as usize) {
                let t = c.frame.min(frames.saturating_sub(1) as u32);
                self.push(
                    t,
                    EventStore {
                        param: param_event(t, m, m.to_plain(c.value)),
                    },
                );
                let _ = self.to_main.push(c.index);
            }
        }
        for n in io.notes {
            let t = n.frame.min(frames.saturating_sub(1) as u32);
            match n.kind {
                shm::NOTE_ON => {
                    let k = (n.key & 127) as u8;
                    self.held[k as usize] = true;
                    self.note(t, true, k, (n.velocity & 127) as u8);
                }
                shm::NOTE_OFF => {
                    let k = (n.key & 127) as u8;
                    self.held[k as usize] = false;
                    self.note(t, false, k, 0);
                }
                _ => {
                    for k in 0..128u8 {
                        if std::mem::take(&mut self.held[k as usize]) {
                            self.note(t, false, k, 0);
                        }
                    }
                }
            }
        }
        self.order.sort_unstable();

        // Audio buffers: our stereo pair on the main ports, silence elsewhere.
        for (pi, port) in self.in_bufs.iter_mut().enumerate() {
            for (ci, ch) in port.iter_mut().enumerate() {
                if pi == 0 && ci < 2 {
                    ch[..frames].copy_from_slice(io.input[ci]);
                } else {
                    ch[..frames].fill(0.0);
                }
            }
            if pi == 0 && port.len() == 1 {
                // Mono input: mix both channels.
                for (i, s) in port[0][..frames].iter_mut().enumerate() {
                    *s = 0.5 * (io.input[0][i] + io.input[1][i]);
                }
            }
        }
        self.in_ports.clear();
        for p in self.in_ptrs.iter_mut() {
            self.in_ports.push(clap_audio_buffer {
                data32: p.as_mut_ptr(),
                data64: std::ptr::null_mut(),
                channel_count: p.len() as u32,
                latency: 0,
                constant_mask: 0,
            });
        }
        self.out_ports.clear();
        for p in self.out_ptrs.iter_mut() {
            self.out_ports.push(clap_audio_buffer {
                data32: p.as_mut_ptr(),
                data64: std::ptr::null_mut(),
                channel_count: p.len() as u32,
                latency: 0,
                constant_mask: 0,
            });
        }
        let t = &io.transport;
        let beats = |b: f64| (b * CLAP_BEATTIME_FACTOR as f64).round() as i64;
        let secs = |b: f64| (b * 60.0 / t.tempo.max(1.0) * CLAP_SECTIME_FACTOR as f64).round() as i64;
        let bpb = t.beats_per_bar.max(1) as f64;
        let bar = (t.song_beat / bpb).floor();
        let transport = clap_event_transport {
            header: header::<clap_event_transport>(0, CLAP_EVENT_TRANSPORT),
            flags: CLAP_TRANSPORT_HAS_TEMPO
                | CLAP_TRANSPORT_HAS_BEATS_TIMELINE
                | CLAP_TRANSPORT_HAS_SECONDS_TIMELINE
                | CLAP_TRANSPORT_HAS_TIME_SIGNATURE
                | if t.playing != 0 { CLAP_TRANSPORT_IS_PLAYING } else { 0 },
            song_pos_beats: beats(t.song_beat),
            song_pos_seconds: secs(t.song_beat),
            tempo: t.tempo,
            tempo_inc: 0.0,
            loop_start_beats: 0,
            loop_end_beats: 0,
            loop_start_seconds: 0,
            loop_end_seconds: 0,
            bar_start: beats(bar * bpb),
            bar_number: bar as i32,
            tsig_num: t.beats_per_bar as u16,
            tsig_denom: 4,
        };
        let in_events = clap_input_events {
            ctx: self as *mut ClapAudio as *mut c_void,
            size: Some(in_size),
            get: Some(in_get),
        };
        let mut out_ctx = OutCtx {
            table: &table,
            to_main: &mut self.to_main,
        };
        let out_events = clap_output_events {
            ctx: &mut out_ctx as *mut OutCtx as *mut c_void,
            try_push: Some(out_try_push),
        };
        let process = clap_process {
            steady_time: self.steady,
            frames_count: frames as u32,
            transport: &transport,
            audio_inputs: self.in_ports.as_ptr(),
            audio_outputs: self.out_ports.as_mut_ptr(),
            audio_inputs_count: self.in_ports.len() as u32,
            audio_outputs_count: self.out_ports.len() as u32,
            in_events: &in_events,
            out_events: &out_events,
        };
        let status = unsafe {
            (*plugin)
                .process
                .map(|f| f(plugin, &process))
                .unwrap_or(CLAP_PROCESS_ERROR)
        };
        self.steady += frames as i64;
        if status == CLAP_PROCESS_ERROR {
            return false;
        }
        match self.out_bufs.first() {
            Some(port) if !port.is_empty() => {
                let l = &port[0];
                let r = port.get(1).unwrap_or(&port[0]);
                io.output[0].copy_from_slice(&l[..frames]);
                io.output[1].copy_from_slice(&r[..frames]);
            }
            _ => {
                io.output[0].fill(0.0);
                io.output[1].fill(0.0);
            }
        }
        true
    }
}

fn param_event(time: u32, m: &ParamMeta, value: f64) -> clap_event_param_value {
    clap_event_param_value {
        header: header::<clap_event_param_value>(time, CLAP_EVENT_PARAM_VALUE),
        param_id: m.id,
        cookie: m.cookie,
        note_id: -1,
        port_index: -1,
        channel: -1,
        key: -1,
        value,
    }
}
