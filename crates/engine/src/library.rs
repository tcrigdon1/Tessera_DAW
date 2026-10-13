//! Built-in content for the browser: instrument presets ("Sounds"), drum kits
//! and a small factory sample library rendered from Tessera's own instruments,
//! so the browser, Sampler and Drum Rack have something to play on a fresh
//! install.

use crate::dsp::drum_rack::{DrumRack, DrumRackParams, PadSource, BASE_NOTE};
use crate::dsp::drums::DrumKitParams;
use crate::dsp::fm::FmParams;
use crate::dsp::sampler::SamplerParams;
use crate::dsp::synth::{PolySynth, SynthParams};
use crate::model::{AudioData, Instrument};

/// A named instrument setting shown in the browser.
#[derive(Debug, Clone, Copy)]
pub struct Preset {
    /// Sub-folder in the Sounds category (Bass, Keys, Pad, Lead, ...).
    pub category: &'static str,
    pub name: &'static str,
    pub instrument: Instrument,
}

const fn preset(category: &'static str, name: &'static str, instrument: Instrument) -> Preset {
    Preset {
        category,
        name,
        instrument,
    }
}

/// The instruments themselves with default settings (browser "Instruments").
pub fn instruments() -> Vec<Preset> {
    vec![
        preset(
            "Instruments",
            "Drum Rack",
            Instrument::DrumRack(DrumRackParams::standard()),
        ),
        preset("Instruments", "Sampler", Instrument::Sampler(SamplerParams::default())),
        preset("Instruments", "FM Synth", Instrument::Fm(FmParams::default())),
        preset("Instruments", "Poly Synth", Instrument::Synth(SynthParams::default())),
        preset("Instruments", "Drum Kit", Instrument::DrumKit(DrumKitParams::default())),
    ]
}

/// Instrument presets grouped by category (browser "Sounds").
pub fn sounds() -> Vec<Preset> {
    vec![
        preset("Bass", "Analog Bass", Instrument::Synth(SynthParams::bass())),
        preset("Pad", "Warm Pad", Instrument::Synth(SynthParams::pad())),
        preset("Lead", "Saw Lead", Instrument::Synth(SynthParams::default())),
        preset("Keys", "FM Electric Piano", Instrument::Fm(FmParams::electric_piano())),
        preset("Keys", "FM Organ", Instrument::Fm(FmParams::organ())),
        preset("Mallets", "FM Bell", Instrument::Fm(FmParams::bell())),
        preset("Bass", "FM Bass", Instrument::Fm(FmParams::bass())),
        preset("Brass", "FM Brass", Instrument::Fm(FmParams::brass())),
        preset("Pluck", "FM Pluck", Instrument::Fm(FmParams::pluck())),
    ]
}

/// Drum Rack kits (browser "Drums").
pub fn drum_kits() -> Vec<Preset> {
    vec![
        preset(
            "Drums",
            "Standard Kit",
            Instrument::DrumRack(DrumRackParams::standard()),
        ),
        preset("Drums", "Boom Kit", Instrument::DrumRack(DrumRackParams::boom())),
        preset("Drums", "Tight Kit", Instrument::DrumRack(DrumRackParams::tight())),
        preset("Drums", "Lo-Fi Kit", Instrument::DrumRack(DrumRackParams::lofi())),
    ]
}

/// One rendered factory sample.
pub struct FactorySample {
    /// Sub-folder (Drums, Loops, Instruments).
    pub folder: &'static str,
    pub name: String,
    pub data: AudioData,
}

fn render_drum(kit: &DrumRackParams, pad: usize, sample_rate: u32, secs: f32) -> AudioData {
    // Samples are centred; the kit's panning is for its own mix.
    let mut kit = *kit;
    kit.pads[pad].pan = 0.0;
    let kit = &kit;
    let mut rack = DrumRack::new(sample_rate as f32);
    rack.note_on(BASE_NOTE + pad as u8, 120);
    let n = (secs * sample_rate as f32) as usize;
    let mut l = vec![0.0; n];
    let mut r = vec![0.0; n];
    for (cl, cr) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        rack.render(kit, &[], cl, cr);
    }
    trim_and_normalize(AudioData {
        sample_rate,
        left: l,
        right: r,
    })
}

/// Cuts trailing silence and normalizes to -1 dBFS.
fn trim_and_normalize(mut d: AudioData) -> AudioData {
    let last = (0..d.frames())
        .rev()
        .find(|&i| d.left[i].abs().max(d.right[i].abs()) > 1e-4)
        .unwrap_or(0);
    let keep = (last + 64).min(d.frames());
    d.left.truncate(keep);
    d.right.truncate(keep);
    let peak = d.left.iter().chain(d.right.iter()).fold(0.0f32, |a, b| a.max(b.abs()));
    if peak > 1e-6 {
        let g = 0.89 / peak;
        for v in d.left.iter_mut().chain(d.right.iter_mut()) {
            *v *= g;
        }
    }
    d
}

/// A one-bar drum loop at `bpm` built with the Drum Rack.
fn render_loop(kit: &DrumRackParams, pattern: &[(f64, u8, u8)], bpm: f64, sample_rate: u32) -> AudioData {
    let beats = 4.0;
    let frames_per_beat = 60.0 / bpm * sample_rate as f64;
    let n = (beats * frames_per_beat).round() as usize;
    let mut rack = DrumRack::new(sample_rate as f32);
    let mut l = vec![0.0; n];
    let mut r = vec![0.0; n];
    let mut hits: Vec<(usize, u8, u8)> = pattern
        .iter()
        .map(|&(beat, note, vel)| ((beat * frames_per_beat).round() as usize, note, vel))
        .collect();
    hits.sort_by_key(|h| h.0);
    let mut pos = 0;
    for (at, note, vel) in hits {
        let at = at.min(n);
        if at > pos {
            rack.render(kit, &[], &mut l[pos..at], &mut r[pos..at]);
            pos = at;
        }
        rack.note_on(note, vel);
    }
    rack.render(kit, &[], &mut l[pos..], &mut r[pos..]);
    let mut d = AudioData {
        sample_rate,
        left: l,
        right: r,
    };
    let peak = d.left.iter().chain(d.right.iter()).fold(0.0f32, |a, b| a.max(b.abs()));
    if peak > 1e-6 {
        let g = 0.89 / peak;
        for v in d.left.iter_mut().chain(d.right.iter_mut()) {
            *v *= g;
        }
    }
    d
}

fn render_synth_note(p: &SynthParams, note: u8, hold_secs: f32, sample_rate: u32) -> AudioData {
    let mut s = PolySynth::new(sample_rate as f32);
    let n_hold = (hold_secs * sample_rate as f32) as usize;
    let n = n_hold + (p.release * 3.0 * sample_rate as f32) as usize + 512;
    let mut l = vec![0.0; n];
    let mut r = vec![0.0; n];
    s.note_on(note, 110);
    s.render(p, &mut l[..n_hold], &mut r[..n_hold]);
    s.note_off(note);
    s.render(p, &mut l[n_hold..], &mut r[n_hold..]);
    trim_and_normalize(AudioData {
        sample_rate,
        left: l,
        right: r,
    })
}

/// Renders the factory sample library. Runs on the UI thread at first start
/// (takes a fraction of a second).
pub fn factory_samples(sample_rate: u32) -> Vec<FactorySample> {
    let mut out = Vec::new();
    let kits = [
        ("Standard", DrumRackParams::standard()),
        ("Boom", DrumRackParams::boom()),
        ("Tight", DrumRackParams::tight()),
    ];
    for (kit_name, kit) in kits {
        for (pad, p) in kit.pads.iter().enumerate() {
            if p.source == PadSource::Sample {
                continue;
            }
            // Skip duplicate sources within a kit (alternate toms, hats).
            if kit.pads[..pad].iter().any(|q| q.source == p.source) {
                continue;
            }
            let secs = match p.source {
                PadSource::Crash | PadSource::Ride => 3.0,
                PadSource::Kick | PadSource::Tom | PadSource::OpenHat => 1.5,
                _ => 1.0,
            };
            out.push(FactorySample {
                folder: "Drums",
                name: format!("{} {kit_name}", p.source.label()),
                data: render_drum(&kit, pad, sample_rate, secs),
            });
        }
    }

    let k = 36u8;
    let s = 38u8;
    let ch = 42u8;
    let oh = 46u8;
    let cp = 39u8;
    let mut four: Vec<(f64, u8, u8)> = Vec::new();
    for b in 0..4 {
        four.push((b as f64, k, 118));
        four.push((b as f64 + 0.5, oh, 80));
    }
    four.push((1.0, cp, 100));
    four.push((3.0, cp, 104));
    let mut breakbeat: Vec<(f64, u8, u8)> = vec![
        (0.0, k, 120),
        (1.0, s, 110),
        (1.75, k, 90),
        (2.25, k, 110),
        (2.5, s, 70),
        (3.0, s, 112),
        (3.75, s, 60),
    ];
    for i in 0..8 {
        breakbeat.push((i as f64 * 0.5, ch, if i % 2 == 0 { 96 } else { 70 }));
    }
    let mut trap: Vec<(f64, u8, u8)> = vec![(0.0, k, 120), (2.0, s, 115), (2.75, k, 100), (3.25, k, 90)];
    for i in 0..16 {
        trap.push((i as f64 * 0.25, ch, if i % 4 == 0 { 100 } else { 60 }));
    }
    for i in 0..3 {
        trap.push((3.5 + i as f64 / 6.0, ch, 70));
    }
    out.push(FactorySample {
        folder: "Loops",
        name: "Four On The Floor 124 bpm".into(),
        data: render_loop(&DrumRackParams::standard(), &four, 124.0, sample_rate),
    });
    out.push(FactorySample {
        folder: "Loops",
        name: "Break 96 bpm".into(),
        data: render_loop(&DrumRackParams::tight(), &breakbeat, 96.0, sample_rate),
    });
    out.push(FactorySample {
        folder: "Loops",
        name: "Trap Hats 140 bpm".into(),
        data: render_loop(&DrumRackParams::boom(), &trap, 140.0, sample_rate),
    });

    for (name, p, note, hold) in [
        ("Saw Stab C3", SynthParams::default(), 60u8, 0.25f32),
        ("Bass Pluck C1", SynthParams::bass(), 36, 0.3),
        ("Pad Chord Root C3", SynthParams::pad(), 60, 2.0),
    ] {
        out.push(FactorySample {
            folder: "Instruments",
            name: name.into(),
            data: render_synth_note(&p, note, hold, sample_rate),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factory_samples_are_audible_normalized_and_named_uniquely() {
        let lib = factory_samples(44100);
        assert!(lib.len() >= 20, "{}", lib.len());
        let mut names: Vec<String> = lib.iter().map(|s| format!("{}/{}", s.folder, s.name)).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), lib.len(), "duplicate names");
        for s in &lib {
            let peak = s.data.left.iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(peak > 0.5 && peak <= 0.9, "{}: {peak}", s.name);
            assert!(s.data.frames() > 400, "{}", s.name);
            assert!(s.data.left.iter().all(|v| v.is_finite()));
        }
        // Loops are exactly one bar long.
        let four = lib.iter().find(|s| s.name.starts_with("Four")).unwrap();
        let expected = (4.0 * 60.0 / 124.0 * 44100.0f64).round() as usize;
        assert_eq!(four.data.frames(), expected);
    }

    #[test]
    fn presets_have_unique_names() {
        let mut all: Vec<String> = sounds()
            .iter()
            .chain(drum_kits().iter())
            .chain(instruments().iter())
            .map(|p| format!("{}/{}", p.category, p.name))
            .collect();
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(all.len(), n);
    }
}
