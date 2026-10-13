//! Offline rendering (export) without an audio device.
//!
//! [`render`] follows Live's Export Audio/Video dialog: a time range, the
//! master or a single track or return as the source (stems), the option to
//! leave out return and master effects, and Render as Loop.

use std::path::Path;

use crate::engine::{Command, Engine, EngineHandle};
use crate::model::{Id, MixerParams, Project};

/// What to render.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RenderSource {
    /// The master output.
    Master,
    /// One track's output (a stem).
    Track(Id),
    /// One return track's output, fed by every track's sends.
    Return(Id),
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    /// The engine runs at this rate (no resampling).
    pub sample_rate: u32,
    /// Start of the range, in beats.
    pub start: f64,
    /// Length of the range, in beats.
    pub length: f64,
    pub source: RenderSource,
    /// For a track: render it as if soloed, through the returns its sends
    /// feed and the master chain and fader. When false, only its own
    /// post-fader output. For a return: whether the master chain and fader
    /// apply. Ignored for the master.
    pub include_return_and_master_fx: bool,
    /// Wrap everything that sounds past the end of the range (reverb and
    /// delay tails, releases) back onto its start, so the file loops
    /// seamlessly. The length of the result is exactly the range.
    pub render_as_loop: bool,
    /// Seconds rendered after the range with the transport stopped, so
    /// releases and effect tails ring out. When looping, 0 means one more
    /// range length.
    pub tail_secs: f64,
}

impl RenderOptions {
    /// The master, from `start` for `length` beats, without tail.
    pub fn master(sample_rate: u32, start: f64, length: f64) -> Self {
        Self {
            sample_rate,
            start,
            length,
            source: RenderSource::Master,
            include_return_and_master_fx: true,
            render_as_loop: false,
            tail_secs: 0.0,
        }
    }
}

/// Every source a stem export renders: each track, then each return.
pub fn stem_sources(project: &Project) -> Vec<(RenderSource, String)> {
    project
        .tracks
        .iter()
        .map(|t| (RenderSource::Track(t.id), t.name.clone()))
        .chain(
            project
                .returns
                .iter()
                .map(|r| (RenderSource::Return(r.id), r.name.clone())),
        )
        .collect()
}

/// Number of frames `beats` take at the project tempo.
fn frames_for(project: &Project, sample_rate: u32, beats: f64) -> usize {
    (beats.max(0.0) * 60.0 / project.tempo * sample_rate as f64).ceil() as usize
}

/// The project as the engine should play it for `o`, and whether tracks
/// reach the master directly. `None` when the source does not exist.
fn prepare(project: &Project, o: &RenderOptions) -> Option<(Project, bool)> {
    let mut p = project.clone();
    // Exports never include the click and play the range straight through.
    p.metronome = false;
    p.loop_enabled = false;
    let strip_master = |p: &mut Project| {
        p.master_devices.clear();
        p.master = MixerParams::default();
    };
    match o.source {
        RenderSource::Master => Some((p, true)),
        RenderSource::Track(id) => {
            p.track(id)?;
            if o.include_return_and_master_fx {
                for t in p.tracks.iter_mut() {
                    t.mixer.solo = t.id == id;
                }
            } else {
                for t in p.tracks.iter_mut() {
                    t.mixer.solo = false;
                    if t.id != id {
                        t.mixer.mute = true;
                    }
                    t.sends.clear();
                }
                p.returns.clear();
                strip_master(&mut p);
            }
            Some((p, true))
        }
        RenderSource::Return(id) => {
            p.returns.iter().position(|r| r.id == id)?;
            for r in p.returns.iter_mut() {
                if r.id != id {
                    r.mixer.mute = true;
                }
            }
            if !o.include_return_and_master_fx {
                strip_master(&mut p);
            }
            Some((p, false))
        }
    }
}

fn run(engine: &mut Engine, handle: &mut EngineHandle, out: &mut [f32]) {
    const BLOCK: usize = 512;
    for chunk in out.chunks_mut(BLOCK * 2) {
        engine.process(chunk);
        handle.collect_garbage();
    }
}

/// Renders interleaved stereo samples as described by `o`.
pub fn render(project: &Project, o: &RenderOptions) -> Vec<f32> {
    let range = frames_for(project, o.sample_rate, o.length);
    let tail_secs = if o.render_as_loop && o.tail_secs <= 0.0 {
        o.length.max(0.0) * 60.0 / project.tempo
    } else {
        o.tail_secs.max(0.0)
    };
    let tail = (tail_secs * o.sample_rate as f64).ceil() as usize;
    let Some((p, direct)) = prepare(project, o) else {
        let len = if o.render_as_loop { range } else { range + tail };
        return vec![0.0; len * 2];
    };

    let (mut engine, mut handle) = Engine::new(o.sample_rate as f32);
    engine.set_offline(true);
    engine.set_direct_outputs(direct);
    handle.load_project(&p, true);
    handle.send(Command::SetPosition(o.start.max(0.0)));
    handle.send(Command::Play);

    let mut out = vec![0.0f32; range * 2];
    run(&mut engine, &mut handle, &mut out);
    if tail > 0 {
        // Stop at the end of the range: notes release, effects ring out.
        handle.send(Command::Stop);
        let mut rest = vec![0.0f32; tail * 2];
        run(&mut engine, &mut handle, &mut rest);
        if o.render_as_loop {
            if !out.is_empty() {
                let n = out.len();
                for (i, s) in rest.iter().enumerate() {
                    out[i % n] += s;
                }
                for s in out.iter_mut() {
                    *s = s.clamp(-1.0, 1.0);
                }
            }
        } else {
            out.extend_from_slice(&rest);
        }
    }
    out
}

/// Renders the master from the start of the Arrangement for `beats` beats
/// and returns interleaved stereo samples.
pub fn render_arrangement(project: &Project, sample_rate: u32, beats: f64) -> Vec<f32> {
    render(project, &RenderOptions::master(sample_rate, 0.0, beats))
}

/// Writes 16-bit stereo WAV without dither (see [`crate::export`] for the
/// full set of formats).
pub fn write_wav(path: &Path, samples: &[f32], sample_rate: u32) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)?;
    }
    w.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth::SynthParams;
    use crate::fx::{Device, DeviceKind};
    use crate::model::{ArrangementClip, Clip, ClipContent, Instrument, Note, TrackKind};

    /// The render as it was before [`render`] existed.
    fn legacy_render(project: &Project, sample_rate: u32, beats: f64) -> Vec<f32> {
        let (mut engine, mut handle) = Engine::new(sample_rate as f32);
        engine.set_offline(true);
        handle.load_project(project, true);
        handle.send(Command::Play);
        let total = frames_for(project, sample_rate, beats);
        let mut out = vec![0.0f32; total * 2];
        let mut done = 0;
        while done < total {
            let n = 512.min(total - done);
            engine.process(&mut out[done * 2..(done + n) * 2]);
            handle.collect_garbage();
            done += n;
        }
        out
    }

    fn peak(s: &[f32]) -> f32 {
        s.iter().fold(0.0f32, |m, x| m.max(x.abs()))
    }

    fn notes_at(starts: &[f64], pitch: u8) -> Vec<Note> {
        starts
            .iter()
            .map(|&start| Note {
                pitch,
                velocity: 110,
                start,
                length: 0.5,
            })
            .collect()
    }

    fn add_midi_track(p: &mut Project, notes: Vec<Note>, len: f64) -> Id {
        let id = p.add_track(TrackKind::Midi, Instrument::Synth(SynthParams::default()));
        let cid = p.alloc_id();
        let aid = p.alloc_id();
        let clip = Clip {
            id: cid,
            name: "c".into(),
            length: len,
            looping: false,
            content: ClipContent::Midi { notes },
            ..Default::default()
        };
        p.track_mut(id).unwrap().arrangement.push(ArrangementClip {
            id: aid,
            start: 0.0,
            length: len,
            clip,
        });
        id
    }

    /// Two synth tracks, no returns and an empty master chain.
    fn plain_project() -> (Project, Id, Id) {
        let mut p = Project::default();
        p.master_devices.clear();
        p.returns.clear();
        let a = add_midi_track(&mut p, notes_at(&[0.0, 2.0], 60), 8.0);
        let b = add_midi_track(&mut p, notes_at(&[1.0, 3.0], 67), 8.0);
        p.fix_sends();
        (p, a, b)
    }

    #[test]
    fn master_render_matches_previous_render() {
        let p = crate::demo::demo_project();
        let p = Project {
            metronome: false,
            loop_enabled: false,
            ..p
        };
        let new = render_arrangement(&p, 44100, 8.0);
        let old = legacy_render(&p, 44100, 8.0);
        assert_eq!(new.len(), old.len());
        assert!(peak(&new) > 0.05);
        assert_eq!(new, old);
    }

    #[test]
    fn stems_without_fx_sum_to_the_master() {
        let (p, a, b) = plain_project();
        let o = RenderOptions::master(44100, 0.0, 4.0);
        let master = render(&p, &o);
        let stem = |id| {
            render(
                &p,
                &RenderOptions {
                    source: RenderSource::Track(id),
                    include_return_and_master_fx: false,
                    ..o.clone()
                },
            )
        };
        let (sa, sb) = (stem(a), stem(b));
        assert!(peak(&sa) > 0.05 && peak(&sb) > 0.05);
        // Track a plays on beats 0 and 2 only, so its stem is silent while b
        // alone plays (after a's release, before beat 2).
        let sr = 44100.0 * 0.5; // frames per beat at 120 BPM
        let window = |s: &[f32], from: f64, to: f64| peak(&s[(from * sr) as usize * 2..(to * sr) as usize * 2]);
        assert!(window(&sa, 1.6, 1.95) < 1e-4, "{}", window(&sa, 1.6, 1.95));
        assert!(window(&sb, 1.0, 1.4) > 0.05);
        assert!(window(&sb, 0.0, 0.95) < 1e-6);
        let worst = master
            .iter()
            .zip(sa.iter().zip(&sb))
            .map(|(m, (x, y))| (m - (x + y)).abs())
            .fold(0.0f32, f32::max);
        assert!(worst < 1e-4, "difference {worst}");
        // A source that does not exist renders silence of the right length.
        let none = stem(9999);
        assert_eq!(none.len(), master.len());
        assert_eq!(peak(&none), 0.0);
    }

    #[test]
    fn muted_and_soloed_stems() {
        let (mut p, a, b) = plain_project();
        // Track a is soloed in the project: b's stem must still sound.
        p.track_mut(a).unwrap().mixer.solo = true;
        let o = RenderOptions {
            source: RenderSource::Track(b),
            include_return_and_master_fx: true,
            ..RenderOptions::master(44100, 0.0, 4.0)
        };
        assert!(peak(&render(&p, &o)) > 0.05);
        // A track muted in the project renders silent.
        p.track_mut(b).unwrap().mixer.mute = true;
        assert_eq!(peak(&render(&p, &o)), 0.0);
        let o = RenderOptions {
            include_return_and_master_fx: false,
            ..o
        };
        assert_eq!(peak(&render(&p, &o)), 0.0);
    }

    #[test]
    fn return_stem_carries_only_the_sends() {
        let (mut p, a, _b) = plain_project();
        let ret = p.add_return(Some(DeviceKind::Reverb)).unwrap();
        let other = p.add_return(None).unwrap();
        p.fix_sends();
        p.track_mut(a).unwrap().sends = vec![0.0, 0.0];
        let o = RenderOptions {
            source: RenderSource::Return(ret),
            include_return_and_master_fx: false,
            ..RenderOptions::master(44100, 0.0, 4.0)
        };
        let r = render(&p, &o);
        assert!(peak(&r) > 0.01);
        // The other return gets the same send but has no devices: its stem
        // is exactly the dry send, which is a's signal alone (b sends nothing).
        let dry = render(
            &p,
            &RenderOptions {
                source: RenderSource::Return(other),
                ..o.clone()
            },
        );
        let a_stem = render(
            &p,
            &RenderOptions {
                source: RenderSource::Track(a),
                ..o.clone()
            },
        );
        let worst = dry
            .iter()
            .zip(&a_stem)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        // Return faders ramp in from silence over the first block.
        let skip = 1024;
        let worst_after = dry[skip..]
            .iter()
            .zip(&a_stem[skip..])
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(worst_after < 1e-4, "{worst_after} (incl. ramp {worst})");
        // With no sends the return is silent.
        p.track_mut(a).unwrap().sends = vec![-70.0, -70.0];
        assert_eq!(peak(&render(&p, &o)), 0.0);
    }

    #[test]
    fn soloed_stem_with_fx_goes_through_returns_and_master() {
        let (mut p, a, _b) = plain_project();
        p.add_return(Some(DeviceKind::Delay)).unwrap();
        p.fix_sends();
        p.track_mut(a).unwrap().sends = vec![0.0];
        let id = p.alloc_id();
        p.master_devices.push(Device::new(id, DeviceKind::Limiter));
        let with = RenderOptions {
            source: RenderSource::Track(a),
            include_return_and_master_fx: true,
            ..RenderOptions::master(44100, 0.0, 4.0)
        };
        let without = RenderOptions {
            include_return_and_master_fx: false,
            ..with.clone()
        };
        let (w, wo) = (render(&p, &with), render(&p, &without));
        let diff = w.iter().zip(&wo).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        assert!(diff > 0.01, "returns/master made no difference");
    }

    #[test]
    fn start_position_offsets_the_render() {
        let mut p = Project::default();
        p.master_devices.clear();
        p.returns.clear();
        add_midi_track(&mut p, notes_at(&[2.5], 60), 8.0);
        add_midi_track(&mut p, notes_at(&[0.0], 64), 8.0);
        p.fix_sends();
        let full = render(&p, &RenderOptions::master(44100, 0.0, 4.0));
        let late = render(&p, &RenderOptions::master(44100, 2.0, 2.0));
        let half = full.len() / 2;
        assert_eq!(late.len(), half);
        // Before beat 2.5 the late render is silent (the beat 0 note was
        // never played), while the full render has it sounding at the start.
        // Interleaved samples per beat at 120 BPM, 44.1 kHz.
        let spb = 22050 * 2;
        // Up to beat 2.5 the late render is silent (the beat 0 note was
        // never played), while the full render has it at its start.
        assert_eq!(peak(&late[..spb / 2]), 0.0);
        assert!(peak(&full[..spb / 2]) > 0.05);
        // From beat 2.5 on, the late render is the full render shifted.
        let at = spb / 2;
        let worst = late[at..]
            .iter()
            .zip(&full[half + at..])
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max);
        assert!(peak(&late[at..]) > 0.05);
        assert!(worst < 1e-4, "{worst}");
    }

    #[test]
    fn render_as_loop_keeps_the_range_length_and_wraps_the_tail() {
        let (mut p, a, _b) = plain_project();
        // A note held to the very end: its release wraps to the start.
        p.track_mut(a).unwrap().arrangement[0]
            .clip
            .notes_mut()
            .unwrap()
            .push(Note {
                pitch: 48,
                velocity: 120,
                start: 3.0,
                length: 1.0,
            });
        let base = RenderOptions::master(44100, 0.0, 4.0);
        let plain = render(&p, &base);
        let looped = render(
            &p,
            &RenderOptions {
                render_as_loop: true,
                ..base.clone()
            },
        );
        assert_eq!(looped.len(), plain.len());
        assert_ne!(looped, plain);
        let tailed = render(
            &p,
            &RenderOptions {
                tail_secs: 0.5,
                ..base.clone()
            },
        );
        assert_eq!(tailed.len(), plain.len() + 22050 * 2);
        assert_eq!(tailed[..plain.len()], plain[..]);
        // The tail is what the loop adds to the start.
        let tail_peak = peak(&tailed[plain.len()..]);
        assert!(tail_peak > 1e-3);
        let looped_tail = render(
            &p,
            &RenderOptions {
                render_as_loop: true,
                tail_secs: 0.5,
                ..base
            },
        );
        for i in 0..22050 * 2 {
            let want = (plain[i] + tailed[plain.len() + i]).clamp(-1.0, 1.0);
            assert!((looped_tail[i] - want).abs() < 1e-6);
        }
        assert_eq!(looped_tail[22050 * 2..], plain[22050 * 2..]);
    }

    #[test]
    fn stem_sources_lists_tracks_then_returns() {
        let p = crate::demo::demo_project();
        let s = stem_sources(&p);
        assert_eq!(s.len(), p.tracks.len() + p.returns.len());
        assert_eq!(s[0], (RenderSource::Track(p.tracks[0].id), p.tracks[0].name.clone()));
        let last = p.returns.last().unwrap();
        assert_eq!(s.last().unwrap(), &(RenderSource::Return(last.id), last.name.clone()));
    }
}
