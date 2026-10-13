//! Auto Pan: LFO-driven amplitude modulation per channel. With the stereo
//! phase at 180 degrees the channels move in opposition (auto-pan, equal
//! power); at 0 degrees both move together (tremolo).

use super::auto_filter::{degrees, sync_rate_beats, wrap, LfoClock, LfoVoice, SYNC_QUARTER, SYNC_RATES};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const LOG: ParamKind = ParamKind::Continuous(Scale::Log);

pub const PAN_SHAPES: &[&str] = &["Sine", "Triangle", "Saw Up", "Saw Down", "Square", "Random"];

device_params! {
    pub struct AutoPanParams {
        amount: "Amount", 0.0, 1.0, 0.6, LIN, Unit::Percent;
        sync: "Sync", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        rate: "Rate", 0.01, 40.0, 1.0, LOG, Unit::Hz;
        div: "Rate", 0.0, 16.0, SYNC_QUARTER, ParamKind::Choice(SYNC_RATES), Unit::Plain;
        shape: "Waveform", 0.0, 5.0, 0.0, ParamKind::Choice(PAN_SHAPES), Unit::Plain;
        sharpen: "Shape", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        phase: "Phase", 0.0, 360.0, 180.0, LIN, Unit::Integer;
        offset: "Offset", 0.0, 360.0, 0.0, LIN, Unit::Integer;
        invert: "Invert", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
    }
}

impl AutoPanParams {
    fn shape_index(&self) -> usize {
        (self.shape.round().max(0.0) as usize).min(PAN_SHAPES.len() - 1)
    }

    /// Index into the shared LFO shape list.
    fn lfo_shape(&self) -> usize {
        match self.shape_index() {
            5 => super::auto_filter::SHAPE_SAMPLE_HOLD,
            s => s,
        }
    }

    /// Applies "Shape" (pushes the waveform towards a square) and "Invert".
    #[inline]
    pub fn shape_lfo(&self, v: f32) -> f32 {
        let s = self.sharpen.clamp(0.0, 1.0);
        let v = if s > 0.0 {
            let k = 1.0 + s * 12.0;
            (k * v).tanh() / k.tanh()
        } else {
            v
        };
        if self.invert >= 0.5 {
            -v
        } else {
            v
        }
    }

    /// Gain of one channel for a bipolar LFO value.
    #[inline]
    pub fn gain(&self, v: f32) -> f32 {
        // u = 0 is full volume, u = 1 silence; equal power so a 180-degree pan
        // keeps the total power constant.
        let u = 0.5 - 0.5 * v.clamp(-1.0, 1.0);
        let a = self.amount.clamp(0.0, 1.0);
        ((1.0 - a) + a * (u * std::f32::consts::FRAC_PI_2).cos()).max(0.0)
    }

    /// Gains of (left, right) at LFO phase `phase` (0..1) for drawing.
    pub fn preview(&self, phase: f32) -> (f32, f32) {
        let shape = self.lfo_shape();
        let l = self.shape_lfo(super::auto_filter::shape_value(shape, phase));
        let r = self.shape_lfo(super::auto_filter::shape_value(shape, phase + degrees(self.phase)));
        (self.gain(l), self.gain(r))
    }

    /// LFO period in seconds for the current rate.
    pub fn period(&self, tempo: f64, beats_per_bar: u32) -> f64 {
        if self.sync >= 0.5 {
            sync_rate_beats(self.div, beats_per_bar) * 60.0 / tempo.max(1.0)
        } else {
            1.0 / self.rate.max(0.001) as f64
        }
    }
}

impl super::EffectParams for AutoPanParams {
    fn is_relevant(&self, index: usize) -> bool {
        let synced = self.sync >= 0.5;
        match index {
            2 => !synced,
            3 => synced,
            _ => true,
        }
    }

    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Gentle Pan",
                Self {
                    amount: 0.5,
                    rate: 0.3,
                    ..d
                },
            ),
            (
                "Synced Tremolo",
                Self {
                    amount: 0.8,
                    sync: 1.0,
                    div: 5.0,
                    phase: 0.0,
                    sharpen: 0.3,
                    ..d
                },
            ),
            (
                "Chopper",
                Self {
                    amount: 1.0,
                    sync: 1.0,
                    div: 2.0,
                    shape: 4.0,
                    phase: 0.0,
                    ..d
                },
            ),
            (
                "Random Space",
                Self {
                    amount: 0.7,
                    sync: 1.0,
                    div: 5.0,
                    shape: 5.0,
                    ..d
                },
            ),
        ]
    }
}

pub struct AutoPan {
    sr: f32,
    clock: LfoClock,
    voices: [LfoVoice; 2],
    gains: [f32; 2],
    started: bool,
}

impl AutoPan {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sr: sample_rate.max(1.0),
            clock: LfoClock::default(),
            voices: [LfoVoice::new(0x2468_ace1), LfoVoice::new(0x1357_9bdf)],
            gains: [1.0; 2],
            started: false,
        }
    }

    pub fn reset(&mut self) {
        self.clock.reset();
    }

    pub fn process(&mut self, p: &AutoPanParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        let frames = left.len().min(right.len());
        let (start, inc) = self.clock.block(p.sync >= 0.5, p.rate, p.div, ctx, frames);
        let shape = p.lfo_shape();
        let off = degrees(p.offset);
        let stereo = degrees(p.phase);
        // A 1 ms de-click so square and random shapes don't pop.
        let smooth = 1.0 - super::smoothing(0.001, self.sr);
        if !self.started {
            self.started = true;
            let ph = start as f32 + off;
            self.gains = [
                p.gain(p.shape_lfo(self.voices[0].value(shape, wrap(ph)))),
                p.gain(p.shape_lfo(self.voices[1].value(shape, wrap(ph + stereo)))),
            ];
        }
        for i in 0..frames {
            let ph = (start + inc * i as f64) as f32 + off;
            let gl = p.gain(p.shape_lfo(self.voices[0].value(shape, wrap(ph))));
            let gr = p.gain(p.shape_lfo(self.voices[1].value(shape, wrap(ph + stereo))));
            self.gains[0] += (gl - self.gains[0]) * smooth;
            self.gains[1] += (gr - self.gains[1]) * smooth;
            left[i] *= self.gains[0];
            right[i] *= self.gains[1];
        }
    }
}

impl super::EffectRuntime for AutoPan {}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(p: &AutoPanParams, ctx: &FxContext, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut a = AutoPan::new(ctx.sample_rate);
        let mut l = vec![1.0f32; n];
        let mut r = vec![1.0f32; n];
        for (x, y) in l.chunks_mut(100).zip(r.chunks_mut(100)) {
            a.process(p, x, y, ctx);
        }
        (l, r)
    }

    #[test]
    fn autopan_keeps_power_constant() {
        let ctx = FxContext::new(48000.0, 120.0);
        let p = AutoPanParams {
            amount: 1.0,
            rate: 2.0,
            ..Default::default()
        };
        let (l, r) = run(&p, &ctx, 48000);
        for i in (1000..48000).step_by(97) {
            let pw = l[i] * l[i] + r[i] * r[i];
            assert!((pw - 1.0).abs() < 0.02, "{i}: {pw}");
        }
        // Moves fully from side to side.
        assert!(l.iter().cloned().fold(1.0, f32::min) < 0.05);
        assert!(r.iter().cloned().fold(1.0, f32::min) < 0.05);
    }

    #[test]
    fn zero_phase_is_tremolo() {
        let ctx = FxContext::new(48000.0, 120.0);
        let p = AutoPanParams {
            amount: 1.0,
            rate: 3.0,
            phase: 0.0,
            ..Default::default()
        };
        let (l, r) = run(&p, &ctx, 48000);
        assert!(l.iter().zip(&r).all(|(a, b)| (a - b).abs() < 1e-5));
        assert!(l.iter().cloned().fold(1.0, f32::min) < 0.05);
    }

    #[test]
    fn amount_zero_is_transparent() {
        let ctx = FxContext::new(48000.0, 120.0);
        let p = AutoPanParams {
            amount: 0.0,
            ..Default::default()
        };
        let (l, r) = run(&p, &ctx, 4800);
        assert!(l.iter().chain(&r).all(|x| (x - 1.0).abs() < 1e-6));
    }

    #[test]
    fn synced_lfo_locks_to_the_beat() {
        // 1/4 at 120 BPM: tremolo with square wave and phase 0; while playing
        // the gain at a given song position is the same regardless of where
        // playback started.
        let sr = 48000.0;
        let p = AutoPanParams {
            amount: 1.0,
            sync: 1.0,
            div: SYNC_QUARTER,
            shape: 4.0,
            phase: 0.0,
            ..Default::default()
        };
        let gain_at = |beat: f64| {
            let ctx = FxContext::new(sr, 120.0).with_transport(true, beat, 4, false);
            let mut a = AutoPan::new(sr);
            let mut l = vec![1.0f32; 256];
            let mut r = vec![1.0f32; 256];
            a.process(&p, &mut l, &mut r, &ctx);
            l[200]
        };
        // First half of each beat is up, second half down.
        assert!(gain_at(0.1) > 0.95);
        assert!(gain_at(7.1) > 0.95);
        assert!(gain_at(2.6) < 0.05);
        assert!(gain_at(5.55) < 0.05);
    }

    #[test]
    fn invert_and_preview() {
        let p = AutoPanParams {
            amount: 1.0,
            phase: 0.0,
            ..Default::default()
        };
        let (l, _) = p.preview(0.25);
        let inv = AutoPanParams { invert: 1.0, ..p };
        let (li, _) = inv.preview(0.25);
        assert!(l > 0.99 && li < 0.01, "{l} {li}");
        let sharp = AutoPanParams { sharpen: 1.0, ..p };
        // Sharpened sine is near full level much earlier in the cycle.
        assert!(sharp.preview(0.05).0 > p.preview(0.05).0 + 0.1);
        assert!((p.period(120.0, 4) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn extremes_stay_finite() {
        let ctx = FxContext::new(192000.0, 999.0);
        for shape in 0..6 {
            let p = AutoPanParams {
                amount: 1.0,
                rate: 40.0,
                shape: shape as f32,
                sharpen: 1.0,
                ..Default::default()
            };
            let (l, r) = run(&p, &ctx, 3000);
            assert!(l.iter().chain(&r).all(|x| x.is_finite() && (0.0..=1.001).contains(x)));
        }
    }
}
