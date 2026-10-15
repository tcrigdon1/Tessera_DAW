//! Device view editing beyond parameters: the selected device, Cut, Copy,
//! Paste, Duplicate, Rename, Show/Hide (fold), deleting instruments and
//! saving or loading device presets.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tessera_engine::device_preset::{self, DevicePreset};
use tessera_engine::fx::{Device, DeviceParams};
use tessera_engine::model::{Id, Instrument};

use crate::app::{DetailMode, TesseraApp};
use crate::prefs::Preferences;

/// Device view state that is not saved with the project (except names, which
/// live on the devices).
#[derive(Default)]
pub struct DeviceUi {
    /// The device last clicked: `(bus, device)`. An instrument is selected
    /// as `(track, track)`.
    pub selected: Option<(Id, Id)>,
    pub clipboard: Option<DevicePreset>,
    /// Devices (and instruments, by track id) showing only their title bar.
    pub folded: HashSet<Id>,
    /// Rename window: bus, device (or the track for its instrument) and the
    /// name being typed.
    pub rename: Option<(Id, Id, String)>,
    /// Save Preset window: bus, device and the name being typed.
    pub save_preset: Option<(Id, Id, String)>,
    /// Saved presets, read when the Browser's Templates category is shown.
    preset_cache: Option<Vec<(PathBuf, DevicePreset)>>,
}

impl DeviceUi {
    /// Saved device presets, sorted by name.
    pub fn presets(&mut self, dir: Option<&Path>) -> &[(PathBuf, DevicePreset)] {
        self.preset_cache
            .get_or_insert_with(|| dir.map(list_presets).unwrap_or_default())
    }

    pub fn refresh_presets(&mut self) {
        self.preset_cache = None;
    }
}

/// Folder of saved device presets.
pub fn presets_dir(prefs: &Preferences) -> Option<PathBuf> {
    prefs.library().map(|l| l.join("Presets"))
}

fn list_presets(dir: &Path) -> Vec<(PathBuf, DevicePreset)> {
    let mut v: Vec<(PathBuf, DevicePreset)> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| device_preset::is_device_file(p))
                .filter_map(|p| DevicePreset::load(&p).ok().map(|c| (p, c)))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|(_, c)| c.name.to_lowercase());
    v
}

impl TesseraApp {
    /// Name shown in a device's title bar: its own name, the plug-in's or
    /// the device kind's.
    pub fn device_title(&self, d: &Device) -> String {
        if let Some(n) = d.name.as_deref().filter(|n| !n.is_empty()) {
            return n.to_string();
        }
        match d.params {
            DeviceParams::Plugin(_) => self
                .project
                .plugin(d.id)
                .map(|s| s.plugin.name.clone())
                .unwrap_or_else(|| "Plug-in".into()),
            _ => d.kind().name().to_string(),
        }
    }

    /// Name shown in an instrument's title bar.
    pub fn instrument_title(&self, track: Id) -> String {
        let Some(t) = self.project.track(track) else {
            return String::new();
        };
        if let Some(n) = t.instrument_name.as_deref().filter(|n| !n.is_empty()) {
            return n.to_string();
        }
        match t.instrument {
            Instrument::Plugin(_) => self
                .project
                .plugin(track)
                .map(|s| s.plugin.name.clone())
                .unwrap_or_else(|| "Plug-in".into()),
            Instrument::Rack(_) => "Instrument Rack".into(),
            i => i.name().to_string(),
        }
    }

    /// Hides the editor windows of every plug-in in a device (and the racks
    /// inside it) before it goes away.
    fn close_editors(&self, ids: &[Id]) {
        for id in ids {
            if let Some(i) = self.plugins.instances.get(id) {
                i.inst.hide_editor();
            }
        }
    }

    fn plugin_ids(d: &Device) -> Vec<Id> {
        let mut out = Vec::new();
        d.visit(&mut |d| {
            if matches!(d.params, DeviceParams::Plugin(_)) {
                out.push(d.id);
            }
        });
        out
    }

    /// Delete: removes a device from its chain. Plug-ins in it close their
    /// windows and their bridge processes stop at the next sync.
    pub fn delete_device(&mut self, bus: Id, device: Id) {
        if let Some(d) = self.find_device(bus, device) {
            let ids = Self::plugin_ids(d);
            self.close_editors(&ids);
            let name = self.device_title(d);
            self.label_edit(&format!("Delete {name}"));
            self.remove_device(bus, device);
            self.status = format!("Deleted {name}");
        }
        if self.devices.selected == Some((bus, device)) {
            self.devices.selected = None;
        }
    }

    /// Delete on an instrument: the track keeps its effects and has no
    /// instrument.
    pub fn delete_instrument(&mut self, track: Id) {
        let Some(inst) = self.project.track(track).map(|t| t.instrument) else {
            return;
        };
        if matches!(inst, Instrument::None) {
            return;
        }
        if matches!(inst, Instrument::Plugin(_)) {
            self.close_editors(&[track]);
        }
        let name = self.instrument_title(track);
        self.label_edit(&format!("Delete {name}"));
        self.set_instrument(track, Instrument::None);
        if let Some(t) = self.project.track_mut(track) {
            t.instrument_name = None;
        }
        self.project.prune_plugins();
        if self.devices.selected == Some((track, track)) {
            self.devices.selected = None;
        }
        self.status = format!("Deleted {name}");
        self.changed();
    }

    /// The device with its plug-in states, as it would be copied.
    fn capture_device(&mut self, bus: Id, device: Id) -> Option<DevicePreset> {
        self.plugins.store_states(&mut self.project);
        let d = self.find_device(bus, device)?.clone();
        let name = self.device_title(&d);
        Some(DevicePreset::capture(&self.project, &d, &name))
    }

    pub fn copy_device(&mut self, bus: Id, device: Id) -> bool {
        match self.capture_device(bus, device) {
            Some(p) => {
                self.status = format!("Copied {}", p.name);
                self.devices.clipboard = Some(p);
                true
            }
            None => false,
        }
    }

    pub fn cut_device(&mut self, bus: Id, device: Id) {
        if self.copy_device(bus, device) {
            self.delete_device(bus, device);
            if let Some(p) = &self.devices.clipboard {
                self.status = format!("Cut {}", p.name);
            }
        }
    }

    /// Puts a copy of a preset into a chain: after `after` (in whatever
    /// chain or rack holds it), else at the end of the bus's chain.
    pub fn insert_device_preset(&mut self, bus: Id, after: Option<Id>, preset: &DevicePreset) -> Option<Id> {
        let (device, slots) = preset.instantiate(&mut || self.project.alloc_id());
        let id = device.id;
        let list = match after {
            Some(a) => self.container_mut(bus, a),
            None => None,
        };
        match list {
            Some(list) => {
                let i = after
                    .and_then(|a| list.iter().position(|d| d.id == a))
                    .map(|i| i + 1)
                    .unwrap_or(list.len());
                list.insert(i, device);
            }
            None => self.project.chain_mut(bus)?.push(device),
        }
        for s in slots {
            self.project.plugins.retain(|p| p.slot != s.slot);
            self.project.plugins.push(s);
        }
        self.devices.selected = Some((bus, id));
        self.selected_track = Some(bus);
        self.detail = DetailMode::Device;
        self.changed();
        Some(id)
    }

    /// Paste: the copied device goes after `after`, or at the end of the
    /// chain.
    pub fn paste_device(&mut self, bus: Id, after: Option<Id>) {
        let Some(p) = self.devices.clipboard.clone() else {
            self.status = "Nothing to paste: copy a device first".into();
            return;
        };
        self.label_edit(&format!("Paste {}", p.name));
        if self.insert_device_preset(bus, after, &p).is_some() {
            self.status = format!("Pasted {}", p.name);
        } else {
            self.status = "Select a track to paste the device onto".into();
        }
    }

    pub fn duplicate_device(&mut self, bus: Id, device: Id) {
        if let Some(p) = self.capture_device(bus, device) {
            self.label_edit(&format!("Duplicate {}", p.name));
            self.insert_device_preset(bus, Some(device), &p);
            self.status = format!("Duplicated {}", p.name);
        }
    }

    /// Renames a device, or the track's instrument when `target` is the
    /// track. An empty name goes back to the default.
    pub fn rename_device(&mut self, bus: Id, target: Id, name: &str) {
        let name = name.trim();
        let name = (!name.is_empty()).then(|| name.to_string());
        if target == bus {
            if let Some(t) = self.project.track_mut(bus) {
                t.instrument_name = name;
                self.dirty = true;
            }
        } else if let Some(d) = self.find_device_mut(bus, target) {
            d.name = name;
            self.dirty = true;
        }
    }

    /// Rename… in a device's menu: opens the name window.
    pub fn ask_rename_device(&mut self, bus: Id, target: Id) {
        let current = if target == bus {
            self.instrument_title(bus)
        } else {
            match self.find_device(bus, target) {
                Some(d) => self.device_title(d),
                None => return,
            }
        };
        self.devices.rename = Some((bus, target, current));
    }

    pub fn toggle_device_fold(&mut self, id: Id) {
        if !self.devices.folded.remove(&id) {
            self.devices.folded.insert(id);
        }
    }

    /// Save Preset… in a device's menu: opens the name window.
    pub fn ask_save_device_preset(&mut self, bus: Id, device: Id) {
        if let Some(d) = self.find_device(bus, device) {
            let name = self.device_title(d);
            self.devices.save_preset = Some((bus, device, name));
        }
    }

    /// Saves a device into the library's Presets folder.
    pub fn save_device_preset(&mut self, bus: Id, device: Id, name: &str) -> Option<PathBuf> {
        let name = name.trim();
        let mut preset = self.capture_device(bus, device)?;
        if !name.is_empty() {
            preset.name = name.to_string();
        }
        let Some(dir) = presets_dir(&self.prefs) else {
            self.status = "No library folder to save presets in (see Preferences)".into();
            return None;
        };
        let path = crate::midi_files::unique(
            &dir,
            &crate::midi_files::sanitize(&preset.name),
            device_preset::EXTENSION,
        );
        match preset.save(&path) {
            Ok(()) => {
                self.devices.refresh_presets();
                self.status = format!(
                    "Saved preset \"{}\" (Browser > Templates > My Device Presets)",
                    preset.name
                );
                Some(path)
            }
            Err(e) => {
                self.status = format!("Could not save the preset: {e}");
                None
            }
        }
    }

    /// A device preset dropped from the Browser: added to the end of the
    /// bus's chain.
    pub fn load_device_preset(&mut self, bus: Id, preset: &DevicePreset) {
        self.label_edit(&format!("Load {}", preset.name));
        let mut p = preset.clone();
        if p.name != p.device.kind().name() {
            p.device.name = Some(p.name.clone());
        }
        match self.insert_device_preset(bus, None, &p) {
            Some(_) => self.status = format!("Loaded {}", p.name),
            None => self.status = "Drop presets onto a track".into(),
        }
    }

    pub fn delete_device_preset(&mut self, path: &Path) {
        self.status = match std::fs::remove_file(path) {
            Ok(()) => format!("Deleted {}", crate::browser::file_label(path)),
            Err(e) => format!("Could not delete {}: {e}", path.display()),
        };
        self.devices.refresh_presets();
    }

    /// Whether the selected device still exists and is on the shown track.
    pub fn selected_device(&self) -> Option<(Id, Id)> {
        let (bus, id) = self.devices.selected?;
        if self.selected_bus() != Some(bus) {
            return None;
        }
        let exists = if id == bus {
            self.project
                .track(bus)
                .is_some_and(|t| !matches!(t.instrument, Instrument::None))
        } else {
            self.find_device(bus, id).is_some()
        };
        exists.then_some((bus, id))
    }
}
