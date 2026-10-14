//! Chain templates in the app: saving a track's device chain to the library,
//! dropping one onto a track or empty space, and the Default MIDI and Audio
//! Track templates that new tracks start from.

use std::path::{Path, PathBuf};

use tessera_engine::chain_preset::{self, Applied, ChainPreset};
use tessera_engine::model::{Id, Instrument, TrackKind};

use crate::app::{DetailMode, TesseraApp};
use crate::prefs::Preferences;

/// Folder of saved chain templates.
pub fn templates_dir(prefs: &Preferences) -> Option<PathBuf> {
    prefs.library().map(|l| l.join("Chains"))
}

/// The template new tracks of `kind` start from, when it exists.
pub fn default_track_file(prefs: &Preferences, kind: TrackKind) -> Option<PathBuf> {
    let name = match kind {
        TrackKind::Midi => "Default MIDI Track",
        TrackKind::Audio => "Default Audio Track",
        TrackKind::Group => return None,
    };
    prefs
        .library()
        .map(|l| l.join("Defaults").join(format!("{name}.{}", chain_preset::EXTENSION)))
}

/// Saved templates, sorted by name, with the files that could be read.
pub fn list_templates(dir: &Path) -> Vec<(PathBuf, ChainPreset)> {
    let mut v: Vec<(PathBuf, ChainPreset)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| chain_preset::is_chain_file(p))
                .filter_map(|p| ChainPreset::load(&p).ok().map(|c| (p, c)))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|(_, c)| c.name.to_lowercase());
    v
}

impl TesseraApp {
    fn capture_chain(&mut self, track: Id, name: &str) -> Option<ChainPreset> {
        // Take the running plug-ins' current state along.
        self.plugins.store_states(&mut self.project);
        let base = self.path.as_deref().and_then(Path::parent);
        ChainPreset::capture(&self.project, track, name, base)
    }

    /// Track menu → Save Chain as Template…: asks for a name.
    pub fn ask_save_chain_template(&mut self, track: Id) {
        if let Some(t) = self.project.track(track) {
            let name = t.name.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ');
            let name = if name.is_empty() { t.name.as_str() } else { name };
            self.browser.save_chain = Some((track, name.to_string()));
        }
    }

    /// Saves a track's chain into the library's Chains folder.
    pub fn save_chain_template(&mut self, track: Id, name: &str) -> Option<PathBuf> {
        let name = name.trim();
        let name = if name.is_empty() { "Chain" } else { name };
        let dir = templates_dir(&self.prefs)?;
        let preset = self.capture_chain(track, name)?;
        let path = crate::midi_files::unique(&dir, &crate::midi_files::sanitize(name), chain_preset::EXTENSION);
        match preset.save(&path) {
            Ok(()) => {
                self.browser.refresh();
                self.status = format!("Saved chain template \"{name}\" ({})", preset.summary());
                Some(path)
            }
            Err(e) => {
                self.status = format!("Could not save the chain template: {e}");
                None
            }
        }
    }

    /// Track menu → Save as Default MIDI/Audio Track.
    pub fn save_default_track(&mut self, track: Id) {
        let Some(kind) = self.project.track(track).map(|t| t.kind) else {
            return;
        };
        let Some(file) = default_track_file(&self.prefs, kind) else {
            self.status = "Group tracks have no default template".into();
            return;
        };
        let label = if kind == TrackKind::Midi { "MIDI" } else { "Audio" };
        let Some(preset) = self.capture_chain(track, &format!("Default {label} Track")) else {
            return;
        };
        self.status = match preset.save(&file) {
            Ok(()) => format!("New {label} tracks now start with {}", preset.summary()),
            Err(e) => format!("Could not save the default track: {e}"),
        };
    }

    /// Forgets the Default MIDI or Audio Track template.
    pub fn clear_default_track(&mut self, kind: TrackKind) {
        if let Some(file) = default_track_file(&self.prefs, kind) {
            if std::fs::remove_file(&file).is_ok() {
                self.status = "New tracks start empty again".into();
            }
        }
    }

    pub fn has_default_track(&self, kind: TrackKind) -> bool {
        default_track_file(&self.prefs, kind).is_some_and(|f| f.is_file())
    }

    /// Gives a newly created track the Default Track template of its kind.
    pub(crate) fn apply_default_track(&mut self, track: Id) {
        let Some(kind) = self.project.track(track).map(|t| t.kind) else {
            return;
        };
        let Some(file) = default_track_file(&self.prefs, kind).filter(|f| f.is_file()) else {
            return;
        };
        match ChainPreset::load(&file) {
            Ok(preset) => {
                preset.apply(&mut self.project, track, true);
                self.changed();
            }
            Err(e) => self.status = format!("Could not read the default track ({e})"),
        }
    }

    /// A chain template dropped (or double-clicked) onto a track: replaces
    /// its chain. On empty space, or a track of the other kind, it makes a
    /// new track named after the template.
    pub fn drop_chain(&mut self, preset: &ChainPreset, track: Option<Id>) {
        let fits = track
            .and_then(|t| self.project.track(t))
            .is_some_and(|t| preset.fits(t.kind));
        let target = match track.filter(|_| fits) {
            Some(t) => {
                self.label_edit("Load Chain Template");
                if self.chain_edit.is_some_and(|(ct, _)| ct == t) {
                    self.chain_edit = None;
                }
                preset.apply(&mut self.project, t, false);
                t
            }
            None => {
                let kind = if preset.kind == TrackKind::Midi {
                    TrackKind::Midi
                } else {
                    TrackKind::Audio
                };
                let id = self.add_track(kind, Instrument::None);
                let n = self.project.tracks.len();
                if let Some(t) = self.project.track_mut(id) {
                    t.name = format!("{n} {}", preset.name);
                }
                if preset.apply(&mut self.project, id, true) != Applied::Done {
                    return;
                }
                id
            }
        };
        self.selected_track = Some(target);
        self.detail = DetailMode::Device;
        self.status = format!("Loaded {}", preset.name);
        self.changed();
    }

    /// Browser → delete a saved template.
    pub fn delete_chain_template(&mut self, path: &Path) {
        self.status = match std::fs::remove_file(path) {
            Ok(()) => format!("Deleted {}", crate::browser::file_label(path)),
            Err(e) => format!("Could not delete {}: {e}", path.display()),
        };
        self.browser.refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_templates_are_listed_by_name() {
        let dir = std::env::temp_dir().join(format!("tessera-chains-{}", std::process::id()));
        for name in ["b Keys", "A Bass"] {
            let mut c = chain_preset::factory_templates().remove(0);
            c.name = name.into();
            c.save(&dir.join(format!("{name}.{}", chain_preset::EXTENSION)))
                .unwrap();
        }
        std::fs::write(dir.join("broken.tchain"), "not json").unwrap();
        std::fs::write(dir.join("other.txt"), "x").unwrap();
        let names: Vec<String> = list_templates(&dir).into_iter().map(|(_, c)| c.name).collect();
        assert_eq!(names, ["A Bass", "b Keys"]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
