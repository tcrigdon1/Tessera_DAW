//! User interface.

pub mod arrangement;
pub mod browser;
pub mod chrome;
pub mod clip_envelopes;
pub mod clip_launch;
pub mod detail;
pub mod device_menu;
pub mod devices;
pub mod envelope;
pub mod export_dialog;
pub mod file_dialogs;
pub mod fx_panels;
pub mod info_view;
pub mod instrument_panels;
pub mod key_commands;
pub mod keyboard;
pub mod midi_devices;
pub mod midi_prefs;
pub mod overview;
pub mod piano_roll;
pub mod plugin_panel;
pub mod prefs;
pub mod racks;
pub mod sample_panels;
pub mod second_window;
pub mod session;
pub mod status_bar;
pub mod synth_panels;
pub mod theme;
pub mod transport;
pub mod view_menu;
pub mod warp;
pub mod widgets;
pub mod workflow_windows;

use eframe::egui;

use crate::app::{TesseraApp, View};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    chrome::begin_frame(app, ui.ctx());
    egui::Panel::top("transport")
        .frame(theme::panel_frame(theme::bg_bar()).inner_margin(egui::Margin::symmetric(8, 5)))
        .show(ui, |ui| transport::draw(app, ui));

    if app.prefs.view.status_bar {
        status_bar::draw(app, ui);
    }

    if app.prefs.view.detail {
        egui::Panel::bottom("detail")
            .resizable(true)
            .default_size(270.0)
            .min_size(120.0)
            .frame(theme::panel_frame(theme::bg_panel()).inner_margin(egui::Margin::same(6)))
            .show(ui, |ui| {
                info_view::panel(app, ui);
                detail::draw(app, ui)
            });
    } else if app.chrome.show_info {
        // The Info View stays reachable with the Detail View hidden.
        egui::Panel::bottom("info_only")
            .resizable(false)
            .exact_size(120.0)
            .frame(theme::panel_frame(theme::bg_panel()).inner_margin(egui::Margin::same(6)))
            .show(ui, |ui| info_view::panel(app, ui));
    }

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
            let view = app.view;
            main_view(app, ui, view)
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
    piano_roll::quantize_window(app, &ctx);
    devices::windows(app, &ctx);
    file_dialogs::draw(app, &ctx);

    midi_prefs::draw(app, &ctx);
    chrome::end_frame(app, &ctx);
    workflow_windows::draw(app, &ctx);
    second_window::draw(app, &ctx);
}

/// The Session or Arrangement view filling `ui`, accepting browser drops.
pub fn main_view(app: &mut TesseraApp, ui: &mut egui::Ui, view: View) {
    let rect = ui.max_rect();
    match view {
        View::Session => session::draw(app, ui),
        View::Arrangement => arrangement::draw(app, ui),
    }
    // Browser items dropped on empty space make a new track.
    browser::drop_on_empty(app, ui, rect);
}
