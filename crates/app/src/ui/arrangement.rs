//! Arrangement view: a timeline of clips per track, with group folding,
//! take lanes and comping, automation lanes, time selections, locators,
//! time signature changes and the Song Tempo lane.

mod clips;
mod ruler;

pub use clips::{audio_to_midi_menu, ConvertPick};

use eframe::egui::{self, pos2, vec2, Color32, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, UiBuilder};
use tessera_engine::arrange;
use tessera_engine::audio_to_midi::ConvertMode;
use tessera_engine::automation::{AutomationLane, AutomationTarget, ParamScale, ParamSpec};
use tessera_engine::model::{Id, TrackKind};
use tessera_engine::timeline::TimeMap;
use tessera_engine::Command;

use super::info_view::{self, InfoExt};
use super::{envelope, theme, widgets};
use crate::app::{ArrDrag, ClipRef, DetailMode, TesseraApp};
use crate::browser::{BrowserItem, DropTarget};
use crate::workflow::{ClipDrag, TimeSelection};

const HEAD_W: f32 = 176.0;
const LOOP_H: f32 = 12.0;
const RULER_H: f32 = 26.0;
/// Strip under the ruler holding the time signature markers.
const SIG_H: f32 = 14.0;
const LANE_H: f32 = 62.0;
/// Height of a take lane below its track.
const TAKE_H: f32 = 30.0;
/// Height of an automation lane below its track.
const AUTO_H: f32 = 44.0;
/// Height of the Song Tempo lane pinned at the bottom.
const TEMPO_H: f32 = 72.0;
const EDGE: f32 = 6.0;
/// Header indent per group level.
const INDENT: f32 = 10.0;
/// Lane tint of a frozen track.
const FROZEN_TINT: Color32 = Color32::from_rgba_premultiplied(0x14, 0x24, 0x38, 0x40);
/// Translucent highlight of the time selection.
const SELECTION_FILL: Color32 = Color32::from_rgba_premultiplied(0x30, 0x30, 0x30, 0x30);
const SIGNATURE_PRESETS: [(u32, u32); 8] = [(2, 4), (3, 4), (4, 4), (5, 4), (6, 8), (7, 8), (9, 8), (12, 8)];

enum Action {
    Select(ClipRef),
    SelectTrack(Id),
    ToggleTrackSelection(Id),
    /// Selects the track unless it is already part of the multi-selection
    /// (before a header menu command).
    EnsureSelected(Id),
    SetPosition(f64),
    Create(Id, f64),
    Delete(ClipRef),
    Mixer(Id),
    Changed,
    Loop,
    ToggleLanes(Id),
    AddLane(Id, AutomationTarget),
    /// An automation lane's envelope was edited in place.
    LaneEdited,
    RemoveLane(Id, usize),
    Drop(BrowserItem, DropTarget),
    // Tracks.
    ToggleFold(Id),
    Rename(Id, String),
    Group,
    Ungroup(Id),
    Duplicate,
    DeleteTracks,
    Freeze(Id),
    Unfreeze(Id),
    Flatten(Id),
    ToggleTakes(Id),
    DeleteTake(Id, Id),
    Comp(Id, Id, f64, f64),
    // Song time.
    AddLocator(f64),
    RenameLocator(Id, String),
    DeleteLocator(Id),
    AddSignature(f64, u32, u32),
    DeleteSignature(usize),
    ClearSelection,
    // Clips.
    Split(ClipRef),
    Consolidate(ClipRef),
    Reverse(ClipRef),
    Crossfade(Id, Id),
    SetFades(Id, Id, f64, f64),
    Convert(ClipRef, ConvertMode),
    Slice(ClipRef, Option<f64>),
    ExtractGroove(ClipRef),
    ToSession(Id, Id),
    DropSlot {
        track: Id,
        scene: usize,
        target: Id,
        beat: f64,
    },
}

/// Drags handled by this view beyond [`ArrDrag`]; kept in egui memory.
#[derive(Clone, Copy, Debug)]
enum LocalDrag {
    /// Time selection on a lane (or a take lane, for comping).
    Select {
        track: Id,
        take: Option<Id>,
        anchor: f64,
    },
    FadeIn {
        track: Id,
        id: Id,
    },
    FadeOut {
        track: Id,
        id: Id,
    },
}

fn local_drag_id() -> egui::Id {
    egui::Id::new("tessera_arr_local_drag")
}

fn get_local_drag(ctx: &egui::Context) -> Option<LocalDrag> {
    ctx.data(|d| d.get_temp(local_drag_id()))
}

fn set_local_drag(ctx: &egui::Context, drag: Option<LocalDrag>) {
    ctx.data_mut(|d| match drag {
        Some(v) => {
            d.insert_temp(local_drag_id(), v);
        }
        None => d.remove::<LocalDrag>(local_drag_id()),
    });
}

/// Start of a clip being moved, to put it back when it is dragged out.
fn move_origin_id() -> egui::Id {
    egui::Id::new("tessera_arr_move_origin")
}

/// Vertical layout of one track and the rows below it.
struct Row {
    /// Offset from the top of the track area (unscrolled).
    y: f32,
    h: f32,
    visible: bool,
    takes: usize,
    auto: bool,
    depth: usize,
}

fn layout(app: &TesseraApp) -> Vec<Row> {
    let p = &app.project;
    let mut y = 0.0;
    p.tracks
        .iter()
        .map(|t| {
            let visible = !arrange::hidden(p, t.id);
            let takes = if t.show_takes && t.kind != TrackKind::Group {
                t.takes.len()
            } else {
                0
            };
            let auto = app.automation.expanded.contains(&t.id);
            let h = if visible {
                LANE_H
                    + takes as f32 * TAKE_H
                    + if auto {
                        (t.automation.len() + 1) as f32 * AUTO_H
                    } else {
                        0.0
                    }
            } else {
                0.0
            };
            let r = Row {
                y,
                h,
                visible,
                takes,
                auto,
                depth: arrange::depth(p, t.id),
            };
            y += h;
            r
        })
        .collect()
}

/// Horizontal mapping and grid shared by everything drawn on the timeline.
#[derive(Clone, Copy)]
struct Geo<'a> {
    timeline: Rect,
    scroll: f64,
    ppb: f32,
    grid: f64,
    map: &'a TimeMap,
    /// Grid lines on screen: x and whether it is a bar line.
    lines: &'a [(f32, bool)],
}

impl Geo<'_> {
    fn x(&self, b: f64) -> f32 {
        self.timeline.left() + ((b - self.scroll) as f32) * self.ppb
    }
    fn beat(&self, x: f32) -> f64 {
        self.scroll + ((x - self.timeline.left()) / self.ppb) as f64
    }
    fn snap(&self, b: f64) -> f64 {
        snap(self.map, b, self.grid)
    }
    fn last(&self) -> f64 {
        self.beat(self.timeline.right())
    }
}

/// Grid size in beats appropriate for the zoom level.
fn grid_for(ppb: f32, bpb: f64) -> f64 {
    if ppb >= 60.0 {
        0.25
    } else if ppb >= 24.0 {
        0.5
    } else if ppb >= 10.0 {
        1.0
    } else if ppb >= 3.0 {
        bpb
    } else {
        bpb * 4.0
    }
}

/// Snaps a beat to the grid, counting from bar lines so signature changes
/// keep the grid aligned. Grids of a bar or more snap to bar lines.
fn snap(map: &TimeMap, b: f64, grid: f64) -> f64 {
    let b = b.max(0.0);
    let (bar, off) = map.bar_of(b);
    let start = map.bar_start(bar);
    let len = map.bar_start(bar + 1) - start;
    if grid >= len - 1e-9 {
        let k = ((grid / len).round() as i64).max(1);
        let b0 = bar - bar.rem_euclid(k);
        let (s0, s1) = (map.bar_start(b0), map.bar_start(b0 + k));
        if b - s0 < s1 - b {
            s0
        } else {
            s1
        }
    } else {
        (start + (off / grid).round() * grid).min(start + len)
    }
}

/// Grid lines (beat, is bar line) between two beats.
fn grid_lines(map: &TimeMap, from: f64, to: f64, grid: f64) -> Vec<(f64, bool)> {
    let (mut bar, _) = map.bar_of(from.max(0.0));
    let mut out = Vec::new();
    loop {
        let s = map.bar_start(bar);
        if s > to || out.len() > 4000 {
            break;
        }
        let e = map.bar_start(bar + 1);
        let len = e - s;
        if grid >= len - 1e-9 {
            let k = ((grid / len).round() as i64).max(1);
            if bar.rem_euclid(k) == 0 {
                out.push((s, true));
            }
        } else {
            out.push((s, true));
            let mut b = s + grid;
            while b < e - 1e-9 {
                out.push((b, false));
                b += grid;
            }
        }
        bar += 1;
    }
    out
}

fn draw_grid(p: &egui::Painter, lines: &[(f32, bool)], y: egui::Rangef) {
    for &(x, bar) in lines {
        p.vline(
            x,
            y,
            Stroke::new(1.0, if bar { theme::grid_bar() } else { theme::grid() }),
        );
    }
}

/// A Session clip dragged over `rect`; returned once it is dropped.
fn slot_drop(ui: &egui::Ui, rect: Rect) -> Option<(Id, usize)> {
    let ctx = ui.ctx();
    let Some(ClipDrag::Slot { track, scene }) = egui::DragAndDrop::payload::<ClipDrag>(ctx).map(|p| *p) else {
        return None;
    };
    let pos = ctx.pointer_latest_pos()?;
    if !rect.contains(pos) || !ui.clip_rect().contains(pos) {
        return None;
    }
    ui.painter()
        .rect_stroke(rect, 2.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside);
    if ctx.input(|i| i.pointer.any_released()) {
        egui::DragAndDrop::take_payload::<ClipDrag>(ctx);
        Some((track, scene))
    } else {
        None
    }
}

/// Starts a time selection when a drag begins on empty lane space.
fn begin_selection(ui: &egui::Ui, resp: &egui::Response, geo: &Geo, track: Id, take: Option<Id>) {
    if resp.drag_started() {
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| resp.interact_pointer_pos());
        if let Some(o) = origin {
            set_local_drag(
                ui.ctx(),
                Some(LocalDrag::Select {
                    track,
                    take,
                    anchor: geo.snap(geo.beat(o.x)),
                }),
            );
        }
    }
}

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let ppb = app.arrangement.px_per_beat;
    let timeline = Rect::from_min_max(pos2(area.left() + HEAD_W, area.top()), area.max);
    let lanes_top = area.top() + LOOP_H + RULER_H + SIG_H;
    let show_tempo = app.work.show_tempo_lane;
    let lanes_bottom = if show_tempo {
        (area.bottom() - TEMPO_H).max(lanes_top)
    } else {
        area.bottom()
    };
    let lanes_rect = Rect::from_min_max(pos2(area.left(), lanes_top), pos2(area.right(), lanes_bottom));
    let rows = layout(app);
    let n_tracks = app.project.tracks.len();
    let tracks_h: f32 = rows.iter().map(|r| r.h).sum();
    let content_h = tracks_h + LANE_H;
    let max_scroll = (content_h - lanes_rect.height()).max(0.0);
    app.arrangement.scroll_y = app.arrangement.scroll_y.clamp(0.0, max_scroll);

    // ----- scrolling and zoom -----
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
            let anchor = pointer
                .map(|p| app.arrangement.scroll_beats + ((p.x - timeline.left()) / ppb) as f64)
                .unwrap_or(app.arrangement.scroll_beats);
            let new_ppb = (ppb * zoom).clamp(1.0, 240.0);
            let px = pointer.map(|p| p.x - timeline.left()).unwrap_or(0.0);
            app.arrangement.px_per_beat = new_ppb;
            app.arrangement.scroll_beats = (anchor - (px / new_ppb) as f64).max(0.0);
        } else {
            let (dx, dy) = if shift {
                (scroll.y + scroll.x, 0.0)
            } else {
                (scroll.x, scroll.y)
            };
            app.arrangement.scroll_beats = (app.arrangement.scroll_beats - (dx / ppb) as f64).max(0.0);
            app.arrangement.scroll_y = (app.arrangement.scroll_y - dy).clamp(0.0, max_scroll);
        }
    }

    // Follow the playhead.
    let playhead = app.song_beat();
    if app.is_playing() && app.arrangement.follow {
        let ppb = app.arrangement.px_per_beat;
        let visible = (timeline.width() / ppb) as f64;
        let s = app.arrangement.scroll_beats;
        if playhead > s + visible * 0.9 || playhead < s {
            app.arrangement.scroll_beats = (playhead - visible * 0.1).max(0.0);
        }
    }
    let scroll_y = app.arrangement.scroll_y;
    let map = app.time_map();
    let ppb = app.arrangement.px_per_beat;
    let scroll_beats = app.arrangement.scroll_beats;
    let grid = grid_for(ppb, map.meter_at(scroll_beats).bar_beats());
    let last_beat = scroll_beats + (timeline.width() / ppb) as f64;
    let lines: Vec<(f32, bool)> = grid_lines(&map, scroll_beats, last_beat, grid)
        .into_iter()
        .map(|(b, bar)| (timeline.left() + ((b - scroll_beats) as f32) * ppb, bar))
        .filter(|(x, _)| *x >= timeline.left() - 1.0)
        .collect();
    let geo = Geo {
        timeline,
        scroll: scroll_beats,
        ppb,
        grid,
        map: &map,
        lines: &lines,
    };

    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme::bg_view());
    let tl_painter = ui.painter_at(Rect::from_min_max(pos2(timeline.left(), area.top()), area.max));

    // ----- loop brace, ruler, locators, signatures -----
    ruler::loop_brace(app, ui, &tl_painter, &mut actions, &geo, area);
    let ruler_rect = ruler::ruler(app, ui, &tl_painter, &mut actions, &geo, area.top() + LOOP_H);
    ruler::signatures(app, ui, &tl_painter, &mut actions, &geo, ruler_rect.bottom());

    // ----- lanes -----
    let mut lui = ui.new_child(UiBuilder::new().max_rect(lanes_rect).id_salt("arr_lanes"));
    lui.set_clip_rect(lanes_rect.intersect(ui.clip_rect()));
    let lanes_painter = lui.painter_at(lanes_rect);
    let tl_lanes = Rect::from_min_max(pos2(timeline.left(), lanes_top), lanes_rect.max);
    let tl_lanes_painter = lui.painter_at(tl_lanes);
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let command = ui.input(|i| i.modifiers.command);
    let drag_active = app.arrangement.drag;
    let mut new_drag = drag_active;
    // Screen rows of the main lanes and take lanes, for overlays.
    let mut lane_rows: Vec<(Id, Option<Id>, Rect)> = Vec::new();
    for (ti, row) in rows.iter().enumerate() {
        if !row.visible {
            continue;
        }
        let y = lanes_top + row.y - scroll_y;
        if y + row.h < lanes_top || y > lanes_bottom {
            continue;
        }
        let track = &app.project.tracks[ti];
        let id = track.id;
        let kind = track.kind;
        let frozen = track.frozen;
        let lane = Rect::from_min_max(pos2(timeline.left(), y), pos2(timeline.right(), y + LANE_H));
        lane_rows.push((id, None, lane));
        if row.auto {
            automation_lanes(
                app,
                &mut lui,
                &mut actions,
                ti,
                y + LANE_H + row.takes as f32 * TAKE_H,
                lanes_rect,
                timeline.left(),
                &geo,
                playhead,
            );
        }
        tl_lanes_painter.rect_filled(
            lane.shrink2(vec2(0.0, 0.5)),
            0.0,
            if ti % 2 == 0 {
                theme::bg_lane()
            } else {
                theme::bg_lane_alt()
            },
        );
        draw_grid(&tl_lanes_painter, &lines, lane.y_range());
        if frozen {
            tl_lanes_painter.rect_filled(lane, 0.0, FROZEN_TINT);
        }
        let lane_resp = lui.interact(lane, lui.id().with(("lane", id)), Sense::click_and_drag());
        if lane_resp.clicked() {
            if let Some(p) = lane_resp.interact_pointer_pos() {
                actions.push(Action::ClearSelection);
                actions.push(Action::SetPosition(geo.snap(geo.beat(p.x))));
                actions.push(Action::SelectTrack(id));
            }
        }
        begin_selection(&lui, &lane_resp, &geo, id, None);
        if kind != TrackKind::Group && !frozen {
            if let Some(item) = super::browser::drop_on(&lui, lane) {
                // Samples land at the grid position under the pointer.
                let at = pointer.map(|p| geo.snap(geo.beat(p.x))).unwrap_or(0.0);
                actions.push(Action::Drop(item, DropTarget::Arrangement(id, at)));
            }
            if let Some((track, scene)) = slot_drop(&lui, lane) {
                let beat = pointer.map(|p| geo.snap(geo.beat(p.x))).unwrap_or(0.0);
                actions.push(Action::DropSlot {
                    track,
                    scene,
                    target: id,
                    beat,
                });
            }
        }
        if lane_resp.double_clicked() && kind == TrackKind::Midi && !frozen {
            if let Some(p) = lane_resp.interact_pointer_pos() {
                let (bar, _) = map.bar_of(geo.beat(p.x).max(0.0));
                actions.push(Action::Create(id, map.bar_start(bar)));
            }
        }

        if kind == TrackKind::Group {
            clips::group_summary(app, &tl_lanes_painter, &geo, ti, lane);
        }

        // Clips.
        let clip_count = app.project.tracks[ti].arrangement.len();
        for ci in 0..clip_count {
            let a = app.project.tracks[ti].arrangement[ci].clone();
            let (x0, x1) = (geo.x(a.start), geo.x(a.end()));
            if x1 < timeline.left() || x0 > timeline.right() {
                continue;
            }
            let rect = Rect::from_min_max(pos2(x0, y + 2.0), pos2(x1.max(x0 + 3.0), y + LANE_H - 2.0));
            let cref = ClipRef::Arrangement { track: id, id: a.id };
            let selected = app.selected_clip == Some(cref);
            clips::draw_clip(app, &tl_lanes_painter, rect, &a, selected, frozen, true);
            let resp = lui
                .interact(rect, lui.id().with(("aclip", a.id)), Sense::click_and_drag())
                .info("Arrangement Clip", info_view::ARRANGEMENT_CLIP);
            let near_edge = pointer.is_some_and(|p| (p.x - rect.right()).abs() < EDGE && rect.y_range().contains(p.y));
            if resp.hovered() && near_edge && !frozen {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.clicked() {
                actions.push(Action::ClearSelection);
                actions.push(Action::Select(cref));
            }
            if resp.drag_started() {
                actions.push(Action::Select(cref));
                if let (Some(p), false) = (resp.interact_pointer_pos(), frozen) {
                    new_drag = if (p.x - rect.right()).abs() < EDGE {
                        Some(ArrDrag::Resize { track: id, id: a.id })
                    } else {
                        ui.ctx().data_mut(|d| d.insert_temp(move_origin_id(), a.start));
                        Some(ArrDrag::Move {
                            track: id,
                            id: a.id,
                            grab_offset: geo.beat(p.x) - a.start,
                        })
                    };
                }
            }
            if resp.secondary_clicked() {
                actions.push(Action::Select(cref));
            }
            let info = clips::MenuInfo {
                track: id,
                id: a.id,
                audio: matches!(a.clip.content, tessera_engine::model::ClipContent::Audio(_)),
                frozen,
                length: a.length,
                converting: app.converting(),
            };
            resp.context_menu(|ui| clips::clip_menu(ui, &mut actions, &info));
            if !frozen {
                clips::fade_handles(&lui, &tl_lanes_painter, &a, id, rect, resp.hovered() || selected);
            }
        }

        // Take lanes.
        for k in 0..row.takes {
            let ty = y + LANE_H + k as f32 * TAKE_H;
            let take_id = app.project.tracks[ti].takes[k].id;
            let rect = Rect::from_min_max(pos2(timeline.left(), ty), pos2(timeline.right(), ty + TAKE_H));
            lane_rows.push((id, Some(take_id), rect));
            clips::take_lane(
                app,
                &mut lui,
                &lanes_painter,
                &tl_lanes_painter,
                &mut actions,
                &geo,
                ti,
                k,
                rect,
            );
        }

        // Track header.
        let indent = row.depth as f32 * INDENT;
        let head = Rect::from_min_max(pos2(area.left() + indent, y), pos2(timeline.left(), y + LANE_H))
            .shrink2(vec2(2.0, 1.0));
        track_header(app, &mut lui, &lanes_painter, &mut actions, ti, head, row.auto, command);
    }

    // ----- takes being recorded -----
    for ti in 0..n_tracks {
        let id = app.project.tracks[ti].id;
        let Some(take) = app.engine.shared.record.take_for(id) else {
            continue;
        };
        if take.slot.load(std::sync::atomic::Ordering::Relaxed) != -1 {
            continue;
        }
        let Some(&(_, _, lane)) = lane_rows.iter().find(|(t, k, _)| *t == id && k.is_none()) else {
            continue;
        };
        let start = take.start_beat.load();
        let (x0, x1) = (geo.x(start).max(timeline.left()), geo.x(playhead).min(timeline.right()));
        if x1 > x0 {
            let r = Rect::from_min_max(pos2(x0, lane.top() + 2.0), pos2(x1, lane.bottom() - 2.0));
            tl_lanes_painter.rect_filled(r, 2.0, theme::dim(theme::ARM, 0.75));
            tl_lanes_painter.text(
                pos2(r.left() + 4.0, r.top() + 3.0),
                egui::Align2::LEFT_TOP,
                "Recording",
                egui::FontId::proportional(11.0),
                Color32::BLACK,
            );
        }
    }

    // ----- time selection -----
    if let Some(s) = app.work.selection {
        if let Some(&(_, _, lane)) = lane_rows.iter().find(|(t, k, _)| *t == s.track && *k == s.take) {
            let (x0, x1) = (geo.x(s.from), geo.x(s.to));
            let r = Rect::from_min_max(pos2(x0, lane.top()), pos2(x1.max(x0 + 1.0), lane.bottom()));
            tl_lanes_painter.rect_filled(r, 0.0, SELECTION_FILL);
            tl_lanes_painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, Color32::from_white_alpha(120)),
                StrokeKind::Inside,
            );
            // Show the selection on the ruler too.
            tl_painter.rect_filled(
                Rect::from_min_max(pos2(x0, ruler_rect.top()), pos2(x1.max(x0 + 1.0), ruler_rect.bottom())),
                0.0,
                Color32::from_white_alpha(28),
            );
        }
    }

    // ----- active drags -----
    app.arrangement.drag = new_drag;
    let (down, pos, alt) = ui.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos(), i.modifiers.alt));
    let snapb = |b: f64| if alt { b.max(0.0) } else { geo.snap(b) };
    if let (Some(drag), Some(p)) = (app.arrangement.drag, pos) {
        let mut dragged_out = false;
        match drag {
            ArrDrag::Move { track, id, grab_offset } => {
                if !area.contains(p) {
                    // Leaving the timeline turns the move into a clip drag
                    // (onto Session slots); the clip goes back where it was.
                    let origin: Option<f64> = ui.ctx().data(|d| d.get_temp(move_origin_id()));
                    if let Some(a) = app
                        .project
                        .track_mut(track)
                        .and_then(|t| t.arrangement.iter_mut().find(|a| a.id == id))
                    {
                        if let Some(o) = origin {
                            a.start = o;
                        }
                    }
                    egui::DragAndDrop::set_payload(ui.ctx(), ClipDrag::Arrangement { track, id });
                    app.arrangement.drag = None;
                    dragged_out = true;
                } else {
                    let start = snapb(geo.beat(p.x) - grab_offset);
                    if let Some(a) = app
                        .project
                        .track_mut(track)
                        .and_then(|t| t.arrangement.iter_mut().find(|a| a.id == id))
                    {
                        a.start = start;
                    }
                }
            }
            ArrDrag::Resize { track, id } => {
                if let Some(a) = app
                    .project
                    .track_mut(track)
                    .and_then(|t| t.arrangement.iter_mut().find(|a| a.id == id))
                {
                    let end = snapb(geo.beat(p.x)).max(a.start + grid.min(1.0));
                    a.length = end - a.start;
                    a.fade_in = a.fade_in.min(a.length);
                    a.fade_out = a.fade_out.min(a.length - a.fade_in);
                }
            }
            ArrDrag::LoopMove { grab_offset } => {
                app.project.loop_start = snapb(geo.beat(p.x) - grab_offset);
            }
            ArrDrag::LoopStart => {
                let end = app.project.loop_start + app.project.loop_length;
                let s = snapb(geo.beat(p.x)).clamp(0.0, end - grid);
                app.project.loop_start = s;
                app.project.loop_length = end - s;
            }
            ArrDrag::LoopEnd => {
                let e = snapb(geo.beat(p.x)).max(app.project.loop_start + grid);
                app.project.loop_length = e - app.project.loop_start;
            }
        }
        if dragged_out {
            app.touch();
        } else if !down {
            app.arrangement.drag = None;
            match drag {
                ArrDrag::Move { track, .. } => {
                    app.label_edit("Move Clip");
                    if let Some(t) = app.project.track_mut(track) {
                        t.sort_arrangement();
                    }
                    actions.push(Action::Changed);
                }
                ArrDrag::Resize { .. } => {
                    app.label_edit("Resize Clip");
                    actions.push(Action::Changed);
                }
                _ => actions.push(Action::Loop),
            }
        }
    }
    if let Some(drag) = get_local_drag(ui.ctx()) {
        local_drag(app, ui, &mut actions, drag, &geo, pos, down);
    }
    clip_drag_preview(app, ui.ctx());

    // ----- Song Tempo lane -----
    if show_tempo {
        let rect = Rect::from_min_max(pos2(area.left(), lanes_bottom), area.max);
        tempo_lane(app, ui, rect, &geo, playhead);
    }

    // ----- playhead and corner -----
    let px = geo.x(playhead);
    if px >= timeline.left() && px <= timeline.right() {
        tl_painter.vline(px, ruler_rect.top()..=area.bottom(), Stroke::new(1.5, theme::select()));
    }
    corner(
        app,
        ui,
        &painter,
        Rect::from_min_max(area.min, pos2(timeline.left(), lanes_top)),
    );
    let hint_y = lanes_top + tracks_h - scroll_y + 14.0;
    if hint_y < lanes_bottom {
        painter.text(
            pos2(timeline.left() + 6.0, hint_y),
            egui::Align2::LEFT_CENTER,
            "Drag on empty space to select time · double-click a MIDI lane for a clip · drag clip corners for fades · right-click for more",
            egui::FontId::proportional(11.0),
            theme::text_dim(),
        );
    }

    apply(app, actions);
}

/// Follows a time selection or fade drag.
fn local_drag(
    app: &mut TesseraApp,
    ui: &egui::Ui,
    actions: &mut Vec<Action>,
    drag: LocalDrag,
    geo: &Geo,
    pos: Option<Pos2>,
    down: bool,
) {
    match drag {
        LocalDrag::Select { track, take, anchor } => {
            if let Some(p) = pos {
                let b = geo.snap(geo.beat(p.x));
                app.work.selection = Some(TimeSelection {
                    track,
                    from: anchor.min(b),
                    to: anchor.max(b),
                    take,
                });
            }
            if !down {
                set_local_drag(ui.ctx(), None);
                actions.push(Action::SelectTrack(track));
                match app.work.selection {
                    Some(s) if s.to - s.from < 1e-9 => app.work.selection = None,
                    Some(s) => {
                        if let Some(take) = s.take {
                            actions.push(Action::Comp(track, take, s.from, s.to));
                        }
                    }
                    None => {}
                }
            }
        }
        LocalDrag::FadeIn { track, id } | LocalDrag::FadeOut { track, id } => {
            let fade_in = matches!(drag, LocalDrag::FadeIn { .. });
            let Some(a) = app
                .project
                .track_mut(track)
                .and_then(|t| t.arrangement.iter_mut().find(|a| a.id == id))
            else {
                set_local_drag(ui.ctx(), None);
                return;
            };
            if let Some(p) = pos {
                let b = geo.beat(p.x);
                if fade_in {
                    a.fade_in = (b - a.start).clamp(0.0, (a.length - a.fade_out).max(0.0));
                } else {
                    a.fade_out = (a.end() - b).clamp(0.0, (a.length - a.fade_in).max(0.0));
                }
            }
            let (fi, fo) = (a.fade_in, a.fade_out);
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            if down {
                app.label_edit("Clip Fade");
                app.touch();
            } else {
                set_local_drag(ui.ctx(), None);
                actions.push(Action::SetFades(track, id, fi, fo));
            }
        }
    }
}

/// Draws the name of an Arrangement clip dragged out of the timeline.
fn clip_drag_preview(app: &TesseraApp, ctx: &egui::Context) {
    let Some(ClipDrag::Arrangement { track, id }) = egui::DragAndDrop::payload::<ClipDrag>(ctx).map(|p| *p) else {
        return;
    };
    let Some(pos) = ctx.pointer_latest_pos() else { return };
    let Some(a) = app
        .project
        .track(track)
        .and_then(|t| t.arrangement.iter().find(|a| a.id == id))
    else {
        return;
    };
    let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("arr_clip_drag")));
    let galley = p.layout_no_wrap(a.clip.name.clone(), egui::FontId::proportional(11.0), Color32::BLACK);
    let r = Rect::from_min_size(pos + vec2(12.0, 8.0), galley.size() + vec2(10.0, 6.0));
    p.rect_filled(r, 2.0, theme::rgb(a.clip.color));
    p.galley(r.min + vec2(5.0, 3.0), galley, Color32::BLACK);
}

/// A track header: name, fold triangle, mute/solo/arm, volume and the
/// track context menu.
#[allow(clippy::too_many_arguments)]
fn track_header(
    app: &mut TesseraApp,
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    actions: &mut Vec<Action>,
    ti: usize,
    head: Rect,
    auto_open: bool,
    command: bool,
) {
    let t = &app.project.tracks[ti];
    let id = t.id;
    let is_group = t.kind == TrackKind::Group;
    let selected = app.selected_track == Some(id) || app.work.selected_tracks.contains(&id);
    let multi = app.work.selected_tracks.contains(&id);
    let color = theme::rgb(t.color);
    painter.rect_filled(
        head,
        2.0,
        if selected {
            theme::gray(0x50)
        } else if is_group {
            theme::gray(0x3a)
        } else {
            theme::bg_panel()
        },
    );
    painter.rect_filled(Rect::from_min_size(head.min, vec2(5.0, head.height())), 2.0, color);
    if multi {
        painter.rect_stroke(head, 2.0, Stroke::new(1.0, theme::ACCENT), StrokeKind::Inside);
    }
    let head_resp = ui
        .interact(head, ui.id().with(("ahead", id)), Sense::click())
        .info("Track Header", info_view::TRACK_TITLE);
    if head_resp.clicked() {
        actions.push(if command {
            Action::ToggleTrackSelection(id)
        } else {
            Action::SelectTrack(id)
        });
    }
    if head_resp.secondary_clicked() {
        actions.push(Action::EnsureSelected(id));
    }
    let (name, frozen, show_takes, n_takes) = (t.name.clone(), t.frozen, t.show_takes, t.takes.len());
    head_resp.context_menu(|ui| {
        ui.set_min_width(170.0);
        let mut n = name.clone();
        ui.horizontal(|ui| {
            ui.label("Name");
            if ui.text_edit_singleline(&mut n).changed() {
                actions.push(Action::Rename(id, n.clone()));
            }
        });
        ui.separator();
        if ui.button("Group Tracks").clicked() {
            actions.push(Action::EnsureSelected(id));
            actions.push(Action::Group);
            ui.close();
        }
        if is_group && ui.button("Ungroup Tracks").clicked() {
            actions.push(Action::Ungroup(id));
            ui.close();
        }
        if ui.button("Duplicate").clicked() {
            actions.push(Action::EnsureSelected(id));
            actions.push(Action::Duplicate);
            ui.close();
        }
        if ui.button("Delete").clicked() {
            actions.push(Action::EnsureSelected(id));
            actions.push(Action::DeleteTracks);
            ui.close();
        }
        if !is_group {
            ui.separator();
            if frozen {
                if ui.button("Unfreeze Track").clicked() {
                    actions.push(Action::Unfreeze(id));
                    ui.close();
                }
            } else if ui.button("Freeze Track").clicked() {
                actions.push(Action::Freeze(id));
                ui.close();
            }
            if ui.button("Flatten").clicked() {
                actions.push(Action::Flatten(id));
                ui.close();
            }
            ui.separator();
            let label = format!("{} Take Lanes ({n_takes})", if show_takes { "Hide" } else { "Show" });
            if ui.button(label).clicked() {
                actions.push(Action::ToggleTakes(id));
                ui.close();
            }
        }
    });

    let inner = head.shrink2(vec2(9.0, 4.0));
    let inner = Rect::from_min_max(pos2(inner.left() + 2.0, inner.top()), inner.max);
    if is_group {
        let folded = app.project.tracks[ti].folded;
        let tri = Rect::from_min_size(pos2(inner.left(), inner.top() + 1.0), vec2(14.0, 14.0));
        let c = tri.center();
        let pts = if folded {
            vec![c + vec2(-3.0, -5.0), c + vec2(4.0, 0.0), c + vec2(-3.0, 5.0)]
        } else {
            vec![c + vec2(-5.0, -3.0), c + vec2(5.0, -3.0), c + vec2(0.0, 4.0)]
        };
        let r = ui.interact(tri, ui.id().with(("fold", id)), Sense::click());
        let fill = if r.hovered() { theme::select() } else { theme::text() };
        painter.add(Shape::convex_polygon(pts, fill, Stroke::NONE));
        if r.clicked() {
            actions.push(Action::ToggleFold(id));
        }
        r.on_hover_text(if folded { "Unfold group" } else { "Fold group" });
    }
    let t = &mut app.project.tracks[ti];
    ui.scope_builder(UiBuilder::new().max_rect(inner).id_salt(("ahead_ui", id)), |ui| {
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        ui.horizontal(|ui| {
            if is_group {
                ui.add_space(15.0);
            }
            ui.add(egui::Label::new(egui::RichText::new(&t.name).color(theme::text()).strong()).truncate());
            if t.frozen {
                ui.label(egui::RichText::new("Frozen").small().color(theme::SOLO));
            }
        });
        ui.horizontal(|ui| {
            if widgets::toggle(ui, !t.mixer.mute, &format!("{}", ti + 1), theme::ACTIVE, 24.0).clicked() {
                t.mixer.mute = !t.mixer.mute;
                actions.push(Action::Mixer(id));
            }
            if widgets::toggle(ui, t.mixer.solo, "S", theme::SOLO, 20.0)
                .info("Solo", info_view::SOLO)
                .clicked()
            {
                t.mixer.solo = !t.mixer.solo;
                actions.push(Action::Mixer(id));
            }
            let has = t.automation.iter().any(|l| !l.envelope.is_empty());
            let label = if has { "A•" } else { "A" };
            if widgets::toggle(ui, auto_open, label, theme::ACCENT, 22.0)
                .on_hover_text("Show automation lanes")
                .clicked()
            {
                actions.push(Action::ToggleLanes(id));
            }
            if !is_group
                && widgets::toggle(ui, t.mixer.arm, "●", theme::ARM, 20.0)
                    .on_hover_text("Arm for recording")
                    .clicked()
            {
                t.mixer.arm = !t.mixer.arm;
                actions.push(Action::Mixer(id));
            }
            let r = ui
                .add(
                    egui::DragValue::new(&mut t.mixer.volume_db)
                        .range(-70.0..=6.0)
                        .speed(0.2)
                        .fixed_decimals(1)
                        .suffix(" dB"),
                )
                .info("Track Volume", info_view::VOLUME);
            if r.changed() {
                actions.push(Action::Mixer(id));
            }
        });
    });
}

/// The pinned "Master / Song Tempo" lane editing the tempo automation.
fn tempo_lane(app: &mut TesseraApp, ui: &mut egui::Ui, rect: Rect, geo: &Geo, playhead: f64) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, theme::gray(0x26));
    painter.hline(rect.x_range(), rect.top() + 0.5, Stroke::new(1.0, theme::grid_bar()));
    let head = Rect::from_min_max(
        pos2(rect.left() + 2.0, rect.top() + 3.0),
        pos2(geo.timeline.left() - 2.0, rect.bottom() - 2.0),
    );
    painter.rect_filled(head, 2.0, theme::bg_panel());
    painter.rect_filled(
        Rect::from_min_size(head.min, vec2(5.0, head.height())),
        2.0,
        theme::text_dim(),
    );
    let body = Rect::from_min_max(pos2(geo.timeline.left(), rect.top() + 2.0), rect.max);
    draw_grid(&ui.painter_at(body), geo.lines, body.y_range());
    let tempo_now = geo.map.tempo_at(playhead);
    let lane = &app.project.tempo_lane;
    if lane.envelope.is_empty() {
        // The fixed tempo, as a guide line.
        let n = lane.normalize(app.project.tempo);
        let r = body.shrink2(vec2(0.0, 3.0));
        let y = r.bottom() - n * r.height();
        painter.hline(body.x_range(), y, Stroke::new(1.0, theme::dim(theme::ACCENT, 0.5)));
    }
    let (mut min, mut max) = (lane.min, lane.max);
    let mut range_changed = false;
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(head.shrink2(vec2(9.0, 4.0)))
            .id_salt("tempo_head"),
        |ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 2.0);
            ui.label(egui::RichText::new("Master").color(theme::text()).strong());
            ui.label(
                egui::RichText::new(format!("Song Tempo · {tempo_now:.1} BPM"))
                    .small()
                    .color(theme::text_dim()),
            );
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Range").small().color(theme::text_dim()));
                range_changed |= ui
                    .add(
                        egui::DragValue::new(&mut min)
                            .range(20.0..=max - 1.0)
                            .speed(0.5)
                            .fixed_decimals(0),
                    )
                    .on_hover_text("Lowest tempo of the lane")
                    .changed();
                range_changed |= ui
                    .add(
                        egui::DragValue::new(&mut max)
                            .range(min + 1.0..=999.0)
                            .speed(0.5)
                            .fixed_decimals(0),
                    )
                    .on_hover_text("Highest tempo of the lane")
                    .changed();
            });
        },
    );
    if range_changed {
        app.label_edit("Tempo Lane Range");
        app.project.tempo_lane.min = min;
        app.project.tempo_lane.max = max;
        app.changed();
    }
    let lane = &mut app.project.tempo_lane;
    let spec = ParamSpec {
        name: "Song Tempo",
        min: lane.min as f32,
        max: lane.max as f32,
        scale: ParamScale::Linear,
        unit: "BPM",
        labels: &[],
    };
    let view = envelope::View {
        rect: body,
        beat0: geo.scroll,
        px_per_beat: geo.ppb,
        grid: geo.grid,
        min_beat: 0.0,
        max_beat: None,
        clip: body.intersect(ui.clip_rect()),
    };
    let r = envelope::edit(
        ui,
        ui.id().with("song_tempo_lane"),
        &mut lane.envelope,
        view,
        Some(spec),
        theme::ACCENT,
        false,
        Some(playhead),
    );
    if r.changed {
        app.label_edit("Song Tempo");
        app.changed();
    }
}

/// The top-left corner: Follow, zoom and the Song Tempo lane toggle.
fn corner(app: &mut TesseraApp, ui: &mut egui::Ui, painter: &egui::Painter, corner: Rect) {
    painter.rect_filled(corner, 0.0, theme::bg_bar());
    ui.scope_builder(
        UiBuilder::new().max_rect(corner.shrink(3.0)).id_salt("arr_corner"),
        |ui| {
            ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
            ui.horizontal(|ui| {
                if widgets::toggle(ui, app.arrangement.follow, "Follow", theme::ACCENT, 48.0)
                    .info(
                        "Follow",
                        "While on, the Arrangement scrolls to keep the playhead in view during playback.",
                    )
                    .clicked()
                {
                    app.arrangement.follow = !app.arrangement.follow;
                }
                if ui
                    .small_button("−")
                    .info(
                        "Zoom Out",
                        "Shows more of the song. Ctrl/Cmd + mouse wheel zooms around the pointer.",
                    )
                    .clicked()
                {
                    app.arrangement.px_per_beat = (app.arrangement.px_per_beat / 1.5).max(1.0);
                }
                if ui
                    .small_button("+")
                    .info(
                        "Zoom In",
                        "Shows more detail. Ctrl/Cmd + mouse wheel zooms around the pointer.",
                    )
                    .clicked()
                {
                    app.arrangement.px_per_beat = (app.arrangement.px_per_beat * 1.5).min(240.0);
                }
            });
            ui.horizontal(|ui| {
                let active = app.project.tempo_lane.is_active();
                let label = if active { "Tempo•" } else { "Tempo" };
                if widgets::toggle(ui, app.work.show_tempo_lane, label, theme::ACCENT, 52.0)
                    .on_hover_text("Show the Song Tempo automation lane")
                    .clicked()
                {
                    app.work.show_tempo_lane = !app.work.show_tempo_lane;
                }
                let map = app.time_map();
                ui.label(
                    egui::RichText::new(map.format_position(app.song_beat()))
                        .small()
                        .monospace()
                        .color(theme::text_dim()),
                );
            });
        },
    );
}

fn apply(app: &mut TesseraApp, actions: Vec<Action>) {
    for a in actions {
        match a {
            Action::Drop(item, target) => app.drop_item(&item, target),
            Action::Select(r) => {
                app.select_clip(r);
                app.detail = DetailMode::Clip;
            }
            Action::SelectTrack(t) => app.select_track(t),
            Action::ToggleTrackSelection(t) => app.toggle_track_selection(t),
            Action::EnsureSelected(t) => {
                if !app.work.selected_tracks.contains(&t) {
                    app.select_track(t);
                }
            }
            Action::SetPosition(b) => app.send(Command::SetPosition(b.max(0.0))),
            Action::Create(t, s) => app.create_arrangement_clip(t, s),
            Action::Delete(r) => app.delete_clip(r),
            Action::Mixer(t) => app.send_mixer(t),
            Action::Changed => app.changed(),
            Action::LaneEdited => {
                app.label_edit("Edit Automation");
                app.changed();
            }
            Action::Loop => app.send_loop(),
            Action::ToggleLanes(t) => {
                app.toggle_automation_lanes(t);
                // Open with a volume lane so there is something to draw on.
                if let Some(track) = app.project.track_mut(t) {
                    if track.automation.is_empty() {
                        track.automation.push(AutomationLane::new(AutomationTarget::Volume));
                    }
                }
            }
            Action::AddLane(t, target) => {
                app.label_edit("Add Automation Lane");
                if let Some(track) = app.project.track_mut(t) {
                    if !track.automation.iter().any(|l| l.target == target) {
                        track.automation.push(AutomationLane::new(target));
                    }
                }
                app.touch();
            }
            Action::RemoveLane(t, i) => {
                app.label_edit("Delete Automation Lane");
                if let Some(track) = app.project.track_mut(t) {
                    if i < track.automation.len() {
                        track.automation.remove(i);
                    }
                }
                app.changed();
            }
            Action::ToggleFold(t) => app.toggle_fold(t),
            Action::Rename(t, name) => {
                app.label_edit("Rename Track");
                if let Some(track) = app.project.track_mut(t) {
                    track.name = name;
                }
                app.touch();
            }
            Action::Group => app.group_selected(),
            Action::Ungroup(g) => app.ungroup(g),
            Action::Duplicate => app.duplicate_tracks(),
            Action::DeleteTracks => app.delete_tracks(),
            Action::Freeze(t) => app.freeze_track(t),
            Action::Unfreeze(t) => app.unfreeze_track(t),
            Action::Flatten(t) => app.flatten_track(t),
            Action::ToggleTakes(t) => app.toggle_takes(t),
            Action::DeleteTake(t, k) => {
                if app.work.selection.is_some_and(|s| s.take == Some(k)) {
                    app.work.selection = None;
                }
                app.delete_take(t, k);
            }
            Action::Comp(t, k, from, to) => {
                if to > from {
                    app.comp(t, k, from, to);
                }
            }
            Action::AddLocator(b) => app.add_locator_at(b),
            Action::RenameLocator(id, name) => {
                app.label_edit("Rename Locator");
                if let Some(l) = app.project.locators.iter_mut().find(|l| l.id == id) {
                    l.name = name;
                }
                app.touch();
            }
            Action::DeleteLocator(id) => app.delete_locator(id),
            Action::AddSignature(b, n, d) => app.add_signature_at(b, n, d),
            Action::DeleteSignature(i) => app.delete_signature(i),
            Action::ClearSelection => app.work.selection = None,
            Action::Split(r) => {
                focus_clip(app, r);
                app.split();
            }
            Action::Consolidate(r) => {
                focus_clip(app, r);
                app.consolidate();
            }
            Action::Reverse(r) => app.reverse_clip(r),
            Action::Crossfade(t, id) => app.crossfade_next(t, id, 1.0),
            Action::SetFades(t, id, fi, fo) => app.set_fades(t, id, fi, fo),
            Action::Convert(r, mode) => app.convert_to_midi(r, mode),
            Action::Slice(r, division) => app.slice_to_midi(r, division),
            Action::ExtractGroove(r) => app.extract_groove(r),
            Action::ToSession(t, id) => {
                let scene = app.work.selected_scene;
                app.arrangement_to_slot(t, id, t, scene);
            }
            Action::DropSlot {
                track,
                scene,
                target,
                beat,
            } => app.slot_to_arrangement_at(track, scene, target, beat),
        }
    }
}

/// Makes a clip the target of Split/Consolidate: selects it and its track,
/// dropping a time selection on another track.
fn focus_clip(app: &mut TesseraApp, r: ClipRef) {
    app.select_clip(r);
    app.selected_track = Some(r.track());
    if app
        .work
        .selection
        .is_some_and(|s| s.track != r.track() || s.take.is_some())
    {
        app.work.selection = None;
    }
}

/// Draws the open automation lanes of track `ti`, starting at `top`, plus a
/// row to add another lane.
#[allow(clippy::too_many_arguments)]
fn automation_lanes(
    app: &mut TesseraApp,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
    ti: usize,
    top: f32,
    visible: Rect,
    timeline_left: f32,
    geo: &Geo,
    playhead: f64,
) {
    let track = &app.project.tracks[ti];
    let id = track.id;
    let color = theme::rgb(track.color);
    let n = track.automation.len();
    let targets = app.automation_target_menu(id);
    let painter = ui.painter_at(visible);
    for li in 0..=n {
        let y = top + li as f32 * AUTO_H;
        if y > visible.bottom() || y + AUTO_H < visible.top() {
            continue;
        }
        let row = Rect::from_min_max(pos2(visible.left(), y), pos2(visible.right(), y + AUTO_H));
        painter.rect_filled(row, 0.0, theme::gray(0x26));
        painter.hline(row.x_range(), row.bottom(), Stroke::new(1.0, theme::bg_view()));
        let head = Rect::from_min_max(
            pos2(row.left() + 14.0, y + 2.0),
            pos2(timeline_left - 2.0, y + AUTO_H - 2.0),
        );
        painter.rect_filled(
            Rect::from_min_max(pos2(row.left() + 4.0, y), pos2(row.left() + 7.0, y + AUTO_H)),
            0.0,
            theme::dim(color, 0.6),
        );
        let body = Rect::from_min_max(pos2(timeline_left, y), pos2(row.right(), y + AUTO_H));
        for &(x, _) in geo.lines {
            painter.vline(x, body.y_range(), Stroke::new(1.0, theme::grid()));
        }
        if li == n {
            ui.scope_builder(UiBuilder::new().max_rect(head).id_salt(("add_lane", id)), |ui| {
                let used: Vec<AutomationTarget> = app.project.tracks[ti].automation.iter().map(|l| l.target).collect();
                egui::ComboBox::from_id_salt(("add_lane_combo", id))
                    .selected_text(egui::RichText::new("+ Add lane").small())
                    .width(head.width() - 8.0)
                    .show_ui(ui, |ui| {
                        let mut last = None;
                        for (t, group, name) in targets.iter().filter(|(t, ..)| !used.contains(t)) {
                            envelope::group_heading(ui, &mut last, group);
                            if ui.selectable_label(false, name).clicked() {
                                actions.push(Action::AddLane(id, *t));
                            }
                        }
                    });
            });
            continue;
        }
        let target = app.project.tracks[ti].automation[li].target;
        let spec = app.automation_target_spec(id, target);
        let name = app.automation_target_name(id, target);
        let lane = &mut app.project.tracks[ti].automation[li];
        ui.scope_builder(UiBuilder::new().max_rect(head).id_salt(("lane_head", id, li)), |ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 1.0);
            ui.horizontal(|ui| {
                let text = egui::RichText::new(&name).small();
                ui.label(if lane.overridden {
                    text.color(theme::text_dim()).italics()
                } else {
                    text.color(theme::text())
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("×").on_hover_text("Delete lane").clicked() {
                        actions.push(Action::RemoveLane(id, li));
                    }
                });
            });
            if let (Some(spec), Some(v)) = (spec, lane.envelope.value_at(playhead)) {
                let mut text = spec.format(spec.denormalize(v));
                if lane.overridden {
                    text.push_str("  (overridden)");
                }
                ui.label(egui::RichText::new(text).small().color(theme::text_dim()));
            }
        });
        let view = envelope::View {
            rect: body,
            beat0: geo.scroll,
            px_per_beat: geo.ppb,
            grid: geo.grid,
            min_beat: 0.0,
            max_beat: None,
            clip: visible,
        };
        let r = envelope::edit(
            ui,
            ui.id().with(("auto_lane", id, li)),
            &mut lane.envelope,
            view,
            spec,
            theme::ACCENT,
            lane.overridden,
            Some(playhead),
        );
        if r.changed {
            // Rebuild the snapshot so the edit is heard right away.
            actions.push(Action::LaneEdited);
        }
    }
}
