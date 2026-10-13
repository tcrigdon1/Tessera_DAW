//! MIDI controller mappings: a knob, fader or pad on a hardware controller
//! bound to a parameter or action. Saved with the project; applied by the UI.

use serde::{Deserialize, Serialize};

use crate::model::Id;

/// The controller message a mapping listens to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MapSource {
    /// A control change; `channel` is 0 ..= 15.
    Cc { channel: u8, cc: u8 },
    /// A note (pads and buttons); note-on is a press.
    Note { channel: u8, note: u8 },
}

impl MapSource {
    pub fn label(&self) -> String {
        match *self {
            MapSource::Cc { channel, cc } => format!("Ch {} CC {}", channel + 1, cc),
            MapSource::Note { channel, note } => {
                format!("Ch {} Note {}", channel + 1, super::note_name(note))
            }
        }
    }
}

/// What a mapping controls. Buses are tracks, return tracks or the master
/// ([`crate::model::MASTER_BUS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MapTarget {
    Volume(Id),
    Pan(Id),
    Send {
        track: Id,
        index: usize,
    },
    Mute(Id),
    Solo(Id),
    Arm(Id),
    /// An audio effect parameter.
    Device {
        bus: Id,
        device: Id,
        index: usize,
    },
    /// An instrument parameter (indexed like
    /// [`crate::automation::instrument_params`]).
    Instrument {
        track: Id,
        index: u16,
    },
    /// A MIDI effect parameter.
    MidiDevice {
        track: Id,
        device: Id,
        index: usize,
    },
    Tempo,
    LaunchClip {
        track: Id,
        scene: usize,
    },
    LaunchScene(usize),
    Play,
    Stop,
}

impl MapTarget {
    /// Toggles and triggers react to presses rather than follow a value.
    pub fn is_button(&self) -> bool {
        matches!(
            self,
            MapTarget::Mute(_)
                | MapTarget::Solo(_)
                | MapTarget::Arm(_)
                | MapTarget::LaunchClip { .. }
                | MapTarget::LaunchScene(_)
                | MapTarget::Play
                | MapTarget::Stop
        )
    }

    /// True if the mapping refers to the track, return or device `id`.
    pub fn refers_to(&self, id: Id) -> bool {
        match *self {
            MapTarget::Volume(b) | MapTarget::Pan(b) | MapTarget::Mute(b) | MapTarget::Solo(b) | MapTarget::Arm(b) => {
                b == id
            }
            MapTarget::Send { track, .. }
            | MapTarget::Instrument { track, .. }
            | MapTarget::LaunchClip { track, .. } => track == id,
            MapTarget::Device { bus, device, .. } => bus == id || device == id,
            MapTarget::MidiDevice { track, device, .. } => track == id || device == id,
            MapTarget::Tempo | MapTarget::LaunchScene(_) | MapTarget::Play | MapTarget::Stop => false,
        }
    }
}

/// One controller mapping. `min` and `max` are normalised (0 ..= 1) bounds
/// of the target's range that the controller sweeps.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MidiMapping {
    pub source: MapSource,
    pub target: MapTarget,
    #[serde(default)]
    pub min: f32,
    #[serde(default = "one")]
    pub max: f32,
}

fn one() -> f32 {
    1.0
}

impl MidiMapping {
    pub fn new(source: MapSource, target: MapTarget) -> Self {
        Self {
            source,
            target,
            min: 0.0,
            max: 1.0,
        }
    }

    /// Normalised target value for a 7-bit controller value.
    pub fn scale(&self, value: u8) -> f32 {
        let t = value.min(127) as f32 / 127.0;
        self.min + (self.max - self.min) * t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mappings_scale_and_serialize() {
        let mut m = MidiMapping::new(MapSource::Cc { channel: 0, cc: 21 }, MapTarget::Volume(3));
        assert_eq!(m.scale(0), 0.0);
        assert_eq!(m.scale(127), 1.0);
        m.min = 0.5;
        assert!((m.scale(127) - 1.0).abs() < 1e-6 && (m.scale(0) - 0.5).abs() < 1e-6);
        let json = serde_json::to_string(&m).unwrap();
        let back: MidiMapping = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
        assert!(MapTarget::Mute(1).is_button() && !MapTarget::Tempo.is_button());
        assert!(MapTarget::Device {
            bus: 1,
            device: 9,
            index: 0
        }
        .refers_to(9));
        assert_eq!(MapSource::Cc { channel: 0, cc: 21 }.label(), "Ch 1 CC 21");
    }
}
