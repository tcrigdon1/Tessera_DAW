//! Device view panel of the Electric Piano.

use eframe::egui::{self, pos2, vec2, Color32, Sense, Stroke};
use tessera_engine::dsp::electric::{idx, ElectricParams};
use tessera_engine::model::{Id, Instrument};

use super::analog::{none, plot_fn, section, DISPLAY_H};
use super::edit_instrument;
use crate::app::TesseraApp;
use crate::ui::theme;

/// The pickup transfer curve as the engine applies it (normalised).
fn pickup(p: &ElectricParams, x: f32) -> f32 {
    let dist = p.p_distance.clamp(0.0, 1.0);
    let k = 1.7 - 1.35 * dist;
    let sym = p.p_symmetry.clamp(-1.0, 1.0);
    if p.p_type < 0.5 {
        let s = sym * 0.5;
        let f = |u: f32| u / (1.0 + u * u);
        let slope = (1.0 - s * s) / ((1.0 + s * s) * (1.0 + s * s));
        (f(x * k + s) - f(s)) / (k.max(0.9) * slope.max(0.3))
    } else {
        let s = sym * 0.4;
        let g = |u: f32| {
            let u = u.clamp(-1.6, 1.6);
            u / (1.0 - 0.4 * u)
        };
        let slope = 1.0 / ((1.0 - 0.4 * s) * (1.0 - 0.4 * s));
        (g(x * k + s) - g(s)) / (k.max(0.9) * slope.max(0.3))
    }
}

/// Bars for the tine partials (height = level, length = decay).
fn partials(ui: &mut egui::Ui, p: &ElectricParams, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, DISPLAY_H), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, Color32::from_rgb(0x1c, 0x1c, 0x1c));
    let inner = rect.shrink2(vec2(6.0, 14.0));
    let ratios: [f32; 3] = if p.p_type < 0.5 {
        [1.0, 7.1, 19.6]
    } else {
        [1.0, 6.27, 17.55]
    };
    let color = p.t_color.clamp(0.0, 1.0);
    let levels = [
        1.0,
        0.06 + 0.45 * color,
        0.015 + 0.2 * color,
        p.tb_level.clamp(0.0, 1.0) * 0.6,
    ];
    let decays = [
        p.t_decay,
        p.t_decay * ratios[1].powf(-0.7),
        p.t_decay * ratios[2].powf(-0.7),
        p.tb_decay,
    ];
    let max_decay = 20f32.ln_1p();
    let names = ["1", format_ratio(ratios[1]), format_ratio(ratios[2]), "bar"];
    let row_h = inner.height() / 4.0;
    for i in 0..4 {
        let y = inner.top() + row_h * (i as f32 + 0.5);
        let len = decays[i].max(0.0).ln_1p() / max_decay * (inner.width() - 18.0);
        let thick = 1.0 + levels[i].sqrt() * (row_h - 3.0);
        let c = if i == 3 {
            theme::dim(theme::ACCENT, 0.6)
        } else {
            theme::ACCENT
        };
        painter.line_segment(
            [
                pos2(inner.left() + 18.0, y),
                pos2(inner.left() + 18.0 + len.max(1.0), y),
            ],
            Stroke::new(thick, c),
        );
        painter.text(
            pos2(inner.left(), y),
            egui::Align2::LEFT_CENTER,
            names[i],
            egui::FontId::proportional(9.0),
            theme::TEXT_DIM,
        );
    }
    painter.text(
        pos2(rect.left() + 4.0, rect.top() + 3.0),
        egui::Align2::LEFT_TOP,
        "Partials",
        egui::FontId::proportional(9.5),
        theme::TEXT_DIM,
    );
}

fn format_ratio(r: f32) -> &'static str {
    if r > 19.0 {
        "19.6"
    } else if r > 17.0 {
        "17.6"
    } else if r > 7.0 {
        "7.1"
    } else {
        "6.3"
    }
}

pub fn device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    ui.scope(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        edit_instrument(app, track, |_app, inst| {
            let Instrument::Electric(p) = inst else { return false };
            panel(ui, track, p)
        });
    });
}

fn panel(ui: &mut egui::Ui, track: Id, p: &mut ElectricParams) -> bool {
    let mut changed = false;
    changed |= section(
        ui,
        track,
        "MALLET",
        p,
        &(idx::STIFFNESS..=idx::NOISE_DECAY).collect::<Vec<_>>(),
        none,
    );
    changed |= section(
        ui,
        track,
        "TINE",
        p,
        &(idx::T_DECAY..=idx::T_KEY).collect::<Vec<_>>(),
        |ui, p| partials(ui, p, 90.0),
    );
    changed |= section(ui, track, "TONE BAR", p, &[idx::TB_DECAY, idx::TB_LEVEL], none);
    changed |= section(
        ui,
        track,
        "DAMPER",
        p,
        &[idx::D_TONE, idx::D_TONE + 1, idx::D_NOISE, idx::VOLUME + 5],
        none,
    );
    changed |= section(
        ui,
        track,
        "PICKUP",
        p,
        &(idx::P_TYPE..=idx::P_DISTANCE).collect::<Vec<_>>(),
        |ui, p| {
            let pc = *p;
            plot_fn(ui, 90.0, "Pickup curve", |t| pickup(&pc, (t * 2.0 - 1.0) * 1.2) * 0.8);
        },
    );
    changed |= section(
        ui,
        track,
        "GLOBAL",
        p,
        &[
            idx::VOLUME,
            idx::VOLUME + 1,
            idx::VOLUME + 2,
            idx::VOLUME + 3,
            idx::VOLUME + 4,
        ],
        none,
    );
    changed |= section(ui, track, "TREMOLO", p, &[idx::TREM_DEPTH - 1, idx::TREM_DEPTH], none);
    changed
}
