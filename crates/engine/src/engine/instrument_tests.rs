//! Engine-level tests for the sample-based instruments and browser preview.

use super::*;
use crate::dsp::drum_rack::{DrumRackParams, PadSource};
use crate::dsp::sampler::SamplerParams;
use crate::model::{Clip, ClipContent, Instrument, Note, Project, SampleRef, TrackKind};

const SR: f32 = 48000.0;

fn tone(freq: f32, secs: f32) -> Arc<AudioData> {
    let n = (secs * SR) as usize;
    let v: Vec<f32> = (0..n)
        .map(|i| (i as f32 * freq * std::f32::consts::TAU / SR).sin() * 0.5)
        .collect();
    Arc::new(AudioData {
        sample_rate: SR as u32,
        right: v.clone(),
        left: v,
    })
}

fn sample_ref(data: Arc<AudioData>) -> SampleRef {
    SampleRef {
        path: "tone.wav".into(),
        data: Some(data),
    }
}

fn note_clip(p: &mut Project, pitch: u8) -> Clip {
    Clip {
        id: p.alloc_id(),
        name: "c".into(),
        color: [0, 0, 0],
        length: 4.0,
        looping: true,
        gain_db: 0.0,
        content: ClipContent::Midi {
            notes: vec![Note {
                pitch,
                velocity: 120,
                start: 0.0,
                length: 2.0,
            }],
        },
    }
}

fn setup(p: &Project) -> (Engine, EngineHandle) {
    let (mut e, mut h) = Engine::new(SR);
    h.load_project(p, true);
    let mut buf = vec![0.0; 2];
    e.process(&mut buf);
    (e, h)
}

fn run(e: &mut Engine, frames: usize) -> f32 {
    let mut buf = vec![0.0; frames * 2];
    let mut peak = 0.0f32;
    for chunk in buf.chunks_mut(1024) {
        e.process(chunk);
        peak = chunk.iter().fold(peak, |a, b| a.max(b.abs()));
    }
    peak
}

#[test]
fn sampler_track_plays_its_sample_and_is_silent_without_one() {
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::Sampler(SamplerParams::default()));
    let c = note_clip(&mut p, 60);
    p.track_mut(id).unwrap().slots[0] = Some(c);
    let (mut e, mut h) = setup(&p);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    assert!(run(&mut e, 24000) < 1e-6, "no sample loaded");

    // Loading a sample is a snapshot change; the playing clip carries on.
    p.track_mut(id)
        .unwrap()
        .set_sample(0, Some(sample_ref(tone(220.0, 2.0))));
    h.load_project(&p, false);
    h.send(Command::LaunchClip { track: id, slot: 0 });
    run(&mut e, 48000 * 2 + 1000); // next bar
    assert!(run(&mut e, 24000) > 0.05, "plays the sample");
    h.collect_garbage();
}

#[test]
fn drum_rack_plays_synth_and_sample_pads_and_reports_hits() {
    let mut p = Project::default();
    let mut kit = DrumRackParams::standard();
    kit.pads[1].source = PadSource::Sample;
    let id = p.add_track(TrackKind::Midi, Instrument::DrumRack(kit));
    p.track_mut(id)
        .unwrap()
        .set_sample(1, Some(sample_ref(tone(500.0, 0.5))));
    let (mut e, mut h) = setup(&p);
    for (note, pad) in [(36u8, 0u32), (37, 1)] {
        h.send(Command::NoteOn {
            track: id,
            pitch: note,
            velocity: 127,
        });
        assert!(run(&mut e, 4800) > 0.05, "note {note}");
        let hits = h.shared.tracks[0].pad_hits.swap(0, Ordering::Relaxed);
        assert_eq!(hits, 1 << pad);
        run(&mut e, 48000 * 3);
    }
}

#[test]
fn preview_plays_and_returns_the_sample_to_the_ui() {
    let p = Project::default();
    let (mut e, mut h) = setup(&p);
    let data = tone(440.0, 0.2);
    h.send(Command::PreviewSample(data.clone()));
    assert!(run(&mut e, 4800) > 0.1);
    run(&mut e, 48000);
    assert!(run(&mut e, 4800) < 1e-6, "preview ended");
    h.collect_garbage();
    assert_eq!(Arc::strong_count(&data), 1, "engine handed the sample back");

    h.send(Command::PreviewSample(tone(440.0, 5.0)));
    run(&mut e, 4800);
    h.send(Command::StopPreview);
    run(&mut e, 4800);
    assert!(run(&mut e, 4800) < 1e-6, "stop fades out");
}

#[test]
fn projects_with_samples_round_trip_and_reload_from_disk() {
    let dir = std::env::temp_dir().join(format!("tessera-inst-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("hit.wav");
    let samples: Vec<f32> = (0..4000).map(|i| ((i as f32) * 0.1).sin() * 0.5).collect();
    crate::render::write_wav(&wav, &samples, 48000).unwrap();
    let mut p = Project::default();
    let id = p.add_track(TrackKind::Midi, Instrument::DrumRack(DrumRackParams::empty()));
    p.track_mut(id)
        .unwrap()
        .set_sample(5, Some(SampleRef::load(&wav).unwrap()));
    let file = dir.join("song.tessera");
    p.save_to(&file).unwrap();
    let q = Project::load_from(&file).unwrap();
    std::fs::remove_dir_all(&dir).ok();
    let t = q.track(id).unwrap();
    assert!(matches!(t.instrument, Instrument::DrumRack(_)));
    let s = t.sample(5).expect("sample slot kept");
    assert_eq!(s.data.as_ref().map(|d| d.frames()), Some(2000));
    assert_eq!(s.name(), "hit");
}
