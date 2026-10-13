//! Colours and global style.

use eframe::egui::{self, Color32};

pub const BG_BAR: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
pub const BG_PANEL: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
pub const BG_VIEW: Color32 = Color32::from_rgb(0x24, 0x24, 0x24);
pub const BG_SLOT: Color32 = Color32::from_rgb(0x3a, 0x3a, 0x3a);
pub const BG_LANE: Color32 = Color32::from_rgb(0x2e, 0x2e, 0x2e);
pub const BG_LANE_ALT: Color32 = Color32::from_rgb(0x2a, 0x2a, 0x2a);
pub const GRID: Color32 = Color32::from_rgb(0x3c, 0x3c, 0x3c);
pub const GRID_BAR: Color32 = Color32::from_rgb(0x55, 0x55, 0x55);
pub const TEXT: Color32 = Color32::from_rgb(0xdd, 0xdd, 0xdd);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x99, 0x99, 0x99);
pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0x9c, 0x2b);
pub const PLAY: Color32 = Color32::from_rgb(0x4c, 0xd9, 0x64);
pub const SOLO: Color32 = Color32::from_rgb(0x4d, 0x9c, 0xff);
pub const ARM: Color32 = Color32::from_rgb(0xff, 0x4d, 0x4d);
pub const ACTIVE: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x3c);
pub const SELECT: Color32 = Color32::from_rgb(0xff, 0xff, 0xff);
pub const METER_LOW: Color32 = Color32::from_rgb(0x3c, 0xd2, 0x5a);
pub const METER_MID: Color32 = Color32::from_rgb(0xf0, 0xd0, 0x3c);
pub const METER_HIGH: Color32 = Color32::from_rgb(0xff, 0x40, 0x40);

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A darker variant of a clip colour for backgrounds.
pub fn dim(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

pub fn panel_frame(fill: Color32) -> egui::Frame {
    egui::Frame::new().fill(fill)
}

pub fn apply(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        style.spacing.interact_size.y = 20.0;
        let v = &mut style.visuals;
        v.panel_fill = BG_PANEL;
        v.window_fill = BG_PANEL;
        v.extreme_bg_color = Color32::from_rgb(0x1c, 0x1c, 0x1c);
        v.selection.bg_fill = dim(ACCENT, 0.6);
        v.selection.stroke = egui::Stroke::new(1.0, ACCENT);
        v.widgets.noninteractive.fg_stroke.color = TEXT;
        v.widgets.inactive.bg_fill = Color32::from_rgb(0x45, 0x45, 0x45);
        v.widgets.inactive.weak_bg_fill = Color32::from_rgb(0x45, 0x45, 0x45);
        v.widgets.hovered.bg_fill = Color32::from_rgb(0x55, 0x55, 0x55);
        v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x55, 0x55, 0x55);
        v.widgets.active.bg_fill = dim(ACCENT, 0.8);
    });
}
