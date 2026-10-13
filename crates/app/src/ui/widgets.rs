//! Custom DAW widgets: knobs, faders, meters and toggle buttons.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};

use super::theme;

/// How a knob maps its drag position to a value.
#[derive(Clone, Copy)]
pub enum Scale {
    Linear,
    /// Logarithmic, for frequencies and times. Range must be positive.
    Log,
}

fn to_norm(v: f32, min: f32, max: f32, scale: Scale) -> f32 {
    match scale {
        Scale::Linear => (v - min) / (max - min),
        Scale::Log => (v.max(min).ln() - min.ln()) / (max.ln() - min.ln()),
    }
    .clamp(0.0, 1.0)
}

fn from_norm(n: f32, min: f32, max: f32, scale: Scale) -> f32 {
    let n = n.clamp(0.0, 1.0);
    match scale {
        Scale::Linear => min + n * (max - min),
        Scale::Log => (min.ln() + n * (max.ln() - min.ln())).exp(),
    }
}

/// A rotary knob with a label above and the value below. Drag vertically to
/// change it, hold Shift for fine control, double-click to reset.
#[allow(clippy::too_many_arguments)]
pub fn knob(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    default: f32,
    scale: Scale,
    format: impl Fn(f32) -> String,
) -> bool {
    let size = vec2(52.0, 62.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let delta = -resp.drag_delta().y / if fine { 600.0 } else { 150.0 };
        let n = to_norm(*value, min, max, scale) + delta;
        let nv = from_norm(n, min, max, scale);
        if nv != *value {
            *value = nv;
            changed = true;
        }
    }
    if resp.double_clicked() {
        *value = default;
        changed = true;
    }
    let p = ui.painter();
    p.text(
        pos2(rect.center().x, rect.top() + 6.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(10.0),
        theme::TEXT_DIM,
    );
    let c = pos2(rect.center().x, rect.top() + 28.0);
    let r = 13.0;
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let n = to_norm(*value, min, max, scale);
    let arc = |from: f32, to: f32, color: Color32| {
        let steps = 24;
        let pts: Vec<_> = (0..=steps)
            .map(|i| {
                let a = from + (to - from) * i as f32 / steps as f32;
                pos2(c.x + a.cos() * r, c.y + a.sin() * r)
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(3.0, color)));
    };
    arc(start, start + sweep, Color32::from_rgb(0x55, 0x55, 0x55));
    if n > 0.0 {
        arc(start, start + sweep * n, theme::ACCENT);
    }
    let a = start + sweep * n;
    p.line_segment(
        [c, pos2(c.x + a.cos() * (r - 2.0), c.y + a.sin() * (r - 2.0))],
        Stroke::new(2.0, theme::TEXT),
    );
    p.text(
        pos2(rect.center().x, rect.bottom() - 7.0),
        egui::Align2::CENTER_CENTER,
        format(*value),
        egui::FontId::proportional(10.0),
        if resp.hovered() || resp.dragged() {
            theme::TEXT
        } else {
            theme::TEXT_DIM
        },
    );
    changed
}

/// Small pan knob without label, -1..1.
pub fn pan_knob(ui: &mut egui::Ui, pan: &mut f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::click_and_drag());
    let mut changed = false;
    if resp.dragged() {
        *pan = (*pan - resp.drag_delta().y / 100.0).clamp(-1.0, 1.0);
        changed = true;
    }
    if resp.double_clicked() {
        *pan = 0.0;
        changed = true;
    }
    let p = ui.painter();
    let c = rect.center();
    let r = 10.0;
    p.circle_stroke(c, r, Stroke::new(2.0, Color32::from_rgb(0x55, 0x55, 0x55)));
    let top = -std::f32::consts::FRAC_PI_2;
    let a = top + *pan * std::f32::consts::PI * 0.75;
    let steps = 16;
    if pan.abs() > 0.01 {
        let pts: Vec<_> = (0..=steps)
            .map(|i| {
                let t = top + (a - top) * i as f32 / steps as f32;
                pos2(c.x + t.cos() * r, c.y + t.sin() * r)
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0, theme::ACCENT)));
    }
    p.line_segment(
        [c, pos2(c.x + a.cos() * r, c.y + a.sin() * r)],
        Stroke::new(2.0, theme::TEXT),
    );
    let text = if pan.abs() < 0.01 {
        "C".to_string()
    } else if *pan < 0.0 {
        format!("{:.0}L", -*pan * 50.0)
    } else {
        format!("{:.0}R", *pan * 50.0)
    };
    resp.on_hover_text(format!("Pan {text}"));
    changed
}

/// Small send knob (-inf..0 dB) with its return's letter. Drag vertically,
/// double-click to switch the send off.
pub fn send_knob(ui: &mut egui::Ui, db: &mut f32, letter: char) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(25.0, 32.0), Sense::click_and_drag());
    let top = db_to_pos(0.0);
    let mut changed = false;
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let n = (db_to_pos(*db) / top - resp.drag_delta().y / if fine { 600.0 } else { 120.0 }).clamp(0.0, 1.0);
        *db = pos_to_db(n * top);
        changed = true;
    }
    if resp.double_clicked() {
        *db = -70.0;
        changed = true;
    }
    let p = ui.painter();
    p.text(
        pos2(rect.center().x, rect.top() + 5.0),
        egui::Align2::CENTER_CENTER,
        letter,
        egui::FontId::proportional(9.0),
        theme::TEXT_DIM,
    );
    let c = pos2(rect.center().x, rect.top() + 20.0);
    let r = 9.0;
    let start = std::f32::consts::PI * 0.75;
    let sweep = std::f32::consts::PI * 1.5;
    let n = (db_to_pos(*db) / top).clamp(0.0, 1.0);
    let arc = |from: f32, to: f32, color: Color32| {
        let pts: Vec<_> = (0..=16)
            .map(|i| {
                let a = from + (to - from) * i as f32 / 16.0;
                pos2(c.x + a.cos() * r, c.y + a.sin() * r)
            })
            .collect();
        p.add(egui::Shape::line(pts, Stroke::new(2.0, color)));
    };
    arc(start, start + sweep, Color32::from_rgb(0x55, 0x55, 0x55));
    if n > 0.0 {
        arc(start, start + sweep * n, theme::SOLO);
    }
    resp.on_hover_text(format!("Send {letter}: {} dB", format_db(*db)));
    changed
}

/// Maps dB to fader position 0..1.
fn db_to_pos(db: f32) -> f32 {
    if db <= -70.0 {
        0.0
    } else if db <= -12.0 {
        (db + 70.0) / 58.0 * 0.5
    } else {
        0.5 + (db + 12.0) / 18.0 * 0.5
    }
}

fn pos_to_db(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    if p <= 0.0 {
        -70.0
    } else if p <= 0.5 {
        p / 0.5 * 58.0 - 70.0
    } else {
        (p - 0.5) / 0.5 * 18.0 - 12.0
    }
}

pub fn format_db(db: f32) -> String {
    if db <= -70.0 {
        "-inf".into()
    } else {
        format!("{db:.1}")
    }
}

/// Peak level to meter position, using the same curve as the fader.
fn level_to_pos(level: f32) -> f32 {
    if level <= 0.0 {
        0.0
    } else {
        db_to_pos(20.0 * level.log10())
    }
}

/// Vertical volume fader (dB, -inf..+6) with a stereo meter next to it.
pub fn fader_with_meter(ui: &mut egui::Ui, db: &mut f32, meter: (f32, f32), height: f32) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, height), Sense::click_and_drag());
    let mut changed = false;
    let track = Rect::from_min_max(
        pos2(rect.left() + 4.0, rect.top() + 4.0),
        pos2(rect.left() + 10.0, rect.bottom() - 4.0),
    );
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let pos = db_to_pos(*db) - resp.drag_delta().y / track.height() * if fine { 0.2 } else { 1.0 };
        *db = pos_to_db(pos);
        changed = true;
    }
    if resp.double_clicked() {
        *db = 0.0;
        changed = true;
    }
    let p = ui.painter();
    p.rect_filled(track, 2.0, Color32::from_rgb(0x1c, 0x1c, 0x1c));
    let y = track.bottom() - db_to_pos(*db) * track.height();
    let zero_y = track.bottom() - db_to_pos(0.0) * track.height();
    p.hline(
        track.left() - 2.0..=track.right() + 2.0,
        zero_y,
        Stroke::new(1.0, theme::TEXT_DIM),
    );
    let handle = Rect::from_center_size(pos2(track.center().x, y), vec2(16.0, 6.0));
    p.rect_filled(handle, 1.0, if resp.dragged() { theme::ACCENT } else { theme::TEXT });

    let m = Rect::from_min_max(
        pos2(rect.left() + 16.0, track.top()),
        pos2(rect.right() - 2.0, track.bottom()),
    );
    meter_rect(p, m, meter.0, meter.1);
    resp.on_hover_text(format!("{} dB", format_db(*db)));
    changed
}

/// Stereo peak meter.
pub fn meter_rect(p: &egui::Painter, rect: Rect, l: f32, r: f32) {
    p.rect_filled(rect, 1.0, Color32::from_rgb(0x1c, 0x1c, 0x1c));
    let w = (rect.width() - 1.0) / 2.0;
    for (i, level) in [l, r].into_iter().enumerate() {
        let x0 = rect.left() + i as f32 * (w + 1.0);
        let pos = level_to_pos(level);
        let h = pos * rect.height();
        let bar = Rect::from_min_max(pos2(x0, rect.bottom() - h), pos2(x0 + w, rect.bottom()));
        let color = if level >= 1.0 {
            theme::METER_HIGH
        } else if level > 0.5 {
            theme::METER_MID
        } else {
            theme::METER_LOW
        };
        p.rect_filled(bar, 0.0, color);
    }
}

/// A small square toggle button like Ableton's mute/solo/arm buttons.
pub fn toggle(ui: &mut egui::Ui, on: bool, label: &str, on_color: Color32, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 18.0), Sense::click());
    let p = ui.painter();
    let fill = if on {
        on_color
    } else if resp.hovered() {
        Color32::from_rgb(0x5a, 0x5a, 0x5a)
    } else {
        Color32::from_rgb(0x48, 0x48, 0x48)
    };
    p.rect(
        rect,
        2.0,
        fill,
        Stroke::new(1.0, Color32::from_rgb(0x22, 0x22, 0x22)),
        StrokeKind::Inside,
    );
    let fg = if on { Color32::BLACK } else { theme::TEXT };
    if label == "●" {
        // Not every font has the glyph; draw it.
        p.circle_filled(rect.center(), 4.0, fg);
    } else {
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(11.0),
            fg,
        );
    }
    resp
}

/// Play triangle used on clips and scenes.
pub fn triangle(p: &egui::Painter, center: egui::Pos2, size: f32, color: Color32) {
    let h = size / 2.0;
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(center.x - h * 0.8, center.y - h),
            pos2(center.x + h, center.y),
            pos2(center.x - h * 0.8, center.y + h),
        ],
        color,
        Stroke::NONE,
    ));
}

pub fn stop_square(p: &egui::Painter, center: egui::Pos2, size: f32, color: Color32) {
    p.rect_filled(Rect::from_center_size(center, vec2(size, size)), 1.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_curve_round_trips() {
        for db in [-60.0, -24.0, -12.0, -6.0, 0.0, 6.0] {
            let back = pos_to_db(db_to_pos(db));
            assert!((back - db).abs() < 1e-3, "{db} -> {back}");
        }
        assert_eq!(db_to_pos(-80.0), 0.0);
        assert!((db_to_pos(6.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn knob_log_scale_round_trips() {
        let v = from_norm(to_norm(1000.0, 20.0, 20000.0, Scale::Log), 20.0, 20000.0, Scale::Log);
        assert!((v - 1000.0).abs() < 0.1);
    }
}
