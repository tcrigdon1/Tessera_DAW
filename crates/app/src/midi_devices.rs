//! Editing a track's MIDI effects and MIDI input routing.

use tessera_engine::midi::mapping::MapTarget;
use tessera_engine::midi_fx::{MidiDevice, MidiDeviceKind, MidiDeviceParams};
use tessera_engine::model::{Id, MidiInput, TrackKind};
use tessera_engine::Command;

use crate::app::{DetailMode, TesseraApp};

impl TesseraApp {
    /// Adds a MIDI effect at the end of a MIDI track's MIDI chain.
    pub fn add_midi_device(&mut self, track: Id, kind: MidiDeviceKind) {
        let (id, chain) = (self.project.alloc_id(), self.project.alloc_id());
        let Some(t) = self.project.track_mut(track) else { return };
        if t.kind != TrackKind::Midi {
            self.status = "MIDI effects go on MIDI tracks".into();
            return;
        }
        if kind == MidiDeviceKind::Rack {
            if t.midi_rack.is_some() {
                self.status = "A track has one MIDI Effect Rack".into();
                return;
            }
            t.midi_rack = Some(tessera_engine::midi_fx::rack::wrap(chain, Vec::new()));
        }
        t.midi_devices.push(MidiDevice::new(id, kind));
        let name = t.name.clone();
        self.selected_track = Some(track);
        self.detail = DetailMode::Device;
        self.status = format!("Added {} to {name}", kind.name());
        self.changed();
    }

    /// The track's MIDI Effect Rack device: its id and macro settings.
    pub fn midi_rack_device(&self, track: Id) -> Option<(Id, tessera_engine::fx::rack::RackParams)> {
        self.project
            .track(track)?
            .midi_devices
            .iter()
            .find_map(|d| match d.params {
                MidiDeviceParams::Rack(p) => Some((d.id, p)),
                _ => None,
            })
    }

    /// Adds a MIDI effect to a chain of the track's MIDI Effect Rack.
    pub fn add_midi_device_to_chain(&mut self, track: Id, chain: Id, kind: MidiDeviceKind) {
        if kind == MidiDeviceKind::Rack {
            return;
        }
        let id = self.project.alloc_id();
        let Some(c) = self
            .project
            .track_mut(track)
            .and_then(|t| t.midi_rack.as_mut())
            .and_then(|r| r.chain_mut(chain))
        else {
            return;
        };
        c.midi_devices.push(MidiDevice::new(id, kind));
        self.status = format!("Added {}", kind.name());
        self.changed();
    }

    /// The device list holding a MIDI effect: the track's or a rack chain's.
    fn midi_list_mut(&mut self, track: Id, device: Id) -> Option<&mut Vec<MidiDevice>> {
        let t = self.project.track_mut(track)?;
        if t.midi_devices.iter().any(|d| d.id == device) {
            return Some(&mut t.midi_devices);
        }
        t.midi_rack
            .as_mut()?
            .chains
            .iter_mut()
            .map(|c| &mut c.midi_devices)
            .find(|l| l.iter().any(|d| d.id == device))
    }

    pub fn remove_midi_device(&mut self, track: Id, device: Id) {
        let is_rack = self.midi_rack_device(track).is_some_and(|(d, _)| d == device);
        if let Some(list) = self.midi_list_mut(track, device) {
            list.retain(|d| d.id != device);
        }
        if let Some(t) = self.project.track_mut(track) {
            if is_rack {
                t.midi_rack = None;
            }
            if let Some(r) = t.midi_rack.as_mut() {
                r.prune_mappings();
            }
            self.prune_mappings();
            self.changed();
        }
    }

    pub fn move_midi_device(&mut self, track: Id, device: Id, delta: i32) {
        let Some(list) = self.midi_list_mut(track, device) else {
            return;
        };
        let Some(i) = list.iter().position(|d| d.id == device) else {
            return;
        };
        let j = i as i32 + delta;
        if j < 0 || j as usize >= list.len() {
            return;
        }
        list.swap(i, j as usize);
        self.changed();
    }

    fn midi_device_mut(&mut self, track: Id, device: Id) -> Option<&mut MidiDevice> {
        self.midi_list_mut(track, device)?.iter_mut().find(|d| d.id == device)
    }

    pub fn set_midi_device_param(&mut self, track: Id, device: Id, index: usize, value: f32) {
        let Some(d) = self.midi_device_mut(track, device) else {
            return;
        };
        d.params.set(index, value);
        let value = d.params.get(index);
        self.touch();
        self.send(Command::SetMidiDeviceParam {
            track,
            device,
            index,
            value,
        });
        self.learn_touch(MapTarget::MidiDevice { track, device, index });
    }

    pub fn set_midi_device_params(&mut self, track: Id, device: Id, params: MidiDeviceParams) {
        let Some(d) = self.midi_device_mut(track, device) else {
            return;
        };
        if d.params.kind() != params.kind() {
            return;
        }
        d.params = params;
        self.touch();
        self.send(Command::SetMidiDeviceParams { track, device, params });
    }

    pub fn set_midi_device_on(&mut self, track: Id, device: Id, on: bool) {
        let Some(d) = self.midi_device_mut(track, device) else {
            return;
        };
        d.on = on;
        self.touch();
        self.send(Command::SetMidiDeviceOn { track, device, on });
    }

    /// Changes where a MIDI track takes hardware MIDI from.
    pub fn set_midi_input(&mut self, track: Id, input: MidiInput) {
        if let Some(t) = self.project.track_mut(track) {
            if t.midi_input != input {
                t.midi_input = input;
                self.changed();
            }
        }
    }
}
