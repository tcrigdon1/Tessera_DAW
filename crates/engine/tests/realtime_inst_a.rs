//! Verifies that the Analog Synth, Simple Synth and Electric Piano never
//! allocate or free on the audio thread, both inside a running engine
//! (MIDI tracks playing clips, presets swapped live) and when driven
//! directly with every preset and odd block sizes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use tessera_engine::dsp::analog::{self, AnalogParams, AnalogSynth};
use tessera_engine::dsp::electric::{self, ElectricParams, ElectricPiano};
use tessera_engine::dsp::simple::{self, SimpleParams, SimpleSynth};
use tessera_engine::model::{Clip, ClipContent, Instrument, Note, Project, TrackKind};
use tessera_engine::{Command, Engine};

struct CountingAlloc;

thread_local! {
    static WATCH: Cell<bool> = const { Cell::new(false) };
}
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCH.with(|w| w.get()) {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if WATCH.with(|w| w.get()) {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

fn watched<R>(f: impl FnOnce() -> R) -> R {
    WATCH.with(|w| w.set(true));
    let r = f();
    WATCH.with(|w| w.set(false));
    r
}

fn clip(p: &mut Project, notes: Vec<Note>) -> Clip {
    Clip {
        id: p.alloc_id(),
        name: "c".into(),
        color: [0, 0, 0],
        length: 4.0,
        looping: true,
        gain_db: 0.0,
        envelopes: Default::default(),
        content: ClipContent::Midi { notes },
        ..Default::default()
    }
}

fn chords() -> Vec<Note> {
    (0..16)
        .flat_map(|i| {
            (0..3).map(move |k| Note {
                pitch: 40 + ((i * 5 + k * 4) % 36) as u8,
                velocity: 60 + (i * 7 % 60) as u8,
                start: i as f64 * 0.25,
                length: 0.2 + (k as f64) * 0.15,
            })
        })
        .collect()
}

#[test]
fn inst_a_in_engine_do_not_allocate() {
    let mut p = Project::default();
    let tracks = [
        p.add_track(TrackKind::Midi, Instrument::Analog(AnalogParams::default())),
        p.add_track(TrackKind::Midi, Instrument::Simple(SimpleParams::default())),
        p.add_track(TrackKind::Midi, Instrument::Electric(ElectricParams::default())),
    ];
    for &t in &tracks {
        let c = clip(&mut p, chords());
        p.track_mut(t).unwrap().slots[0] = Some(c);
    }
    let (mut engine, mut handle) = Engine::new(48000.0);
    handle.load_project(&p, true);
    let mut buf = vec![0.0f32; 512 * 2];
    engine.process(&mut buf);
    handle.send(Command::LaunchScene(0));

    let mut peak = 0.0f32;
    watched(|| {
        for _ in 0..300 {
            engine.process(&mut buf);
            peak = buf.iter().fold(peak, |a, b| a.max(b.abs()));
        }
    });
    assert!(peak > 0.01, "no audio from the instruments (peak {peak})");

    // Swap presets live.
    let mut swaps: Vec<(usize, Instrument)> = Vec::new();
    for (_, _, pr) in analog::presets() {
        swaps.push((0, Instrument::Analog(pr)));
    }
    for (_, _, pr) in simple::presets() {
        swaps.push((1, Instrument::Simple(pr)));
    }
    for (_, _, pr) in electric::presets() {
        swaps.push((2, Instrument::Electric(pr)));
    }
    for (t, inst) in swaps {
        handle.send(Command::SetInstrument {
            track: tracks[t],
            instrument: inst,
        });
        watched(|| {
            for _ in 0..40 {
                engine.process(&mut buf);
            }
        });
    }
    handle.send(Command::Panic);
    watched(|| {
        for _ in 0..20 {
            engine.process(&mut buf);
        }
    });
    assert!(buf.iter().all(|x| x.is_finite()));
    handle.collect_garbage();
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "audio thread allocated");
}

#[test]
fn inst_a_direct_render_does_not_allocate() {
    let mut a = AnalogSynth::new(96000.0);
    let mut s = SimpleSynth::new(96000.0);
    let mut e = ElectricPiano::new(96000.0);
    let mut left = vec![0.0f32; 2048];
    let mut right = vec![0.0f32; 2048];
    let sizes = [1usize, 7, 32, 128, 333, 2048];
    let mut energy = 0.0f32;
    let a_presets: Vec<AnalogParams> = analog::presets().into_iter().map(|p| p.2).collect();
    let s_presets: Vec<SimpleParams> = simple::presets().into_iter().map(|p| p.2).collect();
    let e_presets: Vec<ElectricParams> = electric::presets().into_iter().map(|p| p.2).collect();
    watched(|| {
        for round in 0..6usize {
            let ap = a_presets[round % a_presets.len()];
            let sp = SimpleParams {
                voice_mode: (round % 4) as f32,
                ..s_presets[round % s_presets.len()]
            };
            let ep = e_presets[round % e_presets.len()];
            for block in 0..30usize {
                let n = sizes[(block + round) % sizes.len()];
                if block % 5 == 0 {
                    for k in 0..4u8 {
                        let pitch = 30 + (round as u8 * 3 + block as u8 + k * 7) % 80;
                        a.note_on(pitch, 100);
                        s.note_on(pitch, 90);
                        e.note_on(pitch, 110);
                    }
                }
                if block % 5 == 3 {
                    for k in 0..4u8 {
                        let pitch = 30 + (round as u8 * 3 + block as u8 - 3 + k * 7) % 80;
                        a.note_off(pitch);
                        s.note_off(pitch);
                        e.note_off(pitch);
                    }
                }
                let (l, r) = (&mut left[..n], &mut right[..n]);
                l.fill(0.0);
                r.fill(0.0);
                a.render(&ap, l, r);
                s.render(&sp, l, r);
                e.render(&ep, l, r);
                energy += l.iter().map(|x| x * x).sum::<f32>();
            }
            a.all_notes_off();
            s.all_notes_off();
            e.all_notes_off();
            a.render(&ap, &mut left, &mut right);
            s.render(&sp, &mut left, &mut right);
            e.render(&ep, &mut left, &mut right);
            a.panic();
            s.panic();
            e.panic();
        }
    });
    assert!(energy > 0.0);
    assert!(left.iter().chain(&right).all(|x| x.is_finite()));
    assert_eq!(ALLOCS.load(Ordering::SeqCst), 0, "instrument allocated while rendering");
}
