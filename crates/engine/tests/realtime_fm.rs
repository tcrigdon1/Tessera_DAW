//! Verifies that the FM Synth never allocates or frees memory while handling
//! notes and rendering, across every preset and every algorithm.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use tessera_engine::dsp::fm::{FmParams, FmSynth, LfoDestination, LfoShape, ALGORITHMS};

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

#[test]
fn fm_synth_does_not_allocate() {
    // Everything that may allocate happens out here.
    let mut patches: Vec<FmParams> = FmParams::presets().iter().map(|(_, p)| *p).collect();
    for alg in 0..ALGORITHMS {
        let mut p = FmParams::electric_piano();
        p.algorithm = alg as u8;
        p.feedback = 0.8;
        p.spread = 12.0;
        p.glide = 0.05;
        p.filter_on = true;
        p.lfo_shape = LfoShape::ALL[alg % LfoShape::ALL.len()];
        p.lfo_dest = LfoDestination::ALL[alg % LfoDestination::ALL.len()];
        p.lfo_amount = 0.3;
        p.pitch_env_amount = 7.0;
        patches.push(p);
    }
    let mut synth = FmSynth::new(48000.0);
    let mut left = vec![0.0f32; 512];
    let mut right = vec![0.0f32; 512];

    watched(|| {
        for (i, p) in patches.iter().enumerate() {
            for block in 0..40usize {
                if block % 4 == 0 {
                    for k in 0..3u8 {
                        synth.note_on(36 + (i as u8 * 5 + block as u8 + k * 4) % 72, 90 + k * 10);
                    }
                }
                if block % 4 == 2 {
                    for k in 0..3u8 {
                        synth.note_off(36 + (i as u8 * 5 + block as u8 - 2 + k * 4) % 72);
                    }
                }
                left.fill(0.0);
                right.fill(0.0);
                synth.render(p, &mut left, &mut right);
            }
            synth.all_notes_off();
            for _ in 0..4 {
                synth.render(p, &mut left, &mut right);
            }
            synth.panic();
        }
        let _ = synth.active_voices();
    });
    assert!(left.iter().chain(&right).all(|x| x.is_finite()));
    assert_eq!(
        ALLOCS.load(Ordering::SeqCst),
        0,
        "FM Synth allocated on the audio thread"
    );
}
