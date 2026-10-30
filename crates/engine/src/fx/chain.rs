//! The audio-thread side of a device chain.

use std::sync::Arc;

use super::rack::{ChainMixer, ChainZones, Rack, RackRuntime};
use super::{BuiltinRuntime, Device, DeviceParams};
use crate::model::Id;
use crate::plugin::{self, PluginBlock, PluginProcessor};

/// Per-block information effects may need.
#[derive(Debug, Clone, Copy)]
pub struct FxContext {
    pub sample_rate: f32,
    pub tempo: f64,
    /// Transport state for plug-ins: playing, song position at the block
    /// start in beats, and the time signature numerator.
    pub playing: bool,
    pub song_beat: f64,
    pub beats_per_bar: u32,
    /// Rendering faster than real time (export): plug-ins may take as long
    /// as they need.
    pub offline: bool,
}

impl FxContext {
    pub fn new(sample_rate: f32, tempo: f64) -> Self {
        Self {
            sample_rate,
            tempo,
            playing: false,
            song_beat: 0.0,
            beats_per_bar: 4,
            offline: false,
        }
    }

    /// Adds transport information.
    pub fn with_transport(mut self, playing: bool, song_beat: f64, beats_per_bar: u32, offline: bool) -> Self {
        self.playing = playing;
        self.song_beat = song_beat;
        self.beats_per_bar = beats_per_bar;
        self.offline = offline;
        self
    }
}

/// DSP state of one device.
enum Runtime {
    Builtin(BuiltinRuntime),
    /// An Audio Effect Rack.
    Rack(Box<RackRuntime>),
    /// A hosted plug-in, if one is registered for the device.
    Plugin(Option<Arc<dyn PluginProcessor>>),
}

impl Runtime {
    fn new(d: &Device, sr: f32) -> Self {
        if let Some(rt) = BuiltinRuntime::new(d.id, &d.params, sr) {
            return Runtime::Builtin(rt);
        }
        match &d.params {
            DeviceParams::Rack(_) => Runtime::Rack(Box::new(match d.rack.as_deref() {
                Some(r) => RackRuntime::new(r, sr),
                None => RackRuntime::new(&Rack::default(), sr),
            })),
            _ => Runtime::Plugin(plugin::lookup(d.id)),
        }
    }

    fn same_kind(&self, params: &DeviceParams) -> bool {
        match (self, params) {
            (Runtime::Builtin(b), p) => b.same_kind(p),
            (Runtime::Rack(_), DeviceParams::Rack(_)) => true,
            (Runtime::Plugin(_), DeviceParams::Plugin(_)) => true,
            _ => false,
        }
    }

    /// Takes over state from `old` (same kind). Racks adopt chain by chain
    /// so a rebuilt rack keeps its tails.
    fn adopt(&mut self, old: &mut Runtime) {
        match (self, old) {
            (Runtime::Rack(new), Runtime::Rack(old)) => new.adopt(old),
            (new, old) => std::mem::swap(new, old),
        }
    }

    fn process(&mut self, params: &DeviceParams, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        match (self, params) {
            (Runtime::Builtin(d), p) => d.process(p, l, r, ctx),
            (Runtime::Rack(d), DeviceParams::Rack(p)) => d.process(p, l, r, ctx),
            (Runtime::Plugin(Some(p)), DeviceParams::Plugin(_)) => {
                // A plug-in that cannot process passes the audio through.
                p.process(&mut PluginBlock {
                    left: l,
                    right: r,
                    notes: &[],
                    ctx,
                });
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        match self {
            Runtime::Builtin(d) => d.reset(),
            Runtime::Rack(d) => d.reset(),
            Runtime::Plugin(p) => {
                if let Some(p) = p {
                    p.reset();
                }
            }
        }
    }

    /// Gain reduction in dB for dynamics devices.
    fn meter(&self) -> Option<f32> {
        match self {
            Runtime::Builtin(d) => d.meter(),
            _ => None,
        }
    }
}

/// One device on the audio thread.
pub struct EngineDevice {
    pub id: Id,
    pub on: bool,
    pub params: DeviceParams,
    rt: Runtime,
}

impl EngineDevice {
    /// Gain reduction in dB for compressors and limiters (0 when off).
    pub fn meter(&self) -> f32 {
        if self.on {
            self.rt.meter().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    /// Whether the device publishes a gain reduction meter.
    pub fn has_meter(&self) -> bool {
        self.rt.meter().is_some()
    }

    /// The chains of a rack device.
    fn rack_mut(&mut self) -> Option<&mut RackRuntime> {
        match &mut self.rt {
            Runtime::Rack(r) => Some(r),
            _ => None,
        }
    }
}

/// A chain of insert effects processed in series.
#[derive(Default)]
pub struct FxChain {
    pub devices: Vec<EngineDevice>,
}

impl FxChain {
    /// Builds a chain with fresh DSP state. Allocates; UI thread only.
    pub fn new(devices: &[Device], sample_rate: f32) -> Self {
        Self {
            devices: devices
                .iter()
                .map(|d| EngineDevice {
                    id: d.id,
                    on: d.on,
                    params: d.params.clamped(),
                    rt: Runtime::new(d, sample_rate),
                })
                .collect(),
        }
    }

    /// Takes over the DSP state (tails, envelopes) of devices that also exist
    /// in `old`, leaving this chain's fresh state in `old` so it is freed with
    /// the old snapshot. Real-time safe.
    pub fn adopt(&mut self, old: &mut FxChain) {
        for d in self.devices.iter_mut() {
            if let Some(o) = old.devices.iter_mut().find(|o| o.id == d.id) {
                if o.rt.same_kind(&d.params) {
                    d.rt.adopt(&mut o.rt);
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        for d in self.devices.iter_mut() {
            if d.on {
                d.rt.process(&d.params, left, right, ctx);
            }
        }
    }

    /// Every device of the chain and of racks nested in it, depth first.
    pub fn visit(&self, f: &mut dyn FnMut(&EngineDevice)) {
        for d in &self.devices {
            f(d);
            if let Runtime::Rack(r) = &d.rt {
                for c in r.chains() {
                    c.visit(f);
                }
            }
        }
    }

    /// Runs `f` on the device with `id`, searching nested racks. Returns
    /// false if the device is not in this chain.
    fn with_device(&mut self, id: Id, f: &mut dyn FnMut(&mut EngineDevice)) -> bool {
        for d in self.devices.iter_mut() {
            if d.id == id {
                f(d);
                return true;
            }
            if let Some(r) = d.rack_mut() {
                for c in r.chains_mut() {
                    if c.with_device(id, f) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Sets one parameter. Returns false if the device is not in this chain.
    pub fn set_param(&mut self, device: Id, index: usize, value: f32) -> bool {
        self.with_device(device, &mut |d| {
            if let Runtime::Plugin(Some(p)) = &d.rt {
                p.set_param(index as u32, value);
            }
            d.params.set(index, value);
        })
    }

    /// Sets a parameter from a position 0..1 in its range (macros,
    /// automation). Plug-in parameters are already normalised.
    pub fn set_param_unit(&mut self, device: Id, index: usize, n: f32) -> bool {
        self.with_device(device, &mut |d| match &d.rt {
            Runtime::Plugin(Some(p)) => p.set_param(index as u32, n),
            _ => {
                if let Some(info) = d.params.info().get(index) {
                    let v = info.from_unit(n);
                    d.params.set(index, v);
                }
            }
        })
    }

    /// Applies an automated value (normalised 0..1) to a device. Audio thread.
    pub fn set_plugin_param(&mut self, device: Id, index: usize, value: f32) {
        self.set_param_unit(device, index, value);
    }

    /// Replaces all parameters of a device of the same kind.
    pub fn set_params(&mut self, device: Id, params: DeviceParams) -> bool {
        self.with_device(device, &mut |d| {
            if d.params.kind() == params.kind() {
                d.params = params.clamped();
            }
        })
    }

    pub fn set_on(&mut self, device: Id, on: bool) -> bool {
        self.with_device(device, &mut |d| {
            if on && !d.on {
                // Start clean instead of releasing a stale tail.
                d.rt.reset();
            }
            d.on = on;
        })
    }

    /// Sets the mixer of a rack chain nested in this chain.
    pub fn set_chain_mixer(&mut self, chain: Id, mixer: ChainMixer) -> bool {
        self.devices
            .iter_mut()
            .filter_map(|d| d.rack_mut())
            .any(|r| r.set_chain_mixer(chain, mixer))
    }

    /// Sets the zones of a rack chain nested in this chain.
    pub fn set_chain_zones(&mut self, chain: Id, zones: ChainZones) -> bool {
        self.devices
            .iter_mut()
            .filter_map(|d| d.rack_mut())
            .any(|r| r.set_chain_zones(chain, zones))
    }

    /// Clears all tails and envelopes.
    pub fn reset(&mut self) {
        for d in self.devices.iter_mut() {
            d.rt.reset();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::DeviceKind;

    #[test]
    fn chain_processes_in_series_and_keeps_state_across_rebuilds() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let mut devices = vec![Device::new(1, DeviceKind::Utility), Device::new(2, DeviceKind::Delay)];
        devices[0].params.set(0, -6.0206); // utility: half gain
        if let DeviceParams::Delay(p) = &mut devices[1].params {
            p.sync = 0.0;
            p.time_l = 10.0;
            p.time_r = 10.0;
            p.mix = 1.0;
            p.feedback = 0.0;
        }
        let mut chain = FxChain::new(&devices, sr);
        let mut l = vec![0.0f32; 1000];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        chain.process(&mut l, &mut r, &ctx);
        // A 10 ms echo of the halved impulse (softened by the delay's filters)
        // and nothing dry.
        assert!(l[..470].iter().all(|x| x.abs() < 1e-3));
        let peak = l[470..500].iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak < 0.5 && peak > 0.1, "{peak}");

        // An impulse is now inside the delay line; a rebuilt chain adopts it.
        let mut l = vec![0.0f32; 600];
        let mut r = l.clone();
        l[0] = 1.0;
        r[0] = 1.0;
        let mut chain = FxChain::new(&devices, sr);
        chain.process(&mut l[..100], &mut r[..100], &ctx);
        let mut rebuilt = FxChain::new(&devices, sr);
        rebuilt.adopt(&mut chain);
        rebuilt.process(&mut l[100..], &mut r[100..], &ctx);
        let tail = l[470..500].iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(tail > 0.1, "echo survived the rebuild: {tail}");

        // Switching a device off bypasses it.
        assert!(rebuilt.set_on(2, false));
        assert!(rebuilt.set_on(1, false));
        let mut l = [0.3f32; 16];
        let mut r = [0.3f32; 16];
        rebuilt.process(&mut l, &mut r, &ctx);
        assert!(l.iter().all(|&x| x == 0.3));
        assert!(!rebuilt.set_param(99, 0, 1.0));
    }
}
