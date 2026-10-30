//! Project file handling beyond plain Save/Open: autosave and crash
//! recovery, crash logs, templates and the default set, recent projects, and
//! Collect All and Save.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tessera_engine::model::{Clip, ClipContent, Project};

use crate::app::TesseraApp;
use crate::browser::app_dir;

/// A session that hasn't touched its lock for this long is considered dead.
const STALE_SECS: u64 = 90;
/// How often the running session refreshes its lock.
const HEARTBEAT_SECS: f64 = 20.0;

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn autosave_dir() -> Option<PathBuf> {
    app_dir().map(|d| d.join("Autosave"))
}

pub fn crash_log_dir() -> Option<PathBuf> {
    app_dir().map(|d| d.join("Crash Logs"))
}

/// Written next to each autosave: whose it is and where it belongs.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionLock {
    pub pid: u32,
    /// Last heartbeat (seconds since 1970).
    pub alive_at: u64,
    /// Time of the last autosave, if any.
    pub saved_at: Option<u64>,
    /// The project's own file, if it had been saved.
    pub project_path: Option<PathBuf>,
    pub project_name: String,
}

/// An autosave left behind by a session that didn't quit normally.
#[derive(Debug, Clone)]
pub struct Recovery {
    pub lock_file: PathBuf,
    pub project_file: PathBuf,
    pub info: SessionLock,
}

/// Autosave state of the running app.
pub struct Autosave {
    dir: Option<PathBuf>,
    pid: u32,
    last_save: f64,
    last_beat: f64,
    /// Offered when the app starts.
    pub recovery: Option<Recovery>,
    info: SessionLock,
}

impl Autosave {
    /// Autosave switched off (tests).
    #[cfg(test)]
    pub fn disabled() -> Autosave {
        Autosave {
            dir: None,
            pid: std::process::id(),
            last_save: 0.0,
            last_beat: f64::NEG_INFINITY,
            recovery: None,
            info: SessionLock::default(),
        }
    }

    /// Looks for crashed sessions and claims a lock for this one.
    pub fn start() -> Autosave {
        let dir = autosave_dir();
        let pid = std::process::id();
        let recovery = dir.as_deref().and_then(find_recovery);
        let mut a = Autosave {
            dir,
            pid,
            last_save: 0.0,
            last_beat: f64::NEG_INFINITY,
            recovery,
            info: SessionLock {
                pid,
                ..Default::default()
            },
        };
        a.write_lock();
        a
    }

    fn lock_path(&self) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("session-{}.lock", self.pid)))
    }

    fn project_path(&self) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|d| d.join(format!("session-{}.tessera", self.pid)))
    }

    fn write_lock(&mut self) {
        let (Some(dir), Some(lock)) = (self.dir.clone(), self.lock_path()) else {
            return;
        };
        self.info.alive_at = now_secs();
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(json) = serde_json::to_string_pretty(&self.info) {
                let _ = write_atomic(&lock, json.as_bytes());
            }
        }
    }

    /// Called every frame: refreshes the lock and autosaves a changed
    /// project every `minutes` minutes (0 = never).
    pub fn tick(&mut self, now: f64, minutes: u32, project: &Project, path: Option<&Path>, dirty: bool) {
        if now - self.last_beat >= HEARTBEAT_SECS {
            self.last_beat = now;
            self.write_lock();
        }
        if minutes == 0 || !dirty || now - self.last_save < minutes as f64 * 60.0 {
            return;
        }
        self.last_save = now;
        self.save_now(project, path);
    }

    /// Writes the autosave immediately.
    pub fn save_now(&mut self, project: &Project, path: Option<&Path>) {
        let Some(file) = self.project_path() else { return };
        let mut copy = project.clone();
        absolutize_paths(&mut copy, path.and_then(Path::parent));
        let Ok(json) = copy.save_json() else { return };
        if self
            .dir
            .as_ref()
            .map(std::fs::create_dir_all)
            .is_some_and(|r| r.is_ok())
            && write_atomic(&file, json.as_bytes()).is_ok()
        {
            self.info.saved_at = Some(now_secs());
            self.info.project_path = path.map(Path::to_path_buf);
            self.info.project_name = project.name.clone();
            self.write_lock();
        }
    }

    /// After a normal Save: the autosave is no longer needed.
    pub fn saved(&mut self, project: &Project, path: &Path) {
        if let Some(f) = self.project_path() {
            let _ = std::fs::remove_file(f);
        }
        self.info.saved_at = None;
        self.info.project_path = Some(path.to_path_buf());
        self.info.project_name = project.name.clone();
        self.write_lock();
    }

    /// On a normal quit: remove this session's lock and autosave.
    pub fn shutdown(&mut self) {
        for p in [self.lock_path(), self.project_path()].into_iter().flatten() {
            let _ = std::fs::remove_file(p);
        }
    }

    /// Throws away a crashed session's files.
    pub fn discard_recovery(&mut self) {
        if let Some(r) = self.recovery.take() {
            let _ = std::fs::remove_file(&r.lock_file);
            let _ = std::fs::remove_file(&r.project_file);
        }
    }
}

/// Finds the newest autosave of a session that died (its lock went stale).
/// Stale sessions without an autosave are cleaned up.
pub fn find_recovery(dir: &Path) -> Option<Recovery> {
    let now = now_secs();
    let mut best: Option<Recovery> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let lock_file = e.path();
        if lock_file.extension().and_then(|x| x.to_str()) != Some("lock") {
            continue;
        }
        let Some(info) = std::fs::read_to_string(&lock_file)
            .ok()
            .and_then(|t| serde_json::from_str::<SessionLock>(&t).ok())
        else {
            continue;
        };
        if now.saturating_sub(info.alive_at) < STALE_SECS {
            continue; // Another Tessera is running.
        }
        let project_file = lock_file.with_extension("tessera");
        if info.saved_at.is_none() || !project_file.is_file() {
            let _ = std::fs::remove_file(&lock_file);
            let _ = std::fs::remove_file(&project_file);
            continue;
        }
        if best.as_ref().is_none_or(|b| b.info.saved_at < info.saved_at) {
            best = Some(Recovery {
                lock_file,
                project_file,
                info,
            });
        }
    }
    best
}

/// Writes next to `path` and renames, so a crash or a full disk mid-write
/// never leaves half a file in place of the old one.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

/// Saves a project file atomically (see [`write_atomic`]).
pub fn save_project(p: &Project, path: &Path) -> std::io::Result<()> {
    let json = p.save_json().map_err(std::io::Error::other)?;
    write_atomic(path, json.as_bytes())
}

/// Every audio file path in a project (clip audio and instrument samples).
fn audio_paths_mut(p: &mut Project) -> Vec<&mut String> {
    let mut out = Vec::new();
    for t in &mut p.tracks {
        for s in t.samples.iter_mut().flatten() {
            out.push(&mut s.path);
        }
        let clips = t
            .slots
            .iter_mut()
            .flatten()
            .chain(t.arrangement.iter_mut().map(|a| &mut a.clip));
        for c in clips {
            if let ClipContent::Audio(a) = &mut c.content {
                out.push(&mut a.path);
            }
        }
    }
    out
}

/// Makes relative audio paths absolute against `base`, so the project can
/// be written somewhere else (autosave, templates) and still find its audio.
pub fn absolutize_paths(p: &mut Project, base: Option<&Path>) {
    let Some(base) = base else { return };
    for path in audio_paths_mut(p) {
        if !path.is_empty() && Path::new(path.as_str()).is_relative() {
            *path = base.join(path.as_str()).to_string_lossy().to_string();
        }
    }
}

/// Copies every audio file the project uses from outside its folder into
/// `Samples/Imported` (or `Samples/Recorded` for takes) and points the
/// project at the copies. Returns how many files were copied, or the first
/// error.
pub fn collect_files(p: &mut Project, project_file: &Path) -> Result<usize, String> {
    let dir = project_file.parent().ok_or("the project has no folder")?.to_path_buf();
    let imported = dir.join("Samples").join("Imported");
    let mut copied = 0;
    let mut done: std::collections::HashMap<PathBuf, String> = Default::default();
    for path in audio_paths_mut(p) {
        if path.is_empty() {
            continue;
        }
        let src = PathBuf::from(path.as_str());
        if src.is_relative() || src.starts_with(&dir) {
            if let Ok(rel) = src.strip_prefix(&dir) {
                *path = rel.to_string_lossy().to_string();
            }
            continue;
        }
        if let Some(rel) = done.get(&src) {
            *path = rel.clone();
            continue;
        }
        let Some(name) = src.file_name() else { continue };
        std::fs::create_dir_all(&imported).map_err(|e| e.to_string())?;
        // Don't overwrite a different file with the same name.
        let mut dest = imported.join(name);
        let mut n = 2;
        while dest.exists() && !same_file(&src, &dest) {
            let stem = src.file_stem().unwrap_or_default().to_string_lossy();
            let ext = src
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            dest = imported.join(format!("{stem} {n}{ext}"));
            n += 1;
        }
        if !dest.exists() {
            std::fs::copy(&src, &dest).map_err(|e| format!("Could not copy {}: {e}", src.display()))?;
            copied += 1;
        }
        let rel = dest.strip_prefix(&dir).unwrap_or(&dest).to_string_lossy().to_string();
        done.insert(src, rel.clone());
        *path = rel;
    }
    Ok(copied)
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Templates in a folder, sorted by name.
pub fn list_templates(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("tessera"))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Saves a crash report (the panic message and a backtrace) for
/// Preferences → Licenses/Maintenance.
pub fn install_crash_logger() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = crash_log_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let bt = std::backtrace::Backtrace::force_capture();
            let thread = std::thread::current();
            let text = format!(
                "Tessera {} crashed on thread '{}' at {} (unix time)\n\n{info}\n\nBacktrace:\n{bt}\n",
                env!("CARGO_PKG_VERSION"),
                thread.name().unwrap_or("?"),
                now_secs(),
            );
            let _ = std::fs::write(dir.join(format!("crash-{}.txt", now_secs())), text);
        }
        previous(info);
    }));
}

/// Crash logs, newest first.
pub fn crash_logs() -> Vec<PathBuf> {
    let Some(dir) = crash_log_dir() else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v.reverse();
    v
}

/// Human-readable age of a unix timestamp.
pub fn ago(secs: u64) -> String {
    let d = now_secs().saturating_sub(secs);
    match d {
        0..=59 => "less than a minute ago".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => format!("{} days ago", d / 86_400),
    }
}

impl TesseraApp {
    /// Opens the crashed session's autosave.
    pub fn recover(&mut self) {
        let Some(r) = self.autosave.recovery.take() else { return };
        match Project::load_from(&r.project_file) {
            Ok(p) => {
                let path = r.info.project_path.clone();
                self.load_project(p, path);
                self.dirty = true;
                self.status = "Recovered the autosaved set. Save it to keep it.".into();
                // Keep the recovered copy until the user saves.
                let (proj, path) = (self.project.clone(), self.path.clone());
                self.autosave.save_now(&proj, path.as_deref());
            }
            Err(e) => self.status = format!("Could not recover: {e}"),
        }
        let _ = std::fs::remove_file(&r.lock_file);
        let _ = std::fs::remove_file(&r.project_file);
    }

    /// Saves the current set as a template in the library.
    pub fn save_as_template(&mut self, name: &str) {
        let Some(dir) = self.prefs.templates_dir() else {
            self.status = "No library folder for templates".into();
            return;
        };
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let file = dir.join(format!("{name}.tessera"));
        self.write_template(&file);
    }

    /// Makes the current set what File → New starts from.
    pub fn save_as_default_set(&mut self) {
        if let Some(file) = self.prefs.default_template() {
            self.write_template(&file);
        }
    }

    fn write_template(&mut self, file: &Path) {
        self.plugins.store_states(&mut self.project);
        let mut p = self.project.clone();
        absolutize_paths(&mut p, self.path.as_deref().and_then(Path::parent));
        // Templates start empty of recorded takes' temp files' ownership:
        // the paths are absolute and still work.
        let res = file
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| save_project(&p, file));
        self.status = match res {
            Ok(()) => format!("Saved template {}", file.display()),
            Err(e) => format!("Could not save the template: {e}"),
        };
    }

    /// File → New from a template.
    pub fn new_from_template(&mut self, file: &Path) {
        match Project::load_from(file) {
            Ok(mut p) => {
                p.name = "Untitled".into();
                self.load_project(p, None);
                self.status = format!("New set from {}", crate::browser::file_label(file));
            }
            Err(e) => self.status = format!("Could not open the template: {e}"),
        }
    }

    /// File → Collect All and Save.
    pub fn collect_all_and_save(&mut self) {
        let Some(path) = self.path.clone() else {
            self.save_as();
            if self.path.is_some() {
                self.collect_all_and_save();
            }
            return;
        };
        let mut p = self.project.clone();
        match collect_files(&mut p, &path) {
            Ok(n) => {
                // Only the paths changed; the audio already loaded stays.
                for (dst, src) in self.project.tracks.iter_mut().zip(p.tracks) {
                    copy_paths(dst, src);
                }
                self.save();
                self.status = format!("Collected {n} file(s) into the project folder and saved");
            }
            Err(e) => self.status = e,
        }
    }
}

/// Copies audio paths from `src` (same structure) into `dst`.
fn copy_paths(dst: &mut tessera_engine::model::Track, src: tessera_engine::model::Track) {
    for (d, s) in dst.samples.iter_mut().zip(src.samples) {
        if let (Some(d), Some(s)) = (d, s) {
            d.path = s.path;
        }
    }
    let fix = |d: &mut Clip, s: Clip| {
        if let (ClipContent::Audio(d), ClipContent::Audio(s)) = (&mut d.content, s.content) {
            d.path = s.path;
        }
    };
    for (d, s) in dst.slots.iter_mut().zip(src.slots) {
        if let (Some(d), Some(s)) = (d, s) {
            fix(d, s);
        }
    }
    for (d, s) in dst.arrangement.iter_mut().zip(src.arrangement) {
        fix(&mut d.clip, s.clip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tessera_engine::model::{AudioData, AudioRef, Instrument, TrackKind};

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tessera-pf-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn project_with_audio(path: &str) -> Project {
        let mut p = Project::default();
        let id = p.add_track(TrackKind::Audio, Instrument::None);
        let data = AudioData {
            sample_rate: 44100,
            left: vec![0.0; 10],
            right: vec![0.0; 10],
        };
        p.track_mut(id).unwrap().slots[0] = Some(Clip {
            id: p.alloc_id(),
            length: 4.0,
            content: ClipContent::Audio(AudioRef::new(path, Some(Arc::new(data)))),
            ..Default::default()
        });
        p
    }

    fn clip_path(p: &Project) -> String {
        match &p.tracks[0].slots[0].as_ref().unwrap().content {
            ClipContent::Audio(a) => a.path.clone(),
            _ => unreachable!(),
        }
    }

    #[test]
    fn absolutize_only_touches_relative_paths() {
        let mut p = project_with_audio("Samples/a.wav");
        absolutize_paths(&mut p, Some(Path::new("/music/song")));
        assert_eq!(
            PathBuf::from(clip_path(&p)),
            Path::new("/music/song").join("Samples/a.wav")
        );
        let abs = clip_path(&p);
        absolutize_paths(&mut p, Some(Path::new("/elsewhere")));
        assert_eq!(clip_path(&p), abs);
    }

    #[test]
    fn collect_copies_outside_files_once() {
        let root = temp("collect");
        let outside = root.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let src = outside.join("kick.wav");
        std::fs::write(&src, b"RIFF....").unwrap();
        let song = root.join("song");
        std::fs::create_dir_all(&song).unwrap();
        let mut p = project_with_audio(&src.to_string_lossy());
        // A second clip using the same file.
        let c = p.tracks[0].slots[0].clone();
        p.tracks[0].slots[1] = c;
        let n = collect_files(&mut p, &song.join("Song.tessera")).unwrap();
        assert_eq!(n, 1);
        let rel = clip_path(&p);
        assert_eq!(
            PathBuf::from(&rel),
            Path::new("Samples").join("Imported").join("kick.wav")
        );
        assert!(song.join(&rel).is_file());
        // Collecting again copies nothing.
        assert_eq!(collect_files(&mut p, &song.join("Song.tessera")).unwrap(), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn collect_renames_on_name_clash() {
        let root = temp("clash");
        let song = root.join("song");
        let imported = song.join("Samples").join("Imported");
        std::fs::create_dir_all(&imported).unwrap();
        std::fs::write(imported.join("kick.wav"), b"other").unwrap();
        let src = root.join("kick.wav");
        std::fs::write(&src, b"mine").unwrap();
        let mut p = project_with_audio(&src.to_string_lossy());
        collect_files(&mut p, &song.join("Song.tessera")).unwrap();
        assert!(clip_path(&p).ends_with("kick 2.wav"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_sessions_are_recovered_and_live_ones_are_not() {
        let dir = temp("recover");
        let write = |pid: u32, alive_at: u64, saved: bool| {
            let lock = SessionLock {
                pid,
                alive_at,
                saved_at: saved.then_some(alive_at),
                project_path: None,
                project_name: format!("p{pid}"),
            };
            std::fs::write(
                dir.join(format!("session-{pid}.lock")),
                serde_json::to_string(&lock).unwrap(),
            )
            .unwrap();
            if saved {
                std::fs::write(dir.join(format!("session-{pid}.tessera")), "{}").unwrap();
            }
        };
        let now = now_secs();
        write(1, now, true); // still running
        write(2, now - 1000, true); // crashed, older
        write(3, now - 500, true); // crashed, newer
        write(4, now - 500, false); // crashed without an autosave
        let r = find_recovery(&dir).unwrap();
        assert_eq!(r.info.pid, 3);
        assert!(!dir.join("session-4.lock").exists());
        assert!(dir.join("session-1.lock").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
