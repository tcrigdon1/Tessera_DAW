//! Automation: breakpoint envelopes for mixer and device parameters.
//!
//! Envelopes store *normalised* values (0..1) against a position in beats.
//! A [`ParamSpec`] maps the normalised value to the parameter's real range
//! (dB for faders, Hz for cutoffs, …), so the same envelope editor works for
//! every parameter.
//!
//! Two kinds of envelope exist, sharing the [`AutomationLane`] type:
//!
//! * **Arrangement automation** on a [`crate::model::Track`]: positions are
//!   song beats. It drives the track while it plays the Arrangement.
//! * **Clip envelopes** on a [`crate::model::Clip`]: positions are clip-local
//!   beats and loop with the clip. They drive the track while the clip plays
//!   (Session clips always; Arrangement clips where the track has no lane for
//!   the same parameter).

use serde::{Deserialize, Serialize};

use crate::model::{Id, Instrument};

/// A parameter that can be automated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AutomationTarget {
    /// Track volume fader.
    Volume,
    /// Track pan.
    Pan,
    /// Send level to return track `n`. Evaluated by the engine and exposed
    /// through `TrackAutomation::value`, ready for the send mixer.
    Send(u8),
    /// Parameter `n` of the track's instrument (see [`instrument_params`]).
    Instrument(u16),
    /// Parameter `param` of an effect device in the track's device chain.
    /// Evaluated by the engine and exposed through `TrackAutomation::value`.
    Device { device: Id, param: u16 },
}

/// How a normalised value maps onto a parameter's range.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ParamScale {
    Linear,
    /// Logarithmic, for frequencies and times. Range must be positive.
    Log,
    /// The volume fader curve in dB (-inf at 0, +6 dB at 1).
    Fader,
    /// Linear but rounded to whole numbers.
    Stepped,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub scale: ParamScale,
    pub unit: &'static str,
}

const fn spec(name: &'static str, min: f32, max: f32, scale: ParamScale, unit: &'static str) -> ParamSpec {
    ParamSpec {
        name,
        min,
        max,
        scale,
        unit,
    }
}

pub const VOLUME_SPEC: ParamSpec = spec("Volume", -70.0, 6.0, ParamScale::Fader, "dB");
pub const PAN_SPEC: ParamSpec = spec("Pan", -1.0, 1.0, ParamScale::Linear, "");
pub const SEND_SPEC: ParamSpec = spec("Send", -70.0, 0.0, ParamScale::Fader, "dB");

/// Maps a fader dB value to its 0..1 position (same curve as the mixer fader).
pub fn fader_db_to_pos(db: f32) -> f32 {
    if db <= -70.0 {
        0.0
    } else if db <= -12.0 {
        (db + 70.0) / 58.0 * 0.5
    } else {
        (0.5 + (db + 12.0) / 18.0 * 0.5).min(1.0)
    }
}

pub fn fader_pos_to_db(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    if p <= 0.0 {
        -70.0
    } else if p <= 0.5 {
        p / 0.5 * 58.0 - 70.0
    } else {
        (p - 0.5) / 0.5 * 18.0 - 12.0
    }
}

impl ParamSpec {
    /// Normalised 0..1 to the parameter value.
    pub fn denormalize(&self, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        match self.scale {
            ParamScale::Linear => self.min + n * (self.max - self.min),
            ParamScale::Stepped => (self.min + n * (self.max - self.min)).round(),
            ParamScale::Log => (self.min.ln() + n * (self.max.ln() - self.min.ln())).exp(),
            ParamScale::Fader => {
                // The fader curve is defined for -70..+6; rescale for other tops.
                let top = fader_db_to_pos(self.max);
                fader_pos_to_db(n * top)
            }
        }
    }

    /// Parameter value to normalised 0..1.
    pub fn normalize(&self, v: f32) -> f32 {
        match self.scale {
            ParamScale::Linear | ParamScale::Stepped => (v - self.min) / (self.max - self.min),
            ParamScale::Log => (v.max(self.min).ln() - self.min.ln()) / (self.max.ln() - self.min.ln()),
            ParamScale::Fader => {
                let top = fader_db_to_pos(self.max);
                if top <= 0.0 {
                    0.0
                } else {
                    fader_db_to_pos(v) / top
                }
            }
        }
        .clamp(0.0, 1.0)
    }

    /// Human readable value with unit.
    pub fn format(&self, v: f32) -> String {
        match (self.scale, self.unit) {
            (ParamScale::Fader, _) if v <= -70.0 => "-inf dB".into(),
            (_, "Hz") if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
            (_, "Hz") => format!("{v:.0} Hz"),
            (_, "s") if v < 1.0 => format!("{:.0} ms", v * 1000.0),
            (_, "s") => format!("{v:.2} s"),
            (_, "%") => format!("{:.0}%", v * 100.0),
            (_, "") if self.name.ends_with("Pan") => {
                if v.abs() < 0.01 {
                    "C".into()
                } else if v < 0.0 {
                    format!("{:.0}L", -v * 50.0)
                } else {
                    format!("{:.0}R", v * 50.0)
                }
            }
            (ParamScale::Stepped, u) => format!("{v:+.0} {u}"),
            (_, u) => format!("{v:.1} {u}").trim_end().to_string(),
        }
    }
}

/// Automatable Poly Synth parameters, in a stable order (envelopes refer to
/// them by index, so only append to this list).
pub const SYNTH_PARAMS: [ParamSpec; 15] = [
    spec("Osc 2 Semi", -24.0, 24.0, ParamScale::Stepped, "st"),
    spec("Osc 2 Detune", -50.0, 50.0, ParamScale::Linear, "ct"),
    spec("Osc Mix", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter Cutoff", 30.0, 18000.0, ParamScale::Log, "Hz"),
    spec("Filter Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Filter Env Amt", -4.0, 6.0, ParamScale::Linear, "oct"),
    spec("Amp Attack", 0.001, 5.0, ParamScale::Log, "s"),
    spec("Amp Decay", 0.005, 5.0, ParamScale::Log, "s"),
    spec("Amp Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Amp Release", 0.005, 8.0, ParamScale::Log, "s"),
    spec("Filter Attack", 0.001, 5.0, ParamScale::Log, "s"),
    spec("Filter Decay", 0.005, 5.0, ParamScale::Log, "s"),
    spec("Filter Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter Release", 0.005, 8.0, ParamScale::Log, "s"),
    spec("Synth Volume", -40.0, 6.0, ParamScale::Linear, "dB"),
];

/// Automatable Drum Kit parameters (append only).
pub const DRUM_PARAMS: [ParamSpec; 6] = [
    spec("Kick Tune", 30.0, 90.0, ParamScale::Linear, "Hz"),
    spec("Kick Decay", 0.05, 2.0, ParamScale::Log, "s"),
    spec("Snare Tone", 100.0, 400.0, ParamScale::Linear, "Hz"),
    spec("Snare Decay", 0.03, 1.0, ParamScale::Log, "s"),
    spec("Hat Decay", 0.01, 0.5, ParamScale::Log, "s"),
    spec("Kit Volume", -40.0, 6.0, ParamScale::Linear, "dB"),
];

/// Automatable Sampler parameters (append only).
pub const SAMPLER_PARAMS: [ParamSpec; 18] = [
    spec("Sample Start", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Sample End", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Transpose", -48.0, 48.0, ParamScale::Stepped, "st"),
    spec("Detune", -50.0, 50.0, ParamScale::Linear, "ct"),
    spec("Filter Cutoff", 30.0, 18000.0, ParamScale::Log, "Hz"),
    spec("Filter Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Filter Env Amt", -4.0, 6.0, ParamScale::Linear, "oct"),
    spec("Amp Attack", 0.0005, 5.0, ParamScale::Log, "s"),
    spec("Amp Decay", 0.005, 10.0, ParamScale::Log, "s"),
    spec("Amp Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Amp Release", 0.005, 8.0, ParamScale::Log, "s"),
    spec("Filter Attack", 0.001, 5.0, ParamScale::Log, "s"),
    spec("Filter Decay", 0.005, 5.0, ParamScale::Log, "s"),
    spec("Filter Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter Release", 0.005, 8.0, ParamScale::Log, "s"),
    spec("Velocity", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Sampler Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
];

/// Automatable FM Synth parameters (append only).
pub const FM_PARAMS: [ParamSpec; 36] = [
    spec("Feedback", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("LFO Rate", 0.05, 40.0, ParamScale::Log, "Hz"),
    spec("LFO Amount", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pitch Env Amt", -48.0, 48.0, ParamScale::Linear, "st"),
    spec("Pitch Env Decay", 0.005, 5.0, ParamScale::Log, "s"),
    spec("Filter Cutoff", 30.0, 18000.0, ParamScale::Log, "Hz"),
    spec("Filter Res", 0.0, 2.0, ParamScale::Linear, ""),
    spec("Filter Env Amt", -4.0, 6.0, ParamScale::Linear, "oct"),
    spec("Filter Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Filter Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Filter Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter Release", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Glide", 0.0, 2.0, ParamScale::Linear, "s"),
    spec("Spread", 0.0, 50.0, ParamScale::Linear, "ct"),
    spec("Transpose", -48.0, 48.0, ParamScale::Stepped, "st"),
    spec("FM Volume", -40.0, 6.0, ParamScale::Linear, "dB"),
    spec("Op A Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op A Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Op A Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Op A Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op A Release", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Op B Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op B Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Op B Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Op B Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op B Release", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Op C Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op C Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Op C Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Op C Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op C Release", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Op D Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op D Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Op D Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Op D Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Op D Release", 0.005, 20.0, ParamScale::Log, "s"),
];

/// Automatable Wavetable parameters (append only).
pub const WAVETABLE_PARAMS: [ParamSpec; 40] = [
    spec("Osc 1 Position", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Osc 1 Semi", -48.0, 48.0, ParamScale::Stepped, "st"),
    spec("Osc 1 Cents", -100.0, 100.0, ParamScale::Linear, "ct"),
    spec("Osc 1 Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Osc 1 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Osc 1 Uni Detune", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Osc 1 Uni Spread", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Osc 2 Position", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Osc 2 Semi", -48.0, 48.0, ParamScale::Stepped, "st"),
    spec("Osc 2 Cents", -100.0, 100.0, ParamScale::Linear, "ct"),
    spec("Osc 2 Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Osc 2 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Osc 2 Uni Detune", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Osc 2 Uni Spread", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Sub Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Noise Level", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter 1 Cutoff", 20.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Filter 1 Res", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter 1 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter 2 Cutoff", 20.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Filter 2 Res", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Filter 2 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Amp Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Amp Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Amp Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Amp Release", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Env 2 Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Env 2 Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Env 2 Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Env 2 Release", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Env 3 Attack", 0.001, 10.0, ParamScale::Log, "s"),
    spec("Env 3 Decay", 0.005, 20.0, ParamScale::Log, "s"),
    spec("Env 3 Sustain", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Env 3 Release", 0.005, 20.0, ParamScale::Log, "s"),
    spec("LFO 1 Rate", 0.01, 40.0, ParamScale::Log, "Hz"),
    spec("LFO 2 Rate", 0.01, 40.0, ParamScale::Log, "Hz"),
    spec("Macro 1", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Glide", 0.0, 2.0, ParamScale::Linear, "s"),
    spec("Vel Sens", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Wavetable Volume", -40.0, 6.0, ParamScale::Linear, "dB"),
];

/// Automatable Drum Rack parameters: rack volume, then six per pad (append
/// only, and only at the end so pad indices stay put).
pub const DRUM_RACK_PARAMS: [ParamSpec; 97] = [
    spec("Rack Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 1 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 1 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 1 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 1 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 1 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 1 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 2 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 2 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 2 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 2 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 2 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 2 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 3 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 3 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 3 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 3 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 3 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 3 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 4 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 4 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 4 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 4 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 4 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 4 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 5 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 5 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 5 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 5 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 5 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 5 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 6 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 6 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 6 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 6 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 6 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 6 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 7 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 7 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 7 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 7 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 7 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 7 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 8 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 8 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 8 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 8 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 8 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 8 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 9 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 9 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 9 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 9 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 9 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 9 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 10 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 10 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 10 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 10 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 10 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 10 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 11 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 11 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 11 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 11 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 11 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 11 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 12 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 12 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 12 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 12 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 12 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 12 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 13 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 13 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 13 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 13 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 13 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 13 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 14 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 14 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 14 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 14 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 14 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 14 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 15 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 15 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 15 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 15 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 15 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 15 Pan", -1.0, 1.0, ParamScale::Linear, ""),
    spec("Pad 16 Tune", -24.0, 24.0, ParamScale::Linear, "st"),
    spec("Pad 16 Cutoff", 60.0, 20000.0, ParamScale::Log, "Hz"),
    spec("Pad 16 Res", 0.0, 4.0, ParamScale::Linear, ""),
    spec("Pad 16 Drive", 0.0, 1.0, ParamScale::Linear, "%"),
    spec("Pad 16 Volume", -40.0, 12.0, ParamScale::Linear, "dB"),
    spec("Pad 16 Pan", -1.0, 1.0, ParamScale::Linear, ""),
];

/// Upper bound on the number of automatable instrument parameters.
pub const MAX_INSTRUMENT_PARAMS: usize = 128;

/// The automatable parameters of an instrument.
pub fn instrument_params(i: &Instrument) -> &'static [ParamSpec] {
    match i {
        Instrument::None => &[],
        Instrument::Synth(_) => &SYNTH_PARAMS,
        Instrument::DrumKit(_) => &DRUM_PARAMS,
        Instrument::Sampler(_) => &SAMPLER_PARAMS,
        Instrument::DrumRack(_) => &DRUM_RACK_PARAMS,
        Instrument::Fm(_) => &FM_PARAMS,
        Instrument::Wavetable(_) => &WAVETABLE_PARAMS,
        // Plug-in parameters live in the plug-in.
        Instrument::Plugin(_) => &[],
    }
}

/// The field behind instrument parameter `idx` (indexed like
/// [`instrument_params`]).
fn instrument_field(i: &mut Instrument, idx: u16) -> Option<&mut f32> {
    Some(match i {
        Instrument::None | Instrument::Plugin(_) => return None,
        Instrument::Synth(p) => match idx {
            0 => &mut p.osc2_semitones,
            1 => &mut p.osc2_detune,
            2 => &mut p.osc_mix,
            3 => &mut p.cutoff,
            4 => &mut p.resonance,
            5 => &mut p.filter_env,
            6 => &mut p.attack,
            7 => &mut p.decay,
            8 => &mut p.sustain,
            9 => &mut p.release,
            10 => &mut p.f_attack,
            11 => &mut p.f_decay,
            12 => &mut p.f_sustain,
            13 => &mut p.f_release,
            14 => &mut p.volume_db,
            _ => return None,
        },
        Instrument::DrumKit(p) => match idx {
            0 => &mut p.kick_tune,
            1 => &mut p.kick_decay,
            2 => &mut p.snare_tone,
            3 => &mut p.snare_decay,
            4 => &mut p.hat_decay,
            5 => &mut p.volume_db,
            _ => return None,
        },
        Instrument::Sampler(p) => match idx {
            0 => &mut p.start,
            1 => &mut p.end,
            2 => &mut p.transpose,
            3 => &mut p.detune,
            4 => &mut p.cutoff,
            5 => &mut p.resonance,
            6 => &mut p.filter_env,
            7 => &mut p.attack,
            8 => &mut p.decay,
            9 => &mut p.sustain,
            10 => &mut p.release,
            11 => &mut p.f_attack,
            12 => &mut p.f_decay,
            13 => &mut p.f_sustain,
            14 => &mut p.f_release,
            15 => &mut p.velocity,
            16 => &mut p.pan,
            17 => &mut p.volume_db,
            _ => return None,
        },
        Instrument::Fm(p) => match idx {
            0 => &mut p.feedback,
            1 => &mut p.lfo_rate,
            2 => &mut p.lfo_amount,
            3 => &mut p.pitch_env_amount,
            4 => &mut p.pitch_env_decay,
            5 => &mut p.cutoff,
            6 => &mut p.resonance,
            7 => &mut p.filter_env,
            8 => &mut p.f_attack,
            9 => &mut p.f_decay,
            10 => &mut p.f_sustain,
            11 => &mut p.f_release,
            12 => &mut p.glide,
            13 => &mut p.spread,
            14 => &mut p.transpose,
            15 => &mut p.volume_db,
            16..=35 => {
                let op = &mut p.ops[(idx as usize - 16) / 5];
                match (idx - 16) % 5 {
                    0 => &mut op.level,
                    1 => &mut op.attack,
                    2 => &mut op.decay,
                    3 => &mut op.sustain,
                    _ => &mut op.release,
                }
            }
            _ => return None,
        },
        Instrument::Wavetable(p) => match idx {
            0 => &mut p.osc1.position,
            1 => &mut p.osc1.semitones,
            2 => &mut p.osc1.cents,
            3 => &mut p.osc1.level,
            4 => &mut p.osc1.pan,
            5 => &mut p.osc1.unison_detune,
            6 => &mut p.osc1.unison_spread,
            7 => &mut p.osc2.position,
            8 => &mut p.osc2.semitones,
            9 => &mut p.osc2.cents,
            10 => &mut p.osc2.level,
            11 => &mut p.osc2.pan,
            12 => &mut p.osc2.unison_detune,
            13 => &mut p.osc2.unison_spread,
            14 => &mut p.sub_level,
            15 => &mut p.noise_level,
            16 => &mut p.filter1.cutoff,
            17 => &mut p.filter1.resonance,
            18 => &mut p.filter1.drive,
            19 => &mut p.filter2.cutoff,
            20 => &mut p.filter2.resonance,
            21 => &mut p.filter2.drive,
            22 => &mut p.amp_env.attack,
            23 => &mut p.amp_env.decay,
            24 => &mut p.amp_env.sustain,
            25 => &mut p.amp_env.release,
            26 => &mut p.env2.attack,
            27 => &mut p.env2.decay,
            28 => &mut p.env2.sustain,
            29 => &mut p.env2.release,
            30 => &mut p.env3.attack,
            31 => &mut p.env3.decay,
            32 => &mut p.env3.sustain,
            33 => &mut p.env3.release,
            34 => &mut p.lfo1.rate,
            35 => &mut p.lfo2.rate,
            36 => &mut p.macro1,
            37 => &mut p.glide,
            38 => &mut p.velocity_sens,
            39 => &mut p.volume_db,
            _ => return None,
        },
        Instrument::DrumRack(p) => match idx {
            0 => &mut p.volume_db,
            1..=96 => {
                let pad = &mut p.pads[(idx as usize - 1) / 6];
                match (idx - 1) % 6 {
                    0 => &mut pad.tune,
                    1 => &mut pad.cutoff,
                    2 => &mut pad.resonance,
                    3 => &mut pad.drive,
                    4 => &mut pad.volume_db,
                    _ => &mut pad.pan,
                }
            }
            _ => return None,
        },
    })
}

/// Reads instrument parameter `idx` in its natural units.
pub fn get_instrument_param(i: &Instrument, idx: u16) -> Option<f32> {
    let mut copy = *i;
    instrument_field(&mut copy, idx).map(|v| *v)
}

/// Writes instrument parameter `idx` in its natural units.
pub fn set_instrument_param(i: &mut Instrument, idx: u16, v: f32) {
    if let Some(f) = instrument_field(i, idx) {
        *f = v;
    }
}

const PLUGIN_PARAM_SPEC: ParamSpec = spec("Plug-in Parameter", 0.0, 1.0, ParamScale::Linear, "");

/// The value range of a target, if known for this track's instrument.
pub fn target_spec(target: AutomationTarget, instrument: &Instrument) -> Option<ParamSpec> {
    match target {
        AutomationTarget::Volume => Some(VOLUME_SPEC),
        AutomationTarget::Pan => Some(PAN_SPEC),
        AutomationTarget::Send(_) => Some(SEND_SPEC),
        AutomationTarget::Instrument(i) => instrument_params(instrument).get(i as usize).copied(),
        // Plug-in parameters are already normalised.
        AutomationTarget::Device { .. } => Some(PLUGIN_PARAM_SPEC),
    }
}

/// Display name of a target.
pub fn target_name(target: AutomationTarget, instrument: &Instrument) -> String {
    match target {
        AutomationTarget::Send(n) => format!("Send {}", (b'A' + n.min(25)) as char),
        AutomationTarget::Device { param, .. } => format!("Device Param {}", param + 1),
        t => target_spec(t, instrument)
            .map(|s| s.name.to_string())
            .unwrap_or_else(|| "Unknown".into()),
    }
}

/// Every target the UI can offer for a track.
pub fn available_targets(instrument: &Instrument) -> Vec<AutomationTarget> {
    let mut v = vec![AutomationTarget::Volume, AutomationTarget::Pan];
    v.extend((0..instrument_params(instrument).len() as u16).map(AutomationTarget::Instrument));
    v
}

/// One breakpoint.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AutomationPoint {
    /// Position in beats (song beats for track lanes, clip beats for clip
    /// envelopes).
    pub beat: f64,
    /// Normalised value 0..1.
    pub value: f32,
    /// Shape of the segment from this point to the next: 0 is a straight
    /// line, positive values bend towards a slow start (exponential-like),
    /// negative values towards a fast start. Range -1..1.
    #[serde(default)]
    pub curve: f32,
}

/// Shapes a 0..1 segment position with a curve amount in -1..1.
#[inline]
pub fn shape(t: f32, curve: f32) -> f32 {
    if curve.abs() < 1e-4 {
        return t;
    }
    let e = 1.0 + 6.0 * curve.abs();
    if curve > 0.0 {
        t.powf(e)
    } else {
        1.0 - (1.0 - t).powf(e)
    }
}

/// A breakpoint envelope, points sorted by beat.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub points: Vec<AutomationPoint>,
}

impl Envelope {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Index of the last point at or before `beat`, or `None` if `beat` lies
    /// before the first point.
    fn index_before(&self, beat: f64) -> Option<usize> {
        let n = self.points.partition_point(|p| p.beat <= beat);
        n.checked_sub(1)
    }

    /// Value at `beat`; `None` for an empty envelope. Before the first point
    /// and after the last the envelope holds that point's value.
    pub fn value_at(&self, beat: f64) -> Option<f32> {
        let first = self.points.first()?;
        Some(match self.index_before(beat) {
            None => first.value,
            Some(i) => self.segment_value(i, beat),
        })
    }

    #[inline]
    fn segment_value(&self, i: usize, beat: f64) -> f32 {
        let a = self.points[i];
        match self.points.get(i + 1) {
            None => a.value,
            Some(b) => {
                let span = b.beat - a.beat;
                if span <= 1e-12 {
                    return b.value;
                }
                let t = (((beat - a.beat) / span) as f32).clamp(0.0, 1.0);
                a.value + (b.value - a.value) * shape(t, a.curve)
            }
        }
    }

    /// Value at `beat` using `hint` (the segment index from the previous
    /// call) to avoid a search when positions advance steadily, as they do
    /// when rendering sample by sample. Allocation-free.
    #[inline]
    pub fn value_with_hint(&self, beat: f64, hint: &mut usize) -> Option<f32> {
        let n = self.points.len();
        if n == 0 {
            return None;
        }
        let inside = |i: usize| self.points[i].beat <= beat && self.points.get(i + 1).is_none_or(|p| p.beat > beat);
        let mut i = (*hint).min(n - 1);
        if !inside(i) {
            // Try the next segment before falling back to a binary search.
            if i + 1 < n && inside(i + 1) {
                i += 1;
            } else {
                match self.index_before(beat) {
                    None => {
                        *hint = 0;
                        return Some(self.points[0].value);
                    }
                    Some(j) => i = j,
                }
            }
        }
        *hint = i;
        Some(self.segment_value(i, beat))
    }

    /// Inserts a point, replacing any point at (almost) the same position.
    /// Returns its index.
    pub fn insert(&mut self, beat: f64, value: f32) -> usize {
        let value = value.clamp(0.0, 1.0);
        if let Some(i) = self.points.iter().position(|p| (p.beat - beat).abs() < 1e-9) {
            self.points[i].value = value;
            return i;
        }
        let i = self.points.partition_point(|p| p.beat < beat);
        self.points.insert(
            i,
            AutomationPoint {
                beat,
                value,
                curve: 0.0,
            },
        );
        i
    }

    pub fn remove(&mut self, index: usize) {
        if index < self.points.len() {
            self.points.remove(index);
        }
    }

    /// Moves a point, keeping it between its neighbours so the order never
    /// changes during a drag.
    pub fn move_point(&mut self, index: usize, beat: f64, value: f32) {
        if index >= self.points.len() {
            return;
        }
        let lo = if index > 0 { self.points[index - 1].beat } else { 0.0 };
        let hi = self.points.get(index + 1).map(|p| p.beat).unwrap_or(f64::INFINITY);
        let p = &mut self.points[index];
        p.beat = beat.clamp(lo, hi.max(lo));
        p.value = value.clamp(0.0, 1.0);
    }

    /// Removes the points strictly between `from` and `to` (used while
    /// recording, so new values overwrite what was there).
    pub fn clear_between(&mut self, from: f64, to: f64) {
        let (a, b) = if from <= to { (from, to) } else { (to, from) };
        self.points.retain(|p| p.beat <= a || p.beat >= b);
    }

    /// Records `value` at `beat`, overwriting points since `last` (the
    /// previous write position of the same gesture, if any).
    pub fn record(&mut self, last: Option<f64>, beat: f64, value: f32) {
        if let Some(l) = last.filter(|l| *l < beat) {
            self.clear_between(l, beat + 1e-9);
        }
        self.insert(beat, value);
    }
}

/// An envelope for one parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AutomationLane {
    pub target: AutomationTarget,
    pub envelope: Envelope,
    /// Set when the user moved the parameter by hand: the lane is ignored
    /// until automation is re-enabled. Not saved.
    #[serde(skip)]
    pub overridden: bool,
}

impl AutomationLane {
    pub fn new(target: AutomationTarget) -> Self {
        Self {
            target,
            envelope: Envelope::default(),
            overridden: false,
        }
    }

    /// True if the engine should follow this lane.
    pub fn is_active(&self) -> bool {
        !self.overridden && !self.envelope.is_empty()
    }
}

/// Finds the lane for `target`.
pub fn lane(lanes: &[AutomationLane], target: AutomationTarget) -> Option<&AutomationLane> {
    lanes.iter().find(|l| l.target == target)
}

/// Finds or creates the lane for `target`.
pub fn lane_mut(lanes: &mut Vec<AutomationLane>, target: AutomationTarget) -> &mut AutomationLane {
    match lanes.iter().position(|l| l.target == target) {
        Some(i) => &mut lanes[i],
        None => {
            lanes.push(AutomationLane::new(target));
            lanes.last_mut().expect("just pushed")
        }
    }
}

// ----- audio thread ---------------------------------------------------------------

/// Instrument parameters are updated at this rate (in frames) while
/// automated; mixer gains are evaluated every sample.
pub const CONTROL_FRAMES: usize = 32;

/// A clip envelope compiled for the engine: `slot` indexes the track's
/// automation buffers.
#[derive(Debug, Clone)]
pub struct ClipEnvelope {
    pub slot: usize,
    pub envelope: Envelope,
}

/// Per-track automation state for the audio thread: the compiled lanes and
/// one buffer of evaluated (normalised) values per automated target for the
/// current block. Built on the UI thread; the audio thread only writes into
/// the preallocated buffers.
#[derive(Debug, Default)]
pub struct TrackAutomation {
    targets: Vec<AutomationTarget>,
    /// Active Arrangement lanes: (slot, envelope, search hint).
    lanes: Vec<(usize, Envelope, usize)>,
    /// `targets.len() * MAX_BLOCK` values, NaN where nothing is automated.
    values: Vec<f32>,
    /// Whether a slot received any value in the current block.
    written: Vec<bool>,
    /// Value of each slot at the end of the last block (NaN if none).
    last: Vec<f32>,
    instrument_slots: Vec<(usize, u16)>,
    /// Device parameter targets: (slot, device, param).
    device_slots: Vec<(usize, Id, u16)>,
    volume: Option<usize>,
    pan: Option<usize>,
}

impl TrackAutomation {
    /// Collects every target the track automates (active lanes and clip
    /// envelopes). Runs on the UI thread.
    pub fn new(track: &crate::model::Track) -> Self {
        let mut targets: Vec<AutomationTarget> = Vec::new();
        let mut add = |t: AutomationTarget| {
            if !targets.contains(&t) {
                targets.push(t);
            }
        };
        for l in track.automation.iter().filter(|l| l.is_active()) {
            add(l.target);
        }
        let clips = track
            .slots
            .iter()
            .flatten()
            .chain(track.arrangement.iter().map(|a| &a.clip));
        for c in clips {
            for l in c.envelopes.iter().filter(|l| l.is_active()) {
                add(l.target);
            }
        }
        let slot = |t: AutomationTarget| targets.iter().position(|x| *x == t);
        let lanes = track
            .automation
            .iter()
            .filter(|l| l.is_active())
            .filter_map(|l| Some((slot(l.target)?, l.envelope.clone(), 0)))
            .collect();
        let instrument_slots = targets
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match t {
                AutomationTarget::Instrument(p) if (*p as usize) < MAX_INSTRUMENT_PARAMS => Some((i, *p)),
                _ => None,
            })
            .collect();
        let device_slots = targets
            .iter()
            .enumerate()
            .filter_map(|(i, t)| match *t {
                AutomationTarget::Device { device, param } => Some((i, device, param)),
                _ => None,
            })
            .collect();
        let n = targets.len();
        TrackAutomation {
            device_slots,
            volume: slot(AutomationTarget::Volume),
            pan: slot(AutomationTarget::Pan),
            lanes,
            values: vec![f32::NAN; n * crate::MAX_BLOCK],
            written: vec![false; n],
            last: vec![f32::NAN; n],
            instrument_slots,
            targets,
        }
    }

    /// Compiles a clip's envelopes against this track's slots.
    pub fn compile_clip(&self, clip: &crate::model::Clip) -> Vec<ClipEnvelope> {
        clip.envelopes
            .iter()
            .filter(|l| l.is_active())
            .filter_map(|l| {
                Some(ClipEnvelope {
                    slot: self.targets.iter().position(|t| *t == l.target)?,
                    envelope: l.envelope.clone(),
                })
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Starts a block of `frames` frames.
    pub fn begin(&mut self, frames: usize) {
        for (s, w) in self.written.iter_mut().enumerate() {
            if *w {
                let base = s * crate::MAX_BLOCK;
                self.values[base..base + frames].fill(f32::NAN);
                *w = false;
            }
        }
    }

    /// Writes clip envelopes for frames `f0..f1`, where `local0` is the clip
    /// beat at `f0` and `bpf` beats per frame.
    #[allow(clippy::too_many_arguments)]
    pub fn write_clip(
        &mut self,
        envs: &[ClipEnvelope],
        f0: usize,
        f1: usize,
        local0: f64,
        bpf: f64,
        length: f64,
        looping: bool,
    ) {
        for e in envs {
            let base = e.slot * crate::MAX_BLOCK;
            let mut hint = 0;
            for f in f0..f1.min(crate::MAX_BLOCK) {
                let l = local0 + (f - f0) as f64 * bpf;
                if l < 0.0 {
                    continue;
                }
                let pos = if looping { l.rem_euclid(length) } else { l };
                if pos >= length {
                    break;
                }
                if let Some(v) = e.envelope.value_with_hint(pos, &mut hint) {
                    self.values[base + f] = v;
                    self.written[e.slot] = true;
                }
            }
        }
    }

    /// Writes the Arrangement lanes for a block starting at song beat
    /// `beat0` (`bpf` is 0 while stopped, holding the value at the playhead).
    pub fn write_lanes(&mut self, beat0: f64, bpf: f64, frames: usize) {
        for (slot, env, hint) in self.lanes.iter_mut() {
            let base = *slot * crate::MAX_BLOCK;
            for f in 0..frames {
                if let Some(v) = env.value_with_hint(beat0 + f as f64 * bpf, hint) {
                    self.values[base + f] = v;
                }
            }
            self.written[*slot] = true;
        }
    }

    /// Ends the block, remembering the final value of each slot.
    pub fn end(&mut self, frames: usize) {
        for s in 0..self.targets.len() {
            self.last[s] = if self.written[s] && frames > 0 {
                self.values[s * crate::MAX_BLOCK + frames - 1]
            } else {
                f32::NAN
            };
        }
    }

    /// Normalised value of `target` at `frame` of the current block, if
    /// automated there. For the send and device mixers.
    #[inline]
    pub fn value(&self, target: AutomationTarget, frame: usize) -> Option<f32> {
        let s = self.targets.iter().position(|t| *t == target)?;
        self.slot_value(s, frame)
    }

    #[inline]
    fn slot_value(&self, slot: usize, frame: usize) -> Option<f32> {
        if !self.written[slot] {
            return None;
        }
        let v = self.values[slot * crate::MAX_BLOCK + frame];
        (!v.is_nan()).then_some(v)
    }

    /// Normalised value of `target` at the end of the last block.
    pub fn last_value(&self, target: AutomationTarget) -> Option<f32> {
        let s = self.targets.iter().position(|t| *t == target)?;
        let v = self.last[s];
        (!v.is_nan()).then_some(v)
    }

    /// True if volume or pan is automated somewhere in the current block.
    pub fn mixer_automated(&self) -> bool {
        self.volume.is_some_and(|s| self.written[s]) || self.pan.is_some_and(|s| self.written[s])
    }

    /// Volume (dB) and pan at `frame`, falling back to the given values.
    #[inline]
    pub fn mixer_at(&self, frame: usize, volume_db: f32, pan: f32) -> (f32, f32) {
        let v = self
            .volume
            .and_then(|s| self.slot_value(s, frame))
            .map(|n| VOLUME_SPEC.denormalize(n))
            .unwrap_or(volume_db);
        let p = self
            .pan
            .and_then(|s| self.slot_value(s, frame))
            .map(|n| PAN_SPEC.denormalize(n))
            .unwrap_or(pan);
        (v, p)
    }

    /// True if any instrument parameter is automated in the current block.
    pub fn instrument_automated(&self) -> bool {
        self.instrument_slots.iter().any(|(s, _)| self.written[*s])
    }

    /// The instrument with automated parameters applied at `frame`.
    #[inline]
    /// Calls `f(device, param, value)` for every automated device parameter
    /// with a value at `frame` this block. Audio thread; no allocation.
    pub fn for_each_device(&self, frame: usize, mut f: impl FnMut(Id, u16, f32)) {
        for &(s, device, param) in &self.device_slots {
            if let Some(v) = self.slot_value(s, frame) {
                f(device, param, v);
            }
        }
    }

    pub fn instrument_at(&self, base: &Instrument, frame: usize) -> Instrument {
        let mut i = *base;
        let specs = instrument_params(base);
        for &(s, p) in &self.instrument_slots {
            if let (Some(v), Some(spec)) = (self.slot_value(s, frame), specs.get(p as usize)) {
                set_instrument_param(&mut i, p, spec.denormalize(v));
            }
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth::SynthParams;

    fn env(points: &[(f64, f32)]) -> Envelope {
        let mut e = Envelope::default();
        for &(b, v) in points {
            e.insert(b, v);
        }
        e
    }

    #[test]
    fn envelope_interpolates_and_holds() {
        let e = env(&[(1.0, 0.0), (3.0, 1.0)]);
        assert_eq!(e.value_at(0.0), Some(0.0), "holds before the first point");
        assert_eq!(e.value_at(2.0), Some(0.5));
        assert_eq!(e.value_at(10.0), Some(1.0), "holds after the last point");
        assert_eq!(Envelope::default().value_at(1.0), None);
    }

    #[test]
    fn hinted_lookup_matches_search() {
        let e = env(&[(0.0, 0.2), (1.0, 0.9), (1.5, 0.1), (4.0, 0.6), (4.0001, 0.0)]);
        let mut hint = 0;
        let mut b = -0.5;
        while b < 6.0 {
            let a = e.value_with_hint(b, &mut hint).unwrap();
            let s = e.value_at(b).unwrap();
            assert!((a - s).abs() < 1e-6, "beat {b}: {a} vs {s}");
            b += 0.013;
        }
        // Jumping backwards (loops) still works.
        let a = e.value_with_hint(0.5, &mut hint).unwrap();
        assert!((a - e.value_at(0.5).unwrap()).abs() < 1e-6);
    }

    #[test]
    fn curves_bend_but_keep_endpoints() {
        for c in [-1.0, -0.4, 0.0, 0.5, 1.0] {
            assert_eq!(shape(0.0, c), 0.0);
            assert!((shape(1.0, c) - 1.0).abs() < 1e-6);
        }
        assert!(shape(0.5, 0.8) < 0.5);
        assert!(shape(0.5, -0.8) > 0.5);
        let mut e = env(&[(0.0, 0.0), (1.0, 1.0)]);
        e.points[0].curve = 0.7;
        assert!(e.value_at(0.5).unwrap() < 0.5);
    }

    #[test]
    fn points_stay_sorted() {
        let mut e = env(&[(2.0, 0.5), (0.0, 0.1), (1.0, 0.3)]);
        assert!(e.points.windows(2).all(|w| w[0].beat < w[1].beat));
        assert_eq!(e.insert(1.0, 0.9), 1, "same position replaces");
        assert_eq!(e.points.len(), 3);
        e.move_point(1, 5.0, 2.0);
        assert_eq!(e.points[1].beat, 2.0, "clamped to the next point");
        assert_eq!(e.points[1].value, 1.0);
    }

    #[test]
    fn recording_overwrites_the_touched_range() {
        let mut e = env(&[(0.0, 0.0), (1.0, 1.0), (2.0, 0.0), (3.0, 1.0)]);
        e.record(None, 0.5, 0.5);
        e.record(Some(0.5), 2.5, 0.5);
        let beats: Vec<f64> = e.points.iter().map(|p| p.beat).collect();
        assert_eq!(beats, vec![0.0, 0.5, 2.5, 3.0]);
    }

    #[test]
    fn param_specs_round_trip() {
        for s in SYNTH_PARAMS.iter().chain(DRUM_PARAMS.iter()) {
            for n in [0.0, 0.25, 0.5, 1.0] {
                let v = s.denormalize(n);
                assert!(v >= s.min - 1e-3 && v <= s.max + 1e-3, "{} {v}", s.name);
                if s.scale != ParamScale::Stepped {
                    assert!((s.normalize(v) - n).abs() < 1e-3, "{} {n}", s.name);
                }
            }
        }
        assert!((VOLUME_SPEC.denormalize(VOLUME_SPEC.normalize(0.0))).abs() < 1e-3);
        assert_eq!(VOLUME_SPEC.denormalize(0.0), -70.0);
        assert!((SEND_SPEC.denormalize(1.0)).abs() < 1e-3);
    }

    #[test]
    fn instrument_params_get_and_set() {
        use crate::dsp::{
            drum_rack::DrumRackParams, drums::DrumKitParams, fm::FmParams, sampler::SamplerParams,
            wavetable::WavetableParams,
        };
        let all = [
            Instrument::Synth(SynthParams::default()),
            Instrument::DrumKit(DrumKitParams::default()),
            Instrument::Sampler(SamplerParams::default()),
            Instrument::DrumRack(DrumRackParams::default()),
            Instrument::Fm(FmParams::default()),
            Instrument::Wavetable(WavetableParams::default()),
        ];
        for base in all {
            let specs = instrument_params(&base);
            assert!(specs.len() <= MAX_INSTRUMENT_PARAMS);
            // Defaults sit inside the automation range.
            for (idx, spec) in specs.iter().enumerate() {
                let v = get_instrument_param(&base, idx as u16).unwrap();
                assert!(
                    v >= spec.min - 1e-6 && v <= spec.max + 1e-6,
                    "{} default {v} outside {}..{}",
                    spec.name,
                    spec.min,
                    spec.max
                );
            }
            // Every index addresses its own field.
            let mut i = base;
            for idx in 0..specs.len() as u16 {
                set_instrument_param(&mut i, idx, 0.123 + idx as f32);
            }
            for idx in 0..specs.len() as u16 {
                assert_eq!(
                    get_instrument_param(&i, idx),
                    Some(0.123 + idx as f32),
                    "{}",
                    specs[idx as usize].name
                );
            }
            assert_eq!(get_instrument_param(&i, specs.len() as u16), None);
        }
        assert_eq!(get_instrument_param(&Instrument::None, 0), None);
    }

    #[test]
    fn lanes_round_trip_through_json() {
        let mut lanes = Vec::new();
        lane_mut(&mut lanes, AutomationTarget::Instrument(3))
            .envelope
            .insert(2.0, 0.25);
        lane_mut(&mut lanes, AutomationTarget::Volume).overridden = true;
        let json = serde_json::to_string(&lanes).unwrap();
        let back: Vec<AutomationLane> = serde_json::from_str(&json).unwrap();
        assert_eq!(back[0].target, AutomationTarget::Instrument(3));
        assert_eq!(back[0].envelope.points[0].value, 0.25);
        assert!(!back[1].overridden, "override state is not saved");
    }
}
