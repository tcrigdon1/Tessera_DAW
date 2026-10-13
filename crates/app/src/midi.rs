//! Hardware MIDI: input ports (midir), MIDI preferences, controller mappings
//! and MIDI Map Mode (Ctrl+M).
//!
//! Each enabled input port gets its own midir connection. Its callback sends
//! every message to the engine through a lock-free ring when the port's
//! *Track* switch is on (so notes reach armed tracks with no UI latency) and
//! to the UI through a channel when *Remote* is on (controller mappings).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tessera_engine::automation::{
    get_instrument_param, instrument_params, set_instrument_param, PAN_SPEC, SEND_SPEC, VOLUME_SPEC,
};
use tessera_engine::engine::midi_ring;
use tessera_engine::midi::mapping::{MapSource, MapTarget, MidiMapping};
use tessera_engine::midi::{port_id, MidiKind, MidiMessage};
use tessera_engine::model::{Id, MixerParams, MASTER_BUS};
use tessera_engine::Command;

use crate::app::{ClipRef, TesseraApp};

/// Tempo range a mapped controller sweeps.
const TEMPO_RANGE: (f64, f64) = (60.0, 200.0);

/// Per-port switches, saved in the settings folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortSetting {
    pub name: String,
    /// Notes from this port play armed tracks.
    pub track: bool,
    /// Controller messages from this port drive mappings.
    pub remote: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MidiSettings {
    #[serde(default)]
    pub inputs: Vec<PortSetting>,
}

impl MidiSettings {
    fn path() -> Option<PathBuf> {
        crate::browser::app_dir().map(|d| d.join("midi.json"))
    }

    fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        if let Some(p) = Self::path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(p, json);
            }
        }
    }

    /// The switches of a port, adding new ports switched on.
    fn port(&mut self, name: &str) -> &mut PortSetting {
        if let Some(i) = self.inputs.iter().position(|p| p.name == name) {
            return &mut self.inputs[i];
        }
        self.inputs.push(PortSetting {
            name: name.to_string(),
            track: true,
            remote: true,
        });
        self.inputs.last_mut().unwrap()
    }
}

/// A message on its way to the UI, with whether its port is a Remote port.
#[derive(Debug, Clone, Copy)]
pub struct UiMessage {
    pub msg: MidiMessage,
    pub remote: bool,
    /// From a Track port (the notes play instruments).
    pub track: bool,
}

/// MIDI Map Mode state.
#[derive(Debug, Default)]
pub struct MidiLearn {
    /// The control picked on screen, waiting for a controller message.
    pub target: Option<MapTarget>,
    /// Mixer settings when map mode started (to tell which control moved).
    pub mixers: HashMap<Id, MixerParams>,
}

/// Open MIDI ports and the message plumbing.
pub struct MidiIo {
    pub settings: MidiSettings,
    /// Input ports found by the last scan.
    pub ports: Vec<String>,
    pub error: Option<String>,
    connections: Vec<midir::MidiInputConnection<()>>,
    to_engine: Arc<Mutex<rtrb::Producer<MidiMessage>>>,
    ui_tx: Sender<UiMessage>,
    ui_rx: Receiver<UiMessage>,
    /// Last activity per port id, in UI seconds.
    pub activity: HashMap<u64, f64>,
    /// Last message seen, for the preferences window.
    pub last: Option<MidiMessage>,
}

impl MidiIo {
    /// A fresh ring for a new engine (after the audio device changes); the
    /// open ports write into it from now on.
    pub fn new_engine_ring(&mut self) -> rtrb::Consumer<MidiMessage> {
        let (tx, rx) = midi_ring();
        *self.to_engine.lock().unwrap_or_else(|e| e.into_inner()) = tx;
        rx
    }

    /// Creates the plumbing; returns the ring the engine reads.
    pub fn new() -> (Self, rtrb::Consumer<MidiMessage>) {
        let (tx, rx) = midi_ring();
        let (ui_tx, ui_rx) = channel();
        let mut io = MidiIo {
            settings: MidiSettings::load(),
            ports: Vec::new(),
            error: None,
            connections: Vec::new(),
            to_engine: Arc::new(Mutex::new(tx)),
            ui_tx,
            ui_rx,
            activity: HashMap::new(),
            last: None,
        };
        if std::env::var_os("TESSERA_NO_MIDI").is_none() {
            io.rescan();
        }
        (io, rx)
    }

    /// Lists input ports and (re)connects the enabled ones.
    pub fn rescan(&mut self) {
        self.connections.clear();
        self.error = None;
        let input = match midir::MidiInput::new("Tessera") {
            Ok(i) => i,
            Err(e) => {
                self.error = Some(format!("MIDI is not available: {e}"));
                self.ports.clear();
                return;
            }
        };
        self.ports = input
            .ports()
            .iter()
            .filter_map(|p| input.port_name(p).ok())
            .filter(|n| !n.starts_with("Tessera"))
            .collect();
        for name in self.ports.clone() {
            let s = self.settings.port(&name).clone();
            if !s.track && !s.remote {
                continue;
            }
            if let Err(e) = self.connect(&name, s.track, s.remote) {
                self.error = Some(format!("{name}: {e}"));
            }
        }
        self.settings.save();
    }

    fn connect(&mut self, name: &str, track: bool, remote: bool) -> Result<(), String> {
        let mut input = midir::MidiInput::new("Tessera").map_err(|e| e.to_string())?;
        input.ignore(midir::Ignore::All);
        let port = input
            .ports()
            .into_iter()
            .find(|p| input.port_name(p).is_ok_and(|n| n == name))
            .ok_or("port disappeared")?;
        let id = port_id(name);
        let engine = self.to_engine.clone();
        let ui = self.ui_tx.clone();
        let conn = input
            .connect(
                &port,
                "Tessera In",
                move |_, bytes, _| {
                    let Some(msg) = MidiMessage::from_bytes(id, bytes) else {
                        return;
                    };
                    if track {
                        if let Ok(mut p) = engine.lock() {
                            let _ = p.push(msg);
                        }
                    }
                    let _ = ui.send(UiMessage { msg, remote, track });
                },
                (),
            )
            .map_err(|e| e.to_string())?;
        self.connections.push(conn);
        Ok(())
    }

    /// Changes a port's switches and reconnects.
    pub fn set_port(&mut self, name: &str, track: bool, remote: bool) {
        let p = self.settings.port(name);
        p.track = track;
        p.remote = remote;
        self.rescan();
    }

    pub fn port_setting(&self, name: &str) -> Option<&PortSetting> {
        self.settings.inputs.iter().find(|p| p.name == name)
    }

    /// Messages received since the last call.
    pub fn take(&mut self, now: f64) -> Vec<UiMessage> {
        let msgs: Vec<UiMessage> = self.ui_rx.try_iter().collect();
        for m in &msgs {
            self.activity.insert(m.msg.port, now);
            self.last = Some(m.msg);
        }
        msgs
    }

    /// Sends a message to the engine as if it came from a port (tests and
    /// the on-screen keyboard could use this).
    #[allow(dead_code)]
    pub fn inject(&self, msg: MidiMessage) {
        if let Ok(mut p) = self.to_engine.lock() {
            let _ = p.push(msg);
        }
    }
}

/// What a controller message means for mappings.
fn source_of(msg: &MidiMessage) -> Option<(MapSource, u8)> {
    let channel = msg.channel();
    match msg.kind() {
        MidiKind::Cc { cc, value } => Some((MapSource::Cc { channel, cc }, value)),
        MidiKind::NoteOn { pitch, velocity } => Some((MapSource::Note { channel, note: pitch }, velocity)),
        _ => None,
    }
}

impl TesseraApp {
    /// Reads MIDI from the ports: learns or applies mappings. Once per frame.
    pub fn poll_midi(&mut self, now: f64) {
        // Hardware MIDI follows the selected track when nothing is armed.
        let selected = self.selected_track.unwrap_or(0);
        if self.midi_selected_sent != Some(selected) {
            self.midi_selected_sent = Some(selected);
            self.send(Command::SetSelectedTrack(selected));
        }
        let capture = self.capture_target();
        for m in self.midi.take(now) {
            // Notes played into tracks are remembered for Capture MIDI.
            if let (true, Some(t)) = (m.track, capture) {
                match m.msg.kind() {
                    MidiKind::NoteOn { pitch, velocity } => self.capture_note(t, pitch, velocity, true, now),
                    MidiKind::NoteOff { pitch } => self.capture_note(t, pitch, 0, false, now),
                    _ => {}
                }
            }
            if !m.remote {
                continue;
            }
            let Some((source, value)) = source_of(&m.msg) else {
                continue;
            };
            if self.midi_learn.is_some() {
                self.learn_source(source);
                continue;
            }
            let maps: Vec<MidiMapping> = self
                .project
                .midi_mappings
                .iter()
                .filter(|x| x.source == source)
                .copied()
                .collect();
            for map in maps {
                self.apply_mapping(&map, value);
            }
        }
    }

    // ----- map mode ---------------------------------------------------------------

    pub fn toggle_midi_learn(&mut self) {
        if self.midi_learn.take().is_some() {
            self.status = "MIDI Map Mode off".into();
            return;
        }
        let mut mixers: HashMap<Id, MixerParams> = self.project.tracks.iter().map(|t| (t.id, t.mixer)).collect();
        mixers.extend(self.project.returns.iter().map(|r| (r.id, r.mixer)));
        mixers.insert(MASTER_BUS, self.project.master);
        self.midi_learn = Some(MidiLearn { target: None, mixers });
        self.status = "MIDI Map Mode: move a control on screen, then a knob, fader or pad on your controller".into();
    }

    /// In map mode, remembers the control the user just moved.
    pub fn learn_touch(&mut self, target: MapTarget) {
        if self.midi_learn.is_none() {
            return;
        }
        let name = self.target_name(target);
        if let Some(l) = self.midi_learn.as_mut() {
            l.target = Some(target);
        }
        self.status = format!("Mapping {name}: now move a control on your MIDI controller");
    }

    /// In map mode, works out which mixer control of `bus` moved.
    pub fn learn_mixer(&mut self, bus: Id, now: MixerParams) {
        let Some(l) = self.midi_learn.as_mut() else { return };
        let before = l.mixers.insert(bus, now).unwrap_or(now);
        let target = if (now.volume_db - before.volume_db).abs() > 1e-6 {
            Some(MapTarget::Volume(bus))
        } else if (now.pan - before.pan).abs() > 1e-6 {
            Some(MapTarget::Pan(bus))
        } else if now.mute != before.mute {
            Some(MapTarget::Mute(bus))
        } else if now.solo != before.solo {
            Some(MapTarget::Solo(bus))
        } else if now.arm != before.arm {
            Some(MapTarget::Arm(bus))
        } else {
            None
        };
        if let Some(t) = target {
            self.learn_touch(t);
        }
    }

    fn learn_source(&mut self, source: MapSource) {
        let Some(target) = self.midi_learn.as_ref().and_then(|l| l.target) else {
            self.status = format!("{}: first move the control on screen you want to map", source.label());
            return;
        };
        // One mapping per control and per controller message.
        self.project
            .midi_mappings
            .retain(|m| m.target != target && m.source != source);
        self.project.midi_mappings.push(MidiMapping::new(source, target));
        self.touch();
        if let Some(l) = self.midi_learn.as_mut() {
            l.target = None;
        }
        self.status = format!("Mapped {} to {}", source.label(), self.target_name(target));
    }

    /// Whether a launch in the Session view is being learned instead.
    pub fn launch_clip(&mut self, track: Id, slot: usize) {
        if self.midi_learn.is_some() {
            self.learn_touch(MapTarget::LaunchClip { track, scene: slot });
            return;
        }
        self.held_launch = Some((track, slot));
        self.send(Command::LaunchClip { track, slot });
    }

    /// The launch button was let go (Gate and Repeat clips stop).
    pub fn release_launch(&mut self) {
        if let Some((track, slot)) = self.held_launch.take() {
            self.send(Command::ReleaseClip { track, slot });
        }
    }

    pub fn launch_scene(&mut self, scene: usize) {
        if self.midi_learn.is_some() {
            self.learn_touch(MapTarget::LaunchScene(scene));
            return;
        }
        self.send(Command::LaunchScene(scene));
    }

    /// Display name of a mapping target.
    pub fn target_name(&self, t: MapTarget) -> String {
        let bus = |id: Id| self.bus_name(id);
        match t {
            MapTarget::Volume(b) => format!("{} Volume", bus(b)),
            MapTarget::Pan(b) => format!("{} Pan", bus(b)),
            MapTarget::Send { track, index } => {
                let r = self
                    .project
                    .returns
                    .get(index)
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| format!("{}", index + 1));
                format!("{} Send {r}", bus(track))
            }
            MapTarget::Mute(b) => format!("{} Track Activator", bus(b)),
            MapTarget::Solo(b) => format!("{} Solo", bus(b)),
            MapTarget::Arm(b) => format!("{} Arm", bus(b)),
            MapTarget::Device { bus: b, device, index } => {
                let d = self.project.chain(b).and_then(|c| c.iter().find(|d| d.id == device));
                match d {
                    Some(d) => format!(
                        "{} {} {}",
                        bus(b),
                        d.kind().name(),
                        d.params.info().get(index).map(|i| i.name).unwrap_or("?")
                    ),
                    None => format!("{} (missing device)", bus(b)),
                }
            }
            MapTarget::Instrument { track, index } => {
                let name = self
                    .project
                    .track(track)
                    .and_then(|t| instrument_params(&t.instrument).get(index as usize))
                    .map(|s| s.name)
                    .unwrap_or("?");
                format!("{} {name}", bus(track))
            }
            MapTarget::MidiDevice { track, device, index } => {
                let d = self
                    .project
                    .track(track)
                    .and_then(|t| t.midi_devices.iter().find(|d| d.id == device));
                match d {
                    Some(d) => format!(
                        "{} {} {}",
                        bus(track),
                        d.kind().name(),
                        d.params.info().get(index).map(|i| i.name).unwrap_or("?")
                    ),
                    None => format!("{} (missing device)", bus(track)),
                }
            }
            MapTarget::Tempo => "Tempo".into(),
            MapTarget::LaunchClip { track, scene } => format!("{} Clip {}", bus(track), scene + 1),
            MapTarget::LaunchScene(s) => format!("Scene {}", s + 1),
            MapTarget::Play => "Play/Stop".into(),
            MapTarget::Stop => "Stop".into(),
        }
    }

    // ----- applying mappings ------------------------------------------------------

    fn bus_mixer(&mut self, bus: Id) -> Option<&mut MixerParams> {
        self.project.mixer_mut(bus)
    }

    /// Moves a mapping's target to a controller value (0 ..= 127).
    pub fn apply_mapping(&mut self, map: &MidiMapping, value: u8) {
        let n = map.scale(value);
        let pressed = value > 0;
        match map.target {
            MapTarget::Volume(bus) => {
                if let Some(m) = self.bus_mixer(bus) {
                    m.volume_db = VOLUME_SPEC.denormalize(n);
                    self.send_bus_mixer(bus);
                }
            }
            MapTarget::Pan(bus) => {
                if let Some(m) = self.bus_mixer(bus) {
                    m.pan = PAN_SPEC.denormalize(n);
                    self.send_bus_mixer(bus);
                }
            }
            MapTarget::Mute(bus) | MapTarget::Solo(bus) | MapTarget::Arm(bus) => {
                if !pressed {
                    return;
                }
                if let Some(m) = self.bus_mixer(bus) {
                    match map.target {
                        MapTarget::Mute(_) => m.mute = !m.mute,
                        MapTarget::Solo(_) => m.solo = !m.solo,
                        _ => m.arm = !m.arm,
                    }
                    self.send_bus_mixer(bus);
                }
            }
            MapTarget::Send { track, index } => self.set_send(track, index, SEND_SPEC.denormalize(n)),
            MapTarget::Device { bus, device, index } => {
                let info = self
                    .project
                    .chain(bus)
                    .and_then(|c| c.iter().find(|d| d.id == device))
                    .and_then(|d| d.params.info().get(index).copied());
                if let Some(info) = info {
                    self.set_device_param(bus, device, index, info.from_unit(n));
                }
            }
            MapTarget::MidiDevice { track, device, index } => {
                let info = self
                    .project
                    .track(track)
                    .and_then(|t| t.midi_devices.iter().find(|d| d.id == device))
                    .and_then(|d| d.params.info().get(index).copied());
                if let Some(info) = info {
                    self.set_midi_device_param(track, device, index, info.from_unit(n));
                }
            }
            MapTarget::Instrument { track, index } => {
                let Some(t) = self.project.track_mut(track) else { return };
                let Some(spec) = instrument_params(&t.instrument).get(index as usize).copied() else {
                    return;
                };
                let v = spec.denormalize(n);
                if get_instrument_param(&t.instrument, index) == Some(v) {
                    return;
                }
                set_instrument_param(&mut t.instrument, index, v);
                self.send_instrument(track);
            }
            MapTarget::Tempo => {
                let (lo, hi) = TEMPO_RANGE;
                self.set_tempo((lo + n as f64 * (hi - lo)).round());
            }
            MapTarget::LaunchClip { track, scene } => {
                if pressed {
                    self.send(Command::LaunchClip { track, slot: scene });
                    self.selected_clip = Some(ClipRef::Slot { track, scene }).filter(|r| self.clip(*r).is_some());
                } else {
                    self.send(Command::ReleaseClip { track, slot: scene });
                }
            }
            MapTarget::LaunchScene(s) => {
                if pressed {
                    self.send(Command::LaunchScene(s));
                }
            }
            MapTarget::Play => {
                if pressed {
                    self.toggle_play();
                }
            }
            MapTarget::Stop => {
                if pressed {
                    self.send(Command::Stop);
                }
            }
        }
    }

    /// Removes mappings whose target no longer exists.
    pub fn prune_mappings(&mut self) {
        let p = &self.project;
        let exists = |t: &MapTarget| match *t {
            MapTarget::Volume(b) | MapTarget::Pan(b) | MapTarget::Mute(b) | MapTarget::Solo(b) => {
                b == MASTER_BUS || p.chain(b).is_some()
            }
            MapTarget::Arm(b) => p.track(b).is_some(),
            MapTarget::Send { track, index } => p.track(track).is_some() && index < p.returns.len(),
            MapTarget::Device { bus, device, .. } => self.find_device(bus, device).is_some(),
            MapTarget::MidiDevice { track, device, .. } => p.track(track).is_some_and(|t| {
                t.midi_devices.iter().any(|d| d.id == device)
                    || t.midi_rack
                        .as_ref()
                        .is_some_and(|r| r.find_midi_device(device).is_some())
            }),
            MapTarget::Instrument { track, .. } | MapTarget::LaunchClip { track, .. } => p.track(track).is_some(),
            MapTarget::LaunchScene(s) => s < p.scenes.len(),
            MapTarget::Tempo | MapTarget::Play | MapTarget::Stop => true,
        };
        let keep: Vec<bool> = p.midi_mappings.iter().map(|m| exists(&m.target)).collect();
        if keep.iter().any(|k| !k) {
            let mut i = 0;
            self.project.midi_mappings.retain(|_| {
                i += 1;
                keep[i - 1]
            });
        }
    }
}
