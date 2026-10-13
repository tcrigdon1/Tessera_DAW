//! User interface.

pub mod arrangement;
pub mod browser;
pub mod clip_envelopes;
pub mod detail;
pub mod devices;
pub mod envelope;
pub mod export_dialog;
pub mod file_dialogs;
pub mod keyboard;
pub mod plugin_panel;
pub mod prefs;
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
        .frame(theme::panel_frame(theme::bg_bar()).inner_margin(egui::Margin::symmetric(8, 5)))
        .show(ui, |ui| transport::draw(app, ui));

    egui::Panel::bottom("status")
        .frame(theme::panel_frame(theme::bg_bar()).inner_margin(egui::Margin::symmetric(8, 2)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&app.status).color(theme::text_dim()).small());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let cpu = app.engine.shared.cpu_load.load() * 100.0;
                    ui.label(
                        egui::RichText::new(format!("CPU {cpu:.0}%"))
                            .color(theme::text_dim())
                            .small(),
                    );
                    ui.label(
                        egui::RichText::new(format!("Octave C{}", app.keyboard_octave - 2))
                            .color(theme::text_dim())
                            .small(),
                    );
                });
            });
        });

    egui::Panel::bottom("detail")
        .resizable(true)
        .default_size(270.0)
        .min_size(120.0)
        .frame(theme::panel_frame(theme::bg_panel()).inner_margin(egui::Margin::same(6)))
        .show(ui, |ui| detail::draw(app, ui));

    if app.show_browser {
        egui::Panel::left("browser")
            .resizable(true)
            .default_size(220.0)
            .frame(theme::panel_frame(theme::bg_panel()).inner_margin(egui::Margin::same(6)))
            .show(ui, |ui| browser::draw(app, ui));
    }

    egui::CentralPanel::no_frame()
        .frame(theme::panel_frame(theme::bg_view()))
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

    // MIDI files dropped from the desktop become tracks.
    let dropped: Vec<std::path::PathBuf> = ui.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .filter(|p| crate::midi_files::is_midi_file(p))
            .collect()
    });
    for p in dropped {
        app.import_midi(&p);
    }

    let ctx = ui.ctx().clone();
    prefs::draw(app, &ctx);
    export_dialog::draw(app, &ctx);
    file_dialogs::draw(app, &ctx);
}
