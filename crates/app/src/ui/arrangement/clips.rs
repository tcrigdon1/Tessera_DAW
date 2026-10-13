//! Arrangement clips: drawing, fades, the clip context menu, group summary
//! lanes and take lanes.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Shape, Stroke, StrokeKind, UiBuilder};
use tessera_engine::arrange;
use tessera_engine::audio_to_midi::ConvertMode;
use tessera_engine::model::{ArrangementClip, ClipContent, Id, TrackKind};

use super::{begin_selection, draw_grid, set_local_drag, Action, Geo, LocalDrag, HEAD_W, INDENT};
use crate::app::{ClipRef, TesseraApp};
use crate::ui::theme;

/// Size of the fade handles at a clip's top corners.
const HANDLE: f32 = 7.0;

/// What the clip context menu needs to know.
pub(super) struct MenuInfo {
    pub track: Id,
    pub id: Id,
    pub audio: bool,
    pub frozen: bool,
    pub length: f64,
}

pub(super) fn clip_menu(ui: &mut egui::Ui, actions: &mut Vec<Action>, info: &MenuInfo) {
    ui.set_min_width(200.0);
    let r = ClipRef::Arrangement {
        track: info.track,
        id: info.id,
    };
    let edit = !info.frozen;
    if edit {
        crate::ui::chrome::arrangement_clip_menu_items(ui, info.track, info.id);
    }
    fn item(ui: &mut egui::Ui, actions: &mut Vec<Action>, enabled: bool, label: &str, action: Action) {
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            actions.push(action);
            ui.close();
        }
    }
    if info.frozen {
        ui.label(egui::RichText::new("Track is frozen").small().color(theme::text_dim()));
    }
    item(ui, actions, edit, "Split", Action::Split(r));
    item(ui, actions, edit, "Consolidate", Action::Consolidate(r));
    item(ui, actions, edit, "Reverse", Action::Reverse(r));
    item(
        ui,
        actions,
        edit,
        "Crossfade with Next",
        Action::Crossfade(info.track, info.id),
    );
    ui.add_enabled_ui(edit, |ui| {
        ui.menu_button("Fades", |ui| {
            let half = (info.length / 2.0).max(0.0);
            for (label, beats) in [("1/16", 0.25), ("1/8", 0.5), ("1 Beat", 1.0), ("1 Bar", 4.0)] {
                let b: f64 = f64::min(beats, half);
                if ui.button(format!("Fade In and Out {label}")).clicked() {
                    actions.push(Action::SetFades(info.track, info.id, b, b));
                    ui.close();
                }
            }
            ui.separator();
            if ui.button("Fade In 1 Beat").clicked() {
                actions.push(Action::SetFades(info.track, info.id, 1.0f64.min(info.length), 0.0));
                ui.close();
            }
            if ui.button("Fade Out 1 Beat").clicked() {
                actions.push(Action::SetFades(info.track, info.id, 0.0, 1.0f64.min(info.length)));
                ui.close();
            }
            if ui.button("Reset Fades").clicked() {
                actions.push(Action::SetFades(info.track, info.id, 0.0, 0.0));
                ui.close();
            }
        });
    });
    ui.separator();
    if info.audio {
        item(
            ui,
            actions,
            true,
            "Convert Melody to New MIDI Track",
            Action::Convert(r, ConvertMode::Melody),
        );
        item(
            ui,
            actions,
            true,
            "Convert Harmony to New MIDI Track",
            Action::Convert(r, ConvertMode::Harmony),
        );
        item(
            ui,
            actions,
            true,
            "Convert Drums to New MIDI Track",
            Action::Convert(r, ConvertMode::Drums),
        );
        ui.menu_button("Slice to New MIDI Track", |ui| {
            for (label, division) in [
                ("1/16", Some(0.25)),
                ("1/8", Some(0.5)),
                ("1/4 (Beat)", Some(1.0)),
                ("Bar", Some(4.0)),
                ("Transients", None),
            ] {
                if ui.button(label).clicked() {
                    actions.push(Action::Slice(r, division));
                    ui.close();
                }
            }
        });
        ui.separator();
    }
    item(ui, actions, true, "Extract Groove", Action::ExtractGroove(r));
    item(
        ui,
        actions,
        true,
        "Copy to Session",
        Action::ToSession(info.track, info.id),
    );
    ui.separator();
    item(ui, actions, edit, "Delete", Action::Delete(r));
}

/// Draws an arrangement clip with a title bar, a content preview and its
/// fades. `title` is false for the slim take-lane clips.
pub(super) fn draw_clip(
    app: &mut TesseraApp,
    p: &egui::Painter,
    rect: Rect,
    a: &ArrangementClip,
    selected: bool,
    dimmed: bool,
    title: bool,
) {
    let color = theme::rgb(a.clip.color);
    let color = if dimmed { theme::dim(color, 0.6) } else { color };
    p.rect_filled(rect, 2.0, theme::dim(color, 0.45));
    let title_h = if title { 14.0 } else { 11.0 };
    let bar = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + title_h));
    p.rect_filled(bar, 2.0, color);
    if rect.width() > 14.0 {
        let text_p = p.with_clip_rect(bar.intersect(p.clip_rect()));
        text_p.text(
            pos2(bar.left() + 4.0, bar.center().y),
            egui::Align2::LEFT_CENTER,
            &a.clip.name,
            egui::FontId::proportional(if title { 10.5 } else { 9.5 }),
            Color32::BLACK,
        );
    }
    let body = Rect::from_min_max(
        pos2(rect.left(), bar.bottom() + 2.0),
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
        ClipContent::Midi { notes } if !notes.is_empty() && body.height() > 2.0 => {
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
                let warp = &audio.warp;
                let dur = data.duration_secs();
                let cy = body.center().y;
                let h = body.height() / 2.0;
                let step = 2.0;
                // Only the visible part.
                let mut x = rect.left().max(p.clip_rect().left());
                let right = rect.right().min(p.clip_rect().right());
                while x < right {
                    let beat = ((x - rect.left()) / ppb) as f64;
                    let pos = if a.clip.looping { beat % len } else { beat };
                    let t = if warp.enabled {
                        warp.beat_to_secs(pos)
                    } else {
                        pos * secs_per_beat * warp.rate()
                    };
                    if (0.0..dur).contains(&t) && pos < len {
                        let idx = ((t / dur) * peaks.len() as f64) as usize;
                        let mut v = peaks.get(idx).copied().unwrap_or(0.0).min(1.0);
                        v *= fade_gain(a, beat) as f32;
                        p.vline(x, (cy - v * h)..=(cy + v * h), Stroke::new(1.0, color));
                    }
                    x += step;
                }
            }
        }
        _ => {}
    }
    draw_fades(p, rect, a, ppb);
    if dimmed {
        p.rect_filled(rect, 2.0, Color32::from_black_alpha(60));
    }
    if selected {
        p.rect_stroke(rect, 2.0, Stroke::new(1.5, theme::select()), StrokeKind::Inside);
    }
}

/// Linear fade gain at a beat inside the clip.
fn fade_gain(a: &ArrangementClip, beat: f64) -> f64 {
    let mut g = 1.0;
    if a.fade_in > 0.0 && beat < a.fade_in {
        g *= beat / a.fade_in;
    }
    if a.fade_out > 0.0 && beat > a.length - a.fade_out {
        g *= (a.length - beat) / a.fade_out;
    }
    g.clamp(0.0, 1.0)
}

/// Shades the faded parts of a clip and draws the fade curves.
fn draw_fades(p: &egui::Painter, rect: Rect, a: &ArrangementClip, ppb: f32) {
    let shade = Color32::from_black_alpha(110);
    let line = Stroke::new(1.2, Color32::from_white_alpha(200));
    if a.fade_in > 0.0 {
        let x = rect.left() + a.fade_in as f32 * ppb;
        p.add(Shape::convex_polygon(
            vec![rect.left_top(), pos2(x, rect.top()), rect.left_bottom()],
            shade,
            Stroke::NONE,
        ));
        p.line_segment([rect.left_bottom(), pos2(x, rect.top())], line);
    }
    if a.fade_out > 0.0 {
        let x = rect.right() - a.fade_out as f32 * ppb;
        p.add(Shape::convex_polygon(
            vec![pos2(x, rect.top()), rect.right_top(), rect.right_bottom()],
            shade,
            Stroke::NONE,
        ));
        p.line_segment([pos2(x, rect.top()), rect.right_bottom()], line);
    }
}

/// Fade-in and fade-out handles at the clip's top corners; dragging one
/// starts a fade drag.
pub(super) fn fade_handles(ui: &egui::Ui, p: &egui::Painter, a: &ArrangementClip, track: Id, rect: Rect, show: bool) {
    if rect.width() < 4.0 * HANDLE {
        return;
    }
    let ppb = rect.width() / a.length.max(1e-6) as f32;
    let xin = (rect.left() + a.fade_in as f32 * ppb).clamp(rect.left(), rect.right() - HANDLE);
    let xout = (rect.right() - a.fade_out as f32 * ppb).clamp(rect.left() + HANDLE, rect.right());
    let handles = [
        (
            Rect::from_min_size(pos2(xin, rect.top()), vec2(HANDLE, HANDLE)),
            LocalDrag::FadeIn { track, id: a.id },
            "fade_in",
            "Drag to fade in",
        ),
        (
            Rect::from_min_size(pos2(xout - HANDLE, rect.top()), vec2(HANDLE, HANDLE)),
            LocalDrag::FadeOut { track, id: a.id },
            "fade_out",
            "Drag to fade out",
        ),
    ];
    for (r, drag, salt, tip) in handles {
        // Click as well, so the handle sits on top of the clip for drags.
        let resp = ui.interact(r, ui.id().with((salt, a.id)), Sense::click_and_drag());
        if resp.drag_started() {
            set_local_drag(ui.ctx(), Some(drag));
        }
        if show || resp.hovered() {
            let fill = if resp.hovered() {
                theme::select()
            } else {
                Color32::from_white_alpha(170)
            };
            p.rect_filled(r.shrink(0.5), 1.0, fill);
            p.rect_stroke(r.shrink(0.5), 1.0, Stroke::new(1.0, Color32::BLACK), StrokeKind::Inside);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            resp.on_hover_text(tip);
        }
    }
}

/// A group's lane: thin bars showing where its members have clips.
pub(super) fn group_summary(app: &TesseraApp, p: &egui::Painter, geo: &Geo, ti: usize, lane: Rect) {
    let group = &app.project.tracks[ti];
    p.rect_filled(
        lane.shrink2(vec2(0.0, 1.0)),
        0.0,
        theme::rgb(group.color).gamma_multiply(0.06),
    );
    let members: Vec<usize> = arrange::track_block(&app.project, group.id)
        .into_iter()
        .skip(1)
        .filter(|&i| app.project.tracks[i].kind != TrackKind::Group)
        .collect();
    if members.is_empty() {
        p.text(
            lane.left_center() + vec2(8.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "Empty group",
            egui::FontId::proportional(10.5),
            theme::text_dim(),
        );
        return;
    }
    let body = lane.shrink2(vec2(0.0, 6.0));
    let h = (body.height() / members.len() as f32).min(7.0);
    let gap = if h > 3.0 { 1.0 } else { 0.0 };
    for (k, &i) in members.iter().enumerate() {
        let t = &app.project.tracks[i];
        let y = body.top() + k as f32 * h;
        for a in &t.arrangement {
            let (x0, x1) = (geo.x(a.start), geo.x(a.end()));
            if x1 < lane.left() || x0 > lane.right() {
                continue;
            }
            let r = Rect::from_min_max(pos2(x0, y), pos2(x1.max(x0 + 2.0), y + h - gap));
            p.rect_filled(r, 1.0, theme::rgb(a.clip.color));
        }
    }
}

/// A take lane below its track: dimmed clips, its name and a delete button.
/// Drag selects a range to comp; clicking a clip comps all of it.
#[allow(clippy::too_many_arguments)]
pub(super) fn take_lane(
    app: &mut TesseraApp,
    ui: &mut egui::Ui,
    head_painter: &egui::Painter,
    p: &egui::Painter,
    actions: &mut Vec<Action>,
    geo: &Geo,
    ti: usize,
    k: usize,
    rect: Rect,
) {
    let track = &app.project.tracks[ti];
    let (id, color, frozen) = (track.id, theme::rgb(track.color), track.frozen);
    let depth = arrange::depth(&app.project, id) as f32;
    let take = track.takes[k].clone();
    p.rect_filled(rect, 0.0, theme::gray(0x27));
    draw_grid(p, geo.lines, rect.y_range());
    p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::bg_view()));
    let resp = ui.interact(rect, ui.id().with(("take_lane", take.id)), Sense::click_and_drag());
    if resp.clicked() {
        actions.push(Action::ClearSelection);
    }
    if !frozen {
        begin_selection(ui, &resp, geo, id, Some(take.id));
    }
    for a in &take.clips {
        let (x0, x1) = (geo.x(a.start), geo.x(a.end()));
        if x1 < rect.left() || x0 > rect.right() {
            continue;
        }
        let r = Rect::from_min_max(pos2(x0, rect.top() + 2.0), pos2(x1.max(x0 + 3.0), rect.bottom() - 2.0));
        draw_clip(app, p, r, a, false, true, false);
        let cr = ui.interact(r, ui.id().with(("take_clip", take.id, a.id)), Sense::click_and_drag());
        if !frozen {
            begin_selection(ui, &cr, geo, id, Some(take.id));
            if cr.clicked() {
                actions.push(Action::Comp(id, take.id, a.start, a.end()));
            }
            cr.on_hover_text("Click to comp this take · drag to comp a range");
        }
    }

    // Header.
    let left = rect.left() - HEAD_W;
    let head = Rect::from_min_max(
        pos2(left + 2.0 + depth * INDENT + 14.0, rect.top() + 1.0),
        pos2(rect.left() - 2.0, rect.bottom() - 1.0),
    );
    head_painter.rect_filled(head, 2.0, theme::gray(0x2e));
    head_painter.rect_filled(
        Rect::from_min_size(head.min, vec2(3.0, head.height())),
        0.0,
        theme::dim(color, 0.6),
    );
    ui.scope_builder(
        UiBuilder::new()
            .max_rect(head.shrink2(vec2(7.0, 3.0)))
            .id_salt(("take_head", take.id)),
        |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 0.0);
                ui.add(egui::Label::new(egui::RichText::new(&take.name).small().color(theme::text())).truncate());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!frozen, egui::Button::new("×").small())
                        .on_hover_text("Delete take lane")
                        .clicked()
                    {
                        actions.push(Action::DeleteTake(id, take.id));
                    }
                });
            });
        },
    );
}
