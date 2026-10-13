//! Status bar at the bottom of the window: messages on the left; CPU and
//! disk meters, computer keyboard octave, audio device and the Info View
//! toggle on the right.

use eframe::egui::{self, pos2, vec2, Color32, Rect, RichText, Sense};

use super::info_view::InfoExt;
use super::{theme, widgets};
use crate::app::TesseraApp;

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    egui::Panel::bottom("status")
        .frame(theme::panel_frame(theme::bg_bar()).inner_margin(egui::Margin::symmetric(8, 2)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&app.status).color(theme::text_dim()).small())
                    .info(
                        "Status",
                        "Messages about what Tessera just did: files opened and saved, imports, errors.",
                    );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    right_side(app, ui);
                });
            });
        });
}

fn dim_text(text: impl Into<String>) -> RichText {
    RichText::new(text).color(theme::text_dim()).small()
}

/// Right-to-left: Info toggle, audio device, octave, disk, CPU.
fn right_side(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let on = app.chrome.show_info;
    if widgets::toggle(ui, on, "Info", theme::ACCENT, 34.0)
        .info(
            "Info View",
            "Shows or hides the Info View in the bottom-left corner, which describes whatever the mouse is over. Shortcut: Shift + ?.",
        )
        .clicked()
    {
        app.chrome.show_info = !on;
    }
    ui.separator();

    let out = app.audio_output();
    let rate = out.sample_rate;
    let device = format!(
        "{} · {:.1} kHz · In: {}",
        out.device_name,
        rate as f32 / 1000.0,
        out.input_name.as_deref().unwrap_or("none")
    );
    ui.label(dim_text(device)).info(
        "Audio Device",
        "The audio output in use, its sample rate and the audio input. Tessera opens the system's default devices with their default buffer size; without an output it runs on a silent null device.",
    );
    ui.separator();

    ui.label(dim_text(format!("Octave C{}", app.keyboard_octave - 2))).info(
        "Computer Keyboard Octave",
        "The octave the computer MIDI keyboard plays (A = C). Press Z and X to go down and up.",
    );
    ui.separator();

    // Disk: recorded input waiting to be written as WAV files.
    let bytes = app.takes.pending_bytes();
    let recording = app.takes.is_recording();
    let per_minute = rate as f32 * 2.0 * 4.0 * 60.0;
    let text = if bytes > 0 {
        format!("Disk {:.1} MB", bytes as f32 / 1_048_576.0)
    } else {
        "Disk".to_string()
    };
    let r = ui.label(dim_text(text));
    let bar = small_bar(
        ui,
        (bytes as f32 / per_minute).min(1.0),
        if recording { theme::ARM } else { theme::SOLO },
    );
    r.union(bar).info(
        "Disk",
        "Recorded audio held in memory until its take ends and is written to a WAV file. The bar fills with each minute of stereo audio being recorded. Tessera loads samples into memory, so nothing streams from disk during playback.",
    );
    ui.separator();

    let cpu = app.engine.shared.cpu_load.load().clamp(0.0, 9.99);
    let r = ui.label(dim_text(format!("CPU {:.0}%", cpu * 100.0)));
    let color = if cpu > 0.85 {
        theme::METER_HIGH
    } else if cpu > 0.6 {
        theme::METER_MID
    } else {
        theme::METER_LOW
    };
    let bar = small_bar(ui, cpu.min(1.0), color);
    r.union(bar).info(
        "CPU Load",
        "How much of the time available for each audio block the engine needs. Near 100% the audio starts to drop out: switch off or remove devices and plug-ins.",
    );
}

/// A small horizontal level bar.
fn small_bar(ui: &mut egui::Ui, fraction: f32, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 8.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 1.0, theme::gray(0x1c));
    if fraction > 0.0 {
        let fill = Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * fraction, rect.bottom()));
        p.rect_filled(fill, 1.0, color);
    }
    resp
}
