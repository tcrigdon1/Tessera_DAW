//! Note Expression lane (View > Note Expression Editor): per-note pitch
//! and pressure curves, like MPE in Ableton. The lane shows the curves of
//! the selected notes over their own time span; drag to draw, double-click
//! a point to remove it, right-click for shapes.

use eframe::egui::{self, pos2, Color32, Rect, Sense, Stroke};
use tessera_engine::model::{curve_value, ExpressionKind, Note};

use super::super::theme;
use super::{NoteDrag, View};
use crate::app::{ClipRef, TesseraApp};

/// Points closer than this many pixels are replaced while drawing.
const PAINT_PX: f32 = 4.0;
const TABS_W: f32 = 64.0;

/// Ready-made curves: (label, kind, points as (share of the note, value)).
type Shape = (&'static str, ExpressionKind, &'static [(f64, f32)]);

const SHAPES: &[Shape] = &[
    (
        "Bend Up a Tone",
        ExpressionKind::Pitch,
        &[(0.0, 0.0), (0.3, 2.0), (1.0, 2.0)],
    ),
    (
        "Bend Down a Tone",
        ExpressionKind::Pitch,
        &[(0.0, 0.0), (0.3, -2.0), (1.0, -2.0)],
    ),
    (
        "Scoop Into the Note",
        ExpressionKind::Pitch,
        &[(0.0, -2.0), (0.15, 0.0), (1.0, 0.0)],
    ),
    (
        "Fall Off an Octave",
        ExpressionKind::Pitch,
        &[(0.0, 0.0), (0.7, 0.0), (1.0, -12.0)],
    ),
    (
        "Vibrato",
        ExpressionKind::Pitch,
        &[
            (0.0, 0.0),
            (0.2, 0.0),
            (0.3, 0.4),
            (0.4, -0.4),
            (0.5, 0.4),
            (0.6, -0.4),
            (0.7, 0.4),
            (0.8, -0.4),
            (0.9, 0.4),
            (1.0, 0.0),
        ],
    ),
    ("Swell", ExpressionKind::Pressure, &[(0.0, 0.1), (1.0, 1.0)]),
    ("Fade Out", ExpressionKind::Pressure, &[(0.0, 1.0), (1.0, 0.0)]),
    (
        "Tremolo",
        ExpressionKind::Pressure,
        &[
            (0.0, 1.0),
            (0.125, 0.3),
            (0.25, 1.0),
            (0.375, 0.3),
            (0.5, 1.0),
            (0.625, 0.3),
            (0.75, 1.0),
            (0.875, 0.3),
            (1.0, 1.0),
        ],
    ),
];

fn format_value(kind: ExpressionKind, v: f32) -> String {
    match kind {
        ExpressionKind::Pitch => format!("{v:+.2} st"),
        ExpressionKind::Pressure => format!("{:.0} %", v * 100.0),
    }
}

/// Sets `curve` to `value` at `t`, replacing points between `from` and `t`
/// (a sweep of the mouse) and within `eps` of `t`.
pub(crate) fn paint(curve: &mut Vec<(f64, f32)>, from: f64, t: f64, value: f32, eps: f64) {
    let (lo, hi) = (from.min(t) - eps, from.max(t) + eps);
    curve.retain(|p| p.0 < lo || p.0 > hi);
    let at = curve.partition_point(|p| p.0 < t);
    curve.insert(at, (t, value));
}

pub(super) fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef, view: &View, lane: Rect) {
    let Some(clip) = app.clip(r) else { return };
    let color = theme::rgb(clip.color);
    let notes: Vec<Note> = clip.notes().to_vec();
    let kind = app.piano.expression_kind;
    let (min, max) = kind.range();
    let selection: Vec<usize> = app
        .piano
        .selection
        .iter()
        .copied()
        .filter(|&i| i < notes.len())
        .collect();
    let focus = selection.first().copied();
    // Curves of every note with one, so notes with expression stand out.
    let curves: Vec<(usize, Vec<(f64, f32)>)> = clip
        .expressions
        .iter()
        .filter(|e| e.note < notes.len() && !e.curve(kind).is_empty())
        .map(|e| (e.note, e.curve(kind).to_vec()))
        .collect();

    let area = Rect::from_min_max(pos2(view.grid.left(), lane.top()), lane.max);
    let painter = ui.painter_at(lane);
    painter.rect_filled(lane, 0.0, theme::bg_view());
    painter.rect_filled(
        Rect::from_min_max(lane.min, pos2(area.left() - 1.0, lane.bottom())),
        0.0,
        theme::bg_bar(),
    );
    painter.hline(lane.x_range(), lane.top(), Stroke::new(1.0, Color32::BLACK));

    // Pitch / Pressure tabs on the left.
    let tabs = Rect::from_min_size(
        lane.min + egui::vec2(3.0, 3.0),
        egui::vec2(TABS_W.min(area.left() - lane.left() - 4.0), 16.0),
    );
    for (k, kk) in ExpressionKind::ALL.into_iter().enumerate() {
        let rect = tabs.translate(egui::vec2(0.0, k as f32 * 18.0));
        let on = kk == kind;
        let resp = ui.interact(rect, ui.id().with(("expr_tab", k)), Sense::click());
        painter.rect_filled(rect, 2.0, if on { theme::ACCENT } else { theme::bg_view() });
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            kk.label(),
            egui::FontId::proportional(10.0),
            if on { Color32::BLACK } else { theme::text_dim() },
        );
        if resp.clicked() {
            app.piano.expression_kind = kk;
        }
    }

    let usable = area.height() - 8.0;
    let y_of = |v: f32| area.bottom() - 4.0 - (v - min) / (max - min) * usable;
    let value_at = |y: f32| {
        let v = min + (area.bottom() - 4.0 - y) / usable * (max - min);
        // Pitch snaps to whole semitones near them, for clean bends.
        let v = if kind == ExpressionKind::Pitch && (v - v.round()).abs() < 0.12 {
            v.round()
        } else {
            v
        };
        v.clamp(min, max)
    };
    painter.hline(area.x_range(), y_of(kind.neutral()), Stroke::new(1.0, theme::grid()));
    if kind == ExpressionKind::Pitch {
        for v in [-12.0, 12.0] {
            painter.hline(area.x_range(), y_of(v), Stroke::new(1.0, theme::gray(0x30)));
        }
    }
    let end_x = view.x(view.len);
    if end_x < area.right() {
        painter.rect_filled(
            Rect::from_min_max(pos2(end_x.max(area.left()), area.top()), area.max),
            0.0,
            Color32::from_black_alpha(90),
        );
    }

    // Spans of the selected notes.
    for &i in &selection {
        let n = &notes[i];
        let span = Rect::from_min_max(
            pos2(view.x(n.start).max(area.left()), area.top()),
            pos2(view.x(n.start + n.length).min(area.right()), area.bottom()),
        );
        if span.width() > 0.0 {
            painter.rect_filled(span, 0.0, Color32::from_white_alpha(10));
        }
    }
    // Curves: selected notes bright, others dim.
    for (i, curve) in &curves {
        let n = &notes[*i];
        let selected = selection.contains(i);
        let c = if selected {
            theme::ACCENT
        } else {
            theme::dim(color, 0.5)
        };
        let steps = ((n.length as f32 * view.ppb) / 3.0).clamp(2.0, 400.0) as usize;
        let line: Vec<egui::Pos2> = (0..=steps)
            .map(|s| {
                let t = n.length * s as f64 / steps as f64;
                pos2(view.x(n.start + t), y_of(curve_value(curve, t, kind.neutral())))
            })
            .collect();
        painter.add(egui::Shape::line(
            line,
            Stroke::new(if selected { 1.5 } else { 1.0 }, c),
        ));
        if selected {
            for &(t, v) in curve {
                painter.circle_filled(pos2(view.x(n.start + t), y_of(v)), 2.5, c);
            }
        }
    }
    if focus.is_none() {
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            "Select a note to draw its expression",
            egui::FontId::proportional(11.0),
            theme::text_dim(),
        );
    }

    // ----- interaction -----
    let Some(focus) = focus else { return };
    let resp = ui.interact(area, ui.id().with("piano_expression"), Sense::click_and_drag());
    let fnote = notes[focus];
    let t_at = |x: f32| (view.beat(x) - fnote.start).clamp(0.0, fnote.length);
    let eps = (PAINT_PX / view.ppb) as f64;
    // Paints onto every selected note, each over its own length.
    let paint_all = |app: &mut TesseraApp, from: f64, t: f64, v: f32| {
        if let Some(c) = app.clip_mut(r) {
            for &i in &selection {
                let len = c.notes().get(i).map(|n| n.length).unwrap_or(0.0);
                let curve = c.expression_mut(i).curve_mut(kind);
                paint(curve, from.min(len), t.min(len), v, eps);
            }
        }
    };
    let mut shown = None;
    if resp.double_clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let t = t_at(p.x);
            if let Some(c) = app.clip_mut(r) {
                for &i in &selection {
                    c.expression_mut(i)
                        .curve_mut(kind)
                        .retain(|q| (q.0 - t).abs() > eps * 1.5);
                }
                let old = c.notes().to_vec();
                c.remap_expressions(&old);
            }
            app.label_edit("Note Expression");
            app.changed();
        }
    } else if resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let (t, v) = (t_at(p.x), value_at(p.y));
            app.label_edit("Note Expression");
            paint_all(app, t, t, v);
            app.changed();
        }
    }
    if resp.drag_started() {
        if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
            app.label_edit("Note Expression");
            app.piano.drag = Some(NoteDrag::Expression { last_t: t_at(p.x) });
        }
    }
    if resp.dragged() {
        if let (Some(p), Some(NoteDrag::Expression { last_t })) = (resp.interact_pointer_pos(), app.piano.drag.clone())
        {
            let (t, v) = (t_at(p.x), value_at(p.y));
            paint_all(app, last_t, t, v);
            app.touch();
            app.piano.drag = Some(NoteDrag::Expression { last_t: t });
            shown = Some((p, v));
        }
    }
    if resp.drag_stopped() {
        if matches!(app.piano.drag, Some(NoteDrag::Expression { .. })) {
            app.changed();
        }
        app.piano.drag = None;
    }
    if let Some(p) = resp.hover_pos() {
        if shown.is_none() && resp.hovered() {
            shown = Some((p, value_at(p.y)));
        }
    }
    if let Some((p, v)) = shown {
        painter.text(
            pos2((p.x + 10.0).min(area.right() - 50.0), (p.y - 6.0).max(area.top() + 6.0)),
            egui::Align2::LEFT_CENTER,
            format_value(kind, v),
            egui::FontId::proportional(11.0),
            theme::text(),
        );
    }
    resp.context_menu(|ui| {
        for &(label, k, points) in SHAPES.iter().filter(|s| s.1 == kind) {
            if ui.button(label).clicked() {
                app.label_edit("Note Expression");
                if let Some(c) = app.clip_mut(r) {
                    for &i in &selection {
                        let len = c.notes().get(i).map(|n| n.length).unwrap_or(0.0);
                        *c.expression_mut(i).curve_mut(k) = points.iter().map(|&(f, v)| (f * len, v)).collect();
                    }
                }
                app.changed();
                ui.close();
            }
        }
        ui.separator();
        if ui.button(format!("Clear {}", kind.label())).clicked() {
            app.label_edit("Clear Note Expression");
            if let Some(c) = app.clip_mut(r) {
                for &i in &selection {
                    c.expression_mut(i).curve_mut(kind).clear();
                }
                let old = c.notes().to_vec();
                c.remap_expressions(&old);
            }
            app.changed();
            ui.close();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn painting_replaces_the_swept_points() {
        let mut c = vec![(0.0, 1.0), (0.5, 2.0), (1.0, 3.0)];
        paint(&mut c, 0.4, 0.6, 9.0, 0.01);
        assert_eq!(c, vec![(0.0, 1.0), (0.6, 9.0), (1.0, 3.0)]);
        paint(&mut c, 0.2, 0.2, 5.0, 0.01);
        assert_eq!(c[1], (0.2, 5.0), "kept sorted");
    }
}
