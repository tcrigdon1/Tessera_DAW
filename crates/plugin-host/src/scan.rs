//! Finding installed plug-ins.
//!
//! Plug-in files are looked for in the standard VST3 and CLAP folders of the
//! operating system (plus `CLAP_PATH`, `VST3_PATH` and folders the user adds).
//! Each file is opened by a short-lived bridge process (`scan <path>`) so a
//! plug-in that crashes or hangs while loading cannot take Tessera down; it
//! is listed as failed instead. Results are cached by file and modification
//! time.

use std::collections::HashMap;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tessera_engine::plugin::PluginFormat;

use crate::client::BridgeCommand;
use crate::protocol::{self, FromBridge, Line, PluginInfo};

/// How long one plug-in file may take to scan.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(30);

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn env_paths(var: &str) -> Vec<PathBuf> {
    std::env::var_os(var)
        .map(|v| std::env::split_paths(&v).collect())
        .unwrap_or_default()
}

/// The standard folders for a format on this operating system, followed by
/// the folders in `CLAP_PATH` / `VST3_PATH`.
pub fn standard_folders(format: PluginFormat) -> Vec<PathBuf> {
    let mut v = Vec::new();
    let sub = match format {
        PluginFormat::Vst3 => "VST3",
        PluginFormat::Clap => "CLAP",
    };
    if cfg!(target_os = "windows") {
        if let Some(common) = std::env::var_os("COMMONPROGRAMFILES") {
            v.push(PathBuf::from(common).join(sub));
        } else {
            v.push(PathBuf::from(r"C:\Program Files\Common Files").join(sub));
        }
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            v.push(PathBuf::from(local).join("Programs").join("Common").join(sub));
        }
    } else if cfg!(target_os = "macos") {
        v.push(PathBuf::from("/Library/Audio/Plug-Ins").join(sub));
        if let Some(h) = home() {
            v.push(h.join("Library/Audio/Plug-Ins").join(sub));
        }
    } else {
        let dot = match format {
            PluginFormat::Vst3 => ".vst3",
            PluginFormat::Clap => ".clap",
        };
        let lower = sub.to_lowercase();
        if let Some(h) = home() {
            v.push(h.join(dot));
        }
        v.push(PathBuf::from("/usr/lib").join(&lower));
        v.push(PathBuf::from("/usr/local/lib").join(&lower));
    }
    v.extend(env_paths(match format {
        PluginFormat::Vst3 => "VST3_PATH",
        PluginFormat::Clap => "CLAP_PATH",
    }));
    v
}

fn format_of(path: &Path) -> Option<PluginFormat> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    match ext.as_str() {
        "vst3" => Some(PluginFormat::Vst3),
        "clap" => Some(PluginFormat::Clap),
        _ => None,
    }
}

/// Plug-in files and bundles under `dir` (searched a few levels deep, so
/// vendor sub-folders are found).
pub fn find_plugin_files(dir: &Path) -> Vec<(PluginFormat, PathBuf)> {
    fn walk(dir: &Path, depth: usize, out: &mut Vec<(PluginFormat, PathBuf)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if let Some(f) = format_of(&p) {
                // Bundles are directories ending in .vst3/.clap; don't look
                // inside them for more plug-ins.
                out.push((f, p));
            } else if p.is_dir() && depth > 0 {
                walk(&p, depth - 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, 4, &mut out);
    out
}

/// Every plug-in file in `folders` (both formats), without duplicates.
pub fn find_all(folders: &[PathBuf]) -> Vec<(PluginFormat, PathBuf)> {
    let mut out: Vec<(PluginFormat, PathBuf)> = Vec::new();
    for d in folders {
        for f in find_plugin_files(d) {
            let canon = std::fs::canonicalize(&f.1).unwrap_or_else(|_| f.1.clone());
            if !out
                .iter()
                .any(|(_, p)| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()) == canon)
            {
                out.push(f);
            }
        }
    }
    out
}

fn modified(path: &Path) -> u64 {
    // For bundles the binary inside may change without the folder changing;
    // take the newest of the bundle and its immediate binary folder.
    let t = |p: &Path| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0)
    };
    let mut best = t(path);
    if path.is_dir() {
        for sub in ["Contents", "Contents/MacOS"] {
            best = best.max(t(&path.join(sub)));
        }
    }
    best
}

/// The outcome for one plug-in file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanEntry {
    pub path: PathBuf,
    pub format: PluginFormat,
    pub modified: u64,
    pub plugins: Vec<PluginInfo>,
    /// Why the file could not be scanned.
    pub error: Option<String>,
}

/// Scan results, saved between runs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanCache {
    pub entries: Vec<ScanEntry>,
    /// When the last scan finished (seconds since the epoch).
    #[serde(default)]
    pub scanned_at: u64,
}

impl ScanCache {
    pub fn load(path: &Path) -> ScanCache {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, serde_json::to_string_pretty(self).map_err(io::Error::other)?)
    }

    /// All plug-ins found, instruments first, then by name.
    pub fn plugins(&self) -> Vec<PluginInfo> {
        let mut v: Vec<PluginInfo> = self.entries.iter().flat_map(|e| e.plugins.iter().cloned()).collect();
        v.sort_by(|a, b| {
            b.instrument
                .cmp(&a.instrument)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.format.label().cmp(b.format.label()))
        });
        v
    }

    pub fn failures(&self) -> impl Iterator<Item = &ScanEntry> {
        self.entries.iter().filter(|e| e.error.is_some())
    }
}

/// Scans one file in a bridge process.
pub fn scan_file(bridge: &BridgeCommand, format: PluginFormat, path: &Path) -> ScanEntry {
    let mut entry = ScanEntry {
        path: path.to_path_buf(),
        format,
        modified: modified(path),
        plugins: Vec::new(),
        error: None,
    };
    let mut cmd = bridge.command();
    cmd.arg("scan")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            entry.error = Some(format!("could not start the scanner: {e}"));
            return entry;
        }
    };
    let stdout = child.stdout.take();
    type Outcome = Arc<Mutex<Option<Result<Vec<PluginInfo>, String>>>>;
    let result: Outcome = Arc::new(Mutex::new(None));
    let reader = {
        let result = result.clone();
        std::thread::spawn(move || {
            let Some(out) = stdout else { return };
            let mut r = BufReader::new(out);
            loop {
                match protocol::read_line::<FromBridge>(&mut r, true) {
                    Ok(Line::Message(FromBridge::Scanned { plugins })) => {
                        *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Ok(plugins));
                    }
                    Ok(Line::Message(FromBridge::Failed { error })) => {
                        *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(Err(error));
                    }
                    Ok(Line::Message(_)) | Ok(Line::Other(_)) => {}
                    Ok(Line::Eof) | Err(_) => break,
                }
            }
        })
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < SCAN_TIMEOUT => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                child.kill().ok();
                child.wait().ok();
                break None;
            }
        }
    };
    reader.join().ok();
    let result = result.lock().unwrap_or_else(|e| e.into_inner()).take();
    match (status, result) {
        (_, Some(Ok(plugins))) => entry.plugins = plugins,
        (_, Some(Err(e))) => entry.error = Some(e),
        (None, None) => entry.error = Some("timed out while loading".into()),
        (Some(s), None) => entry.error = Some(format!("crashed while loading ({s})")),
    }
    entry
}

/// Scans every plug-in in `folders`, reusing cached results for files that
/// did not change. `progress` is called with (done, total, current file).
/// Runs a few scanners in parallel.
pub fn scan(
    bridge: &BridgeCommand,
    folders: &[PathBuf],
    cache: &ScanCache,
    progress: &(dyn Fn(usize, usize, &Path) + Sync),
) -> ScanCache {
    let files = find_all(folders);
    let total = files.len();
    let old: HashMap<&Path, &ScanEntry> = cache.entries.iter().map(|e| (e.path.as_path(), e)).collect();
    let todo = Mutex::new(files.into_iter().enumerate().collect::<Vec<_>>());
    let results = Mutex::new(Vec::with_capacity(total));
    let done = Mutex::new(0usize);
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, 4);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let Some((i, (format, path))) = todo.lock().unwrap_or_else(|e| e.into_inner()).pop() else {
                    break;
                };
                let entry = match old.get(path.as_path()) {
                    Some(e) if e.modified == modified(&path) && e.format == format => (*e).clone(),
                    _ => scan_file(bridge, format, &path),
                };
                {
                    let mut d = done.lock().unwrap_or_else(|e| e.into_inner());
                    *d += 1;
                    progress(*d, total, &path);
                }
                results.lock().unwrap_or_else(|e| e.into_inner()).push((i, entry));
            });
        }
    });
    let mut results = results.into_inner().unwrap_or_else(|e| e.into_inner());
    results.sort_by_key(|(i, _)| *i);
    ScanCache {
        entries: results.into_iter().map(|(_, e)| e).collect(),
        scanned_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_plugin_files_and_bundles() {
        let dir = std::env::temp_dir().join(format!("tessera-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Vendor/Sub")).unwrap();
        std::fs::create_dir_all(dir.join("Synth.vst3/Contents/x86_64-linux")).unwrap();
        std::fs::write(dir.join("Synth.vst3/Contents/x86_64-linux/Synth.so"), b"").unwrap();
        std::fs::write(dir.join("Vendor/Sub/Verb.clap"), b"").unwrap();
        std::fs::write(dir.join("readme.txt"), b"").unwrap();
        let found = find_all(&[dir.clone(), dir.clone()]);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found
            .iter()
            .any(|(f, p)| *f == PluginFormat::Vst3 && p.ends_with("Synth.vst3")));
        assert!(found
            .iter()
            .any(|(f, p)| *f == PluginFormat::Clap && p.ends_with("Verb.clap")));
        assert!(!standard_folders(PluginFormat::Vst3).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
