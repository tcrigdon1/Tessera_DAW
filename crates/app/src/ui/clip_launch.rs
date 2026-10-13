//! Clip view sections for the markers (start, loop) and, for Session clips,
//! the launch settings and follow actions.

use eframe::egui::{self, vec2, RichText};
use tessera_engine::model::{FollowKind, LaunchMode};
use tessera_engine::time::Quantization;

use super::theme;
use crate::app::{ClipRef, TesseraApp};

/// Marker positions are edited in 1/16 steps.
const STEP: f64 = 0.25;

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).small().strong().color(theme::TEXT_DIM));
}

/// Edits a beat position shown as `bar.beat.sixteenth` like Ableton.
fn position_drag(ui: &mut egui::Ui, beats: &mut f64, max: f64, bpb: f64) -> bool {
    let mut v = *beats;
    let r = ui.add(
        egui::DragValue::new(&mut v)
            .range(0.0..=max.max(0.0))
            .speed(0.05)
            .custom_formatter(move |b, _| tessera_engine::time::format_position(b, bpb as u32))
            .custom_parser(move |s| parse_position(s, bpb)),
    );
    let v = (v / STEP).round() * STEP;
    if r.changed() && (v - *beats).abs() > 1e-9 {
        *beats = v.clamp(0.0, max.max(0.0));
        return true;
    }
    false
}

/// Parses `bar.beat.sixteenth` (1-based; trailing parts optional).
fn parse_position(s: &str, bpb: f64) -> Option<f64> {
    let parts: Vec<f64> = s
        .trim()
        .split('.')
        .map(|p| p.trim().parse::<f64>().ok())
        .collect::<Option<_>>()?;
    let bar = parts.first().copied()?.max(1.0) - 1.0;
    let beat = parts.get(1).copied().unwrap_or(1.0).max(1.0) - 1.0;
    let six = parts.get(2).copied().unwrap_or(1.0).max(1.0) - 1.0;
    Some(bar * bpb + beat + six * 0.25)
}

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let bpb = app.project.beats_per_bar as f64;
    let global = app.project.launch_quantization;
    let Some(clip) = app.clip_mut(r) else { return };
    let mut changed = false;

    heading(ui, "MARKERS");
    egui::Grid::new("clip_markers")
        .num_columns(2)
        .spacing(vec2(6.0, 4.0))
        .show(ui, |ui| {
            let end = clip.length;
            ui.label("Start").on_hover_text("Where playback starts");
            changed |= position_drag(ui, &mut clip.start_marker, end - STEP, bpb);
            ui.end_row();
            if clip.looping {
                ui.label("Loop")
                    .on_hover_text("Where the loop starts; it ends at the clip end");
                changed |= position_drag(ui, &mut clip.loop_start, end - STEP, bpb);
                ui.end_row();
            }
            ui.label(if clip.looping { "Loop End" } else { "End" });
            let mut e = clip.length;
            if position_drag(ui, &mut e, 1024.0, bpb) {
                clip.length = e.max(clip.loop_start.max(clip.start_marker) + STEP);
                changed = true;
            }
            ui.end_row();
        });

    if let ClipRef::Slot { .. } = r {
        let launch = &mut clip.launch;
        heading(ui, "LAUNCH");
        egui::Grid::new("clip_launch")
            .num_columns(2)
            .spacing(vec2(6.0, 4.0))
            .show(ui, |ui| {
                ui.label("Mode");
                egui::ComboBox::from_id_salt("launch_mode")
                    .width(84.0)
                    .selected_text(launch.mode.label())
                    .show_ui(ui, |ui| {
                        for m in LaunchMode::ALL {
                            let tip = match m {
                                LaunchMode::Trigger => "Click starts the clip",
                                LaunchMode::Gate => "Plays while the button is held",
                                LaunchMode::Toggle => "Click starts, click again stops",
                                LaunchMode::Repeat => "Restarts at the quantization while held",
                            };
                            changed |= ui
                                .selectable_value(&mut launch.mode, m, m.label())
                                .on_hover_text(tip)
                                .changed();
                        }
                    });
                ui.end_row();
                ui.label("Quantize");
                let text = match launch.quantization {
                    None => format!("Global ({})", global.label()),
                    Some(q) => q.label().to_string(),
                };
                egui::ComboBox::from_id_salt("launch_q")
                    .width(84.0)
                    .selected_text(text)
                    .show_ui(ui, |ui| {
                        changed |= ui.selectable_value(&mut launch.quantization, None, "Global").changed();
                        for q in Quantization::ALL {
                            changed |= ui
                                .selectable_value(&mut launch.quantization, Some(q), q.label())
                                .changed();
                        }
                    });
                ui.end_row();
                ui.label("Legato");
                changed |= ui
                    .checkbox(&mut launch.legato, "")
                    .on_hover_text("Start from the position of the clip it replaces")
                    .changed();
                ui.end_row();
            });

        let f = &mut launch.follow;
        ui.add_space(4.0);
        changed |= ui
            .checkbox(&mut f.enabled, RichText::new("Follow Action").small().strong())
            .on_hover_text("After the follow time, do action A or B")
            .changed();
        ui.add_enabled_ui(f.enabled, |ui| {
            egui::Grid::new("clip_follow")
                .num_columns(2)
                .spacing(vec2(6.0, 4.0))
                .show(ui, |ui| {
                    for (label, kind, salt) in [("A", &mut f.a, "follow_a"), ("B", &mut f.b, "follow_b")] {
                        ui.label(label);
                        egui::ComboBox::from_id_salt(salt)
                            .width(84.0)
                            .selected_text(kind.label())
                            .show_ui(ui, |ui| {
                                for k in FollowKind::ALL {
                                    changed |= ui.selectable_value(kind, k, k.label()).changed();
                                }
                            });
                        ui.end_row();
                    }
                    ui.label("Chance A");
                    let mut pct = f.chance_a * 100.0;
                    if ui
                        .add(egui::Slider::new(&mut pct, 0.0..=100.0).suffix(" %").show_value(true))
                        .on_hover_text("How often action A is chosen; B gets the rest")
                        .changed()
                    {
                        f.chance_a = (pct / 100.0).clamp(0.0, 1.0);
                        changed = true;
                    }
                    ui.end_row();
                    ui.label("Linked");
                    changed |= ui
                        .checkbox(&mut f.linked, "")
                        .on_hover_text("Use the clip length as the follow time")
                        .changed();
                    ui.end_row();
                    if !f.linked {
                        ui.label("Time");
                        let mut bars = f.time / bpb;
                        if ui
                            .add(
                                egui::DragValue::new(&mut bars)
                                    .range(0.0625..=256.0)
                                    .speed(0.02)
                                    .max_decimals(2)
                                    .suffix(" bars"),
                            )
                            .changed()
                        {
                            f.time = ((bars * bpb) / STEP).round().max(1.0) * STEP;
                            changed = true;
                        }
                        ui.end_row();
                    }
                });
        });
    }

    if changed {
        clip.loop_start = clip.loop_start.min(clip.length - STEP).max(0.0);
        clip.start_marker = clip.start_marker.min(clip.length - STEP).max(0.0);
        app.changed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_parse_like_the_display() {
        assert_eq!(parse_position("1.1.1", 4.0), Some(0.0));
        assert_eq!(parse_position("2.2.2", 4.0), Some(5.25));
        assert_eq!(parse_position("3", 4.0), Some(8.0));
        assert_eq!(parse_position("x", 4.0), None);
    }
}
