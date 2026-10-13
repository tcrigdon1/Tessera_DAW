//! The audio-thread side of a device chain.

use super::{chorus, compressor, delay, eq, limiter, reverb, saturator, utility};
use super::{Device, DeviceParams};
use crate::model::Id;

/// Per-block information effects may need.
#[derive(Debug, Clone, Copy)]
pub struct FxContext {
    pub sample_rate: f32,
    pub tempo: f64,
}

impl FxContext {
    pub fn new(sample_rate: f32, tempo: f64) -> Self {
        Self { sample_rate, tempo }
    }
}

/// DSP state of one device. Boxed so moving it between snapshots is cheap.
enum Runtime {
    Eq(Box<eq::Eq>),
    Compressor(Box<compressor::Compressor>),
    Reverb(Box<reverb::Reverb>),
    Delay(Box<delay::Delay>),
    Saturator(Box<saturator::Saturator>),
    Chorus(Box<chorus::Chorus>),
    Limiter(Box<limiter::Limiter>),
    Utility(Box<utility::Utility>),
}

impl Runtime {
    fn new(params: &DeviceParams, sr: f32) -> Self {
        match params {
            DeviceParams::Eq(_) => Runtime::Eq(Box::new(eq::Eq::new(sr))),
            DeviceParams::Compressor(_) => Runtime::Compressor(Box::new(compressor::Compressor::new(sr))),
            DeviceParams::Reverb(_) => Runtime::Reverb(Box::new(reverb::Reverb::new(sr))),
            DeviceParams::Delay(_) => Runtime::Delay(Box::new(delay::Delay::new(sr))),
            DeviceParams::Saturator(_) => Runtime::Saturator(Box::new(saturator::Saturator::new(sr))),
            DeviceParams::Chorus(_) => Runtime::Chorus(Box::new(chorus::Chorus::new(sr))),
            DeviceParams::Limiter(_) => Runtime::Limiter(Box::new(limiter::Limiter::new(sr))),
            DeviceParams::Utility(_) => Runtime::Utility(Box::new(utility::Utility::new(sr))),
        }
    }

    fn same_kind(&self, params: &DeviceParams) -> bool {
        matches!(
            (self, params),
            (Runtime::Eq(_), DeviceParams::Eq(_))
                | (Runtime::Compressor(_), DeviceParams::Compressor(_))
                | (Runtime::Reverb(_), DeviceParams::Reverb(_))
                | (Runtime::Delay(_), DeviceParams::Delay(_))
                | (Runtime::Saturator(_), DeviceParams::Saturator(_))
                | (Runtime::Chorus(_), DeviceParams::Chorus(_))
                | (Runtime::Limiter(_), DeviceParams::Limiter(_))
                | (Runtime::Utility(_), DeviceParams::Utility(_))
        )
    }

    fn process(&mut self, params: &DeviceParams, l: &mut [f32], r: &mut [f32], ctx: &FxContext) {
        match (self, params) {
            (Runtime::Eq(d), DeviceParams::Eq(p)) => d.process(p, l, r, ctx),
            (Runtime::Compressor(d), DeviceParams::Compressor(p)) => d.process(p, l, r, ctx),
            (Runtime::Reverb(d), DeviceParams::Reverb(p)) => d.process(p, l, r, ctx),
            (Runtime::Delay(d), DeviceParams::Delay(p)) => d.process(p, l, r, ctx),
            (Runtime::Saturator(d), DeviceParams::Saturator(p)) => d.process(p, l, r, ctx),
            (Runtime::Chorus(d), DeviceParams::Chorus(p)) => d.process(p, l, r, ctx),
            (Runtime::Limiter(d), DeviceParams::Limiter(p)) => d.process(p, l, r, ctx),
            (Runtime::Utility(d), DeviceParams::Utility(p)) => d.process(p, l, r, ctx),
            _ => {}
        }
    }

    fn reset(&mut self) {
        match self {
            Runtime::Eq(d) => d.reset(),
            Runtime::Compressor(d) => d.reset(),
            Runtime::Reverb(d) => d.reset(),
            Runtime::Delay(d) => d.reset(),
            Runtime::Saturator(d) => d.reset(),
            Runtime::Chorus(d) => d.reset(),
            Runtime::Limiter(d) => d.reset(),
            Runtime::Utility(d) => d.reset(),
        }
    }

    /// Gain reduction in dB for dynamics devices, 0 otherwise.
    fn meter(&self) -> f32 {
        match self {
            Runtime::Compressor(d) => d.meter_db,
            Runtime::Limiter(d) => d.meter_db,
            _ => 0.0,
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
    /// Gain reduction in dB for compressors and limiters.
    pub fn meter(&self) -> f32 {
        if self.on {
            self.rt.meter()
        } else {
            0.0
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
                    params: d.params,
                    rt: Runtime::new(&d.params, sample_rate),
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
                    std::mem::swap(&mut o.rt, &mut d.rt);
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

    /// Sets one parameter. Returns false if the device is not in this chain.
    pub fn set_param(&mut self, device: Id, index: usize, value: f32) -> bool {
        match self.devices.iter_mut().find(|d| d.id == device) {
            Some(d) => {
                d.params.set(index, value);
                true
            }
            None => false,
        }
    }

    /// Replaces all parameters of a device of the same kind.
    pub fn set_params(&mut self, device: Id, params: DeviceParams) -> bool {
        match self.devices.iter_mut().find(|d| d.id == device) {
            Some(d) => {
                if d.params.kind() == params.kind() {
                    d.params = params;
                }
                true
            }
            None => false,
        }
    }

    pub fn set_on(&mut self, device: Id, on: bool) -> bool {
        match self.devices.iter_mut().find(|d| d.id == device) {
            Some(d) => {
                if on && !d.on {
                    // Start clean instead of releasing a stale tail.
                    d.rt.reset();
                }
                d.on = on;
                true
            }
            None => false,
        }
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
