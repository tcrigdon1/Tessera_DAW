//! Third-party plug-ins in the app: scanning, one sandboxed instance per
//! plug-in slot in the project, saving their state, and exports.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tessera_engine::automation::{available_targets, target_name, AutomationTarget};
use tessera_engine::fx::DeviceParams;
use tessera_engine::model::{Id, Instrument, Project};
use tessera_engine::plugin::{self, base64, PluginFormat, PluginInstrument, PluginParams, PluginRef, PluginSlot};
use tessera_plugin_host::{scan, BridgeCommand, Event, PluginInfo, PluginInstance, ScanCache, Status};

use crate::app::{DetailMode, TesseraApp};
use crate::browser::app_dir;

/// How long to wait for plug-ins to load before an export or save.
const LOAD_WAIT: Duration = Duration::from_secs(15);
const STATE_WAIT: Duration = Duration::from_secs(3);

/// Progress of a background scan.
#[derive(Default)]
pub struct ScanProgress {
    pub done: usize,
    pub total: usize,
    pub current: String,
}

struct ScanJob {
    progress: Arc<Mutex<ScanProgress>>,
    thread: JoinHandle<ScanCache>,
}

/// Saved between runs.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Settings {
    folders: Vec<PathBuf>,
}

pub struct Instance {
    pub plugin: PluginRef,
    pub inst: PluginInstance,
}

pub struct PluginManager {
    bridge: Option<BridgeCommand>,
    pub sample_rate: f32,
    pub cache: ScanCache,
    /// Extra folders to scan besides the standard ones.
    pub folders: Vec<PathBuf>,
    scan: Option<ScanJob>,
    pub instances: HashMap<Id, Instance>,
    /// Parameter values to put back after a reload.
    restore: HashMap<Id, Vec<(u32, f64)>>,
}

fn cache_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("plugins.json"))
}

fn settings_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("plugin-folders.json"))
}

impl PluginManager {
    pub fn new(sample_rate: f32) -> PluginManager {
        let folders = settings_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .map(|s| s.folders)
            .unwrap_or_default();
        PluginManager {
            bridge: BridgeCommand::current_exe().ok(),
            sample_rate,
            cache: cache_path().map(|p| ScanCache::load(&p)).unwrap_or_default(),
            folders,
            scan: None,
            instances: HashMap::new(),
            restore: HashMap::new(),
        }
    }

    /// Every folder that gets scanned.
    pub fn all_folders(&self) -> Vec<PathBuf> {
        let mut v = scan::standard_folders(PluginFormat::Vst3);
        v.extend(scan::standard_folders(PluginFormat::Clap));
        for f in &self.folders {
            if !v.contains(f) {
                v.push(f.clone());
            }
        }
        v
    }

    pub fn add_folder(&mut self, dir: PathBuf) {
        if !self.folders.contains(&dir) {
            self.folders.push(dir);
            self.save_settings();
        }
        self.start_scan();
    }

    pub fn remove_folder(&mut self, dir: &Path) {
        self.folders.retain(|f| f != dir);
        self.save_settings();
    }

    fn save_settings(&self) {
        if let Some(p) = settings_path() {
            let s = Settings {
                folders: self.folders.clone(),
            };
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(&s).unwrap_or_default());
        }
    }

    pub fn scanning(&self) -> Option<(usize, usize, String)> {
        self.scan.as_ref().map(|j| {
            let p = j.progress.lock().unwrap_or_else(|e| e.into_inner());
            (p.done, p.total, p.current.clone())
        })
    }

    /// Scans in the background; unchanged files keep their cached results.
    pub fn start_scan(&mut self) {
        if self.scan.is_some() {
            return;
        }
        let Some(bridge) = self.bridge.clone() else { return };
        let folders = self.all_folders();
        let cache = self.cache.clone();
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        let p = progress.clone();
        let thread = std::thread::spawn(move || {
            scan::scan(&bridge, &folders, &cache, &|done, total, path: &Path| {
                let mut g = p.lock().unwrap_or_else(|e| e.into_inner());
                g.done = done;
                g.total = total;
                g.current = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
            })
        });
        self.scan = Some(ScanJob { progress, thread });
    }

    /// Picks up a finished scan. Returns a status line when one finished.
    pub fn poll_scan(&mut self) -> Option<String> {
        if !self.scan.as_ref()?.thread.is_finished() {
            return None;
        }
        let job = self.scan.take()?;
        let cache = job.thread.join().ok()?;
        if let Some(p) = cache_path() {
            let _ = cache.save(&p);
        }
        self.cache = cache;
        let n = self.cache.plugins().len();
        let failed = self.cache.failures().count();
        Some(if failed > 0 {
            format!("Found {n} plug-ins ({failed} files could not be loaded)")
        } else {
            format!("Found {n} plug-ins")
        })
    }

    pub fn plugins(&self) -> Vec<PluginInfo> {
        self.cache.plugins()
    }

    fn spawn(&self, slot: &PluginSlot, sample_rate: f32) -> Option<PluginInstance> {
        let bridge = self.bridge.as_ref()?;
        let state = slot.state.as_deref().and_then(base64::decode);
        PluginInstance::spawn(bridge, &slot.plugin, sample_rate, state.as_deref()).ok()
    }

    /// Starts instances for new plug-in slots, stops the ones no longer in
    /// the project, and registers their processors with the engine. Call
    /// before the engine compiles a snapshot.
    pub fn sync(&mut self, project: &mut Project) {
        project.prune_plugins();
        let used = project.plugin_slots_in_use();
        let gone: Vec<Id> = self
            .instances
            .keys()
            .copied()
            .filter(|id| {
                !used.contains(id) || project.plugin(*id).map(|s| &s.plugin) != Some(&self.instances[id].plugin)
            })
            .collect();
        for id in gone {
            plugin::unregister(id);
            self.instances.remove(&id);
        }
        for slot in &project.plugins {
            if self.instances.contains_key(&slot.slot) {
                continue;
            }
            if let Some(inst) = self.spawn(slot, self.sample_rate) {
                // Queued behind loading, so they apply once it has loaded.
                for (index, value) in self.restore.remove(&slot.slot).unwrap_or_default() {
                    inst.set_param(index, value);
                }
                plugin::register(slot.slot, inst.processor());
                self.instances.insert(
                    slot.slot,
                    Instance {
                        plugin: slot.plugin.clone(),
                        inst,
                    },
                );
            }
        }
    }

    /// Stops every instance (a different project was opened).
    pub fn clear(&mut self) {
        for id in self.instances.keys() {
            plugin::unregister(*id);
        }
        self.instances.clear();
    }

    /// Copies each running plug-in's state into the project for saving.
    pub fn store_states(&self, project: &mut Project) {
        for (id, i) in &self.instances {
            if i.inst.status() != Status::Ready {
                continue;
            }
            if let Some(data) = i.inst.save_state(STATE_WAIT) {
                if let Some(slot) = project.plugin_mut(*id) {
                    slot.state = Some(base64::encode(&data));
                }
            }
        }
    }

    /// Restarts a crashed or failed plug-in from its last saved state.
    pub fn reload(&mut self, slot: Id) {
        plugin::unregister(slot);
        if let Some(old) = self.instances.remove(&slot) {
            let values: Vec<(u32, f64)> = old
                .inst
                .params()
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.read_only)
                .map(|(i, p)| (i as u32, p.value))
                .collect();
            self.restore.insert(slot, values);
        }
    }

    /// Collects what the plug-ins reported. Returns (state changed, messages).
    pub fn poll(&mut self) -> (bool, Vec<String>) {
        let mut dirty = false;
        let mut messages = Vec::new();
        for i in self.instances.values() {
            for e in i.inst.take_events() {
                match e {
                    Event::Dirty | Event::ParamChanged { .. } => dirty = true,
                    Event::Crashed => messages.push(format!(
                        "{} crashed and is bypassed; use Reload in its device to restart it",
                        i.plugin.name
                    )),
                    Event::Failed(err) => messages.push(format!("{} could not be loaded: {err}", i.plugin.name)),
                    Event::EditorClosed(Some(err)) => messages.push(format!("{}: {err}", i.plugin.name)),
                    _ => {}
                }
            }
        }
        (dirty, messages)
    }

    /// Fresh instances of every plug-in in `project` at `sample_rate`, for an
    /// offline render. Waits until they have loaded.
    pub fn export_instances(&self, project: &Project, sample_rate: f32) -> Vec<(Id, PluginInstance)> {
        let mut out: Vec<(Id, PluginInstance)> = Vec::new();
        for slot in &project.plugins {
            // Prefer the live state, which may be newer than the saved one.
            let mut slot = slot.clone();
            if let Some(i) = self.instances.get(&slot.slot) {
                if i.inst.status() == Status::Ready {
                    if let Some(d) = i.inst.save_state(STATE_WAIT) {
                        slot.state = Some(base64::encode(&d));
                    }
                }
            }
            if let Some(inst) = self.spawn(&slot, sample_rate) {
                out.push((slot.slot, inst));
            }
        }
        for (_, inst) in &out {
            inst.wait_ready(LOAD_WAIT);
        }
        out
    }
}

/// Renders the Arrangement with its plug-ins running offline in their own
/// instances (the live ones keep playing untouched).
pub fn render_with_plugins(
    project: &Project,
    instances: Vec<(Id, PluginInstance)>,
    sample_rate: u32,
    beats: f64,
) -> Vec<f32> {
    let map: HashMap<Id, Arc<dyn plugin::PluginProcessor>> = instances
        .iter()
        .map(|(id, i)| (*id, i.processor() as Arc<dyn plugin::PluginProcessor>))
        .collect();
    let out = plugin::with_processors(map, || {
        tessera_engine::render::render_arrangement(project, sample_rate, beats)
    });
    drop(instances);
    out
}

/// Instances for a headless render (`tessera render`), without the app.
pub fn headless_instances(project: &Project, sample_rate: f32) -> Vec<(Id, PluginInstance)> {
    let Ok(bridge) = BridgeCommand::current_exe() else {
        return Vec::new();
    };
    let out: Vec<(Id, PluginInstance)> = project
        .plugins
        .iter()
        .filter_map(|slot| {
            let state = slot.state.as_deref().and_then(base64::decode);
            PluginInstance::spawn(&bridge, &slot.plugin, sample_rate, state.as_deref())
                .ok()
                .map(|i| (slot.slot, i))
        })
        .collect();
    for (_, inst) in &out {
        if !inst.wait_ready(LOAD_WAIT) {
            eprintln!("tessera: {} did not load: {:?}", inst.name(), inst.status());
        }
    }
    out
}

impl TesseraApp {
    /// Puts a plug-in on a track: an instrument replaces the track's
    /// instrument, an effect goes at the end of its chain.
    pub fn add_plugin(&mut self, bus: Id, info: &PluginInfo) {
        let plugin = info.to_ref();
        if plugin.instrument {
            let track = match self.project.track(bus) {
                Some(t) if t.kind == tessera_engine::model::TrackKind::Midi => bus,
                _ => {
                    let id = self.add_track(
                        tessera_engine::model::TrackKind::Midi,
                        Instrument::Plugin(PluginInstrument::default()),
                    );
                    let n = self.project.tracks.len();
                    if let Some(t) = self.project.track_mut(id) {
                        t.name = format!("{n} {}", info.name);
                    }
                    id
                }
            };
            self.set_instrument(track, Instrument::Plugin(PluginInstrument::default()));
            self.set_plugin_slot(track, plugin);
            self.selected_track = Some(track);
        } else {
            let id = self.project.alloc_id();
            let Some(chain) = self.project.chain_mut(bus) else {
                self.status = "Drop effects onto a track".into();
                return;
            };
            chain.push(tessera_engine::fx::Device {
                id,
                on: true,
                params: DeviceParams::Plugin(PluginParams::default()),
            });
            self.set_plugin_slot(id, plugin);
            self.selected_track = Some(bus);
        }
        self.detail = DetailMode::Device;
        self.status = format!("Loaded {} ({})", info.name, info.format.label());
        self.changed();
    }

    fn set_plugin_slot(&mut self, slot: Id, plugin: PluginRef) {
        self.project.plugins.retain(|p| p.slot != slot);
        self.project.plugins.push(PluginSlot {
            slot,
            plugin,
            state: None,
        });
    }

    /// Main-loop housekeeping for plug-ins.
    pub fn poll_plugins(&mut self) {
        if let Some(msg) = self.plugins.poll_scan() {
            self.status = msg;
        }
        let (dirty, messages) = self.plugins.poll();
        if dirty {
            self.touch();
        }
        if let Some(m) = messages.into_iter().last() {
            self.status = m;
        }
    }

    pub fn reload_plugin(&mut self, slot: Id) {
        self.plugins.reload(slot);
        self.needs_sync = true;
    }

    /// Automation targets for a track: its own parameters plus the
    /// automatable parameters of its plug-ins.
    pub fn automation_targets(&self, track: Id) -> Vec<AutomationTarget> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let mut v = available_targets(&t.instrument);
        let mut slots: Vec<Id> = Vec::new();
        if matches!(t.instrument, Instrument::Plugin(_)) {
            slots.push(track);
        }
        slots.extend(
            t.devices
                .iter()
                .filter(|d| matches!(d.params, DeviceParams::Plugin(_)))
                .map(|d| d.id),
        );
        for slot in slots {
            if let Some(i) = self.plugins.instances.get(&slot) {
                i.inst.with_params(|params| {
                    for (n, p) in params.iter().enumerate() {
                        if p.automatable && !p.hidden && !p.read_only {
                            v.push(AutomationTarget::Device {
                                device: slot,
                                param: n as u16,
                            });
                        }
                    }
                });
            }
        }
        v
    }

    /// Display name of an automation target on a track.
    pub fn automation_target_name(&self, track: Id, target: AutomationTarget) -> String {
        match target {
            AutomationTarget::Device { device, param } => {
                let plugin = self.project.plugin(device).map(|p| p.plugin.name.clone());
                let name = self
                    .plugins
                    .instances
                    .get(&device)
                    .and_then(|i| i.inst.with_params(|ps| ps.get(param as usize).map(|p| p.name.clone())));
                match (plugin, name) {
                    (Some(pl), Some(n)) => format!("{pl}: {n}"),
                    (Some(pl), None) => format!("{pl}: Parameter {}", param + 1),
                    _ => format!("Device Parameter {}", param + 1),
                }
            }
            t => {
                let instrument = self
                    .project
                    .track(track)
                    .map(|t| t.instrument)
                    .unwrap_or(Instrument::None);
                target_name(t, &instrument)
            }
        }
    }
}
