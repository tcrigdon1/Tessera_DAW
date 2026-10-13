//! Editing device chains, sends, return tracks and track inputs.

use tessera_engine::fx::{Device, DeviceKind, DeviceParams};
use tessera_engine::midi::mapping::MapTarget;
use tessera_engine::model::{Id, MASTER_BUS};
use tessera_engine::Command;

use crate::app::{DetailMode, TesseraApp};

impl TesseraApp {
    /// The bus whose devices the Device view shows: the selected track, return
    /// or the master.
    pub fn selected_bus(&self) -> Option<Id> {
        let id = self.selected_track?;
        (id == MASTER_BUS || self.project.chain(id).is_some()).then_some(id)
    }

    /// Display name of a track, return or the master.
    pub fn bus_name(&self, bus: Id) -> String {
        if bus == MASTER_BUS {
            return "Master".into();
        }
        if let Some(t) = self.project.track(bus) {
            return t.name.clone();
        }
        self.project
            .returns
            .iter()
            .find(|r| r.id == bus)
            .map(|r| r.name.clone())
            .unwrap_or_default()
    }

    /// Appends an effect to a chain and shows it.
    pub fn add_device(&mut self, bus: Id, kind: DeviceKind) {
        let id = self.project.alloc_id();
        let Some(chain) = self.project.chain_mut(bus) else {
            return;
        };
        chain.push(Device::new(id, kind));
        self.selected_track = Some(bus);
        self.detail = DetailMode::Device;
        self.status = format!("Added {} to {}", kind.name(), self.bus_name(bus));
        self.changed();
    }

    pub fn remove_device(&mut self, bus: Id, device: Id) {
        if let Some(chain) = self.project.chain_mut(bus) {
            chain.retain(|d| d.id != device);
            self.prune_mappings();
            self.changed();
        }
    }

    /// Moves a device left (`-1`) or right (`+1`) in its chain.
    pub fn move_device(&mut self, bus: Id, device: Id, delta: i32) {
        let Some(chain) = self.project.chain_mut(bus) else {
            return;
        };
        let Some(i) = chain.iter().position(|d| d.id == device) else {
            return;
        };
        let j = i as i32 + delta;
        if j < 0 || j as usize >= chain.len() {
            return;
        }
        chain.swap(i, j as usize);
        self.changed();
    }

    fn device_mut(&mut self, bus: Id, device: Id) -> Option<&mut Device> {
        self.project.chain_mut(bus)?.iter_mut().find(|d| d.id == device)
    }

    pub fn set_device_param(&mut self, bus: Id, device: Id, index: usize, value: f32) {
        let Some(d) = self.device_mut(bus, device) else { return };
        d.params.set(index, value);
        let value = d.params.get(index);
        self.dirty = true;
        self.send(Command::SetDeviceParam {
            bus,
            device,
            index,
            value,
        });
        self.learn_touch(MapTarget::Device { bus, device, index });
    }

    pub fn set_device_params(&mut self, bus: Id, device: Id, params: DeviceParams) {
        let Some(d) = self.device_mut(bus, device) else { return };
        if d.params.kind() != params.kind() {
            return;
        }
        d.params = params;
        self.dirty = true;
        self.send(Command::SetDeviceParams { bus, device, params });
    }

    pub fn set_device_on(&mut self, bus: Id, device: Id, on: bool) {
        let Some(d) = self.device_mut(bus, device) else { return };
        d.on = on;
        self.dirty = true;
        self.send(Command::SetDeviceOn { bus, device, on });
    }

    pub fn set_send(&mut self, track: Id, index: usize, db: f32) {
        let Some(t) = self.project.track_mut(track) else { return };
        let Some(s) = t.sends.get_mut(index) else { return };
        *s = db;
        self.dirty = true;
        self.send(Command::SetSend { track, index, db });
        self.learn_touch(MapTarget::Send { track, index });
    }

    /// Sends the mixer settings of a track, return or the master.
    pub fn send_bus_mixer(&mut self, bus: Id) {
        if bus == MASTER_BUS {
            let m = self.project.master;
            self.dirty = true;
            self.learn_mixer(bus, m);
            self.send(Command::SetMaster(m));
        } else if self.project.track(bus).is_some() {
            self.send_mixer(bus);
        } else if let Some(r) = self.project.returns.iter().find(|r| r.id == bus) {
            let params = r.mixer;
            self.dirty = true;
            self.learn_mixer(bus, params);
            self.send(Command::SetMixer { track: bus, params });
        }
    }

    pub fn add_return_track(&mut self, effect: Option<DeviceKind>) {
        match self.project.add_return(effect) {
            Some(id) => {
                self.selected_track = Some(id);
                self.changed();
            }
            None => self.status = "Maximum number of return tracks reached".into(),
        }
    }

    pub fn delete_return(&mut self, id: Id) {
        self.project.remove_return(id);
        if self.selected_track == Some(id) {
            self.selected_track = self.project.tracks.first().map(|t| t.id);
        }
        self.changed();
    }

    pub fn send_track_input(&mut self, track: Id) {
        if let Some(t) = self.project.track(track) {
            let input = t.input;
            self.dirty = true;
            self.send(Command::SetTrackInput { track, input });
        }
    }
}
