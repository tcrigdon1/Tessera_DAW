//! Custom panels for the loop and lo-fi effects: the Looper's transport
//! buttons and loop position. (Beat Repeat, Redux, Erosion and Vinyl
//! Distortion use the generic knob layout.)

use eframe::egui::{self, pos2, vec2, Rect, Sense};
use tessera_engine::fx::looper::{self, LooperParams};
use tessera_engine::fx::DeviceParams;

use super::super::{theme, widgets};
use super::PanelCtx;

pub fn body(ui: &mut egui::Ui, ctx: &PanelCtx, params: &DeviceParams, out: &mut Vec<(usize, f32)>) -> bool {
    match params {
        DeviceParams::Looper(p) => {
            looper_body(ui, ctx, params, p, out);
            true
        }
        _ => false,
    }
}

fn looper_body(
    ui: &mut egui::Ui,
    ctx: &PanelCtx,
    params: &DeviceParams,
    p: &LooperParams,
    out: &mut Vec<(usize, f32)>,
) {
    // Clear is a trigger: the button sets it to 1, and the next frame puts it
    // back to 0 so the following press is a new 0 -> 1 edge.
    if p.clear >= 0.5 {
        out.push((looper::CLEAR, 0.0));
    }
    // The buttons show what the runtime is doing, which moves on by itself
    // (fixed length, Clear, Song Control); the State parameter is brought
    // along so the next press is a change again.
    let status = looper::status(ctx.id);
    if let Some(v) = status.and_then(|s| s.resync(p.state)) {
        out.push((looper::STATE, v));
    }
    let state = status.map_or(p.state.round(), |s| s.state);
    let pending = status.and_then(|s| s.pending);
    if status.is_some() {
        ui.ctx().request_repaint();
    }
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
            let w = 74.0;
            let buttons: [(&str, f32, egui::Color32); 4] = [
                ("Record", looper::STATE_RECORD, theme::ARM),
                ("Overdub", looper::STATE_OVERDUB, theme::ACCENT),
                ("Play", looper::STATE_PLAY, theme::PLAY),
                ("Stop", looper::STATE_STOP, theme::SOLO),
            ];
            // A request waiting for its quantization point blinks.
            let blink = (ui.input(|i| i.time) * 4.0) as i64 % 2 == 0;
            for (label, value, color) in buttons {
                let on = state == value || (pending == Some(value) && blink);
                if widgets::toggle(ui, on, label, color, w).clicked() && state != value {
                    out.push((looper::STATE, value));
                }
            }
            if widgets::toggle(ui, p.clear >= 0.5, "Clear", theme::ACTIVE, w).clicked() {
                out.push((looper::CLEAR, 1.0));
            }
            if let Some(s) = status {
                loop_position(ui, &s, w);
            }
        });
        ui.add_space(6.0);
        super::knob_columns(ui, ctx, params, &[1, 2, 3, 4, 5, 6, 7, 8, 9], out);
    });
}

/// Loop position bar with the length (growing while recording).
fn loop_position(ui: &mut egui::Ui, s: &looper::LooperStatus, w: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(w, 18.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::gray(0x1e));
    let recording = s.state == looper::STATE_RECORD;
    let frac = if recording {
        1.0
    } else if s.length > 0.0 {
        (s.position / s.length).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if frac > 0.0 {
        let color = if recording { theme::ARM } else { theme::PLAY };
        let bar = Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * frac, rect.bottom()));
        painter.rect_filled(bar, 2.0, theme::dim(color, 0.6));
    }
    let text = if s.length > 0.0 {
        format!("{:.1} s", s.length)
    } else {
        "Empty".to_string()
    };
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(10.0),
        theme::TEXT,
    );
}
