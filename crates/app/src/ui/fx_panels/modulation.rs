//! Custom panels for the modulation effects: Auto Filter (response curve
//! with LFO sweep range, drag to set cutoff and resonance) and Auto Pan
//! (left / right gain preview over one LFO cycle).

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke};
use tessera_engine::fx::auto_filter::{AutoFilterParams, ENV_OCTAVES, LFO_OCTAVES};
use tessera_engine::fx::auto_pan::AutoPanParams;
use tessera_engine::fx::DeviceParams;

use super::super::theme;
use super::{knob_columns, PanelCtx};

const DISPLAY_H: f32 = 124.0;
const MIN_HZ: f32 = 20.0;
const MAX_HZ: f32 = 20000.0;
const TOP_DB: f32 = 24.0;
const BOTTOM_DB: f32 = -36.0;

pub fn body(ui: &mut egui::Ui, ctx: &PanelCtx, params: &DeviceParams, out: &mut Vec<(usize, f32)>) -> bool {
    match params {
        DeviceParams::AutoFilter(p) => {
            ui.horizontal_top(|ui| {
                filter_display(ui, ctx, p, out);
                knob_columns(ui, ctx, params, &relevant(params, AutoFilterParams::COUNT), out);
            });
            true
        }
        DeviceParams::AutoPan(p) => {
            ui.horizontal_top(|ui| {
                pan_display(ui, p);
                knob_columns(ui, ctx, params, &relevant(params, AutoPanParams::COUNT), out);
            });
            true
        }
        _ => false,
    }
}

fn relevant(params: &DeviceParams, count: usize) -> Vec<usize> {
    (0..count).filter(|&i| params.is_relevant(i)).collect()
}

fn background(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, 2.0, theme::gray(0x1e));
}

fn filter_display(ui: &mut egui::Ui, ctx: &PanelCtx, p: &AutoFilterParams, out: &mut Vec<(usize, f32)>) {
    let sr = if ctx.sample_rate > 1000.0 {
        ctx.sample_rate
    } else {
        48000.0
    };
    let max_hz = MAX_HZ.min(sr * 0.49);
    let (rect, resp) = ui.allocate_exact_size(vec2(200.0, DISPLAY_H), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    background(&painter, rect);
    let span = (max_hz / MIN_HZ).ln();
    let x_of = |f: f32| rect.left() + rect.width() * ((f.max(MIN_HZ) / MIN_HZ).ln() / span).clamp(0.0, 1.0);
    let f_of = |x: f32| MIN_HZ * (max_hz / MIN_HZ).powf(((x - rect.left()) / rect.width()).clamp(0.0, 1.0));
    let y_of = |db: f32| {
        let t = (TOP_DB - db.clamp(BOTTOM_DB, TOP_DB)) / (TOP_DB - BOTTOM_DB);
        rect.top() + 4.0 + t * (rect.height() - 8.0)
    };
    let grid = Stroke::new(1.0, theme::gray(0x33));
    for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
        if f < max_hz {
            painter.vline(x_of(f), rect.y_range(), grid);
        }
    }
    for db in [-24.0, -12.0, 12.0] {
        painter.hline(rect.x_range(), y_of(db), grid);
    }
    painter.hline(rect.x_range(), y_of(0.0), Stroke::new(1.0, theme::gray(0x4a)));
    for (f, label) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
        painter.text(
            pos2(x_of(f) + 2.0, rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            label,
            egui::FontId::proportional(9.0),
            theme::TEXT_DIM,
        );
    }

    let curve = |cutoff: f32| -> Vec<egui::Pos2> {
        (0..=120)
            .map(|i| {
                let x = rect.left() + rect.width() * i as f32 / 120.0;
                let db = p.response_db_at(cutoff, f_of(x), sr);
                pos2(x, y_of(if db.is_finite() { db } else { BOTTOM_DB }))
            })
            .collect()
    };

    // Sweep range of the LFO and envelope.
    let mut lo_oct = 0.0f32;
    let mut hi_oct = 0.0f32;
    if p.lfo_amount > 0.0 {
        lo_oct -= p.lfo_amount * LFO_OCTAVES;
        hi_oct += p.lfo_amount * LFO_OCTAVES;
    }
    if p.env_amount > 0.0 {
        hi_oct += p.env_amount * ENV_OCTAVES;
    } else if p.env_amount < 0.0 {
        lo_oct += p.env_amount * ENV_OCTAVES;
    }
    let lo = (p.freq * lo_oct.exp2()).clamp(MIN_HZ, max_hz);
    let hi = (p.freq * hi_oct.exp2()).clamp(MIN_HZ, max_hz);
    if hi > lo * 1.01 {
        let band = Rect::from_x_y_ranges(x_of(lo)..=x_of(hi), rect.y_range());
        painter.rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(0xff, 0x9c, 0x2b, 0x18));
        let faint = Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xff, 0x9c, 0x2b, 0x60));
        painter.add(egui::Shape::line(curve(lo), faint));
        painter.add(egui::Shape::line(curve(hi), faint));
    }
    painter.add(egui::Shape::line(curve(p.freq), Stroke::new(1.5, theme::ACCENT)));
    let cx = x_of(p.freq);
    painter.vline(
        cx,
        rect.y_range(),
        Stroke::new(1.0, Color32::from_rgba_unmultiplied(0xff, 0xff, 0xff, 0x40)),
    );
    let label = tessera_engine::fx::auto_filter::FILTER_TYPES
        .get(p.kind())
        .copied()
        .unwrap_or("");
    painter.text(
        pos2(rect.left() + 4.0, rect.top() + 3.0),
        egui::Align2::LEFT_TOP,
        format!("{label}  {}", AutoFilterParams::INFO[2].format(p.freq)),
        egui::FontId::proportional(10.0),
        theme::TEXT,
    );

    // Drag: horizontal sets the cutoff, vertical the resonance.
    if resp.dragged() {
        if let Some(pp) = resp.interact_pointer_pos() {
            out.push((2, f_of(pp.x)));
        }
        let dy = resp.drag_delta().y;
        if dy != 0.0 {
            out.push((3, (p.resonance - dy / 150.0).clamp(0.0, 1.0)));
        }
    }
    if resp.double_clicked() {
        out.push((2, AutoFilterParams::INFO[2].default));
        out.push((3, AutoFilterParams::INFO[3].default));
    }
    resp.on_hover_text("Drag left/right for frequency, up/down for resonance");
}

fn pan_display(ui: &mut egui::Ui, p: &AutoPanParams) {
    let (rect, _) = ui.allocate_exact_size(vec2(150.0, DISPLAY_H), Sense::hover());
    let painter = ui.painter_at(rect);
    background(&painter, rect);
    let inner = rect.shrink2(vec2(4.0, 12.0));
    let y_of = |g: f32| inner.bottom() - g.clamp(0.0, 1.0) * inner.height();
    let grid = Stroke::new(1.0, theme::gray(0x33));
    for t in [0.25, 0.5, 0.75] {
        painter.vline(inner.left() + inner.width() * t, rect.y_range(), grid);
    }
    painter.hline(inner.x_range(), y_of(1.0), grid);
    painter.hline(inner.x_range(), y_of(0.0), grid);
    let steps = 120;
    let offset = p.offset / 360.0;
    let (mut left, mut right) = (Vec::with_capacity(steps + 1), Vec::with_capacity(steps + 1));
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let (gl, gr) = p.preview(t + offset);
        let x = inner.left() + inner.width() * t;
        left.push(pos2(x, y_of(gl)));
        right.push(pos2(x, y_of(gr)));
    }
    painter.add(egui::Shape::line(right, Stroke::new(1.5, theme::SOLO)));
    painter.add(egui::Shape::line(left, Stroke::new(1.5, theme::ACCENT)));
    painter.text(
        pos2(rect.left() + 4.0, rect.top() + 1.0),
        egui::Align2::LEFT_TOP,
        "L",
        egui::FontId::proportional(10.0),
        theme::ACCENT,
    );
    painter.text(
        pos2(rect.left() + 14.0, rect.top() + 1.0),
        egui::Align2::LEFT_TOP,
        "R",
        egui::FontId::proportional(10.0),
        theme::SOLO,
    );
    let rate = if p.sync >= 0.5 {
        AutoPanParams::INFO[3].format(p.div)
    } else {
        AutoPanParams::INFO[2].format(p.rate)
    };
    painter.text(
        pos2(rect.right() - 4.0, rect.top() + 1.0),
        egui::Align2::RIGHT_TOP,
        rate,
        egui::FontId::proportional(10.0),
        theme::TEXT_DIM,
    );
}
