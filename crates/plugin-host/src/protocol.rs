//! Control messages between Tessera and a bridge process.
//!
//! Messages are JSON, one per line, on the bridge's stdin (to the bridge)
//! and stdout (from the bridge). Lines from the bridge start with
//! [`PREFIX`]; anything else on stdout is something a plug-in printed and is
//! passed on to stderr. Audio does not travel here: it goes through shared
//! memory (see [`crate::shm`]).

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tessera_engine::plugin::PluginFormat;

/// Marks protocol lines on the bridge's stdout.
pub const PREFIX: &str = "\u{1e}TSR ";

/// What a scan found in one plug-in file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub format: PluginFormat,
    pub path: PathBuf,
    /// CLAP id or VST3 class id (32 hex digits).
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub instrument: bool,
    /// Sub-categories or features as reported by the plug-in (e.g. "Fx|Reverb").
    pub category: String,
}

impl PluginInfo {
    pub fn to_ref(&self) -> tessera_engine::plugin::PluginRef {
        tessera_engine::plugin::PluginRef {
            format: self.format,
            path: self.path.to_string_lossy().to_string(),
            id: self.id.clone(),
            name: self.name.clone(),
            vendor: self.vendor.clone(),
            instrument: self.instrument,
        }
    }
}

/// One plug-in parameter. Values are normalised to 0..1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamDesc {
    /// The plug-in's own id for the parameter.
    pub id: u32,
    pub name: String,
    /// Group or unit the parameter belongs to, may be empty.
    pub group: String,
    pub default: f64,
    /// Number of steps for stepped parameters, 0 for continuous ones.
    pub steps: u32,
    pub automatable: bool,
    pub hidden: bool,
    pub read_only: bool,
    pub value: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ToBridge {
    /// Instantiate and activate a plug-in.
    Load {
        format: PluginFormat,
        path: PathBuf,
        id: String,
        sample_rate: f64,
        max_frames: u32,
    },
    /// Set a parameter from Tessera's UI (normalised value).
    SetParam {
        index: u32,
        value: f64,
    },
    /// Ask for the plug-in's state; answered with [`FromBridge::State`].
    GetState {
        request: u64,
    },
    /// Restore a state saved earlier (base64).
    SetState {
        data: String,
    },
    /// Open the plug-in's own editor window.
    ShowEditor {
        title: String,
    },
    HideEditor,
    /// Shut down cleanly.
    Quit,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FromBridge {
    /// Shared memory and signals are connected.
    Ready,
    Loaded {
        name: String,
        params: Vec<ParamDesc>,
        latency: u32,
        has_editor: bool,
        audio_inputs: u32,
        audio_outputs: u32,
    },
    Failed {
        error: String,
    },
    /// Parameter values changed (in the plug-in's editor, by automation or a
    /// preset): (index, value, display text).
    ParamValues {
        values: Vec<(u32, f64, String)>,
    },
    /// The parameter list changed; replaces it.
    Params {
        params: Vec<ParamDesc>,
    },
    State {
        request: u64,
        data: Option<String>,
    },
    /// The plug-in reports that its state changed (for the unsaved marker).
    Dirty,
    Latency {
        samples: u32,
    },
    EditorOpened,
    EditorClosed {
        error: Option<String>,
    },
    /// Result of a scan (`--plugin-bridge scan <path>`).
    Scanned {
        plugins: Vec<PluginInfo>,
    },
}

/// Writes one message as a protocol line.
pub fn write_line<T: Serialize>(w: &mut impl Write, msg: &T, prefixed: bool) -> io::Result<()> {
    let mut line = String::new();
    if prefixed {
        line.push_str(PREFIX);
    }
    line.push_str(&serde_json::to_string(msg).map_err(io::Error::other)?);
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()
}

/// What reading a line from the bridge produced.
pub enum Line<T> {
    Message(T),
    /// Something a plug-in printed.
    Other(String),
    Eof,
}

/// Reads the next line, decoding it when it carries the prefix (or always,
/// when `prefixed` is false).
pub fn read_line<T: for<'de> Deserialize<'de>>(r: &mut impl BufRead, prefixed: bool) -> io::Result<Line<T>> {
    let mut buf = Vec::new();
    if r.read_until(b'\n', &mut buf)? == 0 {
        return Ok(Line::Eof);
    }
    let text = String::from_utf8_lossy(&buf);
    let text = text.trim_end_matches(['\n', '\r']);
    let body = if prefixed {
        match text.find(PREFIX) {
            Some(i) => &text[i + PREFIX.len()..],
            None => return Ok(Line::Other(text.to_string())),
        }
    } else {
        text
    };
    match serde_json::from_str(body) {
        Ok(m) => Ok(Line::Message(m)),
        Err(_) => Ok(Line::Other(text.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip_and_skip_noise() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"plugin says hello\n");
        write_line(&mut buf, &FromBridge::Latency { samples: 64 }, true).unwrap();
        // A plug-in printing without a newline right before our line.
        buf.extend_from_slice(b"partial");
        write_line(&mut buf, &FromBridge::Dirty, true).unwrap();
        let mut r = io::Cursor::new(buf);
        assert!(matches!(read_line::<FromBridge>(&mut r, true).unwrap(), Line::Other(s) if s == "plugin says hello"));
        assert!(matches!(
            read_line::<FromBridge>(&mut r, true).unwrap(),
            Line::Message(FromBridge::Latency { samples: 64 })
        ));
        assert!(matches!(
            read_line::<FromBridge>(&mut r, true).unwrap(),
            Line::Message(FromBridge::Dirty)
        ));
        assert!(matches!(read_line::<FromBridge>(&mut r, true).unwrap(), Line::Eof));

        let mut buf = Vec::new();
        write_line(&mut buf, &ToBridge::SetParam { index: 3, value: 0.25 }, false).unwrap();
        let mut r = io::Cursor::new(buf);
        assert!(matches!(
            read_line::<ToBridge>(&mut r, false).unwrap(),
            Line::Message(ToBridge::SetParam { index: 3, .. })
        ));
    }
}
