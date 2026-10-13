//! Rack panels in the Device view: macro knobs with mapping and
//! variations, the chain list with activator, solo, volume and pan, and the
//! zone editor (key, velocity and chain-select zones with fades).
//!
//! Drawing only reads the rack document and pushes [`RackAct`]s; the
//! Device view applies them after the frame is drawn.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};
use tessera_engine::automation::{instrument_params, PAN_SPEC};
use tessera_engine::fx::rack::{
    ChainMixer, ChainZones, MacroTarget, Rack, RackChain, RackParams, Zone, MACROS, MAX_RACK_RETURNS,
};
use tessera_engine::fx::{Device, DeviceKind};
use tessera_engine::model::{Id, Instrument};

use super::{theme, widgets};
use crate::app::TesseraApp;
use crate::racks::RackRef;

/// An edit made in a rack panel.
#[derive(Debug, Clone)]
pub enum RackAct {
    Macro(usize, f32),
    Selector(f32),
    AddChain,
    RemoveChain(Id),
    DuplicateChain(Id),
    SelectChain(usize),
    RenameChain(Id, String),
    Mixer(Id, ChainMixer),
    Zones(Id, ChainZones),
    Send(Id, usize, f32),
    ChainInstrument(Id, Box<Instrument>),
    Map(usize, MacroTarget),
    Unmap(MacroTarget),
    Range(MacroTarget, f32, f32),
    RenameMacro(usize, String),
    StoreVariation,
    RecallVariation(usize),
    OverwriteVariation(usize),
    DeleteVariation(usize),
    Randomize,
    /// Show or hide the macros, chain list or zone editor.
    View {
        macros: bool,
        chains: bool,
        zones: bool,
    },
    AddReturn(Option<DeviceKind>),
    RemoveReturn(Id),
}

const CHAIN_ROW: f32 = 20.0;
const MACRO_COLORS: [Color32; MACROS] = [
    Color32::from_rgb(0xff, 0x9c, 0x2b),
    Color32::from_rgb(0xf5, 0xc5, 0x3c),
    Color32::from_rgb(0x8c, 0xe9, 0x9a),
    Color32::from_rgb(0x63, 0xe6, 0xbe),
    Color32::from_rgb(0x74, 0xc0, 0xfc),
    Color32::from_rgb(0xb1, 0x97, 0xfc),
    Color32::from_rgb(0xf7, 0x83, 0xac),
    Color32::from_rgb(0xff, 0x6b, 0x6b),
];

/// Applies the edits of a rack panel.
pub fn apply(app: &mut TesseraApp, r: RackRef, acts: Vec<RackAct>) {
    for a in acts {
        match a {
            RackAct::Macro(i, v) => app.set_macro(r, i, v),
            RackAct::Selector(v) => app.set_chain_selector(r, v),
            RackAct::AddChain => app.add_chain(r, None),
            RackAct::RemoveChain(c) => app.remove_chain(r, c),
            RackAct::DuplicateChain(c) => app.duplicate_chain(r, c),
            RackAct::SelectChain(i) => app.select_chain(r, i),
            RackAct::RenameChain(c, n) => app.rename_chain(r, c, n),
            RackAct::Mixer(c, m) => app.set_chain_mixer(r, c, m),
            RackAct::Zones(c, z) => app.set_chain_zones(r, c, z),
            RackAct::Send(c, i, db) => app.set_chain_send(r.bus(), c, i, db),
            RackAct::ChainInstrument(c, inst) => app.set_chain_instrument(r, c, *inst),
            RackAct::Map(i, t) => app.map_macro(r, i, t),
            RackAct::Unmap(t) => app.unmap(r, t),
            RackAct::Range(t, lo, hi) => app.set_mapping_range(r, t, lo, hi),
            RackAct::RenameMacro(i, n) => app.rename_macro(r, i, n),
            RackAct::StoreVariation => app.store_variation(r),
            RackAct::RecallVariation(i) => app.recall_variation(r, i),
            RackAct::OverwriteVariation(i) => app.overwrite_variation(r, i),
            RackAct::DeleteVariation(i) => app.delete_variation(r, i),
            RackAct::Randomize => {
                let seed = (app.song_beat() * 1e6) as u64 ^ 0x9e37_79b9_7f4a_7c15;
                app.randomize_macros(r, seed);
            }
            RackAct::View { macros, chains, zones } => {
                if let Some(k) = app.rack_mut(r) {
                    k.show_macros = macros;
                    k.show_chains = chains;
                    k.show_zones = zones;
                    app.dirty = true;
                }
            }
            RackAct::AddReturn(kind) => app.add_rack_return(r.bus(), kind),
            RackAct::RemoveReturn(c) => app.remove_chain(r, c),
        }
    }
}

/// View toggles shown in a rack's title bar.
pub fn view_toggles(ui: &mut egui::Ui, rack: &Rack, acts: &mut Vec<RackAct>) {
    let (m, c, z) = (rack.show_macros, rack.show_chains, rack.show_zones);
    if widgets::toggle(ui, m, "Macro", theme::ACCENT, 38.0)
        .on_hover_text("Show or hide the macro knobs")
        .clicked()
    {
        acts.push(RackAct::View {
            macros: !m,
            chains: c,
            zones: z,
        });
    }
    if widgets::toggle(ui, c, "Chain", theme::ACCENT, 38.0)
        .on_hover_text("Show or hide the chain list")
        .clicked()
    {
        acts.push(RackAct::View {
            macros: m,
            chains: !c,
            zones: z,
        });
    }
    if widgets::toggle(ui, z, "Zone", theme::ACCENT, 38.0)
        .on_hover_text("Show the key, velocity and chain-select zones")
        .clicked()
    {
        acts.push(RackAct::View {
            macros: m,
            chains: c || !z,
            zones: !z,
        });
    }
}

/// Labels for every parameter a macro of this rack can control, grouped by
/// device or chain.
fn map_targets(rack: &Rack) -> Vec<(String, Vec<(String, MacroTarget)>)> {
    let mut out = Vec::new();
    for c in &rack.chains {
        let mut chain_items = vec![
            ("Chain Volume".to_string(), MacroTarget::ChainVolume(c.id)),
            ("Chain Pan".to_string(), MacroTarget::ChainPan(c.id)),
        ];
        for (i, spec) in instrument_params(&c.instrument).iter().enumerate() {
            chain_items.push((
                format!("{}: {}", c.instrument.name(), spec.name),
                MacroTarget::Instrument {
                    chain: c.id,
                    param: i as u16,
                },
            ));
        }
        out.push((c.name.clone(), chain_items));
        for d in &c.devices {
            d.visit(&mut |d: &Device| {
                let info = d.params.info();
                if info.is_empty() {
                    return;
                }
                let items = info
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        (
                            p.name.to_string(),
                            MacroTarget::Device {
                                device: d.id,
                                param: i as u16,
                            },
                        )
                    })
                    .collect();
                out.push((format!("{} / {}", c.name, d.kind().name()), items));
            });
        }
    }
    for c in &rack.returns {
        let mut items = vec![("Return Volume".to_string(), MacroTarget::ChainVolume(c.id))];
        items.push(("Return Pan".to_string(), MacroTarget::ChainPan(c.id)));
        out.push((c.name.clone(), items));
    }
    out
}

/// Human-readable name of a mapping target.
pub fn target_label(rack: &Rack, t: MacroTarget) -> String {
    match t {
        MacroTarget::ChainVolume(c) => format!("{} Volume", rack.chain(c).map(|c| c.name.as_str()).unwrap_or("?")),
        MacroTarget::ChainPan(c) => format!("{} Pan", rack.chain(c).map(|c| c.name.as_str()).unwrap_or("?")),
        MacroTarget::Instrument { chain, param } => {
            let c = rack.chain(chain);
            let name = c
                .and_then(|c| instrument_params(&c.instrument).get(param as usize))
                .map(|s| s.name)
                .unwrap_or("?");
            format!("{}: {name}", c.map(|c| c.name.as_str()).unwrap_or("?"))
        }
        MacroTarget::Device { device, param } => match rack.find_device(device) {
            Some(d) => format!(
                "{}: {}",
                d.kind().name(),
                d.params.info().get(param as usize).map(|p| p.name).unwrap_or("?")
            ),
            None => "Missing device".into(),
        },
    }
}

/// The macro section: eight knobs (mapped ones coloured), the variation
/// list and the mapping editor.
pub fn macros(ui: &mut egui::Ui, salt: Id, rack: &Rack, p: &RackParams, acts: &mut Vec<RackAct>) {
    let map_id = ui.id().with(("macro_map", salt));
    let mut mapping: bool = ui.data(|d| d.get_temp(map_id)).unwrap_or(false);
    let values = p.macros();
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            if widgets::toggle(ui, mapping, "Map", theme::ACCENT, 32.0)
                .on_hover_text("Show and edit macro mappings")
                .clicked()
            {
                mapping = !mapping;
            }
            if ui
                .small_button("Rand")
                .on_hover_text("Randomise the mapped macros")
                .clicked()
            {
                acts.push(RackAct::Randomize);
            }
            if ui
                .small_button("Store")
                .on_hover_text("Store the macros as a new variation")
                .clicked()
            {
                acts.push(RackAct::StoreVariation);
            }
        });
        if mapping {
            mapping_list(ui, salt, rack, acts);
            return;
        }
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                for row in 0..2 {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 1.0;
                        for col in 0..4 {
                            let i = row * 4 + col;
                            macro_knob(ui, salt, rack, i, values[i], acts);
                        }
                    });
                }
            });
            variations(ui, rack, acts);
        });
    });
    ui.data_mut(|d| d.insert_temp(map_id, mapping));
}

fn macro_knob(ui: &mut egui::Ui, salt: Id, rack: &Rack, i: usize, value: f32, acts: &mut Vec<RackAct>) {
    let name = rack.macro_name(i);
    let mapped = rack.mappings.iter().filter(|m| m.macro_index as usize == i).count();
    let mut v = value;
    let resp = ui
        .scope(|ui| {
            if mapped == 0 {
                ui.visuals_mut().widgets.inactive.fg_stroke.color = theme::TEXT_DIM;
            }
            let changed = widgets::knob(ui, &name, &mut v, 0.0, 1.0, 0.0, widgets::Scale::Linear, |x| {
                format!("{:.0}", x * 127.0)
            });
            if changed {
                acts.push(RackAct::Macro(i, v));
            }
        })
        .response;
    // Mapped macros get a coloured underline.
    if mapped > 0 {
        let r = resp.rect;
        ui.painter().hline(
            r.left() + 8.0..=r.right() - 8.0,
            r.bottom() - 1.0,
            Stroke::new(2.0, MACRO_COLORS[i]),
        );
    }
    let resp = resp.interact(Sense::click());
    let resp = resp.on_hover_text(if mapped == 0 {
        format!("{name}: right-click to map a parameter")
    } else {
        format!("{name}: {mapped} mapping(s), right-click to edit")
    });
    resp.context_menu(|ui| {
        ui.menu_button("Map to", |ui| {
            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                for (group, items) in map_targets(rack) {
                    ui.menu_button(group, |ui| {
                        egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                            for (label, t) in items {
                                let taken = rack.mapping_for(t).map(|m| m.macro_index as usize);
                                let text = match taken {
                                    Some(m) if m == i => format!("{label} (this macro)"),
                                    Some(m) => format!("{label} (Macro {})", m + 1),
                                    None => label,
                                };
                                if ui.button(text).clicked() {
                                    acts.push(RackAct::Map(i, t));
                                    ui.close();
                                }
                            }
                        });
                    });
                }
            });
        });
        let mine: Vec<MacroTarget> = rack
            .mappings
            .iter()
            .filter(|m| m.macro_index as usize == i)
            .map(|m| m.target)
            .collect();
        if !mine.is_empty() {
            ui.menu_button("Unmap", |ui| {
                for t in mine {
                    if ui.button(target_label(rack, t)).clicked() {
                        acts.push(RackAct::Unmap(t));
                        ui.close();
                    }
                }
            });
        }
        ui.separator();
        let edit_id = ui.id().with(("macro_name", salt, i));
        let mut text: String = ui.data(|d| d.get_temp(edit_id)).unwrap_or_else(|| name.clone());
        ui.horizontal(|ui| {
            ui.label("Name");
            let r = ui.text_edit_singleline(&mut text);
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                acts.push(RackAct::RenameMacro(i, text.clone()));
                ui.close();
            }
        });
        ui.data_mut(|d| d.insert_temp(edit_id, text));
    });
}

fn variations(ui: &mut egui::Ui, rack: &Rack, acts: &mut Vec<RackAct>) {
    ui.vertical(|ui| {
        ui.set_width(84.0);
        ui.label(egui::RichText::new("Variations").size(10.0).color(theme::TEXT_DIM));
        egui::ScrollArea::vertical()
            .id_salt("variations")
            .max_height(96.0)
            .show(ui, |ui| {
                if rack.variations.is_empty() {
                    ui.label(
                        egui::RichText::new("Store saves\nthe macros")
                            .size(9.0)
                            .color(theme::TEXT_DIM),
                    );
                }
                for (i, v) in rack.variations.iter().enumerate() {
                    let r = ui
                        .add_sized(
                            vec2(80.0, 16.0),
                            egui::Button::new(egui::RichText::new(&v.name).size(10.0)),
                        )
                        .on_hover_text("Click to recall, right-click for more");
                    if r.clicked() {
                        acts.push(RackAct::RecallVariation(i));
                    }
                    r.context_menu(|ui| {
                        if ui.button("Overwrite with current macros").clicked() {
                            acts.push(RackAct::OverwriteVariation(i));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            acts.push(RackAct::DeleteVariation(i));
                            ui.close();
                        }
                    });
                }
            });
    });
}

/// Every mapping with its range (Min / Max as percent of the target range).
fn mapping_list(ui: &mut egui::Ui, salt: Id, rack: &Rack, acts: &mut Vec<RackAct>) {
    egui::ScrollArea::vertical()
        .id_salt(("mappings", salt))
        .max_height(110.0)
        .show(ui, |ui| {
            if rack.mappings.is_empty() {
                ui.label(
                    egui::RichText::new("No mappings. Right-click a macro knob\nand choose Map to.")
                        .size(10.0)
                        .color(theme::TEXT_DIM),
                );
            }
            egui::Grid::new(("mapgrid", salt))
                .spacing(vec2(6.0, 2.0))
                .show(ui, |ui| {
                    for m in &rack.mappings {
                        let i = m.macro_index as usize;
                        ui.label(
                            egui::RichText::new(format!("M{}", i + 1))
                                .size(10.0)
                                .color(MACRO_COLORS[i]),
                        );
                        ui.label(egui::RichText::new(target_label(rack, m.target)).size(10.0));
                        let (mut lo, mut hi) = (m.min * 100.0, m.max * 100.0);
                        let a = ui.add(egui::DragValue::new(&mut lo).range(0.0..=100.0).suffix("%").speed(0.5));
                        let b = ui.add(egui::DragValue::new(&mut hi).range(0.0..=100.0).suffix("%").speed(0.5));
                        if a.changed() || b.changed() {
                            acts.push(RackAct::Range(m.target, lo / 100.0, hi / 100.0));
                        }
                        if ui.small_button("x").on_hover_text("Remove mapping").clicked() {
                            acts.push(RackAct::Unmap(m.target));
                        }
                        ui.end_row();
                    }
                });
        });
}

/// The chain list: select, activator, solo, volume and pan per chain. For a
/// Drum Rack `pads` is true: chains are fixed (one per pad) and show sends.
pub fn chain_list(ui: &mut egui::Ui, salt: Id, rack: &Rack, pads: bool, acts: &mut Vec<RackAct>) {
    ui.vertical(|ui| {
        ui.set_width(if pads { 250.0 } else { 220.0 });
        if !pads {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Chains").size(10.0).color(theme::TEXT_DIM));
                if ui.small_button("+").on_hover_text("Add a chain").clicked() {
                    acts.push(RackAct::AddChain);
                }
            });
        }
        egui::ScrollArea::vertical()
            .id_salt(("chains", salt))
            .max_height(if pads { 120.0 } else { 104.0 })
            .show(ui, |ui| {
                if rack.chains.is_empty() {
                    ui.label(
                        egui::RichText::new("Drop an instrument or effect,\nor add a chain with +")
                            .size(10.0)
                            .color(theme::TEXT_DIM),
                    );
                }
                for (i, c) in rack.chains.iter().enumerate() {
                    chain_row(ui, salt, c, i, i == rack.selected_chain, !pads, acts);
                }
            });
    });
}

fn chain_row(
    ui: &mut egui::Ui,
    salt: Id,
    c: &RackChain,
    index: usize,
    selected: bool,
    editable: bool,
    acts: &mut Vec<RackAct>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut m = c.mixer;
        let on = !m.mute;
        if widgets::toggle(ui, on, "", theme::ACCENT, 14.0)
            .on_hover_text("Chain activator")
            .clicked()
        {
            m.mute = on;
        }
        let rename_id = ui.id().with(("rename_chain", salt, c.id));
        let renaming: Option<String> = ui.data(|d| d.get_temp(rename_id));
        if let Some(mut text) = renaming {
            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(96.0));
            r.request_focus();
            if r.lost_focus() {
                acts.push(RackAct::RenameChain(c.id, text.clone()));
                ui.data_mut(|d| d.remove::<String>(rename_id));
            } else {
                ui.data_mut(|d| d.insert_temp(rename_id, text));
            }
        } else {
            let (rect, resp) = ui.allocate_exact_size(vec2(96.0, CHAIN_ROW), Sense::click());
            let p = ui.painter();
            p.rect_filled(
                rect,
                2.0,
                if selected {
                    Color32::from_rgb(0x5a, 0x4c, 0x36)
                } else {
                    Color32::from_rgb(0x3a, 0x3a, 0x3a)
                },
            );
            let mut name = c.name.clone();
            if name.chars().count() > 14 {
                name = name.chars().take(13).collect::<String>() + "…";
            }
            p.text(
                pos2(rect.left() + 4.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                name,
                egui::FontId::proportional(11.0),
                if m.mute { theme::TEXT_DIM } else { theme::TEXT },
            );
            if resp.clicked() {
                acts.push(RackAct::SelectChain(index));
            }
            if editable && resp.double_clicked() {
                ui.data_mut(|d| d.insert_temp(rename_id, c.name.clone()));
            }
            let resp = resp.on_hover_text("Click to show the chain's devices, double-click to rename");
            if editable {
                resp.context_menu(|ui| {
                    if ui.button("Rename").clicked() {
                        ui.data_mut(|d| d.insert_temp(rename_id, c.name.clone()));
                        ui.close();
                    }
                    if ui.button("Duplicate").clicked() {
                        acts.push(RackAct::DuplicateChain(c.id));
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        acts.push(RackAct::RemoveChain(c.id));
                        ui.close();
                    }
                });
            }
        }
        if widgets::toggle(ui, m.solo, "S", theme::SOLO, 16.0)
            .on_hover_text("Solo chain")
            .clicked()
        {
            m.solo = !m.solo;
        }
        let mut db = m.volume_db;
        let r = ui
            .add(
                egui::DragValue::new(&mut db)
                    .range(-70.0..=6.0)
                    .speed(0.2)
                    .custom_formatter(|v, _| widgets::format_db(v as f32)),
            )
            .on_hover_text("Chain volume");
        if r.changed() {
            m.volume_db = db;
        }
        let mut pan = m.pan;
        if widgets::pan_knob(ui, &mut pan) {
            m.pan = pan;
        }
        if m != c.mixer {
            acts.push(RackAct::Mixer(c.id, m));
        }
    });
}

/// Send knobs of a Drum Rack pad chain, one per rack return.
pub fn pad_sends(ui: &mut egui::Ui, rack: &Rack, chain: &RackChain, acts: &mut Vec<RackAct>) {
    if rack.returns.is_empty() {
        return;
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, _) in rack.returns.iter().enumerate().take(MAX_RACK_RETURNS) {
            let mut db = chain
                .sends
                .get(i)
                .copied()
                .unwrap_or(tessera_engine::model::SEND_OFF_DB);
            if widgets::send_knob(ui, &mut db, (b'A' + i as u8) as char) {
                acts.push(RackAct::Send(chain.id, i, db));
            }
        }
    });
}

/// The zone editor: the chain selector on top, then per chain three bars
/// (key, velocity, select) drawn on a 0..127 ruler. Drag a bar's middle to
/// move it, its ends to resize, and with Shift to set the fades.
pub fn zone_editor(
    ui: &mut egui::Ui,
    salt: Id,
    rack: &Rack,
    p: Option<&RackParams>,
    keys: bool,
    acts: &mut Vec<RackAct>,
) {
    let width = 300.0;
    ui.vertical(|ui| {
        ui.set_width(width + 70.0);
        if let Some(p) = p {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Chain Select").size(10.0).color(theme::TEXT_DIM));
                let mut v = p.chain_select;
                let r = ui.add(egui::Slider::new(&mut v, 0.0..=127.0).integer().show_value(true));
                if r.changed() {
                    acts.push(RackAct::Selector(v.round()));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.add_space(70.0);
            ui.label(
                egui::RichText::new(if keys {
                    "Key · Velocity · Select (Shift-drag ends for fades)"
                } else {
                    "Chain select zones (Shift-drag ends for fades)"
                })
                .size(9.0)
                .color(theme::TEXT_DIM),
            );
        });
        egui::ScrollArea::vertical()
            .id_salt(("zones", salt))
            .max_height(100.0)
            .show(ui, |ui| {
                for c in &rack.chains {
                    let mut z = c.zones;
                    ui.horizontal(|ui| {
                        let mut name = c.name.clone();
                        if name.chars().count() > 10 {
                            name = name.chars().take(9).collect::<String>() + "…";
                        }
                        ui.add_sized(vec2(66.0, 14.0), egui::Label::new(egui::RichText::new(name).size(10.0)));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            if keys {
                                zone_bar(
                                    ui,
                                    (salt, c.id, 0),
                                    &mut z.key,
                                    width,
                                    Color32::from_rgb(0x74, 0xc0, 0xfc),
                                    true,
                                );
                                zone_bar(
                                    ui,
                                    (salt, c.id, 1),
                                    &mut z.velocity,
                                    width,
                                    Color32::from_rgb(0x8c, 0xe9, 0x9a),
                                    false,
                                );
                            }
                            let sel = p.map(|p| p.chain_select);
                            zone_bar_with_marker(ui, (salt, c.id, 2), &mut z.select, width, theme::ACCENT, sel);
                        });
                    });
                    if z != c.zones {
                        acts.push(RackAct::Zones(c.id, z));
                    }
                }
            });
    });
}

fn zone_bar(ui: &mut egui::Ui, salt: (Id, Id, u8), z: &mut Zone, width: f32, color: Color32, piano: bool) {
    let r = zone_bar_inner(ui, salt, z, width, color, piano, None);
    let _ = r;
}

fn zone_bar_with_marker(
    ui: &mut egui::Ui,
    salt: (Id, Id, u8),
    z: &mut Zone,
    width: f32,
    color: Color32,
    marker: Option<f32>,
) {
    zone_bar_inner(ui, salt, z, width, color, false, marker);
}

#[allow(clippy::too_many_arguments)]
fn zone_bar_inner(
    ui: &mut egui::Ui,
    salt: (Id, Id, u8),
    z: &mut Zone,
    width: f32,
    color: Color32,
    piano: bool,
    marker: Option<f32>,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 10.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 1.0, Color32::from_rgb(0x1e, 0x1e, 0x1e));
    let x_of = |v: f32| rect.left() + rect.width() * (v / 128.0);
    let v_of = |x: f32| ((x - rect.left()) / rect.width() * 128.0).clamp(0.0, 127.0);
    if piano {
        for oct in 0..11 {
            let x = x_of(oct as f32 * 12.0);
            painter.vline(x, rect.y_range(), Stroke::new(1.0, Color32::from_rgb(0x33, 0x33, 0x33)));
        }
    }
    let (lo, hi) = (z.lo as f32, z.hi as f32 + 1.0);
    let body = Rect::from_min_max(pos2(x_of(lo), rect.top() + 1.0), pos2(x_of(hi), rect.bottom() - 1.0));
    painter.rect_filled(body, 1.0, theme::dim(color, 0.55));
    // Fades as darker triangles at the ends.
    if z.fade_lo > 0 {
        let fx = x_of(lo + z.fade_lo as f32);
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos2(body.left(), body.bottom()),
                pos2(fx, body.top()),
                pos2(body.left(), body.top()),
            ],
            Color32::from_rgb(0x22, 0x22, 0x22),
            Stroke::NONE,
        ));
    }
    if z.fade_hi > 0 {
        let fx = x_of(hi - z.fade_hi as f32);
        painter.add(egui::Shape::convex_polygon(
            vec![
                pos2(body.right(), body.bottom()),
                pos2(body.right(), body.top()),
                pos2(fx, body.top()),
            ],
            Color32::from_rgb(0x22, 0x22, 0x22),
            Stroke::NONE,
        ));
    }
    painter.rect_stroke(body, 1.0, Stroke::new(1.0, color), StrokeKind::Inside);
    if let Some(m) = marker {
        let x = x_of(m + 0.5);
        painter.vline(x, rect.y_range(), Stroke::new(1.5, theme::SELECT));
    }
    // Dragging: which part is held is remembered for the whole drag.
    let drag_id = ui.id().with(("zone_drag", salt.0, salt.1, salt.2));
    if resp.drag_started() {
        if let Some(pp) = resp.interact_pointer_pos() {
            let part = if (pp.x - body.left()).abs() < 5.0 {
                1u8
            } else if (pp.x - body.right()).abs() < 5.0 {
                2
            } else {
                0
            };
            ui.data_mut(|d| d.insert_temp(drag_id, (part, v_of(pp.x), *z)));
        }
    }
    if resp.dragged() {
        if let (Some(pp), Some((part, start, orig))) = (
            resp.interact_pointer_pos(),
            ui.data(|d| d.get_temp::<(u8, f32, Zone)>(drag_id)),
        ) {
            let v = v_of(pp.x).round() as i32;
            let shift = ui.input(|i| i.modifiers.shift);
            let mut nz = orig;
            match (part, shift) {
                (0, _) => {
                    let d = v - start.round() as i32;
                    let span = orig.hi as i32 - orig.lo as i32;
                    let lo = (orig.lo as i32 + d).clamp(0, 127 - span);
                    nz.lo = lo as u8;
                    nz.hi = (lo + span) as u8;
                }
                (1, false) => nz.lo = v.clamp(0, orig.hi as i32) as u8,
                (2, false) => nz.hi = v.clamp(orig.lo as i32, 127) as u8,
                (1, true) => nz.fade_lo = (v - orig.lo as i32).clamp(0, 127) as u8,
                (2, true) => nz.fade_hi = (orig.hi as i32 - v).clamp(0, 127) as u8,
                _ => {}
            }
            *z = nz.normalized();
        }
    }
    if resp.double_clicked() {
        *z = Zone::FULL;
    }
    let tip = if piano {
        format!(
            "{} – {} (fades {} / {}) · double-click to reset",
            note_name(z.lo),
            note_name(z.hi),
            z.fade_lo,
            z.fade_hi
        )
    } else {
        format!(
            "{} – {} (fades {} / {}) · double-click to reset",
            z.lo, z.hi, z.fade_lo, z.fade_hi
        )
    };
    resp.on_hover_text(tip)
}

/// Note name in the C3 = 60 convention.
fn note_name(n: u8) -> String {
    const NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
    format!("{}{}", NAMES[n as usize % 12], n as i32 / 12 - 2)
}

/// A combo choosing a chain's instrument (Instrument Rack).
pub fn chain_instrument_picker(ui: &mut egui::Ui, c: &RackChain, acts: &mut Vec<RackAct>) {
    egui::ComboBox::from_id_salt(("chain_inst", c.id))
        .width(110.0)
        .selected_text(c.instrument.name())
        .show_ui(ui, |ui| {
            if ui.selectable_label(false, "No Instrument").clicked() {
                acts.push(RackAct::ChainInstrument(c.id, Box::new(Instrument::None)));
            }
            for p in tessera_engine::library::instruments()
                .into_iter()
                .chain(tessera_engine::library::sounds())
                .chain(tessera_engine::library::drum_kits())
            {
                if matches!(p.instrument, Instrument::Rack(_) | Instrument::Plugin(_)) {
                    continue;
                }
                let label = if p.category == "Instruments" {
                    p.name.to_string()
                } else {
                    format!("{}: {}", p.category, p.name)
                };
                if ui.selectable_label(false, label).clicked() {
                    acts.push(RackAct::ChainInstrument(c.id, Box::new(p.instrument)));
                }
            }
        });
}

/// Rack returns of a Drum Rack, with add and delete.
pub fn returns_header(ui: &mut egui::Ui, rack: &Rack, acts: &mut Vec<RackAct>) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Returns").size(10.0).color(theme::TEXT_DIM));
        if rack.returns.len() < MAX_RACK_RETURNS {
            ui.menu_button(egui::RichText::new("+").small(), |ui| {
                if ui.button("Empty Return").clicked() {
                    acts.push(RackAct::AddReturn(None));
                    ui.close();
                }
                for k in [DeviceKind::Reverb, DeviceKind::Delay, DeviceKind::Chorus] {
                    if ui.button(k.name()).clicked() {
                        acts.push(RackAct::AddReturn(Some(k)));
                        ui.close();
                    }
                }
            });
        }
        for r in &rack.returns {
            let resp = ui.add(egui::Label::new(egui::RichText::new(&r.name).size(10.0)).sense(Sense::click()));
            resp.context_menu(|ui| {
                if ui.button("Delete Return").clicked() {
                    acts.push(RackAct::RemoveReturn(r.id));
                    ui.close();
                }
            });
        }
    });
}

/// Small pan helper so the spec stays referenced for formatting pan values.
#[allow(dead_code)]
fn pan_text(v: f32) -> String {
    PAN_SPEC.format(v)
}
