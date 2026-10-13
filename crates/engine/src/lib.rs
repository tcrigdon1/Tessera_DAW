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
//! * [`warp`] and [`analysis`] – warp markers, time-stretching and
//!   transient/tempo detection for audio clips.

pub mod analysis;
pub mod atomic;
pub mod automation;
pub mod demo;
pub mod dsp;
pub mod engine;
pub mod fx;
pub mod library;
pub mod model;
pub mod render;
pub mod time;
pub mod warp;

pub use engine::{Command, Engine, EngineHandle, EngineProject, SharedState};
pub use model::Project;

/// Maximum number of tracks the engine reports state for.
pub const MAX_TRACKS: usize = 128;
/// Largest block the engine renders in one go. Device buffers larger than this
/// are processed in several chunks.
pub const MAX_BLOCK: usize = 2048;
