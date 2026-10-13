//! Cabinet: guitar and bass speaker cabinet simulation.
//!
//! Each cabinet is an original approximation built from cascaded biquads:
//! the enclosure's low-frequency cut and resonance, cone dips and the
//! presence peak, and the steep top-end roll-off of a guitar speaker. The
//! microphone adds its own colour (a dynamic's presence lift and soft top,
//! a condenser's extended air) and the position changes proximity bass and
//! brightness; the far position also picks up a short floor reflection.
//!
//! "Mic Mix" blends in a second microphone: the far room mic when a near
//! position is selected, otherwise a near on-axis mic.

use super::biquad::{Biquad, Coefs, FilterShape};
use super::{device_params, FxContext, ParamKind, Scale, Unit};

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);

pub const SPEAKERS: &[&str] = &["1x12", "2x12", "4x12", "4x10 Bass"];
pub const MICS: &[&str] = &["Condenser", "Dynamic"];
pub const POSITIONS: &[&str] = &["Near On-Axis", "Near Off-Axis", "Far"];

const POS_FAR: usize = 2;
const SPEAKER_SECTIONS: usize = 6;
const MIC_SECTIONS: usize = 4;
/// Floor reflection picked up by the far mic.
const REFLECTION_S: f32 = 0.0013;
const REFLECTION_GAIN: f32 = 0.35;

device_params! {
    pub struct CabinetParams {
        speaker: "Speaker", 0.0, 3.0, 2.0, ParamKind::Choice(SPEAKERS), Unit::Plain;
        mic: "Microphone", 0.0, 1.0, 1.0, ParamKind::Choice(MICS), Unit::Plain;
        position: "Position", 0.0, 2.0, 0.0, ParamKind::Choice(POSITIONS), Unit::Plain;
        mic_mix: "Mic Mix", 0.0, 1.0, 0.0, LIN, Unit::Percent;
        dual_mono: "Dual Mono", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        output: "Output", -24.0, 12.0, 0.0, LIN, Unit::Db;
        mix: "Dry/Wet", 0.0, 1.0, 1.0, LIN, Unit::Percent;
    }
}

impl super::EffectParams for CabinetParams {
    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "British Stack",
                Self {
                    speaker: 2.0,
                    mic: 1.0,
                    position: 0.0,
                    mic_mix: 0.2,
                    ..d
                },
            ),
            (
                "Open Back Combo",
                Self {
                    speaker: 0.0,
                    mic: 0.0,
                    position: 1.0,
                    ..d
                },
            ),
            (
                "Roomy 2x12",
                Self {
                    speaker: 1.0,
                    mic: 0.0,
                    position: 2.0,
                    mic_mix: 0.3,
                    ..d
                },
            ),
            (
                "Bass Cab",
                Self {
                    speaker: 3.0,
                    mic: 1.0,
                    position: 0.0,
                    mic_mix: 0.15,
                    ..d
                },
            ),
        ]
    }
}

use FilterShape::{Bell, HighPass, HighShelf, LowPass, LowShelf};

/// (shape, freq, q, gain) for one section.
type Section = (FilterShape, f32, f32, f32);

fn speaker_sections(speaker: usize) -> [Section; SPEAKER_SECTIONS] {
    match speaker {
        // Open-back 1x12: lighter low end, forward upper mids.
        0 => [
            (HighPass, 85.0, 0.9, 0.0),
            (Bell, 120.0, 1.8, 3.0),
            (Bell, 1600.0, 1.2, -2.0),
            (Bell, 2700.0, 1.6, 4.0),
            (LowPass, 5200.0, 0.9, 0.0),
            (LowPass, 7300.0, 0.6, 0.0),
        ],
        // Closed 2x12: rounder, a little scooped.
        1 => [
            (HighPass, 75.0, 1.0, 0.0),
            (Bell, 105.0, 1.8, 4.0),
            (Bell, 500.0, 1.0, -2.0),
            (Bell, 2200.0, 1.4, 3.5),
            (LowPass, 5000.0, 0.85, 0.0),
            (LowPass, 7000.0, 0.6, 0.0),
        ],
        // Closed 4x12: big resonant thump, low-mid dip, tight top.
        2 => [
            (HighPass, 70.0, 1.3, 0.0),
            (Bell, 100.0, 1.6, 5.0),
            (Bell, 400.0, 0.9, -3.0),
            (Bell, 2000.0, 1.3, 4.5),
            (LowPass, 4400.0, 0.9, 0.0),
            (LowPass, 6200.0, 0.6, 0.0),
        ],
        // 4x10 bass: deep extension, growly mids.
        _ => [
            (HighPass, 42.0, 1.1, 0.0),
            (Bell, 75.0, 1.5, 4.0),
            (Bell, 800.0, 1.0, 2.0),
            (Bell, 2800.0, 2.0, 2.0),
            (LowPass, 4000.0, 0.75, 0.0),
            (LowPass, 5600.0, 0.6, 0.0),
        ],
    }
}

fn mic_sections(mic: usize, position: usize) -> [Section; MIC_SECTIONS] {
    let dynamic = mic == 1;
    let (m0, m1) = if dynamic {
        ((Bell, 4500.0, 1.2, 4.0), (LowPass, 12000.0, 0.7, 0.0))
    } else {
        ((HighShelf, 9000.0, 0.7, 2.0), (Bell, 5000.0, 1.0, 1.0))
    };
    let proximity = if dynamic { 4.0 } else { 2.0 };
    let (p0, p1) = match position {
        0 => ((LowShelf, 180.0, 0.7, proximity), (Bell, 3500.0, 1.0, 1.5)),
        1 => ((LowShelf, 180.0, 0.7, proximity * 0.6), (HighShelf, 2500.0, 0.7, -6.0)),
        _ => ((LowShelf, 200.0, 0.7, -3.0), (LowPass, 6000.0, 0.7, 0.0)),
    };
    [m0, m1, p0, p1]
}

fn design<const N: usize>(s: [Section; N], sr: f32) -> [Coefs; N] {
    s.map(|(shape, f, q, g)| Coefs::design(shape, f, q, g, sr))
}

#[derive(Clone, Copy)]
struct CabCoefs {
    speaker: [Coefs; SPEAKER_SECTIONS],
    /// The selected mic and the blend mic.
    mic_a: [Coefs; MIC_SECTIONS],
    mic_b: [Coefs; MIC_SECTIONS],
    far_a: bool,
    far_b: bool,
    /// Normalises each mic path to unity at 1 kHz.
    norm_a: f32,
    norm_b: f32,
}

impl CabCoefs {
    fn new(p: &CabinetParams, sr: f32) -> Self {
        let speaker = super::glue::choice(p.speaker, SPEAKERS.len());
        let mic = super::glue::choice(p.mic, MICS.len());
        let pos = super::glue::choice(p.position, POSITIONS.len());
        let pos_b = if pos == POS_FAR { 0 } else { POS_FAR };
        let spk = design(speaker_sections(speaker), sr);
        let mic_a = design(mic_sections(mic, pos), sr);
        let mic_b = design(mic_sections(mic, pos_b), sr);
        let norm = |m: &[Coefs]| {
            let g: f32 = spk.iter().chain(m.iter()).map(|c| c.magnitude(1000.0, sr)).product();
            1.0 / g.max(1e-3)
        };
        let far_gain = 0.6; // the far mic is quieter
        Self {
            speaker: spk,
            norm_a: norm(&mic_a) * if pos == POS_FAR { far_gain } else { 1.0 },
            norm_b: norm(&mic_b) * if pos_b == POS_FAR { far_gain } else { 1.0 },
            mic_a,
            mic_b,
            far_a: pos == POS_FAR,
            far_b: pos_b == POS_FAR,
        }
    }

    /// Magnitude response of the selected mic path at `freq`.
    #[cfg(test)]
    fn magnitude(&self, freq: f32, sr: f32) -> f32 {
        let g: f32 = self
            .speaker
            .iter()
            .chain(self.mic_a.iter())
            .map(|c| c.magnitude(freq, sr))
            .product();
        g * self.norm_a
    }
}

struct Channel {
    speaker: [Biquad; SPEAKER_SECTIONS],
    mic_a: [Biquad; MIC_SECTIONS],
    mic_b: [Biquad; MIC_SECTIONS],
    /// Delay line for the far mic's floor reflection.
    reflect: Vec<f32>,
    pos: usize,
}

impl Channel {
    fn new(len: usize) -> Self {
        Self {
            speaker: [Biquad::default(); SPEAKER_SECTIONS],
            mic_a: [Biquad::default(); MIC_SECTIONS],
            mic_b: [Biquad::default(); MIC_SECTIONS],
            reflect: vec![0.0; len.max(2)],
            pos: 0,
        }
    }

    fn reset(&mut self) {
        self.speaker = [Biquad::default(); SPEAKER_SECTIONS];
        self.mic_a = [Biquad::default(); MIC_SECTIONS];
        self.mic_b = [Biquad::default(); MIC_SECTIONS];
        self.reflect.iter_mut().for_each(|v| *v = 0.0);
    }

    #[inline]
    fn tick(&mut self, x: f32, c: &CabCoefs, blend: f32) -> f32 {
        let mut s = x;
        for (f, k) in self.speaker.iter_mut().zip(c.speaker.iter()) {
            s = f.process(k, s);
        }
        // The reflection is taken from the speaker output.
        let len = self.reflect.len();
        self.reflect[self.pos] = s;
        let delayed = self.reflect[(self.pos + 1) % len];
        self.pos = (self.pos + 1) % len;
        let path = |filters: &mut [Biquad; MIC_SECTIONS], coefs: &[Coefs; MIC_SECTIONS], far: bool| {
            let mut y = if far { s + REFLECTION_GAIN * delayed } else { s };
            for (f, k) in filters.iter_mut().zip(coefs.iter()) {
                y = f.process(k, y);
            }
            y
        };
        let a = path(&mut self.mic_a, &c.mic_a, c.far_a) * c.norm_a;
        if blend > 1e-4 {
            let b = path(&mut self.mic_b, &c.mic_b, c.far_b) * c.norm_b;
            a + (b - a) * blend
        } else {
            a
        }
    }
}

pub struct Cabinet {
    sr: f32,
    ch: [Channel; 2],
    coefs: CabCoefs,
    key: [f32; 3],
    out: f32,
    mix: f32,
    blend: f32,
}

impl Cabinet {
    pub fn new(sample_rate: f32) -> Self {
        let p = CabinetParams::default();
        // The reflection delay is the whole line length.
        let len = (REFLECTION_S * sample_rate).round().max(1.0) as usize + 1;
        Self {
            sr: sample_rate,
            ch: [Channel::new(len), Channel::new(len)],
            coefs: CabCoefs::new(&p, sample_rate),
            key: [p.speaker, p.mic, p.position],
            out: 1.0,
            mix: 1.0,
            blend: 0.0,
        }
    }

    pub fn reset(&mut self) {
        for c in &mut self.ch {
            c.reset();
        }
    }

    pub fn process(&mut self, p: &CabinetParams, left: &mut [f32], right: &mut [f32], _ctx: &FxContext) {
        let key = [p.speaker, p.mic, p.position];
        if key != self.key {
            self.key = key;
            self.coefs = CabCoefs::new(p, self.sr);
        }
        let c = self.coefs;
        let smooth = super::smoothing(0.01, self.sr);
        let out_t = super::db(p.output.clamp(-24.0, 12.0));
        let mix_t = p.mix.clamp(0.0, 1.0);
        let blend_t = p.mic_mix.clamp(0.0, 1.0);
        let dual = p.dual_mono >= 0.5;
        for i in 0..left.len().min(right.len()) {
            self.out = out_t + (self.out - out_t) * smooth;
            self.mix = mix_t + (self.mix - mix_t) * smooth;
            self.blend = blend_t + (self.blend - blend_t) * smooth;
            let (xl, xr) = (left[i], right[i]);
            let (wl, wr) = if dual {
                (self.ch[0].tick(xl, &c, self.blend), self.ch[1].tick(xr, &c, self.blend))
            } else {
                let w = self.ch[0].tick(0.5 * (xl + xr), &c, self.blend);
                (w, w)
            };
            left[i] = xl + (wl * self.out - xl) * self.mix;
            right[i] = xr + (wr * self.out - xr) * self.mix;
        }
    }
}

impl super::EffectRuntime for Cabinet {}

#[cfg(test)]
mod tests {
    use super::super::overdrive::tests::tone_level;
    use super::super::EffectParams;
    use super::*;
    use std::f32::consts::PI;

    const SR: f32 = 48000.0;

    fn level_at(p: &CabinetParams, f: f32) -> f32 {
        let ctx = FxContext::new(SR, 120.0);
        let mut c = Cabinet::new(SR);
        let x: Vec<f32> = (0..24000)
            .map(|i| 0.25 * (2.0 * PI * f * i as f32 / SR).sin())
            .collect();
        let mut l = x.clone();
        let mut r = x;
        for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            c.process(p, cl, cr, &ctx);
        }
        tone_level(&l[8000..], f, SR) / 0.25
    }

    #[test]
    fn shapes_like_a_guitar_speaker() {
        for s in 0..SPEAKERS.len() {
            let p = CabinetParams {
                speaker: s as f32,
                ..Default::default()
            };
            let mid = level_at(&p, 1000.0);
            assert!((mid - 1.0).abs() < 0.1, "speaker {s}: {mid}");
            assert!(level_at(&p, 12000.0) < 0.05, "speaker {s} top end");
            let sub_limit = if s == 3 { 0.6 } else { 0.3 };
            assert!(level_at(&p, 25.0) < sub_limit, "speaker {s} sub");
        }
    }

    #[test]
    fn processing_matches_the_designed_response() {
        let p = CabinetParams::default();
        let c = CabCoefs::new(&p, SR);
        for f in [100.0, 500.0, 2000.0, 4000.0] {
            let measured = level_at(&p, f);
            let designed = c.magnitude(f, SR);
            assert!(
                (measured - designed).abs() < 0.05 * designed.max(0.2),
                "{f}: {measured} {designed}"
            );
        }
    }

    #[test]
    fn choices_change_the_sound() {
        let base = CabinetParams::default();
        let resp = |p: &CabinetParams| {
            let c = CabCoefs::new(p, SR);
            [80.0, 300.0, 2000.0, 4000.0, 6000.0].map(|f| c.magnitude(f, SR))
        };
        let differ = |a: [f32; 5], b: [f32; 5]| a.iter().zip(b.iter()).any(|(x, y)| (x - y).abs() > 0.05);
        let r0 = resp(&base);
        assert!(differ(r0, resp(&CabinetParams { speaker: 0.0, ..base })));
        assert!(differ(r0, resp(&CabinetParams { mic: 0.0, ..base })));
        // Off-axis is darker than on-axis.
        let off = CabCoefs::new(&CabinetParams { position: 1.0, ..base }, SR);
        let on = CabCoefs::new(&base, SR);
        assert!(off.magnitude(4000.0, SR) < on.magnitude(4000.0, SR) * 0.7);
        // The bass cab reaches lower than the open-back 1x12.
        let bass = CabCoefs::new(&CabinetParams { speaker: 3.0, ..base }, SR);
        let open = CabCoefs::new(&CabinetParams { speaker: 0.0, ..base }, SR);
        assert!(bass.magnitude(50.0, SR) > open.magnitude(50.0, SR) * 1.5);
    }

    #[test]
    fn mic_mix_dual_mono_and_dry() {
        let ctx = FxContext::new(SR, 120.0);
        let p = CabinetParams {
            mic_mix: 1.0,
            dual_mono: 1.0,
            ..Default::default()
        };
        let mut c = Cabinet::new(SR);
        let mut l: Vec<f32> = (0..9600).map(|i| (2.0 * PI * 440.0 * i as f32 / SR).sin()).collect();
        let mut r = vec![0.0f32; 9600];
        c.process(&p, &mut l, &mut r, &ctx);
        assert!(r.iter().all(|v| v.abs() < 1e-6));
        assert!(l.iter().all(|v| v.is_finite()) && tone_level(&l[4800..], 440.0, SR) > 0.2);
        let mut c = Cabinet::new(SR);
        let p = CabinetParams {
            mix: 0.0,
            ..Default::default()
        };
        c.mix = 0.0;
        let x: Vec<f32> = (0..512).map(|i| (i as f32 * 0.1).sin()).collect();
        let mut l = x.clone();
        let mut r = x.clone();
        c.process(&p, &mut l, &mut r, &ctx);
        assert!(l.iter().zip(&x).all(|(a, b)| (a - b).abs() < 1e-6));
    }

    #[test]
    fn presets_stay_finite_at_high_rates() {
        for sr in [44100.0, 192000.0] {
            let ctx = FxContext::new(sr, 120.0);
            for (_, p) in CabinetParams::presets() {
                let mut c = Cabinet::new(sr);
                let mut l: Vec<f32> = (0..2048).map(|i| ((i * 17) % 40) as f32 - 20.0).collect();
                let mut r = l.clone();
                c.process(&p, &mut l, &mut r, &ctx);
                c.process(&p, &mut l[..1], &mut r[..1], &ctx);
                assert!(l.iter().all(|v| v.is_finite() && v.abs() < 200.0));
            }
        }
    }
}
