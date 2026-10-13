//! Custom panels for the loop and lo-fi effects: the Looper's transport
//! buttons. (Beat Repeat, Redux, Erosion and Vinyl Distortion use the
//! generic knob layout.)

use eframe::egui;
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
    let state = p.state.round();
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
            for (label, value, color) in buttons {
                let on = state == value;
                if widgets::toggle(ui, on, label, color, w).clicked() && !on {
                    out.push((looper::STATE, value));
                }
            }
            if widgets::toggle(ui, p.clear >= 0.5, "Clear", theme::ACTIVE, w).clicked() {
                out.push((looper::CLEAR, 1.0));
            }
        });
        ui.add_space(6.0);
        super::knob_columns(ui, ctx, params, &[1, 2, 3, 4, 5, 6, 7, 8, 9], out);
    });
}
