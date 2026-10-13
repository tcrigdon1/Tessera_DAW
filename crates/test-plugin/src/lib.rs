//! A tiny plug-in for testing Tessera's host, built as both CLAP and VST3
//! from one library: "Tessera Test Gain" (an effect) and "Tessera Test
//! Sine" (an instrument).
//!
//! Parameters (normalized 0..1):
//! - 0 Gain: output gain is `2 × value` (default 0.5, unity).
//! - 1 Crash: above 0.5 the plug-in aborts inside `process`.
//! - 2 Stall: above 0.5 every block sleeps 200 ms.
//!
//! State is the gain value as 8 little-endian bytes.

#![allow(clippy::missing_safety_doc)]
// VST3 enum constants are `i32` on Windows and `u32` elsewhere.
#![allow(clippy::unnecessary_cast)]

use std::f64::consts::TAU;
use std::sync::atomic::{AtomicU64, Ordering};

const PARAMS: [(&str, f64); 3] = [("Gain", 0.5), ("Crash", 0.0), ("Stall", 0.0)];

/// Shared parameter storage, readable from any thread.
struct Values([AtomicU64; 3]);

impl Values {
    fn new() -> Values {
        Values(PARAMS.map(|(_, d)| AtomicU64::new(f64::to_bits(d))))
    }
    fn get(&self, i: usize) -> f64 {
        self.0
            .get(i)
            .map(|v| f64::from_bits(v.load(Ordering::Relaxed)))
            .unwrap_or(0.0)
    }
    fn set(&self, i: usize, v: f64) {
        if let Some(a) = self.0.get(i) {
            a.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }
    fn state(&self) -> [u8; 8] {
        self.get(0).to_le_bytes()
    }
    fn load(&self, b: &[u8]) -> bool {
        match b.get(..8).and_then(|b| b.try_into().ok()) {
            Some(b) => {
                self.set(0, f64::from_le_bytes(b));
                true
            }
            None => false,
        }
    }
    fn text(i: usize, v: f64) -> String {
        match i {
            0 => format!("{:.2}x", v * 2.0),
            _ if v > 0.5 => "On".into(),
            _ => "Off".into(),
        }
    }
}

/// The DSP shared by both formats.
struct Dsp {
    instrument: bool,
    sample_rate: f64,
    /// One voice per key: phase, or negative when off.
    phases: [f64; 128],
}

impl Dsp {
    fn new(instrument: bool) -> Dsp {
        Dsp {
            instrument,
            sample_rate: 48_000.0,
            phases: [-1.0; 128],
        }
    }

    fn note(&mut self, key: usize, on: bool) {
        if let Some(p) = self.phases.get_mut(key) {
            *p = if on { 0.0 } else { -1.0 };
        }
    }

    fn checks(values: &Values) {
        if values.get(1) > 0.5 {
            std::process::abort();
        }
        if values.get(2) > 0.5 {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }

    /// Renders the block in place.
    fn run(&mut self, values: &Values, l: &mut [f32], r: &mut [f32]) {
        let gain = (values.get(0) * 2.0) as f32;
        if self.instrument {
            for (a, b) in l.iter_mut().zip(r.iter_mut()) {
                let mut s = 0.0;
                for (k, p) in self.phases.iter_mut().enumerate() {
                    if *p >= 0.0 {
                        let f = 440.0 * 2f64.powf((k as f64 - 69.0) / 12.0);
                        s += (*p * TAU).sin() * 0.2;
                        *p = (*p + f / self.sample_rate).fract();
                    }
                }
                *a = s as f32 * gain;
                *b = s as f32 * gain;
            }
        } else {
            for s in l.iter_mut().chain(r.iter_mut()) {
                *s *= gain;
            }
        }
    }
}

// ===== CLAP ========================================================================

mod clap {
    use super::*;
    use clap_sys::events::*;
    use clap_sys::ext::audio_ports::*;
    use clap_sys::ext::note_ports::*;
    use clap_sys::ext::params::*;
    use clap_sys::ext::state::*;
    use clap_sys::factory::plugin_factory::*;
    use clap_sys::host::clap_host;
    use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
    use clap_sys::process::*;
    use clap_sys::stream::{clap_istream, clap_ostream};
    use clap_sys::version::CLAP_VERSION;
    use std::ffi::{c_char, c_void, CStr};
    use std::ptr::null;

    struct Features([*const c_char; 2]);
    unsafe impl Sync for Features {}
    static GAIN_FEATURES: Features = Features([c"audio-effect".as_ptr(), null()]);
    static SINE_FEATURES: Features = Features([c"instrument".as_ptr(), null()]);

    const fn desc(id: &'static CStr, name: &'static CStr, features: &'static Features) -> clap_plugin_descriptor {
        clap_plugin_descriptor {
            clap_version: CLAP_VERSION,
            id: id.as_ptr(),
            name: name.as_ptr(),
            vendor: c"Tessera".as_ptr(),
            url: c"".as_ptr(),
            manual_url: c"".as_ptr(),
            support_url: c"".as_ptr(),
            version: c"1.0.0".as_ptr(),
            description: c"Test plug-in".as_ptr(),
            features: features.0.as_ptr(),
        }
    }

    static DESCS: [clap_plugin_descriptor; 2] = [
        desc(c"dev.tessera.test.gain", c"Tessera Test Gain", &GAIN_FEATURES),
        desc(c"dev.tessera.test.sine", c"Tessera Test Sine", &SINE_FEATURES),
    ];

    #[repr(C)]
    struct Plugin {
        plugin: clap_plugin,
        values: Values,
        dsp: Dsp,
    }

    unsafe fn this<'a>(p: *const clap_plugin) -> &'a mut Plugin {
        &mut *((*p).plugin_data as *mut Plugin)
    }

    unsafe extern "C" fn init(_: *const clap_plugin) -> bool {
        true
    }
    unsafe extern "C" fn destroy(p: *const clap_plugin) {
        drop(Box::from_raw((*p).plugin_data as *mut Plugin));
    }
    unsafe extern "C" fn activate(p: *const clap_plugin, sr: f64, _: u32, _: u32) -> bool {
        this(p).dsp.sample_rate = sr;
        true
    }
    unsafe extern "C" fn deactivate(_: *const clap_plugin) {}
    unsafe extern "C" fn start(_: *const clap_plugin) -> bool {
        true
    }
    unsafe extern "C" fn stop(_: *const clap_plugin) {}
    unsafe extern "C" fn reset(p: *const clap_plugin) {
        this(p).dsp.phases = [-1.0; 128];
    }

    unsafe extern "C" fn process(p: *const clap_plugin, proc_: *const clap_process) -> clap_process_status {
        let me = this(p);
        let pr = &*proc_;
        let ins = &*pr.in_events;
        let n = ins.size.map(|f| f(ins)).unwrap_or(0);
        for i in 0..n {
            let h = ins.get.map(|f| f(ins, i)).unwrap_or(null());
            if h.is_null() || (*h).space_id != CLAP_CORE_EVENT_SPACE_ID {
                continue;
            }
            match (*h).type_ {
                CLAP_EVENT_PARAM_VALUE => {
                    let e = &*(h as *const clap_event_param_value);
                    me.values.set(e.param_id as usize, e.value);
                }
                CLAP_EVENT_NOTE_ON | CLAP_EVENT_NOTE_OFF => {
                    let e = &*(h as *const clap_event_note);
                    me.dsp.note(e.key as usize, (*h).type_ == CLAP_EVENT_NOTE_ON);
                }
                CLAP_EVENT_MIDI => {
                    let e = &*(h as *const clap_event_midi);
                    let on = e.data[0] & 0xF0 == 0x90 && e.data[2] > 0;
                    if e.data[0] & 0xE0 == 0x80 {
                        me.dsp.note(e.data[1] as usize, on);
                    }
                }
                _ => {}
            }
        }
        Dsp::checks(&me.values);
        let frames = pr.frames_count as usize;
        if pr.audio_outputs_count == 0 {
            return CLAP_PROCESS_ERROR;
        }
        let out = &*pr.audio_outputs;
        let l = std::slice::from_raw_parts_mut(*out.data32, frames);
        let r = std::slice::from_raw_parts_mut(*out.data32.add(1), frames);
        if !me.dsp.instrument && pr.audio_inputs_count > 0 {
            let inp = &*pr.audio_inputs;
            l.copy_from_slice(std::slice::from_raw_parts(*inp.data32, frames));
            r.copy_from_slice(std::slice::from_raw_parts(*inp.data32.add(1), frames));
        }
        me.dsp.run(&me.values, l, r);
        CLAP_PROCESS_CONTINUE
    }

    // --- params
    unsafe extern "C" fn p_count(_: *const clap_plugin) -> u32 {
        PARAMS.len() as u32
    }
    unsafe extern "C" fn p_info(_: *const clap_plugin, i: u32, info: *mut clap_param_info) -> bool {
        let Some(&(name, default)) = PARAMS.get(i as usize) else {
            return false;
        };
        let info = &mut *info;
        info.id = i;
        info.flags = CLAP_PARAM_IS_AUTOMATABLE | if i > 0 { CLAP_PARAM_IS_STEPPED } else { 0 };
        info.cookie = std::ptr::null_mut();
        info.name = [0; 256];
        for (d, s) in info.name.iter_mut().zip(name.bytes()) {
            *d = s as c_char;
        }
        info.module = [0; 1024];
        info.min_value = 0.0;
        info.max_value = 1.0;
        info.default_value = default;
        true
    }
    unsafe extern "C" fn p_value(p: *const clap_plugin, id: u32, v: *mut f64) -> bool {
        *v = this(p).values.get(id as usize);
        (id as usize) < PARAMS.len()
    }
    unsafe extern "C" fn p_to_text(_: *const clap_plugin, id: u32, v: f64, buf: *mut c_char, cap: u32) -> bool {
        let t = Values::text(id as usize, v);
        let n = t.len().min(cap.saturating_sub(1) as usize);
        std::ptr::copy_nonoverlapping(t.as_ptr() as *const c_char, buf, n);
        *buf.add(n) = 0;
        true
    }
    unsafe extern "C" fn p_from_text(_: *const clap_plugin, _: u32, _: *const c_char, _: *mut f64) -> bool {
        false
    }
    unsafe extern "C" fn p_flush(p: *const clap_plugin, ins: *const clap_input_events, _: *const clap_output_events) {
        let ins = &*ins;
        for i in 0..ins.size.map(|f| f(ins)).unwrap_or(0) {
            let h = ins.get.map(|f| f(ins, i)).unwrap_or(null());
            if !h.is_null() && (*h).type_ == CLAP_EVENT_PARAM_VALUE {
                let e = &*(h as *const clap_event_param_value);
                this(p).values.set(e.param_id as usize, e.value);
            }
        }
    }
    static PARAMS_EXT: clap_plugin_params = clap_plugin_params {
        count: Some(p_count),
        get_info: Some(p_info),
        get_value: Some(p_value),
        value_to_text: Some(p_to_text),
        text_to_value: Some(p_from_text),
        flush: Some(p_flush),
    };

    // --- state
    unsafe extern "C" fn s_save(p: *const clap_plugin, s: *const clap_ostream) -> bool {
        let b = this(p).values.state();
        let s = &*s;
        s.write.map(|w| w(s, b.as_ptr() as *const c_void, 8)).unwrap_or(-1) == 8
    }
    unsafe extern "C" fn s_load(p: *const clap_plugin, s: *const clap_istream) -> bool {
        let mut b = [0u8; 8];
        let s = &*s;
        let n = s.read.map(|r| r(s, b.as_mut_ptr() as *mut c_void, 8)).unwrap_or(-1);
        n == 8 && this(p).values.load(&b)
    }
    static STATE_EXT: clap_plugin_state = clap_plugin_state {
        save: Some(s_save),
        load: Some(s_load),
    };

    // --- ports
    unsafe extern "C" fn a_count(p: *const clap_plugin, is_input: bool) -> u32 {
        if is_input && this(p).dsp.instrument {
            0
        } else {
            1
        }
    }
    unsafe extern "C" fn a_get(p: *const clap_plugin, i: u32, is_input: bool, info: *mut clap_audio_port_info) -> bool {
        if i != 0 || a_count(p, is_input) == 0 {
            return false;
        }
        let info = &mut *info;
        info.id = 0;
        info.name = [0; 256];
        info.flags = CLAP_AUDIO_PORT_IS_MAIN;
        info.channel_count = 2;
        info.port_type = CLAP_PORT_STEREO.as_ptr();
        info.in_place_pair = clap_sys::id::CLAP_INVALID_ID;
        true
    }
    static AUDIO_EXT: clap_plugin_audio_ports = clap_plugin_audio_ports {
        count: Some(a_count),
        get: Some(a_get),
    };
    unsafe extern "C" fn n_count(p: *const clap_plugin, is_input: bool) -> u32 {
        (is_input && this(p).dsp.instrument) as u32
    }
    unsafe extern "C" fn n_get(_: *const clap_plugin, i: u32, is_input: bool, info: *mut clap_note_port_info) -> bool {
        if i != 0 || !is_input {
            return false;
        }
        let info = &mut *info;
        info.id = 0;
        info.supported_dialects = CLAP_NOTE_DIALECT_CLAP | CLAP_NOTE_DIALECT_MIDI;
        info.preferred_dialect = CLAP_NOTE_DIALECT_CLAP;
        info.name = [0; 256];
        true
    }
    static NOTES_EXT: clap_plugin_note_ports = clap_plugin_note_ports {
        count: Some(n_count),
        get: Some(n_get),
    };

    unsafe extern "C" fn get_extension(_: *const clap_plugin, id: *const c_char) -> *const c_void {
        let id = CStr::from_ptr(id);
        if id == CLAP_EXT_PARAMS {
            &PARAMS_EXT as *const _ as *const c_void
        } else if id == CLAP_EXT_STATE {
            &STATE_EXT as *const _ as *const c_void
        } else if id == CLAP_EXT_AUDIO_PORTS {
            &AUDIO_EXT as *const _ as *const c_void
        } else if id == CLAP_EXT_NOTE_PORTS {
            &NOTES_EXT as *const _ as *const c_void
        } else {
            null()
        }
    }
    unsafe extern "C" fn on_main_thread(_: *const clap_plugin) {}

    // --- factory
    unsafe extern "C" fn f_count(_: *const clap_plugin_factory) -> u32 {
        DESCS.len() as u32
    }
    unsafe extern "C" fn f_desc(_: *const clap_plugin_factory, i: u32) -> *const clap_plugin_descriptor {
        DESCS.get(i as usize).map(|d| d as *const _).unwrap_or(null())
    }
    unsafe extern "C" fn f_create(
        _: *const clap_plugin_factory,
        _host: *const clap_host,
        id: *const c_char,
    ) -> *const clap_plugin {
        let id = CStr::from_ptr(id);
        let Some(index) = DESCS.iter().position(|d| CStr::from_ptr(d.id) == id) else {
            return null();
        };
        let me = Box::into_raw(Box::new(Plugin {
            plugin: clap_plugin {
                desc: &DESCS[index],
                plugin_data: std::ptr::null_mut(),
                init: Some(init),
                destroy: Some(destroy),
                activate: Some(activate),
                deactivate: Some(deactivate),
                start_processing: Some(start),
                stop_processing: Some(stop),
                reset: Some(reset),
                process: Some(process),
                get_extension: Some(get_extension),
                on_main_thread: Some(on_main_thread),
            },
            values: Values::new(),
            dsp: Dsp::new(index == 1),
        }));
        (*me).plugin.plugin_data = me as *mut c_void;
        &(*me).plugin
    }
    static FACTORY: clap_plugin_factory = clap_plugin_factory {
        get_plugin_count: Some(f_count),
        get_plugin_descriptor: Some(f_desc),
        create_plugin: Some(f_create),
    };

    unsafe extern "C" fn e_init(_: *const c_char) -> bool {
        true
    }
    unsafe extern "C" fn e_deinit() {}
    unsafe extern "C" fn e_factory(id: *const c_char) -> *const c_void {
        if CStr::from_ptr(id) == CLAP_PLUGIN_FACTORY_ID {
            &FACTORY as *const _ as *const c_void
        } else {
            null()
        }
    }

    #[allow(non_upper_case_globals)]
    #[no_mangle]
    pub static clap_entry: clap_sys::entry::clap_plugin_entry = clap_sys::entry::clap_plugin_entry {
        clap_version: CLAP_VERSION,
        init: Some(e_init),
        deinit: Some(e_deinit),
        get_factory: Some(e_factory),
    };
}

// ===== VST3 ========================================================================

#[allow(non_snake_case)]
mod vst3_impl {
    use super::*;
    use std::cell::UnsafeCell;
    use std::ffi::c_void;
    use vst3::Steinberg::Vst::*;
    use vst3::Steinberg::*;
    use vst3::{Class, ComRef, ComWrapper};

    const GAIN_CID: TUID = vst3::uid(0x7E55E2A0, 0x0B1E4A6D, 0x9C1E7E57, 0x6A1A0001);
    const SINE_CID: TUID = vst3::uid(0x7E55E2A0, 0x0B1E4A6D, 0x9C1E7E57, 0x6A1A0002);

    fn copy8(dst: &mut [c_char], s: &str) {
        for (d, b) in dst.iter_mut().zip(s.bytes().chain(std::iter::once(0))) {
            *d = b as c_char;
        }
    }
    use std::ffi::c_char;

    fn copy16(dst: &mut [u16], s: &str) {
        for (d, c) in dst.iter_mut().zip(s.encode_utf16().chain(std::iter::once(0))) {
            *d = c;
        }
    }

    struct Factory;

    impl Class for Factory {
        type Interfaces = (IPluginFactory2,);
    }

    impl IPluginFactory2Trait for Factory {
        unsafe fn getClassInfo2(&self, index: int32, info: *mut PClassInfo2) -> tresult {
            let mut base: PClassInfo = std::mem::zeroed();
            let r = self.getClassInfo(index, &mut base);
            if r != kResultOk {
                return r;
            }
            let info = &mut *info;
            info.cid = base.cid;
            info.cardinality = base.cardinality;
            info.category = base.category;
            info.name = base.name;
            info.classFlags = 0;
            copy8(
                &mut info.subCategories,
                if index == 1 { "Instrument|Synth" } else { "Fx" },
            );
            copy8(&mut info.vendor, "Tessera");
            copy8(&mut info.version, "1.0.0");
            copy8(&mut info.sdkVersion, "VST 3.7");
            kResultOk
        }
    }

    impl IPluginFactoryTrait for Factory {
        unsafe fn getFactoryInfo(&self, info: *mut PFactoryInfo) -> tresult {
            let info = &mut *info;
            copy8(&mut info.vendor, "Tessera");
            copy8(&mut info.url, "");
            copy8(&mut info.email, "");
            info.flags = 16; // kUnicode
            kResultOk
        }
        unsafe fn countClasses(&self) -> int32 {
            2
        }
        unsafe fn getClassInfo(&self, index: int32, info: *mut PClassInfo) -> tresult {
            let info = &mut *info;
            let (cid, name) = match index {
                0 => (GAIN_CID, "Tessera Test Gain"),
                1 => (SINE_CID, "Tessera Test Sine"),
                _ => return kInvalidArgument,
            };
            info.cid = cid;
            info.cardinality = 0x7FFFFFFF;
            copy8(&mut info.category, "Audio Module Class");
            copy8(&mut info.name, name);
            kResultOk
        }
        unsafe fn createInstance(&self, cid: FIDString, iid: FIDString, obj: *mut *mut c_void) -> tresult {
            let cid = &*(cid as *const TUID);
            let instrument = if *cid == GAIN_CID {
                false
            } else if *cid == SINE_CID {
                true
            } else {
                *obj = std::ptr::null_mut();
                return kNoInterface;
            };
            let plugin = ComWrapper::new(Plugin {
                values: Values::new(),
                dsp: UnsafeCell::new(Dsp::new(instrument)),
                instrument,
            });
            let unknown = plugin.as_com_ref::<FUnknown>().expect("unknown").as_ptr();
            ((*(*unknown).vtbl).queryInterface)(unknown, iid as *const TUID, obj)
        }
    }

    /// Component, processor and controller in one object.
    struct Plugin {
        values: Values,
        dsp: UnsafeCell<Dsp>,
        instrument: bool,
    }

    // The DSP is only touched from `process` (one thread at a time).
    unsafe impl Send for Plugin {}
    unsafe impl Sync for Plugin {}

    impl Class for Plugin {
        type Interfaces = (IComponent, IAudioProcessor, IEditController);
    }

    impl IPluginBaseTrait for Plugin {
        unsafe fn initialize(&self, _context: *mut FUnknown) -> tresult {
            kResultOk
        }
        unsafe fn terminate(&self) -> tresult {
            kResultOk
        }
    }

    unsafe fn read_all(stream: *mut IBStream) -> Vec<u8> {
        let mut out = Vec::new();
        let Some(s) = ComRef::from_raw(stream) else { return out };
        let mut buf = [0u8; 64];
        loop {
            let mut n = 0;
            if s.read(buf.as_mut_ptr() as *mut c_void, 64, &mut n) != kResultOk || n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        out
    }

    unsafe fn write_state(values: &Values, stream: *mut IBStream) -> tresult {
        let Some(s) = ComRef::from_raw(stream) else {
            return kInvalidArgument;
        };
        let mut b = values.state();
        let mut n = 0;
        s.write(b.as_mut_ptr() as *mut c_void, 8, &mut n);
        if n == 8 {
            kResultOk
        } else {
            kResultFalse
        }
    }

    impl IComponentTrait for Plugin {
        unsafe fn getControllerClassId(&self, _class_id: *mut TUID) -> tresult {
            kNotImplemented
        }
        unsafe fn setIoMode(&self, _mode: IoMode) -> tresult {
            kResultOk
        }
        unsafe fn getBusCount(&self, media: MediaType, dir: BusDirection) -> int32 {
            let audio = media == MediaTypes_::kAudio as MediaType;
            let input = dir == BusDirections_::kInput as BusDirection;
            match (audio, input) {
                (true, true) => (!self.instrument) as i32,
                (true, false) => 1,
                (false, true) => self.instrument as i32,
                (false, false) => 0,
            }
        }
        unsafe fn getBusInfo(&self, media: MediaType, dir: BusDirection, index: int32, bus: *mut BusInfo) -> tresult {
            if index != 0 || self.getBusCount(media, dir) == 0 {
                return kInvalidArgument;
            }
            let bus = &mut *bus;
            bus.mediaType = media;
            bus.direction = dir;
            bus.channelCount = if media == MediaTypes_::kAudio as MediaType {
                2
            } else {
                16
            };
            copy16(&mut bus.name, "Main");
            bus.busType = BusTypes_::kMain as BusType;
            bus.flags = BusInfo_::BusFlags_::kDefaultActive as u32;
            kResultOk
        }
        unsafe fn getRoutingInfo(&self, _i: *mut RoutingInfo, _o: *mut RoutingInfo) -> tresult {
            kNotImplemented
        }
        unsafe fn activateBus(&self, _m: MediaType, _d: BusDirection, _i: int32, _s: TBool) -> tresult {
            kResultOk
        }
        unsafe fn setActive(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn setState(&self, state: *mut IBStream) -> tresult {
            if self.values.load(&read_all(state)) {
                kResultOk
            } else {
                kResultFalse
            }
        }
        unsafe fn getState(&self, state: *mut IBStream) -> tresult {
            write_state(&self.values, state)
        }
    }

    impl IAudioProcessorTrait for Plugin {
        unsafe fn setBusArrangements(
            &self,
            _inputs: *mut SpeakerArrangement,
            _num_ins: int32,
            _outputs: *mut SpeakerArrangement,
            _num_outs: int32,
        ) -> tresult {
            kResultOk
        }
        unsafe fn getBusArrangement(&self, _dir: BusDirection, _index: int32, arr: *mut SpeakerArrangement) -> tresult {
            *arr = SpeakerArr::kStereo;
            kResultOk
        }
        unsafe fn canProcessSampleSize(&self, size: int32) -> tresult {
            if size == SymbolicSampleSizes_::kSample32 as i32 {
                kResultOk
            } else {
                kResultFalse
            }
        }
        unsafe fn getLatencySamples(&self) -> uint32 {
            0
        }
        unsafe fn setupProcessing(&self, setup: *mut ProcessSetup) -> tresult {
            (*self.dsp.get()).sample_rate = (*setup).sampleRate;
            kResultOk
        }
        unsafe fn setProcessing(&self, _state: TBool) -> tresult {
            kResultOk
        }
        unsafe fn process(&self, data: *mut ProcessData) -> tresult {
            let data = &mut *data;
            let dsp = &mut *self.dsp.get();
            if let Some(changes) = ComRef::from_raw(data.inputParameterChanges) {
                for i in 0..changes.getParameterCount() {
                    if let Some(q) = ComRef::from_raw(changes.getParameterData(i)) {
                        let n = q.getPointCount();
                        let (mut off, mut v) = (0, 0.0);
                        if n > 0 && q.getPoint(n - 1, &mut off, &mut v) == kResultOk {
                            self.values.set(q.getParameterId() as usize, v);
                        }
                    }
                }
            }
            if let Some(events) = ComRef::from_raw(data.inputEvents) {
                for i in 0..events.getEventCount() {
                    let mut e: Event = std::mem::zeroed();
                    if events.getEvent(i, &mut e) == kResultOk {
                        if e.r#type == Event_::EventTypes_::kNoteOnEvent as u16 {
                            dsp.note(e.__field0.noteOn.pitch as usize, true);
                        } else if e.r#type == Event_::EventTypes_::kNoteOffEvent as u16 {
                            dsp.note(e.__field0.noteOff.pitch as usize, false);
                        }
                    }
                }
            }
            Dsp::checks(&self.values);
            let frames = data.numSamples.max(0) as usize;
            if data.numOutputs < 1 || frames == 0 {
                return kResultOk;
            }
            let out = &*data.outputs;
            let ch = out.__field0.channelBuffers32;
            let l = std::slice::from_raw_parts_mut(*ch, frames);
            let r = std::slice::from_raw_parts_mut(*ch.add(1), frames);
            if !self.instrument && data.numInputs > 0 {
                let inp = (*data.inputs).__field0.channelBuffers32;
                l.copy_from_slice(std::slice::from_raw_parts(*inp, frames));
                r.copy_from_slice(std::slice::from_raw_parts(*inp.add(1), frames));
            }
            dsp.run(&self.values, l, r);
            kResultOk
        }
        unsafe fn getTailSamples(&self) -> uint32 {
            0
        }
    }

    impl IEditControllerTrait for Plugin {
        unsafe fn setComponentState(&self, state: *mut IBStream) -> tresult {
            self.values.load(&read_all(state));
            kResultOk
        }
        unsafe fn setState(&self, _state: *mut IBStream) -> tresult {
            kResultOk
        }
        unsafe fn getState(&self, _state: *mut IBStream) -> tresult {
            kResultOk
        }
        unsafe fn getParameterCount(&self) -> int32 {
            PARAMS.len() as i32
        }
        unsafe fn getParameterInfo(&self, index: int32, info: *mut ParameterInfo) -> tresult {
            let Some(&(name, default)) = PARAMS.get(index.max(0) as usize) else {
                return kInvalidArgument;
            };
            let info = &mut *info;
            info.id = index as u32;
            copy16(&mut info.title, name);
            copy16(&mut info.shortTitle, name);
            copy16(&mut info.units, "");
            info.stepCount = if index > 0 { 1 } else { 0 };
            info.defaultNormalizedValue = default;
            info.unitId = 0;
            info.flags = ParameterInfo_::ParameterFlags_::kCanAutomate as i32;
            kResultOk
        }
        unsafe fn getParamStringByValue(&self, id: ParamID, value: ParamValue, string: *mut String128) -> tresult {
            copy16(&mut *string, &Values::text(id as usize, value));
            kResultOk
        }
        unsafe fn getParamValueByString(&self, _id: ParamID, _s: *mut TChar, _v: *mut ParamValue) -> tresult {
            kNotImplemented
        }
        unsafe fn normalizedParamToPlain(&self, _id: ParamID, v: ParamValue) -> ParamValue {
            v
        }
        unsafe fn plainParamToNormalized(&self, _id: ParamID, v: ParamValue) -> ParamValue {
            v
        }
        unsafe fn getParamNormalized(&self, id: ParamID) -> ParamValue {
            self.values.get(id as usize)
        }
        unsafe fn setParamNormalized(&self, id: ParamID, value: ParamValue) -> tresult {
            self.values.set(id as usize, value);
            kResultOk
        }
        unsafe fn setComponentHandler(&self, _handler: *mut IComponentHandler) -> tresult {
            kResultOk
        }
        unsafe fn createView(&self, _name: FIDString) -> *mut IPlugView {
            std::ptr::null_mut()
        }
    }

    #[no_mangle]
    pub extern "system" fn GetPluginFactory() -> *mut IPluginFactory {
        ComWrapper::new(Factory)
            .to_com_ptr::<IPluginFactory2>()
            .map(|p| p.into_raw() as *mut IPluginFactory)
            .unwrap_or(std::ptr::null_mut())
    }

    #[no_mangle]
    pub extern "C" fn ModuleEntry(_: *mut c_void) -> bool {
        true
    }
    #[no_mangle]
    pub extern "C" fn ModuleExit() -> bool {
        true
    }
    #[no_mangle]
    pub extern "C" fn bundleEntry(_: *mut c_void) -> bool {
        true
    }
    #[no_mangle]
    pub extern "C" fn bundleExit() -> bool {
        true
    }
    #[no_mangle]
    pub extern "system" fn InitDll() -> bool {
        true
    }
    #[no_mangle]
    pub extern "system" fn ExitDll() -> bool {
        true
    }
}
