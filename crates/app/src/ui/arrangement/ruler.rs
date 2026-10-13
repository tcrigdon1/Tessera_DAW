//! The loop brace, the bar ruler with locators, and the time signature
//! strip above the Arrangement lanes.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Shape, Stroke};

use super::{Action, Geo, EDGE, LOOP_H, RULER_H, SIGNATURE_PRESETS, SIG_H};
use crate::app::{ArrDrag, TesseraApp};
use crate::ui::info_view::{self, InfoExt};
use crate::ui::theme;

const LOCATOR: Color32 = Color32::from_rgb(0xd8, 0xd8, 0xd8);

/// The loop brace (also the punch-in/out range).
pub(super) fn loop_brace(
    app: &mut TesseraApp,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    actions: &mut Vec<Action>,
    geo: &Geo,
    area: Rect,
) {
    let timeline = geo.timeline;
    let loop_rect = Rect::from_min_max(
        pos2(timeline.left(), area.top()),
        pos2(timeline.right(), area.top() + LOOP_H),
    );
    p.rect_filled(loop_rect, 0.0, theme::bg_bar());
    let (ls, le) = (app.project.loop_start, app.project.loop_start + app.project.loop_length);
    let brace = Rect::from_min_max(
        pos2(geo.x(ls), loop_rect.top() + 1.0),
        pos2(geo.x(le), loop_rect.bottom() - 1.0),
    );
    let brace_color = if app.project.loop_enabled {
        theme::ACCENT
    } else {
        theme::gray(0x70)
    };
    p.rect_filled(brace, 1.0, brace_color);
    // Punch In / Out: recording only happens inside the brace.
    let (pin, pout) = (app.project.punch_in, app.project.punch_out);
    if pin || pout {
        p.rect_filled(brace.shrink2(vec2(0.0, 2.0)), 0.0, theme::ARM.gamma_multiply(0.45));
        let edge = Stroke::new(3.0, theme::ARM);
        if pin {
            p.vline(brace.left() + 1.5, loop_rect.y_range(), edge);
        }
        if pout {
            p.vline(brace.right() - 1.5, loop_rect.y_range(), edge);
        }
    }
    let loop_resp = ui
        .interact(loop_rect, ui.id().with("loop"), Sense::click_and_drag())
        .info("Loop Brace", info_view::LOOP_BRACE);
    if loop_resp.double_clicked() {
        app.project.loop_enabled = !app.project.loop_enabled;
        actions.push(Action::Loop);
    }
    if loop_resp.drag_started() {
        if let Some(pt) = loop_resp.interact_pointer_pos() {
            app.arrangement.drag = if (pt.x - brace.left()).abs() < EDGE {
                Some(ArrDrag::LoopStart)
            } else if (pt.x - brace.right()).abs() < EDGE {
                Some(ArrDrag::LoopEnd)
            } else if brace.x_range().contains(pt.x) {
                Some(ArrDrag::LoopMove {
                    grab_offset: geo.beat(pt.x) - ls,
                })
            } else {
                None
            };
        }
    }
    if let Some(pt) = loop_resp.hover_pos() {
        if (pt.x - brace.left()).abs() < EDGE || (pt.x - brace.right()).abs() < EDGE {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
    }
    let tip = match (pin, pout) {
        (true, true) => "Loop / punch range · double-click: loop on/off",
        (true, false) => "Loop / punch-in point · double-click: loop on/off",
        (false, true) => "Loop / punch-out point · double-click: loop on/off",
        _ => "Loop brace · drag to move, drag edges to resize · double-click: loop on/off",
    };
    loop_resp.on_hover_text(tip);
}

/// Insert Time Signature Change presets.
fn signature_menu(ui: &mut egui::Ui, actions: &mut Vec<Action>, beat: f64) {
    for (n, d) in SIGNATURE_PRESETS {
        if ui.button(format!("{n}/{d}")).clicked() {
            actions.push(Action::AddSignature(beat, n, d));
            ui.close();
        }
    }
}

fn ctx_beat_id() -> egui::Id {
    egui::Id::new("tessera_arr_ruler_ctx_beat")
}

/// The bar ruler: bar numbers from the time map (so signature changes keep
/// the bars right) and locators. Click or drag to move the playhead.
/// Returns the ruler's rect.
pub(super) fn ruler(
    app: &mut TesseraApp,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    actions: &mut Vec<Action>,
    geo: &Geo,
    top: f32,
) -> Rect {
    let timeline = geo.timeline;
    let ruler = Rect::from_min_max(pos2(timeline.left(), top), pos2(timeline.right(), top + RULER_H));
    p.rect_filled(ruler, 0.0, theme::gray(0x30));
    let mid = ruler.top() + RULER_H * 0.5;
    let map = geo.map;
    let last = geo.last();
    let (mut bar, _) = map.bar_of(geo.scroll.max(0.0));
    let bar_px = (map.meter_at(geo.scroll).bar_beats() as f32 * geo.ppb).max(0.1);
    let label_every = ((36.0 / bar_px).ceil() as i64).max(1);
    while map.bar_start(bar) <= last {
        let x = geo.x(map.bar_start(bar));
        if bar % label_every == 0 {
            p.vline(x, ruler.top()..=mid, Stroke::new(1.0, theme::grid_bar()));
            p.text(
                pos2(x + 3.0, ruler.top() + RULER_H * 0.25),
                egui::Align2::LEFT_CENTER,
                format!("{}", bar + 1),
                egui::FontId::proportional(10.5),
                theme::text_dim(),
            );
        } else if bar_px >= 4.0 {
            p.vline(x, ruler.top() + 6.0..=mid, Stroke::new(1.0, theme::grid()));
        }
        bar += 1;
    }
    p.hline(ruler.x_range(), mid, Stroke::new(1.0, theme::gray(0x28)));

    let ruler_resp = ui.interact(ruler, ui.id().with("ruler"), Sense::click_and_drag());
    if ruler_resp.clicked() || ruler_resp.dragged() {
        if let Some(pt) = ruler_resp.interact_pointer_pos() {
            actions.push(Action::SetPosition(geo.snap(geo.beat(pt.x))));
        }
    }
    if ruler_resp.secondary_clicked() {
        if let Some(pt) = ruler_resp.interact_pointer_pos() {
            let b = geo.snap(geo.beat(pt.x));
            ui.ctx().data_mut(|d| d.insert_temp(ctx_beat_id(), b));
        }
    }
    let beat: f64 = ui.ctx().data(|d| d.get_temp(ctx_beat_id())).unwrap_or(0.0);
    ruler_resp.context_menu(|ui| {
        ui.label(
            egui::RichText::new(format!("At {}", map.format_position(beat)))
                .small()
                .color(theme::text_dim()),
        );
        if ui.button("Add Locator").clicked() {
            actions.push(Action::AddLocator(beat));
            ui.close();
        }
        ui.menu_button("Insert Time Signature Change", |ui| signature_menu(ui, actions, beat));
    });

    // Locators: a triangle and a name in the lower half of the ruler.
    let locators = app.project.locators.clone();
    for l in &locators {
        let x = geo.x(l.beat);
        if x < timeline.left() - 80.0 || x > timeline.right() {
            continue;
        }
        let galley = p.layout_no_wrap(l.name.clone(), egui::FontId::proportional(10.5), LOCATOR);
        let hit = Rect::from_min_max(pos2(x - 6.0, mid), pos2(x + 9.0 + galley.size().x, ruler.bottom()));
        let resp = ui.interact(hit, ui.id().with(("locator", l.id)), Sense::click());
        let color = if resp.hovered() { theme::select() } else { LOCATOR };
        p.add(Shape::convex_polygon(
            vec![
                pos2(x - 5.0, mid + 1.0),
                pos2(x + 5.0, mid + 1.0),
                pos2(x, ruler.bottom() - 2.0),
            ],
            color,
            Stroke::NONE,
        ));
        p.galley(pos2(x + 7.0, mid + 1.5), galley, color);
        if resp.clicked() {
            actions.push(Action::SetPosition(l.beat));
        }
        let (id, name) = (l.id, l.name.clone());
        resp.on_hover_text(format!("{} · click to jump", map.format_position(l.beat)))
            .context_menu(|ui| {
                let mut n = name.clone();
                ui.horizontal(|ui| {
                    ui.label("Name");
                    if ui.text_edit_singleline(&mut n).changed() {
                        actions.push(Action::RenameLocator(id, n.clone()));
                    }
                });
                if ui.button("Delete Locator").clicked() {
                    actions.push(Action::DeleteLocator(id));
                    ui.close();
                }
            });
    }
    ruler
}

/// The strip of time signature markers under the ruler.
pub(super) fn signatures(
    app: &mut TesseraApp,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    actions: &mut Vec<Action>,
    geo: &Geo,
    top: f32,
) {
    let timeline = geo.timeline;
    let strip = Rect::from_min_max(pos2(timeline.left(), top), pos2(timeline.right(), top + SIG_H));
    p.rect_filled(strip, 0.0, theme::gray(0x2b));
    p.hline(
        strip.x_range(),
        strip.bottom() - 0.5,
        Stroke::new(1.0, theme::bg_view()),
    );
    let strip_resp = ui.interact(strip, ui.id().with("sig_strip"), Sense::click());
    let id = egui::Id::new("tessera_arr_sig_ctx_beat");
    if strip_resp.secondary_clicked() {
        if let Some(pt) = strip_resp.interact_pointer_pos() {
            let b = geo.beat(pt.x).max(0.0);
            ui.ctx().data_mut(|d| d.insert_temp(id, b));
        }
    }
    let beat: f64 = ui.ctx().data(|d| d.get_temp(id)).unwrap_or(0.0);
    strip_resp
        .on_hover_text("Time signatures · right-click to insert a change")
        .context_menu(|ui| {
            ui.label(
                egui::RichText::new("Insert Time Signature Change")
                    .small()
                    .color(theme::text_dim()),
            );
            signature_menu(ui, actions, beat);
        });

    // The song's starting signature, then the changes.
    let mut markers = vec![(None, 0.0, app.project.beats_per_bar, app.project.beat_unit)];
    markers.extend(
        app.project
            .signatures
            .iter()
            .enumerate()
            .map(|(i, s)| (Some(i), s.beat, s.numerator, s.denominator)),
    );
    for (index, beat, n, d) in markers {
        let x = geo.x(beat);
        if x < timeline.left() - 40.0 || x > timeline.right() {
            continue;
        }
        let galley = p.layout_no_wrap(format!("{n}/{d}"), egui::FontId::proportional(10.0), theme::text());
        let r = Rect::from_min_size(pos2(x, strip.top()), vec2(galley.size().x + 8.0, SIG_H - 1.0));
        let resp = ui.interact(r, ui.id().with(("sig", index)), Sense::click());
        let fill = if resp.hovered() {
            theme::gray(0x5a)
        } else {
            theme::gray(0x44)
        };
        p.rect_filled(r, 1.0, fill);
        p.vline(x, strip.y_range(), Stroke::new(1.0, theme::ACCENT));
        p.galley(
            pos2(x + 4.0, r.center().y - galley.size().y * 0.5),
            galley,
            theme::text(),
        );
        match index {
            Some(i) => {
                resp.on_hover_text("Time signature change · right-click to delete")
                    .context_menu(|ui| {
                        if ui.button("Delete Time Signature Change").clicked() {
                            actions.push(Action::DeleteSignature(i));
                            ui.close();
                        }
                    });
            }
            None => {
                resp.on_hover_text("The song's time signature (set in the transport bar)");
            }
        }
    }
}
