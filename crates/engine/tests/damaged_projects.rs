//! Damaged or hand-edited project files must load and play without
//! crashing, hanging, or sending NaN or infinity to the sound card.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tessera_engine::automation::clamped_instrument;
use tessera_engine::dsp::fm::FmParams;
use tessera_engine::fx::{Device, DeviceKind, DeviceParams};
use tessera_engine::model::{AudioData, AudioRef, Clip, ClipContent, Instrument, Project, TrackKind};
use tessera_engine::warp::{WarpMarker, WarpMode, WarpPlayback, WarpSettings};
use tessera_engine::{demo, library, Command, Engine};

/// Plays the arrangement and then scene 0, returning every output sample.
fn play(p: &Project) -> Vec<f32> {
    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    let mut out = Vec::new();
    engine.process(&mut buf);
    handle.send(Command::Play);
    for i in 0..60 {
        if i == 30 {
            handle.send(Command::LaunchScene(0));
        }
        engine.process(&mut buf);
        out.extend_from_slice(&buf);
    }
    out
}

fn assert_finite(out: &[f32]) {
    assert!(out.iter().all(|s| s.is_finite()), "NaN or infinity reached the output");
}

/// Applies `edit` to the demo song's JSON (as a damaged file would hold it)
/// and loads the result.
fn damaged(edit: impl FnOnce(&mut serde_json::Value)) -> Project {
    let mut json: serde_json::Value = serde_json::from_str(&demo::demo_project().save_json().unwrap()).unwrap();
    edit(&mut json);
    Project::from_json(&json.to_string()).expect("still a valid project")
}

#[test]
fn absurd_gains_stay_finite() {
    let p = damaged(|j| {
        j["master"]["volume_db"] = 65535.into();
        j["tracks"][0]["mixer"]["volume_db"] = 1e9.into();
        j["tracks"][1]["sends"][0] = 1e9.into();
        j["tracks"][2]["instrument"]["Synth"]["volume_db"] = 65535.into();
    });
    assert_finite(&play(&p));
}

#[test]
fn absurd_effect_settings_stay_finite() {
    let mut p = demo::demo_project();
    let mut add = |kind: DeviceKind, edit: &dyn Fn(&mut DeviceParams)| {
        let mut d = Device::new(p.alloc_id(), kind);
        edit(&mut d.params);
        p.tracks[2].devices.push(d);
    };
    add(DeviceKind::Compressor, &|d| {
        if let DeviceParams::Compressor(c) = d {
            c.ratio = 0.0;
            c.makeup = 1e9;
        }
    });
    add(DeviceKind::Eq, &|d| {
        if let DeviceParams::Eq(e) = d {
            e.bands[2].gain = -1e9;
        }
    });
    add(DeviceKind::Saturator, &|d| {
        if let DeviceParams::Saturator(s) = d {
            s.drive = 1e9;
        }
    });
    assert_finite(&play(&p));
}

#[test]
fn absurd_instrument_settings_stay_finite() {
    let mut p = demo::demo_project();
    p.tracks[2].instrument = Instrument::Fm(FmParams {
        transpose: 1e9,
        spread: -1e9,
        ..FmParams::default()
    });
    assert_finite(&play(&p));
}

#[test]
fn clamping_leaves_every_preset_unchanged() {
    for pr in library::instruments()
        .into_iter()
        .chain(library::sounds())
        .chain(library::drum_kits())
    {
        assert_eq!(clamped_instrument(pr.instrument), pr.instrument, "{}", pr.name);
    }
    for kind in DeviceKind::ALL {
        let d = kind.default_params();
        assert_eq!(d.clamped(), d, "{}", kind.name());
        for (name, p) in kind.presets() {
            assert_eq!(p.clamped(), p, "{} preset {name}", kind.name());
        }
    }
}

#[test]
fn nan_audio_is_silenced_and_playback_recovers() {
    let mut p = demo::demo_project();
    let id = p.add_track(TrackKind::Audio, Instrument::None);
    let mut left: Vec<f32> = (0..96000).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
    left[100] = f32::NAN;
    left[200] = f32::INFINITY;
    let data = Arc::new(AudioData {
        sample_rate: 48000,
        right: left.clone(),
        left,
    });
    let clip = Clip {
        id: p.alloc_id(),
        length: 4.0,
        content: ClipContent::Audio(AudioRef::new("bad.wav", Some(data))),
        ..Default::default()
    };
    p.track_mut(id).unwrap().slots[0] = Some(clip);
    let out = play(&p);
    assert_finite(&out);
    // Sound comes back after the bad samples instead of the song staying
    // silent until the project is reloaded.
    let tail = &out[out.len() - 512 * 2..];
    assert!(tail.iter().any(|s| s.abs() > 0.01));
}

#[test]
fn squeezed_warp_markers_do_not_hang() {
    let w = WarpSettings {
        enabled: true,
        mode: WarpMode::Beats,
        bpm: 120.0,
        // Two markers a billion beats apart at the same source time.
        markers: vec![
            WarpMarker { beat: 0.0, secs: 0.0 },
            WarpMarker { beat: 1e9, secs: 1e-9 },
        ],
        ..Default::default()
    };
    let started = Instant::now();
    let pb = WarpPlayback::new(&w, 2.0);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(pb.slices().len() <= 1 << 17);

    // So far out that adding half a beat no longer changes the number.
    let far = WarpSettings {
        markers: vec![
            WarpMarker { beat: 1e17, secs: 0.0 },
            WarpMarker { beat: 2e17, secs: 1e-9 },
        ],
        ..w
    };
    let started = Instant::now();
    WarpPlayback::new(&far, 2.0);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn zero_tempo_file_plays() {
    let p = damaged(|j| j["tempo"] = 0.into());
    assert_finite(&play(&p));
}

#[test]
fn nan_in_float_wav_loads_as_silence() {
    let path = std::env::temp_dir().join(format!("tessera-nan-{}.wav", std::process::id()));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 44100,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for s in [0.5, f32::NAN, f32::INFINITY, -0.25] {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    let data = AudioData::load_wav(&path).unwrap();
    std::fs::remove_file(&path).ok();
    assert_eq!(data.left, vec![0.5, 0.0, 0.0, -0.25]);
}
