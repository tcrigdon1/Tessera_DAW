//! Device presets: one audio effect (a built-in, a plug-in or a whole Audio
//! Effect Rack) saved on its own with its settings and plug-in states. The
//! same type carries a device on the clipboard (Copy, Cut, Paste and
//! Duplicate in the Device view).

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fx::{Device, DeviceParams};
use crate::model::{Id, Project};
use crate::plugin::PluginSlot;

/// File extension of saved device presets.
pub const EXTENSION: &str = "tdevice";
/// Bump when the format changes incompatibly.
const FORMAT: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePreset {
    #[serde(default)]
    pub format: u32,
    pub name: String,
    pub device: Device,
    /// The state of every plug-in in the device (racks can hold several),
    /// keyed by the device ids saved in `device`.
    #[serde(default)]
    pub plugins: Vec<PluginSlot>,
}

impl DevicePreset {
    /// Captures a device with the plug-in states the project holds, so store
    /// the running plug-ins' states first.
    pub fn capture(p: &Project, device: &Device, name: &str) -> DevicePreset {
        let mut slots = Vec::new();
        device.visit(&mut |d| {
            if matches!(d.params, DeviceParams::Plugin(_)) {
                slots.push(d.id);
            }
        });
        DevicePreset {
            format: FORMAT,
            name: name.to_string(),
            device: device.clone(),
            plugins: p.plugins.iter().filter(|s| slots.contains(&s.slot)).cloned().collect(),
        }
    }

    /// A copy of the device with fresh ids from `alloc` (nested rack macro
    /// mappings follow) and its plug-in slots under the new ids.
    pub fn instantiate(&self, alloc: &mut dyn FnMut() -> Id) -> (Device, Vec<PluginSlot>) {
        let mut d = self.device.clone();
        let mut map: HashMap<Id, Id> = HashMap::new();
        d.reid(alloc, &mut map);
        if let Some(r) = d.rack.as_deref_mut() {
            r.remap_ids(&map);
        }
        let plugins = self
            .plugins
            .iter()
            .filter_map(|s| map.get(&s.slot).map(|&slot| PluginSlot { slot, ..s.clone() }))
            .collect();
        (d, plugins)
    }

    /// Short description for the Browser: what kind of device it is.
    pub fn summary(&self) -> String {
        match &self.device.params {
            DeviceParams::Plugin(_) => self
                .plugins
                .iter()
                .find(|s| s.slot == self.device.id)
                .map(|s| format!("{} ({})", s.plugin.name, s.plugin.format.label()))
                .unwrap_or_else(|| "Plug-in".into()),
            p => p.kind().name().to_string(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<DevicePreset, String> {
        let p: DevicePreset = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if p.format > FORMAT {
            return Err("made by a newer version of Tessera".into());
        }
        Ok(p)
    }

    pub fn load(path: &Path) -> Result<DevicePreset, String> {
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

/// Is `p` a device preset file?
pub fn is_device_file(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(EXTENSION))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::rack::{MacroTarget, Rack, RackChain};
    use crate::fx::DeviceKind;
    use crate::plugin::{PluginFormat, PluginParams, PluginRef};

    fn plugin_ref() -> PluginRef {
        PluginRef {
            id: "test.gain".into(),
            name: "Gain".into(),
            vendor: "Test".into(),
            format: PluginFormat::Clap,
            path: "/x/gain.clap".into(),
            instrument: false,
        }
    }

    #[test]
    fn instantiate_gives_fresh_ids_and_moves_plugin_state() {
        let mut p = Project::default();
        let (rack_id, chain_id, plug_id) = (p.alloc_id(), p.alloc_id(), p.alloc_id());
        let plug = Device {
            id: plug_id,
            on: true,
            params: DeviceParams::Plugin(PluginParams::default()),
            rack: None,
            name: Some("My Gain".into()),
        };
        let mut chain = RackChain::new(chain_id, "Chain");
        chain.devices.push(plug);
        let mut rack = Rack::with_chain(chain);
        rack.map(
            0,
            MacroTarget::Device {
                device: plug_id,
                param: 0,
            },
        );
        let device = Device::new_rack(rack_id, rack);
        p.plugins.push(PluginSlot {
            slot: plug_id,
            plugin: plugin_ref(),
            state: Some("c3RhdGU=".into()),
        });

        let preset = DevicePreset::capture(&p, &device, "Rack");
        assert_eq!(preset.plugins.len(), 1);
        let preset = DevicePreset::from_json(&preset.to_json()).unwrap();

        let mut next = 100;
        let (copy, slots) = preset.instantiate(&mut || {
            next += 1;
            next
        });
        assert_ne!(copy.id, rack_id);
        let r = copy.rack.as_deref().unwrap();
        let inner = &r.chains[0].devices[0];
        assert_ne!(inner.id, plug_id);
        assert_eq!(inner.name.as_deref(), Some("My Gain"));
        assert_eq!(slots.len(), 1);
        assert_eq!(slots[0].slot, inner.id);
        assert_eq!(slots[0].state.as_deref(), Some("c3RhdGU="));
        assert_eq!(
            r.mappings[0].target,
            MacroTarget::Device {
                device: inner.id,
                param: 0
            }
        );
    }

    #[test]
    fn summary_names_the_device() {
        let p = Project::default();
        let d = Device::new(1, DeviceKind::Reverb);
        assert_eq!(DevicePreset::capture(&p, &d, "Big Room").summary(), "Reverb");
        assert!(is_device_file(Path::new("a/Big Room.tdevice")));
    }
}
