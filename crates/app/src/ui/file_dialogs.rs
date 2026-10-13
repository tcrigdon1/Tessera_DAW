//! Small modal windows around project files: "Save changes?", crash
//! recovery, and naming a template.

use eframe::egui::{self, RichText};

use super::theme;
use crate::app::{PendingAction, TesseraApp};
use crate::project_files::ago;

#[derive(Default)]
pub struct FileDialogs {
    /// Name being typed for File → Save as Template.
    pub template_name: Option<String>,
}

fn centered(title: &str) -> egui::Window<'_> {
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
}

pub fn draw(app: &mut TesseraApp, ctx: &egui::Context) {
    recovery(app, ctx);
    save_changes(app, ctx);
    template_name(app, ctx);
}

fn recovery(app: &mut TesseraApp, ctx: &egui::Context) {
    let Some(r) = &app.autosave.recovery else { return };
    let name = if r.info.project_name.is_empty() {
        "Untitled".to_string()
    } else {
        r.info.project_name.clone()
    };
    let when = r.info.saved_at.map(ago).unwrap_or_default();
    let mut choice = None;
    centered("Recover your set?").show(ctx, |ui| {
        ui.label("Tessera didn't quit normally last time.");
        ui.label(format!("An autosave of \"{name}\" from {when} can be recovered."));
        if let Some(p) = &r.info.project_path {
            ui.label(RichText::new(p.display().to_string()).small().color(theme::text_dim()));
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Recover").clicked() {
                choice = Some(true);
            }
            if ui.button("Discard").clicked() {
                choice = Some(false);
            }
        });
    });
    match choice {
        Some(true) => app.recover(),
        Some(false) => app.autosave.discard_recovery(),
        None => {}
    }
}

fn save_changes(app: &mut TesseraApp, ctx: &egui::Context) {
    let Some(action) = app.pending.clone() else { return };
    let mut choice = None;
    centered("Save changes?").show(ctx, |ui| {
        ui.label(format!("Save changes to \"{}\" before {}?", app.project.name, match action {
            PendingAction::Quit => "quitting",
            _ => "closing it",
        }));
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Save").clicked() {
                choice = Some(0);
            }
            if ui.button("Don't Save").clicked() {
                choice = Some(1);
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                choice = Some(2);
            }
        });
    });
    match choice {
        Some(0) => {
            app.pending = None;
            app.save();
            // Saving can be cancelled in the file dialog.
            if !app.dirty {
                app.perform(action);
            }
        }
        Some(1) => {
            app.pending = None;
            app.perform(action);
        }
        Some(_) => app.pending = None,
        None => {}
    }
}

fn template_name(app: &mut TesseraApp, ctx: &egui::Context) {
    let Some(mut name) = app.file_dialogs.template_name.take() else {
        return;
    };
    let mut keep = true;
    centered("Save as Template").show(ctx, |ui| {
        ui.label("Template name:");
        let r = ui.text_edit_singleline(&mut name);
        r.request_focus();
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if let Some(dir) = app.prefs.templates_dir() {
            ui.label(RichText::new(dir.display().to_string()).small().color(theme::text_dim()));
        }
        ui.horizontal(|ui| {
            if (ui.button("Save").clicked() || enter) && !name.trim().is_empty() {
                app.save_as_template(&name);
                keep = false;
            }
            if ui.button("Cancel").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                keep = false;
            }
        });
    });
    if keep {
        app.file_dialogs.template_name = Some(name);
    }
}
