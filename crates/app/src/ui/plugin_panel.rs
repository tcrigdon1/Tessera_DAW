//! The body of a VST3/CLAP plug-in in the Device view: what it is, whether
//! it is running, a button for its own editor window, and its parameters.

use eframe::egui::{self, vec2};
use tessera_engine::model::Id;
use tessera_plugin_host::{ParamDesc, Status};

use super::theme;
use crate::app::TesseraApp;

/// Parameters shown before "Show all".
const FIRST_PARAMS: usize = 12;

pub enum PluginAct {
    Param(Id, u32, f64),
    Editor(Id, bool),
    Reload(Id),
    ShowAll(Id, bool),
}

/// What the panel shows, gathered before drawing.
pub struct PluginView {
    pub slot: Id,
    pub name: String,
    pub vendor: String,
    pub format: &'static str,
    pub status: Option<Status>,
    pub has_editor: bool,
    pub editor_open: bool,
    pub params: Vec<ParamDesc>,
    pub show_all: bool,
}

impl PluginView {
    pub fn of(app: &TesseraApp, slot: Id) -> Option<PluginView> {
        let saved = app.project.plugin(slot)?;
        let inst = app.plugins.instances.get(&slot).map(|i| &i.inst);
        Some(PluginView {
            slot,
            name: inst
                .map(|i| i.name())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| saved.plugin.name.clone()),
            vendor: saved.plugin.vendor.clone(),
            format: saved.plugin.format.label(),
            status: inst.map(|i| i.status()),
            has_editor: inst.map(|i| i.has_editor()).unwrap_or(false),
            editor_open: inst.map(|i| i.editor_open()).unwrap_or(false),
            params: inst.map(|i| i.params()).unwrap_or_default(),
            show_all: app.plugin_show_all.contains(&slot),
        })
    }
}

pub fn body(ui: &mut egui::Ui, v: &PluginView, acts: &mut Vec<PluginAct>) {
    ui.set_min_width(230.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(&v.name).strong());
        ui.label(egui::RichText::new(v.format).small().color(theme::text_dim()));
    });
    if !v.vendor.is_empty() {
        ui.label(egui::RichText::new(&v.vendor).small().color(theme::text_dim()));
    }
    let (text, color, reload) = match &v.status {
        None => ("Not available".to_string(), theme::METER_HIGH, true),
        Some(Status::Loading) => ("Loading…".to_string(), theme::text_dim(), false),
        Some(Status::Ready) => (String::new(), theme::text_dim(), false),
        Some(Status::Failed(e)) => (e.clone(), theme::METER_HIGH, true),
        Some(Status::Crashed) => ("Crashed; bypassed".to_string(), theme::METER_HIGH, true),
    };
    if !text.is_empty() {
        ui.label(egui::RichText::new(text).small().color(color));
    }
    ui.horizontal(|ui| {
        if v.has_editor && v.status == Some(Status::Ready) {
            let label = if v.editor_open { "Close Editor" } else { "Edit" };
            if ui
                .button(label)
                .on_hover_text("Open the plug-in's own window")
                .clicked()
            {
                acts.push(PluginAct::Editor(v.slot, !v.editor_open));
            }
        }
        if reload && ui.button("Reload").on_hover_text("Start the plug-in again").clicked() {
            acts.push(PluginAct::Reload(v.slot));
        }
    });
    ui.add_space(2.0);

    let visible: Vec<(u32, &ParamDesc)> = v
        .params
        .iter()
        .enumerate()
        .filter(|(_, p)| !p.hidden)
        .map(|(i, p)| (i as u32, p))
        .collect();
    let shown = if v.show_all {
        visible.len()
    } else {
        visible.len().min(FIRST_PARAMS)
    };
    egui::ScrollArea::vertical()
        .id_salt(("plugin_params", v.slot))
        .max_height(150.0)
        .show(ui, |ui| {
            for &(index, p) in &visible[..shown] {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let (r, _) = ui.allocate_exact_size(vec2(88.0, 16.0), egui::Sense::hover());
                    ui.painter().text(
                        r.left_center(),
                        egui::Align2::LEFT_CENTER,
                        &p.name,
                        egui::FontId::proportional(11.0),
                        theme::text(),
                    );
                    let mut value = p.value;
                    let mut slider = egui::Slider::new(&mut value, 0.0..=1.0).show_value(false);
                    if p.steps > 0 {
                        slider = slider.step_by(1.0 / p.steps as f64);
                    }
                    let resp = ui.add_enabled(!p.read_only, slider);
                    if resp.changed() {
                        acts.push(PluginAct::Param(v.slot, index, value));
                    }
                    if resp.double_clicked() {
                        acts.push(PluginAct::Param(v.slot, index, p.default));
                    }
                    ui.label(egui::RichText::new(tidy(&p.text)).small().color(theme::text_dim()));
                });
            }
        });
    if visible.len() > FIRST_PARAMS {
        let label = if v.show_all {
            "Show fewer".to_string()
        } else {
            format!("Show all {} parameters", visible.len())
        };
        if ui.small_button(label).clicked() {
            acts.push(PluginAct::ShowAll(v.slot, !v.show_all));
        }
    }
}

/// Shortens plain numbers some plug-ins print with six decimals
/// ("50.000000 %" becomes "50 %").
fn tidy(text: &str) -> String {
    let (num, rest) = match text.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-')) {
        Some(i) => text.split_at(i),
        None => (text, ""),
    };
    match num.parse::<f64>() {
        Ok(v) if num.contains('.') && num.len() - num.find('.').unwrap_or(0) > 3 => {
            let s = format!("{v:.2}");
            let s = s.trim_end_matches('0').trim_end_matches('.');
            format!("{s}{rest}")
        }
        _ => text.to_string(),
    }
}

impl TesseraApp {
    pub fn apply_plugin_act(&mut self, act: PluginAct) {
        match act {
            PluginAct::Param(slot, index, value) => {
                if let Some(i) = self.plugins.instances.get(&slot) {
                    i.inst.set_param(index, value);
                    self.touch();
                }
            }
            PluginAct::Editor(slot, open) => {
                let title = self
                    .project
                    .plugin(slot)
                    .map(|p| format!("{} - Tessera", p.plugin.name))
                    .unwrap_or_default();
                if let Some(i) = self.plugins.instances.get(&slot) {
                    if open {
                        i.inst.show_editor(&title);
                    } else {
                        i.inst.hide_editor();
                    }
                }
            }
            PluginAct::Reload(slot) => self.reload_plugin(slot),
            PluginAct::ShowAll(slot, on) => {
                if on {
                    self.plugin_show_all.insert(slot);
                } else {
                    self.plugin_show_all.remove(&slot);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tidies_long_numbers() {
        assert_eq!(super::tidy("50.000000 %"), "50 %");
        assert_eq!(super::tidy("0.123456"), "0.12");
        assert_eq!(super::tidy("1.5 dB"), "1.5 dB");
        assert_eq!(super::tidy("Square"), "Square");
    }
}
