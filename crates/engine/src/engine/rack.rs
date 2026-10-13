//! Instrument Racks and Drum Rack pad chains on the audio thread.
//!
//! An [`EngineRack`] is compiled from a track's [`Rack`] on the UI thread.
//! For an **Instrument Rack** every chain owns an instrument, its samples and
//! an effect chain; notes go to the chains whose key, velocity and select
//! zones contain them, and the chain outputs are summed into the track. For
//! a **Drum Rack** the pads themselves are rendered by the track's
//! [`crate::dsp::drum_rack::DrumRack`]; pads that have a chain render into
//! their own buffer ([`PadBus`]), run through the chain's effects and sends,
//! and are then mixed into the track together with the rack's return chains.

use std::sync::Arc;

use crate::automation::fader_pos_to_db;
use crate::dsp::drum_rack::{PadBus, PADS};
use crate::dsp::sample::SampleSlots;
use crate::dsp::InstrumentRuntime;
use crate::fx::rack::{
    set_unit_instrument_param, ChainMixer, ChainZones, MacroMapping, MacroTarget, Rack, RackChain, RackParams, MACROS,
    MAX_CHAINS, MAX_RACK_RETURNS,
};
use crate::fx::{FxChain, FxContext};
use crate::model::{db_to_gain, pan_gains, AudioData, Id, Instrument};
use crate::MAX_BLOCK;

/// Chain gain smoothing per frame.
const SMOOTH: f32 = 0.004;

/// What kind of rack a track carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RackKind {
    Instrument,
    Drum,
}

struct EngineChain {
    id: Id,
    mixer: ChainMixer,
    zones: ChainZones,
    instrument: Instrument,
    inst: InstrumentRuntime,
    samples: Vec<Option<Arc<AudioData>>>,
    fx: FxChain,
    /// Linear send gains to the rack's returns.
    sends: [f32; MAX_RACK_RETURNS],
    buf_l: Vec<f32>,
    buf_r: Vec<f32>,
    gain_l: f32,
    gain_r: f32,
}

impl EngineChain {
    fn new(c: &RackChain, instruments: bool, sample_rate: f32) -> Self {
        let instrument = if instruments { c.instrument } else { Instrument::None };
        let mut sends = [0.0; MAX_RACK_RETURNS];
        for (o, db) in sends.iter_mut().zip(c.sends.iter()) {
            *o = db_to_gain(*db);
        }
        Self {
            id: c.id,
            mixer: c.mixer,
            zones: c.zones,
            instrument,
            inst: InstrumentRuntime::new(&instrument, sample_rate),
            samples: c
                .samples
                .iter()
                .map(|s| s.as_ref().and_then(|s| s.data.clone()))
                .collect(),
            fx: FxChain::new(&c.devices, sample_rate),
            sends,
            buf_l: vec![0.0; MAX_BLOCK],
            buf_r: vec![0.0; MAX_BLOCK],
            gain_l: 0.0,
            gain_r: 0.0,
        }
    }

    /// Target (left, right) gains: volume, pan, mute/solo and an extra level.
    #[inline]
    fn targets(&self, any_solo: bool, level: f32) -> (f32, f32) {
        if self.mixer.mute || (any_solo && !self.mixer.solo) {
            return (0.0, 0.0);
        }
        let g = db_to_gain(self.mixer.volume_db) * level;
        let (pl, pr) = pan_gains(self.mixer.pan);
        (g * pl, g * pr)
    }

    /// Adds `buf[..n]` to `out` with gains ramped towards `target`.
    #[inline]
    fn mix_into(&mut self, target: (f32, f32), out_l: &mut [f32], out_r: &mut [f32], from: usize) {
        let n = out_l.len();
        let (mut gl, mut gr) = (self.gain_l, self.gain_r);
        for i in 0..n {
            gl += (target.0 - gl) * SMOOTH;
            gr += (target.1 - gr) * SMOOTH;
            out_l[i] += self.buf_l[from + i] * gl;
            out_r[i] += self.buf_r[from + i] * gr;
        }
        if (target.0 - gl).abs() < 1e-6 {
            gl = target.0;
        }
        if (target.1 - gr).abs() < 1e-6 {
            gr = target.1;
        }
        self.gain_l = gl;
        self.gain_r = gr;
    }
}

/// A track's rack on the audio thread.
pub struct EngineRack {
    pub kind: RackKind,
    chains: Vec<EngineChain>,
    returns: Vec<EngineChain>,
    mappings: Vec<MacroMapping>,
    applied: [f32; MACROS],
    /// Chain selector at the last note-on.
    selector: f32,
    /// Drum Rack pad buffers; chain `n` belongs to pad `n`.
    pads: Option<Box<PadBus>>,
}

impl EngineRack {
    /// Compiles a track's rack for its instrument (Instrument Rack or Drum
    /// Rack); `None` for other instruments. Allocates; UI thread only.
    pub fn new(rack: &Rack, instrument: &Instrument, sample_rate: f32) -> Option<Self> {
        let kind = match instrument {
            Instrument::Rack(_) => RackKind::Instrument,
            Instrument::DrumRack(_) => RackKind::Drum,
            _ => return None,
        };
        let instruments = kind == RackKind::Instrument;
        let chains: Vec<EngineChain> = rack
            .chains
            .iter()
            .take(if instruments { MAX_CHAINS } else { PADS })
            .map(|c| EngineChain::new(c, instruments, sample_rate))
            .collect();
        let returns = rack
            .returns
            .iter()
            .take(MAX_RACK_RETURNS)
            .map(|c| EngineChain::new(c, false, sample_rate))
            .collect();
        let pads = (kind == RackKind::Drum).then(|| {
            let routed = (0..chains.len()).fold(0u16, |m, i| m | (1 << i));
            Box::new(PadBus::new(routed))
        });
        Some(Self {
            kind,
            chains,
            returns,
            mappings: rack.mappings.clone(),
            applied: [f32::NAN; MACROS],
            selector: 0.0,
            pads,
        })
    }

    /// Takes over voices, tails and gain ramps of chains that also exist in
    /// `old`. Real-time safe.
    pub fn adopt(&mut self, old: &mut EngineRack) {
        let pairs = self
            .chains
            .iter_mut()
            .zip(std::iter::repeat(false))
            .chain(self.returns.iter_mut().zip(std::iter::repeat(true)));
        for (c, is_return) in pairs {
            let list = if is_return { &mut old.returns } else { &mut old.chains };
            if let Some(o) = list.iter_mut().find(|o| o.id == c.id) {
                c.fx.adopt(&mut o.fx);
                if o.inst.kind_tag() == c.instrument.kind_tag() {
                    std::mem::swap(&mut o.inst, &mut c.inst);
                }
                c.gain_l = o.gain_l;
                c.gain_r = o.gain_r;
            }
        }
        if self.kind == old.kind {
            self.selector = old.selector;
        }
    }

    // ----- notes (Instrument Rack) ---------------------------------------------

    pub fn note_on(&mut self, pitch: u8, velocity: u8) {
        if self.kind != RackKind::Instrument {
            return;
        }
        let selector = self.selector;
        for c in self.chains.iter_mut() {
            if c.mixer.mute {
                continue;
            }
            // The select zone decides which chains take the note; its fades
            // act on the chain output (see `render_instruments`).
            if c.zones.select.gain(selector) <= 0.0 {
                continue;
            }
            let g = c.zones.key.gain(pitch as f32) * c.zones.velocity.gain(velocity as f32);
            if g > 0.0 {
                let v = (velocity as f32 * g).round().clamp(1.0, 127.0) as u8;
                c.inst.note_on(pitch, v);
            }
        }
    }

    pub fn note_off(&mut self, pitch: u8) {
        for c in self.chains.iter_mut() {
            c.inst.note_off(pitch);
        }
    }

    pub fn all_notes_off(&mut self) {
        for c in self.chains.iter_mut() {
            c.inst.all_notes_off();
        }
    }

    /// Silences voices and tails immediately.
    pub fn panic(&mut self) {
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            c.inst.panic();
            c.fx.reset();
        }
    }

    // ----- macros -----------------------------------------------------------------

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
                    for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
                        if c.fx.set_param_unit(device, param as usize, n) {
                            break;
                        }
                    }
                }
                MacroTarget::Instrument { chain, param } => {
                    if let Some(c) = self.chains.iter_mut().find(|c| c.id == chain) {
                        set_unit_instrument_param(&mut c.instrument, param, n);
                    }
                }
                MacroTarget::ChainVolume(chain) => {
                    if let Some(c) = self.chain_mut(chain) {
                        c.mixer.volume_db = fader_pos_to_db(n);
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

    fn chain_mut(&mut self, id: Id) -> Option<&mut EngineChain> {
        self.chains
            .iter_mut()
            .chain(self.returns.iter_mut())
            .find(|c| c.id == id)
    }

    // ----- rendering --------------------------------------------------------------

    /// Instrument Rack: renders every chain over `left.len()` frames and adds
    /// the result to `left`/`right`.
    pub fn render_instruments(&mut self, p: &RackParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        self.apply_macros(p);
        self.selector = p.chain_select;
        let n = left.len();
        let any_solo = self.chains.iter().any(|c| c.mixer.solo);
        let selector = p.chain_select;
        for c in self.chains.iter_mut() {
            let level = c.zones.select.gain(selector);
            let target = c.targets(any_solo, level);
            c.buf_l[..n].fill(0.0);
            c.buf_r[..n].fill(0.0);
            let EngineChain {
                inst,
                instrument,
                samples,
                buf_l,
                buf_r,
                fx,
                ..
            } = c;
            let slots: &SampleSlots = samples;
            inst.render(instrument, slots, &mut buf_l[..n], &mut buf_r[..n]);
            fx.process(&mut buf_l[..n], &mut buf_r[..n], ctx);
            c.mix_into(target, left, right, 0);
        }
    }

    /// Drum Rack: the pad buffers the drum voices write into for frames
    /// starting at `offset`.
    pub fn pad_bus(&mut self, offset: usize) -> Option<&mut PadBus> {
        let b = self.pads.as_deref_mut()?;
        b.offset = offset;
        Some(b)
    }

    /// Drum Rack: clears the pad buffers before a segment.
    pub fn begin_pads(&mut self, frames: usize) {
        if let Some(b) = self.pads.as_deref_mut() {
            b.clear(frames);
        }
    }

    /// Drum Rack: runs the pads' chains and the returns over the segment's
    /// pad buffers and adds them to the track buffer.
    pub fn finish_pads(&mut self, p: Option<&RackParams>, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        if let Some(p) = p {
            self.apply_macros(p);
        }
        let Some(bus) = self.pads.as_deref_mut() else { return };
        let n = left.len();
        for r in self.returns.iter_mut() {
            r.buf_l[..n].fill(0.0);
            r.buf_r[..n].fill(0.0);
        }
        let any_solo = self.chains.iter().any(|c| c.mixer.solo);
        let returns = &mut self.returns;
        for (pad, c) in self.chains.iter_mut().enumerate() {
            let (bl, br) = (&mut bus.left[pad][..n], &mut bus.right[pad][..n]);
            c.fx.process(bl, br, ctx);
            // Post-fader sends to the returns.
            let target = c.targets(any_solo, 1.0);
            let (mut gl, mut gr) = (c.gain_l, c.gain_r);
            let mono_target = 0.5 * (target.0 + target.1);
            for (ri, r) in returns.iter_mut().enumerate() {
                let s = c.sends[ri];
                if s <= 0.0 {
                    continue;
                }
                let g = s * mono_target;
                for i in 0..n {
                    r.buf_l[i] += bl[i] * g;
                    r.buf_r[i] += br[i] * g;
                }
            }
            for i in 0..n {
                gl += (target.0 - gl) * SMOOTH;
                gr += (target.1 - gr) * SMOOTH;
                left[i] += bl[i] * gl;
                right[i] += br[i] * gr;
            }
            if (target.0 - gl).abs() < 1e-6 {
                gl = target.0;
            }
            if (target.1 - gr).abs() < 1e-6 {
                gr = target.1;
            }
            c.gain_l = gl;
            c.gain_r = gr;
        }
        let any_solo = returns.iter().any(|c| c.mixer.solo);
        for r in returns.iter_mut() {
            let (bl, br) = (&mut r.buf_l[..n], &mut r.buf_r[..n]);
            r.fx.process(bl, br, ctx);
            let target = r.targets(any_solo, 1.0);
            r.mix_into(target, left, right, 0);
        }
    }

    // ----- parameter changes from the UI --------------------------------------------

    /// Runs `f` on each chain's effects until it returns true.
    pub fn any_fx(&mut self, mut f: impl FnMut(&mut FxChain) -> bool) -> bool {
        self.chains
            .iter_mut()
            .chain(self.returns.iter_mut())
            .any(|c| f(&mut c.fx))
    }

    /// Forwards automated device values to the chains' effects.
    pub fn for_each_fx(&mut self, mut f: impl FnMut(&mut FxChain)) {
        for c in self.chains.iter_mut().chain(self.returns.iter_mut()) {
            f(&mut c.fx);
        }
    }

    pub fn set_chain_instrument(&mut self, chain: Id, instrument: Instrument) -> bool {
        match self.chains.iter_mut().find(|c| c.id == chain) {
            Some(c) => {
                if c.instrument.kind_tag() == instrument.kind_tag() && self.kind == RackKind::Instrument {
                    c.instrument = instrument;
                }
                true
            }
            None => false,
        }
    }

    pub fn set_chain_mixer(&mut self, chain: Id, mixer: ChainMixer) -> bool {
        if let Some(c) = self.chain_mut(chain) {
            c.mixer = mixer;
            return true;
        }
        self.any_fx(|fx| fx.set_chain_mixer(chain, mixer))
    }

    pub fn set_chain_zones(&mut self, chain: Id, zones: ChainZones) -> bool {
        if let Some(c) = self.chain_mut(chain) {
            c.zones = zones;
            return true;
        }
        self.any_fx(|fx| fx.set_chain_zones(chain, zones))
    }

    pub fn set_chain_send(&mut self, chain: Id, index: usize, db: f32) -> bool {
        match self.chains.iter_mut().find(|c| c.id == chain) {
            Some(c) => {
                if let Some(s) = c.sends.get_mut(index) {
                    *s = db_to_gain(db);
                }
                true
            }
            None => false,
        }
    }

    /// Every effect device in the rack's chains (for meters).
    pub fn visit_fx(&self, f: &mut dyn FnMut(&crate::fx::chain::EngineDevice)) {
        for c in self.chains.iter().chain(self.returns.iter()) {
            c.fx.visit(f);
        }
    }
}
