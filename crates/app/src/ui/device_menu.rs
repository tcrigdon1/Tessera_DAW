//! A device's title bar (fold triangle, activator, name, editor, move and
//! close buttons) and its right-click menu, shared by audio effects,
//! plug-ins, racks and instruments in the Device view.

use eframe::egui::{self, pos2, vec2, Color32, Sense, Stroke};
use tessera_engine::fx::DeviceParams;

use super::info_view::InfoExt;
use super::theme;
use super::widgets;
use crate::app::TesseraApp;

/// What the title bar or its menu asked for.
#[derive(Debug, Clone)]
pub enum MenuAct {
    Select,
    Delete,
    Rename,
    Cut,
    Copy,
    Paste,
    Duplicate,
    SetOn(bool),
    Editor(bool),
    Fold,
    Move(i32),
    SavePreset,
    Group,
    Ungroup,
    Reset(Box<DeviceParams>),
    /// Instruments: save the track's chain as a template.
    SaveChain,
    /// Instruments: wrap the instrument into an Instrument Rack.
    MakeRack,
}

/// What a device is, for its title bar and menu.
#[derive(Default)]
pub struct Title {
    pub name: String,
    /// `None` for instruments, which have no activator.
    pub on: Option<bool>,
    /// A plug-in with an editor window: whether it is open.
    pub editor: Option<bool>,
    pub folded: bool,
    pub selected: bool,
    /// Move Left / Move Right available (effects only).
    pub left: Option<bool>,
    pub right: Option<bool>,
    /// An effect (Cut, Copy, Paste, Duplicate, Save Preset, Group into Rack).
    pub effect: bool,
    pub is_rack: bool,
    pub can_paste: bool,
    /// Built-in presets of the device kind.
    pub presets: Vec<(&'static str, DeviceParams)>,
    pub defaults: Option<DeviceParams>,
    /// Instruments that can go into an Instrument Rack.
    pub can_make_rack: bool,
    /// Instruments on a track (not in a rack chain): Save Chain as Template.
    pub can_save_chain: bool,
    pub renamable: bool,
}

impl Title {
    pub fn effect(app: &TesseraApp, d: &tessera_engine::fx::Device, editor: Option<bool>) -> Title {
        Title {
            name: app.device_title(d),
            on: Some(d.on),
            editor,
            folded: app.devices.folded.contains(&d.id),
            effect: true,
            is_rack: d.rack.is_some(),
            can_paste: app.devices.clipboard.is_some(),
            presets: if matches!(d.params, DeviceParams::Plugin(_)) {
                Vec::new()
            } else {
                d.kind().presets()
            },
            defaults: (!matches!(d.params, DeviceParams::Plugin(_) | DeviceParams::Rack(_)))
                .then(|| d.kind().default_params()),
            renamable: true,
            ..Default::default()
        }
    }
}

/// An X drawn with lines (every font has it this way).
pub fn close_button(ui: &mut egui::Ui, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let hot = resp.hovered();
    if hot {
        ui.painter().rect_filled(rect, 2.0, theme::METER_HIGH);
    }
    let c = if hot { Color32::WHITE } else { theme::text() };
    let r = rect.shrink(4.5);
    let s = Stroke::new(1.6, c);
    ui.painter().line_segment([r.left_top(), r.right_bottom()], s);
    ui.painter().line_segment([r.right_top(), r.left_bottom()], s);
    resp.info("Delete Device", tip)
}

/// A triangle pointing right (folded) or down (open).
fn fold_button(ui: &mut egui::Ui, folded: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(12.0, 16.0), Sense::click());
    let c = if resp.hovered() {
        theme::text()
    } else {
        theme::text_dim()
    };
    let m = rect.center();
    let pts = if folded {
        vec![
            pos2(m.x - 3.0, m.y - 4.0),
            pos2(m.x + 3.0, m.y),
            pos2(m.x - 3.0, m.y + 4.0),
        ]
    } else {
        vec![
            pos2(m.x - 4.0, m.y - 2.5),
            pos2(m.x + 4.0, m.y - 2.5),
            pos2(m.x, m.y + 3.5),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(pts, c, Stroke::NONE));
    resp.info(
        "Show/Hide Device",
        "Folds the device down to its title bar, or opens it again. Double-clicking the title does the same.",
    )
}

/// Draws the title bar; returns what was clicked.
pub fn title_bar(ui: &mut egui::Ui, t: &Title, out: &mut Vec<MenuAct>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        if fold_button(ui, t.folded).clicked() {
            out.push(MenuAct::Fold);
        }
        if let Some(on) = t.on {
            let r = widgets::toggle(ui, on, "", theme::ACCENT, 14.0).info(
                "Device Activator",
                "Switches the device on or off. A device that is off passes the audio through untouched.",
            );
            if r.clicked() {
                out.push(MenuAct::SetOn(!on));
            }
        }
        let lit = t.on.unwrap_or(true);
        let mut text = egui::RichText::new(t.name.to_uppercase()).small().color(if lit {
            theme::ACCENT
        } else {
            theme::text_dim()
        });
        if t.selected {
            text = text.strong();
        }
        let label = ui.add(egui::Label::new(text).sense(Sense::click())).info(
            "Device Title",
            "Click to select the device (Delete removes it). Double-click to fold it. Right-click for Delete, Rename, Copy, Paste, Duplicate, presets and more.",
        );
        if label.double_clicked() {
            out.push(MenuAct::Fold);
        } else if label.clicked() || label.secondary_clicked() {
            out.push(MenuAct::Select);
        }
        label.context_menu(|ui| menu(ui, t, out));
        ui.add_space(6.0);
        if let Some(open) = t.editor {
            let r = widgets::toggle(ui, open, "Edit", theme::ACCENT, 30.0).info(
                "Plug-in Editor",
                "Opens or closes the plug-in's own window.",
            );
            if r.clicked() {
                out.push(MenuAct::Editor(!open));
            }
        }
        let small = |ui: &mut egui::Ui, text: &str, enabled: bool, title: &str, tip: &str| {
            ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(text).small()).small())
                .info(title, tip)
                .clicked()
        };
        if let Some(left) = t.left {
            if small(ui, "<", left, "Move Left", "Moves the device one place earlier in the chain.") {
                out.push(MenuAct::Move(-1));
            }
        }
        if let Some(right) = t.right {
            if small(ui, ">", right, "Move Right", "Moves the device one place later in the chain.") {
                out.push(MenuAct::Move(1));
            }
        }
        let tip = "Removes the device from the chain (Delete key on a selected device). Plug-ins close their window and unload.";
        if close_button(ui, tip).clicked() {
            out.push(MenuAct::Delete);
        }
    });
}

/// The right-click menu of a device title.
pub fn menu(ui: &mut egui::Ui, t: &Title, out: &mut Vec<MenuAct>) {
    ui.set_min_width(190.0);
    fn item(ui: &mut egui::Ui, enabled: bool, label: &str, keys: &str, act: MenuAct, out: &mut Vec<MenuAct>) {
        let b = egui::Button::new(label).shortcut_text(keys);
        if ui.add_enabled(enabled, b).clicked() {
            out.push(act);
            ui.close();
        }
    }
    if t.effect {
        item(ui, true, "Cut", "Ctrl+X", MenuAct::Cut, out);
        item(ui, true, "Copy", "Ctrl+C", MenuAct::Copy, out);
        item(ui, t.can_paste, "Paste", "Ctrl+V", MenuAct::Paste, out);
        item(ui, true, "Duplicate", "Ctrl+D", MenuAct::Duplicate, out);
    }
    item(ui, true, "Delete", "Del", MenuAct::Delete, out);
    if t.renamable {
        item(ui, true, "Rename", "Ctrl+R", MenuAct::Rename, out);
    }
    ui.separator();
    if let Some(on) = t.on {
        item(
            ui,
            true,
            if on { "Deactivate" } else { "Activate" },
            "",
            MenuAct::SetOn(!on),
            out,
        );
    }
    if let Some(open) = t.editor {
        let label = if open {
            "Close Plug-in Window"
        } else {
            "Open Plug-in Window"
        };
        item(ui, true, label, "", MenuAct::Editor(!open), out);
    }
    item(ui, true, if t.folded { "Show" } else { "Hide" }, "", MenuAct::Fold, out);
    if t.left.is_some() || t.right.is_some() {
        item(ui, t.left.unwrap_or(false), "Move Left", "", MenuAct::Move(-1), out);
        item(ui, t.right.unwrap_or(false), "Move Right", "", MenuAct::Move(1), out);
    }
    ui.separator();
    if t.effect {
        item(ui, true, "Save Preset…", "", MenuAct::SavePreset, out);
    }
    if let Some(d) = &t.defaults {
        item(ui, true, "Reset to Defaults", "", MenuAct::Reset(Box::new(*d)), out);
    }
    if !t.presets.is_empty() {
        ui.menu_button("Presets", |ui| {
            for (name, p) in &t.presets {
                if ui.button(*name).clicked() {
                    out.push(MenuAct::Reset(Box::new(*p)));
                    ui.close();
                }
            }
        });
    }
    if t.can_save_chain {
        item(ui, true, "Save Chain as Template…", "", MenuAct::SaveChain, out);
    }
    if t.effect {
        item(ui, true, "Group into Rack", "Ctrl+G", MenuAct::Group, out);
        if t.is_rack {
            item(ui, true, "Ungroup", "", MenuAct::Ungroup, out);
        }
    }
    if t.can_make_rack {
        item(ui, true, "Group into Instrument Rack", "", MenuAct::MakeRack, out);
    }
}
