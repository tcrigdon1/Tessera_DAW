//! Piano roll: the MIDI note editor of the clip view.
//!
//! Top to bottom: a toolbar, the ruler with the clip's markers, the note grid
//! with the keys on its left, and the velocity lane. The selection is a list
//! of indices into the clip's notes; every edit goes through
//! [`tessera_engine::midi::edit`] and keeps the notes inside the clip.

mod notes;
mod ruler;
mod toolbar;
mod velocity;

use eframe::egui::{self, pos2, Key, Modifiers, Pos2, Rect, Sense};
use tessera_engine::midi::edit::{self, Quantize};
use tessera_engine::model::{Id, Note};

use crate::app::{ClipRef, DetailMode, TesseraApp};
use crate::automation::ClipTab;

/// Row height of the keyboard.
const ROW_H: f32 = 11.0;
const RULER_H: f32 = 36.0;
const VELOCITY_H: f32 = 64.0;

/// Grid choices, in the toolbar's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GridChoice {
    /// Follows the zoom.
    #[default]
    Adaptive,
    Quarter,
    Eighth,
    Sixteenth,
    ThirtySecond,
    EighthTriplet,
    SixteenthTriplet,
}

impl GridChoice {
    pub const ALL: [GridChoice; 7] = [
        GridChoice::Adaptive,
        GridChoice::Quarter,
        GridChoice::Eighth,
        GridChoice::Sixteenth,
        GridChoice::ThirtySecond,
        GridChoice::EighthTriplet,
        GridChoice::SixteenthTriplet,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GridChoice::Adaptive => "Adaptive",
            GridChoice::Quarter => "1/4",
            GridChoice::Eighth => "1/8",
            GridChoice::Sixteenth => "1/16",
            GridChoice::ThirtySecond => "1/32",
            GridChoice::EighthTriplet => "1/8T",
            GridChoice::SixteenthTriplet => "1/16T",
        }
    }

    /// Step in beats, `None` for the adaptive grid.
    pub fn beats(self) -> Option<f64> {
        match self {
            GridChoice::Adaptive => None,
            GridChoice::Quarter => Some(1.0),
            GridChoice::Eighth => Some(0.5),
            GridChoice::Sixteenth => Some(0.25),
            GridChoice::ThirtySecond => Some(0.125),
            GridChoice::EighthTriplet => Some(1.0 / 3.0),
            GridChoice::SixteenthTriplet => Some(1.0 / 6.0),
        }
    }

    /// The next finer (`finer`) or coarser fixed grid of the same kind
    /// (straight or triplet).
    pub fn step(self, finer: bool) -> Self {
        use GridChoice::*;
        let chain: &[GridChoice] = match self {
            EighthTriplet | SixteenthTriplet => &[EighthTriplet, SixteenthTriplet],
            _ => &[Quarter, Eighth, Sixteenth, ThirtySecond],
        };
        let i = chain.iter().position(|g| *g == self).unwrap_or(0);
        let j = if finer {
            (i + 1).min(chain.len() - 1)
        } else {
            i.saturating_sub(1)
        };
        chain[j]
    }
}

/// The adaptive grid step for a zoom level: the finest musical step that
/// is at least ~18 px wide. `bias` (Ctrl+1 / Ctrl+2) makes it finer or
/// coarser.
pub fn adaptive_step(px_per_beat: f32, beats_per_bar: f64, bias: i32) -> f64 {
    let min_px = 18.0 * 2f32.powi(bias.clamp(-3, 3));
    let bar = beats_per_bar.max(1.0);
    [0.125, 0.25, 0.5, 1.0, bar, bar * 2.0, bar * 4.0, bar * 8.0]
        .into_iter()
        .find(|&s| s as f32 * px_per_beat >= min_px)
        .unwrap_or(bar * 16.0)
}

/// Piano roll state.
pub struct PianoRollState {
    /// Highest visible pitch.
    pub top_pitch: i32,
    /// Selected notes (indices into the clip's notes).
    pub selection: Vec<usize>,
    pub drag: Option<NoteDrag>,
    /// The clip the view was last fitted to.
    pub centred_on: Option<ClipRef>,
    /// Note being auditioned: (track, pitch, release time).
    pub audition: Option<(Id, u8, f64)>,
    /// Draw mode (B): click and drag on empty space paints notes.
    pub draw_mode: bool,
    pub grid: GridChoice,
    /// Ctrl+1 / Ctrl+2 on the adaptive grid.
    pub adaptive_bias: i32,
    /// Grid step in beats as last drawn (used by the keyboard shortcuts).
    pub step: f64,
    pub px_per_beat: f32,
    /// Beat at the left edge of the grid.
    pub scroll_beats: f64,
    /// Fit the whole clip into the width (until zoomed or scrolled).
    pub fit: bool,
    /// Unused part of a pitch scroll, in pixels.
    pub scroll_rest: f32,
    /// Show only rows with notes.
    pub fold: bool,
    /// Show only rows in the clip's scale.
    pub fold_to_scale: bool,
    pub show_velocity: bool,
    pub quantize: Quantize,
    /// Quantize grid in beats; `None` follows the editor's grid.
    pub quantize_grid: Option<f64>,
    pub quantize_open: bool,
    /// Copied notes, relative to the first one, and the span they cover.
    pub clipboard: Vec<Note>,
    pub clipboard_span: f64,
    /// Where Ctrl+V pastes: after the copied notes, or where empty grid
    /// space was last clicked.
    pub paste_at: f64,
}

impl Default for PianoRollState {
    fn default() -> Self {
        Self {
            top_pitch: 84,
            selection: Vec::new(),
            drag: None,
            centred_on: None,
            audition: None,
            draw_mode: false,
            grid: GridChoice::Adaptive,
            adaptive_bias: 0,
            step: 0.25,
            px_per_beat: 40.0,
            scroll_beats: 0.0,
            fit: true,
            scroll_rest: 0.0,
            fold: false,
            fold_to_scale: false,
            show_velocity: true,
            quantize: Quantize::default(),
            quantize_grid: None,
            quantize_open: false,
            clipboard: Vec::new(),
            clipboard_span: 0.0,
            paste_at: 0.0,
        }
    }
}

/// A mouse drag in progress. Note drags keep the notes as they were when the
/// drag started and recompute the edit from them every frame.
#[derive(Debug, Clone)]
pub enum NoteDrag {
    /// Moves the selection; `anchor` is the note under the pointer.
    Move {
        orig: Vec<Note>,
        anchor: usize,
        grab_beat: f64,
        grab_row: i32,
    },
    /// Changes the length of the selection by dragging `anchor`'s end.
    Resize {
        orig: Vec<Note>,
        anchor: usize,
    },
    /// Selection box from a (beat, row) point; `base` is kept (Shift).
    Box {
        from_beat: f64,
        from_row: f32,
        base: Vec<usize>,
    },
    /// Draw mode: paints notes of the grid length along one row.
    Paint {
        orig: Vec<Note>,
        pitch: u8,
        from_cell: i64,
    },
    /// Drags a velocity stem; `targets` change by the same amount.
    Velocity {
        orig: Vec<Note>,
        anchor: usize,
        targets: Vec<usize>,
        grab_y: f32,
    },
    /// Draws velocities across the lane.
    VelocityDraw {
        last_x: f32,
    },
    Marker(ruler::Marker),
    /// Scales the selection in time around its other end.
    Stretch {
        orig: Vec<Note>,
        from_end: bool,
        first: f64,
        last: f64,
    },
}

/// Geometry of the grid for one frame.
pub(super) struct View {
    /// The note area right of the keys.
    grid: Rect,
    keys: Rect,
    ppb: f32,
    scroll: f64,
    /// Every row the editor can show, top to bottom.
    all_rows: Vec<u8>,
    /// First visible row (index into `all_rows`).
    first: usize,
    /// Visible rows.
    count: usize,
    row_h: f32,
    /// Grid step in beats.
    step: f64,
    /// Clip length (loop end / end) in beats.
    len: f64,
    bpb: f64,
    is_drums: bool,
}

impl View {
    fn x(&self, beat: f64) -> f32 {
        self.grid.left() + (beat - self.scroll) as f32 * self.ppb
    }

    fn beat(&self, x: f32) -> f64 {
        self.scroll + ((x - self.grid.left()) / self.ppb) as f64
    }

    fn rows(&self) -> &[u8] {
        &self.all_rows[self.first..self.first + self.count]
    }

    fn row_y(&self, row: usize) -> f32 {
        self.grid.top() + row as f32 * self.row_h
    }

    /// Row position (in `all_rows`, fractional) at a y coordinate.
    fn global_row(&self, y: f32) -> f32 {
        self.first as f32 + (y - self.grid.top()) / self.row_h
    }

    /// Index into `all_rows` at a y coordinate, clamped to the rows.
    fn row_index_at(&self, y: f32) -> usize {
        (self.global_row(y).floor().max(0.0) as usize).min(self.all_rows.len() - 1)
    }

    /// Visible pitch at a y coordinate.
    fn pitch_at(&self, y: f32) -> Option<u8> {
        let row = ((y - self.grid.top()) / self.row_h).floor();
        if row < 0.0 {
            return None;
        }
        self.rows().get(row as usize).copied()
    }

    fn note_rect(&self, n: &Note) -> Option<Rect> {
        let row = self.rows().iter().position(|&p| p == n.pitch)?;
        let y = self.row_y(row);
        let x0 = self.x(n.start);
        Some(Rect::from_min_max(
            pos2(x0, y + 1.0),
            pos2(self.x(n.start + n.length).max(x0 + 3.0), y + self.row_h - 1.0),
        ))
    }

    /// Nearest grid line.
    fn snap(&self, beat: f64) -> f64 {
        (beat / self.step).round() * self.step
    }

    /// Grid line at or before `beat`.
    fn snap_floor(&self, beat: f64) -> f64 {
        ((beat / self.step) + 1e-9).floor() * self.step
    }

    /// Moves `pitch` by `rows` rows (down is positive), or by semitones
    /// when it is not on a row.
    fn move_rows(&self, pitch: u8, rows: i32) -> u8 {
        match self.all_rows.iter().position(|&p| p == pitch) {
            Some(i) => self.all_rows[(i as i32 + rows).clamp(0, self.all_rows.len() as i32 - 1) as usize],
            None => (pitch as i32 - rows).clamp(0, 127) as u8,
        }
    }
}

/// Draws the piano roll for a MIDI clip.
pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    ui.vertical(|ui| {
        toolbar::draw(app, ui, r);
        let area = ui.available_rect_before_wrap();
        ui.allocate_rect(area, Sense::hover());
        let Some(view) = layout(app, ui, r, area) else {
            return;
        };
        let ruler_rect = Rect::from_min_max(
            pos2(view.grid.left(), area.top()),
            pos2(area.right(), area.top() + RULER_H),
        );
        ruler::draw(app, ui, r, &view, ruler_rect, area.left());
        notes::draw(app, ui, r, &view);
        if app.piano.show_velocity {
            let lane = Rect::from_min_max(
                pos2(area.left(), area.bottom() - VELOCITY_H),
                pos2(area.right(), area.bottom()),
            );
            velocity::draw(app, ui, r, &view, lane);
        }
    });
    toolbar::quantize_window(app, ui.ctx(), r);
}

/// Works out rows, zoom and scroll for this frame, applying the mouse wheel.
fn layout(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef, area: Rect) -> Option<View> {
    let clip = app.clip(r)?;
    let len = clip.length.max(1.0 / 64.0);
    let scale = clip.scale;
    let notes: Vec<Note> = clip.notes().to_vec();
    let bpb = app.project.beats_per_bar.max(1) as f64;
    let pads = super::sample_panels::drum_rows(app, r.track());
    let is_drums = pads.is_some();
    let key_w = if is_drums { 78.0 } else { 44.0 };
    let vel_h = if app.piano.show_velocity { VELOCITY_H + 4.0 } else { 0.0 };
    let grid = Rect::from_min_max(
        pos2(area.left() + key_w, area.top() + RULER_H),
        pos2(area.right(), area.bottom() - vel_h),
    );
    if grid.height() < ROW_H || grid.width() < 20.0 {
        return None;
    }
    let st = &mut app.piano;

    if st.centred_on != Some(r) {
        st.centred_on = Some(r);
        st.fit = true;
        st.scroll_beats = 0.0;
        let top = notes.iter().map(|n| n.pitch as i32).max().unwrap_or(72) + 6;
        st.top_pitch = top.clamp(24, 127);
    }

    let used = edit::used_pitches(&notes);
    let all_rows: Vec<u8> = if let Some(pads) = &pads {
        pads.iter().map(|(p, _)| *p).collect()
    } else if st.fold && !used.is_empty() {
        used
    } else if let (true, Some(s)) = (st.fold_to_scale, scale) {
        (0..=127u8).rev().filter(|&p| s.contains(p)).collect()
    } else {
        (0..=127u8).rev().collect()
    };
    let n = all_rows.len();
    let folded = is_drums || n < 128;
    let row_h = if is_drums {
        (grid.height() / n as f32).min(24.0)
    } else if folded {
        (grid.height() / n as f32).clamp(ROW_H, 20.0)
    } else {
        ROW_H
    };
    let count = ((grid.height() / row_h).floor() as usize).clamp(1, n);

    // Mouse wheel: Ctrl/Cmd zooms time, Shift (or a horizontal wheel)
    // scrolls time, the plain wheel scrolls pitch.
    let mut first = all_rows
        .iter()
        .position(|&p| p as i32 <= st.top_pitch)
        .unwrap_or(0)
        .min(n - count);
    let visible = |ppb: f32| (grid.width() / ppb) as f64;
    if st.fit && !matches!(st.drag, Some(NoteDrag::Marker(_))) {
        st.px_per_beat = grid.width() / len as f32;
        st.scroll_beats = 0.0;
    }
    if ui.rect_contains_pointer(area) {
        let (scroll, zoom, shift, pointer) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.zoom_delta(),
                i.modifiers.shift,
                i.pointer.hover_pos(),
            )
        });
        if zoom != 1.0 {
            let px = pointer.map(|p| p.x - grid.left()).unwrap_or(0.0).max(0.0);
            let anchor = st.scroll_beats + (px / st.px_per_beat) as f64;
            st.px_per_beat = (st.px_per_beat * zoom).clamp(4.0, 600.0);
            st.scroll_beats = anchor - (px / st.px_per_beat) as f64;
            st.fit = false;
        } else {
            let (dx, dy) = if shift {
                (scroll.y + scroll.x, 0.0)
            } else {
                (scroll.x, scroll.y)
            };
            if dx != 0.0 {
                st.scroll_beats -= (dx / st.px_per_beat) as f64;
                st.fit = false;
            }
            if dy != 0.0 && !is_drums {
                st.scroll_rest += dy;
                let rows = (st.scroll_rest / row_h).trunc();
                st.scroll_rest -= rows * row_h;
                first = (first as i32 - rows as i32).clamp(0, (n - count) as i32) as usize;
            }
        }
    }
    let max_scroll = (len + bpb * 4.0 - visible(st.px_per_beat)).max(0.0);
    st.scroll_beats = st.scroll_beats.clamp(0.0, max_scroll);
    st.top_pitch = all_rows[first] as i32;
    let step = st
        .grid
        .beats()
        .unwrap_or_else(|| adaptive_step(st.px_per_beat, bpb, st.adaptive_bias));
    st.step = step;

    Some(View {
        grid,
        keys: Rect::from_min_max(pos2(area.left(), grid.top()), pos2(grid.left() - 1.0, grid.bottom())),
        ppb: st.px_per_beat,
        scroll: st.scroll_beats,
        all_rows,
        first,
        count,
        row_h,
        step,
        len,
        bpb,
        is_drums,
    })
}

/// Applies an edit to the clip's notes and the selection, then keeps the
/// selection valid. `commit` rebuilds the engine snapshot (key presses and
/// drag ends); otherwise the project is only marked dirty (during drags).
pub(super) fn apply(
    app: &mut TesseraApp,
    r: ClipRef,
    commit: bool,
    f: impl FnOnce(&mut Vec<Note>, &mut Vec<usize>, f64),
) {
    let mut sel = std::mem::take(&mut app.piano.selection);
    if let Some(clip) = app.clip_mut(r) {
        let len = clip.length;
        if let Some(notes) = clip.notes_mut() {
            f(notes, &mut sel, len);
            sel.retain(|&i| i < notes.len());
            sel.sort_unstable();
            sel.dedup();
        }
    }
    app.piano.selection = sel;
    if commit {
        app.changed();
    } else {
        app.touch();
    }
}

/// Shortens the selected notes to end by the clip end. Unlike
/// [`edit::keep_inside`] it never removes notes, so drags that recompute
/// from a snapshot keep their indices.
pub(super) fn trim(notes: &mut [Note], sel: &[usize], len: f64) {
    for &i in sel {
        if let Some(n) = notes.get_mut(i) {
            if n.start < len {
                n.length = n.length.min(len - n.start).max(edit::MIN_LENGTH);
            }
        }
    }
}

/// The clip shown in the piano roll, if the clip view shows a MIDI clip.
fn active_clip(app: &TesseraApp) -> Option<ClipRef> {
    let r = app.selected_clip?;
    (app.detail == DetailMode::Clip
        && app.automation.clip_tab == ClipTab::Main
        && app.clip(r).is_some_and(|c| c.is_midi()))
    .then_some(r)
}

/// Plays the highest selected note.
fn audition_selection(app: &mut TesseraApp, r: ClipRef, now: f64) {
    let top = app.clip(r).and_then(|c| {
        app.piano
            .selection
            .iter()
            .filter_map(|&i| c.notes().get(i))
            .map(|n| n.pitch)
            .max()
    });
    if let Some(p) = top {
        app.audition(r.track(), p, now);
    }
}

pub(super) fn quantize_selection(app: &mut TesseraApp, r: ClipRef) {
    let q = Quantize {
        grid: app.piano.quantize_grid.unwrap_or(app.piano.step),
        ..app.piano.quantize
    };
    apply(app, r, true, |notes, sel, len| {
        edit::quantize(notes, sel, &q);
        *sel = edit::keep_inside(notes, sel, len);
    });
}

pub(super) fn legato(app: &mut TesseraApp, r: ClipRef) {
    apply(app, r, true, |notes, sel, len| {
        edit::legato(notes, sel);
        *sel = edit::keep_inside(notes, sel, len);
    });
}

/// Copies the selection right after itself and selects the copies.
pub(super) fn duplicate(app: &mut TesseraApp, r: ClipRef) {
    let step = app.piano.step;
    apply(app, r, true, |notes, sel, len| {
        let all: Vec<usize> = (0..notes.len()).collect();
        let src = if sel.is_empty() { &all } else { &*sel };
        let copies = edit::duplicate(notes, src, step);
        *sel = edit::keep_inside(notes, &copies, len);
    });
}

/// Doubles the clip's loop and copies its notes into the new half.
pub(super) fn double_loop(app: &mut TesseraApp, r: ClipRef) {
    if let Some(clip) = app.clip_mut(r) {
        let (start, end) = (clip.loop_start, clip.length);
        if let Some(end) = clip.notes_mut().map(|notes| edit::double_loop(notes, start, end)) {
            clip.length = end;
        }
        app.changed();
    }
}

fn copy(app: &mut TesseraApp, r: ClipRef) -> bool {
    let Some(clip) = app.clip(r) else { return false };
    let Some((notes, span)) = edit::copy(clip.notes(), &app.piano.selection, app.piano.step) else {
        return false;
    };
    let first = app
        .piano
        .selection
        .iter()
        .filter_map(|&i| clip.notes().get(i))
        .map(|n| n.start)
        .fold(f64::INFINITY, f64::min);
    app.piano.clipboard = notes;
    app.piano.clipboard_span = span;
    app.piano.paste_at = first + span;
    true
}

fn paste(app: &mut TesseraApp, r: ClipRef) {
    let copied = app.piano.clipboard.clone();
    let at = app.piano.paste_at;
    apply(app, r, true, |notes, sel, len| {
        *sel = edit::paste(notes, &copied, at, len);
    });
    app.piano.paste_at = at + app.piano.clipboard_span;
}

/// Note editing shortcuts. Consumes the keys it uses, and only while the
/// clip view shows a MIDI clip; returns true when it handled a key.
///
/// Shortcuts with Shift are checked first: egui's `consume_key` ignores
/// extra Shift and Alt modifiers.
pub fn handle_keys(app: &mut TesseraApp, ctx: &egui::Context) -> bool {
    let Some(r) = active_clip(app) else {
        return false;
    };
    let cmd = Modifiers::COMMAND;
    let shift = Modifiers::SHIFT;
    let count = |m: Modifiers, k: Key| ctx.input_mut(|i| i.count_and_consume_key(m, k)) as i32;
    let has_sel = !app.piano.selection.is_empty();
    let step = app.piano.step;
    let now = ctx.input(|i| i.time);
    let mut handled = false;

    // Keys that need a selection are left alone without one (Delete then
    // deletes the clip, Ctrl+D duplicates it).
    if has_sel {
        let octave = count(shift, Key::ArrowUp) - count(shift, Key::ArrowDown);
        let semis = count(Modifiers::NONE, Key::ArrowUp) - count(Modifiers::NONE, Key::ArrowDown);
        let t = octave * 12 + semis;
        if t != 0 {
            apply(app, r, true, |notes, sel, _| {
                edit::transpose(notes, sel, t);
            });
            audition_selection(app, r, now);
            handled = true;
        }
        let grow = count(shift, Key::ArrowRight) - count(shift, Key::ArrowLeft);
        if grow != 0 {
            apply(app, r, true, |notes, sel, len| {
                edit::resize(notes, sel, grow as f64 * step);
                *sel = edit::keep_inside(notes, sel, len);
            });
            handled = true;
        }
        let moved = count(Modifiers::NONE, Key::ArrowRight) - count(Modifiers::NONE, Key::ArrowLeft);
        if moved != 0 {
            apply(app, r, true, |notes, sel, len| {
                edit::shift(notes, sel, moved as f64 * step, len);
            });
            handled = true;
        }
        if count(Modifiers::NONE, Key::Delete) + count(Modifiers::NONE, Key::Backspace) > 0 {
            apply(app, r, true, |notes, sel, _| {
                edit::delete(notes, sel);
                sel.clear();
            });
            handled = true;
        }
        if count(cmd, Key::D) > 0 {
            duplicate(app, r);
            handled = true;
        }
        if count(cmd, Key::C) > 0 {
            copy(app, r);
            handled = true;
        }
        if count(cmd, Key::X) > 0 {
            if copy(app, r) {
                apply(app, r, true, |notes, sel, _| {
                    edit::delete(notes, sel);
                    sel.clear();
                });
            }
            handled = true;
        }
        if count(Modifiers::NONE, Key::Escape) > 0 {
            app.piano.selection.clear();
            handled = true;
        }
    }
    if !app.piano.clipboard.is_empty() && count(cmd, Key::V) > 0 {
        paste(app, r);
        handled = true;
    }
    if count(cmd, Key::A) > 0 {
        let n = app.clip(r).map(|c| c.notes().len()).unwrap_or(0);
        app.piano.selection = (0..n).collect();
        handled = true;
    }
    if count(cmd, Key::J) > 0 {
        legato(app, r);
        handled = true;
    }
    if count(cmd | shift, Key::U) > 0 {
        app.piano.quantize_open = true;
        handled = true;
    }
    if count(cmd, Key::U) > 0 {
        quantize_selection(app, r);
        handled = true;
    }
    // Ctrl+1 narrows the grid, Ctrl+2 widens it.
    for (key, finer) in [(Key::Num1, true), (Key::Num2, false)] {
        if count(cmd, key) > 0 {
            let p = &mut app.piano;
            if p.grid == GridChoice::Adaptive {
                p.adaptive_bias = (p.adaptive_bias + if finer { -1 } else { 1 }).clamp(-3, 3);
            } else {
                p.grid = p.grid.step(finer);
            }
            handled = true;
        }
    }
    if count(Modifiers::NONE, Key::B) > 0 {
        app.piano.draw_mode = !app.piano.draw_mode;
        handled = true;
    }
    handled
}

/// Whether a point lies within `d` pixels of `x` horizontally.
fn near(p: Pos2, x: f32, d: f32) -> bool {
    (p.x - x).abs() <= d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_choices_step_within_their_kind() {
        assert_eq!(GridChoice::Sixteenth.step(true), GridChoice::ThirtySecond);
        assert_eq!(GridChoice::ThirtySecond.step(true), GridChoice::ThirtySecond);
        assert_eq!(GridChoice::Eighth.step(false), GridChoice::Quarter);
        assert_eq!(GridChoice::Quarter.step(false), GridChoice::Quarter);
        assert_eq!(GridChoice::EighthTriplet.step(true), GridChoice::SixteenthTriplet);
        assert_eq!(GridChoice::Adaptive.beats(), None);
        assert!((GridChoice::SixteenthTriplet.beats().unwrap() * 6.0 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn adaptive_grid_follows_zoom() {
        assert_eq!(adaptive_step(80.0, 4.0, 0), 0.25);
        assert_eq!(adaptive_step(20.0, 4.0, 0), 1.0);
        assert_eq!(adaptive_step(5.0, 4.0, 0), 4.0);
        assert_eq!(adaptive_step(80.0, 4.0, -1), 0.125);
        assert_eq!(adaptive_step(80.0, 4.0, 1), 0.5);
        assert_eq!(adaptive_step(10.0, 3.0, 0), 3.0, "bars of 3/4");
    }
}
