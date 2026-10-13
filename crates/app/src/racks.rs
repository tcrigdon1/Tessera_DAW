//! Editing racks: chains, macros, mappings, variations and the devices
//! nested inside racks.
//!
//! A rack is addressed by [`RackRef`]: either a track's own rack (Instrument
//! Rack chains or Drum Rack pad chains in `Track::rack`) or an Audio Effect
//! Rack device somewhere under a track, return or the master. Devices and
//! chains have project-wide unique ids, so nested ones are found by search.

use std::collections::HashMap;

use tessera_engine::dsp::drum_rack::{pad_note_name, PadSource, PADS};
use tessera_engine::fx::rack::{
    ChainMixer, ChainZones, MacroTarget, Rack, RackChain, RackParams, CHAIN_SELECT, MACROS, MAX_RACK_RETURNS,
};
use tessera_engine::fx::{Device, DeviceKind, DeviceParams};
use tessera_engine::model::{Id, Instrument, Project, SEND_OFF_DB};
use tessera_engine::Command;

use crate::app::{DetailMode, TesseraApp};

/// Which rack an edit is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RackRef {
    /// A track's Instrument Rack or Drum Rack chains.
    Track(Id),
    /// An Audio Effect Rack device under a track, return or the master.
    Device { bus: Id, device: Id },
    /// A track's MIDI Effect Rack.
    Midi(Id),
}

impl RackRef {
    /// The track, return or master the rack belongs to.
    pub fn bus(self) -> Id {
        match self {
            RackRef::Track(t) | RackRef::Midi(t) => t,
            RackRef::Device { bus, .. } => bus,
        }
    }
}

/// A list of devices: a bus's own chain or a rack chain under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainRef {
    Bus(Id),
    Rack { bus: Id, chain: Id },
}

fn find_in_list(list: &mut [Device], id: Id) -> Option<&mut Device> {
    for d in list.iter_mut() {
        if d.id == id {
            return Some(d);
        }
        if let Some(r) = d.rack.as_deref_mut() {
            if let Some(found) = r.find_device_mut(id) {
                return Some(found);
            }
        }
    }
    None
}

/// The device list that directly contains `id`.
fn container_in(list: &mut Vec<Device>, id: Id) -> Option<&mut Vec<Device>> {
    if list.iter().any(|d| d.id == id) {
        return Some(list);
    }
    for d in list.iter_mut() {
        if let Some(r) = d.rack.as_deref_mut() {
            if let Some(found) = container_in_rack(r, id) {
                return Some(found);
            }
        }
    }
    None
}

fn container_in_rack(r: &mut Rack, id: Id) -> Option<&mut Vec<Device>> {
    for c in r.chains.iter_mut().chain(r.returns.iter_mut()) {
        if let Some(found) = container_in(&mut c.devices, id) {
            return Some(found);
        }
    }
    None
}

/// Gives a chain (and everything in it) fresh ids, keeping mappings of
/// nested racks pointing at the copies.
fn reid_chain(p: &mut Project, c: &mut RackChain) {
    let mut map = HashMap::new();
    c.reid(&mut || p.alloc_id(), &mut map);
    for d in c.devices.iter_mut() {
        if let Some(r) = d.rack.as_deref_mut() {
            r.remap_ids(&map);
        }
    }
}

/// A random generator for macro randomisation (no dependency needed).
fn random_unit(seed: &mut u64) -> f32 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    (*seed >> 40) as f32 / (1u64 << 24) as f32
}

impl TesseraApp {
    // ----- finding things ------------------------------------------------------------

    /// A device anywhere under a bus: its chain, racks nested in it, and the
    /// track's own rack.
    pub fn find_device_mut(&mut self, bus: Id, id: Id) -> Option<&mut Device> {
        let in_chain = self.project.chain(bus).is_some_and(|c| {
            c.iter()
                .any(|d| d.id == id || d.rack.as_deref().is_some_and(|r| r.find_device(id).is_some()))
        });
        if in_chain {
            return self.project.chain_mut(bus).and_then(|c| find_in_list(c, id));
        }
        self.project
            .track_mut(bus)
            .and_then(|t| t.rack.as_mut())
            .and_then(|r| r.find_device_mut(id))
    }

    pub fn find_device(&self, bus: Id, id: Id) -> Option<&Device> {
        let chain = self.project.chain(bus)?;
        for d in chain {
            if d.id == id {
                return Some(d);
            }
            if let Some(found) = d.rack.as_deref().and_then(|r| r.find_device(id)) {
                return Some(found);
            }
        }
        self.project
            .track(bus)
            .and_then(|t| t.rack.as_ref())
            .and_then(|r| r.find_device(id))
    }

    /// The device list that holds `device`.
    pub(crate) fn container_mut(&mut self, bus: Id, device: Id) -> Option<&mut Vec<Device>> {
        let in_chain = self.project.chain(bus).is_some_and(|c| {
            c.iter()
                .any(|d| d.id == device || d.rack.as_deref().is_some_and(|r| r.find_device(device).is_some()))
        });
        if in_chain {
            return self.project.chain_mut(bus).and_then(|c| container_in(c, device));
        }
        self.project
            .track_mut(bus)
            .and_then(|t| t.rack.as_mut())
            .and_then(|r| container_in_rack(r, device))
    }

    pub fn rack(&self, r: RackRef) -> Option<&Rack> {
        match r {
            RackRef::Track(t) => self.project.track(t)?.rack.as_ref(),
            RackRef::Device { bus, device } => self.find_device(bus, device)?.rack.as_deref(),
            RackRef::Midi(t) => self.project.track(t)?.midi_rack.as_ref(),
        }
    }

    pub fn rack_mut(&mut self, r: RackRef) -> Option<&mut Rack> {
        match r {
            RackRef::Track(t) => self.project.track_mut(t)?.rack.as_mut(),
            RackRef::Device { bus, device } => self.find_device_mut(bus, device)?.rack.as_deref_mut(),
            RackRef::Midi(t) => self.project.track_mut(t)?.midi_rack.as_mut(),
        }
    }

    /// Macro values and chain selector of a rack (Drum Racks have none).
    pub fn rack_params(&self, r: RackRef) -> Option<RackParams> {
        match r {
            RackRef::Track(t) => match self.project.track(t)?.instrument {
                Instrument::Rack(p) => Some(p),
                _ => None,
            },
            RackRef::Midi(t) => self.midi_rack_device(t).map(|(_, p)| p),
            RackRef::Device { bus, device } => match self.find_device(bus, device)?.params {
                DeviceParams::Rack(p) => Some(p),
                _ => None,
            },
        }
    }

    fn rack_chain_mut(&mut self, bus: Id, chain: Id) -> Option<&mut RackChain> {
        let in_track = self
            .project
            .track(bus)
            .and_then(|t| t.rack.as_ref())
            .is_some_and(|r| r.find_chain(chain).is_some());
        if in_track {
            return self.project.track_mut(bus)?.rack.as_mut()?.find_chain_mut(chain);
        }
        self.project
            .chain_mut(bus)?
            .iter_mut()
            .filter_map(|d| d.rack.as_deref_mut())
            .find_map(|r| r.find_chain_mut(chain))
    }

    pub fn chain_devices_mut(&mut self, c: ChainRef) -> Option<&mut Vec<Device>> {
        match c {
            ChainRef::Bus(bus) => self.project.chain_mut(bus),
            ChainRef::Rack { bus, chain } => self.rack_chain_mut(bus, chain).map(|c| &mut c.devices),
        }
    }

    // ----- devices in chains -----------------------------------------------------------

    /// Appends an effect to a rack chain (or a bus) and shows it.
    pub fn add_device_to(&mut self, c: ChainRef, kind: DeviceKind) {
        let id = self.project.alloc_id();
        let device = if kind == DeviceKind::AudioRack {
            let chain = self.project.alloc_id();
            Device::new_rack(id, Rack::with_chain(RackChain::new(chain, "Chain 1")))
        } else {
            Device::new(id, kind)
        };
        let Some(list) = self.chain_devices_mut(c) else { return };
        list.push(device);
        let bus = match c {
            ChainRef::Bus(b) | ChainRef::Rack { bus: b, .. } => b,
        };
        self.selected_track = Some(bus);
        self.detail = DetailMode::Device;
        self.status = format!("Added {}", kind.name());
        self.changed();
    }

    /// Wraps a device into a new Audio Effect Rack in its place.
    pub fn group_into_rack(&mut self, bus: Id, device: Id) {
        let (rack_id, chain_id) = (self.project.alloc_id(), self.project.alloc_id());
        let Some(list) = self.container_mut(bus, device) else {
            return;
        };
        let Some(i) = list.iter().position(|d| d.id == device) else {
            return;
        };
        let d = list.remove(i);
        let mut chain = RackChain::new(chain_id, d.kind().name());
        chain.devices.push(d);
        list.insert(i, Device::new_rack(rack_id, Rack::with_chain(chain)));
        self.status = "Grouped into an Audio Effect Rack".into();
        self.changed();
    }

    /// Replaces a rack device by the devices of its first chain.
    pub fn ungroup_rack(&mut self, bus: Id, device: Id) {
        let Some(list) = self.container_mut(bus, device) else {
            return;
        };
        let Some(i) = list.iter().position(|d| d.id == device) else {
            return;
        };
        let Some(rack) = list[i].rack.take() else { return };
        list.remove(i);
        let inner: Vec<Device> = rack.chains.into_iter().next().map(|c| c.devices).unwrap_or_default();
        for (k, d) in inner.into_iter().enumerate() {
            list.insert(i + k, d);
        }
        self.project.prune_plugins();
        self.changed();
    }

    // ----- chains --------------------------------------------------------------------

    /// Adds a chain, optionally holding an instrument (Instrument Rack).
    pub fn add_chain(&mut self, r: RackRef, instrument: Option<Instrument>) {
        let id = self.project.alloc_id();
        let Some(rack) = self.rack_mut(r) else { return };
        let n = rack.chains.len();
        let mut c = RackChain::new(id, format!("Chain {}", n + 1));
        if let Some(i) = instrument {
            c.name = i.name().to_string();
            c.instrument = i;
        }
        rack.chains.push(c);
        rack.selected_chain = n;
        rack.show_chains = true;
        self.changed();
    }

    pub fn remove_chain(&mut self, r: RackRef, chain: Id) {
        let Some(rack) = self.rack_mut(r) else { return };
        rack.chains.retain(|c| c.id != chain);
        rack.returns.retain(|c| c.id != chain);
        rack.prune_mappings();
        rack.selected_chain = rack.selected_chain.min(rack.chains.len().saturating_sub(1));
        self.project.prune_plugins();
        self.changed();
    }

    pub fn duplicate_chain(&mut self, r: RackRef, chain: Id) {
        let Some(mut copy) = self.rack(r).and_then(|k| k.chain(chain)).cloned() else {
            return;
        };
        reid_chain(&mut self.project, &mut copy);
        copy.name = format!("{} copy", copy.name);
        let Some(rack) = self.rack_mut(r) else { return };
        let i = rack
            .chains
            .iter()
            .position(|c| c.id == chain)
            .map(|i| i + 1)
            .unwrap_or(rack.chains.len());
        rack.chains.insert(i, copy);
        rack.selected_chain = i;
        self.changed();
    }

    pub fn rename_chain(&mut self, r: RackRef, chain: Id, name: String) {
        if let Some(c) = self.rack_mut(r).and_then(|k| k.chain_mut(chain)) {
            c.name = name;
            self.dirty = true;
        }
    }

    pub fn select_chain(&mut self, r: RackRef, index: usize) {
        if let Some(rack) = self.rack_mut(r) {
            rack.selected_chain = index;
        }
        // A Drum Rack's chains are its pads: selecting one selects the pad.
        if let RackRef::Track(t) = r {
            if matches!(
                self.project.track(t).map(|t| t.instrument),
                Some(Instrument::DrumRack(_))
            ) {
                self.browser.drum_pad.insert(t, index);
            }
        }
    }

    pub fn set_chain_mixer(&mut self, r: RackRef, chain: Id, mixer: ChainMixer) {
        let Some(c) = self.rack_mut(r).and_then(|k| k.chain_mut(chain)) else {
            return;
        };
        c.mixer = mixer;
        self.dirty = true;
        self.send(Command::SetChainMixer {
            bus: r.bus(),
            chain,
            mixer,
        });
    }

    pub fn set_chain_zones(&mut self, r: RackRef, chain: Id, zones: ChainZones) {
        let zones = ChainZones {
            key: zones.key.normalized(),
            velocity: zones.velocity.normalized(),
            select: zones.select.normalized(),
        };
        let Some(c) = self.rack_mut(r).and_then(|k| k.chain_mut(chain)) else {
            return;
        };
        c.zones = zones;
        self.dirty = true;
        self.send(Command::SetChainZones {
            bus: r.bus(),
            chain,
            zones,
        });
    }

    /// Swaps the instrument on an Instrument Rack chain. Samples are kept
    /// only when the instrument kind stays the same.
    pub fn set_chain_instrument(&mut self, r: RackRef, chain: Id, instrument: Instrument) {
        let Some(c) = self.rack_mut(r).and_then(|k| k.chain_mut(chain)) else {
            return;
        };
        if c.instrument.kind_tag() != instrument.kind_tag() {
            c.samples.clear();
            if c.name.starts_with("Chain ") || c.name == c.instrument.name() {
                c.name = instrument.name().to_string();
            }
        }
        c.instrument = instrument;
        self.changed();
    }

    /// Send level of a Drum Rack pad chain to a rack return.
    pub fn set_chain_send(&mut self, track: Id, chain: Id, index: usize, db: f32) {
        let Some(c) = self.rack_mut(RackRef::Track(track)).and_then(|k| k.chain_mut(chain)) else {
            return;
        };
        if c.sends.len() <= index {
            c.sends.resize(index + 1, SEND_OFF_DB);
        }
        c.sends[index] = db;
        self.dirty = true;
        self.send(Command::SetChainSend {
            track,
            chain,
            index,
            db,
        });
    }

    // ----- Instrument and Drum Racks ---------------------------------------------------

    /// Turns the track's instrument into an Instrument Rack whose first
    /// chain holds the old instrument and its samples.
    pub fn make_instrument_rack(&mut self, track: Id) {
        let wrap = self
            .project
            .track(track)
            .is_some_and(|t| !matches!(t.instrument, Instrument::Rack(_) | Instrument::Plugin(_)));
        if wrap {
            self.set_instrument(track, Instrument::Rack(RackParams::default()));
        }
    }

    /// Gives a Drum Rack one chain per pad, so pads can hold effects.
    pub fn ensure_drum_chains(&mut self, track: Id) {
        let have = self
            .project
            .track(track)
            .map(|t| t.rack.as_ref().map(|r| r.chains.len()).unwrap_or(0))
            .unwrap_or(PADS);
        if have >= PADS {
            return;
        }
        let ids: Vec<Id> = (have..PADS).map(|_| self.project.alloc_id()).collect();
        let Some(t) = self.project.track_mut(track) else { return };
        let names: Vec<String> = (have..PADS)
            .map(|i| {
                let label = match t.instrument {
                    Instrument::DrumRack(p) => match p.pads[i].source {
                        PadSource::Sample => t.sample(i).map(|s| s.name()).unwrap_or_else(|| "Sample".into()),
                        s => s.label().to_string(),
                    },
                    _ => format!("Pad {}", i + 1),
                };
                format!("{} {label}", pad_note_name(i))
            })
            .collect();
        let rack = t.rack.get_or_insert_with(Rack::default);
        for (id, name) in ids.into_iter().zip(names) {
            rack.chains.push(RackChain::new(id, name));
        }
        self.changed();
    }

    /// Adds a return chain to a Drum Rack (up to six).
    pub fn add_rack_return(&mut self, track: Id, effect: Option<DeviceKind>) {
        self.ensure_drum_chains(track);
        let (id, dev) = (self.project.alloc_id(), self.project.alloc_id());
        let Some(rack) = self.rack_mut(RackRef::Track(track)) else {
            return;
        };
        if rack.returns.len() >= MAX_RACK_RETURNS {
            self.status = "A Drum Rack has at most six returns".into();
            return;
        }
        let letter = (b'A' + rack.returns.len() as u8) as char;
        let mut c = RackChain::new(id, format!("{letter} Return"));
        if let Some(kind) = effect {
            let mut d = Device::new(dev, kind);
            if let Some(i) = d.params.info().iter().position(|p| p.name == "Dry/Wet") {
                d.params.set(i, 1.0);
            }
            c.name = format!("{letter} {}", kind.name());
            c.devices.push(d);
        }
        rack.returns.push(c);
        self.changed();
    }

    // ----- macros ----------------------------------------------------------------------

    /// Sets one rack parameter (a macro 0..1 or the chain selector), moves
    /// the mapped parameters in the document and tells the engine.
    pub fn set_rack_param(&mut self, r: RackRef, index: usize, value: f32) {
        let Some(mut p) = self.rack_params(r) else { return };
        p.set(index, value);
        let macros = p.macros();
        if let Some(rack) = self.rack_mut(r) {
            rack.apply_macros(&macros);
        }
        match r {
            RackRef::Track(t) => {
                if let Some(tr) = self.project.track_mut(t) {
                    tr.instrument = Instrument::Rack(p);
                }
                self.send_instrument(t);
            }
            RackRef::Device { bus, device } => self.set_device_param(bus, device, index, p.get(index)),
            RackRef::Midi(t) => {
                if let Some((d, _)) = self.midi_rack_device(t) {
                    self.set_midi_device_param(t, d, index, p.get(index));
                }
                // The engine moves the mapped parameters itself; keep the
                // document's copies of the chains' devices in step.
            }
        }
    }

    pub fn set_macro(&mut self, r: RackRef, index: usize, value: f32) {
        if index < MACROS {
            self.set_rack_param(r, index, value);
        }
    }

    pub fn set_chain_selector(&mut self, r: RackRef, value: f32) {
        self.set_rack_param(r, CHAIN_SELECT, value);
    }

    /// Sets all eight macros at once (variations, randomise).
    pub fn set_macros(&mut self, r: RackRef, values: [f32; MACROS]) {
        for (i, v) in values.iter().enumerate() {
            self.set_rack_param(r, i, *v);
        }
    }

    pub fn map_macro(&mut self, r: RackRef, macro_index: usize, target: MacroTarget) {
        let Some(rack) = self.rack_mut(r) else { return };
        rack.map(macro_index, target);
        self.status = format!("Mapped to {}", rack.macro_name(macro_index));
        self.changed();
    }

    pub fn unmap(&mut self, r: RackRef, target: MacroTarget) {
        if let Some(rack) = self.rack_mut(r) {
            rack.unmap(target);
            self.changed();
        }
    }

    /// Changes the range of the mapping of `target`.
    pub fn set_mapping_range(&mut self, r: RackRef, target: MacroTarget, min: f32, max: f32) {
        let Some(rack) = self.rack_mut(r) else { return };
        if let Some(m) = rack.mappings.iter_mut().find(|m| m.target == target) {
            m.min = min.clamp(0.0, 1.0);
            m.max = max.clamp(0.0, 1.0);
        }
        self.changed();
    }

    pub fn rename_macro(&mut self, r: RackRef, index: usize, name: String) {
        if let Some(rack) = self.rack_mut(r) {
            rack.set_macro_name(index, name);
            self.dirty = true;
        }
    }

    pub fn store_variation(&mut self, r: RackRef) {
        let Some(p) = self.rack_params(r) else { return };
        if let Some(rack) = self.rack_mut(r) {
            let n = rack.store_variation(p.macros());
            self.status = format!("Stored macro variation {}", n + 1);
            self.dirty = true;
        }
    }

    pub fn recall_variation(&mut self, r: RackRef, index: usize) {
        let Some(values) = self.rack(r).and_then(|k| k.variations.get(index)).map(|v| v.values) else {
            return;
        };
        self.set_macros(r, values);
    }

    pub fn overwrite_variation(&mut self, r: RackRef, index: usize) {
        let Some(p) = self.rack_params(r) else { return };
        if let Some(v) = self.rack_mut(r).and_then(|k| k.variations.get_mut(index)) {
            v.values = p.macros();
            self.dirty = true;
        }
    }

    pub fn delete_variation(&mut self, r: RackRef, index: usize) {
        if let Some(rack) = self.rack_mut(r) {
            if index < rack.variations.len() {
                rack.variations.remove(index);
                self.dirty = true;
            }
        }
    }

    /// Sets every macro that has a mapping to a random value.
    pub fn randomize_macros(&mut self, r: RackRef, seed: u64) {
        let Some(p) = self.rack_params(r) else { return };
        let Some(rack) = self.rack(r) else { return };
        let mut values = p.macros();
        let mut s = seed | 1;
        for (i, v) in values.iter_mut().enumerate() {
            if rack.mappings.iter().any(|m| m.macro_index as usize == i) {
                *v = random_unit(&mut s);
            }
        }
        self.set_macros(r, values);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicated_chains_get_fresh_ids_and_keep_nested_mappings() {
        let mut p = Project::default();
        let mut inner = Rack::with_chain(RackChain::new(p.alloc_id(), "in"));
        let dev = p.alloc_id();
        inner.chains[0].devices.push(Device::new(dev, DeviceKind::Delay));
        inner.map(0, MacroTarget::Device { device: dev, param: 0 });
        let mut chain = RackChain::new(p.alloc_id(), "c");
        chain.devices.push(Device::new_rack(p.alloc_id(), inner));
        let before: Vec<Id> = {
            let mut v = vec![chain.id];
            for d in &chain.devices {
                d.visit(&mut |d| v.push(d.id));
            }
            v
        };
        let mut copy = chain.clone();
        reid_chain(&mut p, &mut copy);
        let mut after = vec![copy.id];
        for d in &copy.devices {
            d.visit(&mut |d| after.push(d.id));
        }
        assert!(after.iter().all(|id| !before.contains(id)));
        let r = copy.devices[0].rack.as_deref().unwrap();
        let MacroTarget::Device { device, .. } = r.mappings[0].target else {
            panic!()
        };
        assert!(r.find_device(device).is_some(), "mapping follows the copy");
    }

    #[test]
    fn containers_are_found_in_nested_racks() {
        let mut list = vec![Device::new(1, DeviceKind::Eq)];
        let mut rack = Rack::with_chain(RackChain::new(3, "c"));
        rack.chains[0].devices.push(Device::new(4, DeviceKind::Delay));
        list.push(Device::new_rack(2, rack));
        assert_eq!(container_in(&mut list, 1).map(|l| l.len()), Some(2));
        assert_eq!(container_in(&mut list, 4).map(|l| l.len()), Some(1));
        assert!(find_in_list(&mut list, 4).is_some());
        assert!(container_in(&mut list, 9).is_none());
    }
}
