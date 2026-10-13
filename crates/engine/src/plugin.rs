//! Third-party plug-ins (VST3 and CLAP) as seen by the engine.
//!
//! The engine does not load plug-ins itself. The `tessera-plugin-host` crate
//! runs every plug-in in a separate bridge process and implements
//! [`PluginProcessor`] for it; the app registers one processor per plug-in
//! *slot* with [`register`]. A slot is identified by the id of the device that
//! holds an effect plug-in, or by the track id for an instrument plug-in.
//!
//! When a snapshot is compiled ([`crate::fx::FxChain::new`],
//! [`crate::engine::EngineProject::from_model`]) the registered processor is
//! looked up and an `Arc` to it travels with the snapshot to the audio thread,
//! so the audio thread never touches the registry. Old snapshots are dropped
//! on the UI thread, which keeps the last reference to a removed plug-in off
//! the audio thread.
//!
//! What gets saved in the project for each slot (which plug-in, its opaque
//! state) is a [`PluginSlot`] in [`crate::model::Project::plugins`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::{Deserialize, Serialize};

use crate::fx::FxContext;
use crate::model::Id;

/// Plug-in formats Tessera hosts. VST2 is deliberately absent: Steinberg no
/// longer licenses its SDK to new hosts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginFormat {
    Vst3,
    Clap,
}

impl PluginFormat {
    pub fn label(self) -> &'static str {
        match self {
            PluginFormat::Vst3 => "VST3",
            PluginFormat::Clap => "CLAP",
        }
    }
}

/// Identifies an installed plug-in: enough to find and instantiate it again.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginRef {
    pub format: PluginFormat,
    /// The `.vst3` or `.clap` file or bundle.
    pub path: String,
    /// CLAP plug-in id, or the VST3 class id as 32 hex digits.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    /// Takes notes and makes sound (as opposed to an audio effect).
    #[serde(default)]
    pub instrument: bool,
}

/// A plug-in instance in the project, as saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSlot {
    /// Device id (effects) or track id (instruments).
    pub slot: Id,
    pub plugin: PluginRef,
    /// The plug-in's own state, base64 encoded. Kept as-is when the plug-in
    /// is missing on this computer, so saving does not lose it.
    #[serde(default)]
    pub state: Option<String>,
}

/// Settings of an effect plug-in device. The plug-in's parameters live in
/// the plug-in; [`crate::fx::Device::id`] names the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PluginParams {}

/// Settings of an instrument plug-in; the track id names the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PluginInstrument {}

/// A note event inside a block, in frames from the block start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginNote {
    pub frame: u32,
    pub kind: PluginNoteKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginNoteKind {
    On {
        key: u8,
        velocity: u8,
    },
    Off {
        key: u8,
    },
    /// Release every held note.
    AllOff,
}

/// One block of audio for a plug-in. `left`/`right` hold the input and
/// receive the output (instruments get silence in, their output replaces it).
pub struct PluginBlock<'a> {
    pub left: &'a mut [f32],
    pub right: &'a mut [f32],
    pub notes: &'a [PluginNote],
    pub ctx: &'a FxContext,
}

/// The audio-thread side of a hosted plug-in. Implementations must not
/// allocate, free or block indefinitely in these methods.
pub trait PluginProcessor: Send + Sync {
    /// Processes one block in place. Returns false when the plug-in could not
    /// process it (not loaded yet, crashed, too slow); `left`/`right` are then
    /// left untouched for effects and cleared for instruments.
    fn process(&self, block: &mut PluginBlock) -> bool;

    /// Sets parameter `index` (in the plug-in's parameter list) to a
    /// normalised 0..1 value from the next block on (automation).
    fn set_param(&self, index: u32, value: f32);

    /// Asks the plug-in to clear tails and voices (transport jump, panic).
    fn reset(&self) {}
}

type Registry = Mutex<HashMap<Id, Arc<dyn PluginProcessor>>>;

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Makes a processor available to snapshots compiled from now on. UI thread.
pub fn register(slot: Id, processor: Arc<dyn PluginProcessor>) {
    registry()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(slot, processor);
}

/// Removes a slot's processor. Snapshots already sent keep their reference.
pub fn unregister(slot: Id) {
    registry().lock().unwrap_or_else(|e| e.into_inner()).remove(&slot);
}

thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<HashMap<Id, Arc<dyn PluginProcessor>>>> =
        const { std::cell::RefCell::new(None) };
}

/// Runs `f` with `processors` standing in for the registry on this thread,
/// so an export can use its own plug-in instances (at its own sample rate)
/// without touching the ones the live engine plays.
pub fn with_processors<R>(processors: HashMap<Id, Arc<dyn PluginProcessor>>, f: impl FnOnce() -> R) -> R {
    let old = OVERRIDE.with(|o| o.replace(Some(processors)));
    let r = f();
    OVERRIDE.with(|o| *o.borrow_mut() = old);
    r
}

/// The processor registered for a slot. UI thread (snapshot compilation).
pub fn lookup(slot: Id) -> Option<Arc<dyn PluginProcessor>> {
    if let Some(found) = OVERRIDE.with(|o| o.borrow().as_ref().map(|m| m.get(&slot).cloned())) {
        return found;
    }
    registry().lock().unwrap_or_else(|e| e.into_inner()).get(&slot).cloned()
}

/// Minimal base64 (standard alphabet, padded) for plug-in state in projects.
pub mod base64 {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    pub fn decode(text: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(text.len() / 4 * 3);
        let mut acc = 0u32;
        let mut bits = 0;
        for c in text.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => break,
                b' ' | b'\n' | b'\r' | b'\t' => continue,
                _ => return None,
            };
            acc = acc << 6 | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips() {
        for len in 0..40 {
            let data: Vec<u8> = (0..len).map(|i| (i * 37 + 11) as u8).collect();
            let text = base64::encode(&data);
            assert_eq!(text.len() % 4, 0);
            assert_eq!(base64::decode(&text).unwrap(), data);
        }
        assert_eq!(base64::encode(b"Tessera"), "VGVzc2VyYQ==");
        assert!(base64::decode("a$b").is_none());
    }

    struct Gain(f32);

    impl PluginProcessor for Gain {
        fn process(&self, block: &mut PluginBlock) -> bool {
            for s in block.left.iter_mut().chain(block.right.iter_mut()) {
                *s *= self.0;
            }
            true
        }
        fn set_param(&self, _index: u32, _value: f32) {}
    }

    #[test]
    fn registry_hands_out_registered_processors() {
        register(9_000_001, Arc::new(Gain(0.5)));
        let p = lookup(9_000_001).unwrap();
        let mut l = [1.0f32; 4];
        let mut r = [1.0f32; 4];
        let ctx = FxContext::new(48000.0, 120.0);
        assert!(p.process(&mut PluginBlock {
            left: &mut l,
            right: &mut r,
            notes: &[],
            ctx: &ctx,
        }));
        assert_eq!(l, [0.5; 4]);
        unregister(9_000_001);
        assert!(lookup(9_000_001).is_none());
    }
}
