//! Custom effect panels (none yet).

use eframe::egui;
use tessera_engine::fx::DeviceParams;

use super::PanelCtx;

pub fn body(_ui: &mut egui::Ui, _ctx: &PanelCtx, _params: &DeviceParams, _out: &mut Vec<(usize, f32)>) -> bool {
    false
}
