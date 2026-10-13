//! Device view panel of the Granular Synth: the sample (drop target, waveform
//! with scan position, spray range, loop region and grain size; click or drag
//! to set the position), then grain, pitch, filter, amp and LFO sections.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui::{self, pos2, vec2, Rect, Sense, Stroke};
use tessera_engine::dsp::granular::{GranularParams, MODES};
use tessera_engine::model::{AudioData, Id, Instrument, SampleRef};

use super::super::browser::{draw_peaks, drop_on};
use super::super::detail::device_box;
use super::super::{theme, widgets};
use super::{edit_instrument, knob_section};
use crate::app::TesseraApp;
use crate::browser::{BrowserItem, DropTarget};

/// What the panel asks the app to do after drawing.
enum Action {
    Load(PathBuf),
    Dialog,
    Clear,
    Forward(BrowserItem),
}

fn sample_data(app: &TesseraApp, track: Id) -> Option<(String, Arc<AudioData>)> {
    let s = app.project.track(track)?.sample(0)?;
    Some((s.name(), s.data.clone()?))
}

/// The waveform with the scan position, spray, loop and grain-size marks.
/// Returns true when the position was changed by clicking or dragging.
fn wave_view(
    ui: &mut egui::Ui,
    p: &mut GranularParams,
    peaks: Option<&[f32]>,
    data: Option<&AudioData>,
    action: &mut Option<Action>,
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(260.0, 78.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::BG_VIEW);
    if let Some(item) = drop_on(ui, rect) {
        *action = Some(match item {
            BrowserItem::Sample(path) => Action::Load(path),
            other => Action::Forward(other),
        });
    }
    let (Some(peaks), Some(data)) = (peaks, data) else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Drop a sample here\nfrom the browser",
            egui::FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
        return false;
    };
    draw_peaks(&painter, rect, peaks, theme::dim(theme::METER_LOW, 0.8));
    let secs = data.duration_secs().max(1e-6) as f32;
    let x_of = |f: f32| rect.left() + f.clamp(0.0, 1.0) * rect.width();
    let pos = p.position.clamp(0.0, 1.0);
    // Loop region.
    if p.mode.round() as usize == 1 {
        let end = pos + p.loop_len;
        let a = Rect::from_min_max(pos2(x_of(pos), rect.top()), pos2(x_of(end), rect.bottom()));
        painter.rect_filled(a, 0.0, theme::dim(theme::SOLO, 0.25));
        if end > 1.0 {
            let b = Rect::from_min_max(pos2(x_of(0.0), rect.top()), pos2(x_of(end - 1.0), rect.bottom()));
            painter.rect_filled(b, 0.0, theme::dim(theme::SOLO, 0.25));
        }
    }
    // Spray range (doubled in Cloud mode).
    let spray_mul = if p.mode.round() as usize == 2 { 2.0 } else { 1.0 };
    let spray = p.spray / 1000.0 * spray_mul / secs;
    if spray > 0.0 {
        let r = Rect::from_min_max(
            pos2(x_of(pos - spray), rect.bottom() - 10.0),
            pos2(x_of(pos + spray), rect.bottom() - 4.0),
        );
        painter.rect_filled(r, 1.0, theme::dim(theme::ACCENT, 0.4));
    }
    // Grain size as a bracket from the position.
    let size = p.size / 1000.0 / secs;
    let g = Rect::from_min_max(
        pos2(x_of(pos), rect.top() + 4.0),
        pos2(x_of(pos + size), rect.top() + 12.0),
    );
    painter.rect_filled(g, 1.0, theme::dim(theme::ACCENT, 0.7));
    let x = x_of(pos);
    painter.line_segment(
        [pos2(x, rect.top()), pos2(x, rect.bottom())],
        Stroke::new(1.5, theme::ACCENT),
    );
    painter.text(
        pos2(rect.right() - 3.0, rect.bottom() - 3.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{secs:.2} s"),
        egui::FontId::proportional(9.5),
        theme::TEXT_DIM,
    );
    if resp.clicked() || resp.dragged() {
        if let Some(pt) = resp.interact_pointer_pos() {
            let f = ((pt.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            if (f - p.position).abs() > 1e-6 {
                p.position = f;
                return true;
            }
        }
    }
    false
}

pub fn device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let sample = sample_data(app, track);
    let peaks = sample.as_ref().map(|(_, d)| app.waveform_peaks(d));
    let mut action: Option<Action> = None;
    edit_instrument(app, track, |_app, inst| {
        let Instrument::Granular(p) = inst else { return false };
        let mut changed = false;
        device_box(ui, "SAMPLE", |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.horizontal(|ui| {
                    let name = sample
                        .as_ref()
                        .map(|s| s.0.clone())
                        .unwrap_or_else(|| "No sample".into());
                    ui.label(egui::RichText::new(name).color(theme::TEXT).small());
                    if ui.small_button("Load…").clicked() {
                        action = Some(Action::Dialog);
                    }
                    if sample.is_some() && ui.small_button("x").on_hover_text("Remove sample").clicked() {
                        action = Some(Action::Clear);
                    }
                });
                changed |= wave_view(
                    ui,
                    p,
                    peaks.as_deref().map(|v| v.as_slice()),
                    sample.as_ref().map(|(_, d)| d.as_ref()),
                    &mut action,
                );
                ui.horizontal(|ui| {
                    let cur = p.mode.round() as usize;
                    for (i, m) in MODES.iter().enumerate() {
                        if widgets::toggle(ui, cur == i, m, theme::ACCENT, 52.0).clicked() && cur != i {
                            p.mode = i as f32;
                            changed = true;
                        }
                    }
                });
            });
        });
        // Position, scan, loop, spray, size, density, shape.
        changed |= knob_section(ui, track, "GRAINS", p, &[1, 2, 3, 4, 5, 6, 7]);
        changed |= knob_section(ui, track, "PITCH", p, &[8, 9, 10, 11, 12, 13]);
        changed |= knob_section(ui, track, "FILTER", p, &[14, 15, 16, 17]);
        changed |= knob_section(ui, track, "AMP", p, &[18, 19, 20, 21, 22, 28]);
        changed |= knob_section(ui, track, "LFO", p, &[23, 24, 25, 26, 27]);
        changed
    });
    match action {
        Some(Action::Load(path)) => load(app, track, &path),
        Some(Action::Dialog) => {
            if let Some(path) = rfd::FileDialog::new().add_filter("WAV audio", &["wav"]).pick_file() {
                load(app, track, &path);
            }
        }
        Some(Action::Clear) => app.clear_instrument_sample(track, 0),
        Some(Action::Forward(item)) => app.drop_item(&item, DropTarget::Device(track)),
        None => {}
    }
}

/// Loads a WAV file into the track's sample slot 0 (the Granular Synth's
/// source), keeping the instrument.
fn load(app: &mut TesseraApp, track: Id, path: &std::path::Path) {
    match SampleRef::load(path) {
        Ok(s) => {
            let name = s.name();
            if let Some(t) = app.project.track_mut(track) {
                t.set_sample(0, Some(s));
                app.status = format!("Loaded {name}");
                app.changed();
            }
        }
        Err(e) => app.status = format!("Could not load {}: {e}", path.display()),
    }
}
