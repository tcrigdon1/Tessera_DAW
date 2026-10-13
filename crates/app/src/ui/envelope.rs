//! Breakpoint envelope editor, used for Arrangement automation lanes and
//! clip envelopes.
//!
//! * Click on empty space to add a breakpoint; drag to place it.
//! * Drag a breakpoint to move it (snapped to the grid; hold Alt to bypass).
//! * Drag a line segment up or down to move it; Alt + drag bends it into a
//!   curve.
//! * Double-click or right-click a breakpoint to delete it.

use eframe::egui::{self, pos2, Color32, Pos2, Rect, Sense, Shape, Stroke};
use tessera_engine::automation::{Envelope, ParamSpec};

use super::theme;

const HIT: f32 = 6.0;

/// Maps beats to the horizontal axis of the editor.
#[derive(Clone, Copy)]
pub struct View {
    pub rect: Rect,
    /// Beat at `rect.left()`.
    pub beat0: f64,
    pub px_per_beat: f32,
    /// Snap grid in beats.
    pub grid: f64,
    /// Points can't be placed before this beat.
    pub min_beat: f64,
    /// Points can't be placed after this beat (clip length).
    pub max_beat: Option<f64>,
    /// Visible area; parts of `rect` outside it are neither drawn nor
    /// interactive.
    pub clip: Rect,
}

impl View {
    pub fn x(&self, beat: f64) -> f32 {
        self.rect.left() + ((beat - self.beat0) as f32) * self.px_per_beat
    }
    pub fn beat(&self, x: f32) -> f64 {
        self.beat0 + ((x - self.rect.left()) / self.px_per_beat) as f64
    }
    fn y(&self, v: f32) -> f32 {
        let r = self.rect.shrink2(egui::vec2(0.0, 3.0));
        r.bottom() - v.clamp(0.0, 1.0) * r.height()
    }
    fn value(&self, y: f32) -> f32 {
        let r = self.rect.shrink2(egui::vec2(0.0, 3.0));
        ((r.bottom() - y) / r.height()).clamp(0.0, 1.0)
    }
    fn clamp_beat(&self, b: f64) -> f64 {
        let b = b.max(self.min_beat);
        match self.max_beat {
            Some(m) => b.min(m),
            None => b,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Drag {
    Point(usize),
    Segment { index: usize, grab: f32, a: f32, b: f32 },
    Curve { index: usize, grab: f32, curve: f32 },
}

/// What happened during this frame.
#[derive(Default)]
pub struct Response {
    /// The envelope changed (while dragging, every frame).
    pub changed: bool,
    /// A gesture finished; rebuild the engine snapshot.
    pub committed: bool,
    pub hovered: bool,
}

fn point_at(env: &Envelope, view: &View, p: Pos2) -> Option<usize> {
    env.points
        .iter()
        .position(|pt| (view.x(pt.beat) - p.x).abs() < HIT && (view.y(pt.value) - p.y).abs() < HIT)
}

/// Index of the segment starting at the last point at or before `beat`.
fn segment_at(env: &Envelope, beat: f64) -> Option<usize> {
    let n = env.points.partition_point(|p| p.beat <= beat);
    if n == 0 || n >= env.points.len() {
        None
    } else {
        Some(n - 1)
    }
}

/// Draws and edits `env`. `inactive` greys it out (overridden automation).
#[allow(clippy::too_many_arguments)]
pub fn edit(
    ui: &mut egui::Ui,
    id: egui::Id,
    env: &mut Envelope,
    view: View,
    spec: Option<ParamSpec>,
    color: Color32,
    inactive: bool,
    playhead: Option<f64>,
) -> Response {
    let mut out = Response::default();
    let rect = view.rect;
    let resp = ui.interact(rect.intersect(view.clip), id, Sense::click_and_drag());
    out.hovered = resp.hovered();
    let (alt, pointer) = ui.input(|i| (i.modifiers.alt, i.pointer.interact_pos()));
    let snap = |b: f64| {
        if alt || view.grid <= 0.0 {
            b
        } else {
            (b / view.grid).round() * view.grid
        }
    };
    let drag_id = id.with("drag");
    let mut drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id));

    // ----- interaction -----
    let hover_point = resp.hover_pos().and_then(|p| point_at(env, &view, p));
    if (resp.double_clicked() || resp.secondary_clicked()) && hover_point.is_some() {
        if let Some(i) = hover_point {
            env.remove(i);
            out.changed = true;
            out.committed = true;
        }
    } else if resp.clicked() && hover_point.is_none() {
        if let Some(p) = resp.interact_pointer_pos() {
            env.insert(view.clamp_beat(snap(view.beat(p.x))), view.value(p.y));
            out.changed = true;
            out.committed = true;
        }
    } else if resp.drag_started() {
        if let Some(p) = resp.interact_pointer_pos() {
            drag = match point_at(env, &view, p) {
                Some(i) => Some(Drag::Point(i)),
                None => {
                    let beat = view.beat(p.x);
                    let on_line = env.value_at(beat).is_some_and(|v| (view.y(v) - p.y).abs() < HIT);
                    match segment_at(env, beat).filter(|_| on_line) {
                        Some(index) if alt => Some(Drag::Curve {
                            index,
                            grab: p.y,
                            curve: env.points[index].curve,
                        }),
                        Some(index) => Some(Drag::Segment {
                            index,
                            grab: p.y,
                            a: env.points[index].value,
                            b: env.points[index + 1].value,
                        }),
                        None => {
                            let i = env.insert(view.clamp_beat(snap(beat)), view.value(p.y));
                            out.changed = true;
                            Some(Drag::Point(i))
                        }
                    }
                }
            };
        }
    }
    if let (Some(d), Some(p)) = (drag, pointer) {
        if resp.dragged() {
            match d {
                Drag::Point(i) => {
                    env.move_point(i, view.clamp_beat(snap(view.beat(p.x))), view.value(p.y));
                }
                Drag::Segment { index, grab, a, b } => {
                    let dv = (grab - p.y) / (rect.height() - 6.0).max(1.0);
                    if let Some(pt) = env.points.get_mut(index) {
                        pt.value = (a + dv).clamp(0.0, 1.0);
                    }
                    if let Some(pt) = env.points.get_mut(index + 1) {
                        pt.value = (b + dv).clamp(0.0, 1.0);
                    }
                }
                Drag::Curve { index, grab, curve } => {
                    let (a, b) = (env.points[index].value, env.points.get(index + 1).map(|p| p.value));
                    // Dragging up bows the line upwards whichever way it goes.
                    let dir = if b.unwrap_or(a) >= a { -1.0 } else { 1.0 };
                    env.points[index].curve = (curve + dir * (grab - p.y) / 60.0).clamp(-1.0, 1.0);
                }
            }
            out.changed = true;
        }
    }
    if resp.drag_stopped() && drag.is_some() {
        drag = None;
        out.committed = true;
    }
    ui.data_mut(|d| match drag {
        Some(v) => {
            d.insert_temp(drag_id, v);
        }
        None => d.remove::<Drag>(drag_id),
    });

    // ----- drawing -----
    let painter = ui.painter_at(rect.intersect(view.clip));
    let line = if inactive {
        Color32::from_rgb(0x80, 0x80, 0x80)
    } else {
        color
    };
    if env.points.is_empty() {
        painter.text(
            rect.left_center() + egui::vec2(8.0, 0.0),
            egui::Align2::LEFT_CENTER,
            "Click to add a breakpoint",
            egui::FontId::proportional(10.0),
            theme::TEXT_DIM,
        );
    } else {
        let mut pts = Vec::new();
        let mut x = rect.left().max(view.x(view.min_beat));
        let right = view
            .max_beat
            .map(|m| view.x(m).min(rect.right()))
            .unwrap_or(rect.right());
        // Sample the curve every couple of pixels, plus exactly at points.
        let mut i = 0;
        while x <= right {
            let b = view.beat(x);
            while i < env.points.len() && env.points[i].beat <= b {
                let pt = env.points[i];
                let px = view.x(pt.beat);
                if px >= rect.left() && px < x {
                    // A vertical step at a point shows as a vertical line.
                    if let Some(v) = env.value_at(pt.beat - 1e-9) {
                        pts.push(pos2(px, view.y(v)));
                    }
                    pts.push(pos2(px, view.y(pt.value)));
                }
                i += 1;
            }
            if let Some(v) = env.value_at(b) {
                pts.push(pos2(x, view.y(v)));
            }
            x += 2.0;
        }
        if pts.len() >= 2 {
            // Fill under the line as one triangle strip (no seams).
            let fill = line.gamma_multiply(0.12);
            let mut mesh = egui::Mesh::default();
            for (i, p) in pts.iter().enumerate() {
                mesh.colored_vertex(*p, fill);
                mesh.colored_vertex(pos2(p.x, rect.bottom()), fill);
                if i > 0 {
                    let b = (i as u32 - 1) * 2;
                    mesh.add_triangle(b, b + 1, b + 2);
                    mesh.add_triangle(b + 1, b + 3, b + 2);
                }
            }
            painter.add(Shape::mesh(mesh));
            painter.add(Shape::line(pts, Stroke::new(1.5, line)));
        }
        for (i, pt) in env.points.iter().enumerate() {
            let c = pos2(view.x(pt.beat), view.y(pt.value));
            let hot = hover_point == Some(i) || matches!(drag, Some(Drag::Point(j)) if j == i);
            painter.circle(
                c,
                if hot { 4.5 } else { 3.0 },
                if hot { theme::SELECT } else { theme::BG_VIEW },
                Stroke::new(1.5, line),
            );
        }
    }
    if let Some(b) = playhead {
        let x = view.x(b);
        if rect.x_range().contains(x) {
            painter.vline(x, rect.y_range(), Stroke::new(1.0, theme::SELECT));
        }
    }

    // Value read-out while hovering or dragging.
    let shown = match drag {
        Some(Drag::Point(i)) => env.points.get(i).map(|p| (p.beat, p.value)),
        _ => hover_point.and_then(|i| env.points.get(i).map(|p| (p.beat, p.value))),
    };
    if let (Some((beat, v)), Some(spec)) = (shown, spec) {
        let text = spec.format(spec.denormalize(v));
        let p = pos2(view.x(beat), view.y(v));
        let anchor = if p.x > rect.right() - 80.0 {
            egui::Align2::RIGHT_BOTTOM
        } else {
            egui::Align2::LEFT_BOTTOM
        };
        painter.text(
            p + egui::vec2(if anchor == egui::Align2::LEFT_BOTTOM { 6.0 } else { -6.0 }, -4.0),
            anchor,
            text,
            egui::FontId::proportional(10.5),
            theme::TEXT,
        );
    }
    if out.hovered {
        if hover_point.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        resp.on_hover_text_at_pointer(
            "Click: add point · Drag point: move · Drag line: move segment · Alt+drag line: curve · Double/right-click point: delete",
        );
    }
    out
}
