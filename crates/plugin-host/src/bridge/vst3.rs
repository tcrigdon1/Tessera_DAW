//! The VST3 adapter: scanning factories, loading a component and its edit
//! controller, and the host-side COM objects VST3 plug-ins expect.

// The SDK's enum constants are `i32` on Windows and `u32` elsewhere, so the
// casts below are only redundant on some platforms.
#![allow(clippy::unnecessary_cast)]

use std::cell::{Cell, UnsafeCell};
use std::collections::HashMap;
use std::ffi::{c_char, c_void, CStr, CString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tessera_engine::plugin::PluginFormat;
use vst3::Steinberg::Linux::{IEventHandlerTrait, ITimerHandlerTrait};
use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent};
use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::{kCanAutomate, kIsHidden, kIsReadOnly};
use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::{
    kBarPositionValid, kPlaying, kProjectTimeMusicValid, kTempoValid, kTimeSigValid,
};
use vst3::Steinberg::Vst::ProcessModes_::kRealtime;
use vst3::Steinberg::Vst::RestartFlags_::{
    kLatencyChanged, kParamTitlesChanged, kParamValuesChanged, kReloadComponent,
};
use vst3::Steinberg::Vst::SpeakerArr::kStereo;
use vst3::Steinberg::Vst::SymbolicSampleSizes_::kSample32;
use vst3::Steinberg::Vst::*;
use vst3::Steinberg::*;
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

use super::module::Module;
use super::{AudioSide, EditorSize, Hosted, Notice, Parent, ProcessIo};
use crate::protocol::{ParamDesc, PluginInfo};
use crate::shm;

const AUDIO_MODULE_CLASS: &str = "Audio Module Class";
const SEEK_SET: u32 = IBStream_::IStreamSeekMode_::kIBSeekSet as u32;
const SEEK_CUR: u32 = IBStream_::IStreamSeekMode_::kIBSeekCur as u32;
const SEEK_END: u32 = IBStream_::IStreamSeekMode_::kIBSeekEnd as u32;
const STATE_MAGIC: &[u8; 4] = b"TSV3";
/// Parameter queues available to the plug-in per block.
const MAX_QUEUES: usize = 512;
const MAX_POINTS: usize = 64;
const MAX_EVENTS: usize = 2048;

// ----- small helpers ---------------------------------------------------------------

fn cstr(buf: &[c_char]) -> String {
    let bytes: Vec<u8> = buf.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn str16(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n]).trim().to_string()
}

fn write16(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for c in s.encode_utf16() {
        if n + 1 >= dst.len() {
            break;
        }
        dst[n] = c;
        n += 1;
    }
    if !dst.is_empty() {
        dst[n] = 0;
    }
}

/// A class id as the platform-independent 32-digit form of the SDK's
/// `FUID::toString` (Windows lays the bytes out differently in memory).
fn tuid_to_hex(t: &TUID) -> String {
    let b: Vec<u8> = t.iter().map(|&c| c as u8).collect();
    let be = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (a, bb) = if cfg!(target_os = "windows") {
        (
            u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (b[5] as u32) << 24 | (b[4] as u32) << 16 | (b[7] as u32) << 8 | b[6] as u32,
        )
    } else {
        (be(0), be(4))
    };
    format!("{a:08X}{bb:08X}{:08X}{:08X}", be(8), be(12))
}

fn hex_to_tuid(s: &str) -> Option<TUID> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let p = |i: usize| u32::from_str_radix(&s[i * 8..i * 8 + 8], 16).ok();
    Some(vst3::uid(p(0)?, p(1)?, p(2)?, p(3)?))
}

fn same_iid(t: *const TUID, g: &vst3::com_scrape_types::Guid) -> bool {
    !t.is_null() && unsafe { (*t).iter().zip(g.iter()).all(|(&a, &b)| a as u8 == b) }
}

fn iid<I: Interface>() -> FIDString {
    I::IID.as_ptr() as FIDString
}

// ----- the factory -----------------------------------------------------------------

struct Factory {
    // Declared first so it is released before the module unloads.
    factory: ComPtr<IPluginFactory>,
    _module: Module,
}

struct ClassEntry {
    cid: TUID,
    name: String,
    vendor: String,
    version: String,
    sub_categories: String,
}

impl Factory {
    fn open(path: &Path) -> Result<Factory, String> {
        let module = Module::load(PluginFormat::Vst3, path)?;
        type GetFactory = unsafe extern "system" fn() -> *mut IPluginFactory;
        let get = unsafe { module.symbol::<GetFactory>(b"GetPluginFactory\0") }
            .ok_or_else(|| format!("{} is not a VST3 plug-in (no GetPluginFactory)", path.display()))?;
        let factory = unsafe { ComPtr::from_raw(get()) }.ok_or("the plug-in returned no factory")?;
        Ok(Factory {
            factory,
            _module: module,
        })
    }

    fn classes(&self) -> Vec<ClassEntry> {
        let mut out = Vec::new();
        unsafe {
            let mut finfo: PFactoryInfo = std::mem::zeroed();
            let factory_vendor = if self.factory.getFactoryInfo(&mut finfo) == kResultOk {
                cstr(&finfo.vendor)
            } else {
                String::new()
            };
            let f2 = self.factory.cast::<IPluginFactory2>();
            for i in 0..self.factory.countClasses() {
                if let Some(f2) = &f2 {
                    let mut info: PClassInfo2 = std::mem::zeroed();
                    if f2.getClassInfo2(i, &mut info) == kResultOk {
                        if cstr(&info.category) != AUDIO_MODULE_CLASS {
                            continue;
                        }
                        let vendor = cstr(&info.vendor);
                        out.push(ClassEntry {
                            cid: info.cid,
                            name: cstr(&info.name),
                            vendor: if vendor.is_empty() {
                                factory_vendor.clone()
                            } else {
                                vendor
                            },
                            version: cstr(&info.version),
                            sub_categories: cstr(&info.subCategories),
                        });
                        continue;
                    }
                }
                let mut info: PClassInfo = std::mem::zeroed();
                if self.factory.getClassInfo(i, &mut info) == kResultOk && cstr(&info.category) == AUDIO_MODULE_CLASS {
                    out.push(ClassEntry {
                        cid: info.cid,
                        name: cstr(&info.name),
                        vendor: factory_vendor.clone(),
                        version: String::new(),
                        sub_categories: String::new(),
                    });
                }
            }
        }
        out
    }

    fn create<I: Interface>(&self, cid: &TUID) -> Option<ComPtr<I>> {
        let mut obj: *mut c_void = std::ptr::null_mut();
        unsafe {
            if self.factory.createInstance(cid.as_ptr(), iid::<I>(), &mut obj) != kResultOk {
                return None;
            }
            ComPtr::from_raw(obj as *mut I)
        }
    }
}

fn is_instrument(sub_categories: &str) -> bool {
    sub_categories
        .split('|')
        .any(|c| c == "Instrument" || c.starts_with("Instrument"))
}

pub fn scan(path: &Path) -> Result<Vec<PluginInfo>, String> {
    let factory = Factory::open(path)?;
    Ok(factory
        .classes()
        .into_iter()
        .map(|c| PluginInfo {
            format: PluginFormat::Vst3,
            path: path.to_path_buf(),
            id: tuid_to_hex(&c.cid),
            name: c.name,
            vendor: c.vendor,
            version: c.version,
            instrument: is_instrument(&c.sub_categories),
            category: c.sub_categories,
        })
        .collect())
}

// ----- host-side COM objects -------------------------------------------------------

/// Shared by the host application and editor frame: Linux run-loop timers
/// and file descriptors the plug-in registered.
#[derive(Default)]
struct RunLoop {
    timers: Mutex<Vec<(ComPtr<Linux::ITimerHandler>, Duration, Instant)>>,
    fds: Mutex<Vec<(ComPtr<Linux::IEventHandler>, i32)>>,
}

impl RunLoop {
    fn register_timer(&self, handler: *mut Linux::ITimerHandler, ms: u64) -> tresult {
        let Some(h) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        let every = Duration::from_millis(ms.max(1));
        self.timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((h.to_com_ptr(), every, Instant::now() + every));
        kResultOk
    }

    fn unregister_timer(&self, handler: *mut Linux::ITimerHandler) -> tresult {
        self.timers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(h, _, _)| h.as_ptr() != handler);
        kResultOk
    }

    fn register_fd(&self, handler: *mut Linux::IEventHandler, fd: i32) -> tresult {
        let Some(h) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        self.fds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((h.to_com_ptr(), fd));
        kResultOk
    }

    fn unregister_fd(&self, handler: *mut Linux::IEventHandler) -> tresult {
        self.fds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(h, _)| h.as_ptr() != handler);
        kResultOk
    }

    /// Runs due timers and ready file descriptors. Handlers are called
    /// without the lock held, since they often unregister themselves.
    fn service(&self) {
        let now = Instant::now();
        let due: Vec<ComPtr<Linux::ITimerHandler>> = {
            let mut timers = self.timers.lock().unwrap_or_else(|e| e.into_inner());
            timers
                .iter_mut()
                .filter(|t| t.2 <= now)
                .map(|t| {
                    t.2 = now + t.1;
                    t.0.clone()
                })
                .collect()
        };
        for t in due {
            unsafe { t.onTimer() };
        }
        let fds: Vec<(ComPtr<Linux::IEventHandler>, i32)> = self.fds.lock().unwrap_or_else(|e| e.into_inner()).clone();
        #[cfg(unix)]
        for (h, fd) in fds {
            let mut p = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut p, 1, 0) } > 0 {
                unsafe { h.onFDIsSet(fd) };
            }
        }
        #[cfg(not(unix))]
        drop(fds);
    }
}

struct HostApp {
    run_loop: Arc<RunLoop>,
}

impl Class for HostApp {
    type Interfaces = (IHostApplication, Linux::IRunLoop);
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        if name.is_null() {
            return kInvalidArgument;
        }
        write16(&mut *name, "Tessera");
        kResultOk
    }

    unsafe fn createInstance(&self, cid: *mut TUID, _iid: *mut TUID, obj: *mut *mut c_void) -> tresult {
        if obj.is_null() {
            return kInvalidArgument;
        }
        if same_iid(cid, &IMessage::IID) {
            let msg = ComWrapper::new(Message::default());
            *obj = msg
                .to_com_ptr::<IMessage>()
                .map(|p| p.into_raw() as *mut c_void)
                .unwrap_or(std::ptr::null_mut());
            return kResultOk;
        }
        if same_iid(cid, &IAttributeList::IID) {
            let list = ComWrapper::new(AttrList::default());
            *obj = list
                .to_com_ptr::<IAttributeList>()
                .map(|p| p.into_raw() as *mut c_void)
                .unwrap_or(std::ptr::null_mut());
            return kResultOk;
        }
        *obj = std::ptr::null_mut();
        kResultFalse
    }
}

impl Linux::IRunLoopTrait for HostApp {
    unsafe fn registerEventHandler(&self, handler: *mut Linux::IEventHandler, fd: Linux::FileDescriptor) -> tresult {
        self.run_loop.register_fd(handler, fd)
    }
    unsafe fn unregisterEventHandler(&self, handler: *mut Linux::IEventHandler) -> tresult {
        self.run_loop.unregister_fd(handler)
    }
    unsafe fn registerTimer(&self, handler: *mut Linux::ITimerHandler, ms: Linux::TimerInterval) -> tresult {
        self.run_loop.register_timer(handler, ms)
    }
    unsafe fn unregisterTimer(&self, handler: *mut Linux::ITimerHandler) -> tresult {
        self.run_loop.unregister_timer(handler)
    }
}

/// Messages between a plug-in's processor and its controller.
struct Message {
    id: Mutex<Option<CString>>,
    attrs: ComPtr<IAttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        Message {
            id: Mutex::new(None),
            attrs: ComWrapper::new(AttrList::default())
                .to_com_ptr::<IAttributeList>()
                .expect("attribute list"),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        match &*self.id.lock().unwrap_or_else(|e| e.into_inner()) {
            Some(s) => s.as_ptr(),
            None => std::ptr::null(),
        }
    }
    unsafe fn setMessageID(&self, id: FIDString) {
        *self.id.lock().unwrap_or_else(|e| e.into_inner()) = (!id.is_null()).then(|| CStr::from_ptr(id).to_owned());
    }
    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Not reference-counted for the caller, as in the SDK.
        self.attrs.as_ptr()
    }
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<u16>),
    Bin(Vec<u8>),
}

#[derive(Default)]
struct AttrList {
    map: Mutex<HashMap<Vec<u8>, Attr>>,
}

impl Class for AttrList {
    type Interfaces = (IAttributeList,);
}

unsafe fn key(id: *const c_char) -> Option<Vec<u8>> {
    (!id.is_null()).then(|| CStr::from_ptr(id).to_bytes().to_vec())
}

impl AttrList {
    unsafe fn set(&self, id: *const c_char, v: Attr) -> tresult {
        let Some(k) = key(id) else { return kInvalidArgument };
        self.map.lock().unwrap_or_else(|e| e.into_inner()).insert(k, v);
        kResultOk
    }
}

impl IAttributeListTrait for AttrList {
    unsafe fn setInt(&self, id: IAttributeList_::AttrID, value: int64) -> tresult {
        self.set(id, Attr::Int(value))
    }
    unsafe fn getInt(&self, id: IAttributeList_::AttrID, value: *mut int64) -> tresult {
        let Some(k) = key(id) else { return kInvalidArgument };
        match self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            Some(Attr::Int(v)) if !value.is_null() => {
                *value = *v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setFloat(&self, id: IAttributeList_::AttrID, value: f64) -> tresult {
        self.set(id, Attr::Float(value))
    }
    unsafe fn getFloat(&self, id: IAttributeList_::AttrID, value: *mut f64) -> tresult {
        let Some(k) = key(id) else { return kInvalidArgument };
        match self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            Some(Attr::Float(v)) if !value.is_null() => {
                *value = *v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setString(&self, id: IAttributeList_::AttrID, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut n = 0;
        while *string.add(n) != 0 {
            n += 1;
        }
        self.set(id, Attr::Str(std::slice::from_raw_parts(string, n).to_vec()))
    }
    unsafe fn getString(&self, id: IAttributeList_::AttrID, string: *mut TChar, size_in_bytes: uint32) -> tresult {
        let Some(k) = key(id) else { return kInvalidArgument };
        match self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            Some(Attr::Str(s)) if !string.is_null() && size_in_bytes >= 2 => {
                let cap = size_in_bytes as usize / 2 - 1;
                let n = s.len().min(cap);
                std::ptr::copy_nonoverlapping(s.as_ptr(), string, n);
                *string.add(n) = 0;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn setBinary(&self, id: IAttributeList_::AttrID, data: *const c_void, size: uint32) -> tresult {
        let bytes = if data.is_null() {
            Vec::new()
        } else {
            std::slice::from_raw_parts(data as *const u8, size as usize).to_vec()
        };
        self.set(id, Attr::Bin(bytes))
    }
    unsafe fn getBinary(&self, id: IAttributeList_::AttrID, data: *mut *const c_void, size: *mut uint32) -> tresult {
        let Some(k) = key(id) else { return kInvalidArgument };
        match self.map.lock().unwrap_or_else(|e| e.into_inner()).get(&k) {
            // The bytes stay put until the attribute is replaced, as in the SDK.
            Some(Attr::Bin(b)) if !data.is_null() && !size.is_null() => {
                *data = b.as_ptr() as *const c_void;
                *size = b.len() as u32;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
}

/// An in-memory `IBStream` for states.
struct MemStream {
    data: Mutex<(Vec<u8>, usize)>,
}

impl MemStream {
    fn new(bytes: Vec<u8>) -> ComWrapper<MemStream> {
        ComWrapper::new(MemStream {
            data: Mutex::new((bytes, 0)),
        })
    }
}

impl Class for MemStream {
    type Interfaces = (IBStream,);
}

impl IBStreamTrait for MemStream {
    unsafe fn read(&self, buffer: *mut c_void, num_bytes: int32, num_read: *mut int32) -> tresult {
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let (data, pos) = &mut *d;
        let n = (num_bytes.max(0) as usize).min(data.len().saturating_sub(*pos));
        if n > 0 && !buffer.is_null() {
            std::ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer as *mut u8, n);
        }
        *pos += n;
        if !num_read.is_null() {
            *num_read = n as i32;
        }
        kResultOk
    }
    unsafe fn write(&self, buffer: *mut c_void, num_bytes: int32, num_written: *mut int32) -> tresult {
        if buffer.is_null() || num_bytes < 0 {
            return kInvalidArgument;
        }
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let (data, pos) = &mut *d;
        let n = num_bytes as usize;
        if data.len() < *pos + n {
            data.resize(*pos + n, 0);
        }
        std::ptr::copy_nonoverlapping(buffer as *const u8, data.as_mut_ptr().add(*pos), n);
        *pos += n;
        if !num_written.is_null() {
            *num_written = n as i32;
        }
        kResultOk
    }
    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        let mut d = self.data.lock().unwrap_or_else(|e| e.into_inner());
        let base = match mode as u32 {
            SEEK_SET => 0,
            SEEK_CUR => d.1 as i64,
            SEEK_END => d.0.len() as i64,
            _ => return kInvalidArgument,
        };
        let new = (base + pos).max(0) as usize;
        d.1 = new;
        if !result.is_null() {
            *result = new as i64;
        }
        kResultOk
    }
    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        *pos = self.data.lock().unwrap_or_else(|e| e.into_inner()).1 as i64;
        kResultOk
    }
}

fn stream_bytes(s: &ComWrapper<MemStream>) -> Vec<u8> {
    s.data.lock().unwrap_or_else(|e| e.into_inner()).0.clone()
}

/// What the edit controller reported, for the main loop to pick up.
struct Shared {
    to_audio: Mutex<rtrb::Producer<(u32, f64)>>,
    edits: Mutex<Vec<u32>>,
    restart: AtomicI32,
    dirty: AtomicBool,
    resize: Mutex<Option<(u32, u32)>>,
}

struct Handler {
    shared: Arc<Shared>,
}

impl Class for Handler {
    type Interfaces = (IComponentHandler,);
}

impl IComponentHandlerTrait for Handler {
    unsafe fn beginEdit(&self, _id: ParamID) -> tresult {
        kResultOk
    }
    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        let _ = self
            .shared
            .to_audio
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((id, value));
        let mut edits = self.shared.edits.lock().unwrap_or_else(|e| e.into_inner());
        if !edits.contains(&id) {
            edits.push(id);
        }
        kResultOk
    }
    unsafe fn endEdit(&self, _id: ParamID) -> tresult {
        self.shared.dirty.store(true, Ordering::Release);
        kResultOk
    }
    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.shared.restart.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

struct Frame {
    shared: Arc<Shared>,
    run_loop: Arc<RunLoop>,
}

impl Class for Frame {
    type Interfaces = (IPlugFrame, Linux::IRunLoop);
}

impl IPlugFrameTrait for Frame {
    unsafe fn resizeView(&self, _view: *mut IPlugView, size: *mut ViewRect) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let r = &*size;
        let w = (r.right - r.left).max(1) as u32;
        let h = (r.bottom - r.top).max(1) as u32;
        *self.shared.resize.lock().unwrap_or_else(|e| e.into_inner()) = Some((w, h));
        kResultTrue
    }
}

impl Linux::IRunLoopTrait for Frame {
    unsafe fn registerEventHandler(&self, handler: *mut Linux::IEventHandler, fd: Linux::FileDescriptor) -> tresult {
        self.run_loop.register_fd(handler, fd)
    }
    unsafe fn unregisterEventHandler(&self, handler: *mut Linux::IEventHandler) -> tresult {
        self.run_loop.unregister_fd(handler)
    }
    unsafe fn registerTimer(&self, handler: *mut Linux::ITimerHandler, ms: Linux::TimerInterval) -> tresult {
        self.run_loop.register_timer(handler, ms)
    }
    unsafe fn unregisterTimer(&self, handler: *mut Linux::ITimerHandler) -> tresult {
        self.run_loop.unregister_timer(handler)
    }
}

// ----- audio-thread COM objects (preallocated, never allocate) ---------------------

struct Queue {
    id: Cell<u32>,
    points: UnsafeCell<Vec<(i32, f64)>>,
}

// Only touched from the bridge's audio thread during `process`.
unsafe impl Send for Queue {}
unsafe impl Sync for Queue {}

impl Class for Queue {
    type Interfaces = (IParamValueQueue,);
}

impl IParamValueQueueTrait for Queue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }
    unsafe fn getPointCount(&self) -> int32 {
        (*self.points.get()).len() as i32
    }
    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        match (&*self.points.get()).get(index.max(0) as usize) {
            Some(&(o, v)) if index >= 0 && !offset.is_null() && !value.is_null() => {
                *offset = o;
                *value = v;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let points = &mut *self.points.get();
        // Same offset: replace. Otherwise keep the points in time order.
        if let Some(last) = points.last_mut() {
            if last.0 == offset {
                last.1 = value;
                if !index.is_null() {
                    *index = points.len() as i32 - 1;
                }
                return kResultOk;
            }
        }
        if points.len() == points.capacity() {
            return kResultFalse;
        }
        points.push((offset, value));
        if !index.is_null() {
            *index = points.len() as i32 - 1;
        }
        kResultOk
    }
}

struct Changes {
    queues: Vec<ComWrapper<Queue>>,
    ptrs: Vec<*mut IParamValueQueue>,
    used: Cell<usize>,
}

unsafe impl Send for Changes {}
unsafe impl Sync for Changes {}

impl Changes {
    fn new() -> ComWrapper<Changes> {
        let queues: Vec<ComWrapper<Queue>> = (0..MAX_QUEUES)
            .map(|_| {
                ComWrapper::new(Queue {
                    id: Cell::new(0),
                    points: UnsafeCell::new(Vec::with_capacity(MAX_POINTS)),
                })
            })
            .collect();
        let ptrs = queues
            .iter()
            .map(|q| q.as_com_ref::<IParamValueQueue>().expect("queue").as_ptr())
            .collect();
        ComWrapper::new(Changes {
            queues,
            ptrs,
            used: Cell::new(0),
        })
    }

    fn clear(&self) {
        self.used.set(0);
    }

    fn queue(&self, i: usize) -> &Queue {
        &self.queues[i]
    }

    fn add(&self, id: u32, offset: i32, value: f64) {
        let mut index = 0;
        let q = unsafe { self.addParameterData(&id, &mut index) };
        if !q.is_null() {
            unsafe {
                self.queues[index as usize].addPoint(offset, value, std::ptr::null_mut());
            }
        }
    }
}

impl Class for Changes {
    type Interfaces = (IParameterChanges,);
}

impl IParameterChangesTrait for Changes {
    unsafe fn getParameterCount(&self) -> int32 {
        self.used.get() as i32
    }
    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        if index >= 0 && (index as usize) < self.used.get() {
            self.ptrs[index as usize]
        } else {
            std::ptr::null_mut()
        }
    }
    unsafe fn addParameterData(&self, id: *const ParamID, index: *mut int32) -> *mut IParamValueQueue {
        if id.is_null() {
            return std::ptr::null_mut();
        }
        let used = self.used.get();
        let i = match (0..used).find(|&i| self.queues[i].id.get() == *id) {
            Some(i) => i,
            None if used < self.queues.len() => {
                let q = &self.queues[used];
                q.id.set(*id);
                (*q.points.get()).clear();
                self.used.set(used + 1);
                used
            }
            None => return std::ptr::null_mut(),
        };
        if !index.is_null() {
            *index = i as i32;
        }
        self.ptrs[i]
    }
}

struct Events {
    list: UnsafeCell<Vec<Event>>,
}

unsafe impl Send for Events {}
unsafe impl Sync for Events {}

impl Class for Events {
    type Interfaces = (IEventList,);
}

impl Events {
    fn new() -> ComWrapper<Events> {
        ComWrapper::new(Events {
            list: UnsafeCell::new(Vec::with_capacity(MAX_EVENTS)),
        })
    }
    fn clear(&self) {
        unsafe { (*self.list.get()).clear() }
    }
    fn push(&self, mut e: Event) {
        unsafe { self.addEvent(&mut e) };
    }
}

impl IEventListTrait for Events {
    unsafe fn getEventCount(&self) -> int32 {
        (*self.list.get()).len() as i32
    }
    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match (&*self.list.get()).get(index.max(0) as usize) {
            Some(ev) if index >= 0 && !e.is_null() => {
                *e = *ev;
                kResultOk
            }
            _ => kResultFalse,
        }
    }
    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        let list = &mut *self.list.get();
        if e.is_null() || list.len() == list.capacity() {
            return kResultFalse;
        }
        list.push(*e);
        kResultOk
    }
}

fn note_event(offset: i32, on: bool, key: i16, velocity: f32) -> Event {
    Event {
        busIndex: 0,
        sampleOffset: offset,
        ppqPosition: 0.0,
        flags: Event_::EventFlags_::kIsLive as u16,
        r#type: if on { kNoteOnEvent } else { kNoteOffEvent } as u16,
        __field0: if on {
            Event__type0 {
                noteOn: NoteOnEvent {
                    channel: 0,
                    pitch: key,
                    tuning: 0.0,
                    velocity,
                    length: 0,
                    noteId: -1,
                },
            }
        } else {
            Event__type0 {
                noteOff: NoteOffEvent {
                    channel: 0,
                    pitch: key,
                    velocity,
                    noteId: -1,
                    tuning: 0.0,
                },
            }
        },
    }
}

// ----- the hosted plug-in ----------------------------------------------------------

struct ParamMeta {
    id: u32,
    info: ParameterInfo,
}

type Ids = Arc<Mutex<Vec<u32>>>;

struct Control {
    pause: AtomicBool,
    paused: AtomicBool,
}

pub struct Vst3Hosted {
    name: String,
    component: ComPtr<IComponent>,
    processor: Option<ComPtr<IAudioProcessor>>,
    controller: Option<ComPtr<IEditController>>,
    /// The controller is its own object (not the component).
    separate: bool,
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    host: ComWrapper<HostApp>,
    handler: ComWrapper<Handler>,
    frame: ComWrapper<Frame>,
    run_loop: Arc<RunLoop>,
    shared: Arc<Shared>,
    params: Vec<ParamMeta>,
    ids: Ids,
    from_audio: rtrb::Consumer<(u32, f64)>,
    control: Arc<Control>,
    view: Option<ComPtr<IPlugView>>,
    has_editor: bool,
    audio: Option<Box<Vst3Audio>>,
    io: (u32, u32),
    active: bool,
    // Last, so every COM pointer above is released before the library goes.
    factory: Option<Factory>,
}

pub fn load(path: &Path, id: &str, sample_rate: f64, max_frames: u32) -> Result<Box<dyn Hosted>, String> {
    let factory = Factory::open(path)?;
    let classes = factory.classes();
    let class = classes
        .iter()
        .find(|c| tuid_to_hex(&c.cid).eq_ignore_ascii_case(id))
        .or_else(|| if id.is_empty() { classes.first() } else { None })
        .ok_or_else(|| format!("{} no longer contains this plug-in", path.display()))?;
    let cid = match hex_to_tuid(id) {
        Some(c) => c,
        None => class.cid,
    };
    let name = class.name.clone();

    let run_loop = Arc::new(RunLoop::default());
    let host = ComWrapper::new(HostApp {
        run_loop: run_loop.clone(),
    });
    let host_unknown = host.as_com_ref::<FUnknown>().expect("host").as_ptr();

    let component: ComPtr<IComponent> = factory
        .create::<IComponent>(&cid)
        .ok_or("the plug-in could not create its audio component")?;
    unsafe {
        if component.initialize(host_unknown) != kResultOk {
            return Err("the plug-in failed to initialize".into());
        }
    }
    let processor = component
        .cast::<IAudioProcessor>()
        .ok_or("the plug-in has no audio processor")?;

    // The edit controller: the component itself, or a separate class.
    let mut separate = false;
    let controller = match component.cast::<IEditController>() {
        Some(c) => Some(c),
        None => unsafe {
            let mut ccid: TUID = [0; 16];
            if component.getControllerClassId(&mut ccid) == kResultOk {
                factory.create::<IEditController>(&ccid).and_then(|c| {
                    if c.initialize(host_unknown) == kResultOk {
                        separate = true;
                        Some(c)
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        },
    };
    let connection = match (&controller, separate) {
        (Some(c), true) => match (component.cast::<IConnectionPoint>(), c.cast::<IConnectionPoint>()) {
            (Some(a), Some(b)) => unsafe {
                a.connect(b.as_ptr());
                b.connect(a.as_ptr());
                Some((a, b))
            },
            _ => None,
        },
        _ => None,
    };

    let (to_audio, from_main) = rtrb::RingBuffer::<(u32, f64)>::new(4096);
    let (to_main, from_audio) = rtrb::RingBuffer::<(u32, f64)>::new(4096);
    let shared = Arc::new(Shared {
        to_audio: Mutex::new(to_audio),
        edits: Mutex::new(Vec::new()),
        restart: AtomicI32::new(0),
        dirty: AtomicBool::new(false),
        resize: Mutex::new(None),
    });
    let handler = ComWrapper::new(Handler { shared: shared.clone() });
    let frame = ComWrapper::new(Frame {
        shared: shared.clone(),
        run_loop: run_loop.clone(),
    });

    unsafe {
        if let Some(c) = &controller {
            // Give the controller the processor's initial state.
            if separate {
                let s = MemStream::new(Vec::new());
                let sp = s.as_com_ref::<IBStream>().expect("stream").as_ptr();
                if component.getState(sp) == kResultOk {
                    s.seek(0, SEEK_SET as i32, std::ptr::null_mut());
                    c.setComponentState(sp);
                }
            }
            c.setComponentHandler(handler.as_com_ref::<IComponentHandler>().expect("handler").as_ptr());
        }
    }

    // Buses: stereo on the main audio buses, the first event input on.
    let bus_count = |media, dir| unsafe { component.getBusCount(media, dir).max(0) };
    let (n_in, n_out) = (
        bus_count(kAudio as i32, kInput as i32),
        bus_count(kAudio as i32, kOutput as i32),
    );
    unsafe {
        let mut ins: Vec<SpeakerArrangement> = (0..n_in)
            .map(|i| {
                let mut a = 0;
                processor.getBusArrangement(kInput as i32, i, &mut a);
                if i == 0 {
                    kStereo
                } else {
                    a
                }
            })
            .collect();
        let mut outs: Vec<SpeakerArrangement> = (0..n_out)
            .map(|i| {
                let mut a = 0;
                processor.getBusArrangement(kOutput as i32, i, &mut a);
                if i == 0 {
                    kStereo
                } else {
                    a
                }
            })
            .collect();
        processor.setBusArrangements(ins.as_mut_ptr(), n_in, outs.as_mut_ptr(), n_out);
        if n_in > 0 {
            component.activateBus(kAudio as i32, kInput as i32, 0, 1);
        }
        if n_out > 0 {
            component.activateBus(kAudio as i32, kOutput as i32, 0, 1);
        }
        if bus_count(kEvent as i32, kInput as i32) > 0 {
            component.activateBus(kEvent as i32, kInput as i32, 0, 1);
        }
    }
    let channels = |dir, n: i32| -> Vec<usize> {
        (0..n)
            .map(|i| unsafe {
                let mut info: BusInfo = std::mem::zeroed();
                if component.getBusInfo(kAudio as i32, dir, i, &mut info) == kResultOk {
                    info.channelCount.max(0) as usize
                } else {
                    0
                }
            })
            .collect()
    };
    let in_channels = channels(kInput as i32, n_in);
    let out_channels = channels(kOutput as i32, n_out);
    if out_channels.iter().all(|&c| c == 0) {
        return Err("the plug-in has no audio output".into());
    }

    unsafe {
        if processor.canProcessSampleSize(kSample32 as i32) != kResultOk {
            return Err("the plug-in can't process 32-bit audio".into());
        }
        let mut setup = ProcessSetup {
            processMode: kRealtime as i32,
            symbolicSampleSize: kSample32 as i32,
            maxSamplesPerBlock: max_frames as i32,
            sampleRate: sample_rate,
        };
        if processor.setupProcessing(&mut setup) != kResultOk {
            return Err("the plug-in rejected the sample rate or block size".into());
        }
        if component.setActive(1) != kResultOk {
            return Err("the plug-in could not be activated".into());
        }
    }

    let mut hosted = Vst3Hosted {
        name,
        component: component.clone(),
        processor: Some(processor.clone()),
        controller,
        separate,
        connection,
        host,
        handler,
        frame,
        run_loop,
        shared,
        params: Vec::new(),
        ids: Arc::new(Mutex::new(Vec::new())),
        from_audio,
        control: Arc::new(Control {
            pause: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        }),
        view: None,
        has_editor: false,
        audio: None,
        io: (
            in_channels.first().copied().unwrap_or(0) as u32,
            out_channels.first().copied().unwrap_or(0) as u32,
        ),
        active: true,
        factory: Some(factory),
    };
    hosted.refresh_params();
    // Most hosts probe for an editor this way; the view is created again
    // when the window opens.
    hosted.has_editor = hosted
        .controller
        .as_ref()
        .and_then(|c| unsafe { ComPtr::from_raw(c.createView(ViewType::kEditor)) })
        .is_some();

    let max = max_frames as usize;
    let bufs = |chs: &[usize]| -> Vec<Vec<Vec<f32>>> {
        chs.iter().map(|&n| (0..n).map(|_| vec![0.0; max]).collect()).collect()
    };
    let mut audio = Box::new(Vst3Audio {
        processor,
        ids: hosted.ids.clone(),
        from_main,
        to_main,
        control: hosted.control.clone(),
        started: false,
        in_bufs: bufs(&in_channels),
        out_bufs: bufs(&out_channels),
        in_ptrs: Vec::new(),
        out_ptrs: Vec::new(),
        in_buses: Vec::new(),
        out_buses: Vec::new(),
        in_changes: Changes::new(),
        out_changes: Changes::new(),
        in_events: Events::new(),
        out_events: Events::new(),
        held: [false; 128],
        steady: 0,
    });
    audio.in_ptrs = audio
        .in_bufs
        .iter_mut()
        .map(|b| b.iter_mut().map(|c| c.as_mut_ptr()).collect())
        .collect();
    audio.out_ptrs = audio
        .out_bufs
        .iter_mut()
        .map(|b| b.iter_mut().map(|c| c.as_mut_ptr()).collect())
        .collect();
    audio.in_buses = Vec::with_capacity(audio.in_ptrs.len());
    audio.out_buses = Vec::with_capacity(audio.out_ptrs.len());
    hosted.audio = Some(audio);
    Ok(Box::new(hosted))
}

impl Vst3Hosted {
    fn refresh_params(&mut self) {
        let mut params = Vec::new();
        if let Some(c) = &self.controller {
            unsafe {
                for i in 0..c.getParameterCount().max(0) {
                    let mut info: ParameterInfo = std::mem::zeroed();
                    if c.getParameterInfo(i, &mut info) == kResultOk {
                        params.push(ParamMeta { id: info.id, info });
                    }
                }
            }
        }
        *self.ids.lock().unwrap_or_else(|e| e.into_inner()) = params.iter().map(|p| p.id).collect();
        self.params = params;
    }

    fn index_of(&self, id: u32) -> Option<u32> {
        self.params.iter().position(|p| p.id == id).map(|i| i as u32)
    }

    fn pause_audio(&self) {
        self.control.paused.store(false, Ordering::Release);
        self.control.pause.store(true, Ordering::Release);
        let start = Instant::now();
        while !self.control.paused.load(Ordering::Acquire) && start.elapsed() < Duration::from_millis(500) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn resume_audio(&self) {
        self.control.pause.store(false, Ordering::Release);
    }

    /// Deactivates and reactivates the component, which plug-ins need for
    /// latency changes and reloads.
    fn restart(&mut self) {
        self.pause_audio();
        unsafe {
            self.component.setActive(0);
            self.component.setActive(1);
        }
        self.resume_audio();
    }

    fn get_component_state(&self) -> Option<Vec<u8>> {
        let s = MemStream::new(Vec::new());
        let sp = s.as_com_ref::<IBStream>()?.as_ptr();
        (unsafe { self.component.getState(sp) } == kResultOk).then(|| stream_bytes(&s))
    }
}

fn split_state(data: &[u8]) -> (Vec<u8>, Option<Vec<u8>>) {
    let take = |at: usize| -> Option<(Vec<u8>, usize)> {
        let len = u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?) as usize;
        Some((data.get(at + 4..at + 4 + len)?.to_vec(), at + 4 + len))
    };
    if data.starts_with(STATE_MAGIC) {
        if let Some((comp, next)) = take(4) {
            let ctrl = take(next).map(|(c, _)| c);
            return (comp, ctrl);
        }
    }
    (data.to_vec(), None)
}

impl Hosted for Vst3Hosted {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn audio_io(&self) -> (u32, u32) {
        self.io
    }

    fn params(&mut self) -> Vec<ParamDesc> {
        (0..self.params.len() as u32)
            .map(|i| {
                let info = &self.params[i as usize].info;
                let desc = ParamDesc {
                    id: info.id,
                    name: str16(&info.title),
                    group: String::new(),
                    default: info.defaultNormalizedValue,
                    steps: info.stepCount.max(0) as u32,
                    automatable: info.flags & kCanAutomate as i32 != 0,
                    hidden: info.flags & kIsHidden as i32 != 0,
                    read_only: info.flags & kIsReadOnly as i32 != 0,
                    value: 0.0,
                    text: String::new(),
                };
                let value = self.param_value(i);
                let text = self.param_text(i, value);
                ParamDesc { value, text, ..desc }
            })
            .collect()
    }

    fn param_value(&mut self, index: u32) -> f64 {
        match (&self.controller, self.params.get(index as usize)) {
            (Some(c), Some(p)) => unsafe { c.getParamNormalized(p.id) },
            _ => 0.0,
        }
    }

    fn param_text(&mut self, index: u32, value: f64) -> String {
        match (&self.controller, self.params.get(index as usize)) {
            (Some(c), Some(p)) => unsafe {
                let mut s: String128 = [0; 128];
                if c.getParamStringByValue(p.id, value, &mut s) == kResultOk {
                    let text = str16(&s);
                    let units = str16(&p.info.units);
                    if units.is_empty() || text.ends_with(&units) {
                        text
                    } else {
                        format!("{text} {units}")
                    }
                } else {
                    format!("{:.0}%", value * 100.0)
                }
            },
            _ => String::new(),
        }
    }

    fn set_param(&mut self, index: u32, value: f64) {
        let Some(p) = self.params.get(index as usize) else {
            return;
        };
        let value = value.clamp(0.0, 1.0);
        if let Some(c) = &self.controller {
            unsafe { c.setParamNormalized(p.id, value) };
        }
        let _ = self
            .shared
            .to_audio
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((p.id, value));
    }

    fn get_state(&mut self) -> Option<Vec<u8>> {
        let comp = self.get_component_state()?;
        let ctrl = match (&self.controller, self.separate) {
            (Some(c), true) => {
                let s = MemStream::new(Vec::new());
                let sp = s.as_com_ref::<IBStream>()?.as_ptr();
                (unsafe { c.getState(sp) } == kResultOk).then(|| stream_bytes(&s))
            }
            _ => None,
        }
        .unwrap_or_default();
        let mut out = Vec::with_capacity(12 + comp.len() + ctrl.len());
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&comp);
        out.extend_from_slice(&(ctrl.len() as u32).to_le_bytes());
        out.extend_from_slice(&ctrl);
        Some(out)
    }

    fn set_state(&mut self, data: &[u8]) -> bool {
        let (comp, ctrl) = split_state(data);
        unsafe {
            let s = MemStream::new(comp);
            let Some(sp) = s.as_com_ref::<IBStream>().map(|r| r.as_ptr()) else {
                return false;
            };
            if self.component.setState(sp) != kResultOk {
                return false;
            }
            if let Some(c) = &self.controller {
                if self.separate {
                    s.seek(0, SEEK_SET as i32, std::ptr::null_mut());
                    c.setComponentState(sp);
                }
                if let Some(ctrl) = ctrl.filter(|c| !c.is_empty()) {
                    let cs = MemStream::new(ctrl);
                    if let Some(csp) = cs.as_com_ref::<IBStream>() {
                        c.setState(csp.as_ptr());
                    }
                }
            }
        }
        true
    }

    fn latency(&mut self) -> u32 {
        self.processor
            .as_ref()
            .map(|p| unsafe { p.getLatencySamples() })
            .unwrap_or(0)
    }

    fn has_editor(&mut self) -> bool {
        self.has_editor
    }

    fn create_editor(&mut self, scale: f64) -> Result<EditorSize, String> {
        let c = self.controller.as_ref().ok_or("this plug-in has no editor")?;
        let view = unsafe { ComPtr::from_raw(c.createView(ViewType::kEditor)) }.ok_or("this plug-in has no editor")?;
        unsafe {
            if view.isPlatformTypeSupported(platform_type()) != kResultTrue {
                return Err("the plug-in's editor doesn't support this window system".into());
            }
            view.setFrame(self.frame.as_com_ref::<IPlugFrame>().expect("frame").as_ptr());
            if !cfg!(target_os = "macos") {
                if let Some(s) = view.cast::<IPlugViewContentScaleSupport>() {
                    s.setContentScaleFactor(scale as f32);
                }
            }
            let mut r = ViewRect {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            let (w, h) = if view.getSize(&mut r) == kResultOk {
                ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
            } else {
                (640, 400)
            };
            let resizable = view.canResize() == kResultTrue;
            self.view = Some(view);
            Ok(EditorSize {
                width: w,
                height: h,
                resizable,
            })
        }
    }

    fn attach_editor(&mut self, parent: Parent) -> Result<(), String> {
        let view = self.view.as_ref().ok_or("the editor was not created")?;
        let ptr = match parent {
            Parent::Win32(h) => h,
            Parent::Cocoa(v) => v,
            Parent::X11(w) => w as usize as *mut c_void,
        };
        if unsafe { view.attached(ptr, platform_type()) } != kResultOk {
            return Err("the plug-in could not embed its editor".into());
        }
        Ok(())
    }

    fn editor_resized(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let view = self.view.as_ref()?;
        unsafe {
            let mut r = ViewRect {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            if view.canResize() == kResultTrue {
                view.checkSizeConstraint(&mut r);
                view.onSize(&mut r);
            } else {
                view.getSize(&mut r);
            }
            Some(((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32))
        }
    }

    fn close_editor(&mut self) {
        if let Some(view) = self.view.take() {
            unsafe {
                view.removed();
                view.setFrame(std::ptr::null_mut());
            }
        }
    }

    fn idle(&mut self) -> Vec<Notice> {
        let mut out = Vec::new();
        self.run_loop.service();

        // Values the processor reported or automation moved: keep the
        // controller (and its editor) in step.
        let mut changed: Vec<u32> = Vec::new();
        while let Ok((id, value)) = self.from_audio.pop() {
            if let Some(c) = &self.controller {
                unsafe { c.setParamNormalized(id, value) };
            }
            if let Some(i) = self.index_of(id) {
                if !changed.contains(&i) {
                    changed.push(i);
                }
            }
        }
        let edits = std::mem::take(&mut *self.shared.edits.lock().unwrap_or_else(|e| e.into_inner()));
        for id in edits {
            if let Some(i) = self.index_of(id) {
                if !changed.contains(&i) {
                    changed.push(i);
                }
            }
        }

        let flags = self.shared.restart.swap(0, Ordering::AcqRel);
        if flags & (kReloadComponent as i32 | kLatencyChanged as i32) != 0 {
            self.restart();
            out.push(Notice::Latency(self.latency()));
        }
        if flags & (kParamTitlesChanged as i32 | kReloadComponent as i32) != 0 {
            self.refresh_params();
            out.push(Notice::Params);
            changed.clear();
        } else if flags & kParamValuesChanged as i32 != 0 {
            changed = (0..self.params.len() as u32).collect();
        }
        if !changed.is_empty() {
            out.push(Notice::Values(changed));
        }
        if self.shared.dirty.swap(false, Ordering::AcqRel) {
            out.push(Notice::Dirty);
        }
        if let Some((w, h)) = self.shared.resize.lock().unwrap_or_else(|e| e.into_inner()).take() {
            out.push(Notice::Resize(w, h));
        }
        out
    }

    fn take_audio(&mut self) -> Option<Box<dyn AudioSide>> {
        self.audio.take().map(|a| a as Box<dyn AudioSide>)
    }

    fn shutdown(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        self.close_editor();
        // Plug-ins check that nothing else holds the processor when the
        // component terminates.
        self.audio = None;
        self.processor = None;
        unsafe {
            self.component.setActive(0);
            if let Some((a, b)) = self.connection.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = &self.controller {
                c.setComponentHandler(std::ptr::null_mut());
                if self.separate {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
        self.controller = None;
        self.audio = None;
        let _ = (&self.host, &self.handler);
    }
}

impl Drop for Vst3Hosted {
    fn drop(&mut self) {
        self.shutdown();
        self.run_loop.timers.lock().unwrap_or_else(|e| e.into_inner()).clear();
        self.run_loop.fds.lock().unwrap_or_else(|e| e.into_inner()).clear();
        // The remaining COM pointers drop with the struct, before `factory`
        // (declared last) unloads the library.
        let _ = self.factory.as_ref();
    }
}

fn platform_type() -> FIDString {
    if cfg!(target_os = "windows") {
        kPlatformTypeHWND
    } else if cfg!(target_os = "macos") {
        kPlatformTypeNSView
    } else {
        kPlatformTypeX11EmbedWindowID
    }
}

// ----- audio half ------------------------------------------------------------------

struct Vst3Audio {
    processor: ComPtr<IAudioProcessor>,
    ids: Ids,
    from_main: rtrb::Consumer<(u32, f64)>,
    to_main: rtrb::Producer<(u32, f64)>,
    control: Arc<Control>,
    started: bool,
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    in_ptrs: Vec<Vec<*mut f32>>,
    out_ptrs: Vec<Vec<*mut f32>>,
    in_buses: Vec<AudioBusBuffers>,
    out_buses: Vec<AudioBusBuffers>,
    in_changes: ComWrapper<Changes>,
    out_changes: ComWrapper<Changes>,
    in_events: ComWrapper<Events>,
    out_events: ComWrapper<Events>,
    held: [bool; 128],
    steady: i64,
}

// The audio half moves to the bridge's audio thread once and stays there.
unsafe impl Send for Vst3Audio {}

impl Vst3Audio {
    fn set_processing(&mut self, on: bool) {
        if self.started != on {
            unsafe { self.processor.setProcessing(on as u8) };
            self.started = on;
        }
    }
}

impl Drop for Vst3Audio {
    fn drop(&mut self) {
        self.set_processing(false);
    }
}

impl AudioSide for Vst3Audio {
    fn process(&mut self, io: &mut ProcessIo) -> bool {
        if self.control.pause.load(Ordering::Acquire) {
            self.set_processing(false);
            self.control.paused.store(true, Ordering::Release);
            return false;
        }
        if io.reset && self.started {
            // VST3 has no reset; restarting processing flushes tails.
            self.set_processing(false);
        }
        self.set_processing(true);
        let frames = io.frames;
        let ids_lock = self.ids.clone();
        let Ok(ids) = ids_lock.try_lock() else {
            return false;
        };

        // Parameters: edits from either UI, then automation.
        self.in_changes.clear();
        self.out_changes.clear();
        while let Ok((id, value)) = self.from_main.pop() {
            self.in_changes.add(id, 0, value);
        }
        for c in io.params {
            if let Some(&id) = ids.get(c.index as usize) {
                let t = c.frame.min(frames.saturating_sub(1) as u32) as i32;
                self.in_changes.add(id, t, c.value.clamp(0.0, 1.0));
                let _ = self.to_main.push((id, c.value.clamp(0.0, 1.0)));
            }
        }

        self.in_events.clear();
        self.out_events.clear();
        for n in io.notes {
            let t = n.frame.min(frames.saturating_sub(1) as u32) as i32;
            match n.kind {
                shm::NOTE_ON => {
                    let k = (n.key & 127) as usize;
                    self.held[k] = true;
                    self.in_events
                        .push(note_event(t, true, k as i16, (n.velocity & 127) as f32 / 127.0));
                }
                shm::NOTE_OFF => {
                    let k = (n.key & 127) as usize;
                    self.held[k] = false;
                    self.in_events.push(note_event(t, false, k as i16, 0.0));
                }
                _ => {
                    for k in 0..128 {
                        if std::mem::take(&mut self.held[k]) {
                            self.in_events.push(note_event(t, false, k as i16, 0.0));
                        }
                    }
                }
            }
        }

        for (bi, bus) in self.in_bufs.iter_mut().enumerate() {
            let mono = bus.len() == 1;
            for (ci, ch) in bus.iter_mut().enumerate() {
                if bi == 0 && mono {
                    for (i, s) in ch[..frames].iter_mut().enumerate() {
                        *s = 0.5 * (io.input[0][i] + io.input[1][i]);
                    }
                } else if bi == 0 && ci < 2 {
                    ch[..frames].copy_from_slice(io.input[ci]);
                } else {
                    ch[..frames].fill(0.0);
                }
            }
        }
        self.in_buses.clear();
        for p in self.in_ptrs.iter_mut() {
            self.in_buses.push(AudioBusBuffers {
                numChannels: p.len() as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: p.as_mut_ptr(),
                },
            });
        }
        self.out_buses.clear();
        for p in self.out_ptrs.iter_mut() {
            self.out_buses.push(AudioBusBuffers {
                numChannels: p.len() as i32,
                silenceFlags: 0,
                __field0: AudioBusBuffers__type0 {
                    channelBuffers32: p.as_mut_ptr(),
                },
            });
        }

        let t = &io.transport;
        let bpb = t.beats_per_bar.max(1) as f64;
        let mut context: ProcessContext = unsafe { std::mem::zeroed() };
        context.state = kTempoValid
            | kProjectTimeMusicValid
            | kBarPositionValid
            | kTimeSigValid
            | if t.playing != 0 { kPlaying } else { 0 };
        context.sampleRate = 0.0;
        context.projectTimeSamples = t.song_samples;
        context.continousTimeSamples = self.steady;
        context.projectTimeMusic = t.song_beat;
        context.barPositionMusic = (t.song_beat / bpb).floor() * bpb;
        context.tempo = t.tempo;
        context.timeSigNumerator = t.beats_per_bar as i32;
        context.timeSigDenominator = 4;

        let mut data = ProcessData {
            processMode: kRealtime as i32,
            symbolicSampleSize: kSample32 as i32,
            numSamples: frames as i32,
            numInputs: self.in_buses.len() as i32,
            numOutputs: self.out_buses.len() as i32,
            inputs: if self.in_buses.is_empty() {
                std::ptr::null_mut()
            } else {
                self.in_buses.as_mut_ptr()
            },
            outputs: self.out_buses.as_mut_ptr(),
            inputParameterChanges: self
                .in_changes
                .as_com_ref::<IParameterChanges>()
                .map(|r| r.as_ptr())
                .unwrap_or(std::ptr::null_mut()),
            outputParameterChanges: self
                .out_changes
                .as_com_ref::<IParameterChanges>()
                .map(|r| r.as_ptr())
                .unwrap_or(std::ptr::null_mut()),
            inputEvents: self
                .in_events
                .as_com_ref::<IEventList>()
                .map(|r| r.as_ptr())
                .unwrap_or(std::ptr::null_mut()),
            outputEvents: self
                .out_events
                .as_com_ref::<IEventList>()
                .map(|r| r.as_ptr())
                .unwrap_or(std::ptr::null_mut()),
            processContext: &mut context,
        };
        let result = unsafe { self.processor.process(&mut data) };
        self.steady += frames as i64;
        if result != kResultOk {
            return false;
        }

        // Values the plug-in changed itself (meters, linked controls).
        for i in 0..self.out_changes.used.get() {
            let q = self.out_changes.queue(i);
            if let Some(&(_, v)) = unsafe { (*q.points.get()).last() } {
                let _ = self.to_main.push((q.id.get(), v));
            }
        }

        match self.out_bufs.first() {
            Some(bus) if !bus.is_empty() => {
                let l = &bus[0];
                let r = bus.get(1).unwrap_or(&bus[0]);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_ids_round_trip() {
        let t = vst3::uid(0x12345678, 0x9ABCDEF0, 0x0F1E2D3C, 0x4B5A6978);
        let hex = tuid_to_hex(&t);
        assert_eq!(hex, "123456789ABCDEF00F1E2D3C4B5A6978");
        assert_eq!(hex_to_tuid(&hex), Some(t));
    }

    #[test]
    fn state_container() {
        let (c, k) = split_state(b"raw");
        assert_eq!(c, b"raw");
        assert!(k.is_none());
        let mut s = STATE_MAGIC.to_vec();
        s.extend_from_slice(&2u32.to_le_bytes());
        s.extend_from_slice(b"ab");
        s.extend_from_slice(&1u32.to_le_bytes());
        s.extend_from_slice(b"c");
        let (c, k) = split_state(&s);
        assert_eq!(c, b"ab");
        assert_eq!(k.as_deref(), Some(&b"c"[..]));
    }

    #[test]
    fn param_queues_reuse_ids() {
        let ch = Changes::new();
        ch.add(7, 0, 0.5);
        ch.add(7, 10, 0.6);
        ch.add(9, 0, 1.0);
        unsafe {
            assert_eq!(ch.getParameterCount(), 2);
            assert_eq!(ch.queue(0).getPointCount(), 2);
        }
        ch.clear();
        unsafe { assert_eq!(ch.getParameterCount(), 0) };
    }
}
