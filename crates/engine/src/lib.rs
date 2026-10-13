//! Tessera audio engine.
//!
//! The crate is split into three layers:
//!
//! * [`model`] – the serialisable project document (tracks, clips, scenes,
//!   mixer settings). The UI owns and edits this.
//! * [`engine`] – the real-time audio engine. The UI compiles the model into an
//!   immutable [`engine::EngineProject`] snapshot and hands it to the audio
//!   thread through a lock-free queue. Small, frequent changes (fader moves,
//!   clip launches, transport) travel as [`engine::Command`]s. The audio thread
//!   never allocates, locks or frees memory.
//! * [`dsp`] – instruments and DSP building blocks used by the engine.
//! * [`automation`] – breakpoint envelopes for mixer and device parameters.
//! * [`midi`] and [`midi_fx`] – MIDI input, scales, note editing, controller
//!   mappings and MIDI effects.
//! * [`warp`] and [`analysis`] – warp markers, time-stretching and
//!   transient/tempo detection for audio clips.

pub mod analysis;
pub mod arrange;
pub mod atomic;
pub mod audio_to_midi;
pub mod automation;
pub mod capture;
pub mod clip_edit;
pub mod demo;
pub mod dsp;
pub mod engine;
pub mod export;
pub mod fx;
pub mod groove;
pub mod history;
pub mod library;
pub mod midi;
pub mod midi_fx;
pub mod midifile;
pub mod migrate;
pub mod model;
pub mod plugin;
pub mod render;
pub mod resample;
pub mod time;
pub mod timeline;
pub mod warp;

pub use engine::{Command, Engine, EngineHandle, EngineProject, SharedState};
pub use model::Project;

/// Maximum number of tracks the engine reports state for.
pub const MAX_TRACKS: usize = 128;
/// Largest block the engine renders in one go. Device buffers larger than this
/// are processed in several chunks.
pub const MAX_BLOCK: usize = 2048;
