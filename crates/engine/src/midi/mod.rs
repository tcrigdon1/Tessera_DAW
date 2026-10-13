//! MIDI: hardware input messages, musical scales, note editing operations
//! and controller mappings.
//!
//! MIDI effects (Arpeggiator, Chord, ...) live in [`crate::midi_fx`].

pub mod edit;
pub mod mapping;
pub mod scale;

/// A short (channel voice) MIDI message from a hardware input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MidiMessage {
    /// Which input port it came from ([`port_id`] of the port's name).
    pub port: u64,
    pub status: u8,
    pub data1: u8,
    pub data2: u8,
}

/// What a [`MidiMessage`] means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidiKind {
    NoteOn {
        pitch: u8,
        velocity: u8,
    },
    NoteOff {
        pitch: u8,
    },
    Cc {
        cc: u8,
        value: u8,
    },
    /// -8192 ..= 8191.
    PitchBend(i16),
    Other,
}

impl MidiMessage {
    /// Parses raw bytes from a MIDI port. System messages (SysEx, clock,
    /// active sensing) are ignored.
    pub fn from_bytes(port: u64, bytes: &[u8]) -> Option<Self> {
        let status = *bytes.first()?;
        if !(0x80..0xf0).contains(&status) {
            return None;
        }
        let data = |i: usize| bytes.get(i).copied().unwrap_or(0) & 0x7f;
        Some(Self {
            port,
            status,
            data1: data(1),
            data2: data(2),
        })
    }

    /// MIDI channel, 0 ..= 15.
    pub fn channel(&self) -> u8 {
        self.status & 0x0f
    }

    pub fn kind(&self) -> MidiKind {
        match self.status & 0xf0 {
            0x90 if self.data2 > 0 => MidiKind::NoteOn {
                pitch: self.data1,
                velocity: self.data2,
            },
            0x80 | 0x90 => MidiKind::NoteOff { pitch: self.data1 },
            0xb0 => MidiKind::Cc {
                cc: self.data1,
                value: self.data2,
            },
            0xe0 => MidiKind::PitchBend(((self.data2 as i16) << 7 | self.data1 as i16) - 8192),
            _ => MidiKind::Other,
        }
    }

    pub fn note_on(port: u64, channel: u8, pitch: u8, velocity: u8) -> Self {
        Self {
            port,
            status: 0x90 | (channel & 0x0f),
            data1: pitch & 0x7f,
            data2: velocity & 0x7f,
        }
    }

    pub fn note_off(port: u64, channel: u8, pitch: u8) -> Self {
        Self {
            port,
            status: 0x80 | (channel & 0x0f),
            data1: pitch & 0x7f,
            data2: 0,
        }
    }

    pub fn cc(port: u64, channel: u8, cc: u8, value: u8) -> Self {
        Self {
            port,
            status: 0xb0 | (channel & 0x0f),
            data1: cc & 0x7f,
            data2: value & 0x7f,
        }
    }

    /// Pitch wheel, `value` in -8192 ..= 8191.
    pub fn pitch_bend(port: u64, channel: u8, value: i16) -> Self {
        let v = (value.clamp(-8192, 8191) + 8192) as u16;
        Self {
            port,
            status: 0xe0 | (channel & 0x0f),
            data1: (v & 0x7f) as u8,
            data2: (v >> 7) as u8,
        }
    }
}

/// A stable, non-zero id for an input port name (FNV-1a), so the audio
/// thread can filter messages by port without strings.
pub fn port_id(name: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in name.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h.max(1)
}

/// Name of a MIDI note, `C3` = 60 (Ableton's convention).
pub fn note_name(pitch: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[pitch as usize % 12], pitch as i32 / 12 - 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_channel_messages() {
        let m = MidiMessage::from_bytes(1, &[0x93, 60, 100]).unwrap();
        assert_eq!(m.channel(), 3);
        assert_eq!(
            m.kind(),
            MidiKind::NoteOn {
                pitch: 60,
                velocity: 100
            }
        );
        let off = MidiMessage::from_bytes(1, &[0x90, 60, 0]).unwrap();
        assert_eq!(off.kind(), MidiKind::NoteOff { pitch: 60 });
        let cc = MidiMessage::from_bytes(1, &[0xb0, 7, 127]).unwrap();
        assert_eq!(cc.kind(), MidiKind::Cc { cc: 7, value: 127 });
        let bend = MidiMessage::from_bytes(1, &[0xe0, 0, 0x40]).unwrap();
        assert_eq!(bend.kind(), MidiKind::PitchBend(0));
        assert!(MidiMessage::from_bytes(1, &[0xf8]).is_none());
        assert!(MidiMessage::from_bytes(1, &[]).is_none());
    }

    #[test]
    fn port_ids_are_stable_and_non_zero() {
        assert_eq!(port_id("Keystation"), port_id("Keystation"));
        assert_ne!(port_id("A"), port_id("B"));
        assert_ne!(port_id(""), 0);
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(61), "C#3");
    }
}
