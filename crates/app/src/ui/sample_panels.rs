//! Device view panels for the sample-based instruments: Sampler and Drum
//! Rack.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};
use tessera_engine::dsp::drum_rack::{self, DrumRackParams, PadParams, PadSource, CHOKE_GROUPS, PADS};
use tessera_engine::dsp::sample::detect_transients;
use tessera_engine::dsp::sampler::{FilterType, PlayMode, SamplerParams, MAX_SLICES, SLICE_BASE_NOTE};
use tessera_engine::model::{AudioData, Id, Instrument};

use super::browser::{draw_peaks, drop_on};
use super::detail::device_box;
use super::widgets::{self, Scale};
use super::{session, theme};
use crate::app::TesseraApp;
use crate::browser::DropTarget;

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn note_name(n: u8) -> String {
    format!("{}{}", NOTE_NAMES[n as usize % 12], n as i32 / 12 - 2)
}

fn fmt_hz(v: f32) -> String {
    if v >= 1000.0 {
        format!("{:.1} kHz", v / 1000.0)
    } else {
        format!("{v:.0} Hz")
    }
}

fn fmt_time(v: f32) -> String {
    if v >= 1.0 {
        format!("{v:.2} s")
    } else {
        format!("{:.0} ms", v * 1000.0)
    }
}

fn fmt_pct(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

fn fmt_db(v: f32) -> String {
    format!("{v:.1} dB")
}

fn fmt_pan(v: f32) -> String {
    if v.abs() < 0.005 {
        "C".into()
    } else if v < 0.0 {
        format!("{:.0}L", -v * 50.0)
    } else {
        format!("{:.0}R", v * 50.0)
    }
}

/// A device box whose content is stacked in rows.
fn rows_box(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    device_box(ui, title, |ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui);
        });
    });
}

fn small(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into()).small().color(theme::TEXT_DIM)
}

fn sample_data(app: &TesseraApp, track: Id, slot: usize) -> Option<(String, Arc<AudioData>)> {
    let s = app.project.track(track)?.sample(slot)?;
    Some((s.name(), s.data.clone()?))
}

/// What a waveform marker drag changes.
#[derive(Clone, Copy, PartialEq)]
enum Marker {
    Start,
    End,
    Loop,
    Slice(usize),
}

// ----- Sampler ---------------------------------------------------------------------

pub fn sampler_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let Some(Instrument::Sampler(mut p)) = app.project.track(track).map(|t| t.instrument) else {
        return;
    };
    let before = p;
    let sample = sample_data(app, track, 0);
    let mut load = false;
    let mut clear = false;
    let mut audition: Option<u8> = None;
    let sens_id = egui::Id::new(("slice_sens", track));
    let mut sensitivity: f32 = ui.data(|d| d.get_temp(sens_id)).unwrap_or(0.5);

    rows_box(ui, "SAMPLE", |ui| {
        ui.horizontal(|ui| {
            let name = sample
                .as_ref()
                .map(|s| s.0.clone())
                .unwrap_or_else(|| "No sample".into());
            ui.label(egui::RichText::new(name).color(theme::TEXT).small());
            if ui.small_button("Load…").clicked() {
                load = true;
            }
            if sample.is_some() && ui.small_button("x").on_hover_text("Remove sample").clicked() {
                clear = true;
            }
        });
        let peaks = sample.as_ref().map(|(_, d)| app.waveform_peaks(d));
        let frames = sample.as_ref().map(|(_, d)| d.frames()).unwrap_or(0);
        sample_view(
            ui,
            track,
            &mut p,
            peaks.as_deref().map(|v| v.as_slice()),
            frames,
            &mut audition,
        );
        ui.horizontal(|ui| {
            for m in PlayMode::ALL {
                if widgets::toggle(ui, p.mode == m, m.label(), theme::ACCENT, 46.0).clicked() {
                    p.mode = m;
                    if m == PlayMode::Slice && p.voices > 1 {
                        p.voices = 1;
                    }
                }
            }
            ui.add_space(6.0);
            if widgets::toggle(ui, p.reverse, "Rev", theme::ACCENT, 34.0)
                .on_hover_text("Play backwards")
                .clicked()
            {
                p.reverse = !p.reverse;
            }
            if p.mode == PlayMode::Classic
                && widgets::toggle(ui, p.loop_on, "Loop", theme::ACCENT, 38.0)
                    .on_hover_text("Loop while the key is held")
                    .clicked()
            {
                p.loop_on = !p.loop_on;
            }
        });
    });

    if p.mode == PlayMode::Slice {
        rows_box(ui, "SLICES", |ui| {
            ui.label(small(format!(
                "{} slices on {}–{}",
                p.slice_count,
                note_name(SLICE_BASE_NOTE),
                note_name(SLICE_BASE_NOTE + p.slice_count.saturating_sub(1))
            )));
            ui.horizontal(|ui| {
                let changed = widgets::knob(
                    ui,
                    "Sensitivity",
                    &mut sensitivity,
                    0.0,
                    1.0,
                    0.5,
                    Scale::Linear,
                    fmt_pct,
                );
                ui.vertical(|ui| {
                    let by_transients = ui.small_button("Transients").clicked();
                    if (by_transients || changed) && sample.is_some() {
                        if let Some((_, d)) = &sample {
                            let pts = detect_transients(d, MAX_SLICES, sensitivity);
                            p.set_slices(&pts);
                        }
                    }
                    ui.horizontal(|ui| {
                        for n in [4usize, 8, 16, 32] {
                            if ui.small_button(n.to_string()).on_hover_text("Equal slices").clicked() {
                                p.slices = tessera_engine::dsp::sampler::even_slices(n);
                                p.slice_count = n as u8;
                            }
                        }
                    });
                });
            });
        });
    }

    rows_box(ui, "PITCH", |ui| {
        ui.horizontal(|ui| {
            if p.mode != PlayMode::Slice {
                let mut root = p.root as f32;
                if widgets::knob(ui, "Root", &mut root, 0.0, 127.0, 60.0, Scale::Linear, |v| {
                    note_name(v.round() as u8)
                }) {
                    p.root = root.round().clamp(0.0, 127.0) as u8;
                }
            }
            widgets::knob(ui, "Transp", &mut p.transpose, -48.0, 48.0, 0.0, Scale::Linear, |v| {
                format!("{:+.0} st", v.round())
            });
            p.transpose = p.transpose.round();
        });
        ui.horizontal(|ui| {
            widgets::knob(ui, "Detune", &mut p.detune, -50.0, 50.0, 0.0, Scale::Linear, |v| {
                format!("{v:+.0} ct")
            });
            let mut voices = p.voices as f32;
            if widgets::knob(ui, "Voices", &mut voices, 1.0, 16.0, 8.0, Scale::Linear, |v| {
                if v.round() <= 1.0 {
                    "Mono".into()
                } else {
                    format!("{:.0}", v.round())
                }
            }) {
                p.voices = voices.round().clamp(1.0, 16.0) as u8;
            }
        });
    });

    rows_box(ui, "FILTER", |ui| {
        ui.horizontal(|ui| {
            if widgets::toggle(
                ui,
                p.filter_on,
                if p.filter_on { "On" } else { "Off" },
                theme::ACCENT,
                34.0,
            )
            .clicked()
            {
                p.filter_on = !p.filter_on;
            }
            for t in FilterType::ALL {
                if widgets::toggle(ui, p.filter_type == t, t.label(), theme::SOLO, 28.0).clicked() {
                    p.filter_type = t;
                    p.filter_on = true;
                }
            }
        });
        ui.horizontal(|ui| {
            widgets::knob(ui, "Cutoff", &mut p.cutoff, 30.0, 18000.0, 18000.0, Scale::Log, fmt_hz);
            widgets::knob(ui, "Res", &mut p.resonance, 0.0, 4.0, 0.7, Scale::Linear, |v| {
                format!("{v:.2}")
            });
            widgets::knob(ui, "Env", &mut p.filter_env, -4.0, 6.0, 0.0, Scale::Linear, |v| {
                format!("{v:+.1} oct")
            });
        });
        ui.horizontal(|ui| {
            widgets::knob(ui, "Attack", &mut p.f_attack, 0.001, 5.0, 0.001, Scale::Log, fmt_time);
            widgets::knob(ui, "Decay", &mut p.f_decay, 0.005, 5.0, 0.4, Scale::Log, fmt_time);
            widgets::knob(ui, "Sustain", &mut p.f_sustain, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
            widgets::knob(ui, "Release", &mut p.f_release, 0.005, 8.0, 0.2, Scale::Log, fmt_time);
        });
    });

    rows_box(ui, "AMP", |ui| {
        ui.horizontal(|ui| {
            widgets::knob(ui, "Attack", &mut p.attack, 0.0005, 5.0, 0.001, Scale::Log, fmt_time);
            widgets::knob(ui, "Decay", &mut p.decay, 0.005, 10.0, 1.0, Scale::Log, fmt_time);
            widgets::knob(ui, "Sustain", &mut p.sustain, 0.0, 1.0, 1.0, Scale::Linear, fmt_pct);
            widgets::knob(ui, "Release", &mut p.release, 0.005, 8.0, 0.08, Scale::Log, fmt_time);
        });
        ui.horizontal(|ui| {
            widgets::knob(ui, "Vel", &mut p.velocity, 0.0, 1.0, 0.6, Scale::Linear, fmt_pct);
            widgets::knob(ui, "Pan", &mut p.pan, -1.0, 1.0, 0.0, Scale::Linear, fmt_pan);
            widgets::knob(ui, "Volume", &mut p.volume_db, -40.0, 12.0, -3.0, Scale::Linear, fmt_db);
        });
    });

    ui.data_mut(|d| d.insert_temp(sens_id, sensitivity));
    if p != before {
        if let Some(t) = app.project.track_mut(track) {
            t.instrument = Instrument::Sampler(p);
        }
        app.send_instrument(track);
    }
    if load {
        app.load_instrument_sample_dialog(track, 0);
    }
    if clear {
        app.clear_instrument_sample(track, 0);
    }
    if let Some(n) = audition {
        let now = ui.input(|i| i.time);
        app.audition(track, n, now);
    }
}

/// The Sampler's waveform with draggable start/end, loop and slice markers.
fn sample_view(
    ui: &mut egui::Ui,
    track: Id,
    p: &mut SamplerParams,
    peaks: Option<&[f32]>,
    frames: usize,
    audition: &mut Option<u8>,
) {
    let (rect, resp) = ui.allocate_exact_size(vec2(300.0, 92.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::BG_VIEW);
    let Some(peaks) = peaks else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Drop a sample here\nor from the browser",
            egui::FontId::proportional(11.0),
            theme::TEXT_DIM,
        );
        return;
    };
    let x_of = |f: f32| rect.left() + f.clamp(0.0, 1.0) * rect.width();
    let f_of = |x: f32| ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
    let (s, e) = (p.start.min(p.end), p.start.max(p.end));
    // Dim outside the region.
    draw_peaks(&painter, rect, peaks, theme::dim(theme::METER_LOW, 0.45));
    let region = Rect::from_min_max(pos2(x_of(s), rect.top()), pos2(x_of(e), rect.bottom()));
    if p.mode != PlayMode::Slice {
        let inner = ui.painter_at(region);
        draw_peaks(&inner, rect, peaks, theme::METER_LOW);
    } else {
        draw_peaks(&painter, rect, peaks, theme::METER_LOW);
    }

    // Markers.
    let mut markers: Vec<(Marker, f32, Color32)> = Vec::new();
    match p.mode {
        PlayMode::Slice => {
            for i in 0..(p.slice_count as usize).min(MAX_SLICES) {
                markers.push((Marker::Slice(i), p.slices[i], theme::SOLO));
            }
        }
        _ => {
            markers.push((Marker::Start, p.start, theme::ACCENT));
            markers.push((Marker::End, p.end, theme::ACCENT));
            if p.mode == PlayMode::Classic && p.loop_on {
                markers.push((Marker::Loop, s + (e - s) * p.loop_start, theme::ACTIVE));
            }
        }
    }
    for (m, f, c) in &markers {
        let x = x_of(*f);
        painter.vline(x, rect.y_range(), Stroke::new(1.5, *c));
        let label = match m {
            Marker::Start => "S".to_string(),
            Marker::End => "E".to_string(),
            Marker::Loop => "L".to_string(),
            Marker::Slice(i) => format!("{}", i + 1),
        };
        let fx = if x > rect.right() - 12.0 { x - 12.0 } else { x };
        let flag = Rect::from_min_size(pos2(fx, rect.top()), vec2(12.0, 11.0));
        painter.rect_filled(flag, 1.0, *c);
        painter.text(
            flag.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(8.0),
            Color32::BLACK,
        );
    }
    painter.text(
        pos2(rect.right() - 3.0, rect.bottom() - 2.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{frames} smp"),
        egui::FontId::proportional(9.0),
        theme::TEXT_DIM,
    );

    // Dragging the nearest marker.
    let drag_id = egui::Id::new(("sampler_marker", track));
    if resp.drag_started() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let nearest = markers
                .iter()
                .map(|(m, f, _)| (*m, (x_of(*f) - pos.x).abs()))
                .filter(|(_, d)| *d < 8.0)
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(m, _)| m);
            ui.data_mut(|d| d.insert_temp(drag_id, nearest));
        }
    }
    if resp.dragged() {
        let held: Option<Marker> = ui.data(|d| d.get_temp(drag_id)).flatten();
        if let (Some(m), Some(pos)) = (held, resp.interact_pointer_pos()) {
            let f = f_of(pos.x);
            match m {
                Marker::Start => p.start = f.min(p.end - 0.001),
                Marker::End => p.end = f.max(p.start + 0.001),
                Marker::Loop => p.loop_start = ((f - s) / (e - s).max(1e-4)).clamp(0.0, 0.98),
                Marker::Slice(i) => {
                    if i > 0 {
                        let lo = p.slices[i - 1] + 0.002;
                        let hi = if i + 1 < p.slice_count as usize {
                            p.slices[i + 1] - 0.002
                        } else {
                            0.998
                        };
                        p.slices[i] = f.clamp(lo, hi.max(lo));
                    }
                }
            }
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<Option<Marker>>(drag_id));
    }
    if resp.clicked() {
        // Click: play the slice under the pointer, or the root note.
        if let Some(pos) = resp.interact_pointer_pos() {
            let f = f_of(pos.x);
            *audition = Some(match p.mode {
                PlayMode::Slice => {
                    let count = (p.slice_count as usize).max(1);
                    let i = (0..count).rev().find(|&i| p.slices[i] <= f).unwrap_or(0);
                    SLICE_BASE_NOTE + i as u8
                }
                _ => p.root,
            });
        }
    }
    resp.on_hover_text("Drag the markers · Click to play");
}

// ----- Drum Rack -------------------------------------------------------------------

fn pad_label(app: &TesseraApp, track: Id, pad: usize, pp: &PadParams) -> String {
    match pp.source {
        PadSource::Sample => app
            .project
            .track(track)
            .and_then(|t| t.sample(pad))
            .map(|s| s.name())
            .unwrap_or_else(|| "Empty".into()),
        s => s.label().to_string(),
    }
}

/// Piano-roll rows for drum instruments, top to bottom: (note, name).
pub fn drum_rows(app: &TesseraApp, track: Id) -> Option<Vec<(u8, String)>> {
    match app.project.track(track)?.instrument {
        Instrument::DrumKit(_) => Some(
            tessera_engine::dsp::drums::PADS
                .iter()
                .rev()
                .map(|(n, name)| (*n, name.to_string()))
                .collect(),
        ),
        Instrument::DrumRack(p) => Some(
            (0..PADS)
                .rev()
                .map(|i| (drum_rack::BASE_NOTE + i as u8, pad_label(app, track, i, &p.pads[i])))
                .collect(),
        ),
        _ => None,
    }
}

pub fn drum_rack_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let Some(Instrument::DrumRack(mut p)) = app.project.track(track).map(|t| t.instrument) else {
        return;
    };
    let before = p;
    let now = ui.input(|i| i.time);
    // Light up pads the engine played.
    if let Some(index) = app.project.track_index(track) {
        if let Some(ts) = session::track_state(app, index, track) {
            let hits = ts.pad_hits.swap(0, Ordering::Relaxed);
            for i in 0..PADS {
                if hits & (1 << i) != 0 {
                    app.browser.pad_flash[i] = now;
                }
            }
        }
    }
    let mut selected = app.drum_pad(track).min(PADS - 1);
    let mut hit: Option<usize> = None;
    let mut drops: Vec<(crate::browser::BrowserItem, usize)> = Vec::new();
    let mut load: Option<usize> = None;
    let mut clear: Option<usize> = None;
    let labels: Vec<String> = (0..PADS).map(|i| pad_label(app, track, i, &p.pads[i])).collect();

    device_box(ui, "PADS", |ui| {
        let size = vec2(64.0, 40.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
            // Bottom row holds the lowest pads, as on hardware.
            for row in (0..4).rev() {
                ui.horizontal(|ui| {
                    for col in 0..4 {
                        let i = row * 4 + col;
                        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
                        let pp = &p.pads[i];
                        let flash = ((now - app.browser.pad_flash[i]) / 0.25).clamp(0.0, 1.0) as f32;
                        let base = if i == selected {
                            Color32::from_rgb(0x6a, 0x5a, 0x40)
                        } else {
                            Color32::from_rgb(0x44, 0x44, 0x44)
                        };
                        let lit = theme::ACCENT;
                        let fill = Color32::from_rgb(
                            (lit.r() as f32 * (1.0 - flash) + base.r() as f32 * flash) as u8,
                            (lit.g() as f32 * (1.0 - flash) + base.g() as f32 * flash) as u8,
                            (lit.b() as f32 * (1.0 - flash) + base.b() as f32 * flash) as u8,
                        );
                        let painter = ui.painter_at(rect);
                        painter.rect(
                            rect,
                            3.0,
                            fill,
                            Stroke::new(
                                1.0,
                                if i == selected {
                                    theme::ACCENT
                                } else {
                                    Color32::from_rgb(0x22, 0x22, 0x22)
                                },
                            ),
                            StrokeKind::Inside,
                        );
                        let text_col = if pp.mute { theme::TEXT_DIM } else { theme::TEXT };
                        painter.text(
                            pos2(rect.left() + 4.0, rect.top() + 8.0),
                            egui::Align2::LEFT_CENTER,
                            drum_rack::pad_note_name(i),
                            egui::FontId::proportional(9.0),
                            theme::TEXT_DIM,
                        );
                        if pp.choke != 0 {
                            painter.text(
                                pos2(rect.right() - 4.0, rect.top() + 8.0),
                                egui::Align2::RIGHT_CENTER,
                                format!("C{}", pp.choke),
                                egui::FontId::proportional(9.0),
                                theme::TEXT_DIM,
                            );
                        }
                        let mut label = labels[i].clone();
                        if label.chars().count() > 10 {
                            label = label.chars().take(9).collect::<String>() + "…";
                        }
                        painter.text(
                            pos2(rect.center().x, rect.bottom() - 12.0),
                            egui::Align2::CENTER_CENTER,
                            label,
                            egui::FontId::proportional(11.0),
                            text_col,
                        );
                        if resp.clicked() {
                            selected = i;
                            hit = Some(i);
                        }
                        let resp = resp.on_hover_text(format!(
                            "{} · {}\nClick to play · Drop a sample to load it",
                            drum_rack::pad_note_name(i),
                            labels[i]
                        ));
                        resp.context_menu(|ui| {
                            if ui.button("Load Sample…").clicked() {
                                load = Some(i);
                                ui.close();
                            }
                            if ui.button("Clear Sample").clicked() {
                                clear = Some(i);
                                ui.close();
                            }
                            ui.menu_button("Source", |ui| {
                                for s in PadSource::ALL {
                                    if ui.selectable_label(p.pads[i].source == s, s.label()).clicked() {
                                        p.pads[i].source = s;
                                        ui.close();
                                    }
                                }
                            });
                        });
                        if let Some(item) = drop_on(ui, rect) {
                            drops.push((item, i));
                        }
                    }
                });
            }
        });
    });

    let sel_sample = sample_data(app, track, selected);
    let pp = &mut p.pads[selected];
    let title = format!("PAD {} · {}", drum_rack::pad_note_name(selected), labels[selected]);
    rows_box(ui, &title, |ui| {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("pad_source", track, selected))
                .width(90.0)
                .selected_text(pp.source.label())
                .show_ui(ui, |ui| {
                    for s in PadSource::ALL {
                        ui.selectable_value(&mut pp.source, s, s.label());
                    }
                });
            if pp.source == PadSource::Sample {
                if ui.small_button("Load…").clicked() {
                    load = Some(selected);
                }
                if sel_sample.is_some() && ui.small_button("x").on_hover_text("Clear sample").clicked() {
                    clear = Some(selected);
                }
            }
            if widgets::toggle(ui, pp.mute, "M", theme::ACCENT, 20.0)
                .on_hover_text("Mute pad")
                .clicked()
            {
                pp.mute = !pp.mute;
            }
            if pp.source == PadSource::Sample
                && widgets::toggle(ui, pp.reverse, "Rev", theme::ACCENT, 32.0)
                    .on_hover_text("Play backwards")
                    .clicked()
            {
                pp.reverse = !pp.reverse;
            }
            ui.label(small("Choke"));
            egui::ComboBox::from_id_salt(("pad_choke", track, selected))
                .width(44.0)
                .selected_text(if pp.choke == 0 {
                    "–".to_string()
                } else {
                    pp.choke.to_string()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut pp.choke, 0, "None");
                    for g in 1..=CHOKE_GROUPS {
                        ui.selectable_value(&mut pp.choke, g, g.to_string());
                    }
                });
        });
        ui.horizontal(|ui| {
            widgets::knob(ui, "Tune", &mut pp.tune, -24.0, 24.0, 0.0, Scale::Linear, |v| {
                format!("{v:+.1} st")
            });
            if pp.source == PadSource::Sample {
                widgets::knob(ui, "Fade", &mut pp.decay, 0.0, 8.0, 0.0, Scale::Linear, |v| {
                    if v < 0.01 {
                        "Off".into()
                    } else {
                        fmt_time(v)
                    }
                });
                widgets::knob(ui, "Start", &mut pp.start, 0.0, 0.95, 0.0, Scale::Linear, fmt_pct);
            } else {
                widgets::knob(ui, "Decay", &mut pp.decay, 0.1, 4.0, 1.0, Scale::Log, |v| {
                    format!("{v:.2}x")
                });
            }
            widgets::knob(ui, "Cutoff", &mut pp.cutoff, 60.0, 20000.0, 20000.0, Scale::Log, |v| {
                if v >= 19500.0 {
                    "Open".into()
                } else {
                    fmt_hz(v)
                }
            });
            widgets::knob(ui, "Res", &mut pp.resonance, 0.0, 4.0, 0.7, Scale::Linear, |v| {
                format!("{v:.2}")
            });
        });
        ui.horizontal(|ui| {
            widgets::knob(ui, "Drive", &mut pp.drive, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
            widgets::knob(ui, "Volume", &mut pp.volume_db, -40.0, 12.0, 0.0, Scale::Linear, fmt_db);
            widgets::knob(ui, "Pan", &mut pp.pan, -1.0, 1.0, 0.0, Scale::Linear, fmt_pan);
            if let Some((_, d)) = &sel_sample {
                if pp.source == PadSource::Sample {
                    let peaks = app.waveform_peaks(d);
                    let (rect, _) = ui.allocate_exact_size(vec2(110.0, 50.0), Sense::hover());
                    let painter = ui.painter_at(rect);
                    painter.rect_filled(rect, 2.0, theme::BG_VIEW);
                    draw_peaks(&painter, rect, &peaks, theme::METER_LOW);
                    let x = rect.left() + pp.start * rect.width();
                    painter.vline(x, rect.y_range(), Stroke::new(1.5, theme::ACCENT));
                }
            }
        });
    });

    rows_box(ui, "KIT", |ui| {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("kit", track))
                .width(96.0)
                .selected_text("Load Kit")
                .show_ui(ui, |ui| {
                    for (name, kit) in [
                        ("Standard", DrumRackParams::standard()),
                        ("Boom", DrumRackParams::boom()),
                        ("Tight", DrumRackParams::tight()),
                        ("Lo-Fi", DrumRackParams::lofi()),
                        ("Empty (samples)", DrumRackParams::empty()),
                    ] {
                        if ui.selectable_label(false, name).clicked() {
                            p.pads = kit.pads;
                        }
                    }
                });
        });
        widgets::knob(ui, "Volume", &mut p.volume_db, -40.0, 12.0, -4.0, Scale::Linear, fmt_db);
    });

    app.browser.drum_pad.insert(track, selected);
    if p != before {
        if let Some(t) = app.project.track_mut(track) {
            t.instrument = Instrument::DrumRack(p);
        }
        app.send_instrument(track);
    }
    if let Some(i) = hit {
        app.audition(track, drum_rack::BASE_NOTE + i as u8, now);
    }
    if let Some(i) = load {
        app.browser.drum_pad.insert(track, i);
        app.load_instrument_sample_dialog(track, i);
    }
    if let Some(i) = clear {
        app.clear_instrument_sample(track, i);
    }
    for (item, pad) in drops {
        app.browser.drum_pad.insert(track, pad);
        app.drop_item(&item, DropTarget::Pad(track, pad));
    }
}
