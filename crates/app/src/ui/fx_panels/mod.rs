//! Custom Device view panels for effects whose controls need more than a
//! row of knobs (displays, meters, transport buttons). Effects without a
//! custom panel use the generic knob layout in `devices.rs`.
//!
//! Each group module returns `true` when it drew the device, pushing
//! `(parameter index, new value)` pairs for every change.

mod analysis;
mod dynamics;
mod lofi;
mod modulation;

use eframe::egui;
use tessera_engine::fx::DeviceParams;
use tessera_engine::model::Id;

/// What a panel knows besides the parameters.
#[allow(dead_code)]
pub struct PanelCtx {
    pub id: Id,
    /// Gain reduction in dB, for devices that publish one.
    pub meter: Option<f32>,
    pub sample_rate: f32,
}

/// Draws a custom panel for `params`, if the effect has one.
pub fn body(ui: &mut egui::Ui, ctx: &PanelCtx, params: &DeviceParams, out: &mut Vec<(usize, f32)>) -> bool {
    modulation::body(ui, ctx, params, out)
        || dynamics::body(ui, ctx, params, out)
        || lofi::body(ui, ctx, params, out)
        || analysis::body(ui, ctx, params, out)
}

/// Knobs for `indices` laid out in columns of two (the generic layout, for
/// panels that mix custom displays with plain knobs).
#[allow(dead_code)]
pub fn knob_columns(
    ui: &mut egui::Ui,
    ctx: &PanelCtx,
    params: &DeviceParams,
    indices: &[usize],
    out: &mut Vec<(usize, f32)>,
) {
    let info = params.info();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(2.0, 2.0);
        for col in indices.chunks(2) {
            ui.vertical(|ui| {
                for &i in col {
                    if let Some(info) = info.get(i) {
                        if let Some(v) = super::devices::param_control(ui, (ctx.id, i), info, params.get(i)) {
                            out.push((i, v));
                        }
                    }
                }
            });
        }
    });
}
