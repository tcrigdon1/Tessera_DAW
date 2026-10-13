//! Custom panels for dynamics devices. The Bus Compressor, Gate and Drum Bus
//! use the generic knob layout (with its gain reduction meter); Multiband
//! Dynamics gets a threshold display and three band columns.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke};
use tessera_engine::fx::multiband::{band, MultibandParams, BANDS, BAND_NAMES, GLOBAL};
use tessera_engine::fx::{DeviceParams, ParamInfo, ParamKind, Scale};

use super::super::{devices, theme, widgets};
use super::PanelCtx;

pub fn body(ui: &mut egui::Ui, ctx: &PanelCtx, params: &DeviceParams, out: &mut Vec<(usize, f32)>) -> bool {
    match params {
        DeviceParams::MultibandDynamics(p) => {
            multiband_body(ui, ctx, params, p, out);
            true
        }
        _ => false,
    }
}

const BAND_COLORS: [Color32; BANDS] = [
    Color32::from_rgb(0xff, 0x8a, 0x5c),
    Color32::from_rgb(0xf5, 0xd0, 0x4c),
    Color32::from_rgb(0x6c, 0xc8, 0xff),
];
const DISPLAY_MIN_DB: f32 = -80.0;

fn multiband_body(
    ui: &mut egui::Ui,
    ctx: &PanelCtx,
    params: &DeviceParams,
    p: &MultibandParams,
    out: &mut Vec<(usize, f32)>,
) {
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing = vec2(4.0, 2.0);
        threshold_display(ui, ctx, p, out);
        for b in 0..BANDS {
            band_column(ui, ctx, p, b, out);
        }
        let globals: Vec<usize> = (GLOBAL..MultibandParams::COUNT).collect();
        super::knob_columns(ui, ctx, params, &globals, out);
        devices::gain_reduction_meter(ui, ctx.meter.unwrap_or(0.0));
    });
}

/// Three columns showing each band's above (orange) and below (blue)
/// thresholds; drag a line to move it.
fn threshold_display(ui: &mut egui::Ui, ctx: &PanelCtx, p: &MultibandParams, out: &mut Vec<(usize, f32)>) {
    let (rect, resp) = ui.allocate_exact_size(vec2(132.0, 124.0), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, theme::gray(0x1e));
    let plot = Rect::from_min_max(rect.min + vec2(0.0, 2.0), pos2(rect.right(), rect.bottom() - 14.0));
    let y_of = |db: f32| plot.top() + plot.height() * (db.clamp(DISPLAY_MIN_DB, 0.0) / DISPLAY_MIN_DB);
    let db_of = |y: f32| ((y - plot.top()) / plot.height()).clamp(0.0, 1.0) * DISPLAY_MIN_DB;
    let col_w = rect.width() / BANDS as f32;
    let col_rect = |b: usize| {
        Rect::from_min_max(
            pos2(plot.left() + col_w * b as f32 + 3.0, plot.top()),
            pos2(plot.left() + col_w * (b + 1) as f32 - 3.0, plot.bottom()),
        )
    };
    let grid = Stroke::new(1.0, theme::gray(0x33));
    for db in [-20.0, -40.0, -60.0] {
        painter.hline(plot.x_range(), y_of(db), grid);
    }
    let accent = theme::ACCENT;
    let below_color = theme::SOLO;
    let any_solo = (0..BANDS).any(|b| p.band(b).solo);
    for b in 0..BANDS {
        let s = p.band(b);
        let r = col_rect(b);
        let audible = !s.mute && (!any_solo || s.solo);
        let fill = if audible { theme::gray(0x2a) } else { theme::gray(0x22) };
        painter.rect_filled(r, 1.0, fill);
        if s.active {
            let ya = y_of(s.above_thr);
            let yb = y_of(s.below_thr);
            if (s.above_ratio - 1.0).abs() > 0.01 {
                painter.rect_filled(
                    Rect::from_min_max(r.min, pos2(r.right(), ya)),
                    0.0,
                    theme::dim(accent, 0.35),
                );
            }
            if (s.below_ratio - 1.0).abs() > 0.01 {
                painter.rect_filled(
                    Rect::from_min_max(pos2(r.left(), yb), r.max),
                    0.0,
                    theme::dim(below_color, 0.35),
                );
            }
            painter.hline(r.x_range(), ya, Stroke::new(2.0, accent));
            painter.hline(r.x_range(), yb, Stroke::new(2.0, below_color));
        }
        let label_color = if audible { BAND_COLORS[b] } else { theme::TEXT_DIM };
        painter.text(
            pos2(r.center().x, rect.bottom() - 2.0),
            egui::Align2::CENTER_BOTTOM,
            BAND_NAMES[b],
            egui::FontId::proportional(10.0),
            label_color,
        );
    }

    // Dragging: pick the nearest threshold line in the column under the pointer.
    let drag_id = ui.id().with(("mb_drag", ctx.id));
    let pointer = resp.interact_pointer_pos();
    if resp.drag_started() {
        let pick = pointer.and_then(|pp| {
            let b = (((pp.x - plot.left()) / col_w).floor().max(0.0) as usize).min(BANDS - 1);
            let s = p.band(b);
            let da = (y_of(s.above_thr) - pp.y).abs();
            let db = (y_of(s.below_thr) - pp.y).abs();
            let param = if da <= db {
                band::ABOVE_THRESHOLD
            } else {
                band::BELOW_THRESHOLD
            };
            (da.min(db) < 14.0).then_some(MultibandParams::index(b, param))
        });
        ui.data_mut(|d| d.insert_temp::<Option<usize>>(drag_id, pick));
    }
    if resp.dragged() {
        let target: Option<usize> = ui.data(|d| d.get_temp(drag_id)).flatten();
        if let (Some(i), Some(pp)) = (target, pointer) {
            let info = &MultibandParams::INFO[i];
            out.push((i, info.clamp(db_of(pp.y))));
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.insert_temp::<Option<usize>>(drag_id, None));
    }
    let hover = resp.hover_pos().map(|pp| db_of(pp.y));
    resp.on_hover_text(match hover {
        Some(db) => format!("{db:.1} dB  (drag a line to set the above/below threshold)"),
        None => String::new(),
    });
}

fn short_knob(ui: &mut egui::Ui, label: &str, info: &ParamInfo, value: f32) -> Option<f32> {
    let scale = match info.kind {
        ParamKind::Continuous(Scale::Log) => widgets::Scale::Log,
        _ => widgets::Scale::Linear,
    };
    let mut v = value;
    widgets::knob(ui, label, &mut v, info.min, info.max, info.default, scale, |x| {
        info.format(x)
    })
    .then_some(v)
}

fn band_column(ui: &mut egui::Ui, _ctx: &PanelCtx, p: &MultibandParams, b: usize, out: &mut Vec<(usize, f32)>) {
    let idx = |k: usize| MultibandParams::index(b, k);
    let info = MultibandParams::INFO;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 0.0);
        ui.horizontal(|ui| {
            for (k, label) in [(band::ABOVE_RATIO, "Above"), (band::BELOW_RATIO, "Below")] {
                if let Some(v) = short_knob(ui, label, &info[idx(k)], p.get(idx(k))) {
                    out.push((idx(k), v));
                }
            }
        });
        ui.horizontal(|ui| {
            let k = band::GAIN;
            let label = BAND_NAMES[b];
            if let Some(v) = short_knob(ui, label, &info[idx(k)], p.get(idx(k))) {
                out.push((idx(k), v));
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 2.0);
                ui.add_space(2.0);
                for (k, text, color) in [
                    (band::ACTIVE, "On", BAND_COLORS[b]),
                    (band::SOLO, "S", theme::SOLO),
                    (band::MUTE, "M", theme::ARM),
                ] {
                    let on = p.get(idx(k)) >= 0.5;
                    let resp = widgets::toggle(ui, on, text, color, 40.0);
                    if resp.clicked() {
                        out.push((idx(k), if on { 0.0 } else { 1.0 }));
                    }
                }
            });
        });
    });
}
