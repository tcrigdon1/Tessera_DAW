//! Spectrum: a real-time frequency analyzer. The device passes audio through
//! unchanged and copies the selected channel into its [`scope`] tap; the
//! Device view panel runs the FFT, smoothing and drawing on the UI thread.

use std::sync::Arc;

use super::scope::{self, Scope};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::model::Id;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const BLOCK_SIZES: [usize; 4] = [1024, 2048, 4096, 8192];

device_params! {
    pub struct SpectrumParams {
        block: "Block", 0.0, 3.0, 2.0, ParamKind::Choice(&["1024", "2048", "4096", "8192"]), Unit::Plain;
        channel: "Channel", 0.0, 2.0, 0.0, ParamKind::Choice(&["L+R", "L", "R"]), Unit::Plain;
        refresh: "Refresh", 10.0, 250.0, 30.0, LOG, Unit::Ms;
        average: "Avg", 0.0, 0.98, 0.6, LIN, Unit::Percent;
        scale: "Scale", 0.0, 1.0, 0.0, ParamKind::Choice(&["Log", "Linear"]), Unit::Plain;
        range: "Range", -120.0, -30.0, -84.0, LIN, Unit::Db;
        auto: "Auto", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
    }
}

impl SpectrumParams {
    /// FFT length.
    pub fn block_size(&self) -> usize {
        BLOCK_SIZES[(self.block.round().max(0.0) as usize).min(BLOCK_SIZES.len() - 1)]
    }

    pub fn log_scale(&self) -> bool {
        self.scale < 0.5
    }
}

impl super::EffectParams for SpectrumParams {
    fn is_relevant(&self, index: usize) -> bool {
        // The floor does not apply while the range follows the signal.
        !(index == 5 && self.auto >= 0.5)
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Fast Response",
                Self {
                    block: 1.0,
                    refresh: 15.0,
                    average: 0.3,
                    ..d
                },
            ),
            (
                "Mix Overview",
                Self {
                    block: 3.0,
                    refresh: 60.0,
                    average: 0.85,
                    range: -96.0,
                    ..d
                },
            ),
            (
                "Auto Range",
                Self {
                    auto: 1.0,
                    average: 0.7,
                    ..d
                },
            ),
        ]
    }
}

pub struct Spectrum {
    sr: f32,
    scope: Option<Arc<Scope>>,
}

impl Spectrum {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate,
            scope: None,
        }
    }

    pub fn reset(&mut self) {}

    /// The analysis tap (after `attach`).
    pub fn scope(&self) -> Option<&Arc<Scope>> {
        self.scope.as_ref()
    }

    pub fn process(&mut self, p: &SpectrumParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        if let Some(s) = &self.scope {
            let (gl, gr) = match p.channel.round() as i32 {
                1 => (1.0, 0.0),
                2 => (0.0, 1.0),
                _ => (0.5, 0.5),
            };
            s.push_stereo(left, right, gl, gr);
        }
    }
}

impl super::EffectRuntime for Spectrum {
    fn attach(&mut self, device: Id) {
        let s = scope::tap(device);
        s.set_sample_rate(self.sr);
        self.scope = Some(s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::EffectRuntime;

    #[test]
    fn passes_audio_and_feeds_the_scope() {
        let sr = 48000.0;
        let ctx = FxContext::new(sr, 120.0);
        let mut s = Spectrum::new(sr);
        s.attach(0x5bec_0001);
        let p = SpectrumParams {
            channel: 1.0,
            ..Default::default()
        };
        let mut l: Vec<f32> = (0..512).map(|i| (i as f32 * 0.1).sin()).collect();
        let mut r = vec![0.25f32; 512];
        let (l0, r0) = (l.clone(), r.clone());
        s.process(&p, &mut l, &mut r, &ctx);
        assert_eq!(l, l0);
        assert_eq!(r, r0);
        let scope = scope::find(0x5bec_0001).unwrap();
        assert_eq!(scope.sample_rate(), sr);
        let mut snap = Vec::new();
        scope.snapshot_len(&mut snap, 512);
        assert_eq!(snap, l0, "left channel selected");
        assert_eq!(SpectrumParams::default().block_size(), 4096);
    }
}
