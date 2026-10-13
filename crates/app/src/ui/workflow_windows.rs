//! Floating windows: the undo History and the Groove Pool.

use eframe::egui::{self, RichText};
use tessera_engine::groove;
use tessera_engine::model::Id;

use super::theme;
use crate::app::TesseraApp;

pub fn draw(app: &mut TesseraApp, ctx: &egui::Context) {
    history(app, ctx);
    groove_pool(app, ctx);
}

/// Every undo step; clicking one returns the project to that point.
fn history(app: &mut TesseraApp, ctx: &egui::Context) {
    let mut open = app.work.show_history;
    let mut jump = None;
    egui::Window::new("History")
        .open(&mut open)
        .default_size([240.0, 320.0])
        .show(ctx, |ui| {
            let h = &app.work.history;
            let pos = h.position();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if ui.selectable_label(pos == 0, "Open Project").clicked() {
                    jump = Some(0);
                }
                let done = h.done().map(|l| (l.to_string(), false));
                let undone = h.undone().map(|l| (l.to_string(), true));
                for (i, (label, is_undone)) in done.chain(undone).enumerate() {
                    let text = if is_undone {
                        RichText::new(label).color(theme::text_dim()).italics()
                    } else {
                        RichText::new(label)
                    };
                    if ui.selectable_label(pos == i + 1, text).clicked() {
                        jump = Some(i + 1);
                    }
                }
            });
        });
    app.work.show_history = open;
    if let Some(p) = jump {
        app.history_jump(p);
    }
}

/// Grooves in the project with their settings, the presets to add, and the
/// global groove amount.
fn groove_pool(app: &mut TesseraApp, ctx: &egui::Context) {
    let mut open = app.work.show_grooves;
    let mut add = None;
    let mut remove: Option<Id> = None;
    let mut changed = false;
    egui::Window::new("Groove Pool")
        .open(&mut open)
        .default_size([460.0, 260.0])
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Global Amount");
                let mut pct = app.project.groove_amount * 100.0;
                if ui.add(egui::Slider::new(&mut pct, 0.0..=130.0).suffix(" %")).changed() {
                    app.project.groove_amount = pct / 100.0;
                    changed = true;
                }
                ui.menu_button("Add Groove", |ui| {
                    egui::ScrollArea::vertical().max_height(300.0).show(ui, |ui| {
                        for g in groove::presets() {
                            if ui.button(&g.name).clicked() {
                                add = Some(g);
                                ui.close();
                            }
                        }
                    });
                });
            });
            ui.separator();
            if app.project.grooves.is_empty() {
                ui.label(
                    RichText::new("No grooves yet. Add a preset, or right-click a clip and choose Extract Groove.")
                        .color(theme::text_dim()),
                );
                return;
            }
            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("groove_pool")
                    .num_columns(7)
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["Groove", "Base", "Quantize", "Timing", "Random", "Velocity", ""] {
                            ui.label(RichText::new(h).color(theme::text_dim()).small());
                        }
                        ui.end_row();
                        for g in app.project.grooves.iter_mut() {
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut g.name).desired_width(120.0))
                                .changed();
                            ui.label(base_label(g.base));
                            changed |= ui.add(pct(&mut g.quantize, 1.0, 0.0)).changed();
                            changed |= ui.add(pct(&mut g.timing, groove::MAX_AMOUNT, 0.0)).changed();
                            changed |= ui.add(pct(&mut g.random, 1.0, 0.0)).changed();
                            changed |= ui.add(pct(&mut g.velocity, 1.0, -1.0)).changed();
                            if ui.small_button("✕").on_hover_text("Remove from the pool").clicked() {
                                remove = Some(g.id);
                            }
                            ui.end_row();
                        }
                    });
            });
        });
    app.work.show_grooves = open;
    if changed {
        app.label_edit("Groove Settings");
        app.changed();
    }
    if let Some(g) = add {
        app.add_groove(g);
    }
    if let Some(id) = remove {
        app.remove_groove(id);
    }
}

/// A percentage DragValue over a 0..1-style setting.
fn pct(v: &mut f32, max: f32, min: f32) -> egui::DragValue<'_> {
    egui::DragValue::from_get_set(move |n: Option<f64>| {
        if let Some(n) = n {
            *v = (n as f32 / 100.0).clamp(min, max);
        }
        (*v * 100.0) as f64
    })
    .speed(0.5)
    .suffix("%")
}

fn base_label(base: f64) -> String {
    let triplet = ((1.0 / base) * 3.0).fract().abs() < 1e-6 && (1.0 / base).fract().abs() > 1e-6;
    let div = if triplet {
        (4.0 / base / 1.5).round()
    } else {
        (4.0 / base).round()
    };
    format!("1/{div}{}", if triplet { "T" } else { "" })
}
