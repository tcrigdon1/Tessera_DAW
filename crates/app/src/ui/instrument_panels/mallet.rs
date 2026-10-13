//! Device view panel of the Mallet Synth: exciters, one resonator at a time
//! (switchable 1 / 2) with a partial display, and the global section.

use eframe::egui::{self, pos2, vec2, Sense, Stroke};
use tessera_engine::dsp::mallet::{
    is_delay_type, partial_preview, MalletParams, RES_A_FIRST, RES_B_FIRST, RES_PARAMS, RES_TYPES,
};
use tessera_engine::fx::Params;
use tessera_engine::model::{Id, Instrument};

use super::super::detail::device_box;
use super::super::devices::param_control;
use super::super::{theme, widgets};
use super::{edit_instrument, knob_section};
use crate::app::TesseraApp;

/// Generic controls for `indices`, in columns of two.
fn knobs(ui: &mut egui::Ui, salt: Id, p: &mut MalletParams, indices: &[usize]) -> bool {
    let mut changed = false;
    let info = MalletParams::INFO;
    for col in indices.chunks(2) {
        ui.vertical(|ui| {
            for &i in col {
                let Some(inf) = info.get(i) else { continue };
                if let Some(v) = param_control(ui, (salt, i), inf, p.get_param(i)) {
                    p.set_param(i, v);
                    changed = true;
                }
            }
        });
    }
    changed
}

/// The partials of one resonator as lines over a log frequency axis.
fn partial_display(ui: &mut egui::Ui, p: &MalletParams, which: usize, on: bool) {
    let (rect, _) = ui.allocate_exact_size(vec2(150.0, 100.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::BG_VIEW);
    let first = if which == 0 { RES_A_FIRST } else { RES_B_FIRST };
    let kind = (p.get(first + 1).round().max(0.0) as usize).min(RES_TYPES.len() - 1);
    let partials = partial_preview(p, which);
    let max_ratio: f32 = 48.0;
    let x_of = |r: f32| rect.left() + 4.0 + (r.max(1.0).ln() / max_ratio.ln()).min(1.0) * (rect.width() - 8.0);
    let base = rect.bottom() - 14.0;
    let h = rect.height() - 22.0;
    let color = if on { theme::ACCENT } else { theme::TEXT_DIM };
    for (ratio, amp) in partials {
        if ratio > max_ratio {
            continue;
        }
        let x = x_of(ratio);
        let a = amp.clamp(0.0, 1.0);
        painter.line_segment([pos2(x, base), pos2(x, base - a * h)], Stroke::new(1.5, color));
    }
    painter.line_segment(
        [pos2(rect.left() + 2.0, base), pos2(rect.right() - 2.0, base)],
        Stroke::new(1.0, theme::GRID_BAR),
    );
    let model = if is_delay_type(kind) { "delay line" } else { "modal" };
    painter.text(
        pos2(rect.left() + 4.0, rect.bottom() - 6.0),
        egui::Align2::LEFT_CENTER,
        format!("{} · {model}", RES_TYPES[kind]),
        egui::FontId::proportional(9.5),
        theme::TEXT_DIM,
    );
}

/// A tiny ADSR curve of the noise exciter.
fn noise_env_display(ui: &mut egui::Ui, p: &MalletParams) {
    let (rect, _) = ui.allocate_exact_size(vec2(70.0, 40.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::BG_VIEW);
    let r = rect.shrink(3.0);
    let total = p.noise_attack + p.noise_decay + p.noise_release + 200.0;
    let w = |t: f32| (t / total).sqrt() * r.width() * 0.5;
    let x0 = r.left();
    let xa = x0 + w(p.noise_attack);
    let xd = xa + w(p.noise_decay);
    let xs = (xd + r.width() * 0.15).min(r.right());
    let xr = (xs + w(p.noise_release)).min(r.right());
    let y = |v: f32| r.bottom() - v.clamp(0.0, 1.0) * r.height();
    let color = if p.noise_on >= 0.5 {
        theme::ACCENT
    } else {
        theme::TEXT_DIM
    };
    let pts = vec![
        pos2(x0, y(0.0)),
        pos2(xa, y(1.0)),
        pos2(xd, y(p.noise_sustain)),
        pos2(xs, y(p.noise_sustain)),
        pos2(xr, y(0.0)),
    ];
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, color)));
}

pub fn device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let sel_id = egui::Id::new(("mallet_res", track));
    let mut which: usize = ui.data(|d| d.get_temp(sel_id)).unwrap_or(0);
    edit_instrument(app, track, |_app, inst| {
        let Instrument::Mallet(p) = inst else { return false };
        let mut changed = false;
        changed |= knob_section(ui, track, "MALLET", p, &[0, 1, 2, 3, 4, 5, 6]);
        device_box(ui, "NOISE", |ui| {
            ui.vertical(|ui| {
                changed |= knobs_inline(ui, track, p, &[7, 8]);
                noise_env_display(ui, p);
            });
            changed |= knobs(ui, track, p, &[9, 10, 11, 12, 13, 14, 15, 16]);
        });
        let title = if which == 0 { "RESONATOR 1" } else { "RESONATOR 2" };
        device_box(ui, title, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    for (i, label) in ["1", "2"].iter().enumerate() {
                        if widgets::toggle(ui, which == i, label, theme::SOLO, 24.0)
                            .on_hover_text("Show this resonator")
                            .clicked()
                        {
                            which = i;
                        }
                    }
                });
                let first = if which == 0 { RES_A_FIRST } else { RES_B_FIRST };
                let on = p.get(first) >= 0.5;
                partial_display(ui, p, which, on);
            });
            let first = if which == 0 { RES_A_FIRST } else { RES_B_FIRST };
            let idx: Vec<usize> = (first..first + RES_PARAMS).collect();
            changed |= knobs(ui, track, p, &idx);
        });
        // Structure, LFO, release, transpose, voices and volume.
        let global: Vec<usize> = (RES_B_FIRST + RES_PARAMS..MalletParams::COUNT).collect();
        changed |= knob_section(ui, track, "GLOBAL", p, &global);
        changed
    });
    ui.data_mut(|d| d.insert_temp(sel_id, which));
}

/// Two controls side by side in one row.
fn knobs_inline(ui: &mut egui::Ui, salt: Id, p: &mut MalletParams, indices: &[usize]) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        for &i in indices {
            let Some(inf) = MalletParams::INFO.get(i) else { continue };
            if let Some(v) = param_control(ui, (salt, i), inf, p.get_param(i)) {
                p.set_param(i, v);
                changed = true;
            }
        }
    });
    changed
}
