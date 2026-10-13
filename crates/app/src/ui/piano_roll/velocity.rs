//! Velocity lane under the note grid: one stem per note. Drag a stem to
//! change its velocity (a selected stem moves the whole selection by the
//! same amount); drag across empty space to draw velocities.

use eframe::egui::{self, pos2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke};
use tessera_engine::model::Note;

use super::super::theme;
use super::{NoteDrag, View};
use crate::app::{ClipRef, TesseraApp};

const HIT: f32 = 4.0;

pub(super) fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef, view: &View, lane: Rect) {
    let Some(clip) = app.clip(r) else { return };
    let color = theme::rgb(clip.color);
    let notes: Vec<Note> = clip.notes().to_vec();
    let stems = Rect::from_min_max(pos2(view.grid.left(), lane.top()), lane.max);
    let painter = ui.painter_at(lane);
    painter.rect_filled(lane, 0.0, theme::bg_view());
    painter.rect_filled(
        Rect::from_min_max(lane.min, pos2(stems.left() - 1.0, lane.bottom())),
        0.0,
        theme::bg_bar(),
    );
    painter.hline(lane.x_range(), lane.top(), Stroke::new(1.0, Color32::BLACK));
    painter.text(
        pos2(lane.left() + 3.0, lane.top() + 3.0),
        egui::Align2::LEFT_TOP,
        "Velocity",
        egui::FontId::proportional(10.0),
        theme::text_dim(),
    );
    let usable = stems.height() - 8.0;
    let y_of = |v: u8| stems.bottom() - 2.0 - v as f32 / 127.0 * usable;
    let vel_at = |y: f32| {
        (((stems.bottom() - 2.0 - y) / usable) * 127.0)
            .round()
            .clamp(1.0, 127.0) as u8
    };
    for v in [64u8, 127] {
        painter.hline(stems.x_range(), y_of(v), Stroke::new(1.0, theme::grid()));
    }
    let end_x = view.x(view.len);
    if end_x < stems.right() {
        painter.rect_filled(
            Rect::from_min_max(pos2(end_x.max(stems.left()), stems.top()), stems.max),
            0.0,
            Color32::from_black_alpha(90),
        );
    }

    let selection = app.piano.selection.clone();
    let order: Vec<usize> = {
        // Selected stems on top.
        let mut o: Vec<usize> = (0..notes.len()).collect();
        o.sort_by_key(|i| selection.contains(i));
        o
    };
    for &i in &order {
        let n = &notes[i];
        let x = view.x(n.start) + 1.0;
        if !stems.x_range().contains(x) {
            continue;
        }
        let c = if selection.contains(&i) {
            theme::ACCENT
        } else {
            theme::dim(color, 0.55 + 0.45 * (n.velocity as f32 / 127.0))
        };
        let top = y_of(n.velocity);
        painter.vline(x, top..=stems.bottom(), Stroke::new(2.0, c));
        painter.rect_filled(Rect::from_center_size(pos2(x, top), egui::vec2(6.0, 3.0)), 1.0, c);
    }

    // ----- interaction -----
    let resp = ui.interact(stems, ui.id().with("piano_velocity"), Sense::click_and_drag());
    // The stem nearest to a point, preferring selected notes.
    let stem_at = |p: Pos2| -> Option<usize> {
        order
            .iter()
            .rev()
            .copied()
            .filter(|&i| (view.x(notes[i].start) + 1.0 - p.x).abs() <= HIT)
            .min_by(|&a, &b| {
                let d =
                    |i: usize| (y_of(notes[i].velocity) - p.y).abs() - if selection.contains(&i) { 6.0 } else { 0.0 };
                d(a).total_cmp(&d(b))
            })
    };
    if let Some(p) = resp.hover_pos() {
        if stem_at(p).is_some() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
    }
    if resp.clicked() {
        if let Some(i) = resp.interact_pointer_pos().and_then(stem_at) {
            let shift = ui.input(|inp| inp.modifiers.shift || inp.modifiers.command);
            if shift {
                if !app.piano.selection.contains(&i) {
                    app.piano.selection.push(i);
                }
            } else {
                app.piano.selection = vec![i];
            }
        }
    }
    if resp.drag_started() {
        if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
            app.piano.drag = Some(match stem_at(p) {
                Some(i) => NoteDrag::Velocity {
                    orig: notes.clone(),
                    anchor: i,
                    targets: if selection.contains(&i) {
                        selection.clone()
                    } else {
                        vec![i]
                    },
                    grab_y: p.y,
                },
                None => NoteDrag::VelocityDraw { last_x: p.x },
            });
        }
    }
    let mut shown: Option<(Pos2, u8)> = None;
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            match app.piano.drag.clone() {
                Some(NoteDrag::Velocity {
                    orig,
                    anchor,
                    targets,
                    grab_y,
                }) => {
                    let delta = ((grab_y - p.y) / usable * 127.0).round() as i32;
                    let mut v = orig;
                    for &i in &targets {
                        if let Some(n) = v.get_mut(i) {
                            n.velocity = (n.velocity as i32 + delta).clamp(1, 127) as u8;
                        }
                    }
                    let value = v.get(anchor).map(|n| n.velocity).unwrap_or(0);
                    super::apply(app, r, false, |notes, _, _| *notes = v);
                    shown = Some((p, value));
                }
                Some(NoteDrag::VelocityDraw { last_x }) => {
                    let value = vel_at(p.y);
                    let (x0, x1) = (last_x.min(p.x) - HIT, last_x.max(p.x) + HIT);
                    let only_selected = !selection.is_empty();
                    super::apply(app, r, false, |notes, sel, _| {
                        for (i, n) in notes.iter_mut().enumerate() {
                            let x = view.x(n.start) + 1.0;
                            if x >= x0 && x <= x1 && (!only_selected || sel.contains(&i)) {
                                n.velocity = value;
                            }
                        }
                    });
                    app.piano.drag = Some(NoteDrag::VelocityDraw { last_x: p.x });
                    shown = Some((p, value));
                }
                _ => {}
            }
        }
    }
    if let Some((p, value)) = shown {
        let text_pos = pos2(
            (p.x + 10.0).min(stems.right() - 30.0),
            (p.y - 6.0).max(stems.top() + 6.0),
        );
        painter.text(
            text_pos,
            egui::Align2::LEFT_CENTER,
            value.to_string(),
            egui::FontId::proportional(11.0),
            theme::text(),
        );
    }
    if resp.drag_stopped() {
        if matches!(
            app.piano.drag,
            Some(NoteDrag::Velocity { .. } | NoteDrag::VelocityDraw { .. })
        ) {
            app.changed();
        }
        app.piano.drag = None;
    }
}
