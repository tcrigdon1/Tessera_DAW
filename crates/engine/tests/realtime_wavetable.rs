//! Verifies that the Wavetable instrument never allocates or frees memory on
//! the audio thread (note on/off, rendering, panic).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use tessera_engine::dsp::wavetable::{Wavetable, WavetableParams};

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
fn wavetable_does_not_allocate() {
    let presets: Vec<WavetableParams> = (0..WavetableParams::PRESET_NAMES.len())
        .map(WavetableParams::preset)
        .collect();
    let mut synth = Wavetable::new(48000.0);
    let mut left = vec![0.0f32; 512];
    let mut right = vec![0.0f32; 512];

    watched(|| {
        for p in &presets {
            for block in 0..120u32 {
                let note = 36 + ((block * 7) % 60) as u8;
                if block % 3 == 0 {
                    synth.note_on(note, 100);
                    synth.note_on(note + 7, 80);
                }
                // Odd sub-block sizes exercise partial control ticks.
                let split = 100 + (block as usize * 13) % 300;
                let (l1, l2) = left.split_at_mut(split);
                let (r1, r2) = right.split_at_mut(split);
                synth.render(p, l1, r1);
                synth.render(p, l2, r2);
                if block % 4 == 1 {
                    synth.note_off(note);
                    synth.note_off(note + 7);
                }
            }
            synth.all_notes_off();
            for _ in 0..20 {
                synth.render(p, &mut left, &mut right);
            }
            synth.panic();
        }
    });
    assert!(left.iter().all(|x| x.is_finite()));
    assert_eq!(
        ALLOCS.load(Ordering::SeqCst),
        0,
        "wavetable allocated on the audio thread"
    );
}
