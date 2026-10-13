//! Device panels for the FM Synth and Wavetable instruments.
//!
//! Both follow the Poly Synth panel in `detail.rs`: read a copy of the
//! track's instrument params, edit the copy with knobs and combos, and when
//! anything changed write it back and send it to the engine.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};
use tessera_engine::dsp::fm::{self, FmParams, OpParams, OpWaveform, ALGORITHMS, OPS, OP_NAMES};
use tessera_engine::dsp::wavetable::{
    self, EnvParams, FilterParams, FilterRouting, FilterType, LfoParams, ModDest, ModSource, OscParams,
    WavetableParams, TABLE_NAMES,
};
use tessera_engine::model::{Id, Instrument};

use super::detail::device_box;
use super::theme;
use super::widgets::{self, Scale};
use crate::app::TesseraApp;

// ----- formatting ----------------------------------------------------------------

fn fmt_hz(v: f32) -> String {
    if v >= 1000.0 {
        format!("{:.1} kHz", v / 1000.0)
    } else if v < 10.0 {
        format!("{v:.2} Hz")
    } else {
        format!("{v:.0} Hz")
    }
}

fn fmt_time(v: f32) -> String {
    if v >= 1.0 {
        format!("{v:.2} s")
    } else {
        format!("{:.0} ms", v * 1000.0)
    }
}

fn fmt_pct(v: f32) -> String {
    format!("{:.0}%", v * 100.0)
}

fn fmt_db(v: f32) -> String {
    format!("{v:.1} dB")
}

fn fmt_st(v: f32) -> String {
    format!("{:+.0} st", v.round())
}

fn fmt_ct(v: f32) -> String {
    format!("{v:+.0} ct")
}

fn small(text: &str) -> egui::RichText {
    egui::RichText::new(text).small()
}

/// A knob for an integer value stored as `f32`.
#[allow(clippy::too_many_arguments)]
fn int_knob(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    min: f32,
    max: f32,
    default: f32,
    format: impl Fn(f32) -> String,
) {
    widgets::knob(ui, label, value, min, max, default, Scale::Linear, format);
    *value = value.round();
}

/// Keeps a knob row aligned when a knob is hidden.
fn knob_gap(ui: &mut egui::Ui) {
    ui.allocate_exact_size(vec2(52.0, 62.0), Sense::hover());
}

/// A row of tab buttons whose selection lives in egui temp memory under `id`.
fn tabs(ui: &mut egui::Ui, id: egui::Id, labels: &[&str], width: f32) -> usize {
    let mut sel = ui.data(|d| d.get_temp::<usize>(id)).unwrap_or(0).min(labels.len() - 1);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, l) in labels.iter().enumerate() {
            if widgets::toggle(ui, sel == i, l, theme::ACCENT, width).clicked() {
                sel = i;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(id, sel));
    sel
}

/// Bipolar -1..1 amount bar: drag horizontally (Shift for fine), double-click
/// to reset.
fn amount_bar(ui: &mut egui::Ui, value: &mut f32, width: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 16.0), Sense::click_and_drag());
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let d = resp.drag_delta().x / rect.width() * 2.0 * if fine { 0.2 } else { 1.0 };
        *value = (*value + d).clamp(-1.0, 1.0);
    }
    if resp.double_clicked() {
        *value = 0.0;
    }
    let p = ui.painter();
    p.rect_filled(rect, 2.0, theme::gray(0x1c));
    let cx = rect.center().x;
    let x = cx + *value * rect.width() / 2.0;
    if value.abs() > 0.001 {
        let bar = Rect::from_min_max(pos2(cx.min(x), rect.top() + 2.0), pos2(cx.max(x), rect.bottom() - 2.0));
        p.rect_filled(bar, 1.0, theme::dim(theme::ACCENT, 0.5));
    }
    p.vline(cx, rect.y_range(), Stroke::new(1.0, theme::grid_bar()));
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        format!("{:+.0}%", *value * 100.0),
        egui::FontId::proportional(9.5),
        if resp.hovered() || resp.dragged() || value.abs() > 0.001 {
            theme::text()
        } else {
            theme::text_dim()
        },
    );
    resp.on_hover_text("Amount: drag left/right, Shift for fine, double-click to reset");
}

fn adsr_knobs(ui: &mut egui::Ui, a: &mut f32, d: &mut f32, s: &mut f32, r: &mut f32, defaults: [f32; 4]) {
    widgets::knob(ui, "Attack", a, 0.001, 10.0, defaults[0], Scale::Log, fmt_time);
    widgets::knob(ui, "Decay", d, 0.005, 20.0, defaults[1], Scale::Log, fmt_time);
    widgets::knob(ui, "Sustain", s, 0.0, 1.0, defaults[2], Scale::Linear, fmt_pct);
    widgets::knob(ui, "Release", r, 0.005, 20.0, defaults[3], Scale::Log, fmt_time);
}

// ----- FM Synth ------------------------------------------------------------------

pub fn fm_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    // Boxes scrolled past the right edge get no available width; keep their
    // labels on one line instead of letting them wrap and grow the panel.
    ui.scope(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        fm_device_inner(app, ui, track);
    });
}

fn fm_device_inner(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let Some(Instrument::Fm(mut p)) = app.project.track(track).map(|t| t.instrument) else {
        return;
    };
    let before = p;
    let op_id = egui::Id::new(("fm_op_tab", track));
    let mut sel_op = ui.data(|d| d.get_temp::<usize>(op_id)).unwrap_or(0).min(OPS - 1);

    device_box(ui, "ALGORITHM", |ui| {
        ui.vertical(|ui| {
            let presets = FmParams::presets();
            let current = presets
                .iter()
                .find(|(_, pp)| *pp == p)
                .map(|(n, _)| *n)
                .unwrap_or("Custom");
            ui.horizontal(|ui| {
                ui.label(small("Preset").color(theme::text_dim()));
                egui::ComboBox::from_id_salt(("fm_preset", track))
                    .width(110.0)
                    .selected_text(current)
                    .show_ui(ui, |ui| {
                        for (name, pp) in presets {
                            if ui.selectable_label(pp == p, name).clicked() {
                                p = pp;
                            }
                        }
                    });
            });
            let alg = p.algorithm as usize % ALGORITHMS;
            egui::ComboBox::from_id_salt(("fm_alg", track))
                .width(150.0)
                .selected_text(format!("{} {}", alg + 1, fm::algorithm_routing(alg).name))
                .show_ui(ui, |ui| {
                    for a in 0..ALGORITHMS {
                        let label = format!("{} {}", a + 1, fm::algorithm_routing(a).name);
                        if ui.selectable_label(a == alg, label).clicked() {
                            p.algorithm = a as u8;
                        }
                    }
                });
            ui.horizontal(|ui| {
                if let Some(op) = algorithm_diagram(ui, &p) {
                    sel_op = op;
                }
                ui.vertical(|ui| {
                    ui.add_space(14.0);
                    widgets::knob(ui, "Feedback", &mut p.feedback, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
                });
            });
        });
    });

    device_box(ui, "OPERATOR", |ui| {
        let routing = fm::algorithm_routing(p.algorithm as usize);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            for (i, op) in p.ops.iter().enumerate() {
                if op_tab(ui, i, op, i == sel_op, routing.carriers[i]).clicked() {
                    sel_op = i;
                }
            }
        });
        ui.add_space(4.0);
        let op = &mut p.ops[sel_op];
        operator_controls(ui, op, track, sel_op);
    });
    ui.data_mut(|d| d.insert_temp(op_id, sel_op));

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        device_box(ui, "LFO", |ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 4.0;
                ui.add_space(4.0);
                egui::ComboBox::from_id_salt(("fm_lfo_shape", track))
                    .width(76.0)
                    .selected_text(p.lfo_shape.label())
                    .show_ui(ui, |ui| {
                        for s in fm::LfoShape::ALL {
                            ui.selectable_value(&mut p.lfo_shape, s, s.label());
                        }
                    });
                egui::ComboBox::from_id_salt(("fm_lfo_dest", track))
                    .width(76.0)
                    .selected_text(p.lfo_dest.label())
                    .show_ui(ui, |ui| {
                        for d in fm::LfoDestination::ALL {
                            ui.selectable_value(&mut p.lfo_dest, d, d.label());
                        }
                    });
            });
            widgets::knob(ui, "Rate", &mut p.lfo_rate, 0.05, 40.0, 5.0, Scale::Log, fmt_hz);
            widgets::knob(ui, "Amount", &mut p.lfo_amount, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
        });
        device_box(ui, "PITCH ENV", |ui| {
            widgets::knob(
                ui,
                "Amount",
                &mut p.pitch_env_amount,
                -48.0,
                48.0,
                0.0,
                Scale::Linear,
                |v| format!("{v:+.1} st"),
            );
            widgets::knob(
                ui,
                "Decay",
                &mut p.pitch_env_decay,
                0.005,
                5.0,
                0.1,
                Scale::Log,
                fmt_time,
            );
        });
    });

    device_box(ui, "FILTER", |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                if widgets::toggle(ui, p.filter_on, "On", theme::ACCENT, 30.0).clicked() {
                    p.filter_on = !p.filter_on;
                }
                egui::ComboBox::from_id_salt(("fm_filter_mode", track))
                    .width(80.0)
                    .selected_text(p.filter_mode.label())
                    .show_ui(ui, |ui| {
                        for m in fm::FilterMode::ALL {
                            ui.selectable_value(&mut p.filter_mode, m, m.label());
                        }
                    });
            });
            ui.horizontal(|ui| {
                widgets::knob(ui, "Cutoff", &mut p.cutoff, 30.0, 18000.0, 8000.0, Scale::Log, fmt_hz);
                widgets::knob(ui, "Res", &mut p.resonance, 0.0, 2.0, 0.3, Scale::Linear, |v| {
                    format!("{v:.2}")
                });
                widgets::knob(ui, "Env Amt", &mut p.filter_env, -4.0, 6.0, 0.0, Scale::Linear, |v| {
                    format!("{v:+.1} oct")
                });
            });
        });
        ui.vertical(|ui| {
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                adsr_knobs(
                    ui,
                    &mut p.f_attack,
                    &mut p.f_decay,
                    &mut p.f_sustain,
                    &mut p.f_release,
                    [0.002, 0.4, 0.5, 0.3],
                );
            });
        });
    });

    device_box(ui, "GLOBAL", |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                widgets::knob(ui, "Glide", &mut p.glide, 0.0, 2.0, 0.0, Scale::Linear, |v| {
                    if v <= 0.0005 {
                        "Off".into()
                    } else {
                        fmt_time(v)
                    }
                });
                widgets::knob(ui, "Spread", &mut p.spread, 0.0, 50.0, 0.0, Scale::Linear, |v| {
                    format!("{v:.0} ct")
                });
            });
            ui.horizontal(|ui| {
                int_knob(ui, "Transpose", &mut p.transpose, -48.0, 48.0, 0.0, fmt_st);
                widgets::knob(ui, "Volume", &mut p.volume_db, -40.0, 6.0, -12.0, Scale::Linear, fmt_db);
            });
        });
    });

    if p != before {
        if let Some(t) = app.project.track_mut(track) {
            t.instrument = Instrument::Fm(p);
        }
        app.send_instrument(track);
    }
}

/// One operator tab: letter, carrier/modulator tag and a level bar.
fn op_tab(ui: &mut egui::Ui, i: usize, op: &OpParams, selected: bool, carrier: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(34.0, 29.0), Sense::click());
    let p = ui.painter();
    let fill = if selected {
        theme::gray(0x5a)
    } else if resp.hovered() {
        theme::gray(0x48)
    } else {
        theme::gray(0x3a)
    };
    p.rect(
        rect,
        2.0,
        fill,
        Stroke::new(1.0, if selected { theme::ACCENT } else { theme::gray(0x22) }),
        StrokeKind::Inside,
    );
    let fg = if !op.on {
        theme::gray(0x66)
    } else if carrier {
        theme::ACCENT
    } else {
        theme::text()
    };
    p.text(
        pos2(rect.center().x, rect.top() + 11.0),
        egui::Align2::CENTER_CENTER,
        OP_NAMES[i],
        egui::FontId::proportional(13.0),
        fg,
    );
    let bar = Rect::from_min_max(
        pos2(rect.left() + 4.0, rect.bottom() - 7.0),
        pos2(rect.right() - 4.0, rect.bottom() - 4.0),
    );
    p.rect_filled(bar, 1.0, theme::gray(0x1c));
    let lvl = if op.on { op.level.clamp(0.0, 1.0) } else { 0.0 };
    if lvl > 0.0 {
        let mut b = bar;
        b.set_right(bar.left() + bar.width() * lvl);
        p.rect_filled(b, 1.0, theme::dim(fg, 0.9));
    }
    let role = if carrier { "carrier" } else { "modulator" };
    let state = if op.on { "" } else { ", off" };
    resp.on_hover_text(format!(
        "Operator {} ({role}{state}), level {:.0}%",
        OP_NAMES[i],
        op.level * 100.0
    ))
}

fn operator_controls(ui: &mut egui::Ui, op: &mut OpParams, track: Id, n: usize) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 5.0;
        ui.add_space(2.0);
        if widgets::toggle(ui, op.on, if op.on { "On" } else { "Off" }, theme::ACCENT, 66.0).clicked() {
            op.on = !op.on;
        }
        egui::ComboBox::from_id_salt(("fm_wave", track, n))
            .width(60.0)
            .selected_text(small(op.wave.label()))
            .show_ui(ui, |ui| {
                for w in OpWaveform::ALL {
                    ui.selectable_value(&mut op.wave, w, w.label());
                }
            });
        if widgets::toggle(ui, op.fixed, "Fixed", theme::SOLO, 66.0)
            .on_hover_text("Ignore the played note and run at a fixed frequency")
            .clicked()
        {
            op.fixed = !op.fixed;
        }
        let ratio = if op.fixed {
            fmt_hz(op.fixed_hz)
        } else {
            format!("Ratio {:.3}", op.ratio())
        };
        ui.label(small(&ratio).color(theme::text_dim()));
    });
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            if op.fixed {
                widgets::knob(ui, "Freq", &mut op.fixed_hz, 1.0, 18000.0, 440.0, Scale::Log, fmt_hz);
                knob_gap(ui);
            } else {
                let mut coarse = op.coarse as f32;
                int_knob(ui, "Coarse", &mut coarse, 0.0, 32.0, 1.0, |v| {
                    if v < 0.5 {
                        "0.5".into()
                    } else {
                        format!("{:.0}", v.round())
                    }
                });
                op.coarse = coarse.clamp(0.0, 32.0) as u8;
                let mut fine = op.fine as f32;
                int_knob(ui, "Fine", &mut fine, 0.0, 1000.0, 0.0, |v| format!("{v:.0}"));
                op.fine = fine.clamp(0.0, 1000.0) as u16;
            }
            widgets::knob(ui, "Level", &mut op.level, 0.0, 1.0, 1.0, Scale::Linear, fmt_pct);
            widgets::knob(ui, "Vel", &mut op.vel_sens, 0.0, 1.0, 0.5, Scale::Linear, fmt_pct);
        });
        ui.horizontal(|ui| {
            adsr_knobs(
                ui,
                &mut op.attack,
                &mut op.decay,
                &mut op.sustain,
                &mut op.release,
                [0.002, 0.5, 0.8, 0.3],
            );
        });
    });
    ui.vertical(|ui| {
        widgets::knob(
            ui,
            "Key Scale",
            &mut op.key_scale,
            0.0,
            1.0,
            0.0,
            Scale::Linear,
            fmt_pct,
        );
    });
}

/// Depth of each operator above the output: carriers 0, their modulators 1...
fn op_depths(r: &fm::AlgorithmRouting) -> [usize; OPS] {
    let mut depth = [0usize; OPS];
    // Modulators always have a higher index than their targets.
    for src in 0..OPS {
        let targets: Vec<usize> = (0..OPS).filter(|&dst| r.modulators[dst][src]).collect();
        if !targets.is_empty() && !r.carriers[src] {
            depth[src] = 1 + targets.iter().map(|&t| depth[t]).max().unwrap_or(0);
        }
    }
    depth
}

/// Box centres for the routing diagram, inside `rect`.
fn diagram_layout(r: &fm::AlgorithmRouting, rect: Rect) -> [egui::Pos2; OPS] {
    let depth = op_depths(r);
    let gap = 26.0;
    let mut x = [rect.center().x; OPS];
    let max_depth = depth.iter().copied().max().unwrap_or(0);
    let pitch = if max_depth == 0 {
        0.0
    } else {
        (rect.height() - 34.0) / max_depth as f32
    }
    .min(26.0);
    let base = (rect.center().y + 6.0 + max_depth as f32 * pitch / 2.0).min(rect.bottom() - 19.0);
    let y_of = |d: usize| base - d as f32 * pitch;
    for level in 0..=max_depth {
        let mut ops: Vec<usize> = (0..OPS).filter(|&o| depth[o] == level).collect();
        if ops.is_empty() {
            continue;
        }
        // Where each operator wants to be: carriers spread evenly, modulators
        // above the middle of their targets.
        let desired: Vec<f32> = ops
            .iter()
            .enumerate()
            .map(|(k, &o)| {
                let targets: Vec<usize> = (0..OPS)
                    .filter(|&dst| r.modulators[dst][o] && depth[dst] < level)
                    .collect();
                if level == 0 || targets.is_empty() {
                    let n = ops.len() as f32;
                    rect.center().x + (k as f32 - (n - 1.0) / 2.0) * (gap + 4.0)
                } else {
                    targets.iter().map(|&t| x[t]).sum::<f32>() / targets.len() as f32
                }
            })
            .collect();
        let mut order: Vec<usize> = (0..ops.len()).collect();
        order.sort_by(|&a, &b| desired[a].total_cmp(&desired[b]).then(ops[a].cmp(&ops[b])));
        ops = order.iter().map(|&k| ops[k]).collect();
        let want: Vec<f32> = order.iter().map(|&k| desired[k]).collect();
        let mut placed = want.clone();
        for i in 1..placed.len() {
            placed[i] = placed[i].max(placed[i - 1] + gap);
        }
        let shift = (want.iter().sum::<f32>() - placed.iter().sum::<f32>()) / placed.len() as f32;
        for (k, &o) in ops.iter().enumerate() {
            x[o] = (placed[k] + shift).clamp(rect.left() + 12.0, rect.right() - 12.0);
        }
    }
    std::array::from_fn(|o| pos2(x[o], y_of(depth[o])))
}

/// Paints the algorithm's routing. Clicking an operator returns it.
fn algorithm_diagram(ui: &mut egui::Ui, p: &FmParams) -> Option<usize> {
    let (rect, resp) = ui.allocate_exact_size(vec2(110.0, 90.0), Sense::click());
    let r = fm::algorithm_routing(p.algorithm as usize);
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::gray(0x1c));
    let pos = diagram_layout(&r, rect);
    let size = vec2(20.0, 15.0);
    let line = Stroke::new(1.2, theme::text_dim());

    // Output bus under the carriers.
    let carrier_y = (0..OPS)
        .find(|&o| r.carriers[o])
        .map_or(rect.bottom() - 19.0, |o| pos[o].y);
    let bus_y = carrier_y + size.y / 2.0 + 6.0;
    let carriers: Vec<f32> = (0..OPS).filter(|&o| r.carriers[o]).map(|o| pos[o].x).collect();
    if let (Some(lo), Some(hi)) = (
        carriers.iter().copied().reduce(f32::min),
        carriers.iter().copied().reduce(f32::max),
    ) {
        let acc = Stroke::new(1.5, theme::dim(theme::ACCENT, 0.8));
        painter.hline(lo..=hi, bus_y, acc);
        for &cx in &carriers {
            painter.vline(cx, carrier_y + size.y / 2.0..=bus_y, acc);
        }
    }
    // Modulation lines.
    for dst in 0..OPS {
        for src in 0..OPS {
            if r.modulators[dst][src] {
                let a = pos2(pos[src].x, pos[src].y + size.y / 2.0);
                let b = pos2(pos[dst].x, pos[dst].y - size.y / 2.0);
                painter.line_segment([a, b], line);
            }
        }
    }
    // Feedback loop on D.
    let d = pos[3];
    let fb_col = if p.feedback > 0.001 {
        theme::ACCENT
    } else {
        theme::text_dim()
    };
    let fb = Stroke::new(1.2, fb_col);
    let rx = d.x + size.x / 2.0;
    let loop_pts = vec![
        pos2(rx, d.y),
        pos2(rx + 5.0, d.y),
        pos2(rx + 5.0, d.y - size.y / 2.0 - 4.0),
        pos2(d.x, d.y - size.y / 2.0 - 4.0),
        pos2(d.x, d.y - size.y / 2.0),
    ];
    painter.add(egui::Shape::line(loop_pts, fb));

    let mut clicked = None;
    let hover = resp.hover_pos();
    for o in 0..OPS {
        let b = Rect::from_center_size(pos[o], size);
        let on = p.ops[o].on;
        let carrier = r.carriers[o];
        let (fill, stroke, text) = if carrier {
            (theme::dim(theme::ACCENT, 0.55), theme::ACCENT, Color32::BLACK)
        } else {
            (theme::gray(0x4a), theme::text_dim(), theme::text())
        };
        let (fill, text) = if on {
            (fill, text)
        } else {
            (theme::dim(fill, 0.5), theme::gray(0x77))
        };
        let hovered = hover.is_some_and(|h| b.expand(2.0).contains(h));
        painter.rect(
            b,
            2.0,
            fill,
            Stroke::new(1.0, if hovered { theme::select() } else { stroke }),
            StrokeKind::Inside,
        );
        painter.text(
            b.center(),
            egui::Align2::CENTER_CENTER,
            OP_NAMES[o],
            egui::FontId::proportional(10.5),
            text,
        );
        if resp.clicked() && hovered {
            clicked = Some(o);
        }
    }
    resp.on_hover_text(format!(
        "{}: carriers are orange and heard at the output. Lines run from a modulator down to the operator it modulates. Click an operator to edit it.",
        r.name
    ));
    clicked
}

// ----- Wavetable -----------------------------------------------------------------

pub fn wavetable_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    // Boxes scrolled past the right edge get no available width; keep their
    // labels on one line instead of letting them wrap and grow the panel.
    ui.scope(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        wavetable_device_inner(app, ui, track);
    });
}

fn wavetable_device_inner(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let Some(Instrument::Wavetable(mut p)) = app.project.track(track).map(|t| t.instrument) else {
        return;
    };
    let before = p;

    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        device_box(ui, "PRESET", |ui| {
            let current = (0..WavetableParams::PRESET_NAMES.len())
                .find(|&i| WavetableParams::preset(i) == p)
                .map(|i| WavetableParams::PRESET_NAMES[i])
                .unwrap_or("Custom");
            egui::ComboBox::from_id_salt(("wt_preset", track))
                .width(100.0)
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (i, name) in WavetableParams::PRESET_NAMES.iter().enumerate() {
                        if ui.selectable_label(*name == current, *name).clicked() {
                            p = WavetableParams::preset(i);
                        }
                    }
                });
        });
        device_box(ui, "SUB / NOISE", |ui| {
            widgets::knob(ui, "Sub", &mut p.sub_level, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
            widgets::knob(ui, "Noise", &mut p.noise_level, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
        });
    });

    osc_box(ui, "OSC 1", &mut p.osc1, track, 0);
    osc_box(ui, "OSC 2", &mut p.osc2, track, 1);

    filter_box(ui, "FILTER 1", &mut p.filter1, Some(&mut p.routing), track, 0);
    filter_box(ui, "FILTER 2", &mut p.filter2, None, track, 1);

    device_box(ui, "ENVELOPES", |ui| {
        ui.vertical(|ui| {
            let t = tabs(
                ui,
                egui::Id::new(("wt_env_tab", track)),
                &["Amp", "Env 2", "Env 3"],
                50.0,
            );
            let (env, defaults): (&mut EnvParams, [f32; 4]) = match t {
                0 => (&mut p.amp_env, [0.005, 0.4, 0.8, 0.3]),
                1 => (&mut p.env2, [0.002, 0.5, 0.2, 0.4]),
                _ => (&mut p.env3, [0.5, 1.0, 0.5, 1.0]),
            };
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                adsr_knobs(
                    ui,
                    &mut env.attack,
                    &mut env.decay,
                    &mut env.sustain,
                    &mut env.release,
                    defaults,
                );
            });
            envelope_shape(ui, env, 214.0);
        });
    });

    device_box(ui, "LFO", |ui| {
        ui.vertical(|ui| {
            lfo_row(ui, "1", &mut p.lfo1, track);
            lfo_row(ui, "2", &mut p.lfo2, track);
        });
    });

    device_box(ui, "MOD MATRIX", |ui| {
        mod_matrix(ui, &mut p, track);
    });

    device_box(ui, "GLOBAL", |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                widgets::knob(ui, "Macro 1", &mut p.macro1, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
                widgets::knob(ui, "Glide", &mut p.glide, 0.0, 2.0, 0.0, Scale::Linear, |v| {
                    if v <= 0.0005 {
                        "Off".into()
                    } else {
                        fmt_time(v)
                    }
                });
            });
            ui.horizontal(|ui| {
                widgets::knob(
                    ui,
                    "Vel Sens",
                    &mut p.velocity_sens,
                    0.0,
                    1.0,
                    0.6,
                    Scale::Linear,
                    fmt_pct,
                );
                widgets::knob(ui, "Volume", &mut p.volume_db, -40.0, 6.0, -12.0, Scale::Linear, fmt_db);
            });
        });
    });

    if p != before {
        if let Some(t) = app.project.track_mut(track) {
            t.instrument = Instrument::Wavetable(p);
        }
        app.send_instrument(track);
    }
}

fn osc_box(ui: &mut egui::Ui, title: &str, o: &mut OscParams, track: Id, n: usize) {
    device_box(ui, title, |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if widgets::toggle(ui, o.enabled, "On", theme::ACCENT, 28.0).clicked() {
                    o.enabled = !o.enabled;
                }
                let table = o.table.min(TABLE_NAMES.len() - 1);
                egui::ComboBox::from_id_salt(("wt_table", track, n))
                    .width(96.0)
                    .selected_text(small(TABLE_NAMES[table]))
                    .show_ui(ui, |ui| {
                        for (i, name) in TABLE_NAMES.iter().enumerate() {
                            ui.selectable_value(&mut o.table, i, *name);
                        }
                    });
            });
            ui.add_space(2.0);
            wave_display(ui, o, vec2(128.0, 100.0));
        });
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                widgets::knob(ui, "Position", &mut o.position, 0.0, 1.0, 0.6, Scale::Linear, fmt_pct);
                int_knob(ui, "Semi", &mut o.semitones, -48.0, 48.0, 0.0, fmt_st);
                widgets::knob(ui, "Cents", &mut o.cents, -100.0, 100.0, 0.0, Scale::Linear, fmt_ct);
                widgets::knob(ui, "Level", &mut o.level, 0.0, 1.0, 0.8, Scale::Linear, fmt_pct);
            });
            ui.horizontal(|ui| {
                widgets::knob(ui, "Pan", &mut o.pan, -1.0, 1.0, 0.0, Scale::Linear, |v| {
                    if v.abs() < 0.01 {
                        "C".into()
                    } else if v < 0.0 {
                        format!("{:.0}L", -v * 50.0)
                    } else {
                        format!("{:.0}R", v * 50.0)
                    }
                });
                let mut uni = o.unison as f32;
                int_knob(ui, "Unison", &mut uni, 1.0, 8.0, 1.0, |v| format!("{v:.0}"));
                o.unison = uni.clamp(1.0, 8.0) as u8;
                widgets::knob(
                    ui,
                    "Detune",
                    &mut o.unison_detune,
                    0.0,
                    1.0,
                    0.25,
                    Scale::Linear,
                    fmt_pct,
                );
                widgets::knob(
                    ui,
                    "Spread",
                    &mut o.unison_spread,
                    0.0,
                    1.0,
                    0.5,
                    Scale::Linear,
                    fmt_pct,
                );
            });
        });
    });
}

/// The current wavetable frame. Drag horizontally to morph the position.
fn wave_display(ui: &mut egui::Ui, o: &mut OscParams, size: egui::Vec2) {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
    if resp.dragged() {
        let fine = ui.input(|i| i.modifiers.shift);
        let d = resp.drag_delta().x / rect.width() * if fine { 0.2 } else { 1.0 };
        o.position = (o.position + d).clamp(0.0, 1.0);
    }
    if resp.double_clicked() {
        o.position = 0.0;
    }
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::gray(0x1c));
    painter.hline(rect.x_range(), rect.center().y, Stroke::new(1.0, theme::grid()));
    let samples = wavetable::frame_samples(o.table, o.position);
    let peak = samples.iter().fold(0.0f32, |a, s| a.max(s.abs())).max(1e-6);
    let h = rect.height() / 2.0 - 6.0;
    let w = rect.width() - 8.0;
    let n = samples.len().max(2);
    let pts: Vec<egui::Pos2> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| {
            pos2(
                rect.left() + 4.0 + i as f32 / (n - 1) as f32 * w,
                rect.center().y - s / peak * h,
            )
        })
        .collect();
    let color = if o.enabled { theme::ACCENT } else { theme::text_dim() };
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, color)));
    // Position strip along the bottom.
    let strip_y = rect.bottom() - 2.0;
    painter.hline(
        rect.left() + 4.0..=rect.left() + 4.0 + w * o.position,
        strip_y,
        Stroke::new(2.0, theme::dim(color, 0.7)),
    );
    painter.text(
        pos2(rect.right() - 4.0, rect.top() + 3.0),
        egui::Align2::RIGHT_TOP,
        format!("{:.0}%", o.position * 100.0),
        egui::FontId::proportional(9.5),
        theme::text_dim(),
    );
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    resp.on_hover_text("Drag left/right to morph through the table (Shift for fine)");
}

fn filter_box(
    ui: &mut egui::Ui,
    title: &str,
    f: &mut FilterParams,
    routing: Option<&mut FilterRouting>,
    track: Id,
    n: usize,
) {
    device_box(ui, title, |ui| {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if widgets::toggle(ui, f.enabled, "On", theme::ACCENT, 28.0).clicked() {
                    f.enabled = !f.enabled;
                }
                egui::ComboBox::from_id_salt(("wt_filter", track, n))
                    .width(80.0)
                    .selected_text(small(f.kind.label()))
                    .show_ui(ui, |ui| {
                        for k in FilterType::ALL {
                            ui.selectable_value(&mut f.kind, k, k.label());
                        }
                    });
            });
            ui.horizontal(|ui| {
                widgets::knob(ui, "Cutoff", &mut f.cutoff, 20.0, 20000.0, 4000.0, Scale::Log, fmt_hz);
                widgets::knob(ui, "Res", &mut f.resonance, 0.0, 1.0, 0.2, Scale::Linear, fmt_pct);
                widgets::knob(ui, "Drive", &mut f.drive, 0.0, 1.0, 0.0, Scale::Linear, fmt_pct);
            });
            if let Some(routing) = routing {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(small("Routing").color(theme::text_dim()));
                    egui::ComboBox::from_id_salt(("wt_routing", track))
                        .width(72.0)
                        .selected_text(small(routing.label()))
                        .show_ui(ui, |ui| {
                            for r in FilterRouting::ALL {
                                ui.selectable_value(routing, r, r.label());
                            }
                        })
                        .response
                        .on_hover_text(
                            "Serial: Filter 1 into Filter 2. Parallel: both filters side by side. \
                             Split: Osc 1 into Filter 1, Osc 2 into Filter 2.",
                        );
                });
            }
        });
    });
}

/// Small read-only picture of an ADSR envelope.
fn envelope_shape(ui: &mut egui::Ui, e: &EnvParams, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme::gray(0x1c));
    // Each stage gets a width by log time so short and long settings both read.
    let seg = |t: f32| (1.0 + t.max(0.0) * 20.0).ln();
    let hold = seg(0.3);
    let total = seg(e.attack) + seg(e.decay) + hold + seg(e.release);
    let inner = rect.shrink2(vec2(4.0, 4.0));
    let k = inner.width() / total.max(1e-6);
    let y_of = |v: f32| inner.bottom() - v.clamp(0.0, 1.0) * inner.height();
    let x0 = inner.left();
    let x1 = x0 + seg(e.attack) * k;
    let x2 = x1 + seg(e.decay) * k;
    let x3 = x2 + hold * k;
    let x4 = x3 + seg(e.release) * k;
    let pts = vec![
        pos2(x0, y_of(0.0)),
        pos2(x1, y_of(1.0)),
        pos2(x2, y_of(e.sustain)),
        pos2(x3, y_of(e.sustain)),
        pos2(x4, y_of(0.0)),
    ];
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::ACCENT)));
}

fn lfo_row(ui: &mut egui::Ui, n: &str, l: &mut LfoParams, track: Id) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(small(&format!("LFO {n}")).color(theme::text_dim()));
            });
            egui::ComboBox::from_id_salt(("wt_lfo_shape", track, n))
                .width(64.0)
                .selected_text(small(l.shape.label()))
                .show_ui(ui, |ui| {
                    for s in wavetable::LfoShape::ALL {
                        ui.selectable_value(&mut l.shape, s, s.label());
                    }
                });
            if widgets::toggle(ui, l.retrigger, "Retrig", theme::SOLO, 72.0)
                .on_hover_text("Restart the LFO on every note")
                .clicked()
            {
                l.retrigger = !l.retrigger;
            }
        });
        widgets::knob(ui, "Rate", &mut l.rate, 0.01, 40.0, 1.0, Scale::Log, fmt_hz);
    });
}

fn mod_matrix(ui: &mut egui::Ui, p: &mut WavetableParams, track: Id) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = vec2(3.0, 2.0);
        ui.spacing_mut().interact_size.y = 16.0;
        ui.spacing_mut().button_padding = vec2(4.0, 0.0);
        ui.horizontal(|ui| {
            for (t, w) in [("Source", 76.0), ("Destination", 98.0), ("Amount", 70.0)] {
                let (r, _) = ui.allocate_exact_size(vec2(w, 12.0), Sense::hover());
                ui.painter().text(
                    r.left_center(),
                    egui::Align2::LEFT_CENTER,
                    t,
                    egui::FontId::proportional(9.5),
                    theme::text_dim(),
                );
            }
        });
        for (i, slot) in p.mod_matrix.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let active = slot.source != ModSource::None && slot.dest != ModDest::None;
                let col = if active { theme::text() } else { theme::text_dim() };
                egui::ComboBox::from_id_salt(("wt_mod_src", track, i))
                    .width(70.0)
                    .selected_text(small(slot.source.label()).color(col))
                    .show_ui(ui, |ui| {
                        for s in ModSource::ALL {
                            ui.selectable_value(&mut slot.source, s, s.label());
                        }
                    });
                egui::ComboBox::from_id_salt(("wt_mod_dst", track, i))
                    .width(92.0)
                    .selected_text(small(slot.dest.label()).color(col))
                    .show_ui(ui, |ui| {
                        for d in ModDest::ALL {
                            ui.selectable_value(&mut slot.dest, d, d.label());
                        }
                    });
                amount_bar(ui, &mut slot.amount, 70.0);
            });
        }
    });
}
