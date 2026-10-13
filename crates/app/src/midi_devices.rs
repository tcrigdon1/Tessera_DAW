//! Editing a track's MIDI effects and MIDI input routing.

use tessera_engine::midi::mapping::MapTarget;
use tessera_engine::midi_fx::{MidiDevice, MidiDeviceKind, MidiDeviceParams};
use tessera_engine::model::{Id, MidiInput, TrackKind};
use tessera_engine::Command;

use crate::app::{DetailMode, TesseraApp};

impl TesseraApp {
    /// Adds a MIDI effect at the end of a MIDI track's MIDI chain.
    pub fn add_midi_device(&mut self, track: Id, kind: MidiDeviceKind) {
        let id = self.project.alloc_id();
        let Some(t) = self.project.track_mut(track) else { return };
        if t.kind != TrackKind::Midi {
            self.status = "MIDI effects go on MIDI tracks".into();
            return;
        }
        t.midi_devices.push(MidiDevice::new(id, kind));
        let name = t.name.clone();
        self.selected_track = Some(track);
        self.detail = DetailMode::Device;
        self.status = format!("Added {} to {name}", kind.name());
        self.changed();
    }

    pub fn remove_midi_device(&mut self, track: Id, device: Id) {
        if let Some(t) = self.project.track_mut(track) {
            t.midi_devices.retain(|d| d.id != device);
            self.prune_mappings();
            self.changed();
        }
    }

    pub fn move_midi_device(&mut self, track: Id, device: Id, delta: i32) {
        let Some(t) = self.project.track_mut(track) else { return };
        let Some(i) = t.midi_devices.iter().position(|d| d.id == device) else {
            return;
        };
        let j = i as i32 + delta;
        if j < 0 || j as usize >= t.midi_devices.len() {
            return;
        }
        t.midi_devices.swap(i, j as usize);
        self.changed();
    }

    fn midi_device_mut(&mut self, track: Id, device: Id) -> Option<&mut MidiDevice> {
        self.project
            .track_mut(track)?
            .midi_devices
            .iter_mut()
            .find(|d| d.id == device)
    }

    pub fn set_midi_device_param(&mut self, track: Id, device: Id, index: usize, value: f32) {
        let Some(d) = self.midi_device_mut(track, device) else {
            return;
        };
        d.params.set(index, value);
        let value = d.params.get(index);
        self.dirty = true;
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
        self.dirty = true;
        self.send(Command::SetMidiDeviceParams { track, device, params });
    }

    pub fn set_midi_device_on(&mut self, track: Id, device: Id, on: bool) {
        let Some(d) = self.midi_device_mut(track, device) else {
            return;
        };
        d.on = on;
        self.dirty = true;
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
