//! The ruler above the note grid. Three strips, top to bottom: bar numbers
//! (with note-stretch handles when two or more notes are selected), the loop
//! brace, and the Start marker (plus the End marker when the clip does not
//! loop). Markers snap to the grid; Alt drags freely.

use eframe::egui::{self, pos2, Color32, CursorIcon, Pos2, Rect, Sense, Shape, Stroke};
use tessera_engine::midi::edit;

use super::super::theme;
use super::{near, NoteDrag, View};
use crate::app::{ClipRef, TesseraApp};

const STRIP_A: f32 = 13.0;
const STRIP_B: f32 = 24.0;
const HIT: f32 = 5.0;

/// A marker being dragged.
#[derive(Debug, Clone, Copy)]
pub enum Marker {
    Start,
    LoopStart,
    LoopEnd,
    /// Moves the whole loop; `grab` is the pointer's offset from its start.
    LoopMove {
        grab: f64,
    },
    /// End of a clip that does not loop.
    End,
}

pub(super) fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef, view: &View, rect: Rect, left: f32) {
    let Some(clip) = app.clip(r) else { return };
    let (start, loop_start, len, looping) = (clip.start_marker, clip.loop_start, clip.length, clip.looping);
    let notes_now = clip.notes().to_vec();
    let bounds = selection_bounds(app, r);
    let painter = ui.painter_at(Rect::from_min_max(pos2(left, rect.top()), rect.max));
    painter.rect_filled(
        Rect::from_min_max(pos2(left, rect.top()), pos2(rect.left(), rect.bottom())),
        0.0,
        theme::BG_BAR,
    );
    painter.rect_filled(rect, 0.0, Color32::from_rgb(0x30, 0x30, 0x30));
    let a = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + STRIP_A));
    let b = Rect::from_min_max(pos2(rect.left(), a.bottom()), pos2(rect.right(), rect.top() + STRIP_B));
    let c = Rect::from_min_max(pos2(rect.left(), b.bottom()), rect.max);
    painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, Color32::BLACK));

    // Bar numbers, thinned out when bars get narrow, and beat ticks.
    let bpb = view.bpb;
    let bar_px = bpb as f32 * view.ppb;
    let every = [1, 2, 4, 8, 16, 32]
        .into_iter()
        .find(|k| bar_px * *k as f32 >= 30.0)
        .unwrap_or(64);
    let first_bar = (view.scroll / bpb).floor() as i64;
    let last_bar = (view.beat(rect.right()) / bpb).ceil() as i64;
    let font = egui::FontId::proportional(10.0);
    for bar in first_bar.max(0)..=last_bar {
        let x = view.x(bar as f64 * bpb);
        if bar % every == 0 {
            painter.vline(x, a.y_range(), Stroke::new(1.0, theme::GRID_BAR));
            painter.text(
                pos2(x + 3.0, a.top() + 1.0),
                egui::Align2::LEFT_TOP,
                format!("{}", bar + 1),
                font.clone(),
                theme::TEXT,
            );
        }
        if view.ppb >= 8.0 {
            for beat in 1..bpb as i64 {
                let bx = view.x(bar as f64 * bpb + beat as f64);
                painter.vline(bx, (a.bottom() - 4.0)..=a.bottom(), Stroke::new(1.0, theme::GRID_BAR));
                if view.ppb >= 44.0 {
                    painter.text(
                        pos2(bx + 2.0, a.top() + 1.0),
                        egui::Align2::LEFT_TOP,
                        format!("{}.{}", bar + 1, beat + 1),
                        font.clone(),
                        theme::TEXT_DIM,
                    );
                }
            }
        }
    }

    // Loop brace.
    let (xl, xe) = (view.x(loop_start), view.x(len));
    if looping {
        let brace = Rect::from_min_max(pos2(xl, b.top() + 1.0), pos2(xe, b.bottom() - 1.0));
        let hot = matches!(
            app.piano.drag,
            Some(NoteDrag::Marker(
                Marker::LoopMove { .. } | Marker::LoopStart | Marker::LoopEnd
            ))
        );
        painter.rect_filled(
            brace,
            1.0,
            if hot {
                Color32::from_rgb(0xa0, 0xa0, 0xa0)
            } else {
                Color32::from_rgb(0x84, 0x84, 0x84)
            },
        );
        for x in [xl, xe] {
            painter.vline(x, b.y_range(), Stroke::new(2.0, Color32::from_rgb(0xd0, 0xd0, 0xd0)));
        }
    } else {
        flag(&painter, view.x(len), c, false, theme::TEXT);
    }
    flag(&painter, view.x(start), c, true, theme::TEXT);

    // Stretch handles.
    if let Some((first, last)) = bounds {
        for x in [view.x(first), view.x(last)] {
            painter.add(Shape::convex_polygon(
                vec![
                    pos2(x - 4.0, a.bottom() - 6.0),
                    pos2(x + 4.0, a.bottom() - 6.0),
                    pos2(x, a.bottom()),
                ],
                theme::ACCENT,
                Stroke::NONE,
            ));
        }
    }

    // ----- interaction -----
    let resp = ui.interact(rect, ui.id().with("piano_ruler"), Sense::click_and_drag());
    let target = |p: Pos2| -> Option<NoteDrag> {
        if p.y < a.bottom() {
            let (first, last) = bounds?;
            let from_end = if near(p, view.x(last), HIT) {
                true
            } else if near(p, view.x(first), HIT) {
                false
            } else {
                return None;
            };
            let orig = notes_now.clone();
            return Some(NoteDrag::Stretch {
                orig,
                from_end,
                first,
                last,
            });
        }
        let m = if p.y < b.bottom() {
            if !looping {
                return None;
            }
            if near(p, xe, HIT) {
                Marker::LoopEnd
            } else if near(p, xl, HIT) {
                Marker::LoopStart
            } else if p.x > xl && p.x < xe {
                Marker::LoopMove {
                    grab: view.beat(p.x) - loop_start,
                }
            } else {
                return None;
            }
        } else {
            let xs = view.x(start);
            if p.x >= xs - 3.0 && p.x <= xs + 10.0 {
                Marker::Start
            } else if !looping && p.x >= xe - 10.0 && p.x <= xe + 3.0 {
                Marker::End
            } else {
                return None;
            }
        };
        Some(NoteDrag::Marker(m))
    };
    if let Some(p) = resp.hover_pos() {
        match target(p) {
            Some(NoteDrag::Marker(Marker::LoopMove { .. })) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
            Some(_) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            None => {}
        }
    }
    if resp.double_clicked() {
        app.piano.fit = true;
    }
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin());
        app.piano.drag = origin.and_then(target);
    }
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            let alt = ui.input(|i| i.modifiers.alt);
            drag_to(app, r, view, p, alt);
        }
    }
    if resp.drag_stopped() {
        if matches!(app.piano.drag, Some(NoteDrag::Marker(_) | NoteDrag::Stretch { .. })) {
            app.changed();
        }
        // Keep the zoom after moving the end, so the view does not jump.
        if matches!(app.piano.drag, Some(NoteDrag::Marker(_))) {
            app.piano.fit = false;
        }
        app.piano.drag = None;
    }
}

fn drag_to(app: &mut TesseraApp, r: ClipRef, view: &View, p: Pos2, alt: bool) {
    let beat = view.beat(p.x);
    let snap = |b: f64| if alt { b } else { view.snap(b) };
    // Markers keep at least one grid step (or 1/64 beat off the grid) apart.
    let gap = if alt { 1.0 / 64.0 } else { view.step };
    match app.piano.drag.clone() {
        Some(NoteDrag::Marker(m)) => {
            let Some(c) = app.clip_mut(r) else { return };
            match m {
                Marker::Start => c.start_marker = snap(beat).clamp(0.0, (c.length - gap).max(0.0)),
                Marker::LoopStart => c.loop_start = snap(beat).clamp(0.0, (c.length - gap).max(0.0)),
                Marker::LoopEnd | Marker::End => {
                    c.length = snap(beat).max(c.loop_start.max(c.start_marker) + gap);
                }
                Marker::LoopMove { grab } => {
                    let span = c.length - c.loop_start;
                    c.loop_start = snap(beat - grab).max(0.0);
                    c.length = c.loop_start + span;
                    c.start_marker = c.start_marker.min(c.length - gap).max(0.0);
                }
            }
            app.touch();
        }
        Some(NoteDrag::Stretch {
            orig,
            from_end,
            first,
            last,
        }) => {
            let span = (last - first).max(1e-9);
            let edge = snap(beat);
            let (anchor, factor) = if from_end {
                let end = edge.clamp(first + gap, view.len);
                (first, (end - first) / span)
            } else {
                let s = edge.clamp(0.0, last - gap);
                (last, (last - s) / span)
            };
            super::apply(app, r, false, |notes, sel, len| {
                *notes = orig;
                edit::stretch(notes, sel, anchor, factor);
                super::trim(notes, sel, len);
            });
        }
        _ => {}
    }
}

/// Time span of the selection when it has at least two notes.
fn selection_bounds(app: &TesseraApp, r: ClipRef) -> Option<(f64, f64)> {
    if app.piano.selection.len() < 2 {
        return None;
    }
    let (first, last) = edit::bounds(app.clip(r)?.notes(), &app.piano.selection)?;
    (last - first > 1e-9).then_some((first, last))
}

/// A marker flag in the bottom strip, pointing right (Start) or left (End).
fn flag(p: &egui::Painter, x: f32, strip: Rect, right: bool, color: Color32) {
    let d = if right { 9.0 } else { -9.0 };
    p.add(Shape::convex_polygon(
        if right {
            vec![
                pos2(x, strip.top() + 1.0),
                pos2(x + d, strip.center().y),
                pos2(x, strip.bottom() - 1.0),
            ]
        } else {
            vec![
                pos2(x, strip.top() + 1.0),
                pos2(x, strip.bottom() - 1.0),
                pos2(x + d, strip.center().y),
            ]
        },
        color,
        Stroke::NONE,
    ));
}
