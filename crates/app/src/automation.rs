//! Automation on the app side: showing automated values on the controls,
//! overriding automation by hand, re-enabling it, and recording parameter
//! moves into envelopes while Automation Arm is on.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;

use tessera_engine::automation::{
    available_targets, get_instrument_param, instrument_params, lane_mut, set_instrument_param, specs_from_info,
    target_name, target_spec, AutomationLane, AutomationTarget, ParamSpec,
};
use tessera_engine::fx::{Device, DeviceParams, MAX_RETURNS};
use tessera_engine::model::{Id, Instrument, MixerParams};

use crate::app::{ClipRef, TesseraApp};

/// How long after the last move a recording gesture ends, in seconds.
const TOUCH_TIMEOUT: f64 = 0.4;

type Key = (Id, AutomationTarget);

/// Which part of the clip view is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipTab {
    #[default]
    Main,
    Envelopes,
}

#[derive(Debug, Clone, Copy)]
struct Touch {
    /// Last beat written (song beat, or clip beat when `clip` is set).
    beat: f64,
    time: f64,
    clip: Option<ClipRef>,
}

#[derive(Default)]
pub struct AutomationUi {
    /// Automation Arm: moving a parameter while playing records it.
    pub arm: bool,
    /// Tracks whose automation lanes are open in the Arrangement.
    pub expanded: HashSet<Id>,
    pub clip_tab: ClipTab,
    /// Parameter shown in the clip envelope editor.
    pub clip_target: Option<AutomationTarget>,
    /// Values the controls showed last frame, to tell which one the user
    /// moved.
    last_mixer: HashMap<Id, MixerParams>,
    last_instrument: HashMap<Id, Instrument>,
    /// Keys the engine is currently automating.
    automated: HashSet<Key>,
    touching: HashMap<Key, Touch>,
    /// Keys whose engine value is ignored until the given time, while an
    /// override reaches the audio thread.
    suppress: HashMap<Key, f64>,
    now: f64,
}

impl TesseraApp {
    /// Copies automated values from the engine onto the controls, so faders
    /// and knobs move with the automation. Called once per frame.
    pub fn sync_automation(&mut self, now: f64) {
        self.automation.now = now;
        let mut resend_mixer = Vec::new();
        let mut resend_instrument = Vec::new();
        let mut automated = HashSet::new();
        for ti in 0..self.project.tracks.len() {
            let Some(ts) = self.engine.shared.tracks.get(ti) else {
                break;
            };
            let t = &mut self.project.tracks[ti];
            if ts.id.load(Ordering::Relaxed) != t.id {
                continue;
            }
            let id = t.id;
            let touching = &self.automation.touching;
            let suppress = &self.automation.suppress;
            let mut apply = |target: AutomationTarget, v: f32, set: &mut dyn FnMut(f32)| {
                let key = (id, target);
                if v.is_nan() || touching.contains_key(&key) || suppress.get(&key).is_some_and(|&u| now < u) {
                    return false;
                }
                set(v);
                automated.insert((id, target));
                true
            };
            apply(AutomationTarget::Volume, ts.auto_volume.load(), &mut |v| {
                t.mixer.volume_db = v
            });
            apply(AutomationTarget::Pan, ts.auto_pan.load(), &mut |v| t.mixer.pan = v);
            for i in 0..instrument_params(&t.instrument).len() {
                let v = ts.auto_params[i].load();
                let instrument = &mut t.instrument;
                apply(AutomationTarget::Instrument(i as u16), v, &mut |v| {
                    set_instrument_param(instrument, i as u16, v)
                });
            }
            // When automation stops (a clip envelope's clip stopped), the
            // parameter keeps its last value: make that the engine's value.
            for target in [AutomationTarget::Volume, AutomationTarget::Pan] {
                if self.automation.automated.contains(&(id, target)) && !automated.contains(&(id, target)) {
                    resend_mixer.push(id);
                }
            }
            if self
                .automation
                .automated
                .iter()
                .any(|k| k.0 == id && matches!(k.1, AutomationTarget::Instrument(_)) && !automated.contains(k))
            {
                resend_instrument.push(id);
            }
            self.automation.last_mixer.insert(id, t.mixer);
            self.automation.last_instrument.insert(id, t.instrument);
        }
        self.automation.automated = automated;
        self.automation.suppress.retain(|_, until| now < *until);
        resend_mixer.dedup();
        for id in resend_mixer {
            self.send_mixer(id);
        }
        for id in resend_instrument {
            self.send_instrument(id);
        }

        // Finished recording gestures hand the lane back to the engine.
        let ended: Vec<(Key, Touch)> = self
            .automation
            .touching
            .iter()
            .filter(|(_, t)| now - t.time > TOUCH_TIMEOUT)
            .map(|(k, t)| (*k, *t))
            .collect();
        if !ended.is_empty() {
            for (key, touch) in ended {
                self.automation.touching.remove(&key);
                if let Some(l) = self.recording_lane(key, touch.clip) {
                    l.overridden = false;
                }
            }
            self.changed();
        }
    }

    /// Called after the user moved a mixer control of `track`.
    pub(crate) fn mixer_edited(&mut self, track: Id) {
        let Some(t) = self.project.track(track) else { return };
        let Some(before) = self.automation.last_mixer.get(&track).copied() else {
            return;
        };
        let mut moved = Vec::new();
        if (t.mixer.volume_db - before.volume_db).abs() > 1e-6 {
            moved.push((AutomationTarget::Volume, t.mixer.volume_db));
        }
        if (t.mixer.pan - before.pan).abs() > 1e-6 {
            moved.push((AutomationTarget::Pan, t.mixer.pan));
        }
        self.automation.last_mixer.insert(track, t.mixer);
        for (target, v) in moved {
            self.param_moved(track, target, v);
        }
    }

    /// Called after the user changed instrument settings of `track`.
    pub(crate) fn instrument_edited(&mut self, track: Id) {
        let Some(t) = self.project.track(track) else { return };
        let instrument = t.instrument;
        let Some(before) = self.automation.last_instrument.get(&track).copied() else {
            return;
        };
        self.automation.last_instrument.insert(track, instrument);
        if before.kind_tag() != instrument.kind_tag() {
            return;
        }
        for i in 0..instrument_params(&instrument).len() as u16 {
            let (a, b) = (get_instrument_param(&before, i), get_instrument_param(&instrument, i));
            if let (Some(a), Some(b)) = (a, b) {
                if (a - b).abs() > 1e-6 {
                    self.param_moved(track, AutomationTarget::Instrument(i), b);
                }
            }
        }
    }

    /// The user moved a parameter by hand: record it when armed and playing,
    /// otherwise override any automation driving it.
    fn param_moved(&mut self, track: Id, target: AutomationTarget, value: f32) {
        if let AutomationTarget::Instrument(index) = target {
            self.learn_touch(tessera_engine::midi::mapping::MapTarget::Instrument { track, index });
        }
        let key = (track, target);
        if self.automation.arm && self.is_playing() {
            self.record(track, target, value);
            return;
        }
        if self.automation.automated.contains(&key) {
            let mut any = false;
            if let Some(t) = self.project.track_mut(track) {
                let clips = t
                    .slots
                    .iter_mut()
                    .flatten()
                    .chain(t.arrangement.iter_mut().map(|a| &mut a.clip));
                for lanes in std::iter::once(&mut t.automation).chain(clips.map(|c| &mut c.envelopes)) {
                    for l in lanes
                        .iter_mut()
                        .filter(|l| l.target == target && !l.envelope.is_empty())
                    {
                        l.overridden = true;
                        any = true;
                    }
                }
            }
            if any {
                self.automation.automated.remove(&key);
                self.automation.suppress.insert(key, self.automation.now + 0.25);
                self.status = "Automation overridden. Click Re-Enable Automation to follow it again.".into();
                self.changed();
            }
        }
    }

    /// Writes a recorded value into the playing Session clip's envelope, or
    /// the track's Arrangement lane.
    fn record(&mut self, track: Id, target: AutomationTarget, value: f32) {
        let Some(ti) = self.project.track_index(track) else {
            return;
        };
        let Some(spec) = self.automation_target_spec(track, target) else {
            return;
        };
        let norm = spec.normalize(value);
        let shared = self.engine.shared.tracks.get(ti);
        let session = shared.and_then(|s| {
            let active = s.id.load(Ordering::Relaxed) == track && s.session_active.load(Ordering::Relaxed);
            let slot = s.playing_slot.load(Ordering::Relaxed);
            (active && slot >= 0).then(|| (slot as usize, s.clip_position.load()))
        });
        let (clip, beat) = match session {
            Some((scene, pos)) => (Some(ClipRef::Slot { track, scene }), pos),
            None => (None, self.song_beat()),
        };
        let key = (track, target);
        let now = self.automation.now;
        let last = self
            .automation
            .touching
            .get(&key)
            .filter(|t| t.clip == clip)
            .map(|t| t.beat);
        if let Some(l) = self.recording_lane(key, clip) {
            l.envelope.record(last, beat, norm);
            // Follow the hand, not the old envelope, until the gesture ends.
            l.overridden = true;
        }
        self.automation.touching.insert(key, Touch { beat, time: now, clip });
        self.status = "Recording automation".into();
        self.changed();
    }

    fn recording_lane(&mut self, key: Key, clip: Option<ClipRef>) -> Option<&mut AutomationLane> {
        let lanes = match clip {
            Some(r) => &mut self.clip_mut(r)?.envelopes,
            None => &mut self.project.track_mut(key.0)?.automation,
        };
        Some(lane_mut(lanes, key.1))
    }

    /// True if the user has overridden any automation.
    pub fn automation_overridden(&self) -> bool {
        self.project.tracks.iter().any(|t| {
            t.automation.iter().any(|l| l.overridden)
                || t.slots
                    .iter()
                    .flatten()
                    .chain(t.arrangement.iter().map(|a| &a.clip))
                    .any(|c| c.envelopes.iter().any(|l| l.overridden))
        })
    }

    /// Makes every overridden lane and envelope drive its parameter again.
    pub fn reenable_automation(&mut self) {
        for t in &mut self.project.tracks {
            for l in t.automation.iter_mut() {
                l.overridden = false;
            }
            let clips = t
                .slots
                .iter_mut()
                .flatten()
                .chain(t.arrangement.iter_mut().map(|a| &mut a.clip));
            for c in clips {
                for l in c.envelopes.iter_mut() {
                    l.overridden = false;
                }
            }
        }
        self.automation.touching.clear();
        self.status = "Automation re-enabled".into();
        self.changed();
    }

    /// Automation targets for a track: its own parameters, its sends, and
    /// the parameters of the effects in its device chain (racks included)
    /// and of its plug-ins.
    pub fn automation_targets(&self, track: Id) -> Vec<AutomationTarget> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let mut v = available_targets(&t.instrument);
        let returns = self.project.returns.len().min(MAX_RETURNS) as u8;
        v.extend((0..returns).map(AutomationTarget::Send));
        let mut slots: Vec<(Id, DeviceParams)> = Vec::new();
        if let Instrument::Plugin(_) = t.instrument {
            slots.push((track, DeviceParams::Plugin(Default::default())));
        }
        for d in &t.devices {
            d.visit(&mut |d| slots.push((d.id, d.params)));
        }
        for (device, params) in slots {
            if !matches!(params, DeviceParams::Plugin(_)) {
                v.extend((0..params.info().len() as u16).map(|param| AutomationTarget::Device { device, param }));
                continue;
            }
            if let Some(i) = self.plugins.instances.get(&device) {
                i.inst.with_params(|params| {
                    for (n, p) in params.iter().enumerate() {
                        if p.automatable && !p.hidden && !p.read_only {
                            v.push(AutomationTarget::Device {
                                device,
                                param: n as u16,
                            });
                        }
                    }
                });
            }
        }
        v
    }

    /// The targets of a track for a lane chooser: each with the device it
    /// belongs to (shown as a heading) and its name under that heading.
    pub fn automation_target_menu(&self, track: Id) -> Vec<(AutomationTarget, Option<String>, String)> {
        self.automation_targets(track)
            .into_iter()
            .map(|t| {
                let name = self.automation_target_name(track, t);
                let group = self.automation_target_device(track, t).map(|(g, _)| g);
                let short = group
                    .as_ref()
                    .and_then(|g| name.strip_prefix(&format!("{g}: ")))
                    .map(str::to_string)
                    .unwrap_or_else(|| name.clone());
                (t, group, short)
            })
            .collect()
    }

    /// The device that owns a device target: its name in the lane chooser
    /// (numbered when the track has several of the same kind) and the
    /// device itself, or `None` for a plug-in instrument.
    pub fn automation_target_device(&self, track: Id, target: AutomationTarget) -> Option<(String, Option<&Device>)> {
        let AutomationTarget::Device { device, .. } = target else {
            return None;
        };
        let t = self.project.track(track)?;
        let mut all: Vec<&Device> = Vec::new();
        for d in &t.devices {
            d.visit(&mut |d| all.push(d));
        }
        let name = |id: Id, d: Option<&Device>| match self.project.plugin(id) {
            Some(slot) => slot.plugin.name.clone(),
            None => d.map(|d| d.kind().name().to_string()).unwrap_or_default(),
        };
        let Some(i) = all.iter().position(|d| d.id == device) else {
            // A plug-in instrument (its slot is the track).
            return (device == track && self.project.plugin(device).is_some()).then(|| (name(device, None), None));
        };
        let d = all[i];
        let label = name(d.id, Some(d));
        let same = |o: &&&Device| o.kind() == d.kind() && name(o.id, Some(o)) == label;
        if all.iter().filter(same).count() > 1 {
            let n = all[..=i].iter().filter(same).count();
            return Some((format!("{label} {n}"), Some(d)));
        }
        Some((label, Some(d)))
    }

    /// Display name of an automation target on a track.
    pub fn automation_target_name(&self, track: Id, target: AutomationTarget) -> String {
        let AutomationTarget::Device { device, param } = target else {
            let instrument = self
                .project
                .track(track)
                .map(|t| t.instrument)
                .unwrap_or(Instrument::None);
            return target_name(target, &instrument);
        };
        let Some((label, d)) = self.automation_target_device(track, target) else {
            return format!("Device Parameter {}", param + 1);
        };
        let name = match d.map(|d| d.params) {
            Some(p) if !matches!(p, DeviceParams::Plugin(_)) => {
                p.info().get(param as usize).map(|i| i.name.to_string())
            }
            _ => self
                .plugins
                .instances
                .get(&device)
                .and_then(|i| i.inst.with_params(|ps| ps.get(param as usize).map(|p| p.name.clone()))),
        };
        format!(
            "{label}: {}",
            name.unwrap_or_else(|| format!("Parameter {}", param + 1))
        )
    }

    /// The value range of an automation target on a track, in the units the
    /// device shows.
    pub fn automation_target_spec(&self, track: Id, target: AutomationTarget) -> Option<ParamSpec> {
        let t = self.project.track(track)?;
        if let (AutomationTarget::Device { param, .. }, Some((_, Some(d)))) =
            (target, self.automation_target_device(track, target))
        {
            // Plug-in parameters are already normalised (see `target_spec`).
            if !matches!(d.params, DeviceParams::Plugin(_)) {
                let info = d.params.info().get(param as usize)?;
                return Some(specs_from_info::<1>(std::slice::from_ref(info))[0]);
            }
        }
        target_spec(target, &t.instrument)
    }

    pub fn toggle_automation_lanes(&mut self, track: Id) {
        if !self.automation.expanded.remove(&track) {
            self.automation.expanded.insert(track);
        }
    }
}
