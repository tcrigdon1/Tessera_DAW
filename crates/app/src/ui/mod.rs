//! User interface.

pub mod arrangement;
pub mod browser;
pub mod chrome;
pub mod clip_envelopes;
pub mod detail;
pub mod devices;
pub mod envelope;
pub mod info_view;
pub mod key_commands;
pub mod keyboard;
pub mod plugin_panel;
pub mod sample_panels;
pub mod session;
pub mod status_bar;
pub mod synth_panels;
pub mod theme;
pub mod transport;
pub mod warp;
pub mod widgets;

use eframe::egui;

use crate::app::{TesseraApp, View};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    chrome::begin_frame(app, ui.ctx());
    egui::Panel::top("transport")
        .frame(theme::panel_frame(theme::BG_BAR).inner_margin(egui::Margin::symmetric(8, 5)))
        .show(ui, |ui| transport::draw(app, ui));

    status_bar::draw(app, ui);

    egui::Panel::bottom("detail")
        .resizable(true)
        .default_size(270.0)
        .min_size(120.0)
        .frame(theme::panel_frame(theme::BG_PANEL).inner_margin(egui::Margin::same(6)))
        .show(ui, |ui| {
            info_view::panel(app, ui);
            detail::draw(app, ui)
        });

    if app.show_browser {
        egui::Panel::left("browser")
            .resizable(true)
            .default_size(220.0)
            .frame(theme::panel_frame(theme::BG_PANEL).inner_margin(egui::Margin::same(6)))
            .show(ui, |ui| browser::draw(app, ui));
    }

    egui::CentralPanel::no_frame()
        .frame(theme::panel_frame(theme::BG_VIEW))
        .show(ui, |ui| {
            let rect = ui.max_rect();
            match app.view {
                View::Session => session::draw(app, ui),
                View::Arrangement => arrangement::draw(app, ui),
            }
            // Browser items dropped on empty space make a new track.
            browser::drop_on_empty(app, ui, rect);
        });
    browser::drag_preview(ui.ctx());
    chrome::end_frame(app, ui.ctx());
}
