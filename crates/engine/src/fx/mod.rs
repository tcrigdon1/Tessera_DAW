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

pub mod biquad;
pub mod chain;
pub mod chorus;
pub mod compressor;
pub mod delay;
pub mod eq;
pub mod limiter;
pub mod reverb;
pub mod saturator;
pub mod utility;

use serde::{Deserialize, Serialize};

use crate::model::Id;

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
    };
}
pub(crate) use device_params;

/// The kinds of built-in audio effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Eq,
    Compressor,
    Reverb,
    Delay,
    Saturator,
    Chorus,
    Limiter,
    Utility,
}

impl DeviceKind {
    pub const ALL: [DeviceKind; 8] = [
        DeviceKind::Eq,
        DeviceKind::Compressor,
        DeviceKind::Saturator,
        DeviceKind::Chorus,
        DeviceKind::Delay,
        DeviceKind::Reverb,
        DeviceKind::Limiter,
        DeviceKind::Utility,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DeviceKind::Eq => "Parametric EQ",
            DeviceKind::Compressor => "Compressor",
            DeviceKind::Reverb => "Reverb",
            DeviceKind::Delay => "Delay",
            DeviceKind::Saturator => "Saturator",
            DeviceKind::Chorus => "Chorus",
            DeviceKind::Limiter => "Limiter",
            DeviceKind::Utility => "Utility",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            DeviceKind::Eq => "Eight-band parametric equalizer with cuts, shelves and bells",
            DeviceKind::Compressor => "Feed-forward compressor with soft knee and makeup gain",
            DeviceKind::Reverb => "Algorithmic stereo reverb (feedback delay network)",
            DeviceKind::Delay => "Stereo delay with tempo sync, ping-pong and filtered feedback",
            DeviceKind::Saturator => "Waveshaping distortion: soft clip, hard clip, tube, fold",
            DeviceKind::Chorus => "Modulated delay chorus / flanger",
            DeviceKind::Limiter => "Look-ahead brickwall limiter",
            DeviceKind::Utility => "Gain, stereo width, mono and phase",
        }
    }

    pub fn default_params(self) -> DeviceParams {
        match self {
            DeviceKind::Eq => DeviceParams::Eq(eq::EqParams::default()),
            DeviceKind::Compressor => DeviceParams::Compressor(compressor::CompressorParams::default()),
            DeviceKind::Reverb => DeviceParams::Reverb(reverb::ReverbParams::default()),
            DeviceKind::Delay => DeviceParams::Delay(delay::DelayParams::default()),
            DeviceKind::Saturator => DeviceParams::Saturator(saturator::SaturatorParams::default()),
            DeviceKind::Chorus => DeviceParams::Chorus(chorus::ChorusParams::default()),
            DeviceKind::Limiter => DeviceParams::Limiter(limiter::LimiterParams::default()),
            DeviceKind::Utility => DeviceParams::Utility(utility::UtilityParams::default()),
        }
    }
}

/// Settings of one effect.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum DeviceParams {
    Eq(eq::EqParams),
    Compressor(compressor::CompressorParams),
    Reverb(reverb::ReverbParams),
    Delay(delay::DelayParams),
    Saturator(saturator::SaturatorParams),
    Chorus(chorus::ChorusParams),
    Limiter(limiter::LimiterParams),
    Utility(utility::UtilityParams),
}

impl DeviceParams {
    pub fn kind(&self) -> DeviceKind {
        match self {
            DeviceParams::Eq(_) => DeviceKind::Eq,
            DeviceParams::Compressor(_) => DeviceKind::Compressor,
            DeviceParams::Reverb(_) => DeviceKind::Reverb,
            DeviceParams::Delay(_) => DeviceKind::Delay,
            DeviceParams::Saturator(_) => DeviceKind::Saturator,
            DeviceParams::Chorus(_) => DeviceKind::Chorus,
            DeviceParams::Limiter(_) => DeviceKind::Limiter,
            DeviceParams::Utility(_) => DeviceKind::Utility,
        }
    }

    pub fn info(&self) -> &'static [ParamInfo] {
        match self {
            DeviceParams::Eq(_) => eq::EqParams::INFO,
            DeviceParams::Compressor(_) => compressor::CompressorParams::INFO,
            DeviceParams::Reverb(_) => reverb::ReverbParams::INFO,
            DeviceParams::Delay(_) => delay::DelayParams::INFO,
            DeviceParams::Saturator(_) => saturator::SaturatorParams::INFO,
            DeviceParams::Chorus(_) => chorus::ChorusParams::INFO,
            DeviceParams::Limiter(_) => limiter::LimiterParams::INFO,
            DeviceParams::Utility(_) => utility::UtilityParams::INFO,
        }
    }

    pub fn get(&self, index: usize) -> f32 {
        match self {
            DeviceParams::Eq(p) => p.get(index),
            DeviceParams::Compressor(p) => p.get(index),
            DeviceParams::Reverb(p) => p.get(index),
            DeviceParams::Delay(p) => p.get(index),
            DeviceParams::Saturator(p) => p.get(index),
            DeviceParams::Chorus(p) => p.get(index),
            DeviceParams::Limiter(p) => p.get(index),
            DeviceParams::Utility(p) => p.get(index),
        }
    }

    pub fn set(&mut self, index: usize, value: f32) {
        match self {
            DeviceParams::Eq(p) => p.set(index, value),
            DeviceParams::Compressor(p) => p.set(index, value),
            DeviceParams::Reverb(p) => p.set(index, value),
            DeviceParams::Delay(p) => p.set(index, value),
            DeviceParams::Saturator(p) => p.set(index, value),
            DeviceParams::Chorus(p) => p.set(index, value),
            DeviceParams::Limiter(p) => p.set(index, value),
            DeviceParams::Utility(p) => p.set(index, value),
        }
    }

    /// Whether a parameter matters with the current settings (for example a
    /// delay's millisecond time is hidden while it is synced to the tempo).
    pub fn is_relevant(&self, index: usize) -> bool {
        match self {
            DeviceParams::Delay(p) => p.is_relevant(index),
            _ => true,
        }
    }
}

/// One effect in a device chain, as saved in the project.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Device {
    pub id: Id,
    /// Device activator; a switched-off device passes audio through.
    pub on: bool,
    pub params: DeviceParams,
}

impl Device {
    pub fn new(id: Id, kind: DeviceKind) -> Self {
        Self {
            id,
            on: true,
            params: kind.default_params(),
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
        assert!((i.from_unit(0.5) - 632.4555).abs() < 0.01);
        assert!((i.to_unit(i.from_unit(0.3)) - 0.3).abs() < 1e-5);
        assert_eq!(c.from_unit(0.6), 1.0);
    }
}
