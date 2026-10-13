//! Session view: the grid of clip slots and scenes, plus the mixer.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind, UiBuilder};
use tessera_engine::arrange;
use tessera_engine::engine::TrackShared;
use tessera_engine::model::{CrossfadeAssign, Id, SceneFollowAction, TrackKind, MASTER_BUS};
use tessera_engine::Command;

use super::info_view::{self, InfoExt};
use super::{theme, widgets};
use crate::app::{ClipRef, DetailMode, TesseraApp};
use crate::browser::{BrowserItem, DropTarget};
use crate::workflow::ClipDrag;

const COL_W: f32 = 112.0;
const MASTER_W: f32 = 130.0;
const HEADER_H: f32 = 22.0;
const ROW_H: f32 = 24.0;
const STOP_H: f32 = 22.0;
const MIXER_H: f32 = 200.0;
/// Height of the routing / crossfade / delay rows under each mixer strip.
const EXTRA_H: f32 = 44.0;
/// Height of one row of send knobs above the mixer.
const SEND_ROW_H: f32 = 34.0;
const SENDS_PER_ROW: usize = 4;
const TRI_W: f32 = 18.0;
/// Indent of a group member's header per nesting level.
const INDENT: f32 = 5.0;
/// Frost over frozen tracks.
const FROST: Color32 = Color32::from_rgba_premultiplied(40, 60, 80, 60);
const FROST_MARK: Color32 = Color32::from_rgb(0xb8, 0xe0, 0xff);

/// Track colours offered in the context menus.
pub const PALETTE: [[u8; 3]; 16] = [
    [0xff, 0x94, 0xa6],
    [0xff, 0xa5, 0x29],
    [0xcc, 0x99, 0x27],
    [0xf7, 0xf4, 0x7c],
    [0xbf, 0xfb, 0x00],
    [0x1a, 0xff, 0x2f],
    [0x25, 0xff, 0xa8],
    [0x5c, 0xff, 0xe8],
    [0x8b, 0xc5, 0xff],
    [0x54, 0x80, 0xe4],
    [0x92, 0xa7, 0xff],
    [0xd8, 0x6c, 0xe4],
    [0xe5, 0x53, 0xa0],
    [0xff, 0xff, 0xff],
    [0xb0, 0x8b, 0x6b],
    [0x9a, 0x9a, 0x9a],
];

enum Action {
    Launch(Id, usize),
    StopTrack(Id),
    LaunchScene(usize),
    StopAll,
    Select(ClipRef),
    SelectTrack(Id),
    ToggleTrackSelection(Id),
    /// Right-click on a header: select it unless it's already selected.
    ContextSelect(Id),
    Create(Id, usize),
    Delete(ClipRef),
    Duplicate(Id, usize),
    ToArrangement(Id, usize),
    SaveToLibrary(ClipRef),
    ExportMidi(ClipRef),
    ImportDialog(Id, usize),
    Import(PathBuf, Id, usize),
    DuplicateTracks,
    DeleteTracks,
    GroupSelected,
    Ungroup(Id),
    ToggleFold(Id),
    LaunchGroup(Id, usize),
    StopGroup(Id),
    Freeze(Id),
    Unfreeze(Id),
    Flatten(Id),
    Nudge(Id, i32),
    TrackColor(Id, [u8; 3]),
    SelectScene(usize),
    InsertScene(usize),
    DeleteScene(usize),
    CaptureScene,
    AddScene,
    ToggleStop(Id, usize),
    Mixer(Id),
    Master,
    /// A labelled edit that changes what the engine plays.
    Changed(&'static str),
    /// A labelled edit that only changes the document (names).
    Touched(&'static str),
    RecordSlot(Id, usize),
    Send(Id, usize, f32),
    BusMixer(Id),
    DeleteReturn(Id),
    AddReturn,
    Drop(BrowserItem, DropTarget),
    Crossfade(Id, CrossfadeAssign),
    Delay(Id, f32),
    Crossfader(f32),
    Cue(&'static str),
    MoveSlot {
        from: (Id, usize),
        to: (Id, usize),
        copy: bool,
    },
    ArrangementToSlot {
        track: Id,
        id: Id,
        target: Id,
        scene: usize,
    },
}

/// What a mixer strip belongs to.
#[derive(Clone, Copy)]
pub enum StripKind {
    /// A track, with its 1-based number.
    Track(usize),
    /// A return track, with its letter.
    Return(char),
    Master,
}

/// One visible track column.
struct Col {
    ti: usize,
    id: Id,
    depth: usize,
    /// Colour of the group the track sits in.
    group_color: Option<Color32>,
    group_name: Option<String>,
    is_group: bool,
    folded: bool,
    frozen: bool,
    /// For a group: which scenes have a clip on any member.
    group_has: Vec<bool>,
    /// For a group: member tracks (indices).
    members: Vec<usize>,
}

/// Published engine state for track `index`, if it belongs to track `id`.
pub fn track_state(app: &TesseraApp, index: usize, id: Id) -> Option<&TrackShared> {
    let ts = app.engine.shared.tracks.get(index)?;
    (ts.id.load(Ordering::Relaxed) == id).then_some(ts)
}

/// Playing and queued slot of a track (-1 when none).
fn slot_state(app: &TesseraApp, ti: usize) -> (i32, i32) {
    let id = app.project.tracks[ti].id;
    track_state(app, ti, id)
        .map(|s| {
            (
                s.playing_slot.load(Ordering::Relaxed),
                s.queued_slot.load(Ordering::Relaxed),
            )
        })
        .unwrap_or((-1, -1))
}

fn columns(app: &TesseraApp) -> Vec<Col> {
    let p = &app.project;
    let n_scenes = p.scenes.len();
    p.tracks
        .iter()
        .enumerate()
        .filter(|(_, t)| !arrange::hidden(p, t.id))
        .map(|(ti, t)| {
            let parent = t.output.and_then(|g| p.track(g));
            let is_group = t.kind == TrackKind::Group;
            let members: Vec<usize> = if is_group {
                arrange::track_block(p, t.id)
                    .into_iter()
                    .skip(1)
                    .filter(|&i| p.tracks[i].kind != TrackKind::Group)
                    .collect()
            } else {
                Vec::new()
            };
            let group_has = (0..n_scenes)
                .map(|si| {
                    members
                        .iter()
                        .any(|&i| p.tracks[i].slots.get(si).is_some_and(|s| s.is_some()))
                })
                .collect();
            Col {
                ti,
                id: t.id,
                depth: arrange::depth(p, t.id),
                group_color: parent.map(|g| theme::rgb(g.color)),
                group_name: parent.map(|g| g.name.clone()),
                is_group,
                folded: t.folded,
                frozen: t.frozen,
                group_has,
                members,
            }
        })
        .collect()
}

/// Takes a dragged clip dropped on `rect`, highlighting the target while
/// one hovers over it.
fn clip_drop_on(ui: &egui::Ui, rect: Rect) -> Option<ClipDrag> {
    let ctx = ui.ctx();
    if !egui::DragAndDrop::has_payload_of_type::<ClipDrag>(ctx) {
        return None;
    }
    let pos = ctx.pointer_latest_pos()?;
    if !rect.contains(pos) || !ui.clip_rect().contains(pos) {
        return None;
    }
    ui.painter()
        .rect_stroke(rect, 2.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside);
    if ctx.input(|i| i.pointer.any_released()) {
        egui::DragAndDrop::take_payload::<ClipDrag>(ctx).map(|d| *d)
    } else {
        None
    }
}

/// Draws the name of a Session clip being dragged next to the pointer.
fn clip_drag_preview(app: &TesseraApp, ctx: &egui::Context) {
    let Some(drag) = egui::DragAndDrop::payload::<ClipDrag>(ctx) else {
        return;
    };
    let ClipDrag::Slot { track, scene } = *drag else {
        return;
    };
    let Some(clip) = app.clip(ClipRef::Slot { track, scene }) else {
        return;
    };
    let Some(pos) = ctx.pointer_latest_pos() else { return };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("clip_drag")));
    let r = Rect::from_min_size(pos + vec2(12.0, 4.0), vec2(COL_W - 2.0, ROW_H - 2.0));
    painter.rect_filled(r, 2.0, theme::rgb(clip.color).gamma_multiply(0.85));
    painter.text(
        pos2(r.left() + 6.0, r.center().y),
        egui::Align2::LEFT_CENTER,
        &clip.name,
        egui::FontId::proportional(11.0),
        Color32::BLACK,
    );
}

/// A context menu that stays open while its fields are edited (checkboxes,
/// values); buttons close it themselves.
fn sticky_menu(resp: &egui::Response, add_contents: impl FnOnce(&mut egui::Ui)) {
    egui::Popup::context_menu(resp)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(add_contents);
}

/// A grid of colour swatches; returns the one clicked.
fn color_palette(ui: &mut egui::Ui, current: Option<[u8; 3]>) -> Option<[u8; 3]> {
    let mut picked = None;
    ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
    for row in PALETTE.chunks(8) {
        ui.horizontal(|ui| {
            for &c in row {
                let (r, resp) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
                ui.painter().rect_filled(r, 2.0, theme::rgb(c));
                if current == Some(c) || resp.hovered() {
                    ui.painter()
                        .rect_stroke(r, 2.0, Stroke::new(1.5, theme::select()), StrokeKind::Inside);
                }
                if resp.clicked() {
                    picked = Some(c);
                }
            }
        });
    }
    picked
}

/// Disclosure triangle of a group track: pointing right when folded.
fn fold_triangle(p: &egui::Painter, c: egui::Pos2, folded: bool, color: Color32) {
    let h = 4.0;
    let pts = if folded {
        vec![
            pos2(c.x - h * 0.7, c.y - h),
            pos2(c.x + h * 0.8, c.y),
            pos2(c.x - h * 0.7, c.y + h),
        ]
    } else {
        vec![
            pos2(c.x - h, c.y - h * 0.7),
            pos2(c.x + h, c.y - h * 0.7),
            pos2(c.x, c.y + h * 0.8),
        ]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// A small six-armed star marking frozen tracks.
fn snowflake(p: &egui::Painter, c: egui::Pos2, r: f32, color: Color32) {
    let stroke = Stroke::new(1.2, color);
    for k in 0..3 {
        let a = k as f32 * std::f32::consts::PI / 3.0 + std::f32::consts::FRAC_PI_2;
        let d = vec2(a.cos(), a.sin()) * r;
        p.line_segment([c - d, c + d], stroke);
    }
}

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let mut actions: Vec<Action> = Vec::new();
    let cols = columns(app);
    let n_cols = cols.len();
    let n_scenes = app.project.scenes.len();
    let n_ret = app.project.returns.len();
    let ret_x = n_cols as f32 * COL_W;
    let master_x = (n_cols + n_ret) as f32 * COL_W;
    let total_w = master_x + MASTER_W + 8.0;
    let send_rows = n_ret.div_ceil(SENDS_PER_ROW);
    let mixer_h = MIXER_H + EXTRA_H + send_rows as f32 * SEND_ROW_H;
    let time = ui.input(|i| i.time);
    let blink = (time * 4.0).fract() < 0.5;
    let command = ui.input(|i| i.modifiers.command);
    let cue_mode = app.project.cue.cue_mode;
    let selected_scene = app.work.selected_scene;
    // Playing / queued slot of every track, for group launch buttons.
    let states: Vec<(i32, i32)> = (0..app.project.tracks.len()).map(|ti| slot_state(app, ti)).collect();

    // Dropped files land on the slot under the pointer.
    let (dropped, hovering_files, pointer) = ui.input(|i| {
        (
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .filter(|p| !p.as_os_str().is_empty())
                .collect::<Vec<_>>(),
            !i.raw.hovered_files.is_empty(),
            i.pointer.latest_pos(),
        )
    });

    egui::ScrollArea::horizontal()
        .id_salt("session_h")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let avail_h = ui.available_height();

            // ----- track headers -----
            let (hdr, _) = ui.allocate_exact_size(vec2(total_w, HEADER_H), Sense::hover());
            for (ci, col) in cols.iter().enumerate() {
                let r = Rect::from_min_size(pos2(hdr.left() + ci as f32 * COL_W, hdr.top()), vec2(COL_W, HEADER_H))
                    .shrink(1.0);
                let id = col.id;
                let selected = app.selected_track == Some(id) || app.work.selected_tracks.contains(&id);
                let resp = ui
                    .interact(r, ui.id().with(("hdr", id)), Sense::click())
                    .info("Track Title", info_view::TRACK_TITLE);
                let t = &mut app.project.tracks[col.ti];
                let color = theme::rgb(t.color);
                let painter = ui.painter_at(r);
                painter.rect_filled(r, 2.0, if selected { color } else { theme::dim(color, 0.75) });
                if selected {
                    painter.rect_stroke(r, 2.0, Stroke::new(1.0, theme::select()), StrokeKind::Inside);
                }
                // Group members carry their group's colour on the left.
                let indent = col.depth as f32 * INDENT;
                if let Some(gc) = col.group_color {
                    let strip = Rect::from_min_size(r.min, vec2(indent.max(3.0), r.height()));
                    painter.rect_filled(strip, 0.0, gc);
                    painter.vline(
                        strip.right(),
                        r.y_range(),
                        Stroke::new(1.0, Color32::from_black_alpha(120)),
                    );
                }
                let mut text_x = r.left() + indent + 5.0;
                if col.is_group {
                    let fold_rect = Rect::from_min_size(pos2(r.left() + indent, r.top()), vec2(14.0, r.height()));
                    let fold = ui.interact(fold_rect, ui.id().with(("fold", id)), Sense::click());
                    fold_triangle(
                        &painter,
                        fold_rect.center() + vec2(2.0, 0.0),
                        col.folded,
                        if fold.hovered() { Color32::WHITE } else { Color32::BLACK },
                    );
                    if fold
                        .on_hover_text(if col.folded { "Unfold group" } else { "Fold group" })
                        .clicked()
                    {
                        actions.push(Action::ToggleFold(id));
                    }
                    text_x += 12.0;
                }
                let right = if col.frozen { r.right() - 14.0 } else { r.right() - 2.0 };
                let name_painter = ui.painter_at(Rect::from_x_y_ranges(r.left()..=right, r.y_range()));
                name_painter.text(
                    pos2(text_x, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    &t.name,
                    egui::FontId::proportional(12.0),
                    Color32::BLACK,
                );
                if col.frozen {
                    painter.rect_filled(r, 2.0, FROST);
                    let c = pos2(r.right() - 8.0, r.center().y);
                    painter.circle_filled(c, 6.0, Color32::from_rgb(0x30, 0x48, 0x60));
                    snowflake(&painter, c, 4.5, FROST_MARK);
                }
                if resp.clicked() {
                    if command {
                        actions.push(Action::ToggleTrackSelection(id));
                    } else {
                        actions.push(Action::SelectTrack(id));
                    }
                }
                if resp.secondary_clicked() {
                    actions.push(Action::ContextSelect(id));
                }
                if !col.frozen {
                    if let Some(item) = super::browser::drop_on(ui, r) {
                        actions.push(Action::Drop(item, DropTarget::Track(id)));
                    }
                }
                let resp = if col.frozen {
                    resp.on_hover_text("Frozen: unfreeze to edit")
                } else {
                    resp
                };
                let (is_group, frozen) = (col.is_group, col.frozen);
                sticky_menu(&resp, |ui| {
                    ui.label("Name");
                    if ui.text_edit_singleline(&mut t.name).changed() {
                        actions.push(Action::Touched("Rename Track"));
                    }
                    if let Some(c) = color_palette(ui, Some(t.color)) {
                        actions.push(Action::TrackColor(id, c));
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Duplicate").clicked() {
                        actions.push(Action::DuplicateTracks);
                        ui.close();
                    }
                    if ui.button("Delete").clicked() {
                        actions.push(Action::DeleteTracks);
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Group Tracks  Ctrl+G").clicked() {
                        actions.push(Action::GroupSelected);
                        ui.close();
                    }
                    if is_group && ui.button("Ungroup").clicked() {
                        actions.push(Action::Ungroup(id));
                        ui.close();
                    }
                    if !is_group {
                        ui.separator();
                        if frozen {
                            if ui.button("Unfreeze Track").clicked() {
                                actions.push(Action::Unfreeze(id));
                                ui.close();
                            }
                            if ui.button("Flatten").clicked() {
                                actions.push(Action::Flatten(id));
                                ui.close();
                            }
                        } else if ui.button("Freeze Track").clicked() {
                            actions.push(Action::Freeze(id));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button("Move Left").clicked() {
                        actions.push(Action::Nudge(id, -1));
                        ui.close();
                    }
                    if ui.button("Move Right").clicked() {
                        actions.push(Action::Nudge(id, 1));
                        ui.close();
                    }
                    super::chrome::track_menu_items(ui, id, false, false);
                });
            }
            for ri in 0..n_ret {
                let r = Rect::from_min_size(
                    pos2(hdr.left() + ret_x + ri as f32 * COL_W, hdr.top()),
                    vec2(COL_W, HEADER_H),
                )
                .shrink(1.0);
                let ret = &mut app.project.returns[ri];
                let id = ret.id;
                let selected = app.selected_track == Some(id);
                let resp = ui
                    .interact(r, ui.id().with(("rhdr", id)), Sense::click())
                    .info("Return Track", info_view::RETURN_TITLE);
                let color = theme::rgb(ret.color);
                ui.painter()
                    .rect_filled(r, 2.0, if selected { color } else { theme::dim(color, 0.7) });
                ui.painter_at(r).text(
                    pos2(r.left() + 5.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    &ret.name,
                    egui::FontId::proportional(12.0),
                    Color32::BLACK,
                );
                if resp.clicked() {
                    actions.push(Action::SelectTrack(id));
                }
                if let Some(item) = super::browser::drop_on(ui, r) {
                    actions.push(Action::Drop(item, DropTarget::Track(id)));
                }
                sticky_menu(&resp, |ui| {
                    ui.label("Name");
                    if ui.text_edit_singleline(&mut ret.name).changed() {
                        actions.push(Action::Touched("Rename Return Track"));
                    }
                    if ui.button("Insert Return Track").clicked() {
                        actions.push(Action::AddReturn);
                        ui.close();
                    }
                    if ui.button("Delete Return Track").clicked() {
                        actions.push(Action::DeleteReturn(id));
                        ui.close();
                    }
                });
            }
            let mr = Rect::from_min_size(pos2(hdr.left() + master_x, hdr.top()), vec2(MASTER_W, HEADER_H)).shrink(1.0);
            let master_resp = ui
                .interact(mr, ui.id().with("master_hdr"), Sense::click())
                .info("Master Track", info_view::MASTER_TITLE);
            if master_resp.clicked() {
                actions.push(Action::SelectTrack(MASTER_BUS));
            }
            if let Some(item) = super::browser::drop_on(ui, mr) {
                actions.push(Action::Drop(item, DropTarget::Track(MASTER_BUS)));
            }
            master_resp.context_menu(|ui| {
                if ui.button("Insert Return Track").clicked() {
                    actions.push(Action::AddReturn);
                    ui.close();
                }
            });
            ui.painter().rect_filled(
                mr,
                2.0,
                if app.selected_track == Some(MASTER_BUS) {
                    theme::gray(0x8a)
                } else {
                    theme::gray(0x5a)
                },
            );
            ui.painter().text(
                pos2(mr.left() + 5.0, mr.center().y),
                egui::Align2::LEFT_CENTER,
                "Master",
                egui::FontId::proportional(12.0),
                theme::text(),
            );

            // ----- clip slots -----
            let slots_h = (avail_h - HEADER_H - STOP_H - mixer_h).max(ROW_H * 2.0);
            egui::ScrollArea::vertical()
                .id_salt("session_v")
                .auto_shrink([false, false])
                .max_height(slots_h)
                .min_scrolled_height(slots_h)
                .show(ui, |ui| {
                    let (grid, _) =
                        ui.allocate_exact_size(vec2(total_w, (n_scenes + 1) as f32 * ROW_H), Sense::hover());
                    for (ci, col) in cols.iter().enumerate() {
                        let t = &app.project.tracks[col.ti];
                        let id = t.id;
                        let state = track_state(app, col.ti, id);
                        let (playing, queued) = states[col.ti];
                        let clip_pos = state.map(|s| s.clip_position.load()).unwrap_or(0.0);
                        let track_busy = playing >= 0 || queued >= 0;
                        let can_record =
                            matches!(t.kind, TrackKind::Audio | TrackKind::Midi) && t.mixer.arm && !col.frozen;
                        // Session take on this track: (slot, recording?)
                        let take = app.engine.shared.record.take_for(id).and_then(|tk| {
                            let slot = tk.slot.load(Ordering::Relaxed);
                            (slot >= 0).then(|| (slot as usize, tk.recording.load(Ordering::Relaxed)))
                        });
                        for si in 0..n_scenes {
                            let cell = Rect::from_min_size(
                                pos2(grid.left() + ci as f32 * COL_W, grid.top() + si as f32 * ROW_H),
                                vec2(COL_W, ROW_H),
                            )
                            .shrink(1.0);
                            let r = ClipRef::Slot { track: id, scene: si };
                            let selected = app.selected_clip == Some(r);
                            let painter = ui.painter_at(cell.expand(1.0));
                            let has_clip = t.slots.get(si).is_some_and(|s| s.is_some());
                            let sense = if has_clip && !col.frozen {
                                Sense::click_and_drag()
                            } else {
                                Sense::click()
                            };
                            let body = ui
                                .interact(cell, ui.id().with(("slot", id, si)), sense)
                                .info("Clip Slot", info_view::slot_text(has_clip, t.kind, can_record));
                            let tri_rect = Rect::from_min_size(cell.min, vec2(TRI_W, cell.height()));
                            let has_stop = !t.no_stop_slots.contains(&si);
                            let editable = !col.is_group && !col.frozen;

                            if editable {
                                if hovering_files && pointer.is_some_and(|p| cell.contains(p)) {
                                    painter.rect_stroke(cell, 2.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside);
                                }
                                if !dropped.is_empty() && pointer.is_some_and(|p| cell.contains(p)) {
                                    actions.push(Action::Import(dropped[0].clone(), id, si));
                                }
                                if let Some(item) = super::browser::drop_on(ui, cell) {
                                    actions.push(Action::Drop(item, DropTarget::Slot(id, si)));
                                }
                                match clip_drop_on(ui, cell) {
                                    Some(ClipDrag::Slot { track, scene }) => actions.push(Action::MoveSlot {
                                        from: (track, scene),
                                        to: (id, si),
                                        copy: command,
                                    }),
                                    Some(ClipDrag::Arrangement { track, id: clip }) => {
                                        actions.push(Action::ArrangementToSlot {
                                            track,
                                            id: clip,
                                            target: id,
                                            scene: si,
                                        })
                                    }
                                    None => {}
                                }
                            }

                            if col.is_group {
                                // The group's slot launches its members' clips in the scene.
                                if col.group_has[si] {
                                    let gc = theme::rgb(t.color);
                                    let member_playing = col.members.iter().any(|&m| states[m].0 == si as i32);
                                    let member_queued = col.members.iter().any(|&m| states[m].1 == si as i32);
                                    painter.rect_filled(
                                        cell,
                                        2.0,
                                        if member_playing {
                                            theme::dim(gc, 0.9)
                                        } else {
                                            theme::dim(gc, 0.55)
                                        },
                                    );
                                    let tri_resp =
                                        ui.interact(tri_rect, ui.id().with(("gtri", id, si)), Sense::click());
                                    let c = if member_playing || (member_queued && blink) {
                                        theme::PLAY
                                    } else if tri_resp.hovered() {
                                        Color32::WHITE
                                    } else {
                                        theme::gray(0x20)
                                    };
                                    widgets::triangle(&painter, tri_rect.center(), 10.0, c);
                                    // Thin bars hint at the member clips.
                                    let n = col.members.len().max(1);
                                    let area = Rect::from_min_max(
                                        pos2(cell.left() + TRI_W + 2.0, cell.top() + 5.0),
                                        pos2(cell.right() - 4.0, cell.bottom() - 5.0),
                                    );
                                    let h = area.height() / n as f32;
                                    for (k, &m) in col.members.iter().enumerate() {
                                        let mt = &app.project.tracks[m];
                                        if let Some(Some(clip)) = mt.slots.get(si) {
                                            let bar = Rect::from_min_size(
                                                pos2(area.left(), area.top() + k as f32 * h),
                                                vec2(area.width(), (h - 1.0).max(1.0)),
                                            );
                                            painter.rect_filled(bar, 1.0, theme::rgb(clip.color));
                                        }
                                    }
                                    if tri_resp.on_hover_text("Launch group clips").clicked() || body.double_clicked() {
                                        actions.push(Action::LaunchGroup(id, si));
                                    }
                                } else {
                                    painter.rect_filled(cell, 2.0, theme::bg_slot());
                                    if has_stop {
                                        let stop_resp =
                                            ui.interact(tri_rect, ui.id().with(("gstop", id, si)), Sense::click());
                                        let c = if stop_resp.hovered() {
                                            theme::text()
                                        } else {
                                            theme::dim(theme::text_dim(), 0.7)
                                        };
                                        widgets::stop_square(&painter, tri_rect.center(), 7.0, c);
                                        if stop_resp.on_hover_text("Stop group clips").clicked() {
                                            actions.push(Action::StopGroup(id));
                                        }
                                    }
                                    body.context_menu(|ui| {
                                        let label = if has_stop {
                                            "Remove Stop Button"
                                        } else {
                                            "Add Stop Button"
                                        };
                                        if ui.button(label).clicked() {
                                            actions.push(Action::ToggleStop(id, si));
                                            ui.close();
                                        }
                                    });
                                }
                                if body.clicked() {
                                    actions.push(Action::SelectTrack(id));
                                }
                            } else {
                                match &t.slots[si] {
                                    Some(clip) => {
                                        let color = theme::rgb(clip.color);
                                        let is_playing = playing == si as i32;
                                        let is_queued = queued == si as i32;
                                        painter.rect_filled(
                                            cell,
                                            2.0,
                                            if is_playing { color } else { theme::dim(color, 0.8) },
                                        );
                                        let tri_resp = ui
                                            .interact(tri_rect, ui.id().with(("tri", id, si)), Sense::click())
                                            .info("Clip Launch", info_view::CLIP_LAUNCH);
                                        let tri_color = if is_playing || (is_queued && blink) {
                                            theme::PLAY
                                        } else if tri_resp.hovered() {
                                            Color32::WHITE
                                        } else {
                                            theme::gray(0x20)
                                        };
                                        widgets::triangle(&painter, tri_rect.center(), 10.0, tri_color);
                                        painter.text(
                                            pos2(cell.left() + TRI_W + 2.0, cell.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            &clip.name,
                                            egui::FontId::proportional(11.0),
                                            Color32::BLACK,
                                        );
                                        if is_playing && clip.length > 0.0 {
                                            let frac = (clip_pos / clip.length).clamp(0.0, 1.0) as f32;
                                            let bar = Rect::from_min_max(
                                                pos2(cell.left(), cell.bottom() - 3.0),
                                                pos2(cell.left() + cell.width() * frac, cell.bottom()),
                                            );
                                            painter.rect_filled(
                                                bar,
                                                0.0,
                                                Color32::from_rgba_unmultiplied(0, 0, 0, 140),
                                            );
                                        }
                                        // Clips launch on press (Gate and Repeat clips stop on release).
                                        let pressed = tri_resp.hovered() && ui.input(|i| i.pointer.primary_pressed());
                                        if pressed || body.double_clicked() {
                                            actions.push(Action::Launch(id, si));
                                        } else if body.clicked() {
                                            actions.push(Action::Select(r));
                                        }
                                        if body.drag_started() {
                                            egui::DragAndDrop::set_payload(
                                                ui.ctx(),
                                                ClipDrag::Slot { track: id, scene: si },
                                            );
                                        }
                                        let frozen = col.frozen;
                                        body.context_menu(|ui| {
                                            if ui.button("Launch").clicked() {
                                                actions.push(Action::Launch(id, si));
                                                ui.close();
                                            }
                                            if frozen {
                                                ui.label(
                                                    egui::RichText::new("Frozen: unfreeze to edit")
                                                        .small()
                                                        .color(theme::text_dim()),
                                                );
                                                return;
                                            }
                                            if ui.button("Duplicate").clicked() {
                                                actions.push(Action::Duplicate(id, si));
                                                ui.close();
                                            }
                                            if ui.button("Add to Arrangement").clicked() {
                                                actions.push(Action::ToArrangement(id, si));
                                                ui.close();
                                            }
                                            if ui.button("Save to Library").clicked() {
                                                actions.push(Action::SaveToLibrary(r));
                                                ui.close();
                                            }
                                            if clip.is_midi() && ui.button("Export MIDI Clip…").clicked() {
                                                actions.push(Action::ExportMidi(r));
                                                ui.close();
                                            }
                                            if ui.button("Delete").clicked() {
                                                actions.push(Action::Delete(r));
                                                ui.close();
                                            }
                                        });
                                    }
                                    None if take.map(|t| t.0) == Some(si) => {
                                        let recording = take.map(|t| t.1).unwrap_or(false);
                                        painter.rect_filled(
                                            cell,
                                            2.0,
                                            if recording {
                                                theme::dim(theme::ARM, 0.8)
                                            } else {
                                                theme::bg_slot()
                                            },
                                        );
                                        if recording || blink {
                                            painter.circle_filled(tri_rect.center(), 5.0, theme::ARM);
                                        }
                                        painter.text(
                                            pos2(cell.left() + TRI_W + 2.0, cell.center().y),
                                            egui::Align2::LEFT_CENTER,
                                            if recording { "Recording" } else { "Waiting…" },
                                            egui::FontId::proportional(11.0),
                                            if recording { Color32::BLACK } else { theme::text_dim() },
                                        );
                                        if body.clicked() {
                                            // Stop recording and play the new clip.
                                            actions.push(Action::Launch(id, si));
                                        }
                                    }
                                    None => {
                                        painter.rect_filled(cell, 2.0, theme::bg_slot());
                                        if can_record {
                                            let rec_resp =
                                                ui.interact(tri_rect, ui.id().with(("rec", id, si)), Sense::click());
                                            let c = if rec_resp.hovered() {
                                                theme::ARM
                                            } else {
                                                theme::dim(theme::ARM, 0.6)
                                            };
                                            painter.circle_filled(tri_rect.center(), 5.0, c);
                                            if rec_resp.info("Session Record", info_view::SLOT_RECORD).clicked() {
                                                actions.push(Action::RecordSlot(id, si));
                                            }
                                        } else if has_stop {
                                            let stop_resp =
                                                ui.interact(tri_rect, ui.id().with(("stop", id, si)), Sense::click());
                                            let c = if stop_resp.hovered() {
                                                theme::text()
                                            } else if track_busy {
                                                theme::text_dim()
                                            } else {
                                                theme::dim(theme::text_dim(), 0.7)
                                            };
                                            widgets::stop_square(&painter, tri_rect.center(), 7.0, c);
                                            if stop_resp.info("Clip Stop", info_view::CLIP_STOP).clicked() {
                                                actions.push(Action::StopTrack(id));
                                            }
                                        }
                                        if body.double_clicked() && editable {
                                            match t.kind {
                                                TrackKind::Midi => actions.push(Action::Create(id, si)),
                                                TrackKind::Audio => actions.push(Action::ImportDialog(id, si)),
                                                TrackKind::Group => {}
                                            }
                                        } else if body.clicked() {
                                            actions.push(Action::Select(r));
                                        }
                                        let kind = t.kind;
                                        body.context_menu(|ui| {
                                            if editable {
                                                match kind {
                                                    TrackKind::Midi => {
                                                        if ui.button("Insert MIDI Clip").clicked() {
                                                            actions.push(Action::Create(id, si));
                                                            ui.close();
                                                        }
                                                    }
                                                    TrackKind::Audio => {
                                                        if ui.button("Import WAV…").clicked() {
                                                            actions.push(Action::ImportDialog(id, si));
                                                            ui.close();
                                                        }
                                                    }
                                                    TrackKind::Group => {}
                                                }
                                            }
                                            let label = if has_stop {
                                                "Remove Stop Button"
                                            } else {
                                                "Add Stop Button"
                                            };
                                            if ui.button(label).clicked() {
                                                actions.push(Action::ToggleStop(id, si));
                                                ui.close();
                                            }
                                        });
                                    }
                                }
                            }
                            if let Some(gc) = col.group_color {
                                let strip = Rect::from_min_size(cell.min, vec2(2.0, cell.height()));
                                painter.rect_filled(strip, 0.0, gc);
                            }
                            if col.frozen {
                                painter.rect_filled(cell, 2.0, FROST);
                            }
                            if selected {
                                painter.rect_stroke(cell, 2.0, Stroke::new(1.5, theme::select()), StrokeKind::Inside);
                            }
                        }
                    }

                    // Return tracks have no clip slots.
                    for ri in 0..n_ret {
                        let col = Rect::from_min_size(
                            pos2(grid.left() + ret_x + ri as f32 * COL_W, grid.top()),
                            vec2(COL_W, n_scenes as f32 * ROW_H),
                        )
                        .shrink(1.0);
                        ui.painter().rect_filled(col, 2.0, theme::gray(0x2c));
                    }

                    // Scene column on the master track.
                    let mx = grid.left() + master_x;
                    for si in 0..n_scenes {
                        let cell = Rect::from_min_size(pos2(mx, grid.top() + si as f32 * ROW_H), vec2(MASTER_W, ROW_H))
                            .shrink(1.0);
                        let painter = ui.painter_at(cell.expand(1.0));
                        let body = ui
                            .interact(cell, ui.id().with(("scene", si)), Sense::click())
                            .info("Scene", info_view::SCENE);
                        let scene = &mut app.project.scenes[si];
                        let is_selected = selected_scene == si;
                        let base = scene.color.map(theme::rgb);
                        let fill = match (base, is_selected) {
                            (Some(c), true) => c,
                            (Some(c), false) => theme::dim(c, 0.7),
                            (None, true) => theme::gray(0x66),
                            (None, false) => theme::gray(0x48),
                        };
                        painter.rect_filled(cell, 2.0, fill);
                        if is_selected {
                            painter.rect_stroke(cell, 2.0, Stroke::new(1.0, theme::select()), StrokeKind::Inside);
                        }
                        let fg = if base.is_some() { Color32::BLACK } else { theme::text() };
                        let tri_rect = Rect::from_min_size(cell.min, vec2(TRI_W, cell.height()));
                        let tri_resp = ui
                            .interact(tri_rect, ui.id().with(("scene_tri", si)), Sense::click())
                            .info("Scene Launch", info_view::SCENE_LAUNCH);
                        widgets::triangle(
                            &painter,
                            tri_rect.center(),
                            10.0,
                            if tri_resp.hovered() {
                                theme::PLAY
                            } else if base.is_some() {
                                theme::gray(0x20)
                            } else {
                                theme::text_dim()
                            },
                        );
                        let follow_w = if scene.follow.enabled { 14.0 } else { 0.0 };
                        let text_painter = ui.painter_at(Rect::from_x_y_ranges(
                            cell.left()..=cell.right() - 2.0 - follow_w,
                            cell.y_range(),
                        ));
                        text_painter.text(
                            pos2(cell.left() + TRI_W + 2.0, cell.center().y),
                            egui::Align2::LEFT_CENTER,
                            arrange::scene_label(scene),
                            egui::FontId::proportional(11.0),
                            fg,
                        );
                        if scene.follow.enabled {
                            // A small arrow: this scene has a follow action.
                            let c = pos2(cell.right() - 8.0, cell.center().y);
                            painter.arrow(c - vec2(4.0, 0.0), vec2(7.0, 0.0), Stroke::new(1.2, fg));
                        }
                        if tri_resp.clicked() || body.double_clicked() {
                            actions.push(Action::LaunchScene(si));
                            actions.push(Action::SelectScene(si));
                        } else if body.clicked() || body.secondary_clicked() {
                            actions.push(Action::SelectScene(si));
                        }
                        sticky_menu(&body, |ui| {
                            ui.label("Scene name");
                            if ui.text_edit_singleline(&mut scene.name).changed() {
                                actions.push(Action::Touched("Rename Scene"));
                            }
                            let mut has_tempo = scene.tempo.is_some();
                            if ui.checkbox(&mut has_tempo, "Scene tempo").changed() {
                                scene.tempo = if has_tempo { Some(120.0) } else { None };
                                actions.push(Action::Changed("Scene Tempo"));
                            }
                            if let Some(t) = scene.tempo.as_mut() {
                                if ui
                                    .add(egui::DragValue::new(t).range(20.0..=999.0).speed(0.2).suffix(" BPM"))
                                    .changed()
                                {
                                    actions.push(Action::Changed("Scene Tempo"));
                                }
                            }
                            let mut has_sig = scene.signature.is_some();
                            if ui.checkbox(&mut has_sig, "Time signature").changed() {
                                scene.signature = has_sig.then_some((4, 4));
                                actions.push(Action::Changed("Scene Time Signature"));
                            }
                            if let Some((n, d)) = scene.signature.as_mut() {
                                ui.horizontal(|ui| {
                                    if ui.add(egui::DragValue::new(n).range(1..=32).speed(0.1)).changed() {
                                        actions.push(Action::Changed("Scene Time Signature"));
                                    }
                                    ui.label("/");
                                    ui.menu_button(d.to_string(), |ui| {
                                        for v in [1u32, 2, 4, 8, 16] {
                                            if ui.selectable_label(*d == v, v.to_string()).clicked() {
                                                *d = v;
                                                actions.push(Action::Changed("Scene Time Signature"));
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                            }
                            ui.separator();
                            let follow = &mut scene.follow;
                            if ui.checkbox(&mut follow.enabled, "Follow action").changed() {
                                actions.push(Action::Changed("Scene Follow Action"));
                            }
                            ui.add_enabled_ui(follow.enabled, |ui| {
                                ui.horizontal(|ui| {
                                    ui.menu_button(follow.action.label(), |ui| {
                                        for a in SceneFollowAction::ALL {
                                            if ui.selectable_label(follow.action == a, a.label()).clicked() {
                                                follow.action = a;
                                                actions.push(Action::Changed("Scene Follow Action"));
                                                ui.close();
                                            }
                                        }
                                    });
                                    ui.label("after");
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut follow.beats)
                                                .range(1.0..=1024.0)
                                                .speed(0.25)
                                                .max_decimals(2)
                                                .suffix(" beats"),
                                        )
                                        .changed()
                                    {
                                        actions.push(Action::Changed("Scene Follow Action"));
                                    }
                                });
                            });
                            ui.separator();
                            if let Some(c) = color_palette(ui, scene.color) {
                                scene.color = Some(c);
                                actions.push(Action::Touched("Scene Color"));
                                ui.close();
                            }
                            if scene.color.is_some() && ui.button("No Colour").clicked() {
                                scene.color = None;
                                actions.push(Action::Touched("Scene Color"));
                                ui.close();
                            }
                            ui.separator();
                            if ui.button("Capture and Insert Scene").clicked() {
                                actions.push(Action::CaptureScene);
                                ui.close();
                            }
                            if ui.button("Insert Scene").clicked() {
                                actions.push(Action::InsertScene(si));
                                ui.close();
                            }
                            if ui.button("Delete Scene").clicked() {
                                actions.push(Action::DeleteScene(si));
                                ui.close();
                            }
                        });
                    }
                    let add =
                        Rect::from_min_size(pos2(mx, grid.top() + n_scenes as f32 * ROW_H), vec2(MASTER_W, ROW_H))
                            .shrink(1.0);
                    let add_resp = ui
                        .interact(add, ui.id().with("add_scene"), Sense::click())
                        .info("Add Scene", "Adds an empty scene (row of clip slots) at the bottom.");
                    ui.painter().text(
                        add.center(),
                        egui::Align2::CENTER_CENTER,
                        "+ Scene",
                        egui::FontId::proportional(11.0),
                        if add_resp.hovered() {
                            theme::text()
                        } else {
                            theme::text_dim()
                        },
                    );
                    if add_resp.clicked() {
                        actions.push(Action::AddScene);
                    }
                });

            // ----- clip stop buttons -----
            let (stops, _) = ui.allocate_exact_size(vec2(total_w, STOP_H), Sense::hover());
            for (ci, col) in cols.iter().enumerate() {
                let id = col.id;
                let busy = if col.is_group {
                    col.members.iter().any(|&m| states[m].0 >= 0)
                } else {
                    states[col.ti].0 >= 0
                };
                let r = Rect::from_min_size(pos2(stops.left() + ci as f32 * COL_W, stops.top()), vec2(COL_W, STOP_H))
                    .shrink(2.0);
                let resp = ui.interact(r, ui.id().with(("stopbtn", id)), Sense::click());
                ui.painter().rect_filled(r, 2.0, theme::gray(0x40));
                widgets::stop_square(
                    ui.painter(),
                    r.center(),
                    8.0,
                    if busy {
                        theme::PLAY
                    } else if resp.hovered() {
                        theme::text()
                    } else {
                        theme::text_dim()
                    },
                );
                let tip = if col.is_group {
                    "Stops every clip playing in the group at the next launch quantization boundary."
                } else {
                    info_view::CLIP_STOP
                };
                if resp.info("Clip Stop", tip).clicked() {
                    actions.push(if col.is_group {
                        Action::StopGroup(id)
                    } else {
                        Action::StopTrack(id)
                    });
                }
            }
            let r = Rect::from_min_size(pos2(stops.left() + master_x, stops.top()), vec2(MASTER_W, STOP_H)).shrink(2.0);
            let resp = ui
                .interact(r, ui.id().with("stop_all"), Sense::click())
                .info("Stop All Clips", info_view::STOP_ALL);
            ui.painter().rect_filled(r, 2.0, theme::gray(0x40));
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "■ Stop All Clips",
                egui::FontId::proportional(11.0),
                if resp.hovered() {
                    theme::text()
                } else {
                    theme::text_dim()
                },
            );
            if resp.clicked() {
                actions.push(Action::StopAll);
            }

            // ----- mixer -----
            let (mixer, _) = ui.allocate_exact_size(vec2(total_w, mixer_h), Sense::hover());
            ui.painter().rect_filled(mixer, 0.0, theme::bg_panel());
            // Splits a mixer column into sends, strip and the rows below it.
            let split = |x: f32, w: f32| {
                let col = Rect::from_min_size(pos2(x, mixer.top()), vec2(w, mixer_h)).shrink2(vec2(3.0, 4.0));
                let (sends, rest) = col.split_top_bottom_at_y(col.top() + send_rows as f32 * SEND_ROW_H);
                let (strip, extras) = rest.split_top_bottom_at_y(rest.bottom() - EXTRA_H);
                (sends, strip, extras)
            };
            let line = Stroke::new(1.0, theme::gray(0x28));
            for (ci, col) in cols.iter().enumerate() {
                let id = col.id;
                let meter = track_state(app, col.ti, id)
                    .map(|s| (s.meter_l.load(), s.meter_r.load()))
                    .unwrap_or((0.0, 0.0));
                let (sends_rect, strip, extras) = split(mixer.left() + ci as f32 * COL_W, COL_W);
                ui.painter().vline(strip.right() + 3.0, mixer.y_range(), line);
                let number = col.ti + 1;
                let t = &mut app.project.tracks[col.ti];
                if n_ret > 0 {
                    ui.scope_builder(UiBuilder::new().max_rect(sends_rect).id_salt(("sends", id)), |ui| {
                        ui.spacing_mut().item_spacing = vec2(1.0, 0.0);
                        for (row, chunk) in (0..n_ret).collect::<Vec<_>>().chunks(SENDS_PER_ROW).enumerate() {
                            let row_rect = Rect::from_min_size(
                                pos2(sends_rect.left(), sends_rect.top() + row as f32 * SEND_ROW_H),
                                vec2(sends_rect.width(), SEND_ROW_H),
                            );
                            ui.scope_builder(UiBuilder::new().max_rect(row_rect), |ui| {
                                ui.horizontal(|ui| {
                                    for &k in chunk {
                                        let letter = (b'A' + k as u8) as char;
                                        let mut db = t.sends.get(k).copied().unwrap_or(-70.0);
                                        if widgets::send_knob(ui, &mut db, letter) {
                                            actions.push(Action::Send(id, k, db));
                                        }
                                    }
                                });
                            });
                        }
                    });
                }
                let can_arm = !col.is_group && !col.frozen;
                ui.scope_builder(UiBuilder::new().max_rect(strip).id_salt(("strip", id)), |ui| {
                    if mixer_strip_ex(ui, &mut t.mixer, meter, StripKind::Track(number), can_arm, cue_mode) {
                        actions.push(Action::Mixer(id));
                    }
                });
                ui.scope_builder(UiBuilder::new().max_rect(extras).id_salt(("extras", id)), |ui| {
                    track_extras(ui, id, t.crossfade, t.delay_ms, col.group_name.as_deref(), &mut actions);
                });
            }
            for ri in 0..n_ret {
                let ret = &mut app.project.returns[ri];
                let id = ret.id;
                let meter = app
                    .engine
                    .shared
                    .returns
                    .get(ri)
                    .filter(|s| s.id.load(Ordering::Relaxed) == id)
                    .map(|s| (s.meter_l.load(), s.meter_r.load()))
                    .unwrap_or((0.0, 0.0));
                let (_, strip, _) = split(mixer.left() + ret_x + ri as f32 * COL_W, COL_W);
                ui.painter().vline(strip.right() + 3.0, mixer.y_range(), line);
                let letter = (b'A' + ri as u8) as char;
                ui.scope_builder(UiBuilder::new().max_rect(strip).id_salt(("rstrip", id)), |ui| {
                    if mixer_strip(ui, &mut ret.mixer, meter, StripKind::Return(letter)) {
                        actions.push(Action::BusMixer(id));
                    }
                });
            }
            let (_, strip, extras) = split(mixer.left() + master_x, MASTER_W);
            let meter = (
                app.engine.shared.master_meter_l.load(),
                app.engine.shared.master_meter_r.load(),
            );
            ui.scope_builder(UiBuilder::new().max_rect(strip).id_salt("master_strip"), |ui| {
                if mixer_strip(ui, &mut app.project.master, meter, StripKind::Master) {
                    actions.push(Action::Master);
                }
            });
            ui.scope_builder(UiBuilder::new().max_rect(extras).id_salt("master_extras"), |ui| {
                master_extras(ui, app, &mut actions);
            });
        });

    clip_drag_preview(app, ui.ctx());

    for a in actions {
        apply(app, a);
    }
}

fn apply(app: &mut TesseraApp, a: Action) {
    let frozen = |app: &TesseraApp, t: Id| app.project.track(t).is_some_and(|t| t.frozen);
    match a {
        Action::Launch(t, s) => app.launch_clip(t, s),
        Action::StopTrack(t) => app.send(Command::StopTrack(t)),
        Action::LaunchScene(s) => app.launch_scene(s),
        Action::StopAll => app.send(Command::StopAllClips),
        Action::Select(r) => {
            app.select_clip(r);
            // Frozen clips can't be edited.
            if app.clip(r).is_some() && !frozen(app, r.track()) {
                app.detail = DetailMode::Clip;
            }
        }
        Action::SelectTrack(t) => app.select_track(t),
        Action::ToggleTrackSelection(t) => app.toggle_track_selection(t),
        Action::ContextSelect(t) => {
            if !app.target_tracks().contains(&t) {
                app.select_track(t);
            }
        }
        Action::Create(t, s) => {
            app.label_edit("Insert MIDI Clip");
            app.create_clip_in_slot(t, s);
        }
        Action::Delete(r) => {
            app.label_edit("Delete Clip");
            app.delete_clip(r);
        }
        Action::Duplicate(t, s) => {
            app.label_edit("Duplicate Clip");
            app.duplicate_slot(t, s);
        }
        Action::ToArrangement(t, s) => {
            app.label_edit("Add Clip to Arrangement");
            app.slot_to_arrangement(t, s);
        }
        Action::SaveToLibrary(r) => {
            app.save_clip_to_library(r);
        }
        Action::ExportMidi(r) => {
            app.select_clip(r);
            app.export_midi_clip_dialog();
        }
        Action::ImportDialog(t, s) => {
            app.label_edit("Import Audio");
            app.import_audio_dialog(t, s);
        }
        // Files from the desktop behave like samples from the browser.
        Action::Import(p, t, s) => {
            app.label_edit("Import Audio");
            app.drop_item(&BrowserItem::Sample(p), DropTarget::Slot(t, s));
        }
        Action::Drop(item, target) => {
            app.label_edit("Drop from Browser");
            app.drop_item(&item, target);
        }
        Action::DuplicateTracks => app.duplicate_tracks(),
        Action::DeleteTracks => {
            let ids = app.target_tracks();
            let single = match ids.as_slice() {
                [t] => app.project.track(*t).is_some_and(|t| t.kind != TrackKind::Group),
                _ => false,
            };
            if single {
                // One plain track: also drops its MIDI mappings.
                app.label_edit("Delete Track");
                app.delete_track(ids[0]);
                app.work.selected_tracks.clear();
            } else {
                app.delete_tracks();
            }
        }
        Action::GroupSelected => app.group_selected(),
        Action::Ungroup(g) => app.ungroup(g),
        Action::ToggleFold(g) => app.toggle_fold(g),
        Action::LaunchGroup(g, s) => app.launch_group_slot(g, s),
        Action::StopGroup(g) => app.stop_group(g),
        Action::Freeze(t) => app.freeze_track(t),
        Action::Unfreeze(t) => app.unfreeze_track(t),
        Action::Flatten(t) => app.flatten_track(t),
        Action::Nudge(t, d) => app.nudge_track(t, d),
        Action::TrackColor(t, c) => app.set_track_color(t, c),
        Action::SelectScene(s) => app.work.selected_scene = s,
        Action::InsertScene(s) => {
            app.label_edit("Insert Scene");
            app.project.insert_scene(s);
            app.work.selected_scene = s;
            app.changed();
        }
        Action::DeleteScene(s) => {
            app.label_edit("Delete Scene");
            app.project.remove_scene(s);
            app.selected_clip = None;
            let n = app.project.scenes.len();
            app.work.selected_scene = app.work.selected_scene.min(n.saturating_sub(1));
            app.changed();
        }
        Action::CaptureScene => app.capture_scene(),
        Action::AddScene => {
            app.label_edit("Insert Scene");
            app.project.add_scene();
            app.changed();
        }
        Action::ToggleStop(t, s) => app.toggle_stop_button(t, s),
        Action::Mixer(t) => {
            app.label_edit("Mixer");
            app.send_mixer(t);
        }
        Action::Master => {
            app.label_edit("Master Mixer");
            let m = app.project.master;
            app.send(Command::SetMaster(m));
            app.touch();
        }
        Action::Changed(label) => {
            app.label_edit(label);
            app.changed();
        }
        Action::Touched(label) => {
            app.label_edit(label);
            app.touch();
        }
        Action::RecordSlot(t, s) => app.record_slot(t, s),
        Action::Send(t, k, db) => {
            app.label_edit("Send Level");
            app.set_send(t, k, db);
        }
        Action::BusMixer(b) => {
            app.label_edit("Mixer");
            app.send_bus_mixer(b);
        }
        Action::DeleteReturn(r) => {
            app.label_edit("Delete Return Track");
            app.delete_return(r);
        }
        Action::AddReturn => {
            app.label_edit("Insert Return Track");
            app.add_return_track(None);
        }
        Action::Crossfade(t, x) => app.set_crossfade_assign(t, x),
        Action::Delay(t, ms) => app.set_track_delay(t, ms),
        Action::Crossfader(x) => {
            app.label_edit("Crossfader");
            app.set_crossfader(x);
        }
        Action::Cue(label) => {
            app.label_edit(label);
            app.send_cue();
        }
        Action::MoveSlot { from, to, copy } => move_slot(app, from, to, copy),
        Action::ArrangementToSlot {
            track,
            id,
            target,
            scene,
        } => app.arrangement_to_slot(track, id, target, scene),
    }
}

/// Moves (or with `copy`, copies) a Session clip onto another slot of a track
/// of the same kind, replacing what was there.
fn move_slot(app: &mut TesseraApp, from: (Id, usize), to: (Id, usize), copy: bool) {
    if from == to {
        return;
    }
    let kind = |app: &TesseraApp, t: Id| app.project.track(t).map(|t| t.kind);
    if kind(app, from.0) != kind(app, to.0) {
        app.status = "Clips go onto a track of the same kind".into();
        return;
    }
    let Some(mut clip) = app
        .clip(ClipRef::Slot {
            track: from.0,
            scene: from.1,
        })
        .cloned()
    else {
        return;
    };
    app.label_edit(if copy { "Copy Clip" } else { "Move Clip" });
    if copy {
        clip.id = app.project.alloc_id();
    } else if let Some(s) = app.project.track_mut(from.0).and_then(|t| t.slots.get_mut(from.1)) {
        *s = None;
    }
    if let Some(s) = app.project.track_mut(to.0).and_then(|t| t.slots.get_mut(to.1)) {
        *s = Some(clip);
    }
    app.select_clip(ClipRef::Slot {
        track: to.0,
        scene: to.1,
    });
    app.changed();
}

/// Routing, crossfade assign and track delay under a track's mixer strip.
fn track_extras(
    ui: &mut egui::Ui,
    id: Id,
    crossfade: CrossfadeAssign,
    delay_ms: f32,
    group: Option<&str>,
    actions: &mut Vec<Action>,
) {
    ui.spacing_mut().item_spacing = vec2(2.0, 4.0);
    let (text, color) = match group {
        Some(g) => (format!("→ {g}"), theme::text()),
        None => ("→ Master".to_string(), theme::text_dim()),
    };
    ui.add(egui::Label::new(egui::RichText::new(text).small().color(color)).truncate())
        .on_hover_text("Audio output");
    ui.horizontal(|ui| {
        for side in [CrossfadeAssign::A, CrossfadeAssign::B] {
            let on = crossfade == side;
            if widgets::toggle(ui, on, side.label(), theme::ACCENT, 18.0)
                .on_hover_text("Crossfader assign")
                .clicked()
            {
                actions.push(Action::Crossfade(id, if on { CrossfadeAssign::None } else { side }));
            }
        }
        ui.add_space(2.0);
        let mut ms = delay_ms;
        let resp = ui
            .add_sized(
                vec2(ui.available_width(), 18.0),
                egui::DragValue::new(&mut ms)
                    .range(-100.0..=100.0)
                    .speed(0.2)
                    .max_decimals(1)
                    .suffix(" ms"),
            )
            .on_hover_text("Track delay");
        if resp.changed() {
            actions.push(Action::Delay(id, ms));
        }
    });
}

/// Crossfader and cue output controls under the master strip.
fn master_extras(ui: &mut egui::Ui, app: &mut TesseraApp, actions: &mut Vec<Action>) {
    ui.spacing_mut().item_spacing = vec2(3.0, 4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("A").small().color(theme::text_dim()));
        ui.spacing_mut().slider_width = ui.available_width() - 12.0;
        let mut x = app.project.crossfader;
        let resp = ui
            .add(egui::Slider::new(&mut x, -1.0..=1.0).show_value(false))
            .on_hover_text("Crossfader (double-click to centre)");
        if resp.double_clicked() {
            actions.push(Action::Crossfader(0.0));
        } else if resp.changed() {
            actions.push(Action::Crossfader(x));
        }
        ui.label(egui::RichText::new("B").small().color(theme::text_dim()));
    });
    ui.horizontal(|ui| {
        let cue = &mut app.project.cue;
        if widgets::toggle(ui, cue.cue_mode, "Cue", theme::SOLO, 34.0)
            .on_hover_text("Solo/Cue: solo buttons pre-listen on the cue output")
            .clicked()
        {
            cue.cue_mode = !cue.cue_mode;
            actions.push(Action::Cue("Solo/Cue Mode"));
        }
        let resp = ui
            .add_sized(
                vec2(ui.available_width(), 18.0),
                egui::DragValue::new(&mut cue.volume_db)
                    .range(-70.0..=6.0)
                    .speed(0.2)
                    .max_decimals(1)
                    .suffix(" dB"),
            )
            .on_hover_text("Cue volume");
        if resp.changed() {
            actions.push(Action::Cue("Cue Volume"));
        }
    });
}

/// One mixer channel: activator, solo, arm, pan, fader and meter. Return
/// and master strips have no solo or arm buttons.
pub fn mixer_strip(
    ui: &mut egui::Ui,
    m: &mut tessera_engine::model::MixerParams,
    meter: (f32, f32),
    kind: StripKind,
) -> bool {
    mixer_strip_ex(ui, m, meter, kind, true, false)
}

/// [`mixer_strip`] with a choice of arm button (`can_arm`, track strips
/// only) and the solo button shown as a cue button (`cue_mode`).
pub fn mixer_strip_ex(
    ui: &mut egui::Ui,
    m: &mut tessera_engine::model::MixerParams,
    meter: (f32, f32),
    kind: StripKind,
    can_arm: bool,
    cue_mode: bool,
) -> bool {
    let (number, can_arm) = match kind {
        StripKind::Track(n) => (Some(n), can_arm),
        _ => (None, false),
    };
    let mut changed = false;
    ui.vertical_centered(|ui| {
        ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
        if widgets::pan_knob(ui, &mut m.pan) {
            changed = true;
        }
        ui.add_space(2.0);
        let fader_h = ui.available_height() - 52.0;
        if widgets::fader_with_meter(ui, &mut m.volume_db, meter, fader_h.max(40.0)) {
            changed = true;
        }
        ui.label(
            egui::RichText::new(widgets::format_db(m.volume_db))
                .small()
                .color(theme::text_dim()),
        );
        ui.horizontal(|ui| {
            let total = if number.is_some() {
                26.0 + 22.0 + if can_arm { 22.0 } else { 0.0 } + 6.0
            } else {
                26.0
            };
            ui.add_space(((ui.available_width() - total) / 2.0).max(0.0));
            let label = match kind {
                StripKind::Track(n) => n.to_string(),
                StripKind::Return(c) => c.to_string(),
                StripKind::Master => "M".into(),
            };
            if widgets::toggle(ui, !m.mute, &label, theme::ACTIVE, 26.0)
                .info("Track Activator", info_view::ACTIVATOR)
                .clicked()
            {
                m.mute = !m.mute;
                changed = true;
            }
            if number.is_some() {
                let (solo_label, solo_title, solo_tip) = if cue_mode {
                    (
                        "C",
                        "Cue",
                        "Cue: pre-listen to this track on the cue output (outputs 3/4).",
                    )
                } else {
                    ("S", "Solo", info_view::SOLO)
                };
                if widgets::toggle(ui, m.solo, solo_label, theme::SOLO, 22.0)
                    .info(solo_title, solo_tip)
                    .clicked()
                {
                    m.solo = !m.solo;
                    changed = true;
                }
                if can_arm
                    && widgets::toggle(ui, m.arm, "●", theme::ARM, 22.0)
                        .info("Arm Recording", info_view::ARM)
                        .clicked()
                {
                    m.arm = !m.arm;
                    changed = true;
                }
            }
        });
    });
    changed
}
