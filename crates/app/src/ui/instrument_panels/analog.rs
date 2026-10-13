//! Device view panel of the Analog Synth, plus small drawing helpers
//! (sections with a display, waveform / envelope / filter pictures) shared
//! with the Simple Synth and Electric Piano panels.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke};
use tessera_engine::dsp::analog::{idx, AnalogParams};
use tessera_engine::fx::Params;
use tessera_engine::model::{Id, Instrument};

use super::edit_instrument;
use crate::app::TesseraApp;
use crate::ui::detail::device_box;
use crate::ui::devices::param_control;
use crate::ui::theme;

/// Height of the small displays drawn next to a section's knobs.
pub(super) const DISPLAY_H: f32 = 124.0;
const DISPLAY_BG: Color32 = Color32::from_rgb(0x1c, 0x1c, 0x1c);

/// A titled box: optional display first, then generic controls for
/// `indices` in columns of two. Returns true when a value changed.
pub(super) fn section<P: Params>(
    ui: &mut egui::Ui,
    salt: Id,
    title: &str,
    p: &mut P,
    indices: &[usize],
    display: impl FnOnce(&mut egui::Ui, &P),
) -> bool {
    let mut changed = false;
    let info = P::info();
    device_box(ui, title, |ui| {
        display(ui, p);
        for col in indices.chunks(2) {
            ui.vertical(|ui| {
                for &i in col {
                    let Some(inf) = info.get(i) else { continue };
                    if let Some(v) = param_control(ui, (salt, i), inf, p.get_param(i)) {
                        p.set_param(i, v);
                        changed = true;
                    }
                }
            });
        }
    });
    changed
}

/// No display.
pub(super) fn none<P>(_: &mut egui::Ui, _: &P) {}

fn frame(ui: &mut egui::Ui, width: f32) -> (Rect, egui::Painter) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, DISPLAY_H), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, DISPLAY_BG);
    (rect, painter)
}

fn caption(painter: &egui::Painter, rect: Rect, text: &str) {
    painter.text(
        pos2(rect.left() + 4.0, rect.top() + 3.0),
        egui::Align2::LEFT_TOP,
        text,
        egui::FontId::proportional(9.5),
        theme::TEXT_DIM,
    );
}

/// Plots one cycle (or any 0..1 domain) of `f`, whose output is -1..1.
pub(super) fn plot_fn(ui: &mut egui::Ui, width: f32, label: &str, f: impl Fn(f32) -> f32) {
    let (rect, painter) = frame(ui, width);
    let inner = rect.shrink2(vec2(5.0, 14.0));
    painter.hline(inner.x_range(), inner.center().y, Stroke::new(1.0, theme::GRID));
    let n = 96;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let x = i as f32 / n as f32;
            let y = f(x);
            let y = if y.is_finite() { y.clamp(-1.2, 1.2) } else { 0.0 };
            pos2(
                inner.left() + x * inner.width(),
                inner.center().y - y * inner.height() / 2.0,
            )
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::ACCENT)));
    caption(&painter, rect, label);
}

/// Picture of an ADSR envelope; stage widths follow log time. With
/// `cycle`, attack/decay repeat to show a looping envelope.
pub(super) fn plot_adsr(ui: &mut egui::Ui, width: f32, a: f32, d: f32, s: f32, r: f32, cycle: bool) {
    let (rect, painter) = frame(ui, width);
    let inner = rect.shrink2(vec2(5.0, 14.0));
    let seg = |t: f32| (1.0 + t.max(0.0) * 20.0).ln().max(0.02);
    let y_of = |v: f32| inner.bottom() - v.clamp(0.0, 1.0) * inner.height();
    let mut pts = Vec::new();
    if cycle {
        let period = seg(a) + seg(d);
        let k = inner.width() / (period * 3.0);
        let mut x = inner.left();
        pts.push(pos2(x, y_of(0.0)));
        for _ in 0..3 {
            x += seg(a) * k;
            pts.push(pos2(x, y_of(1.0)));
            let x0 = x;
            for j in 1..=8 {
                let t = j as f32 / 8.0;
                pts.push(pos2(x0 + t * seg(d) * k, y_of((-4.6 * t).exp())));
            }
            x += seg(d) * k;
        }
    } else {
        let hold = seg(0.3);
        let total = seg(a) + seg(d) + hold + seg(r);
        let k = inner.width() / total;
        let x1 = inner.left() + seg(a) * k;
        pts.push(pos2(inner.left(), y_of(0.0)));
        pts.push(pos2(x1, y_of(1.0)));
        for j in 1..=8 {
            let t = j as f32 / 8.0;
            pts.push(pos2(x1 + t * seg(d) * k, y_of(s + (1.0 - s) * (-4.6 * t).exp())));
        }
        let x3 = x1 + (seg(d) + hold) * k;
        pts.push(pos2(x3, y_of(s)));
        for j in 1..=8 {
            let t = j as f32 / 8.0;
            pts.push(pos2(x3 + t * seg(r) * k, y_of(s * (-4.6 * t).exp())));
        }
    }
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::ACCENT)));
    caption(&painter, rect, if cycle { "Cycle" } else { "ADSR" });
}

/// Filter response, `mag(freq_hz) -> linear gain`, on a log frequency axis
/// with a -36..+18 dB range and a marker at `cutoff`.
pub(super) fn plot_response(ui: &mut egui::Ui, width: f32, label: &str, cutoff: f32, mag: impl Fn(f32) -> f32) {
    let (rect, painter) = frame(ui, width);
    let inner = rect.shrink2(vec2(4.0, 14.0));
    let (lo, hi) = (20f32.ln(), 20000f32.ln());
    let (db_lo, db_hi) = (-36.0, 18.0);
    let y_of = |db: f32| inner.bottom() - (db - db_lo) / (db_hi - db_lo) * inner.height();
    painter.hline(inner.x_range(), y_of(0.0), Stroke::new(1.0, theme::GRID));
    let n = 80;
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let f = (lo + t * (hi - lo)).exp();
            let db = 20.0 * mag(f).max(1e-6).log10();
            let db = if db.is_finite() { db.clamp(db_lo, db_hi) } else { db_lo };
            pos2(inner.left() + t * inner.width(), y_of(db))
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::ACCENT)));
    let cx = inner.left() + (cutoff.clamp(20.0, 20000.0).ln() - lo) / (hi - lo) * inner.width();
    painter.vline(cx, inner.y_range(), Stroke::new(1.0, theme::dim(theme::ACCENT, 0.4)));
    caption(&painter, rect, label);
}

/// Magnitude of the analog prototypes of the engine's filters:
/// 0 LP12, 1 LP24 (ladder), 2 BP, 3 HP, 4 Notch. `x` = f / cutoff.
pub(super) fn filter_mag(mode: usize, x: f32, res: f32) -> f32 {
    let res = res.clamp(0.0, 1.0);
    if mode == 1 {
        // Ladder: G^4 / (1 + k G^4), G = 1 / (1 + jx), with input compensation.
        let k = 4.05 * res * 0.97;
        let (mut re, mut im) = (1.0f32, 0.0f32);
        for _ in 0..4 {
            // multiply by 1/(1+jx)
            let d = 1.0 + x * x;
            let (a, b) = (re / d, im / d);
            re = a + b * x;
            im = b - a * x;
        }
        let (dr, di) = (1.0 + k * re, k * im);
        let num = (re * re + im * im).sqrt() * (1.0 + 0.6 * k);
        return num / (dr * dr + di * di).sqrt().max(1e-6);
    }
    let k = 2.0 - 1.95 * res;
    let (dr, di) = (1.0 - x * x, k * x);
    let den = (dr * dr + di * di).sqrt().max(1e-6);
    let num = match mode {
        0 => 1.0,
        2 => k * x,
        3 => x * x,
        _ => (1.0 - x * x).abs(),
    };
    num / den
}

fn lfo_shape(shape: usize, x: f32) -> f32 {
    match shape {
        0 => (x * std::f32::consts::TAU).sin(),
        1 => 1.0 - 4.0 * (x - 0.5).abs(),
        2 => 1.0 - 2.0 * x,
        3 => {
            if x < 0.5 {
                0.9
            } else {
                -0.9
            }
        }
        _ => {
            // Stepped random picture.
            let step = (x * 6.0) as usize;
            [0.3, -0.7, 0.8, -0.2, 0.5, -0.9, 0.1][step.min(6)]
        }
    }
}

/// Picture of an LFO shape: 0 sine, 1 triangle, 2 saw, 3 square, 4 S&H,
/// 5 smooth random.
pub(super) fn plot_lfo(ui: &mut egui::Ui, width: f32, shape: usize) {
    if shape == 5 {
        plot_fn(ui, width, "Wander", |x| {
            let t = x * 6.0;
            let i = (t as usize).min(5);
            let f = t - i as f32;
            let v = [0.3, -0.7, 0.8, -0.2, 0.5, -0.9, 0.1];
            let s = f * f * (3.0 - 2.0 * f);
            v[i] + (v[i + 1] - v[i]) * s
        });
    } else {
        let names = ["Sine", "Triangle", "Saw", "Square", "S&H"];
        plot_fn(ui, width, names[shape.min(4)], |x| lfo_shape(shape, x));
    }
}

fn osc_shape(wave: usize, x: f32, pw: f32) -> f32 {
    match wave {
        0 => 2.0 * x - 1.0,
        1 => {
            if x < pw {
                0.9
            } else {
                -0.9
            }
        }
        2 => 2.0 * (2.0 * x - 1.0).abs() - 1.0,
        _ => (x * std::f32::consts::TAU).sin(),
    }
}

fn choice(v: f32) -> usize {
    v.round().max(0.0) as usize
}

pub fn device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    ui.scope(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        edit_instrument(app, track, |_app, inst| {
            let Instrument::Analog(p) = inst else { return false };
            panel(ui, track, p)
        });
    });
}

fn panel(ui: &mut egui::Ui, track: Id, p: &mut AnalogParams) -> bool {
    let mut changed = false;
    // Oscillators.
    for (n, base) in [(1, idx::O1_WAVE), (2, idx::O2_WAVE)] {
        let wave = choice(p.get(base));
        let mut ids: Vec<usize> = vec![base, base + 1, base + 2, base + 3];
        if wave == 1 {
            ids.push(base + 4);
        }
        ids.push(base + 5);
        if n == 2 {
            ids.push(idx::O2_SYNC);
        }
        let title = if n == 1 { "OSC 1" } else { "OSC 2" };
        changed |= section(ui, track, title, p, &ids, |ui, p| {
            let pw = p.get(base + 4);
            plot_fn(ui, 70.0, "", |x| osc_shape(wave, x, pw));
        });
    }
    changed |= section(ui, track, "NOISE", p, &[idx::NOISE, idx::NOISE + 1], none);
    // Filters.
    changed |= section(
        ui,
        track,
        "FILTER 1",
        p,
        &(idx::F1_TYPE..=idx::F1_ENV).collect::<Vec<_>>(),
        |ui, p| {
            let mode = choice(p.f1_type);
            let (fc, res) = (p.f1_cutoff, p.f1_res);
            let f2 = (p.f2_on >= 0.5).then(|| (choice(p.f2_type), fc * (p.f2_offset / 12.0).exp2(), p.f2_res));
            let parallel = p.routing >= 0.5;
            let names = ["LP12", "LP24", "BP", "HP", "Notch"];
            plot_response(ui, 110.0, names[mode.min(4)], fc, |f| {
                let a = filter_mag(mode, f / fc, res);
                match f2 {
                    None => a,
                    Some((m2, c2, r2)) => {
                        let b = filter_mag(m2, f / c2.max(1.0), r2);
                        if parallel {
                            0.5 * (a + b)
                        } else {
                            a * b
                        }
                    }
                }
            });
        },
    );
    let f2_ids: Vec<usize> = if p.f2_on >= 0.5 {
        (idx::F2_ON..=idx::ROUTING).collect()
    } else {
        vec![idx::F2_ON]
    };
    changed |= section(ui, track, "FILTER 2", p, &f2_ids, none);
    // Envelopes.
    changed |= section(
        ui,
        track,
        "FILTER ENV",
        p,
        &(idx::FE_ATTACK..=idx::FE_VEL).collect::<Vec<_>>(),
        |ui, p| plot_adsr(ui, 90.0, p.fe_attack, p.fe_decay, p.fe_sustain, p.fe_release, false),
    );
    changed |= section(
        ui,
        track,
        "AMP ENV",
        p,
        &(idx::AE_ATTACK..=idx::AE_VEL).collect::<Vec<_>>(),
        |ui, p| plot_adsr(ui, 90.0, p.ae_attack, p.ae_decay, p.ae_sustain, p.ae_release, false),
    );
    // LFOs.
    for (n, base) in [(1, idx::L1_SHAPE), (2, idx::L2_SHAPE)] {
        let title = if n == 1 { "LFO 1" } else { "LFO 2" };
        let ids: Vec<usize> = (base..base + idx::LFO_LEN).collect();
        changed |= section(ui, track, title, p, &ids, |ui, p| {
            plot_lfo(ui, 60.0, choice(p.get(base)));
        });
    }
    // Voicing and output.
    let mut ids = vec![idx::GLIDE_MODE];
    if p.glide_mode >= 0.5 {
        ids.push(idx::GLIDE_MODE + 1);
    }
    ids.extend([idx::VOICE_MODE, idx::UNISON]);
    if p.unison >= 1.5 {
        ids.extend([idx::UNISON + 1, idx::UNISON + 2]);
    }
    ids.extend([idx::VIB_RATE, idx::VIB_RATE + 1, idx::VOLUME]);
    changed |= section(ui, track, "VOICE", p, &ids, none);
    changed
}
