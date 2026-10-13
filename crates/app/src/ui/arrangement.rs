//! Arrangement view: a timeline of clips per track.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind, UiBuilder};
use tessera_engine::automation::{target_spec, AutomationLane, AutomationTarget};
use tessera_engine::model::{ClipContent, Id, TrackKind};
use tessera_engine::Command;

use super::{envelope, theme, widgets};
use crate::app::{ArrDrag, ClipRef, DetailMode, TesseraApp};
use crate::browser::{BrowserItem, DropTarget};

const HEAD_W: f32 = 176.0;
const LOOP_H: f32 = 12.0;
const RULER_H: f32 = 20.0;
const LANE_H: f32 = 62.0;
/// Height of an automation lane below its track.
const AUTO_H: f32 = 44.0;
const EDGE: f32 = 6.0;

enum Action {
    Select(ClipRef),
    SelectTrack(Id),
    SetPosition(f64),
    Create(Id, f64),
    Delete(ClipRef),
    Mixer(Id),
    Changed,
    Loop,
    ToggleLanes(Id),
    AddLane(Id, AutomationTarget),
    RemoveLane(Id, usize),
    Drop(BrowserItem, DropTarget),
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

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let mut actions = Vec::new();
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let bpb = app.project.beats_per_bar as f64;
    let ppb = app.arrangement.px_per_beat;
    let timeline = Rect::from_min_max(pos2(area.left() + HEAD_W, area.top()), area.max);
    let lanes_top = area.top() + LOOP_H + RULER_H;
    let lanes_rect = Rect::from_min_max(pos2(area.left(), lanes_top), area.max);
    let n_tracks = app.project.tracks.len();
    // Track rows grow by their open automation lanes (plus an "add" row).
    let heights: Vec<f32> = app
        .project
        .tracks
        .iter()
        .map(|t| {
            if app.automation.expanded.contains(&t.id) {
                LANE_H + (t.automation.len() + 1) as f32 * AUTO_H
            } else {
                LANE_H
            }
        })
        .collect();
    let offsets: Vec<f32> = heights
        .iter()
        .scan(0.0, |acc, h| {
            let o = *acc;
            *acc += h;
            Some(o)
        })
        .collect();
    let tracks_h: f32 = heights.iter().sum();
    let content_h = tracks_h + LANE_H;
    app.arrangement.scroll_y = app
        .arrangement
        .scroll_y
        .clamp(0.0, (content_h - lanes_rect.height()).max(0.0));
    let scroll_y = app.arrangement.scroll_y;

    // ----- scrolling and zoom -----
    let hovered = ui.rect_contains_pointer(area);
    if hovered {
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
            app.arrangement.scroll_y =
                (app.arrangement.scroll_y - dy).clamp(0.0, (content_h - lanes_rect.height()).max(0.0));
        }
    }
    let ppb = app.arrangement.px_per_beat;
    let scroll_beats = app.arrangement.scroll_beats;
    let x_of = |b: f64| timeline.left() + ((b - scroll_beats) as f32) * ppb;
    let beat_of = |x: f32| scroll_beats + ((x - timeline.left()) / ppb) as f64;
    let grid = grid_for(ppb, bpb);
    let snap = |b: f64| (b / grid).round() * grid;

    // Follow the playhead.
    let playhead = app.song_beat();
    if app.is_playing() && app.arrangement.follow {
        let visible = (timeline.width() / ppb) as f64;
        if playhead > scroll_beats + visible * 0.9 || playhead < scroll_beats {
            app.arrangement.scroll_beats = (playhead - visible * 0.1).max(0.0);
        }
    }

    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme::BG_VIEW);

    // ----- grid -----
    let first = (scroll_beats / grid).floor() * grid;
    let last = beat_of(timeline.right());
    let mut b = first;
    let tl_painter = ui.painter_at(Rect::from_min_max(pos2(timeline.left(), area.top()), area.max));
    while b <= last {
        let x = x_of(b);
        let is_bar = (b / bpb).fract().abs() < 1e-9;
        tl_painter.vline(
            x,
            lanes_top..=area.bottom(),
            Stroke::new(1.0, if is_bar { theme::GRID_BAR } else { theme::GRID }),
        );
        b += grid;
    }

    // ----- loop brace -----
    let loop_rect = Rect::from_min_max(
        pos2(timeline.left(), area.top()),
        pos2(timeline.right(), area.top() + LOOP_H),
    );
    tl_painter.rect_filled(loop_rect, 0.0, theme::BG_BAR);
    let (ls, le) = (app.project.loop_start, app.project.loop_start + app.project.loop_length);
    let brace = Rect::from_min_max(
        pos2(x_of(ls), loop_rect.top() + 1.0),
        pos2(x_of(le), loop_rect.bottom() - 1.0),
    );
    let brace_color = if app.project.loop_enabled {
        theme::ACCENT
    } else {
        Color32::from_rgb(0x70, 0x70, 0x70)
    };
    tl_painter.rect_filled(brace, 1.0, brace_color);
    let loop_resp = ui.interact(loop_rect, ui.id().with("loop"), Sense::click_and_drag());
    if loop_resp.double_clicked() {
        app.project.loop_enabled = !app.project.loop_enabled;
        actions.push(Action::Loop);
    }
    if loop_resp.drag_started() {
        if let Some(p) = loop_resp.interact_pointer_pos() {
            app.arrangement.drag = if (p.x - brace.left()).abs() < EDGE {
                Some(ArrDrag::LoopStart)
            } else if (p.x - brace.right()).abs() < EDGE {
                Some(ArrDrag::LoopEnd)
            } else if brace.x_range().contains(p.x) {
                Some(ArrDrag::LoopMove {
                    grab_offset: beat_of(p.x) - ls,
                })
            } else {
                None
            };
        }
    }
    if let Some(p) = loop_resp.hover_pos() {
        if (p.x - brace.left()).abs() < EDGE || (p.x - brace.right()).abs() < EDGE {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }

    // ----- ruler -----
    let ruler = Rect::from_min_max(
        pos2(timeline.left(), loop_rect.bottom()),
        pos2(timeline.right(), lanes_top),
    );
    tl_painter.rect_filled(ruler, 0.0, Color32::from_rgb(0x30, 0x30, 0x30));
    let bar_px = bpb as f32 * ppb;
    let label_every = ((40.0 / bar_px).ceil() as i64).max(1);
    let mut bar = (scroll_beats / bpb).floor() as i64;
    while (bar as f64) * bpb <= last {
        if bar % label_every == 0 {
            let x = x_of(bar as f64 * bpb);
            tl_painter.vline(x, ruler.y_range(), Stroke::new(1.0, theme::GRID_BAR));
            tl_painter.text(
                pos2(x + 3.0, ruler.center().y),
                egui::Align2::LEFT_CENTER,
                format!("{}", bar + 1),
                egui::FontId::proportional(11.0),
                theme::TEXT_DIM,
            );
        }
        bar += 1;
    }
    let ruler_resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click_and_drag());
    if ruler_resp.clicked() || ruler_resp.dragged() {
        if let Some(p) = ruler_resp.interact_pointer_pos() {
            actions.push(Action::SetPosition(snap(beat_of(p.x)).max(0.0)));
        }
    }

    // ----- lanes -----
    let lanes_painter = ui.painter_at(lanes_rect);
    let tl_lanes_painter = ui.painter_at(Rect::from_min_max(pos2(timeline.left(), lanes_top), area.max));
    let pointer = ui.input(|i| i.pointer.interact_pos());
    let drag_active = app.arrangement.drag;
    let mut new_drag = drag_active;
    for ti in 0..n_tracks {
        let y = lanes_top + offsets[ti] - scroll_y;
        if y + heights[ti] < lanes_top || y > area.bottom() {
            continue;
        }
        if heights[ti] > LANE_H {
            automation_lanes(
                app,
                ui,
                &mut actions,
                ti,
                y + LANE_H,
                Rect::from_min_max(pos2(area.left(), lanes_top), area.max),
                timeline.left(),
                (scroll_beats, ppb, grid),
                playhead,
            );
        }
        let lane = Rect::from_min_max(pos2(timeline.left(), y), pos2(timeline.right(), y + LANE_H));
        let id = app.project.tracks[ti].id;
        let kind = app.project.tracks[ti].kind;
        let selected_track = app.selected_track == Some(id);
        tl_lanes_painter.rect_filled(
            lane.shrink2(vec2(0.0, 0.5)),
            0.0,
            if ti % 2 == 0 {
                theme::BG_LANE
            } else {
                theme::BG_LANE_ALT
            },
        );
        // Re-draw grid on top of the lane background.
        let mut b = first;
        while b <= last {
            let is_bar = (b / bpb).fract().abs() < 1e-9;
            tl_lanes_painter.vline(
                x_of(b),
                lane.y_range(),
                Stroke::new(1.0, if is_bar { theme::GRID_BAR } else { theme::GRID }),
            );
            b += grid;
        }
        let lane_resp = ui.interact(lane, ui.id().with(("lane", id)), Sense::click());
        if lane_resp.clicked() {
            if let Some(p) = lane_resp.interact_pointer_pos() {
                actions.push(Action::SetPosition(snap(beat_of(p.x)).max(0.0)));
                actions.push(Action::SelectTrack(id));
            }
        }
        if let Some(item) = super::browser::drop_on(ui, lane) {
            // Samples land at the grid position under the pointer.
            let at = pointer.map(|p| snap(beat_of(p.x)).max(0.0)).unwrap_or(0.0);
            actions.push(Action::Drop(item, DropTarget::Arrangement(id, at)));
        }
        if lane_resp.double_clicked() && kind == TrackKind::Midi {
            if let Some(p) = lane_resp.interact_pointer_pos() {
                let start = (beat_of(p.x) / bpb).floor() * bpb;
                actions.push(Action::Create(id, start.max(0.0)));
            }
        }

        // Clips.
        let clip_count = app.project.tracks[ti].arrangement.len();
        for ci in 0..clip_count {
            let a = &app.project.tracks[ti].arrangement[ci];
            let (x0, x1) = (x_of(a.start), x_of(a.end()));
            if x1 < timeline.left() || x0 > timeline.right() {
                continue;
            }
            let rect = Rect::from_min_max(pos2(x0, y + 2.0), pos2(x1.max(x0 + 3.0), y + LANE_H - 2.0));
            let cref = ClipRef::Arrangement { track: id, id: a.id };
            let selected = app.selected_clip == Some(cref);
            draw_clip(app, &tl_lanes_painter, rect, ti, ci, selected);
            let a = &app.project.tracks[ti].arrangement[ci];
            let resp = ui.interact(rect, ui.id().with(("aclip", a.id)), Sense::click_and_drag());
            let near_edge = pointer.is_some_and(|p| (p.x - rect.right()).abs() < EDGE && rect.y_range().contains(p.y));
            if resp.hovered() && near_edge {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
            if resp.clicked() {
                actions.push(Action::Select(cref));
            }
            if resp.drag_started() {
                actions.push(Action::Select(cref));
                if let Some(p) = resp.interact_pointer_pos() {
                    new_drag = if (p.x - rect.right()).abs() < EDGE {
                        Some(ArrDrag::Resize { track: id, id: a.id })
                    } else {
                        Some(ArrDrag::Move {
                            track: id,
                            id: a.id,
                            grab_offset: beat_of(p.x) - a.start,
                        })
                    };
                }
            }
            resp.context_menu(|ui| {
                if ui.button("Delete").clicked() {
                    actions.push(Action::Delete(cref));
                    ui.close();
                }
            });
        }

        // Track header.
        let head = Rect::from_min_max(pos2(area.left(), y), pos2(timeline.left(), y + LANE_H)).shrink2(vec2(2.0, 1.0));
        let t = &mut app.project.tracks[ti];
        let color = theme::rgb(t.color);
        lanes_painter.rect_filled(
            head,
            2.0,
            if selected_track {
                Color32::from_rgb(0x50, 0x50, 0x50)
            } else {
                theme::BG_PANEL
            },
        );
        lanes_painter.rect_filled(Rect::from_min_size(head.min, vec2(5.0, head.height())), 2.0, color);
        let head_resp = ui.interact(head, ui.id().with(("ahead", id)), Sense::click());
        if head_resp.clicked() {
            actions.push(Action::SelectTrack(id));
        }
        let inner = head.shrink2(vec2(9.0, 4.0));
        let inner = Rect::from_min_max(pos2(inner.left() + 2.0, inner.top()), inner.max);
        ui.scope_builder(UiBuilder::new().max_rect(inner).id_salt(("ahead_ui", id)), |ui| {
            ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
            ui.label(egui::RichText::new(&t.name).color(theme::TEXT).strong());
            ui.horizontal(|ui| {
                if widgets::toggle(ui, !t.mixer.mute, &format!("{}", ti + 1), theme::ACTIVE, 24.0).clicked() {
                    t.mixer.mute = !t.mixer.mute;
                    actions.push(Action::Mixer(id));
                }
                if widgets::toggle(ui, t.mixer.solo, "S", theme::SOLO, 20.0).clicked() {
                    t.mixer.solo = !t.mixer.solo;
                    actions.push(Action::Mixer(id));
                }
                let open = heights[ti] > LANE_H;
                let has = t.automation.iter().any(|l| !l.envelope.is_empty());
                let label = if has { "A•" } else { "A" };
                if widgets::toggle(ui, open, label, theme::ACCENT, 22.0)
                    .on_hover_text("Show automation lanes")
                    .clicked()
                {
                    actions.push(Action::ToggleLanes(id));
                }
                if widgets::toggle(ui, t.mixer.arm, "●", theme::ARM, 20.0)
                    .on_hover_text("Arm for recording")
                    .clicked()
                {
                    t.mixer.arm = !t.mixer.arm;
                    actions.push(Action::Mixer(id));
                }
                let r = ui.add(
                    egui::DragValue::new(&mut t.mixer.volume_db)
                        .range(-70.0..=6.0)
                        .speed(0.2)
                        .fixed_decimals(1)
                        .suffix(" dB"),
                );
                if r.changed() {
                    actions.push(Action::Mixer(id));
                }
            });
        });
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
        let y = lanes_top + ti as f32 * LANE_H - scroll_y;
        if y + LANE_H < lanes_top || y > area.bottom() {
            continue;
        }
        let start = take.start_beat.load();
        let (x0, x1) = (x_of(start).max(timeline.left()), x_of(playhead).min(timeline.right()));
        if x1 > x0 {
            let r = Rect::from_min_max(pos2(x0, y + 2.0), pos2(x1, y + LANE_H - 2.0));
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

    // ----- active drag -----
    app.arrangement.drag = new_drag;
    let (down, pos, alt) = ui.input(|i| (i.pointer.primary_down(), i.pointer.interact_pos(), i.modifiers.alt));
    if let (Some(drag), Some(p)) = (app.arrangement.drag, pos) {
        let snapb = |b: f64| if alt { b } else { snap(b) };
        match drag {
            ArrDrag::Move { track, id, grab_offset } => {
                let start = snapb(beat_of(p.x) - grab_offset).max(0.0);
                if let Some(a) = app
                    .project
                    .track_mut(track)
                    .and_then(|t| t.arrangement.iter_mut().find(|a| a.id == id))
                {
                    a.start = start;
                }
            }
            ArrDrag::Resize { track, id } => {
                if let Some(a) = app
                    .project
                    .track_mut(track)
                    .and_then(|t| t.arrangement.iter_mut().find(|a| a.id == id))
                {
                    let end = snapb(beat_of(p.x)).max(a.start + grid.min(1.0));
                    a.length = end - a.start;
                }
            }
            ArrDrag::LoopMove { grab_offset } => {
                app.project.loop_start = snapb(beat_of(p.x) - grab_offset).max(0.0);
            }
            ArrDrag::LoopStart => {
                let end = app.project.loop_start + app.project.loop_length;
                let s = snapb(beat_of(p.x)).clamp(0.0, end - grid);
                app.project.loop_start = s;
                app.project.loop_length = end - s;
            }
            ArrDrag::LoopEnd => {
                let e = snapb(beat_of(p.x)).max(app.project.loop_start + grid);
                app.project.loop_length = e - app.project.loop_start;
            }
        }
        if !down {
            app.arrangement.drag = None;
            match drag {
                ArrDrag::Move { track, .. } => {
                    if let Some(t) = app.project.track_mut(track) {
                        t.sort_arrangement();
                    }
                    actions.push(Action::Changed);
                }
                ArrDrag::Resize { .. } => actions.push(Action::Changed),
                _ => actions.push(Action::Loop),
            }
        }
    }

    // ----- playhead and corner -----
    let px = x_of(playhead);
    if px >= timeline.left() && px <= timeline.right() {
        tl_painter.vline(px, ruler.top()..=area.bottom(), Stroke::new(1.5, theme::SELECT));
    }
    let corner = Rect::from_min_max(area.min, pos2(timeline.left(), lanes_top));
    painter.rect_filled(corner, 0.0, theme::BG_BAR);
    ui.scope_builder(
        UiBuilder::new().max_rect(corner.shrink(3.0)).id_salt("arr_corner"),
        |ui| {
            ui.horizontal(|ui| {
                if widgets::toggle(ui, app.arrangement.follow, "Follow", theme::ACCENT, 48.0).clicked() {
                    app.arrangement.follow = !app.arrangement.follow;
                }
                if ui.small_button("−").on_hover_text("Zoom out").clicked() {
                    app.arrangement.px_per_beat = (app.arrangement.px_per_beat / 1.5).max(1.0);
                }
                if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                    app.arrangement.px_per_beat = (app.arrangement.px_per_beat * 1.5).min(240.0);
                }
            });
        },
    );
    painter.text(
        pos2(timeline.left() + 6.0, lanes_top + tracks_h - scroll_y + 14.0),
        egui::Align2::LEFT_CENTER,
        "Double-click a MIDI lane to create a clip · drag clips to move · drag the right edge to resize · Ctrl+wheel to zoom · A shows automation",
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );

    for a in actions {
        match a {
            Action::Drop(item, target) => app.drop_item(&item, target),
            Action::Select(r) => {
                app.select_clip(r);
                app.detail = DetailMode::Clip;
            }
            Action::SelectTrack(t) => app.selected_track = Some(t),
            Action::SetPosition(b) => app.send(Command::SetPosition(b)),
            Action::Create(t, s) => app.create_arrangement_clip(t, s),
            Action::Delete(r) => app.delete_clip(r),
            Action::Mixer(t) => app.send_mixer(t),
            Action::Changed => app.changed(),
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
                if let Some(track) = app.project.track_mut(t) {
                    if !track.automation.iter().any(|l| l.target == target) {
                        track.automation.push(AutomationLane::new(target));
                    }
                }
                app.dirty = true;
            }
            Action::RemoveLane(t, i) => {
                if let Some(track) = app.project.track_mut(t) {
                    if i < track.automation.len() {
                        track.automation.remove(i);
                    }
                }
                app.changed();
            }
        }
    }
}

/// Draws the open automation lanes of track `ti`, starting at `top`, plus a
/// row to add another lane.
#[allow(clippy::too_many_arguments)]
fn automation_lanes(
    app: &mut crate::app::TesseraApp,
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
    ti: usize,
    top: f32,
    visible: Rect,
    timeline_left: f32,
    (scroll_beats, ppb, grid): (f64, f32, f64),
    playhead: f64,
) {
    let track = &app.project.tracks[ti];
    let id = track.id;
    let color = theme::rgb(track.color);
    let instrument = track.instrument;
    let n = track.automation.len();
    let targets: Vec<(AutomationTarget, String)> = app
        .automation_targets(id)
        .into_iter()
        .map(|t| (t, app.automation_target_name(id, t)))
        .collect();
    let painter = ui.painter_at(visible);
    for li in 0..=n {
        let y = top + li as f32 * AUTO_H;
        if y > visible.bottom() || y + AUTO_H < visible.top() {
            continue;
        }
        let row = Rect::from_min_max(pos2(visible.left(), y), pos2(visible.right(), y + AUTO_H));
        painter.rect_filled(row, 0.0, Color32::from_rgb(0x26, 0x26, 0x26));
        painter.hline(row.x_range(), row.bottom(), Stroke::new(1.0, theme::BG_VIEW));
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
        let mut b = (scroll_beats / grid).floor() * grid;
        while b <= scroll_beats + (body.width() / ppb) as f64 {
            let x = timeline_left + ((b - scroll_beats) as f32) * ppb;
            if x >= timeline_left {
                painter.vline(x, body.y_range(), Stroke::new(1.0, theme::GRID));
            }
            b += grid;
        }
        let head_visible = head.top() >= visible.top();
        if li == n {
            if head_visible {
                ui.scope_builder(UiBuilder::new().max_rect(head).id_salt(("add_lane", id)), |ui| {
                    let used: Vec<AutomationTarget> =
                        app.project.tracks[ti].automation.iter().map(|l| l.target).collect();
                    egui::ComboBox::from_id_salt(("add_lane_combo", id))
                        .selected_text(egui::RichText::new("+ Add lane").small())
                        .width(head.width() - 8.0)
                        .show_ui(ui, |ui| {
                            for (t, name) in &targets {
                                if !used.contains(t) && ui.selectable_label(false, name).clicked() {
                                    actions.push(Action::AddLane(id, *t));
                                }
                            }
                        });
                });
            }
            continue;
        }
        let spec = target_spec(app.project.tracks[ti].automation[li].target, &instrument);
        let name = app.automation_target_name(id, app.project.tracks[ti].automation[li].target);
        let lane = &mut app.project.tracks[ti].automation[li];
        if head_visible {
            ui.scope_builder(UiBuilder::new().max_rect(head).id_salt(("lane_head", id, li)), |ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 1.0);
                ui.horizontal(|ui| {
                    let text = egui::RichText::new(&name).small();
                    ui.label(if lane.overridden {
                        text.color(theme::TEXT_DIM).italics()
                    } else {
                        text.color(theme::TEXT)
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
                    ui.label(egui::RichText::new(text).small().color(theme::TEXT_DIM));
                }
            });
        }
        let view = envelope::View {
            rect: body,
            beat0: scroll_beats,
            px_per_beat: ppb,
            grid,
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
            actions.push(Action::Changed);
        }
        let _ = r.committed;
    }
}

/// Draws an arrangement clip with a title bar and a content preview.
fn draw_clip(app: &mut TesseraApp, p: &egui::Painter, rect: Rect, ti: usize, ci: usize, selected: bool) {
    let a = app.project.tracks[ti].arrangement[ci].clone();
    let color = theme::rgb(a.clip.color);
    p.rect_filled(rect, 2.0, theme::dim(color, 0.45));
    let title = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + 14.0));
    p.rect_filled(title, 2.0, color);
    p.text(
        pos2(title.left() + 4.0, title.center().y),
        egui::Align2::LEFT_CENTER,
        &a.clip.name,
        egui::FontId::proportional(10.5),
        Color32::BLACK,
    );
    let body = Rect::from_min_max(
        pos2(rect.left(), title.bottom() + 2.0),
        pos2(rect.right(), rect.bottom() - 2.0),
    );
    let ppb = rect.width() / a.length.max(1e-6) as f32;
    let len = a.clip.length.max(1e-6);
    // Loop boundaries inside the clip.
    let mut rep = len;
    while rep < a.length {
        let x = rect.left() + rep as f32 * ppb;
        p.vline(x, body.y_range(), Stroke::new(1.0, theme::dim(color, 0.7)));
        rep += len;
    }
    match &a.clip.content {
        ClipContent::Midi { notes } if !notes.is_empty() => {
            let lo = notes.iter().map(|n| n.pitch).min().unwrap_or(60) as f32;
            let hi = notes.iter().map(|n| n.pitch).max().unwrap_or(60) as f32;
            let span = (hi - lo + 1.0).max(8.0);
            let row = body.height() / span;
            let reps = if a.clip.looping {
                (a.length / len).ceil() as usize
            } else {
                1
            };
            for r in 0..reps {
                let offset = r as f64 * len;
                for n in notes {
                    let s = offset + n.start;
                    if s >= a.length {
                        continue;
                    }
                    let e = (s + n.length.min(len - n.start)).min(a.length);
                    let y = body.bottom() - (n.pitch as f32 - lo + 1.0) * row;
                    let nr = Rect::from_min_max(
                        pos2(rect.left() + s as f32 * ppb, y),
                        pos2(
                            (rect.left() + e as f32 * ppb).max(rect.left() + s as f32 * ppb + 1.5),
                            y + row.max(1.5),
                        ),
                    );
                    p.rect_filled(nr, 0.0, color);
                }
            }
        }
        ClipContent::Audio(audio) => {
            if let Some(data) = &audio.data {
                let peaks = app.waveform_peaks(data);
                let secs_per_beat = 60.0 / app.project.tempo;
                let warp = audio.warp.clone();
                let dur = data.duration_secs();
                let cy = body.center().y;
                let h = body.height() / 2.0;
                let step = 2.0;
                let mut x = rect.left();
                while x < rect.right() {
                    let beat = ((x - rect.left()) / ppb) as f64;
                    let pos = if a.clip.looping { beat % len } else { beat };
                    let t = if warp.enabled {
                        warp.beat_to_secs(pos)
                    } else {
                        pos * secs_per_beat * warp.rate()
                    };
                    if (0.0..dur).contains(&t) && pos < len {
                        let idx = ((t / dur) * peaks.len() as f64) as usize;
                        let v = peaks.get(idx).copied().unwrap_or(0.0).min(1.0);
                        p.vline(x, (cy - v * h)..=(cy + v * h), Stroke::new(1.0, color));
                    }
                    x += step;
                }
            }
        }
        _ => {}
    }
    if selected {
        p.rect_stroke(rect, 2.0, Stroke::new(1.5, theme::SELECT), StrokeKind::Inside);
    }
}
