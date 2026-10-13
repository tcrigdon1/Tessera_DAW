//! Hosting VST3 and CLAP plug-ins for Tessera.
//!
//! Every plug-in instance runs in its own bridge process so a crashing
//! plug-in can't take the DAW down with it. Audio moves through shared
//! memory, woken by OS signals; control messages travel as JSON lines over
//! the bridge's stdin and stdout. See `client` for Tessera's side and
//! `bridge` for the child process.

pub mod bridge;
pub mod client;
pub mod protocol;
pub mod scan;
pub mod shm;
pub mod signal;

pub use client::{BridgeCommand, Event, PluginInstance, Processor, Status};
pub use protocol::{ParamDesc, PluginInfo};
pub use scan::{ScanCache, ScanEntry};
