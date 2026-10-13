//! Sample editor for audio clips: waveform on the beat grid, warp markers,
//! warp mode, clip tempo and transpose.
//!
//! * Double-click the waveform to add a warp marker (it snaps to a nearby
//!   transient).
//! * Drag a marker to stretch the audio around it; dragging elsewhere on the
//!   waveform creates a marker there (or on the nearest transient) and drags
//!   it. Markers snap to 1/16 notes; hold Alt for free movement.
//! * Double-click or right-click a marker to delete it.

use std::sync::atomic::Ordering;

use eframe::egui::{self, pos2, Rect, Sense, Shape, Stroke};
use tessera_engine::analysis;
use tessera_engine::model::ClipContent;
use tessera_engine::warp::{WarpMode, WarpSettings};

use super::{session, theme};
use crate::app::{ClipRef, TesseraApp};

const STRIP_H: f32 = 16.0;
const HIT: f32 = 7.0;

pub fn editor(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let Some(clip) = app.clip(r).cloned() else { return };
    let ClipContent::Audio(audio) = &clip.content else {
        return;
    };
    let mut warp = audio.warp.clone();
    let mut length = clip.length;
    let mut changed = false;
    let data = audio.data.clone();

    // ----- controls -----
    ui.horizontal(|ui| {
        changed |= ui
            .checkbox(&mut warp.enabled, "Warp")
            .on_hover_text("Follow the project tempo")
            .changed();
        ui.add_enabled_ui(warp.enabled, |ui| {
            egui::ComboBox::from_id_salt("warp_mode")
                .selected_text(warp.mode.label())
                .width(80.0)
                .show_ui(ui, |ui| {
                    for m in WarpMode::ALL {
                        changed |= ui.selectable_value(&mut warp.mode, m, m.label()).changed();
                    }
                })
                .response
                .on_hover_text(
                    "Beats: slices at transients (drums) · Tones: granular (melodic) · \
                     Complex: aligned grains (full mixes) · Re-Pitch: speed and pitch together",
                );
            ui.label(egui::RichText::new("Seg. BPM").color(theme::text_dim()));
            let mut bpm = warp.bpm;
            if ui
                .add(
                    egui::DragValue::new(&mut bpm)
                        .range(20.0..=999.0)
                        .speed(0.1)
                        .fixed_decimals(2),
                )
                .changed()
            {
                warp.set_bpm(bpm);
                changed = true;
            }
            if ui.small_button(":2").on_hover_text("Halve the clip tempo").clicked() {
                warp.set_bpm(warp.bpm / 2.0);
                changed = true;
            }
            if ui.small_button("×2").on_hover_text("Double the clip tempo").clicked() {
                warp.set_bpm(warp.bpm * 2.0);
                changed = true;
            }
        });
        ui.add_enabled_ui(!(warp.enabled && warp.mode == WarpMode::RePitch), |ui| {
            ui.label(egui::RichText::new("Transp.").color(theme::text_dim()));
            changed |= ui
                .add(
                    egui::DragValue::new(&mut warp.transpose)
                        .range(-48.0..=48.0)
                        .speed(0.1)
                        .fixed_decimals(0)
                        .suffix(" st"),
                )
                .changed();
        });
        if let Some(d) = &data {
            if ui
                .button("Auto-Warp")
                .on_hover_text("Detect tempo and transients and lock the clip to the grid")
                .clicked()
            {
                let a = analysis::auto_warp(d, app.project.tempo, app.project.beats_per_bar);
                warp = a.warp;
                length = a.length;
                app.status = format!("Auto-warped at {:.2} BPM ({})", warp.bpm, warp.mode.label());
                changed = true;
            }
        }
        if ui
            .add_enabled(!warp.markers.is_empty(), egui::Button::new("Clear Markers").small())
            .clicked()
        {
            warp.markers.clear();
            changed = true;
        }
    });

    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme::bg_view());
    let Some(data) = data else {
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            format!("Missing audio file: {}", audio.path),
            egui::FontId::proportional(12.0),
            theme::ARM,
        );
        return;
    };

    // ----- mapping -----
    let spb_project = 60.0 / app.project.tempo;
    let rate = warp.rate();
    let dur = data.duration_secs().max(1e-6);
    let to_secs = |w: &WarpSettings, b: f64| {
        if w.enabled {
            w.beat_to_secs(b)
        } else {
            b * spb_project * rate
        }
    };
    let to_beat = |w: &WarpSettings, s: f64| {
        if w.enabled {
            w.secs_to_beat(s)
        } else {
            s / (spb_project * rate)
        }
    };
    let first = to_beat(&warp, 0.0).min(0.0);
    let last = to_beat(&warp, dur).max(length);
    let span = (last - first).max(0.25);
    let strip = Rect::from_min_max(area.min, pos2(area.right(), area.top() + STRIP_H));
    let wave = Rect::from_min_max(pos2(area.left(), strip.bottom()), area.max);
    let ppb = area.width() / span as f32;
    let x_of = |b: f64| area.left() + ((b - first) as f32) * ppb;
    let beat_of = |x: f32| first + ((x - area.left()) / ppb) as f64;

    // ----- interaction -----
    let resp = ui.interact(area, ui.id().with("warp_area"), Sense::click_and_drag());
    let drag_id = ui.id().with("warp_drag");
    let mut drag: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    let (alt, pointer) = ui.input(|i| (i.modifiers.alt, i.pointer.interact_pos()));
    let marker_at = |w: &WarpSettings, x: f32| w.markers.iter().position(|m| (x_of(m.beat) - x).abs() < HIT);
    let transient_near = |w: &WarpSettings, x: f32| {
        w.transients
            .iter()
            .map(|&t| to_beat(w, t))
            .filter(|&b| (x_of(b) - x).abs() < HIT + 2.0)
            .min_by(|a, b| {
                (x_of(*a) - x)
                    .abs()
                    .partial_cmp(&(x_of(*b) - x).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    };
    if warp.enabled {
        let hover_marker = resp.hover_pos().and_then(|p| marker_at(&warp, p.x));
        if hover_marker.is_some() || drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if resp.double_clicked() || resp.secondary_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                match marker_at(&warp, p.x) {
                    Some(i) => warp.remove_marker(i),
                    None if resp.double_clicked() => {
                        let b = transient_near(&warp, p.x).unwrap_or_else(|| beat_of(p.x));
                        warp.add_marker(b);
                    }
                    None => {}
                }
                changed = true;
            }
        } else if resp.drag_started() {
            if let Some(p) = resp.interact_pointer_pos() {
                drag = marker_at(&warp, p.x).or_else(|| {
                    let b = transient_near(&warp, p.x).unwrap_or_else(|| beat_of(p.x));
                    // A marker can only stretch against another one: pin the
                    // clip start first if there's nothing to stretch against.
                    if warp.markers.is_empty() && b > 0.25 {
                        warp.add_marker(0.0);
                    }
                    warp.add_marker(b)
                });
                changed = true;
            }
        }
        if let (Some(i), Some(p)) = (drag, pointer) {
            if resp.dragged() {
                let b = beat_of(p.x);
                let b = if alt { b } else { (b * 4.0).round() / 4.0 };
                warp.move_marker(i, b);
                changed = true;
            }
        }
        if resp.drag_stopped() {
            drag = None;
        }
    }
    ui.data_mut(|d| match drag {
        Some(i) => {
            d.insert_temp(drag_id, i);
        }
        None => d.remove::<usize>(drag_id),
    });

    // ----- drawing -----
    let color = theme::rgb(clip.color);
    // Outside the clip (before its start, after its end) is dimmed.
    painter.rect_filled(
        Rect::from_min_max(pos2(x_of(length), area.top()), area.max),
        0.0,
        theme::gray(0x1e),
    );
    painter.rect_filled(
        Rect::from_min_max(area.min, pos2(x_of(0.0), area.bottom())),
        0.0,
        theme::gray(0x1e),
    );
    painter.rect_filled(strip, 0.0, theme::bg_bar());
    let bpb = app.project.beats_per_bar as f64;
    let step = if ppb > 40.0 {
        0.25
    } else if ppb > 10.0 {
        1.0
    } else {
        bpb
    };
    let mut b = (first / step).ceil() * step;
    while b <= last {
        let is_bar = (b / bpb).fract().abs() < 1e-9;
        let x = x_of(b);
        painter.vline(
            x,
            wave.y_range(),
            Stroke::new(1.0, if is_bar { theme::grid_bar() } else { theme::grid() }),
        );
        if is_bar {
            painter.text(
                pos2(x + 2.0, strip.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{}", (b / bpb).floor() as i64 + 1),
                egui::FontId::proportional(10.0),
                theme::text_dim(),
            );
        }
        b += step;
    }
    let peaks = app.waveform_peaks(&data);
    let n = peaks.len();
    let cy = wave.center().y;
    let h = wave.height() / 2.0 - 4.0;
    let mut x = area.left();
    while x < area.right() {
        let t = to_secs(&warp, beat_of(x));
        if (0.0..dur).contains(&t) {
            let v = peaks[((t / dur) * n as f64) as usize % n].min(1.0);
            let inside = (0.0..length).contains(&beat_of(x));
            let c = if inside { color } else { theme::dim(color, 0.5) };
            painter.vline(x, (cy - v * h)..=(cy + v * h), Stroke::new(1.0, c));
        }
        x += 1.0;
    }
    for &t in &warp.transients {
        let x = x_of(to_beat(&warp, t));
        painter.vline(x, wave.top()..=wave.top() + 8.0, Stroke::new(1.0, theme::gray(0x90)));
    }
    if warp.enabled {
        for (i, m) in warp.markers.iter().enumerate() {
            let x = x_of(m.beat);
            let hot = drag == Some(i);
            let c = if hot { theme::select() } else { theme::ACTIVE };
            painter.vline(x, wave.y_range(), Stroke::new(1.0, c));
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(x - 5.0, strip.top() + 2.0),
                    pos2(x + 5.0, strip.top() + 2.0),
                    pos2(x, strip.bottom()),
                ],
                c,
                Stroke::NONE,
            ));
        }
    }
    painter.vline(x_of(length), area.y_range(), Stroke::new(1.5, theme::ACCENT));

    // Playhead for a playing Session clip.
    if let ClipRef::Slot { track, scene } = r {
        if let Some(ti) = app.project.track_index(track) {
            if let Some(s) = session::track_state(app, ti, track) {
                if s.playing_slot.load(Ordering::Relaxed) == scene as i32 {
                    let x = x_of(s.clip_position.load());
                    painter.vline(x, area.y_range(), Stroke::new(1.0, theme::select()));
                }
            }
        }
    }

    let info = if warp.enabled {
        let seg = resp
            .hover_pos()
            .map(|p| format!(" · segment {:.2} BPM here", warp.bpm_at(beat_of(p.x))))
            .unwrap_or_default();
        format!(
            "{} · {} Hz · {:.2} s · {} markers, {} transients{seg}",
            audio.path,
            data.sample_rate,
            dur,
            warp.markers.len(),
            warp.transients.len()
        )
    } else {
        format!(
            "{} · {} Hz · {:.2} s · not warped: plays at its own speed",
            audio.path, data.sample_rate, dur
        )
    };
    painter.text(
        pos2(area.left() + 6.0, area.bottom() - 9.0),
        egui::Align2::LEFT_CENTER,
        info,
        egui::FontId::proportional(10.5),
        theme::text_dim(),
    );
    if resp.hovered() && warp.enabled {
        resp.on_hover_text_at_pointer(
            "Double-click: add warp marker · Drag: move/create marker (Alt: no snap) · Double/right-click marker: delete",
        );
    }

    if changed {
        if let Some(c) = app.clip_mut(r) {
            c.length = length;
            if let ClipContent::Audio(a) = &mut c.content {
                a.warp = warp;
            }
        }
        app.changed();
    }
}
