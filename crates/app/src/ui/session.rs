//! Session view: the grid of clip slots and scenes, plus the mixer.

use std::path::PathBuf;
use std::sync::atomic::Ordering;

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind, UiBuilder};
use tessera_engine::engine::TrackShared;
use tessera_engine::model::{Id, TrackKind, MASTER_BUS};
use tessera_engine::Command;

use super::{theme, widgets};
use crate::app::{ClipRef, DetailMode, TesseraApp};
use crate::browser::{BrowserItem, DropTarget};

const COL_W: f32 = 112.0;
const MASTER_W: f32 = 130.0;
const HEADER_H: f32 = 22.0;
const ROW_H: f32 = 24.0;
const STOP_H: f32 = 22.0;
const MIXER_H: f32 = 200.0;
/// Height of one row of send knobs above the mixer.
const SEND_ROW_H: f32 = 34.0;
const SENDS_PER_ROW: usize = 4;
const TRI_W: f32 = 18.0;

enum Action {
    Launch(Id, usize),
    StopTrack(Id),
    LaunchScene(usize),
    StopAll,
    Select(ClipRef),
    SelectTrack(Id),
    Create(Id, usize),
    Delete(ClipRef),
    Duplicate(Id, usize),
    ToArrangement(Id, usize),
    ImportDialog(Id, usize),
    Import(PathBuf, Id, usize),
    DeleteTrack(Id),
    InsertScene(usize),
    DeleteScene(usize),
    AddScene,
    Mixer(Id),
    Master,
    Changed,
    RecordSlot(Id, usize),
    Send(Id, usize, f32),
    BusMixer(Id),
    DeleteReturn(Id),
    AddReturn,
    Drop(BrowserItem, DropTarget),
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

/// Published engine state for track `index`, if it belongs to track `id`.
pub fn track_state(app: &TesseraApp, index: usize, id: Id) -> Option<&TrackShared> {
    let ts = app.engine.shared.tracks.get(index)?;
    (ts.id.load(Ordering::Relaxed) == id).then_some(ts)
}

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let mut actions: Vec<Action> = Vec::new();
    let n_tracks = app.project.tracks.len();
    let n_scenes = app.project.scenes.len();
    let n_ret = app.project.returns.len();
    let ret_x = n_tracks as f32 * COL_W;
    let master_x = (n_tracks + n_ret) as f32 * COL_W;
    let total_w = master_x + MASTER_W + 8.0;
    let send_rows = n_ret.div_ceil(SENDS_PER_ROW);
    let mixer_h = MIXER_H + send_rows as f32 * SEND_ROW_H;
    let time = ui.input(|i| i.time);
    let blink = (time * 4.0).fract() < 0.5;

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
            for ti in 0..n_tracks {
                let r = Rect::from_min_size(pos2(hdr.left() + ti as f32 * COL_W, hdr.top()), vec2(COL_W, HEADER_H))
                    .shrink(1.0);
                let id = app.project.tracks[ti].id;
                let selected = app.selected_track == Some(id);
                let resp = ui.interact(r, ui.id().with(("hdr", id)), Sense::click());
                let t = &mut app.project.tracks[ti];
                let color = theme::rgb(t.color);
                ui.painter()
                    .rect_filled(r, 2.0, if selected { color } else { theme::dim(color, 0.75) });
                ui.painter().text(
                    pos2(r.left() + 5.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    &t.name,
                    egui::FontId::proportional(12.0),
                    Color32::BLACK,
                );
                if resp.clicked() {
                    actions.push(Action::SelectTrack(id));
                }
                if let Some(item) = super::browser::drop_on(ui, r) {
                    actions.push(Action::Drop(item, DropTarget::Track(id)));
                }
                resp.context_menu(|ui| {
                    ui.label("Name");
                    if ui.text_edit_singleline(&mut t.name).changed() {
                        actions.push(Action::Changed);
                    }
                    if ui.button("Delete Track").clicked() {
                        actions.push(Action::DeleteTrack(id));
                        ui.close();
                    }
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
                let resp = ui.interact(r, ui.id().with(("rhdr", id)), Sense::click());
                let color = theme::rgb(ret.color);
                ui.painter()
                    .rect_filled(r, 2.0, if selected { color } else { theme::dim(color, 0.7) });
                ui.painter().text(
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
                resp.context_menu(|ui| {
                    ui.label("Name");
                    if ui.text_edit_singleline(&mut ret.name).changed() {
                        actions.push(Action::Changed);
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
            let master_resp = ui.interact(mr, ui.id().with("master_hdr"), Sense::click());
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
                    Color32::from_rgb(0x8a, 0x8a, 0x8a)
                } else {
                    Color32::from_rgb(0x5a, 0x5a, 0x5a)
                },
            );
            ui.painter().text(
                pos2(mr.left() + 5.0, mr.center().y),
                egui::Align2::LEFT_CENTER,
                "Master",
                egui::FontId::proportional(12.0),
                theme::TEXT,
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
                    for ti in 0..n_tracks {
                        let t = &app.project.tracks[ti];
                        let id = t.id;
                        let state = track_state(app, ti, id);
                        let playing = state.map(|s| s.playing_slot.load(Ordering::Relaxed)).unwrap_or(-1);
                        let queued = state.map(|s| s.queued_slot.load(Ordering::Relaxed)).unwrap_or(-1);
                        let clip_pos = state.map(|s| s.clip_position.load()).unwrap_or(0.0);
                        let track_busy = playing >= 0 || queued >= 0;
                        let can_record = t.kind == TrackKind::Audio && t.mixer.arm;
                        // Session take on this track: (slot, recording?)
                        let take = app.engine.shared.record.take_for(id).and_then(|tk| {
                            let slot = tk.slot.load(Ordering::Relaxed);
                            (slot >= 0).then(|| (slot as usize, tk.recording.load(Ordering::Relaxed)))
                        });
                        for si in 0..n_scenes {
                            let cell = Rect::from_min_size(
                                pos2(grid.left() + ti as f32 * COL_W, grid.top() + si as f32 * ROW_H),
                                vec2(COL_W, ROW_H),
                            )
                            .shrink(1.0);
                            let r = ClipRef::Slot { track: id, scene: si };
                            let selected = app.selected_clip == Some(r);
                            let painter = ui.painter_at(cell.expand(1.0));
                            let body = ui.interact(cell, ui.id().with(("slot", id, si)), Sense::click());
                            let tri_rect = Rect::from_min_size(cell.min, vec2(TRI_W, cell.height()));

                            if hovering_files && pointer.is_some_and(|p| cell.contains(p)) {
                                painter.rect_stroke(cell, 2.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside);
                            }
                            if !dropped.is_empty() && pointer.is_some_and(|p| cell.contains(p)) {
                                actions.push(Action::Import(dropped[0].clone(), id, si));
                            }
                            if let Some(item) = super::browser::drop_on(ui, cell) {
                                actions.push(Action::Drop(item, DropTarget::Slot(id, si)));
                            }

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
                                    let tri_resp = ui.interact(tri_rect, ui.id().with(("tri", id, si)), Sense::click());
                                    let tri_color = if is_playing || (is_queued && blink) {
                                        theme::PLAY
                                    } else if tri_resp.hovered() {
                                        Color32::WHITE
                                    } else {
                                        Color32::from_rgb(0x20, 0x20, 0x20)
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
                                        painter.rect_filled(bar, 0.0, Color32::from_rgba_unmultiplied(0, 0, 0, 140));
                                    }
                                    if tri_resp.clicked() || body.double_clicked() {
                                        actions.push(Action::Launch(id, si));
                                    } else if body.clicked() {
                                        actions.push(Action::Select(r));
                                    }
                                    body.context_menu(|ui| {
                                        if ui.button("Launch").clicked() {
                                            actions.push(Action::Launch(id, si));
                                            ui.close();
                                        }
                                        if ui.button("Duplicate").clicked() {
                                            actions.push(Action::Duplicate(id, si));
                                            ui.close();
                                        }
                                        if ui.button("Add to Arrangement").clicked() {
                                            actions.push(Action::ToArrangement(id, si));
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
                                            theme::BG_SLOT
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
                                        if recording { Color32::BLACK } else { theme::TEXT_DIM },
                                    );
                                    if body.clicked() {
                                        // Stop recording and play the new clip.
                                        actions.push(Action::Launch(id, si));
                                    }
                                }
                                None => {
                                    painter.rect_filled(cell, 2.0, theme::BG_SLOT);
                                    if can_record {
                                        let rec_resp =
                                            ui.interact(tri_rect, ui.id().with(("rec", id, si)), Sense::click());
                                        let c = if rec_resp.hovered() {
                                            theme::ARM
                                        } else {
                                            theme::dim(theme::ARM, 0.6)
                                        };
                                        painter.circle_filled(tri_rect.center(), 5.0, c);
                                        if rec_resp.on_hover_text("Record into this slot").clicked() {
                                            actions.push(Action::RecordSlot(id, si));
                                        }
                                    } else if track_busy {
                                        let stop_resp =
                                            ui.interact(tri_rect, ui.id().with(("stop", id, si)), Sense::click());
                                        let c = if stop_resp.hovered() {
                                            theme::TEXT
                                        } else {
                                            theme::TEXT_DIM
                                        };
                                        widgets::stop_square(&painter, tri_rect.center(), 7.0, c);
                                        if stop_resp.clicked() {
                                            actions.push(Action::StopTrack(id));
                                        }
                                    }
                                    if body.double_clicked() {
                                        match t.kind {
                                            TrackKind::Midi => actions.push(Action::Create(id, si)),
                                            TrackKind::Audio => actions.push(Action::ImportDialog(id, si)),
                                        }
                                    } else if body.clicked() {
                                        actions.push(Action::Select(r));
                                    }
                                    let kind = t.kind;
                                    body.context_menu(|ui| match kind {
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
                                    });
                                }
                            }
                            if selected {
                                painter.rect_stroke(cell, 2.0, Stroke::new(1.5, theme::SELECT), StrokeKind::Inside);
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
                        ui.painter().rect_filled(col, 2.0, Color32::from_rgb(0x2c, 0x2c, 0x2c));
                    }

                    // Scene column on the master track.
                    let mx = grid.left() + master_x;
                    for si in 0..n_scenes {
                        let cell = Rect::from_min_size(pos2(mx, grid.top() + si as f32 * ROW_H), vec2(MASTER_W, ROW_H))
                            .shrink(1.0);
                        let painter = ui.painter_at(cell.expand(1.0));
                        let body = ui.interact(cell, ui.id().with(("scene", si)), Sense::click());
                        painter.rect_filled(cell, 2.0, Color32::from_rgb(0x48, 0x48, 0x48));
                        let tri_rect = Rect::from_min_size(cell.min, vec2(TRI_W, cell.height()));
                        let tri_resp = ui.interact(tri_rect, ui.id().with(("scene_tri", si)), Sense::click());
                        widgets::triangle(
                            &painter,
                            tri_rect.center(),
                            10.0,
                            if tri_resp.hovered() {
                                theme::PLAY
                            } else {
                                theme::TEXT_DIM
                            },
                        );
                        let scene = &mut app.project.scenes[si];
                        let label = match scene.tempo {
                            Some(t) => format!("{}  ({t:.0} BPM)", scene.name),
                            None => scene.name.clone(),
                        };
                        painter.text(
                            pos2(cell.left() + TRI_W + 2.0, cell.center().y),
                            egui::Align2::LEFT_CENTER,
                            label,
                            egui::FontId::proportional(11.0),
                            theme::TEXT,
                        );
                        if tri_resp.clicked() || body.double_clicked() {
                            actions.push(Action::LaunchScene(si));
                        }
                        body.context_menu(|ui| {
                            ui.label("Scene name");
                            if ui.text_edit_singleline(&mut scene.name).changed() {
                                actions.push(Action::Changed);
                            }
                            let mut has_tempo = scene.tempo.is_some();
                            if ui.checkbox(&mut has_tempo, "Scene tempo").changed() {
                                scene.tempo = if has_tempo { Some(120.0) } else { None };
                                actions.push(Action::Changed);
                            }
                            if let Some(t) = scene.tempo.as_mut() {
                                if ui
                                    .add(egui::DragValue::new(t).range(20.0..=999.0).speed(0.2).suffix(" BPM"))
                                    .changed()
                                {
                                    actions.push(Action::Changed);
                                }
                            }
                            ui.separator();
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
                    let add_resp = ui.interact(add, ui.id().with("add_scene"), Sense::click());
                    ui.painter().text(
                        add.center(),
                        egui::Align2::CENTER_CENTER,
                        "+ Scene",
                        egui::FontId::proportional(11.0),
                        if add_resp.hovered() {
                            theme::TEXT
                        } else {
                            theme::TEXT_DIM
                        },
                    );
                    if add_resp.clicked() {
                        actions.push(Action::AddScene);
                    }
                });

            // ----- clip stop buttons -----
            let (stops, _) = ui.allocate_exact_size(vec2(total_w, STOP_H), Sense::hover());
            for ti in 0..n_tracks {
                let id = app.project.tracks[ti].id;
                let busy = track_state(app, ti, id)
                    .map(|s| s.playing_slot.load(Ordering::Relaxed) >= 0)
                    .unwrap_or(false);
                let r = Rect::from_min_size(pos2(stops.left() + ti as f32 * COL_W, stops.top()), vec2(COL_W, STOP_H))
                    .shrink(2.0);
                let resp = ui.interact(r, ui.id().with(("stopbtn", id)), Sense::click());
                ui.painter().rect_filled(r, 2.0, Color32::from_rgb(0x40, 0x40, 0x40));
                widgets::stop_square(
                    ui.painter(),
                    r.center(),
                    8.0,
                    if busy {
                        theme::PLAY
                    } else if resp.hovered() {
                        theme::TEXT
                    } else {
                        theme::TEXT_DIM
                    },
                );
                if resp.on_hover_text("Stop clip").clicked() {
                    actions.push(Action::StopTrack(id));
                }
            }
            let r = Rect::from_min_size(pos2(stops.left() + master_x, stops.top()), vec2(MASTER_W, STOP_H)).shrink(2.0);
            let resp = ui.interact(r, ui.id().with("stop_all"), Sense::click());
            ui.painter().rect_filled(r, 2.0, Color32::from_rgb(0x40, 0x40, 0x40));
            ui.painter().text(
                r.center(),
                egui::Align2::CENTER_CENTER,
                "■ Stop All Clips",
                egui::FontId::proportional(11.0),
                if resp.hovered() { theme::TEXT } else { theme::TEXT_DIM },
            );
            if resp.clicked() {
                actions.push(Action::StopAll);
            }

            // ----- mixer -----
            let (mixer, _) = ui.allocate_exact_size(vec2(total_w, mixer_h), Sense::hover());
            ui.painter().rect_filled(mixer, 0.0, theme::BG_PANEL);
            for ti in 0..n_tracks {
                let id = app.project.tracks[ti].id;
                let meter = track_state(app, ti, id)
                    .map(|s| (s.meter_l.load(), s.meter_r.load()))
                    .unwrap_or((0.0, 0.0));
                let strip = Rect::from_min_size(
                    pos2(mixer.left() + ti as f32 * COL_W, mixer.top()),
                    vec2(COL_W, mixer_h),
                )
                .shrink2(vec2(3.0, 4.0));
                let (sends_rect, strip) = strip.split_top_bottom_at_y(strip.top() + send_rows as f32 * SEND_ROW_H);
                ui.painter().vline(
                    strip.right() + 3.0,
                    mixer.y_range(),
                    Stroke::new(1.0, Color32::from_rgb(0x28, 0x28, 0x28)),
                );
                let number = ti + 1;
                let t = &mut app.project.tracks[ti];
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
                ui.scope_builder(UiBuilder::new().max_rect(strip).id_salt(("strip", id)), |ui| {
                    if mixer_strip(ui, &mut t.mixer, meter, StripKind::Track(number)) {
                        actions.push(Action::Mixer(id));
                    }
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
                let strip = Rect::from_min_size(
                    pos2(
                        mixer.left() + ret_x + ri as f32 * COL_W,
                        mixer.top() + send_rows as f32 * SEND_ROW_H,
                    ),
                    vec2(COL_W, MIXER_H),
                )
                .shrink2(vec2(3.0, 4.0));
                ui.painter().vline(
                    strip.right() + 3.0,
                    mixer.y_range(),
                    Stroke::new(1.0, Color32::from_rgb(0x28, 0x28, 0x28)),
                );
                let letter = (b'A' + ri as u8) as char;
                ui.scope_builder(UiBuilder::new().max_rect(strip).id_salt(("rstrip", id)), |ui| {
                    if mixer_strip(ui, &mut ret.mixer, meter, StripKind::Return(letter)) {
                        actions.push(Action::BusMixer(id));
                    }
                });
            }
            let strip = Rect::from_min_size(
                pos2(mixer.left() + master_x, mixer.top() + send_rows as f32 * SEND_ROW_H),
                vec2(MASTER_W, MIXER_H),
            )
            .shrink2(vec2(3.0, 4.0));
            let meter = (
                app.engine.shared.master_meter_l.load(),
                app.engine.shared.master_meter_r.load(),
            );
            ui.scope_builder(UiBuilder::new().max_rect(strip).id_salt("master_strip"), |ui| {
                if mixer_strip(ui, &mut app.project.master, meter, StripKind::Master) {
                    actions.push(Action::Master);
                }
            });
        });

    for a in actions {
        match a {
            Action::Launch(t, s) => app.send(Command::LaunchClip { track: t, slot: s }),
            Action::StopTrack(t) => app.send(Command::StopTrack(t)),
            Action::LaunchScene(s) => app.send(Command::LaunchScene(s)),
            Action::StopAll => app.send(Command::StopAllClips),
            Action::Select(r) => {
                app.select_clip(r);
                if app.clip(r).is_some() {
                    app.detail = DetailMode::Clip;
                }
            }
            Action::SelectTrack(t) => app.selected_track = Some(t),
            Action::Create(t, s) => app.create_clip_in_slot(t, s),
            Action::Delete(r) => app.delete_clip(r),
            Action::Duplicate(t, s) => app.duplicate_slot(t, s),
            Action::ToArrangement(t, s) => app.slot_to_arrangement(t, s),
            Action::ImportDialog(t, s) => app.import_audio_dialog(t, s),
            // Files from the desktop behave like samples from the browser.
            Action::Import(p, t, s) => app.drop_item(&BrowserItem::Sample(p), DropTarget::Slot(t, s)),
            Action::Drop(item, target) => app.drop_item(&item, target),
            Action::DeleteTrack(t) => app.delete_track(t),
            Action::InsertScene(s) => {
                app.project.insert_scene(s);
                app.changed();
            }
            Action::DeleteScene(s) => {
                app.project.remove_scene(s);
                app.selected_clip = None;
                app.changed();
            }
            Action::AddScene => {
                app.project.add_scene();
                app.changed();
            }
            Action::Mixer(t) => app.send_mixer(t),
            Action::Master => {
                let m = app.project.master;
                app.send(Command::SetMaster(m));
                app.dirty = true;
            }
            Action::Changed => app.changed(),
            Action::RecordSlot(t, s) => app.record_slot(t, s),
            Action::Send(t, k, db) => app.set_send(t, k, db),
            Action::BusMixer(b) => app.send_bus_mixer(b),
            Action::DeleteReturn(r) => app.delete_return(r),
            Action::AddReturn => app.add_return_track(None),
        }
    }
}

/// One mixer channel: activator, solo, arm, pan, fader and meter. Return
/// and master strips have no solo or arm buttons.
pub fn mixer_strip(
    ui: &mut egui::Ui,
    m: &mut tessera_engine::model::MixerParams,
    meter: (f32, f32),
    kind: StripKind,
) -> bool {
    let (number, can_arm) = match kind {
        StripKind::Track(n) => (Some(n), true),
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
                .color(theme::TEXT_DIM),
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
                .on_hover_text("Track activator (mute)")
                .clicked()
            {
                m.mute = !m.mute;
                changed = true;
            }
            if number.is_some() {
                if widgets::toggle(ui, m.solo, "S", theme::SOLO, 22.0)
                    .on_hover_text("Solo")
                    .clicked()
                {
                    m.solo = !m.solo;
                    changed = true;
                }
                if can_arm
                    && widgets::toggle(ui, m.arm, "●", theme::ARM, 22.0)
                        .on_hover_text("Arm for recording")
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
