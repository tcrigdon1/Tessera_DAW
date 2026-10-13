//! Lock-free audio taps from the audio thread to the UI, for analysis
//! devices (Spectrum, Tuner).
//!
//! A [`Scope`] is a fixed ring of the latest [`SCOPE_LEN`] mono samples
//! stored as atomics. The audio thread pushes samples wait-free; the UI copies
//! the most recent window with [`Scope::snapshot`]. A sample being
//! overwritten while the UI copies it may be read torn between old and new,
//! which is harmless for display purposes.
//!
//! Analysis runtimes get their scope from [`tap`] in
//! `EffectRuntime::attach` (UI thread) and keep the `Arc` for their whole
//! life; the panel looks it up with [`find`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::atomic::AtomicF32;
use crate::model::Id;

/// Ring length in samples (a power of two).
pub const SCOPE_LEN: usize = 8192;
const MASK: usize = SCOPE_LEN - 1;

/// Number of small result slots on a scope.
pub const SLOTS: usize = 4;

pub struct Scope {
    buf: Box<[AtomicU32]>,
    /// Total number of samples ever written.
    write: AtomicUsize,
    sample_rate: AtomicF32,
    /// Small values computed on the audio thread (e.g. a level).
    pub slots: [AtomicF32; SLOTS],
}

impl Default for Scope {
    fn default() -> Self {
        Self::new()
    }
}

impl Scope {
    /// Allocates; UI thread only.
    pub fn new() -> Self {
        Self {
            buf: (0..SCOPE_LEN).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicUsize::new(0),
            sample_rate: AtomicF32::new(48000.0),
            slots: Default::default(),
        }
    }

    pub fn set_sample_rate(&self, sr: f32) {
        self.sample_rate.store(sr);
    }

    /// Sample rate of the pushed audio.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate.load()
    }

    /// Total number of samples pushed so far (wraps). The UI can compare it
    /// between frames to tell whether new audio arrived.
    pub fn written(&self) -> usize {
        self.write.load(Ordering::Acquire)
    }

    /// Pushes one stereo frame as its mono sum. Audio thread, wait-free.
    #[inline]
    pub fn push(&self, l: f32, r: f32) {
        self.push_mono(0.5 * (l + r));
    }

    /// Pushes one mono sample. Audio thread, wait-free.
    #[inline]
    pub fn push_mono(&self, x: f32) {
        let w = self.write.load(Ordering::Relaxed);
        self.buf[w & MASK].store(x.to_bits(), Ordering::Relaxed);
        self.write.store(w.wrapping_add(1), Ordering::Release);
    }

    /// Pushes a block of mono samples. Audio thread, wait-free.
    pub fn push_block(&self, samples: &[f32]) {
        let w = self.write.load(Ordering::Relaxed);
        // Only the last SCOPE_LEN samples survive anyway.
        let skip = samples.len().saturating_sub(SCOPE_LEN);
        for (i, &x) in samples.iter().enumerate().skip(skip) {
            self.buf[w.wrapping_add(i) & MASK].store(x.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w.wrapping_add(samples.len()), Ordering::Release);
    }

    /// Pushes a stereo block, mixing `left` and `right` with the given gains
    /// (e.g. 0.5 / 0.5 for a mono sum, 1 / 0 for the left channel).
    pub fn push_stereo(&self, left: &[f32], right: &[f32], gl: f32, gr: f32) {
        let n = left.len().min(right.len());
        let w = self.write.load(Ordering::Relaxed);
        let skip = n.saturating_sub(SCOPE_LEN);
        for i in skip..n {
            let x = left[i] * gl + right[i] * gr;
            self.buf[w.wrapping_add(i) & MASK].store(x.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w.wrapping_add(n), Ordering::Release);
    }

    /// Copies the latest [`SCOPE_LEN`] samples, oldest first, into `out`
    /// (resized). UI thread.
    pub fn snapshot(&self, out: &mut Vec<f32>) {
        self.snapshot_len(out, SCOPE_LEN);
    }

    /// Copies the latest `len` (at most [`SCOPE_LEN`]) samples, oldest first.
    pub fn snapshot_len(&self, out: &mut Vec<f32>, len: usize) {
        let len = len.min(SCOPE_LEN);
        out.clear();
        out.reserve(len);
        let w = self.write.load(Ordering::Acquire);
        let start = w.wrapping_sub(len);
        out.extend((0..len).map(|i| f32::from_bits(self.buf[start.wrapping_add(i) & MASK].load(Ordering::Relaxed))));
    }

    /// Clears the ring (UI or audio thread; used by `reset`).
    pub fn clear(&self) {
        for v in self.buf.iter() {
            v.store(0, Ordering::Relaxed);
        }
    }
}

fn registry() -> &'static Mutex<HashMap<Id, Weak<Scope>>> {
    static REG: OnceLock<Mutex<HashMap<Id, Weak<Scope>>>> = OnceLock::new();
    REG.get_or_init(Default::default)
}

/// The scope of `device`, created if none is alive. UI thread only (locks
/// and may allocate).
pub fn tap(device: Id) -> Arc<Scope> {
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(s) = reg.get(&device).and_then(Weak::upgrade) {
        return s;
    }
    reg.retain(|_, w| w.strong_count() > 0);
    let s = Arc::new(Scope::new());
    reg.insert(device, Arc::downgrade(&s));
    s
}

/// The live scope of `device`, if its runtime exists. UI thread only.
pub fn find(device: Id) -> Option<Arc<Scope>> {
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.get(&device).and_then(Weak::upgrade)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_latest_samples_in_order() {
        let s = Scope::new();
        for i in 0..(SCOPE_LEN + 100) {
            s.push_mono(i as f32);
        }
        let mut out = Vec::new();
        s.snapshot(&mut out);
        assert_eq!(out.len(), SCOPE_LEN);
        assert_eq!(out[0], 100.0);
        assert_eq!(*out.last().unwrap(), (SCOPE_LEN + 99) as f32);
        s.snapshot_len(&mut out, 4);
        assert_eq!(out, vec![8288.0, 8289.0, 8290.0, 8291.0]);
        let block: Vec<f32> = (0..10).map(|i| -(i as f32)).collect();
        s.push_block(&block);
        s.snapshot_len(&mut out, 3);
        assert_eq!(out, vec![-7.0, -8.0, -9.0]);
        s.push_stereo(&[1.0, 2.0], &[3.0, 4.0], 0.5, 0.5);
        s.snapshot_len(&mut out, 2);
        assert_eq!(out, vec![2.0, 3.0]);
    }

    #[test]
    fn registry_shares_scopes_while_alive() {
        let id: Id = 0xdead_beef_0001;
        assert!(find(id).is_none());
        let a = tap(id);
        let b = tap(id);
        assert!(Arc::ptr_eq(&a, &b));
        assert!(find(id).is_some());
        drop(a);
        drop(b);
        assert!(find(id).is_none());
    }
}
