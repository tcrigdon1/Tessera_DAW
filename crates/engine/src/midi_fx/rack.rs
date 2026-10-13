//! MIDI Effect Rack: parallel chains of MIDI effects. Every chain gets the
//! notes inside its key, velocity and chain-select zones (velocity scaled by
//! the zone fades), runs them through its own effects, and the chains'
//! output is merged. The rack's macros move mapped parameters of the
//! effects inside it.
//!
//! The rack's chains live in the track's `midi_rack`; the rack device in
//! the track's MIDI chain marks where they run.

use super::{EventKind, MidiDevice, MidiEvent, MidiFxChain, MidiFxContext, MAX_MIDI_EVENTS};
use crate::fx::rack::{ChainMixer, ChainZones, MacroMapping, MacroTarget, Rack, RackParams, MACROS};
use crate::model::Id;

struct Chain {
    id: Id,
    mixer: ChainMixer,
    zones: ChainZones,
    fx: MidiFxChain,
    /// Notes this chain let through, so their note-offs follow them.
    held: [bool; 128],
    buf: Vec<MidiEvent>,
}

/// The audio-thread side of a MIDI Effect Rack.
#[derive(Default)]
pub struct MidiRack {
    chains: Vec<Chain>,
    mappings: Vec<MacroMapping>,
    applied: Option<[f32; MACROS]>,
}

impl MidiRack {
    /// Builds the chains. UI thread (allocates).
    pub fn new(rack: &Rack, sample_rate: f32) -> Self {
        Self {
            chains: rack
                .chains
                .iter()
                .map(|c| Chain {
                    id: c.id,
                    mixer: c.mixer,
                    zones: c.zones,
                    fx: MidiFxChain::new(&c.midi_devices, sample_rate),
                    held: [false; 128],
                    buf: Vec::with_capacity(MAX_MIDI_EVENTS),
                })
                .collect(),
            mappings: rack.mappings.clone(),
            applied: None,
        }
    }

    /// Takes over held notes and effect state chain by chain.
    pub fn adopt(&mut self, old: &mut MidiRack) {
        for c in self.chains.iter_mut() {
            if let Some(o) = old.chains.iter_mut().find(|o| o.id == c.id) {
                c.fx.adopt(&mut o.fx);
                c.held = o.held;
            }
        }
    }

    pub fn reset(&mut self) {
        for c in self.chains.iter_mut() {
            c.fx.reset();
            c.held = [false; 128];
        }
    }

    pub(super) fn chains_mut(&mut self) -> impl Iterator<Item = &mut MidiFxChain> {
        self.chains.iter_mut().map(|c| &mut c.fx)
    }

    pub fn set_chain_mixer(&mut self, chain: Id, mixer: ChainMixer) -> bool {
        match self.chains.iter_mut().find(|c| c.id == chain) {
            Some(c) => {
                c.mixer = mixer;
                true
            }
            None => false,
        }
    }

    pub fn set_chain_zones(&mut self, chain: Id, zones: ChainZones) -> bool {
        match self.chains.iter_mut().find(|c| c.id == chain) {
            Some(c) => {
                c.zones = zones;
                true
            }
            None => false,
        }
    }

    fn apply_macros(&mut self, macros: &[f32; MACROS]) {
        if self.applied.as_ref() == Some(macros) {
            return;
        }
        self.applied = Some(*macros);
        for m in self.mappings.iter() {
            if let MacroTarget::Device { device, param } = m.target {
                let n = m.position(macros);
                for c in self.chains.iter_mut() {
                    if c.fx.set_param_unit(device, param as usize, n) {
                        break;
                    }
                }
            }
        }
    }

    pub(super) fn process(
        &mut self,
        p: &RackParams,
        input: &[MidiEvent],
        out: &mut Vec<MidiEvent>,
        ctx: &MidiFxContext,
    ) {
        self.apply_macros(&p.macros());
        let any_solo = self.chains.iter().any(|c| c.mixer.solo);
        let select = p.chain_select;
        for c in self.chains.iter_mut() {
            let active = !c.mixer.mute && (!any_solo || c.mixer.solo);
            let select_gain = c.zones.select.gain(select);
            c.buf.clear();
            for e in input {
                let kind = match e.kind {
                    EventKind::On(pitch, vel) => {
                        let g = c.zones.key.gain(pitch as f32) * c.zones.velocity.gain(vel as f32) * select_gain;
                        if !active || g <= 0.0 {
                            continue;
                        }
                        c.held[pitch as usize & 127] = true;
                        EventKind::On(pitch, ((vel as f32 * g).round() as u8).clamp(1, 127))
                    }
                    EventKind::Off(pitch) => {
                        let held = &mut c.held[pitch as usize & 127];
                        if !std::mem::take(held) {
                            continue;
                        }
                        EventKind::Off(pitch)
                    }
                    EventKind::AllOff => {
                        c.held = [false; 128];
                        EventKind::AllOff
                    }
                };
                super::emit(&mut c.buf, e.frame as usize, kind);
            }
            c.fx.process(&mut c.buf, ctx);
            let room = out.capacity() - out.len();
            out.extend_from_slice(&c.buf[..c.buf.len().min(room)]);
        }
        out.sort_unstable_by_key(|e| e.order());
    }
}

/// The chains a new MIDI Effect Rack starts with: one holding `devices`.
pub fn wrap(chain: Id, devices: Vec<MidiDevice>) -> Rack {
    let mut c = crate::fx::rack::RackChain::new(chain, "Chain 1");
    c.midi_devices = devices;
    Rack::with_chain(c)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{buf, ctx, off, on};
    use super::super::{MidiDeviceKind, MidiDeviceParams};
    use super::*;
    use crate::fx::rack::{RackChain, Zone};

    fn rack() -> Rack {
        let mut low = RackChain::new(1, "Low");
        low.zones.key = Zone::range(0, 59);
        let mut high = RackChain::new(2, "High");
        high.zones.key = Zone::range(60, 127);
        // The high chain transposes up an octave.
        let mut pitch = MidiDevice::new(10, MidiDeviceKind::Pitch);
        pitch.params.set(0, 12.0);
        high.midi_devices.push(pitch);
        let mut r = Rack::with_chain(low);
        r.chains.push(high);
        r
    }

    #[test]
    fn key_zones_split_notes_between_chains() {
        let mut m = MidiRack::new(&rack(), 48000.0);
        let mut out = buf();
        m.process(
            &RackParams::default(),
            &[on(0, 40, 100), on(10, 70, 100)],
            &mut out,
            &ctx(64),
        );
        let ons: Vec<_> = out
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::On(p, _) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(ons, vec![40, 82]);
        // Note-offs follow the chain that played the note.
        out.clear();
        m.process(&RackParams::default(), &[off(0, 40), off(0, 70)], &mut out, &ctx(64));
        let offs: Vec<_> = out
            .iter()
            .filter_map(|e| match e.kind {
                EventKind::Off(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(offs.len(), 2);
        assert!(offs.contains(&40) && offs.contains(&82));
    }

    #[test]
    fn mute_solo_and_selector() {
        let mut r = rack();
        r.chains[1].mixer.solo = true;
        let mut m = MidiRack::new(&r, 48000.0);
        let mut out = buf();
        m.process(
            &RackParams::default(),
            &[on(0, 40, 100), on(0, 70, 100)],
            &mut out,
            &ctx(64),
        );
        assert_eq!(out.len(), 1);

        let mut r = rack();
        r.chains[0].zones.key = Zone::FULL;
        r.chains[0].zones.select = Zone::range(0, 63);
        r.chains[1].zones.key = Zone::FULL;
        r.chains[1].zones.select = Zone::range(64, 127);
        let mut m = MidiRack::new(&r, 48000.0);
        let mut p = RackParams::default();
        p.chain_select = 100.0;
        out.clear();
        m.process(&p, &[on(0, 40, 100)], &mut out, &ctx(64));
        assert!(matches!(out[0].kind, EventKind::On(52, _)));
    }

    #[test]
    fn macros_move_mapped_parameters() {
        let mut r = rack();
        r.chains[1].zones.key = Zone::FULL;
        r.chains[0].mixer.mute = true;
        r.map(0, MacroTarget::Device { device: 10, param: 0 });
        let mut m = MidiRack::new(&r, 48000.0);
        let mut p = RackParams::default();
        let info = MidiDeviceParams::Pitch(Default::default()).info()[0];
        p.set(0, info.to_unit(-12.0));
        let mut out = buf();
        m.process(&p, &[on(0, 60, 100)], &mut out, &ctx(64));
        assert!(matches!(out[0].kind, EventKind::On(48, _)), "{:?}", out[0].kind);
    }
}
