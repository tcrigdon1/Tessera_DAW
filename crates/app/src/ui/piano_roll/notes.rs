//! The keys and the note grid: drawing, selection, drawing notes, moving,
//! resizing and copy-dragging.

use std::sync::atomic::Ordering;

use eframe::egui::{self, pos2, Color32, CursorIcon, Pos2, Rect, Sense, Stroke, StrokeKind};
use tessera_engine::midi::edit;
use tessera_engine::model::Note;

use super::super::{session, theme};
use super::{NoteDrag, View};
use crate::app::{ClipRef, TesseraApp};

/// Pixels from a note's right edge that grab it for resizing.
const EDGE: f32 = 5.0;

pub(super) fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef, view: &View) {
    let Some(clip) = app.clip(r) else { return };
    let color = theme::rgb(clip.color);
    let scale = clip.scale;
    let (loop_start, looping) = (clip.loop_start, clip.looping);
    let notes: Vec<Note> = clip.notes().to_vec();
    let track = r.track();
    let pads = if view.is_drums {
        super::super::sample_panels::drum_rows(app, track)
    } else {
        None
    };
    let grid = view.grid;
    let rows_bottom = view.row_y(view.count);
    let painter = ui.painter_at(Rect::from_min_max(view.keys.min, grid.max));
    painter.rect_filled(grid, 0.0, theme::BG_VIEW);

    // Rows and keys.
    let now = ui.input(|i| i.time);
    for (i, &pitch) in view.rows().iter().enumerate() {
        let y = view.row_y(i);
        let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
        let row = Rect::from_min_max(pos2(grid.left(), y), pos2(grid.right(), y + view.row_h));
        let in_scale = scale.is_none_or(|s| s.contains(pitch));
        let mut fill = if view.is_drums || !black {
            Color32::from_rgb(0x2e, 0x2e, 0x2e)
        } else {
            Color32::from_rgb(0x28, 0x28, 0x28)
        };
        if !in_scale {
            fill = Color32::from_rgb(0x21, 0x21, 0x21);
        } else if scale.is_some_and(|s| s.is_root(pitch)) {
            fill = blend(fill, color, 0.18);
        }
        painter.rect_filled(row, 0.0, fill);
        let line = if pitch % 12 == 0 && !view.is_drums {
            Color32::from_rgb(0x1a, 0x1a, 0x1a)
        } else {
            Color32::from_rgb(0x24, 0x24, 0x24)
        };
        painter.hline(grid.x_range(), y + view.row_h, Stroke::new(1.0, line));

        let key = Rect::from_min_max(pos2(view.keys.left(), y), pos2(view.keys.right(), y + view.row_h));
        let label = match &pads {
            Some(pads) => pads
                .iter()
                .find(|(p, _)| *p == pitch)
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
            None if pitch % 12 == 0 || view.all_rows.len() < 128 && view.row_h > ROW_LABEL_H => {
                tessera_engine::midi::note_name(pitch)
            }
            None => String::new(),
        };
        let key_fill = if !view.is_drums && black {
            Color32::from_rgb(0x20, 0x20, 0x20)
        } else {
            Color32::from_rgb(0xc8, 0xc8, 0xc8)
        };
        painter.rect_filled(key, 0.0, key_fill);
        painter.hline(
            key.x_range(),
            key.bottom(),
            Stroke::new(1.0, Color32::from_rgb(0x60, 0x60, 0x60)),
        );
        // Scale degrees: a tick on the key's edge, wider for the root.
        if let Some(s) = scale {
            if s.contains(pitch) {
                let w = if s.is_root(pitch) { 5.0 } else { 2.0 };
                painter.rect_filled(
                    Rect::from_min_max(
                        pos2(key.right() - w, key.top() + 1.0),
                        pos2(key.right(), key.bottom() - 1.0),
                    ),
                    0.0,
                    theme::dim(color, if s.is_root(pitch) { 1.0 } else { 0.8 }),
                );
            }
        }
        if !label.is_empty() {
            painter.text(
                pos2(key.left() + 3.0, key.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(view.row_h.clamp(8.0, 11.0)),
                if !view.is_drums && black {
                    theme::TEXT
                } else {
                    Color32::BLACK
                },
            );
        }
        if ui.interact(key, ui.id().with(("key", pitch)), Sense::click()).clicked() {
            app.audition(track, pitch, now);
        }
    }

    // Beat grid: grid steps, beats and bars.
    let step = view.step;
    let k0 = (view.scroll / step).floor() as i64;
    let k1 = (view.beat(grid.right()) / step).ceil() as i64;
    for k in k0.max(0)..=k1 {
        let b = k as f64 * step;
        let on = |unit: f64| ((b / unit) - (b / unit).round()).abs() < 1e-6;
        let c = if on(view.bpb) {
            theme::GRID_BAR
        } else if on(1.0) {
            Color32::from_rgb(0x42, 0x42, 0x42)
        } else {
            Color32::from_rgb(0x36, 0x36, 0x36)
        };
        painter.vline(view.x(b), grid.top()..=rows_bottom, Stroke::new(1.0, c));
    }

    // Notes, shaded by velocity.
    let selection = app.piano.selection.clone();
    for (i, n) in notes.iter().enumerate() {
        let Some(nr) = view.note_rect(n) else { continue };
        if nr.right() < grid.left() || nr.left() > grid.right() {
            continue;
        }
        let selected = selection.contains(&i);
        let shade = 0.55 + 0.45 * (n.velocity as f32 / 127.0);
        let fill = if selected {
            blend(theme::dim(color, shade), Color32::WHITE, 0.45)
        } else {
            theme::dim(color, shade)
        };
        painter.rect(
            nr,
            1.0,
            fill,
            Stroke::new(1.0, if selected { theme::SELECT } else { Color32::BLACK }),
            StrokeKind::Inside,
        );
    }

    // Outside the loop (or after the end) is dimmed.
    let shade = Color32::from_black_alpha(110);
    let end_x = view.x(view.len).max(grid.left());
    if end_x < grid.right() {
        painter.rect_filled(Rect::from_min_max(pos2(end_x, grid.top()), grid.max), 0.0, shade);
    }
    if looping && loop_start > 0.0 {
        let x = view.x(loop_start).min(grid.right());
        if x > grid.left() {
            painter.rect_filled(Rect::from_min_max(grid.min, pos2(x, grid.bottom())), 0.0, shade);
        }
    }

    // Playhead for a playing Session clip.
    if let ClipRef::Slot { track, scene } = r {
        if let Some(ti) = app.project.track_index(track) {
            if let Some(s) = session::track_state(app, ti, track) {
                if s.playing_slot.load(Ordering::Relaxed) == scene as i32 {
                    let x = view.x(s.clip_position.load());
                    if grid.x_range().contains(x) {
                        painter.vline(x, grid.y_range(), Stroke::new(1.0, theme::SELECT));
                    }
                }
            }
        }
    }

    interact(app, ui, r, view, &notes, &painter);
}

const ROW_LABEL_H: f32 = 9.0;

fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

fn interact(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef, view: &View, notes: &[Note], painter: &egui::Painter) {
    let grid = view.grid;
    let resp = ui.interact(grid, ui.id().with("piano_grid"), Sense::click_and_drag());
    let track = r.track();
    let now = ui.input(|i| i.time);
    let m = ui.input(|i| i.modifiers);
    let draw_mode = app.piano.draw_mode;
    // Topmost note under a point (later notes are drawn on top).
    let hit = |p: Pos2| -> Option<usize> {
        notes
            .iter()
            .enumerate()
            .rev()
            .find(|(_, n)| view.note_rect(n).is_some_and(|r| r.contains(p)))
            .map(|(i, _)| i)
    };
    let on_edge = |p: Pos2, i: usize| {
        view.note_rect(&notes[i])
            .is_some_and(|nr| nr.width() > 2.0 * EDGE && (p.x - nr.right()).abs() < EDGE)
    };
    let in_clip = |b: f64| b >= 0.0 && b < view.len - 1e-9;

    if let Some(p) = resp.hover_pos() {
        match hit(p) {
            Some(i) if on_edge(p, i) => ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal),
            None if draw_mode => ui.ctx().set_cursor_icon(CursorIcon::Crosshair),
            _ => {}
        }
    }

    let new_note = |pitch: u8, start: f64| Note {
        pitch,
        velocity: 100,
        start,
        length: view.step.min(view.len - start),
    };

    // ----- clicks -----
    let pos = resp.interact_pointer_pos();
    if resp.double_clicked() && !draw_mode {
        // Edit mode: double-click adds or deletes a note.
        if let Some(p) = pos {
            match hit(p) {
                Some(i) => super::apply(app, r, true, |v, sel, _| {
                    v.remove(i);
                    sel.clear();
                }),
                None => {
                    let start = view.snap_floor(view.beat(p.x));
                    if let (Some(pitch), true) = (view.pitch_at(p.y), in_clip(start)) {
                        let n = new_note(pitch, start);
                        super::apply(app, r, true, |v, sel, _| {
                            v.push(n);
                            *sel = vec![v.len() - 1];
                        });
                        app.audition(track, pitch, now);
                    }
                }
            }
        }
    } else if resp.secondary_clicked() {
        if let Some(i) = pos.and_then(hit) {
            super::apply(app, r, true, |v, sel, _| {
                v.remove(i);
                sel.clear();
            });
        }
    } else if resp.clicked() {
        if let Some(p) = pos {
            match hit(p) {
                Some(i) if draw_mode => super::apply(app, r, true, |v, sel, _| {
                    v.remove(i);
                    sel.clear();
                }),
                Some(i) => {
                    let sel = &mut app.piano.selection;
                    if m.shift || m.command {
                        if let Some(k) = sel.iter().position(|&s| s == i) {
                            sel.remove(k);
                        } else {
                            sel.push(i);
                        }
                    } else {
                        *sel = vec![i];
                    }
                    app.audition(track, notes[i].pitch, now);
                }
                None => {
                    let start = view.snap_floor(view.beat(p.x));
                    app.piano.paste_at = start.max(0.0);
                    if draw_mode {
                        if let (Some(pitch), true) = (view.pitch_at(p.y), in_clip(start)) {
                            let n = new_note(pitch, start);
                            super::apply(app, r, true, |v, sel, _| {
                                v.push(n);
                                *sel = vec![v.len() - 1];
                            });
                            app.audition(track, pitch, now);
                        }
                    } else if !(m.shift || m.command) {
                        app.piano.selection.clear();
                    }
                }
            }
        }
    }

    // ----- drags -----
    if resp.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).or(pos);
        if let Some(p) = origin {
            app.piano.drag = start_drag(app, r, view, notes, p, m, hit(p), hit(p).is_some_and(|i| on_edge(p, i)));
            if let Some(NoteDrag::Paint { pitch, .. }) = app.piano.drag {
                app.audition(track, pitch, now);
            }
        }
    }
    if resp.dragged() {
        if let Some(p) = pos {
            drag_to(app, r, view, p, m.alt, now);
        }
    }
    if let Some(NoteDrag::Box {
        from_beat, from_row, ..
    }) = &app.piano.drag
    {
        if let Some(p) = pos {
            let y = grid.top() + (from_row - view.first as f32) * view.row_h;
            let rect = Rect::from_two_pos(pos2(view.x(*from_beat), y), p);
            painter.rect(
                rect,
                0.0,
                Color32::from_white_alpha(18),
                Stroke::new(1.0, theme::SELECT),
                StrokeKind::Inside,
            );
        }
    }
    if resp.drag_stopped() {
        let commit = !matches!(app.piano.drag, Some(NoteDrag::Box { .. }));
        app.piano.drag = None;
        if commit {
            app.changed();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn start_drag(
    app: &mut TesseraApp,
    r: ClipRef,
    view: &View,
    notes: &[Note],
    p: Pos2,
    m: egui::Modifiers,
    hit: Option<usize>,
    edge: bool,
) -> Option<NoteDrag> {
    let sel = &mut app.piano.selection;
    match hit {
        Some(i) => {
            if !sel.contains(&i) {
                if m.shift {
                    sel.push(i);
                } else {
                    *sel = vec![i];
                }
            }
            if edge {
                return Some(NoteDrag::Resize {
                    orig: notes.to_vec(),
                    anchor: i,
                });
            }
            let mut orig = notes.to_vec();
            let mut anchor = i;
            // Alt or Ctrl/Cmd + drag moves a copy; the originals stay.
            if m.alt || m.command {
                let copies: Vec<usize> = sel
                    .iter()
                    .filter(|&&s| s < notes.len())
                    .map(|&s| {
                        orig.push(notes[s]);
                        if s == i {
                            anchor = orig.len() - 1;
                        }
                        orig.len() - 1
                    })
                    .collect();
                *sel = copies;
                let snapshot = orig.clone();
                super::apply(app, r, false, |v, _, _| *v = snapshot);
            }
            Some(NoteDrag::Move {
                orig,
                anchor,
                grab_beat: view.beat(p.x),
                grab_row: view.row_index_at(p.y) as i32,
            })
        }
        None if app.piano.draw_mode => {
            let pitch = view.pitch_at(p.y)?;
            Some(NoteDrag::Paint {
                orig: notes.to_vec(),
                pitch,
                from_cell: (view.beat(p.x) / view.step + 1e-9).floor() as i64,
            })
        }
        None => {
            let base = if m.shift || m.command { sel.clone() } else { Vec::new() };
            Some(NoteDrag::Box {
                from_beat: view.beat(p.x),
                from_row: view.global_row(p.y),
                base,
            })
        }
    }
}

fn drag_to(app: &mut TesseraApp, r: ClipRef, view: &View, p: Pos2, alt: bool, now: f64) {
    let Some(drag) = app.piano.drag.clone() else { return };
    match drag {
        NoteDrag::Move {
            orig,
            anchor,
            grab_beat,
            grab_row,
        } => {
            let Some(a) = orig.get(anchor).copied() else { return };
            let raw = view.beat(p.x) - grab_beat;
            let delta = if alt { raw } else { view.snap(a.start + raw) - a.start };
            let sel = app.piano.selection.clone();
            // Rows moved, limited so every selected note stays on a row.
            let n = view.all_rows.len() as i32;
            let mut rows = view.row_index_at(p.y) as i32 - grab_row;
            for &i in &sel {
                if let Some(k) = view.all_rows.iter().position(|&q| q == orig[i].pitch) {
                    rows = rows.clamp(-(k as i32), n - 1 - k as i32);
                }
            }
            let before = app.clip(r).and_then(|c| c.notes().get(anchor)).map(|n| n.pitch);
            let mut moved = orig.clone();
            for &i in &sel {
                moved[i].pitch = view.move_rows(orig[i].pitch, rows);
            }
            edit::shift(&mut moved, &sel, delta, view.len);
            let pitch = moved[anchor].pitch;
            super::apply(app, r, false, |v, _, _| *v = moved);
            if before != Some(pitch) {
                app.audition(r.track(), pitch, now);
            }
        }
        NoteDrag::Resize { orig, anchor } => {
            let Some(a) = orig.get(anchor).copied() else { return };
            let beat = view.beat(p.x);
            let end = if alt {
                beat.max(a.start + edit::MIN_LENGTH)
            } else {
                view.snap(beat).max(a.start + view.step.min(a.length))
            };
            let delta = end.min(view.len) - (a.start + a.length);
            super::apply(app, r, false, |v, sel, len| {
                *v = orig;
                edit::resize(v, sel, delta);
                super::trim(v, sel, len);
            });
        }
        NoteDrag::Box {
            from_beat,
            from_row,
            base,
        } => {
            let to_row = view.global_row(p.y);
            let n = view.all_rows.len() as f32;
            let r0 = from_row.min(to_row).floor().clamp(0.0, n - 1.0) as usize;
            let r1 = from_row.max(to_row).floor().clamp(0.0, n - 1.0) as usize;
            let pitches = &view.all_rows[r0..=r1];
            let (lo, hi) = (
                pitches.iter().copied().min().unwrap_or(0),
                pitches.iter().copied().max().unwrap_or(127),
            );
            let Some(clip) = app.clip(r) else { return };
            let mut sel = base;
            for i in edit::in_box(clip.notes(), from_beat, view.beat(p.x), lo, hi) {
                if pitches.contains(&clip.notes()[i].pitch) && !sel.contains(&i) {
                    sel.push(i);
                }
            }
            app.piano.selection = sel;
        }
        NoteDrag::Paint { orig, pitch, from_cell } => {
            let cell = (view.beat(p.x) / view.step + 1e-9).floor() as i64;
            let mut v = orig.clone();
            let mut painted = Vec::new();
            for k in from_cell.min(cell)..=from_cell.max(cell) {
                let start = k as f64 * view.step;
                let taken = orig
                    .iter()
                    .any(|n| n.pitch == pitch && n.start <= start + 1e-9 && n.start + n.length > start + 1e-9);
                if start < 0.0 || start >= view.len - 1e-9 || taken {
                    continue;
                }
                v.push(Note {
                    pitch,
                    velocity: 100,
                    start,
                    length: view.step.min(view.len - start),
                });
                painted.push(v.len() - 1);
            }
            super::apply(app, r, false, |notes, sel, _| {
                *notes = v;
                *sel = painted;
            });
        }
        _ => {}
    }
}
