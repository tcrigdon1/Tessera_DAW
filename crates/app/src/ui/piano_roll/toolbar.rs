//! The toolbar above the piano roll and the quantize settings window.

use eframe::egui::{self, vec2};
use tessera_engine::midi::edit;
use tessera_engine::midi::scale::{MusicalScale, ScaleKind, ROOT_NAMES};

use super::super::{theme, widgets};
use super::GridChoice;
use crate::app::{ClipRef, TesseraApp};

pub(super) fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let is_drums = super::super::sample_panels::drum_rows(app, r.track()).is_some();
    let scale = app.clip(r).and_then(|c| c.scale);
    let bpb = app.project.beats_per_bar.max(1) as f64;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let p = &mut app.piano;
        if widgets::toggle(ui, p.draw_mode, "✏", theme::ACCENT, 24.0)
            .on_hover_text("Draw mode (B): click or drag to draw notes, click a note to delete it")
            .clicked()
        {
            p.draw_mode = !p.draw_mode;
        }
        let grid_label = match p.grid {
            GridChoice::Adaptive => format!("Adaptive ({})", step_label(p.step, bpb)),
            g => g.label().to_string(),
        };
        egui::ComboBox::from_id_salt("piano_grid_choice")
            .width(96.0)
            .selected_text(grid_label)
            .show_ui(ui, |ui| {
                for g in GridChoice::ALL {
                    ui.selectable_value(&mut p.grid, g, g.label());
                }
            })
            .response
            .on_hover_text("Grid (Ctrl+1 narrower, Ctrl+2 wider)");
        if ui
            .small_button("Fit")
            .on_hover_text("Fit the clip into the view (or double-click the ruler)")
            .clicked()
        {
            p.fit = true;
        }
        ui.separator();
        ui.add_enabled_ui(!is_drums, |ui| {
            if widgets::toggle(ui, p.fold, "Fold", theme::ACCENT, 36.0)
                .on_hover_text("Show only rows that have notes")
                .clicked()
            {
                p.fold = !p.fold;
            }
        });
        // Scale: kind (or Off) and root.
        let mut new_scale = scale;
        egui::ComboBox::from_id_salt("piano_scale_kind")
            .width(90.0)
            .selected_text(scale.map(|s| s.kind.label()).unwrap_or("Scale: Off"))
            .show_ui(ui, |ui| {
                if ui.selectable_label(scale.is_none(), "Off").clicked() {
                    new_scale = None;
                }
                for k in ScaleKind::ALL {
                    if ui
                        .selectable_label(scale.is_some_and(|s| s.kind == k), k.label())
                        .clicked()
                    {
                        new_scale = Some(MusicalScale::new(scale.map(|s| s.root).unwrap_or(0), k));
                    }
                }
            });
        ui.add_enabled_ui(scale.is_some(), |ui| {
            egui::ComboBox::from_id_salt("piano_scale_root")
                .width(36.0)
                .selected_text(ROOT_NAMES[scale.map(|s| s.root as usize).unwrap_or(0) % 12])
                .show_ui(ui, |ui| {
                    for (i, name) in ROOT_NAMES.iter().enumerate() {
                        let on = scale.is_some_and(|s| s.root as usize == i);
                        if ui.selectable_label(on, *name).clicked() {
                            if let Some(s) = scale {
                                new_scale = Some(MusicalScale::new(i as u8, s.kind));
                            }
                        }
                    }
                });
            if widgets::toggle(ui, p.fold_to_scale, "In Key", theme::ACCENT, 44.0)
                .on_hover_text("Fold to scale: show only rows in the scale")
                .clicked()
            {
                p.fold_to_scale = !p.fold_to_scale;
            }
        });
        if widgets::toggle(ui, p.show_velocity, "Vel", theme::ACCENT, 30.0)
            .on_hover_text("Show the velocity lane")
            .clicked()
        {
            p.show_velocity = !p.show_velocity;
        }
        ui.separator();
        if ui
            .small_button("Quantize")
            .on_hover_text("Quantize the selected notes, or all (Ctrl+U)")
            .clicked()
        {
            app.quantize_command();
        }
        if ui
            .small_button("…")
            .on_hover_text("Quantize settings (Ctrl+Shift+U)")
            .clicked()
        {
            app.piano.quantize_open = !app.piano.quantize_open;
        }
        if ui
            .small_button("Legato")
            .on_hover_text("Extend notes to the next one (Ctrl+J)")
            .clicked()
        {
            super::legato(app, r);
        }
        if ui
            .small_button("Reverse")
            .on_hover_text("Reverse notes in time")
            .clicked()
        {
            super::apply(app, r, true, |notes, sel, _| edit::reverse(notes, sel));
        }
        if ui.small_button("Invert").on_hover_text("Mirror pitches").clicked() {
            super::apply(app, r, true, |notes, sel, _| edit::invert(notes, sel));
        }
        if ui
            .small_button("Duplicate")
            .on_hover_text("Copy the selection (or all notes) right after itself (Ctrl+D)")
            .clicked()
        {
            super::duplicate(app, r);
        }
        if ui
            .small_button("×2 Loop")
            .on_hover_text("Double the loop and copy its notes")
            .clicked()
        {
            super::double_loop(app, r);
        }
        if new_scale != scale {
            if let Some(c) = app.clip_mut(r) {
                c.scale = new_scale;
            }
            app.changed();
        }
    });
    ui.add_space(2.0);
}

/// A grid step as a note value ("1/16", "1/8T", "2 Bars").
fn step_label(step: f64, bpb: f64) -> String {
    if let Some(g) = GridChoice::ALL
        .iter()
        .find(|g| g.beats().is_some_and(|b| (b - step).abs() < 1e-9))
    {
        return g.label().to_string();
    }
    if step >= bpb - 1e-9 {
        let bars = (step / bpb).round();
        format!("{bars} Bar{}", if bars == 1.0 { "" } else { "s" })
    } else {
        format!("1/{}", (4.0 / step).round())
    }
}

/// Grid sizes the Quantize window offers, in beats.
const QUANTIZE_GRIDS: [f64; 9] = [4.0, 2.0, 1.0, 0.5, 1.0 / 3.0, 0.25, 1.0 / 6.0, 0.125, 1.0 / 12.0];

/// Quick settings: (label, grid, triplets, strength, swing).
const QUANTIZE_PRESETS: [(&str, f64, bool, f64, f64); 5] = [
    ("1/16 tight", 0.25, false, 1.0, 0.0),
    ("1/16 loose", 0.25, false, 0.5, 0.0),
    ("1/16 swing", 0.25, false, 1.0, 0.5),
    ("1/8 + 1/8T", 0.5, true, 1.0, 0.0),
    ("1/8 swing", 0.5, false, 1.0, 0.6),
];

fn percent(ui: &mut egui::Ui, v: &mut f64, hint: &str) {
    let mut pct = *v * 100.0;
    ui.add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix(" %").fixed_decimals(0))
        .on_hover_text(hint);
    *v = pct / 100.0;
}

/// Quantize settings (Ctrl+Shift+U), like Ableton's: grid, starts and ends,
/// amount and swing, plus a catch range and humanize. Ctrl+U applies the
/// last settings.
pub(super) fn quantize_window(app: &mut TesseraApp, ctx: &egui::Context) {
    if !app.piano.quantize_open {
        return;
    }
    let mut open = true;
    let mut apply = false;
    let before = (app.prefs.quantize, app.prefs.quantize_follow_grid);
    let bpb = app.project.beats_per_bar.max(1) as f64;
    let step = app.piano.step;
    let what = if super::active_clip(app).is_none() {
        "the selected clip"
    } else if app.piano.selection.is_empty() {
        "all notes"
    } else {
        "the selected notes"
    };
    egui::Window::new("Quantize")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(260.0)
        .show(ctx, |ui| {
            let follow = &mut app.prefs.quantize_follow_grid;
            let q = &mut app.prefs.quantize;
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                for (label, grid, triplets, strength, swing) in QUANTIZE_PRESETS {
                    if ui.small_button(label).clicked() {
                        *follow = false;
                        q.grid = grid;
                        q.triplets = triplets;
                        q.strength = strength;
                        q.swing = swing;
                    }
                }
            });
            ui.add_space(4.0);
            egui::Grid::new("quantize_settings")
                .num_columns(2)
                .spacing(vec2(8.0, 6.0))
                .show(ui, |ui| {
                    ui.label("Grid");
                    let current = format!("Current ({})", step_label(step, bpb));
                    let label = if *follow { current.clone() } else { step_label(q.grid, bpb) };
                    egui::ComboBox::from_id_salt("quantize_grid")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(*follow, current).clicked() {
                                *follow = true;
                            }
                            for g in QUANTIZE_GRIDS {
                                let on = !*follow && (q.grid - g).abs() < 1e-9;
                                if ui.selectable_label(on, step_label(g, bpb)).clicked() {
                                    *follow = false;
                                    q.grid = g;
                                }
                            }
                        });
                    ui.end_row();
                    ui.label("");
                    ui.checkbox(&mut q.triplets, "Add triplets")
                        .on_hover_text("Also snap to the triplet grid of the same size, like 1/16 + 1/16T.");
                    ui.end_row();
                    ui.label("Adjust");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut q.starts, "Note start");
                        ui.checkbox(&mut q.ends, "Note end");
                    });
                    ui.end_row();
                    ui.label("Amount");
                    percent(ui, &mut q.strength, "How far notes move towards the grid. 100 % lands them on it.");
                    ui.end_row();
                    ui.label("Swing");
                    percent(ui, &mut q.swing, "Pushes every other grid line late for a shuffle feel.");
                    ui.end_row();
                    ui.label("Range");
                    percent(
                        ui,
                        &mut q.range,
                        "Only notes this close to a grid line move (100 % moves every note). Lower it to keep deliberate pushes and drags.",
                    );
                    ui.end_row();
                    ui.label("Humanize");
                    percent(
                        ui,
                        &mut q.humanize,
                        "After quantizing, nudges each note slightly off the grid so it feels played.",
                    );
                    ui.end_row();
                });
            if !q.starts && !q.ends {
                ui.label(
                    egui::RichText::new("Tick Note start or Note end to quantize.")
                        .small()
                        .color(theme::text_dim()),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                apply = ui.button("Quantize").clicked();
                ui.label(egui::RichText::new(format!("{what} (Ctrl+U)")).color(theme::text_dim()));
                if ui.small_button("Reset").on_hover_text("Back to the default settings").clicked() {
                    *q = edit::Quantize::default();
                    *follow = true;
                }
            });
        });
    if apply {
        app.quantize_command();
    }
    if (app.prefs.quantize, app.prefs.quantize_follow_grid) != before {
        app.save_prefs();
    }
    app.piano.quantize_open = open;
}
