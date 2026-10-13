//! Audio effects and device chains.
//!
//! Every track, return track and the master bus carries a chain of insert
//! effects ([`Device`]s) processed in series after the instrument. Each
//! effect's settings are a plain `Copy` struct of `f32` parameters described
//! by a static [`ParamInfo`] table, so the UI can draw generic controls and
//! single parameters can be changed (or later automated) by index without
//! rebuilding the engine snapshot.
//!
//! The audio-thread side lives in [`chain`]: a [`chain::FxChain`] owns one
//! runtime per device, allocated on the UI thread when a snapshot is compiled
//! and moved between snapshots so reverb and delay tails survive edits.

// Per-channel DSP loops read more clearly with explicit indices.
#![allow(clippy::needless_range_loop)]

pub mod amp;
pub mod auto_filter;
pub mod auto_pan;
pub mod beat_repeat;
pub mod biquad;
pub mod cabinet;
pub mod chain;
pub mod chorus;
pub mod compressor;
pub mod delay;
pub mod drum_bus;
pub mod echo;
pub mod eq;
pub mod erosion;
pub mod fft;
pub mod gate;
pub mod glue;
pub mod grain_delay;
pub mod hybrid_reverb;
pub mod limiter;
pub mod looper;
pub mod multiband;
pub mod overdrive;
pub mod phaser;
pub mod rack;
pub mod redux;
pub mod reverb;
pub mod saturator;
pub mod scope;
pub mod shifter;
pub mod spectrum;
pub mod tuner;
pub mod utility;
pub mod vinyl;

use serde::{Deserialize, Serialize};

use crate::model::Id;
use crate::plugin::PluginParams;

pub use chain::{FxChain, FxContext};

/// Upper bound on return tracks (sends per track).
pub const MAX_RETURNS: usize = 12;

/// How a knob maps to its value range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Linear,
    /// Logarithmic; the range must be positive.
    Log,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Continuous(Scale),
    /// 0 = off, 1 = on.
    Toggle,
    /// An index into a list of labels.
    Choice(&'static [&'static str]),
}

/// Display unit of a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Plain,
    Db,
    Hz,
    Ms,
    Seconds,
    Percent,
    Ratio,
    /// A whole number of semitones, shown with a sign.
    Semitones,
    /// A whole number.
    Integer,
}

/// Static description of one device parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamInfo {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub kind: ParamKind,
    pub unit: Unit,
}

impl ParamInfo {
    pub const fn new(name: &'static str, min: f32, max: f32, default: f32, kind: ParamKind, unit: Unit) -> Self {
        Self {
            name,
            min,
            max,
            default,
            kind,
            unit,
        }
    }

    /// Clamps (and for toggles and choices, rounds) a value into range.
    pub fn clamp(&self, v: f32) -> f32 {
        let v = if v.is_finite() { v } else { self.default };
        match self.kind {
            ParamKind::Continuous(_) => v.clamp(self.min, self.max),
            ParamKind::Toggle => {
                if v >= 0.5 {
                    1.0
                } else {
                    0.0
                }
            }
            ParamKind::Choice(labels) => v.round().clamp(0.0, labels.len().saturating_sub(1) as f32),
        }
    }

    /// Maps 0 ..= 1 onto the parameter's range (logarithmically for log
    /// parameters), e.g. for a MIDI controller knob.
    pub fn from_unit(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        let v = match self.kind {
            ParamKind::Continuous(Scale::Log) if self.min > 0.0 => {
                (self.min.ln() + n * (self.max.ln() - self.min.ln())).exp()
            }
            _ => self.min + n * (self.max - self.min),
        };
        self.clamp(v)
    }

    /// The inverse of [`ParamInfo::from_unit`].
    pub fn to_unit(&self, v: f32) -> f32 {
        let n = match self.kind {
            ParamKind::Continuous(Scale::Log) if self.min > 0.0 => {
                (v.max(self.min).ln() - self.min.ln()) / (self.max.ln() - self.min.ln())
            }
            _ => (v - self.min) / (self.max - self.min),
        };
        if n.is_finite() {
            n.clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Formats a value for display.
    pub fn format(&self, v: f32) -> String {
        match self.kind {
            ParamKind::Toggle => return if v >= 0.5 { "On".into() } else { "Off".into() },
            ParamKind::Choice(labels) => {
                return labels
                    .get(v.round().max(0.0) as usize)
                    .copied()
                    .unwrap_or("")
                    .to_string()
            }
            ParamKind::Continuous(_) => {}
        }
        match self.unit {
            Unit::Plain => format!("{v:.2}"),
            Unit::Db => {
                if v <= -69.9 {
                    "-inf dB".into()
                } else {
                    format!("{v:.1} dB")
                }
            }
            Unit::Hz => {
                if v >= 1000.0 {
                    format!("{:.2} kHz", v / 1000.0)
                } else if v >= 100.0 {
                    format!("{v:.0} Hz")
                } else {
                    format!("{v:.1} Hz")
                }
            }
            Unit::Ms => {
                if v >= 1000.0 {
                    format!("{:.2} s", v / 1000.0)
                } else if v >= 10.0 {
                    format!("{v:.0} ms")
                } else {
                    format!("{v:.1} ms")
                }
            }
            Unit::Seconds => format!("{v:.2} s"),
            Unit::Percent => format!("{:.0} %", v * 100.0),
            Unit::Ratio => {
                if v >= 19.95 {
                    "inf : 1".into()
                } else {
                    format!("{v:.1} : 1")
                }
            }
            Unit::Semitones => format!("{:+} st", v.round() as i32),
            Unit::Integer => format!("{}", v.round() as i32),
        }
    }
}

/// Declares a parameter struct together with its [`ParamInfo`] table and
/// index-based accessors.
macro_rules! device_params {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            $( $field:ident : $label:literal, $min:expr, $max:expr, $def:expr, $kind:expr, $unit:expr; )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(default)]
        pub struct $name {
            $( pub $field: f32, )*
        }

        impl Default for $name {
            fn default() -> Self {
                Self { $( $field: $def, )* }
            }
        }

        impl $name {
            /// Number of parameters.
            pub const COUNT: usize = [$( stringify!($field), )*].len();

            pub const INFO: &'static [$crate::fx::ParamInfo] = &[
                $( $crate::fx::ParamInfo::new($label, $min, $max, $def, $kind, $unit), )*
            ];

            pub fn get(&self, index: usize) -> f32 {
                let values = [ $( self.$field, )* ];
                values.get(index).copied().unwrap_or(0.0)
            }

            pub fn set(&mut self, index: usize, value: f32) {
                let Some(info) = Self::INFO.get(index) else { return };
                let value = info.clamp(value);
                let fields = [ $( &mut self.$field, )* ];
                if let Some(f) = fields.into_iter().nth(index) {
                    *f = value;
                }
            }
        }

        impl $crate::fx::Params for $name {
            fn info() -> &'static [$crate::fx::ParamInfo] {
                Self::INFO
            }
            fn get_param(&self, index: usize) -> f32 {
                self.get(index)
            }
            fn set_param(&mut self, index: usize, value: f32) {
                self.set(index, value)
            }
        }
    };
}
pub(crate) use device_params;

/// A parameter table generated by `device_params!`, for generic code (UI
/// panels, automation).
pub trait Params: Copy {
    fn info() -> &'static [ParamInfo];
    fn get_param(&self, index: usize) -> f32;
    fn set_param(&mut self, index: usize, value: f32);
}

/// Hooks every effect's parameter struct provides (default methods cover the
/// common case).
pub trait EffectParams: Sized {
    /// Whether a parameter matters with the current settings (for example a
    /// delay's millisecond time is hidden while it is synced to the tempo).
    fn is_relevant(&self, _index: usize) -> bool {
        true
    }
    /// Factory presets shown in the browser under the effect.
    fn presets() -> Vec<(&'static str, Self)> {
        Vec::new()
    }
}

/// Hooks every effect's audio-thread state provides.
pub trait EffectRuntime {
    /// Gain reduction in dB for dynamics devices.
    fn meter(&self) -> Option<f32> {
        None
    }
    /// Called once after `new` with the device id, on the UI thread (may
    /// allocate). Analysis devices use it to find their [`scope`] tap.
    fn attach(&mut self, _device: Id) {}
}

macro_rules! count_kinds {
    () => { 0usize };
    ($head:ident $($tail:ident)*) => { 1usize + count_kinds!($($tail)*) };
}

/// Declares the built-in effects: [`DeviceKind`], [`DeviceParams`] and the
/// audio-thread [`chain`] runtime are generated from this one list, so a new
/// effect is a module plus one line here.
macro_rules! builtin_effects {
    ($( $kind:ident => $module:ident :: $params:ident / $rt:ident, $name:literal, $category:ident, $desc:literal; )*) => {
        /// The kinds of audio effects.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DeviceKind {
            $( $kind, )*
            /// Parallel chains of effects with macro knobs (see [`rack`]).
            AudioRack,
            /// A VST3 or CLAP plug-in (see [`crate::plugin`]). Not in [`DeviceKind::ALL`]:
            /// plug-in devices are created from the browser's Plug-Ins category.
            Plugin,
        }

        impl DeviceKind {
            /// Every built-in effect (racks and plug-ins aside).
            pub const ALL: [DeviceKind; count_kinds!($($kind)*)] = [ $( DeviceKind::$kind, )* ];

            pub fn name(self) -> &'static str {
                match self {
                    $( DeviceKind::$kind => $name, )*
                    DeviceKind::AudioRack => "Audio Effect Rack",
                    DeviceKind::Plugin => "Plug-in",
                }
            }

            pub fn description(self) -> &'static str {
                match self {
                    $( DeviceKind::$kind => $desc, )*
                    DeviceKind::AudioRack => "Parallel chains of effects with eight macro knobs",
                    DeviceKind::Plugin => "A VST3 or CLAP audio effect",
                }
            }

            /// Browser and menu folder.
            pub fn category(self) -> Category {
                match self {
                    $( DeviceKind::$kind => Category::$category, )*
                    DeviceKind::AudioRack => Category::Racks,
                    DeviceKind::Plugin => Category::Utilities,
                }
            }

            pub fn default_params(self) -> DeviceParams {
                match self {
                    $( DeviceKind::$kind => DeviceParams::$kind($module::$params::default()), )*
                    DeviceKind::AudioRack => DeviceParams::Rack(rack::RackParams::default()),
                    DeviceKind::Plugin => DeviceParams::Plugin(PluginParams::default()),
                }
            }

            /// Factory presets of the effect.
            pub fn presets(self) -> Vec<(&'static str, DeviceParams)> {
                match self {
                    $( DeviceKind::$kind => <$module::$params as EffectParams>::presets()
                        .into_iter()
                        .map(|(n, p)| (n, DeviceParams::$kind(p)))
                        .collect(), )*
                    DeviceKind::AudioRack | DeviceKind::Plugin => Vec::new(),
                }
            }
        }

        /// Settings of one effect.
        #[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
        pub enum DeviceParams {
            $( $kind($module::$params), )*
            /// Macro values and chain selector of an Audio Effect Rack; the
            /// chains live in [`Device::rack`].
            Rack(rack::RackParams),
            /// Parameters live in the plug-in; the device id names its slot.
            Plugin(PluginParams),
        }

        impl DeviceParams {
            pub fn kind(&self) -> DeviceKind {
                match self {
                    $( DeviceParams::$kind(_) => DeviceKind::$kind, )*
                    DeviceParams::Rack(_) => DeviceKind::AudioRack,
                    DeviceParams::Plugin(_) => DeviceKind::Plugin,
                }
            }

            pub fn info(&self) -> &'static [ParamInfo] {
                match self {
                    $( DeviceParams::$kind(_) => $module::$params::INFO, )*
                    DeviceParams::Rack(_) => rack::RackParams::INFO,
                    DeviceParams::Plugin(_) => &[],
                }
            }

            pub fn get(&self, index: usize) -> f32 {
                match self {
                    $( DeviceParams::$kind(p) => p.get(index), )*
                    DeviceParams::Rack(p) => p.get(index),
                    DeviceParams::Plugin(_) => 0.0,
                }
            }

            pub fn set(&mut self, index: usize, value: f32) {
                match self {
                    $( DeviceParams::$kind(p) => p.set(index, value), )*
                    DeviceParams::Rack(p) => p.set(index, value),
                    DeviceParams::Plugin(_) => {}
                }
            }

            /// Whether a parameter matters with the current settings.
            pub fn is_relevant(&self, index: usize) -> bool {
                match self {
                    $( DeviceParams::$kind(p) => EffectParams::is_relevant(p, index), )*
                    DeviceParams::Rack(_) | DeviceParams::Plugin(_) => true,
                }
            }
        }

        /// DSP state of one built-in effect. Boxed so moving it between
        /// snapshots is cheap.
        pub(crate) enum BuiltinRuntime {
            $( $kind(Box<$module::$rt>), )*
        }

        impl BuiltinRuntime {
            /// `None` for racks and plug-ins. Allocates; UI thread only.
            pub(crate) fn new(device: Id, params: &DeviceParams, sample_rate: f32) -> Option<Self> {
                Some(match params {
                    $( DeviceParams::$kind(_) => {
                        let mut rt = Box::new($module::$rt::new(sample_rate));
                        EffectRuntime::attach(&mut *rt, device);
                        BuiltinRuntime::$kind(rt)
                    } )*
                    DeviceParams::Rack(_) | DeviceParams::Plugin(_) => return None,
                })
            }

            pub(crate) fn same_kind(&self, params: &DeviceParams) -> bool {
                match (self, params) {
                    $( (BuiltinRuntime::$kind(_), DeviceParams::$kind(_)) => true, )*
                    _ => false,
                }
            }

            #[inline]
            pub(crate) fn process(&mut self, params: &DeviceParams, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
                match (self, params) {
                    $( (BuiltinRuntime::$kind(d), DeviceParams::$kind(p)) => d.process(p, l, r, ctx), )*
                    _ => {}
                }
            }

            pub(crate) fn reset(&mut self) {
                match self {
                    $( BuiltinRuntime::$kind(d) => d.reset(), )*
                }
            }

            pub(crate) fn meter(&self) -> Option<f32> {
                match self {
                    $( BuiltinRuntime::$kind(d) => EffectRuntime::meter(&**d), )*
                }
            }
        }
    };
}

/// Folders of the effect menus and the browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    EqFilters,
    Dynamics,
    Distortion,
    Modulation,
    DelayLoop,
    Reverb,
    Pitch,
    Utilities,
    Racks,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Racks,
        Category::Dynamics,
        Category::EqFilters,
        Category::Distortion,
        Category::Modulation,
        Category::DelayLoop,
        Category::Reverb,
        Category::Pitch,
        Category::Utilities,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Category::EqFilters => "EQ & Filters",
            Category::Dynamics => "Dynamics",
            Category::Distortion => "Distortion",
            Category::Modulation => "Modulation",
            Category::DelayLoop => "Delay & Loop",
            Category::Reverb => "Reverb",
            Category::Pitch => "Pitch",
            Category::Utilities => "Utilities",
            Category::Racks => "Racks",
        }
    }

    /// Built-in effects in this folder, in menu order.
    pub fn kinds(self) -> Vec<DeviceKind> {
        let mut v: Vec<DeviceKind> = DeviceKind::ALL.into_iter().filter(|k| k.category() == self).collect();
        if self == Category::Racks {
            v.push(DeviceKind::AudioRack);
        }
        v
    }
}

// The first eight keep their original order (tests index into `ALL`).
builtin_effects! {
    Eq => eq::EqParams / Eq, "Parametric EQ", EqFilters,
        "Eight-band parametric equalizer with cuts, shelves and bells";
    Compressor => compressor::CompressorParams / Compressor, "Compressor", Dynamics,
        "Feed-forward compressor with soft knee and makeup gain";
    Saturator => saturator::SaturatorParams / Saturator, "Saturator", Distortion,
        "Waveshaping distortion: soft clip, hard clip, tube, fold";
    Chorus => chorus::ChorusParams / Chorus, "Chorus", Modulation,
        "Modulated delay chorus / flanger";
    Delay => delay::DelayParams / Delay, "Delay", DelayLoop,
        "Stereo delay with tempo sync, ping-pong and filtered feedback";
    Reverb => reverb::ReverbParams / Reverb, "Reverb", Reverb,
        "Algorithmic stereo reverb (feedback delay network)";
    Limiter => limiter::LimiterParams / Limiter, "Limiter", Dynamics,
        "Look-ahead brickwall limiter";
    Utility => utility::UtilityParams / Utility, "Utility", Utilities,
        "Gain, stereo width, mono and phase";
    AutoFilter => auto_filter::AutoFilterParams / AutoFilter, "Auto Filter", EqFilters,
        "Resonant filter with LFO, envelope follower and sidechain-free sweeps";
    AutoPan => auto_pan::AutoPanParams / AutoPan, "Auto Pan", Modulation,
        "Tempo-synced panning and tremolo";
    PhaserFlanger => phaser::PhaserFlangerParams / PhaserFlanger, "Phaser-Flanger", Modulation,
        "Allpass phaser, flanger and doubler with LFO and envelope modulation";
    Shifter => shifter::ShifterParams / Shifter, "Shifter", Pitch,
        "Pitch shifter, frequency shifter and ring modulator";
    Echo => echo::EchoParams / Echo, "Echo", DelayLoop,
        "Character tape-style delay with modulation, filters, ducking and reverb";
    GrainDelay => grain_delay::GrainDelayParams / GrainDelay, "Grain Delay", DelayLoop,
        "Granular delay with pitch, spray and frequency controls";
    BeatRepeat => beat_repeat::BeatRepeatParams / BeatRepeat, "Beat Repeat", DelayLoop,
        "Tempo-synced stutters and repeats with pitch decay and filters";
    Looper => looper::LooperParams / Looper, "Looper", DelayLoop,
        "Record, overdub and play back tempo-synced loops";
    BusCompressor => glue::BusCompressorParams / BusCompressor, "Bus Compressor", Dynamics,
        "Bus-style glue compressor with soft clip and range";
    MultibandDynamics => multiband::MultibandParams / MultibandDynamics, "Multiband Dynamics", Dynamics,
        "Three-band upward and downward compressor / expander";
    Gate => gate::GateParams / Gate, "Gate", Dynamics,
        "Noise gate with hysteresis, hold, range and flip";
    DrumBus => drum_bus::DrumBusParams / DrumBus, "Drum Bus", Dynamics,
        "Drive, crunch, transient shaping and low-end boom for drums";
    Overdrive => overdrive::OverdriveParams / Overdrive, "Overdrive", Distortion,
        "Guitar pedal: overdrive, distortion and fuzz with tone stack";
    Amp => amp::AmpParams / Amp, "Amp", Distortion,
        "Guitar amplifier models with gain, EQ and presence";
    Cabinet => cabinet::CabinetParams / Cabinet, "Cabinet", Distortion,
        "Speaker cabinet and microphone simulation";
    Redux => redux::ReduxParams / Redux, "Redux", Distortion,
        "Sample-rate and bit-depth reduction";
    Erosion => erosion::ErosionParams / Erosion, "Erosion", Distortion,
        "Noise and sine modulation degradation";
    VinylDistortion => vinyl::VinylParams / VinylDistortion, "Vinyl Distortion", Distortion,
        "Record wear, pinch distortion and crackle";
    HybridReverb => hybrid_reverb::HybridReverbParams / HybridReverb, "Hybrid Reverb", Reverb,
        "Convolution and algorithmic reverb in series or parallel";
    Spectrum => spectrum::SpectrumParams / Spectrum, "Spectrum", Utilities,
        "Real-time frequency analyzer";
    Tuner => tuner::TunerParams / Tuner, "Tuner", Utilities,
        "Pitch detector for tuning instruments";
}

/// One effect in a device chain, as saved in the project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: Id,
    /// Device activator; a switched-off device passes audio through.
    pub on: bool,
    pub params: DeviceParams,
    /// The chains of an Audio Effect Rack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rack: Option<Box<rack::Rack>>,
}

impl Device {
    /// A device with default settings. An Audio Effect Rack starts without
    /// chains (see [`Device::new_rack`]).
    pub fn new(id: Id, kind: DeviceKind) -> Self {
        Self {
            id,
            on: true,
            params: kind.default_params(),
            rack: (kind == DeviceKind::AudioRack).then(|| Box::new(rack::Rack::default())),
        }
    }

    /// An Audio Effect Rack holding `rack`.
    pub fn new_rack(id: Id, rack: rack::Rack) -> Self {
        Self {
            id,
            on: true,
            params: DeviceParams::Rack(rack::RackParams::default()),
            rack: Some(Box::new(rack)),
        }
    }

    /// This device and every device nested in it, depth first.
    pub fn visit<'a>(&'a self, f: &mut dyn FnMut(&'a Device)) {
        f(self);
        if let Some(r) = &self.rack {
            r.visit_devices(f);
        }
    }

    pub fn kind(&self) -> DeviceKind {
        self.params.kind()
    }
}

/// Converts decibels to linear gain without the -70 dB silence floor used by
/// faders.
#[inline]
pub fn db(v: f32) -> f32 {
    10f32.powf(v * 0.05)
}

/// One-pole smoothing coefficient for a time constant in seconds.
#[inline]
pub fn smoothing(seconds: f32, sample_rate: f32) -> f32 {
    if seconds <= 0.0 {
        0.0
    } else {
        (-1.0 / (seconds * sample_rate)).exp()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_round_trip_by_index() {
        for kind in DeviceKind::ALL {
            let mut p = kind.default_params();
            for (i, info) in p.info().iter().enumerate() {
                assert_eq!(p.get(i), info.default, "{} {}", kind.name(), info.name);
                assert!(info.min <= info.default && info.default <= info.max, "{}", info.name);
                p.set(i, info.max + 1000.0);
                assert_eq!(p.get(i), info.clamp(info.max), "{} {}", kind.name(), info.name);
            }
        }
    }

    #[test]
    fn devices_serialize() {
        let d = Device::new(7, DeviceKind::Delay);
        let json = serde_json::to_string(&d).unwrap();
        let back: Device = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        // Missing parameters fall back to defaults.
        let partial: Device =
            serde_json::from_str(r#"{"id":1,"on":true,"params":{"Compressor":{"ratio":8.0}}}"#).unwrap();
        let DeviceParams::Compressor(c) = partial.params else {
            panic!()
        };
        assert_eq!(c.ratio, 8.0);
        assert_eq!(c.threshold, compressor::CompressorParams::default().threshold);
    }

    #[test]
    fn formatting() {
        let i = ParamInfo::new("f", 20.0, 20000.0, 1000.0, ParamKind::Continuous(Scale::Log), Unit::Hz);
        assert_eq!(i.format(1500.0), "1.50 kHz");
        let c = ParamInfo::new("t", 0.0, 2.0, 0.0, ParamKind::Choice(&["A", "B", "C"]), Unit::Plain);
        assert_eq!(c.format(1.2), "B");
        assert_eq!(c.clamp(7.0), 2.0);
    }
}
