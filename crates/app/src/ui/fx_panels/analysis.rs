//! Custom panels for the analysis devices (Spectrum, Tuner) and Hybrid
//! Reverb. The analyzers read their device's audio tap
//! (`tessera_engine::fx::scope`) and do all the analysis here on the UI
//! thread; per-device display state lives in egui temp memory.

use std::collections::VecDeque;
use std::sync::Arc;

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke};
use tessera_engine::fx::fft::{hann, Complex, RealFft};
use tessera_engine::fx::hybrid_reverb::{self, HybridReverbParams, Routing};
use tessera_engine::fx::spectrum::SpectrumParams;
use tessera_engine::fx::tuner::{self, PitchDetector, TunerParams};
use tessera_engine::fx::{scope, DeviceParams, EffectParams};

use super::super::theme;
use super::{knob_columns, PanelCtx};

pub fn body(ui: &mut egui::Ui, ctx: &PanelCtx, params: &DeviceParams, out: &mut Vec<(usize, f32)>) -> bool {
    match params {
        DeviceParams::Spectrum(p) => {
            ui.horizontal_top(|ui| {
                spectrum_display(ui, ctx, p);
                knob_columns(ui, ctx, params, &relevant(p, &[0, 1, 2, 3, 4, 5, 6]), out);
            });
            true
        }
        DeviceParams::Tuner(p) => {
            ui.horizontal_top(|ui| {
                tuner_display(ui, ctx, p);
                knob_columns(ui, ctx, params, &[0, 1, 2], out);
            });
            true
        }
        DeviceParams::HybridReverb(p) => {
            hybrid_reverb_body(ui, ctx, params, p, out);
            true
        }
        _ => false,
    }
}

fn relevant<P: EffectParams>(p: &P, indices: &[usize]) -> Vec<usize> {
    indices.iter().copied().filter(|&i| p.is_relevant(i)).collect()
}

const DISPLAY_BG: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x1e);
const GRID_LINE: Color32 = Color32::from_rgb(0x33, 0x33, 0x33);
const HEIGHT: f32 = 124.0;

fn small_text(painter: &egui::Painter, pos: egui::Pos2, align: egui::Align2, text: impl ToString, color: Color32) {
    painter.text(pos, align, text.to_string(), egui::FontId::proportional(9.0), color);
}

fn fmt_hz(f: f32) -> String {
    if f >= 1000.0 {
        format!("{:.2} kHz", f / 1000.0)
    } else {
        format!("{f:.1} Hz")
    }
}

// ---------------------------------------------------------------------------
// Spectrum

#[derive(Clone, Default)]
struct SpectrumState {
    fft: Option<Arc<RealFft>>,
    window: Arc<Vec<f32>>,
    snap: Vec<f32>,
    frame: Vec<f32>,
    spec: Vec<Complex>,
    /// Smoothed magnitudes in dB per bin.
    db: Vec<f32>,
    last: f64,
    written: usize,
}

const SPEC_MIN_HZ: f32 = 20.0;

fn spectrum_display(ui: &mut egui::Ui, ctx: &PanelCtx, p: &SpectrumParams) {
    let (rect, resp) = ui.allocate_exact_size(vec2(320.0, HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, DISPLAY_BG);
    let key = egui::Id::new(("spectrum_state", ctx.id));
    let mut st: SpectrumState = ui.data_mut(|d| d.remove_temp(key)).unwrap_or_default();
    let scope = scope::find(ctx.id);
    let sr = scope
        .as_ref()
        .map(|s| s.sample_rate())
        .unwrap_or(ctx.sample_rate)
        .max(1000.0);
    let n = p.block_size();
    if st.fft.as_ref().map(|f| f.len()) != Some(n) {
        st.fft = Some(Arc::new(RealFft::new(n)));
        st.window = Arc::new(hann(n));
        st.db = vec![-200.0; n / 2 + 1];
        st.spec = vec![Complex::ZERO; n / 2 + 1];
        st.frame = vec![0.0; n];
    }
    let now = ui.input(|i| i.time);
    if let (Some(scope), Some(fft)) = (&scope, st.fft.clone()) {
        let written = scope.written();
        if now - st.last >= p.refresh as f64 * 0.001 && written != st.written {
            st.last = now;
            st.written = written;
            scope.snapshot_len(&mut st.snap, n);
            st.snap.resize(n, 0.0);
            for i in 0..n {
                st.frame[i] = st.snap[i] * st.window[i];
            }
            fft.forward(&st.frame, &mut st.spec);
            // A full-scale sine reads 0 dB (Hann coherent gain is 1/2).
            let norm = 4.0 / n as f32;
            let avg = p.average.clamp(0.0, 0.98);
            for (d, c) in st.db.iter_mut().zip(&st.spec) {
                let v = 20.0 * (c.abs() * norm + 1e-10).log10();
                *d = if *d < -150.0 { v } else { *d * avg + v * (1.0 - avg) };
            }
        }
    }
    let nyq = sr * 0.5;
    let max_hz = nyq.min(22000.0);
    let log = p.log_scale();
    let x_of = |f: f32| {
        let t = if log {
            (f.max(SPEC_MIN_HZ) / SPEC_MIN_HZ).ln() / (max_hz / SPEC_MIN_HZ).ln()
        } else {
            f / max_hz
        };
        rect.left() + rect.width() * t.clamp(0.0, 1.0)
    };
    let f_of = |x: f32| {
        let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        if log {
            SPEC_MIN_HZ * (max_hz / SPEC_MIN_HZ).powf(t)
        } else {
            t * max_hz
        }
    };
    let bins = st.db.len();
    let df = sr / n as f32;
    let peak = st.db.iter().skip(1).copied().fold(-200.0f32, f32::max);
    let (top, floor) = if p.auto >= 0.5 && peak > -150.0 {
        let top = ((peak + 6.0) / 6.0).ceil() * 6.0;
        (top, top - 72.0)
    } else {
        (0.0, p.range.min(-30.0))
    };
    let span = (top - floor).max(1.0);
    let y_of = |db: f32| rect.top() + 2.0 + (rect.height() - 4.0) * ((top - db) / span).clamp(0.0, 1.0);
    let db_of = |y: f32| top - (y - rect.top() - 2.0) / (rect.height() - 4.0) * span;

    // Grid.
    let grid = Stroke::new(1.0, GRID_LINE);
    let freqs: &[f32] = if log {
        &[50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0]
    } else {
        &[
            2000.0, 4000.0, 6000.0, 8000.0, 10000.0, 12000.0, 14000.0, 16000.0, 18000.0, 20000.0,
        ]
    };
    for &f in freqs {
        if f < max_hz {
            painter.vline(x_of(f), rect.y_range(), grid);
        }
    }
    let mut db = (top / 12.0).floor() * 12.0;
    while db > floor {
        painter.hline(rect.x_range(), y_of(db), grid);
        small_text(
            &painter,
            pos2(rect.right() - 2.0, y_of(db) + 1.0),
            egui::Align2::RIGHT_TOP,
            format!("{db:.0}"),
            theme::TEXT_DIM,
        );
        db -= 12.0;
    }
    let labels: &[(f32, &str)] = if log {
        &[(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")]
    } else {
        &[(5000.0, "5k"), (10000.0, "10k"), (15000.0, "15k")]
    };
    for &(f, l) in labels {
        small_text(
            &painter,
            pos2(x_of(f) + 2.0, rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            l,
            theme::TEXT_DIM,
        );
    }

    // Spectrum, one column per pixel: the loudest bin in the column's band,
    // or an interpolated value where bins are wider than a pixel.
    let value_at = |f0: f32, f1: f32| -> f32 {
        let k0 = f0 / df;
        let k1 = f1 / df;
        if k1 - k0 < 1.0 {
            let k = (0.5 * (k0 + k1)).clamp(0.0, (bins - 1) as f32);
            let i = k as usize;
            let j = (i + 1).min(bins - 1);
            let t = k - i as f32;
            st.db[i] + (st.db[j] - st.db[i]) * t
        } else {
            let a = (k0.floor() as usize).min(bins - 1);
            let b = (k1.ceil() as usize).clamp(a + 1, bins);
            st.db[a..b].iter().copied().fold(-200.0, f32::max)
        }
    };
    if bins > 2 && scope.is_some() {
        let cols = rect.width().max(1.0) as usize;
        let fill = theme::dim(theme::ACCENT, 0.35);
        let mut pts = Vec::with_capacity(cols);
        for c in 0..cols {
            let x0 = rect.left() + c as f32;
            let f0 = f_of(x0);
            let f1 = f_of(x0 + 1.0);
            if f1 > nyq {
                break;
            }
            let v = value_at(f0, f1);
            let y = y_of(v);
            painter.vline(x0 + 0.5, egui::Rangef::new(y, rect.bottom()), Stroke::new(1.0, fill));
            pts.push(pos2(x0 + 0.5, y));
        }
        painter.add(egui::Shape::line(pts, Stroke::new(1.2, theme::ACCENT)));
    } else {
        small_text(
            &painter,
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No signal",
            theme::TEXT_DIM,
        );
    }

    if let Some(pp) = resp.hover_pos() {
        let f = f_of(pp.x);
        let here = if bins > 2 { value_at(f, f * 1.0001) } else { -200.0 };
        painter.vline(pp.x, rect.y_range(), Stroke::new(1.0, theme::dim(theme::TEXT, 0.4)));
        let text = format!(
            "{}   {:.1} dB   (cursor {:.1} dB)",
            fmt_hz(f),
            here.max(-200.0),
            db_of(pp.y)
        );
        painter.text(
            pos2(rect.left() + 4.0, rect.top() + 3.0),
            egui::Align2::LEFT_TOP,
            text,
            egui::FontId::proportional(10.0),
            theme::TEXT,
        );
    }
    ui.data_mut(|d| d.insert_temp(key, st));
    ui.ctx().request_repaint();
}

// ---------------------------------------------------------------------------
// Tuner

#[derive(Clone, Default)]
struct TunerState {
    det: PitchDetector,
    snap: Vec<f32>,
    hz: f32,
    clarity: f32,
    last_seen: f64,
    last_time: f64,
    written: usize,
    history: VecDeque<f32>,
    strobe: [f32; 3],
}

const HISTORY: usize = 240;

fn tuner_display(ui: &mut egui::Ui, ctx: &PanelCtx, p: &TunerParams) {
    let (rect, _resp) = ui.allocate_exact_size(vec2(300.0, HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, DISPLAY_BG);
    let key = egui::Id::new(("tuner_state", ctx.id));
    let mut st: TunerState = ui.data_mut(|d| d.remove_temp(key)).unwrap_or_default();
    let now = ui.input(|i| i.time);
    let dt = (now - st.last_time).clamp(0.0, 0.1) as f32;
    st.last_time = now;
    let reference = p.reference.clamp(400.0, 500.0);

    if let Some(scope) = scope::find(ctx.id) {
        let sr = scope.sample_rate().max(1000.0);
        let written = scope.written();
        if written != st.written {
            st.written = written;
            let len = if sr > 60000.0 { 8192 } else { 4096 };
            scope.snapshot_len(&mut st.snap, len);
            match st.det.detect(&st.snap, sr, tuner::MIN_HZ, tuner::MAX_HZ) {
                Some(pitch) => {
                    let jump = st.hz <= 0.0 || (1200.0 * (pitch.hz / st.hz).log2()).abs() > 40.0;
                    st.hz = if jump { pitch.hz } else { st.hz * 0.6 + pitch.hz * 0.4 };
                    st.clarity = pitch.clarity;
                    st.last_seen = now;
                }
                None => {
                    if now - st.last_seen > 0.4 {
                        st.hz = 0.0;
                    }
                }
            }
        }
    }
    let active = st.hz > 0.0;
    let (note, cents) = if active {
        tuner::hz_to_note(st.hz, reference)
    } else {
        (0, 0.0)
    };
    st.history.push_back(if active { cents } else { f32::NAN });
    while st.history.len() > HISTORY {
        st.history.pop_front();
    }
    let in_tune = active && cents.abs() < 3.0;
    let color = if !active {
        theme::TEXT_DIM
    } else if in_tune {
        theme::PLAY
    } else if cents.abs() < 15.0 {
        theme::ACTIVE
    } else {
        theme::ARM
    };

    // Readout on the left.
    let left = Rect::from_min_size(rect.min, vec2(92.0, 94.0));
    painter.text(
        pos2(left.center().x, left.top() + 30.0),
        egui::Align2::CENTER_CENTER,
        if active { tuner::note_name(note) } else { "--".into() },
        egui::FontId::proportional(30.0),
        color,
    );
    painter.text(
        pos2(left.center().x, left.top() + 60.0),
        egui::Align2::CENTER_CENTER,
        if active {
            format!("{cents:+.1} ct")
        } else {
            String::new()
        },
        egui::FontId::proportional(12.0),
        color,
    );
    painter.text(
        pos2(left.center().x, left.top() + 78.0),
        egui::Align2::CENTER_CENTER,
        if active { fmt_hz(st.hz) } else { "No pitch".into() },
        egui::FontId::proportional(10.0),
        theme::TEXT_DIM,
    );

    // Meter on the right.
    let meter = Rect::from_min_max(
        pos2(rect.left() + 96.0, rect.top() + 4.0),
        pos2(rect.right() - 4.0, rect.top() + 90.0),
    );
    if p.mode >= 0.5 {
        // Strobe: three bands of stripes drifting at a speed proportional to
        // the deviation (still when in tune).
        let speed = if active { cents } else { 0.0 };
        let rows = 3;
        let row_h = meter.height() / rows as f32;
        for r in 0..rows {
            let period = [32.0, 16.0, 8.0][r];
            st.strobe[r] = (st.strobe[r] + speed * dt * 1.2 * (1 << r) as f32).rem_euclid(period);
            let y0 = meter.top() + r as f32 * row_h;
            let band = Rect::from_min_max(pos2(meter.left(), y0 + 1.0), pos2(meter.right(), y0 + row_h - 1.0));
            let clip = painter.with_clip_rect(band);
            clip.rect_filled(band, 0.0, Color32::from_rgb(0x15, 0x15, 0x15));
            let stripe = if active {
                theme::dim(color, 0.85)
            } else {
                Color32::from_rgb(0x40, 0x40, 0x40)
            };
            let mut x = band.left() - period + st.strobe[r];
            while x < band.right() {
                clip.rect_filled(
                    Rect::from_min_max(pos2(x, band.top()), pos2(x + period * 0.5, band.bottom())),
                    0.0,
                    stripe,
                );
                x += period;
            }
        }
    } else {
        // Classic needle: -50 .. +50 cents over a 120 degree arc.
        let pivot = pos2(meter.center().x, meter.bottom() + 8.0);
        let radius = (meter.height() + 2.0).min(meter.width() * 0.55);
        let angle = |c: f32| (c.clamp(-50.0, 50.0) / 50.0) * 60f32.to_radians();
        let at = |c: f32, r: f32| {
            let a = angle(c);
            pos2(pivot.x + r * a.sin(), pivot.y - r * a.cos())
        };
        for c in (-50..=50).step_by(10) {
            let major = c % 50 == 0 || c == 0;
            let len = if major { 10.0 } else { 6.0 };
            let col = if c == 0 { theme::PLAY } else { theme::TEXT_DIM };
            painter.line_segment(
                [at(c as f32, radius - len), at(c as f32, radius)],
                Stroke::new(1.0, col),
            );
        }
        small_text(
            &painter,
            at(-50.0, radius + 2.0),
            egui::Align2::RIGHT_BOTTOM,
            "-50",
            theme::TEXT_DIM,
        );
        small_text(
            &painter,
            at(50.0, radius + 2.0),
            egui::Align2::LEFT_BOTTOM,
            "+50",
            theme::TEXT_DIM,
        );
        // In-tune zone.
        let zone: Vec<_> = (-3..=3).map(|c| at(c as f32, radius - 2.0)).collect();
        painter.add(egui::Shape::line(zone, Stroke::new(3.0, theme::dim(theme::PLAY, 0.6))));
        if active {
            painter.line_segment([pivot, at(cents, radius - 4.0)], Stroke::new(2.0, color));
        }
        painter.circle_filled(pivot, 3.0, theme::TEXT_DIM);
    }

    // History strip.
    let strip = Rect::from_min_max(
        pos2(rect.left() + 4.0, rect.bottom() - 28.0),
        pos2(rect.right() - 4.0, rect.bottom() - 4.0),
    );
    painter.rect_filled(strip, 1.0, Color32::from_rgb(0x16, 0x16, 0x16));
    painter.hline(strip.x_range(), strip.center().y, Stroke::new(1.0, GRID_LINE));
    let step = strip.width() / HISTORY as f32;
    let offset = HISTORY - st.history.len();
    let mut seg: Vec<egui::Pos2> = Vec::new();
    for (i, &c) in st.history.iter().enumerate() {
        if c.is_finite() {
            let x = strip.left() + (offset + i) as f32 * step;
            let y = strip.center().y - c.clamp(-50.0, 50.0) / 50.0 * strip.height() * 0.5;
            seg.push(pos2(x, y));
        } else if !seg.is_empty() {
            painter.add(egui::Shape::line(
                std::mem::take(&mut seg),
                Stroke::new(1.0, theme::ACCENT),
            ));
        }
    }
    if !seg.is_empty() {
        painter.add(egui::Shape::line(seg, Stroke::new(1.0, theme::ACCENT)));
    }
    small_text(
        &painter,
        pos2(rect.right() - 4.0, rect.top() + 92.0),
        egui::Align2::RIGHT_TOP,
        format!("A = {:.1} Hz", reference),
        theme::TEXT_DIM,
    );

    ui.data_mut(|d| d.insert_temp(key, st));
    ui.ctx().request_repaint();
}

// ---------------------------------------------------------------------------
// Hybrid Reverb

fn hybrid_reverb_body(
    ui: &mut egui::Ui,
    ctx: &PanelCtx,
    params: &DeviceParams,
    p: &HybridReverbParams,
    out: &mut Vec<(usize, f32)>,
) {
    use hybrid_reverb::idx::*;
    ui.horizontal_top(|ui| {
        reverb_preview(ui, ctx, p);
        let groups: [&[usize]; 4] = [
            &[ROUTING, IR, IR_DECAY, IR_ATTACK],
            &[PREDELAY, SYNC_DIV, SYNC, BLEND],
            &[ALGORITHM, DECAY, SIZE, DAMPING, DIFFUSION, MODULATION, SHIMMER, FREEZE],
            &[LOW_CUT, HIGH_CUT, WIDTH, MIX],
        ];
        for (g, group) in groups.iter().enumerate() {
            if g > 0 {
                ui.separator();
            }
            knob_columns(ui, ctx, params, &relevant(p, group), out);
        }
    });
}

/// IR envelope (with the decay and attack envelopes applied) and the
/// algorithm's decay line on a shared time axis.
fn reverb_preview(ui: &mut egui::Ui, ctx: &PanelCtx, p: &HybridReverbParams) {
    let (rect, _) = ui.allocate_exact_size(vec2(140.0, HEIGHT), Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, DISPLAY_BG);
    let routing = p.routing();
    let env = if routing != Routing::Algorithm {
        hybrid_reverb::ir_envelope(ctx.sample_rate, p.ir_index())
    } else {
        None
    };
    let algo = routing != Routing::Convolution;
    let ir_len = env.as_ref().map(|e| e.1).unwrap_or(0.0);
    let algo_len = if algo { p.decay.min(12.0) } else { 0.0 };
    let total = ir_len.max(algo_len).max(0.5);
    let plot = rect.shrink2(vec2(3.0, 14.0));
    let x_of = |t: f32| plot.left() + plot.width() * (t / total).clamp(0.0, 1.0);
    let y_of = |a: f32| plot.bottom() - plot.height() * a.clamp(0.0, 1.0);
    if let Some((env, len)) = &env {
        let n = env.len().max(1);
        let w = plot.width() * (*len / total) / n as f32;
        let fill = theme::dim(theme::ACCENT, 0.55);
        for (i, &a) in env.iter().enumerate() {
            let t = (i as f32 + 0.5) / n as f32 * len;
            let g = hybrid_reverb::ir_gain(t, *len, p.ir_decay, p.ir_attack);
            let x = x_of(i as f32 / n as f32 * len);
            let y = y_of((a * g).sqrt());
            painter.rect_filled(
                Rect::from_min_max(pos2(x, y), pos2(x + w.max(1.0), plot.bottom())),
                0.0,
                fill,
            );
        }
    }
    if algo {
        // -60 dB at the decay time; amplitude drawn on a square-root scale
        // like the IR bars.
        let rt = p.decay.max(0.05);
        let pts: Vec<_> = (0..=60)
            .map(|i| {
                let t = total * i as f32 / 60.0;
                let a = 10f32.powf(-3.0 * t / rt);
                pos2(x_of(t), y_of(a.sqrt()))
            })
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.3, theme::SOLO)));
    }
    let title = match routing {
        Routing::Algorithm => hybrid_reverb::ALGORITHMS
            [(p.algorithm.round().max(0.0) as usize).min(hybrid_reverb::ALGORITHMS.len() - 1)]
        .to_string(),
        Routing::Convolution => hybrid_reverb::IR_NAMES[p.ir_index()].to_string(),
        _ => format!(
            "{} {} {}",
            hybrid_reverb::IR_NAMES[p.ir_index()],
            if routing == Routing::Serial { ">" } else { "+" },
            hybrid_reverb::ALGORITHMS[(p.algorithm.round().max(0.0) as usize).min(hybrid_reverb::ALGORITHMS.len() - 1)]
        ),
    };
    small_text(
        &painter,
        pos2(rect.left() + 3.0, rect.top() + 2.0),
        egui::Align2::LEFT_TOP,
        title,
        theme::TEXT,
    );
    small_text(
        &painter,
        pos2(rect.right() - 3.0, rect.bottom() - 2.0),
        egui::Align2::RIGHT_BOTTOM,
        format!("{total:.1} s"),
        theme::TEXT_DIM,
    );
    if p.freeze >= 0.5 {
        small_text(
            &painter,
            pos2(rect.left() + 3.0, rect.bottom() - 2.0),
            egui::Align2::LEFT_BOTTOM,
            "FREEZE",
            theme::SOLO,
        );
    }
}
