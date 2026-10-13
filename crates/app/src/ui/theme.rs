//! Colours and global style.
//!
//! The UI colours come from the current [`Theme`] (chosen under
//! Preferences → Look/Feel). Draw code calls the functions below
//! (`theme::text()`, `theme::bg_view()`, `theme::gray(0x48)`, ...); the
//! upper-case constants are the Dark theme's values for accent colours that
//! are the same in every theme.

use std::sync::atomic::{AtomicU8, Ordering};

use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};

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

/// The colour themes offered in Preferences → Look/Feel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    Dark,
    MidGrey,
    Light,
    HighContrast,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::Dark, Theme::MidGrey, Theme::Light, Theme::HighContrast];

    pub fn label(self) -> &'static str {
        match self {
            Theme::Dark => "Dark",
            Theme::MidGrey => "Mid Grey",
            Theme::Light => "Light",
            Theme::HighContrast => "Dark High Contrast",
        }
    }

    fn index(self) -> u8 {
        match self {
            Theme::Dark => 0,
            Theme::MidGrey => 1,
            Theme::Light => 2,
            Theme::HighContrast => 3,
        }
    }

    fn from_index(i: u8) -> Theme {
        match i {
            1 => Theme::MidGrey,
            2 => Theme::Light,
            3 => Theme::HighContrast,
            _ => Theme::Dark,
        }
    }

    pub fn is_light(self) -> bool {
        self == Theme::Light
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The theme in use.
pub fn current() -> Theme {
    Theme::from_index(CURRENT.load(Ordering::Relaxed))
}

/// The greys of a theme, as a mapping from the Dark theme's grey levels.
/// Every UI grey is written as its Dark level (0x00..=0xff) and mapped here,
/// so the whole interface follows the theme.
fn map_gray(theme: Theme, level: u8) -> u8 {
    let l = level as f32;
    let v = match theme {
        Theme::Dark => l,
        // Lifted a little: panels around 0x50.
        Theme::MidGrey => 0x22 as f32 + l * 0.98,
        // Inverted around the panel greys: dark backgrounds become light,
        // light text becomes dark.
        Theme::Light => {
            if l <= 0x70 as f32 {
                // Backgrounds 0x1c..0x70 -> 0xf2..0xa8.
                0xf2 as f32 - (l - 0x1c as f32).max(0.0) * 0.87
            } else {
                // Text 0x99..0xff -> 0x60..0x10.
                0x60 as f32 - (l - 0x99 as f32).max(0.0) * 0.5
            }
        }
        // Blacker backgrounds, brighter text.
        Theme::HighContrast => {
            if l <= 0x70 as f32 {
                l * 0.7
            } else {
                (l * 1.25).min(255.0)
            }
        }
    };
    v.round().clamp(0.0, 255.0) as u8
}

/// A grey given as its Dark-theme level, in the current theme.
pub fn gray(level: u8) -> Color32 {
    let g = map_gray(current(), level);
    Color32::from_rgb(g, g, g)
}

fn themed(c: Color32) -> Color32 {
    // Theme colours are greys in the Dark theme.
    gray(c.r())
}

pub fn bg_bar() -> Color32 {
    themed(BG_BAR)
}
pub fn bg_panel() -> Color32 {
    themed(BG_PANEL)
}
pub fn bg_view() -> Color32 {
    themed(BG_VIEW)
}
pub fn bg_slot() -> Color32 {
    themed(BG_SLOT)
}
pub fn bg_lane() -> Color32 {
    themed(BG_LANE)
}
pub fn bg_lane_alt() -> Color32 {
    themed(BG_LANE_ALT)
}
pub fn grid() -> Color32 {
    themed(GRID)
}
pub fn grid_bar() -> Color32 {
    themed(GRID_BAR)
}
pub fn text() -> Color32 {
    themed(TEXT)
}
pub fn text_dim() -> Color32 {
    themed(TEXT_DIM)
}
pub fn select() -> Color32 {
    if current().is_light() {
        Color32::from_rgb(0x10, 0x10, 0x10)
    } else {
        SELECT
    }
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A darker variant of a clip colour for backgrounds (lighter in the Light
/// theme, so clips stay readable on light lanes).
pub fn dim(c: Color32, f: f32) -> Color32 {
    if current().is_light() {
        let mix = |v: u8| (v as f32 + (255.0 - v as f32) * (1.0 - f) * 0.8) as u8;
        return Color32::from_rgb(mix(c.r()), mix(c.g()), mix(c.b()));
    }
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

pub fn panel_frame(fill: Color32) -> egui::Frame {
    egui::Frame::new().fill(fill)
}

/// Switches the whole UI to `theme`.
pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    CURRENT.store(theme.index(), Ordering::Relaxed);
    ctx.set_theme(if theme.is_light() {
        egui::Theme::Light
    } else {
        egui::Theme::Dark
    });
    let light = theme.is_light();
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(6.0, 4.0);
        style.spacing.button_padding = egui::vec2(6.0, 2.0);
        style.spacing.interact_size.y = 20.0;
        let v = &mut style.visuals;
        v.panel_fill = bg_panel();
        v.window_fill = bg_panel();
        v.extreme_bg_color = gray(0x1c);
        v.faint_bg_color = gray(0x30);
        v.selection.bg_fill = dim(ACCENT, 0.6);
        v.selection.stroke = egui::Stroke::new(1.0, if light { Color32::BLACK } else { ACCENT });
        v.widgets.noninteractive.fg_stroke.color = text();
        v.widgets.noninteractive.bg_fill = bg_panel();
        v.widgets.noninteractive.weak_bg_fill = bg_panel();
        v.widgets.noninteractive.bg_stroke.color = gray(0x50);
        v.widgets.inactive.bg_fill = gray(0x45);
        v.widgets.inactive.weak_bg_fill = gray(0x45);
        v.widgets.inactive.fg_stroke.color = text();
        v.widgets.hovered.bg_fill = gray(0x55);
        v.widgets.hovered.weak_bg_fill = gray(0x55);
        v.widgets.hovered.fg_stroke.color = text();
        v.widgets.active.bg_fill = dim(ACCENT, 0.8);
        v.widgets.active.fg_stroke.color = text();
        v.widgets.open.weak_bg_fill = gray(0x50);
        v.override_text_color = None;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_theme_is_identity() {
        for l in [0u8, 0x1c, 0x48, 0x99, 0xdd, 0xff] {
            assert_eq!(map_gray(Theme::Dark, l), l);
        }
    }

    #[test]
    fn light_theme_inverts_contrast() {
        // Backgrounds become light and text dark, keeping text readable.
        let bg = map_gray(Theme::Light, BG_VIEW.r());
        let fg = map_gray(Theme::Light, TEXT.r());
        let dim = map_gray(Theme::Light, TEXT_DIM.r());
        assert!(bg > 0xd0, "background {bg:#x}");
        assert!(fg < 0x50, "text {fg:#x}");
        assert!(dim < bg && dim > fg);
        // Lighter dark-theme panels stay ordered (darker in Light).
        assert!(map_gray(Theme::Light, 0x55) < map_gray(Theme::Light, 0x2a));
    }
}
