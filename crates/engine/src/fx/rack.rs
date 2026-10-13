//! Racks: parallel chains of devices behind eight macro knobs.
//!
//! The same [`Rack`] document backs three devices:
//!
//! * the **Audio Effect Rack**, a device in an effect chain
//!   ([`super::Device::rack`]) whose chains each hold effects. The input is
//!   split to every chain and the outputs are summed;
//! * the **Instrument Rack**, a track instrument
//!   ([`crate::model::Instrument::Rack`], chains in
//!   [`crate::model::Track::rack`]) whose chains each hold an instrument and
//!   effects. Notes go to the chains whose key and velocity zones contain them;
//! * the **Drum Rack**, whose pads each own a chain of effects and sends to
//!   the rack's return chains ([`Rack::returns`]).
//!
//! The chain selector and each chain's select zone crossfade between chains.
//! Macros are mapped onto parameters inside the rack with a range each;
//! moving a macro moves every mapped parameter. Macro variations store and
//! recall snapshots of all eight macros.
//!
//! The audio-thread side of the Audio Effect Rack is [`RackRuntime`]; the
//! Instrument and Drum Rack runtimes live in [`crate::engine`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::chain::{FxChain, FxContext};
use super::{device_params, Device, ParamKind, Scale, Unit};
use crate::automation::{fader_pos_to_db, instrument_params, set_instrument_param};
use crate::model::{db_to_gain, pan_gains, Id, Instrument, SampleRef};
use crate::MAX_BLOCK;

/// Macro knobs per rack.
pub const MACROS: usize = 8;
/// Return chains of a Drum Rack.
pub const MAX_RACK_RETURNS: usize = 6;
/// Upper bound on chains in one rack.
pub const MAX_CHAINS: usize = 64;

const PCT: ParamKind = ParamKind::Continuous(Scale::Linear);

device_params! {
    /// The automatable part of a rack: macro values (0..1) and the chain
    /// selector (0..127).
    pub struct RackParams {
        macro1: "Macro 1", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        macro2: "Macro 2", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        macro3: "Macro 3", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        macro4: "Macro 4", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        macro5: "Macro 5", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        macro6: "Macro 6", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        macro7: "Macro 7", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        macro8: "Macro 8", 0.0, 1.0, 0.0, PCT, Unit::Percent;
        chain_select: "Chain Select", 0.0, 127.0, 0.0, PCT, Unit::Integer;
    }
}

/// Parameter index of the chain selector in [`RackParams`].
pub const CHAIN_SELECT: usize = MACROS;

impl RackParams {
    pub fn macros(&self) -> [f32; MACROS] {
        std::array::from_fn(|i| self.get(i))
    }

    pub fn set_macros(&mut self, values: &[f32; MACROS]) {
        for (i, v) in values.iter().enumerate() {
            self.set(i, *v);
        }
    }
}

/// A key, velocity or chain-select range (0..=127) with fades at both ends.
/// Inside `lo + fade_lo ..= hi - fade_hi` a chain plays at full level; in the
/// fades it is crossfaded linearly; outside it is silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Zone {
    pub lo: u8,
    pub hi: u8,
    pub fade_lo: u8,
    pub fade_hi: u8,
}

impl Default for Zone {
    fn default() -> Self {
        Self::FULL
    }
}

impl Zone {
    pub const FULL: Zone = Zone {
        lo: 0,
        hi: 127,
        fade_lo: 0,
        fade_hi: 0,
    };

    /// A zone covering exactly `lo..=hi`.
    pub fn range(lo: u8, hi: u8) -> Self {
        Zone {
            lo: lo.min(hi),
            hi: hi.max(lo),
            fade_lo: 0,
            fade_hi: 0,
        }
    }

    /// Keeps the bounds ordered and the fades inside the range.
    pub fn normalized(self) -> Self {
        let lo = self.lo.min(127);
        let hi = self.hi.clamp(lo, 127);
        let span = hi - lo;
        let fade_lo = self.fade_lo.min(span);
        let fade_hi = self.fade_hi.min(span - fade_lo);
        Zone {
            lo,
            hi,
            fade_lo,
            fade_hi,
        }
    }

    pub fn is_full(&self) -> bool {
        self.lo == 0 && self.hi == 127 && self.fade_lo == 0 && self.fade_hi == 0
    }

    /// Level 0..1 at `v` (a note, velocity or selector value).
    #[inline]
    pub fn gain(&self, v: f32) -> f32 {
        let (lo, hi) = (self.lo as f32, self.hi as f32);
        if v < lo - 0.5 || v > hi + 0.5 {
            return 0.0;
        }
        let mut g = 1.0f32;
        if self.fade_lo > 0 {
            let f = self.fade_lo as f32 + 1.0;
            g = g.min(((v - lo + 1.0) / f).clamp(0.0, 1.0));
        }
        if self.fade_hi > 0 {
            let f = self.fade_hi as f32 + 1.0;
            g = g.min(((hi - v + 1.0) / f).clamp(0.0, 1.0));
        }
        g
    }
}

/// A chain's mixer: level, pan, activator and solo.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChainMixer {
    pub volume_db: f32,
    pub pan: f32,
    /// Chain activator off.
    pub mute: bool,
    pub solo: bool,
}

impl Default for ChainMixer {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
        }
    }
}

/// The zones of a chain.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ChainZones {
    pub key: Zone,
    pub velocity: Zone,
    pub select: Zone,
}

/// One chain of a rack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RackChain {
    pub id: Id,
    pub name: String,
    #[serde(default)]
    pub mixer: ChainMixer,
    #[serde(default)]
    pub zones: ChainZones,
    /// The chain's instrument (Instrument Rack only).
    #[serde(default = "no_instrument")]
    pub instrument: Instrument,
    /// Samples of the chain's instrument (Sampler, Drum Rack, Granular).
    #[serde(default)]
    pub samples: Vec<Option<SampleRef>>,
    /// Effects after the instrument.
    #[serde(default)]
    pub devices: Vec<Device>,
    /// Send levels in dB to the rack's return chains (Drum Rack pads).
    #[serde(default)]
    pub sends: Vec<f32>,
    /// MIDI effects of a MIDI Effect Rack chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub midi_devices: Vec<crate::midi_fx::MidiDevice>,
}

fn no_instrument() -> Instrument {
    Instrument::None
}

impl RackChain {
    pub fn new(id: Id, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            mixer: ChainMixer::default(),
            zones: ChainZones::default(),
            instrument: Instrument::None,
            samples: Vec::new(),
            devices: Vec::new(),
            sends: Vec::new(),
            midi_devices: Vec::new(),
        }
    }

    pub fn with_instrument(mut self, instrument: Instrument) -> Self {
        self.instrument = instrument;
        self
    }

    /// Puts a sample into an instrument slot, growing the list as needed.
    pub fn set_sample(&mut self, slot: usize, sample: Option<SampleRef>) {
        if self.samples.len() <= slot {
            self.samples.resize(slot + 1, None);
        }
        self.samples[slot] = sample;
    }

    /// Gives the chain and everything in it fresh ids (see [`Rack::reid`]).
    pub fn reid(&mut self, alloc: &mut dyn FnMut() -> Id, map: &mut HashMap<Id, Id>) {
        let id = alloc();
        map.insert(self.id, id);
        self.id = id;
        for d in self.devices.iter_mut() {
            d.reid(alloc, map);
        }
        for d in self.midi_devices.iter_mut() {
            let id = alloc();
            map.insert(d.id, id);
            d.id = id;
        }
    }

    /// Whether the chain holds anything that changes its audio.
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty() && matches!(self.instrument, Instrument::None)
    }
}

/// What a macro moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MacroTarget {
    /// A parameter of a device anywhere inside the rack.
    Device {
        device: Id,
        param: u16,
    },
    /// An automatable parameter of a chain's instrument (see
    /// [`instrument_params`]).
    Instrument {
        chain: Id,
        param: u16,
    },
    ChainVolume(Id),
    ChainPan(Id),
}

impl MacroTarget {
    /// The same target with its device or chain id looked up in `map`.
    pub fn remap(self, map: &HashMap<Id, Id>) -> Self {
        let new = |id: Id| map.get(&id).copied().unwrap_or(id);
        match self {
            MacroTarget::Device { device, param } => MacroTarget::Device {
                device: new(device),
                param,
            },
            MacroTarget::Instrument { chain, param } => MacroTarget::Instrument {
                chain: new(chain),
                param,
            },
            MacroTarget::ChainVolume(c) => MacroTarget::ChainVolume(new(c)),
            MacroTarget::ChainPan(c) => MacroTarget::ChainPan(new(c)),
        }
    }
}

/// A macro mapped onto one parameter. `min` and `max` are positions 0..1
/// in the target's range; `min > max` inverts the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MacroMapping {
    pub macro_index: u8,
    pub target: MacroTarget,
    pub min: f32,
    pub max: f32,
}

impl MacroMapping {
    pub fn new(macro_index: usize, target: MacroTarget) -> Self {
        Self {
            macro_index: macro_index.min(MACROS - 1) as u8,
            target,
            min: 0.0,
            max: 1.0,
        }
    }

    /// Target position 0..1 for the given macro values.
    #[inline]
    pub fn position(&self, macros: &[f32; MACROS]) -> f32 {
        let m = macros[(self.macro_index as usize).min(MACROS - 1)].clamp(0.0, 1.0);
        (self.min + (self.max - self.min) * m).clamp(0.0, 1.0)
    }
}

/// A stored snapshot of the macro knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MacroVariation {
    pub name: String,
    pub values: [f32; MACROS],
}

/// The contents of a rack.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Rack {
    pub chains: Vec<RackChain>,
    /// Return chains fed by the chains' sends (Drum Rack).
    pub returns: Vec<RackChain>,
    pub mappings: Vec<MacroMapping>,
    pub variations: Vec<MacroVariation>,
    /// Custom macro names; empty entries show "Macro n".
    pub macro_names: Vec<String>,
    /// UI: chain list shown.
    pub show_chains: bool,
    /// UI: macro knobs shown.
    pub show_macros: bool,
    /// UI: which chain's devices are shown.
    pub selected_chain: usize,
    /// UI: zone editor shown instead of the chain list.
    pub show_zones: bool,
}

impl Rack {
    /// A rack with one empty chain.
    pub fn with_chain(chain: RackChain) -> Self {
        Self {
            chains: vec![chain],
            show_chains: true,
            show_macros: true,
            ..Self::default()
        }
    }

    pub fn macro_name(&self, i: usize) -> String {
        match self.macro_names.get(i) {
            Some(n) if !n.is_empty() => n.clone(),
            _ => format!("Macro {}", i + 1),
        }
    }

    pub fn set_macro_name(&mut self, i: usize, name: String) {
        if i >= MACROS {
            return;
        }
        if self.macro_names.len() <= i {
            self.macro_names.resize(i + 1, String::new());
        }
        self.macro_names[i] = name;
    }

    pub fn chain(&self, id: Id) -> Option<&RackChain> {
        self.chains.iter().chain(self.returns.iter()).find(|c| c.id == id)
    }

    pub fn chain_mut(&mut self, id: Id) -> Option<&mut RackChain> {
        self.chains
            .iter_mut()
            .chain(self.returns.iter_mut())
            .find(|c| c.id == id)
    }

    /// The chain with `id` in this rack or any rack nested in it.
    pub fn find_chain_mut(&mut self, id: Id) -> Option<&mut RackChain> {
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            if c.id == id {
                return Some(c);
            }
            for d in c.devices.iter_mut() {
                if let Some(r) = d.rack.as_deref_mut() {
                    if let Some(found) = r.find_chain_mut(id) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    pub fn find_chain(&self, id: Id) -> Option<&RackChain> {
        for c in self.chains.iter().chain(self.returns.iter()) {
            if c.id == id {
                return Some(c);
            }
            for d in c.devices.iter() {
                if let Some(found) = d.rack.as_deref().and_then(|r| r.find_chain(id)) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// A device anywhere inside the rack.
    pub fn find_device_mut(&mut self, id: Id) -> Option<&mut Device> {
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            for d in c.devices.iter_mut() {
                if d.id == id {
                    return Some(d);
                }
                if let Some(r) = d.rack.as_deref_mut() {
                    if let Some(found) = r.find_device_mut(id) {
                        return Some(found);
                    }
                }
            }
        }
        None
    }

    /// A MIDI effect in one of the chains (MIDI Effect Rack).
    pub fn find_midi_device_mut(&mut self, id: Id) -> Option<&mut crate::midi_fx::MidiDevice> {
        self.chains
            .iter_mut()
            .flat_map(|c| c.midi_devices.iter_mut())
            .find(|d| d.id == id)
    }

    pub fn find_midi_device(&self, id: Id) -> Option<&crate::midi_fx::MidiDevice> {
        self.chains
            .iter()
            .flat_map(|c| c.midi_devices.iter())
            .find(|d| d.id == id)
    }

    pub fn find_device(&self, id: Id) -> Option<&Device> {
        for c in self.chains.iter().chain(self.returns.iter()) {
            for d in c.devices.iter() {
                if d.id == id {
                    return Some(d);
                }
                if let Some(found) = d.rack.as_deref().and_then(|r| r.find_device(id)) {
                    return Some(found);
                }
            }
        }
        None
    }

    /// Every device in the rack, nested ones included.
    pub fn visit_devices<'a>(&'a self, f: &mut dyn FnMut(&'a Device)) {
        for c in self.chains.iter().chain(self.returns.iter()) {
            for d in &c.devices {
                d.visit(f);
            }
        }
    }

    /// Gives every chain and device in the rack (nested racks included) a
    /// fresh id from `alloc`, recording old -> new in `map`. Mappings are left
    /// alone; see [`Rack::remap_ids`].
    pub fn reid(&mut self, alloc: &mut dyn FnMut() -> Id, map: &mut HashMap<Id, Id>) {
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            c.reid(alloc, map);
        }
    }

    /// Points the macro mappings of this rack and of racks nested in it at
    /// the ids in `map`.
    pub fn remap_ids(&mut self, map: &HashMap<Id, Id>) {
        for m in self.mappings.iter_mut() {
            m.target = m.target.remap(map);
        }
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            for d in c.devices.iter_mut() {
                if let Some(r) = d.rack.as_deref_mut() {
                    r.remap_ids(map);
                }
            }
        }
    }

    /// The macro mapped onto a target, if any.
    pub fn mapping_for(&self, target: MacroTarget) -> Option<&MacroMapping> {
        self.mappings.iter().find(|m| m.target == target)
    }

    /// Maps a macro onto a target, replacing an earlier mapping of the same
    /// target.
    pub fn map(&mut self, macro_index: usize, target: MacroTarget) {
        self.mappings.retain(|m| m.target != target);
        self.mappings.push(MacroMapping::new(macro_index, target));
    }

    pub fn unmap(&mut self, target: MacroTarget) {
        self.mappings.retain(|m| m.target != target);
    }

    /// Drops mappings whose target no longer exists.
    pub fn prune_mappings(&mut self) {
        let mappings = std::mem::take(&mut self.mappings);
        self.mappings = mappings
            .into_iter()
            .filter(|m| match m.target {
                MacroTarget::Device { device, .. } => {
                    self.find_device(device).is_some() || self.find_midi_device(device).is_some()
                }
                MacroTarget::Instrument { chain, .. } => self.chain(chain).is_some(),
                MacroTarget::ChainVolume(c) | MacroTarget::ChainPan(c) => self.chain(c).is_some(),
            })
            .collect();
    }

    /// Writes the mapped parameter values for the given macros into the
    /// rack's document, so the UI shows what the engine plays. The engine
    /// does the same on its copy ([`RackRuntime`], the instrument racks).
    pub fn apply_macros(&mut self, macros: &[f32; MACROS]) {
        let mappings = self.mappings.clone();
        for m in mappings {
            let n = m.position(macros);
            match m.target {
                MacroTarget::Device { device, param } => {
                    if let Some(d) = self.find_device_mut(device) {
                        if let Some(info) = d.params.info().get(param as usize) {
                            let v = info.from_unit(n);
                            d.params.set(param as usize, v);
                        }
                    } else if let Some(d) = self.find_midi_device_mut(device) {
                        if let Some(info) = d.params.info().get(param as usize) {
                            let v = info.from_unit(n);
                            d.params.set(param as usize, v);
                        }
                    }
                }
                MacroTarget::Instrument { chain, param } => {
                    if let Some(c) = self.chain_mut(chain) {
                        set_unit_instrument_param(&mut c.instrument, param, n);
                    }
                }
                MacroTarget::ChainVolume(chain) => {
                    if let Some(c) = self.chain_mut(chain) {
                        c.mixer.volume_db = chain_volume_db(n);
                    }
                }
                MacroTarget::ChainPan(chain) => {
                    if let Some(c) = self.chain_mut(chain) {
                        c.mixer.pan = n * 2.0 - 1.0;
                    }
                }
            }
        }
    }

    /// Stores the current macros as a new variation.
    pub fn store_variation(&mut self, values: [f32; MACROS]) -> usize {
        let n = self.variations.len();
        self.variations.push(MacroVariation {
            name: format!("Variation {}", n + 1),
            values,
        });
        n
    }

    /// Chain gains 0..1 for a selector value, honouring mute and solo.
    pub fn chain_levels(&self, selector: f32) -> Vec<f32> {
        let any_solo = self.chains.iter().any(|c| c.mixer.solo);
        self.chains
            .iter()
            .map(|c| {
                if c.mixer.mute || (any_solo && !c.mixer.solo) {
                    0.0
                } else {
                    c.zones.select.gain(selector)
                }
            })
            .collect()
    }
}

/// Chain volume in dB for a macro position (the fader curve, -inf .. +6 dB).
pub fn chain_volume_db(n: f32) -> f32 {
    fader_pos_to_db(n)
}

/// Sets instrument parameter `param` from a position 0..1 in its range.
pub fn set_unit_instrument_param(i: &mut Instrument, param: u16, n: f32) {
    if let Some(spec) = instrument_params(i).get(param as usize) {
        let v = spec.denormalize(n);
        set_instrument_param(i, param, v);
    }
}

// ----- audio thread --------------------------------------------------------------

/// Smoothing per frame of chain gains (about 5 ms at 48 kHz).
const GAIN_SMOOTH: f32 = 0.004;

/// One chain of an Audio Effect Rack on the audio thread.
struct ChainRt {
    id: Id,
    mixer: ChainMixer,
    select: Zone,
    fx: FxChain,
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    gain_l: f32,
    gain_r: f32,
}

/// The audio-thread side of an Audio Effect Rack. Built on the UI thread.
pub struct RackRuntime {
    chains: Vec<ChainRt>,
    mappings: Vec<MacroMapping>,
    /// Macro values the mappings were last applied for (NaN = never).
    applied: [f32; MACROS],
    dry_l: Vec<f32>,
    dry_r: Vec<f32>,
}

impl RackRuntime {
    /// Allocates; UI thread only.
    pub fn new(rack: &Rack, sample_rate: f32) -> Self {
        Self {
            chains: rack
                .chains
                .iter()
                .take(MAX_CHAINS)
                .map(|c| ChainRt {
                    id: c.id,
                    mixer: c.mixer,
                    select: c.zones.select,
                    fx: FxChain::new(&c.devices, sample_rate),
                    buf_l: vec![0.0; MAX_BLOCK],
                    buf_r: vec![0.0; MAX_BLOCK],
                    gain_l: 0.0,
                    gain_r: 0.0,
                })
                .collect(),
            mappings: rack.mappings.clone(),
            applied: [f32::NAN; MACROS],
            dry_l: vec![0.0; MAX_BLOCK],
            dry_r: vec![0.0; MAX_BLOCK],
        }
    }

    /// Takes over the DSP state of chains that also exist in `old`.
    /// Real-time safe.
    pub fn adopt(&mut self, old: &mut RackRuntime) {
        for c in self.chains.iter_mut() {
            if let Some(o) = old.chains.iter_mut().find(|o| o.id == c.id) {
                c.fx.adopt(&mut o.fx);
                c.gain_l = o.gain_l;
                c.gain_r = o.gain_r;
            }
        }
    }

    pub fn reset(&mut self) {
        for c in self.chains.iter_mut() {
            c.fx.reset();
        }
    }

    /// Applies the macro mappings when the macros moved.
    fn apply_macros(&mut self, p: &RackParams) {
        let macros = p.macros();
        if macros == self.applied {
            return;
        }
        self.applied = macros;
        for i in 0..self.mappings.len() {
            let m = self.mappings[i];
            let n = m.position(&macros);
            match m.target {
                MacroTarget::Device { device, param } => {
                    for c in self.chains.iter_mut() {
                        if c.fx.set_param_unit(device, param as usize, n) {
                            break;
                        }
                    }
                }
                MacroTarget::ChainVolume(id) => {
                    if let Some(c) = self.chains.iter_mut().find(|c| c.id == id) {
                        c.mixer.volume_db = chain_volume_db(n);
                    }
                }
                MacroTarget::ChainPan(id) => {
                    if let Some(c) = self.chains.iter_mut().find(|c| c.id == id) {
                        c.mixer.pan = n * 2.0 - 1.0;
                    }
                }
                MacroTarget::Instrument { .. } => {}
            }
        }
    }

    pub fn process(&mut self, p: &RackParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        self.apply_macros(p);
        if self.chains.is_empty() {
            return;
        }
        let mut done = 0;
        while done < left.len() {
            let n = (left.len() - done).min(MAX_BLOCK);
            self.process_block(p, &mut left[done..done + n], &mut right[done..done + n], ctx);
            done += n;
        }
    }

    fn process_block(&mut self, p: &RackParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let n = left.len();
        self.dry_l[..n].copy_from_slice(left);
        self.dry_r[..n].copy_from_slice(right);
        left.fill(0.0);
        right.fill(0.0);
        let any_solo = self.chains.iter().any(|c| c.mixer.solo);
        let selector = p.chain_select;
        for c in self.chains.iter_mut() {
            let audible = !c.mixer.mute && (!any_solo || c.mixer.solo);
            let g = if audible {
                db_to_gain(c.mixer.volume_db) * c.select.gain(selector)
            } else {
                0.0
            };
            let (pl, pr) = pan_gains(c.mixer.pan);
            let (tl, tr) = (g * pl, g * pr);
            if tl == 0.0 && tr == 0.0 && c.gain_l < 1e-5 && c.gain_r < 1e-5 {
                // Silent chain: skip it (its effects pause).
                c.gain_l = 0.0;
                c.gain_r = 0.0;
                continue;
            }
            let (bl, br) = (&mut c.buf_l[..n], &mut c.buf_r[..n]);
            bl.copy_from_slice(&self.dry_l[..n]);
            br.copy_from_slice(&self.dry_r[..n]);
            c.fx.process(bl, br, ctx);
            let (mut gl, mut gr) = (c.gain_l, c.gain_r);
            for i in 0..n {
                gl += (tl - gl) * GAIN_SMOOTH;
                gr += (tr - gr) * GAIN_SMOOTH;
                left[i] += bl[i] * gl;
                right[i] += br[i] * gr;
            }
            if (tl - gl).abs() < 1e-6 {
                gl = tl;
            }
            if (tr - gr).abs() < 1e-6 {
                gr = tr;
            }
            c.gain_l = gl;
            c.gain_r = gr;
        }
    }

    pub fn set_chain_mixer(&mut self, chain: Id, mixer: ChainMixer) -> bool {
        if let Some(c) = self.chains.iter_mut().find(|c| c.id == chain) {
            c.mixer = mixer;
            return true;
        }
        self.chains.iter_mut().any(|c| c.fx.set_chain_mixer(chain, mixer))
    }

    pub fn set_chain_zones(&mut self, chain: Id, zones: ChainZones) -> bool {
        if let Some(c) = self.chains.iter_mut().find(|c| c.id == chain) {
            c.select = zones.select;
            return true;
        }
        self.chains.iter_mut().any(|c| c.fx.set_chain_zones(chain, zones))
    }

    /// Chains, for the device search in [`FxChain`].
    pub(super) fn chains_mut(&mut self) -> impl Iterator<Item = &mut FxChain> {
        self.chains.iter_mut().map(|c| &mut c.fx)
    }

    pub(super) fn chains(&self) -> impl Iterator<Item = &FxChain> {
        self.chains.iter().map(|c| &c.fx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::{DeviceKind, DeviceParams};

    #[test]
    fn zones_fade_at_their_edges() {
        let z = Zone {
            lo: 10,
            hi: 20,
            fade_lo: 3,
            fade_hi: 0,
        };
        assert_eq!(z.gain(9.0), 0.0);
        assert!((z.gain(10.0) - 0.25).abs() < 1e-6);
        assert!((z.gain(12.0) - 0.75).abs() < 1e-6);
        assert_eq!(z.gain(13.0), 1.0);
        assert_eq!(z.gain(20.0), 1.0);
        assert_eq!(z.gain(21.0), 0.0);
        assert_eq!(Zone::FULL.gain(0.0), 1.0);
        assert_eq!(Zone::FULL.gain(127.0), 1.0);
        let n = Zone {
            lo: 50,
            hi: 40,
            fade_lo: 30,
            fade_hi: 30,
        }
        .normalized();
        assert_eq!((n.lo, n.hi, n.fade_lo, n.fade_hi), (50, 50, 0, 0));
    }

    fn utility_gain_rack(gain_db: f32) -> (Rack, Id) {
        let mut d = Device::new(10, DeviceKind::Utility);
        d.params.set(0, gain_db);
        let mut c = RackChain::new(2, "Chain");
        c.devices.push(d);
        (Rack::with_chain(c), 10)
    }

    #[test]
    fn parallel_chains_sum_and_the_selector_crossfades() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let (mut rack, _) = utility_gain_rack(0.0);
        let mut b = RackChain::new(3, "B");
        b.zones.select = Zone::range(64, 127);
        rack.chains.push(b);
        rack.chains[0].zones.select = Zone::range(0, 63);
        let mut rt = RackRuntime::new(&rack, sr);
        let mut p = RackParams::default();
        // Selector at 0: only chain A (the utility at 0 dB) plays.
        let mut l = vec![0.5f32; 4096];
        let mut r = l.clone();
        rt.process(&p, &mut l, &mut r, &ctx);
        assert!((l[4000] - 0.5).abs() < 1e-3, "{}", l[4000]);
        // Both chains audible: dry chain B adds a second copy.
        rack.chains[1].zones.select = Zone::FULL;
        rack.chains[0].zones.select = Zone::FULL;
        let mut rt2 = RackRuntime::new(&rack, sr);
        rt2.adopt(&mut rt);
        let mut l = vec![0.5f32; 4096];
        let mut r = l.clone();
        rt2.process(&p, &mut l, &mut r, &ctx);
        assert!((l[4000] - 1.0).abs() < 1e-3, "{}", l[4000]);
        // Selector 100 with the original zones: only B.
        p.chain_select = 100.0;
        rack.chains[0].zones.select = Zone::range(0, 63);
        rack.chains[1].zones.select = Zone::range(64, 127);
        let mut rt3 = RackRuntime::new(&rack, sr);
        rt3.adopt(&mut rt2);
        let mut l = vec![0.5f32; 8192];
        let mut r = l.clone();
        rt3.process(&p, &mut l, &mut r, &ctx);
        assert!((l[8000] - 0.5).abs() < 1e-3);
        // Mute B: silence.
        assert!(rt3.set_chain_mixer(
            3,
            ChainMixer {
                mute: true,
                ..Default::default()
            }
        ));
        let mut l = vec![0.5f32; 8192];
        let mut r = l.clone();
        rt3.process(&p, &mut l, &mut r, &ctx);
        assert!(l[8000].abs() < 1e-3);
    }

    #[test]
    fn macros_move_mapped_parameters_in_the_document_and_the_engine() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let (mut rack, dev) = utility_gain_rack(0.0);
        rack.map(0, MacroTarget::Device { device: dev, param: 0 });
        let info = crate::fx::utility::UtilityParams::INFO[0];
        // Macro at 0 puts the gain at its minimum.
        let mut macros = [0.0; MACROS];
        rack.apply_macros(&macros);
        let d = rack.find_device(dev).unwrap();
        assert_eq!(d.params.get(0), info.min);
        macros[0] = 1.0;
        rack.apply_macros(&macros);
        assert_eq!(rack.find_device(dev).unwrap().params.get(0), info.max);

        let mut rt = RackRuntime::new(&rack, sr);
        let p = RackParams::default();
        let mut l = vec![0.5f32; 4096];
        let mut r = l.clone();
        rt.process(&p, &mut l, &mut r, &ctx);
        // Minimum gain of the utility is very quiet.
        assert!(l[4000].abs() < 0.05, "{}", l[4000]);

        // Volume and pan targets, inverted range.
        rack.map(1, MacroTarget::ChainVolume(2));
        rack.mappings.last_mut().unwrap().min = 1.0;
        rack.mappings.last_mut().unwrap().max = 0.0;
        macros[1] = 1.0;
        rack.apply_macros(&macros);
        assert_eq!(rack.chains[0].mixer.volume_db, -70.0);
        rack.unmap(MacroTarget::ChainVolume(2));
        assert_eq!(rack.mappings.len(), 1);
        rack.chains[0].devices.clear();
        rack.prune_mappings();
        assert!(rack.mappings.is_empty());
        let DeviceParams::Rack(_) = DeviceKind::AudioRack.default_params() else {
            panic!()
        };
    }

    #[test]
    fn racks_round_trip_and_nest() {
        let (mut rack, _) = utility_gain_rack(-3.0);
        let inner = Device::new_rack(20, Rack::with_chain(RackChain::new(21, "Inner")));
        rack.chains[0].devices.push(inner);
        rack.find_chain_mut(21)
            .unwrap()
            .devices
            .push(Device::new(22, DeviceKind::Delay));
        assert!(rack.find_device(22).is_some());
        let d = Device::new_rack(1, rack);
        let json = serde_json::to_string(&d).unwrap();
        let back: Device = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
        let mut ids = Vec::new();
        back.visit(&mut |d| ids.push(d.id));
        assert_eq!(ids, vec![1, 10, 20, 22]);
        // Plain devices do not write a rack field.
        let plain = serde_json::to_string(&Device::new(5, DeviceKind::Eq)).unwrap();
        assert!(!plain.contains("rack"));
    }
}
