//! User interface.

pub mod arrangement;
pub mod browser;
pub mod clip_envelopes;
pub mod detail;
pub mod devices;
pub mod envelope;
pub mod keyboard;
pub mod sample_panels;
pub mod session;
pub mod synth_panels;
pub mod theme;
pub mod transport;
pub mod warp;
pub mod widgets;

use eframe::egui;

use crate::app::{TesseraApp, View};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    egui::Panel::top("transport")
        .frame(theme::panel_frame(theme::BG_BAR).inner_margin(egui::Margin::symmetric(8, 5)))
        .show(ui, |ui| transport::draw(app, ui));

    egui::Panel::bottom("status")
        .frame(theme::panel_frame(theme::BG_BAR).inner_margin(egui::Margin::symmetric(8, 2)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&app.status).color(theme::TEXT_DIM).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cpu = app.engine.shared.cpu_load.load() * 100.0;
                    ui.label(
                        egui::RichText::new(format!("CPU {cpu:.0}%"))
                            .color(theme::TEXT_DIM)
                            .small(),
                    );
                    ui.label(
                        egui::RichText::new(format!("Octave C{}", app.keyboard_octave - 2))
                            .color(theme::TEXT_DIM)
                            .small(),
                    );
                });
            });
        });

    egui::Panel::bottom("detail")
        .resizable(true)
        .default_size(270.0)
        .min_size(120.0)
        .frame(theme::panel_frame(theme::BG_PANEL).inner_margin(egui::Margin::same(6)))
        .show(ui, |ui| detail::draw(app, ui));

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
}
