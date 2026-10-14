//! Chain templates (track presets): a track's whole device chain saved on
//! its own, so it can be dropped onto another track or start a new one.
//!
//! A template holds the MIDI effects (and MIDI Effect Rack), the instrument
//! with its samples and Instrument or Drum Rack chains, the audio effects
//! (Audio Effect Racks included) and the state of every plug-in in them. It
//! leaves out clips, automation and routing. The track's volume and pan come
//! along too, but only a new track takes them: dropping a template on an
//! existing track keeps its mixer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::fx::rack::Rack;
use crate::fx::{Device, DeviceKind, DeviceParams};
use crate::midi_fx::MidiDevice;
use crate::model::{AudioData, Id, Instrument, Project, SampleRef, Track, TrackKind};
use crate::plugin::PluginSlot;

/// File extension of saved chain templates.
pub const EXTENSION: &str = "tchain";
/// Bump when the format changes incompatibly.
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainPreset {
    #[serde(default)]
    pub format: u32,
    pub name: String,
    /// The kind of track it was saved from. MIDI templates go on MIDI
    /// tracks; audio and group templates (effects only) on audio and group
    /// tracks.
    pub kind: TrackKind,
    #[serde(default = "no_instrument")]
    pub instrument: Instrument,
    /// Instrument samples, with absolute paths.
    #[serde(default)]
    pub samples: Vec<Option<SampleRef>>,
    #[serde(default)]
    pub devices: Vec<Device>,
    #[serde(default)]
    pub midi_devices: Vec<MidiDevice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rack: Option<Rack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_rack: Option<Rack>,
    /// Plug-ins in the chain with their state. Slots are the template's own
    /// device ids; an instrument plug-in uses [`ChainPreset::track`].
    #[serde(default)]
    pub plugins: Vec<PluginSlot>,
    /// Id the track had when it was saved (the instrument plug-in's slot).
    #[serde(default)]
    pub track: Id,
    /// Volume (dB) and pan, applied to new tracks only.
    #[serde(default)]
    pub volume_db: f32,
    #[serde(default)]
    pub pan: f32,
}

fn no_instrument() -> Instrument {
    Instrument::None
}

/// How a template went onto a track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    Done,
    /// The track is of the wrong kind (a MIDI chain on an audio track or the
    /// other way round).
    WrongKind,
    NoTrack,
}

impl ChainPreset {
    /// Captures a track's chain. Relative sample paths are made absolute
    /// against `base` (the project's folder). Plug-in states are whatever
    /// the project holds, so store the running plug-ins' states first.
    pub fn capture(p: &Project, track: Id, name: &str, base: Option<&Path>) -> Option<ChainPreset> {
        let t = p.track(track)?;
        let mut preset = ChainPreset {
            format: FORMAT,
            name: name.to_string(),
            kind: t.kind,
            instrument: t.instrument,
            samples: t.samples.clone(),
            devices: t.devices.clone(),
            midi_devices: t.midi_devices.clone(),
            rack: t.rack.clone(),
            midi_rack: t.midi_rack.clone(),
            plugins: Vec::new(),
            track: t.id,
            volume_db: t.mixer.volume_db,
            pan: t.mixer.pan,
        };
        let slots = preset.plugin_slots();
        preset.plugins = p.plugins.iter().filter(|s| slots.contains(&s.slot)).cloned().collect();
        for s in preset.samples_mut() {
            let path = Path::new(&s.path);
            if path.is_relative() {
                if let Some(b) = base {
                    s.path = b.join(path).to_string_lossy().to_string();
                }
            }
        }
        Some(preset)
    }

    /// Plug-in slots the chain uses.
    fn plugin_slots(&self) -> Vec<Id> {
        let mut out = Vec::new();
        let mut f = |d: &Device| {
            if matches!(d.params, DeviceParams::Plugin(_)) {
                out.push(d.id);
            }
        };
        for d in &self.devices {
            d.visit(&mut f);
        }
        if let Some(r) = &self.rack {
            r.visit_devices(&mut f);
        }
        if matches!(self.instrument, Instrument::Plugin(_)) {
            out.push(self.track);
        }
        out
    }

    fn samples_mut(&mut self) -> impl Iterator<Item = &mut SampleRef> {
        let chains = self
            .rack
            .iter_mut()
            .flat_map(|r| r.chains.iter_mut().chain(r.returns.iter_mut()));
        self.samples
            .iter_mut()
            .flatten()
            .chain(chains.flat_map(|c| c.samples.iter_mut().flatten()))
    }

    /// Short description for the Browser: the instrument and devices.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.kind == TrackKind::Midi && !matches!(self.instrument, Instrument::None) {
            let name = match self.instrument {
                Instrument::Plugin(_) => self
                    .plugins
                    .iter()
                    .find(|s| s.slot == self.track)
                    .map(|s| s.plugin.name.clone())
                    .unwrap_or_else(|| "Plug-in".into()),
                i => i.name().to_string(),
            };
            parts.push(name);
        }
        for d in &self.devices {
            parts.push(match &d.params {
                DeviceParams::Plugin(_) => self
                    .plugins
                    .iter()
                    .find(|s| s.slot == d.id)
                    .map(|s| s.plugin.name.clone())
                    .unwrap_or_else(|| "Plug-in".into()),
                p => p.kind().name().to_string(),
            });
        }
        if parts.is_empty() {
            "Empty chain".into()
        } else {
            parts.join(" > ")
        }
    }

    /// Whether the template fits a track of `kind`.
    pub fn fits(&self, kind: TrackKind) -> bool {
        (self.kind == TrackKind::Midi) == (kind == TrackKind::Midi)
    }

    /// Replaces a track's chain with this template, giving every device
    /// fresh ids and copying the plug-in states to the new slots. Samples
    /// are loaded from disk (missing files stay empty). With `mixer`, the
    /// track also takes the template's volume and pan.
    pub fn apply(&self, p: &mut Project, track: Id, mixer: bool) -> Applied {
        let Some(kind) = p.track(track).map(|t| t.kind) else {
            return Applied::NoTrack;
        };
        if !self.fits(kind) {
            return Applied::WrongKind;
        }
        let mut t = Track {
            id: track,
            kind,
            instrument: if kind == TrackKind::Midi {
                self.instrument
            } else {
                Instrument::None
            },
            samples: self.samples.clone(),
            devices: self.devices.clone(),
            midi_devices: self.midi_devices.clone(),
            rack: self.rack.clone(),
            midi_rack: self.midi_rack.clone(),
            ..p.track(track).cloned().expect("checked")
        };
        let mut map: HashMap<Id, Id> = HashMap::new();
        map.insert(self.track, track);
        crate::arrange::reid_devices(&mut t, &mut || p.alloc_id(), &mut map);
        for d in t.devices.iter_mut() {
            if let Some(r) = d.rack.as_deref_mut() {
                r.remap_ids(&map);
            }
        }
        for r in t.rack.iter_mut().chain(t.midi_rack.iter_mut()) {
            r.remap_ids(&map);
        }
        load_samples(&mut t);
        if mixer {
            t.mixer.volume_db = self.volume_db;
            t.mixer.pan = self.pan;
        }
        // An instrument plug-in's slot is the track itself: the old one goes.
        p.plugins.retain(|s| s.slot != track);
        for s in &self.plugins {
            if let Some(&slot) = map.get(&s.slot) {
                let mut s = s.clone();
                s.slot = slot;
                p.plugins.push(s);
            }
        }
        if let Some(dst) = p.track_mut(track) {
            *dst = t;
        }
        p.prune_plugins();
        Applied::Done
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<ChainPreset, String> {
        let p: ChainPreset = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if p.format > FORMAT {
            return Err("made by a newer version of Tessera".into());
        }
        Ok(p)
    }

    pub fn load(path: &Path) -> Result<ChainPreset, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::from_json(&text)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(path, self.to_json())
    }
}

/// Loads the audio of every sample in a track's instrument and rack chains
/// that isn't loaded yet.
fn load_samples(t: &mut Track) {
    let load = |s: &mut SampleRef| {
        if s.data.is_none() {
            s.data = AudioData::load_wav(&PathBuf::from(&s.path)).ok().map(Arc::new);
        }
    };
    t.samples.iter_mut().flatten().for_each(load);
    for r in t.rack.iter_mut() {
        for c in r.chains.iter_mut().chain(r.returns.iter_mut()) {
            c.samples.iter_mut().flatten().for_each(load);
        }
    }
}

/// Is `p` a chain template file?
pub fn is_chain_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

// ----- factory templates ----------------------------------------------------------

fn fx(id: &mut Id, kind: DeviceKind, preset: Option<&str>) -> Device {
    *id += 1;
    let mut d = Device::new(*id, kind);
    if let Some(name) = preset {
        if let Some((_, params)) = kind.presets().into_iter().find(|(n, _)| *n == name) {
            d.params = params;
        }
    }
    d
}

fn factory(name: &str, kind: TrackKind, instrument: Instrument, devices: Vec<Device>) -> ChainPreset {
    ChainPreset {
        format: FORMAT,
        name: name.to_string(),
        kind,
        instrument,
        samples: Vec::new(),
        devices,
        midi_devices: Vec::new(),
        rack: None,
        midi_rack: None,
        plugins: Vec::new(),
        track: 0,
        volume_db: 0.0,
        pan: 0.0,
    }
}

/// Built-in chain templates so the Browser has a few to start from.
pub fn factory_templates() -> Vec<ChainPreset> {
    use crate::dsp::electric::ElectricParams;
    use crate::dsp::synth::SynthParams;
    use crate::fx::DeviceKind as K;
    let mut id: Id = 1_000_000;
    let mut out = Vec::new();

    let mut d = |kind, preset| fx(&mut id, kind, preset);
    let mut comp = d(K::Compressor, None);
    if let DeviceParams::Compressor(c) = &mut comp.params {
        c.threshold = -20.0;
        c.ratio = 3.0;
        c.makeup = 3.0;
    }
    let mut reverb = d(K::Reverb, None);
    if let DeviceParams::Reverb(r) = &mut reverb.params {
        r.mix = 0.18;
        r.decay = 1.6;
    }
    let mut delay = d(K::Delay, None);
    if let DeviceParams::Delay(x) = &mut delay.params {
        x.mix = 0.15;
        x.feedback = 0.3;
    }
    out.push(factory(
        "Vocal Chain",
        TrackKind::Audio,
        Instrument::None,
        vec![d(K::Eq, None), comp, delay, reverb],
    ));

    out.push(factory(
        "Drum Bus Glue",
        TrackKind::Audio,
        Instrument::None,
        vec![
            d(K::DrumBus, Some("Punchy Kit")),
            d(K::BusCompressor, Some("Drum Smash")),
        ],
    ));

    out.push(factory(
        "Lo-Fi Keys",
        TrackKind::Midi,
        Instrument::Electric(ElectricParams::default()),
        vec![
            d(K::VinylDistortion, Some("Dusty Record")),
            d(K::AutoFilter, Some("Slow Sweep")),
            d(K::Chorus, None),
        ],
    ));

    let mut wide = d(K::Reverb, None);
    if let DeviceParams::Reverb(r) = &mut wide.params {
        r.mix = 0.45;
        r.decay = 6.0;
        r.size = 0.9;
    }
    out.push(factory(
        "Wide Pad",
        TrackKind::Midi,
        Instrument::Synth(SynthParams::pad()),
        vec![d(K::Chorus, None), d(K::Echo, Some("Ambient Wash")), wide],
    ));

    let mut sat = d(K::Saturator, None);
    if let DeviceParams::Saturator(s) = &mut sat.params {
        s.drive = 9.0;
    }
    out.push(factory(
        "Driven Bass",
        TrackKind::Midi,
        Instrument::Synth(SynthParams::bass()),
        vec![sat, d(K::Compressor, None)],
    ));

    out.push(factory(
        "Dub Drums",
        TrackKind::Midi,
        Instrument::DrumRack(crate::dsp::drum_rack::DrumRackParams::boom()),
        vec![d(K::DrumBus, Some("Boom Bap")), d(K::Echo, Some("Dub Tape"))],
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{PluginFormat, PluginInstrument, PluginParams, PluginRef};

    fn plugin(name: &str, instrument: bool) -> PluginRef {
        PluginRef {
            format: PluginFormat::Clap,
            path: format!("/x/{name}.clap"),
            id: name.into(),
            name: name.into(),
            vendor: String::new(),
            instrument,
        }
    }

    #[test]
    fn capture_and_apply_moves_devices_and_plugin_states() {
        let mut p = Project::default();
        let src = p.add_track(TrackKind::Midi, Instrument::Plugin(PluginInstrument::default()));
        let fx_id = p.alloc_id();
        p.track_mut(src).unwrap().devices = vec![
            Device::new(p.alloc_id(), DeviceKind::Reverb),
            Device {
                id: fx_id,
                on: true,
                params: DeviceParams::Plugin(PluginParams::default()),
                rack: None,
            },
        ];
        p.track_mut(src).unwrap().mixer.volume_db = -6.0;
        p.plugins.push(PluginSlot {
            slot: src,
            plugin: plugin("Synth", true),
            state: Some("aW5zdA==".into()),
        });
        p.plugins.push(PluginSlot {
            slot: fx_id,
            plugin: plugin("Verb", false),
            state: Some("ZnhzdGF0ZQ==".into()),
        });

        let preset = ChainPreset::capture(&p, src, "My Chain", None).unwrap();
        assert_eq!(preset.plugins.len(), 2);
        assert_eq!(preset.summary(), "Synth > Reverb > Verb");
        let preset = ChainPreset::from_json(&preset.to_json()).unwrap();

        let dst = p.add_track(TrackKind::Midi, Instrument::Synth(Default::default()));
        assert_eq!(preset.apply(&mut p, dst, false), Applied::Done);
        let t = p.track(dst).unwrap();
        assert!(matches!(t.instrument, Instrument::Plugin(_)));
        assert_eq!(t.devices.len(), 2);
        assert_eq!(t.mixer.volume_db, 0.0, "existing tracks keep their mixer");
        let new_fx = t.devices[1].id;
        assert_ne!(new_fx, fx_id);
        assert_eq!(p.plugin(dst).unwrap().state.as_deref(), Some("aW5zdA=="));
        assert_eq!(p.plugin(new_fx).unwrap().state.as_deref(), Some("ZnhzdGF0ZQ=="));
        // The source keeps its own plug-ins.
        assert!(p.plugin(src).is_some() && p.plugin(fx_id).is_some());
        assert_eq!(p.plugins.len(), 4);

        let audio = p.add_track(TrackKind::Audio, Instrument::None);
        assert_eq!(preset.apply(&mut p, audio, false), Applied::WrongKind);

        // Applying again replaces the chain and drops the old plug-ins.
        let fresh = p.add_track(TrackKind::Midi, Instrument::Synth(Default::default()));
        preset.apply(&mut p, fresh, true);
        preset.apply(&mut p, fresh, true);
        assert_eq!(p.plugins.len(), 6);
        assert_eq!(p.track(fresh).unwrap().mixer.volume_db, -6.0);
    }

    #[test]
    fn relative_sample_paths_become_absolute() {
        let mut p = Project::default();
        let t = p.add_track(TrackKind::Midi, Instrument::Sampler(Default::default()));
        p.track_mut(t).unwrap().set_sample(
            0,
            Some(SampleRef {
                path: "Samples/kick.wav".into(),
                data: None,
            }),
        );
        let base = Path::new("/projects/song");
        let preset = ChainPreset::capture(&p, t, "S", Some(base)).unwrap();
        let path = preset.samples[0].as_ref().unwrap().path.clone();
        assert_eq!(Path::new(&path), base.join("Samples/kick.wav"));
    }

    #[test]
    fn factory_templates_apply_cleanly() {
        let list = factory_templates();
        assert!(list.len() >= 5);
        let mut p = Project::default();
        let midi = p.add_track(TrackKind::Midi, Instrument::Synth(Default::default()));
        let audio = p.add_track(TrackKind::Audio, Instrument::None);
        for t in &list {
            let target = if t.kind == TrackKind::Midi { midi } else { audio };
            assert_eq!(t.apply(&mut p, target, false), Applied::Done, "{}", t.name);
            let ids: Vec<Id> = p.track(target).unwrap().devices.iter().map(|d| d.id).collect();
            assert!(ids.iter().all(|&i| i < 1_000_000), "fresh ids for {}", t.name);
        }
    }
}
