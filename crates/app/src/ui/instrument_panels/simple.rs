//! Device view panel of the Simple Synth.

use eframe::egui;
use tessera_engine::dsp::simple::{idx, SimpleParams};
use tessera_engine::model::{Id, Instrument};

use super::analog::{filter_mag, none, plot_adsr, plot_fn, plot_lfo, plot_response, section};
use super::edit_instrument;
use crate::app::TesseraApp;

/// The oscillator 1 morph as drawn: sine -> triangle -> saw -> square.
fn morph(shape: f32, x: f32) -> f32 {
    let basic = |w: usize| match w {
        0 => (x * std::f32::consts::TAU).sin(),
        1 => {
            let ph = (x + 0.75).fract();
            2.0 * (2.0 * ph - 1.0).abs() - 1.0
        }
        2 => 2.0 * x - 1.0,
        _ => {
            if x < 0.5 {
                1.0
            } else {
                -1.0
            }
        }
    };
    let s = shape.clamp(0.0, 1.0) * 3.0;
    let i = (s as usize).min(2);
    let f = s - i as f32;
    basic(i) + (basic(i + 1) - basic(i)) * f
}

pub fn device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    ui.scope(|ui| {
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        edit_instrument(app, track, |_app, inst| {
            let Instrument::Simple(p) = inst else { return false };
            panel(ui, track, p)
        });
    });
}

fn panel(ui: &mut egui::Ui, track: Id, p: &mut SimpleParams) -> bool {
    let mut changed = false;
    changed |= section(ui, track, "OSC 1", p, &[idx::SHAPE, idx::SHAPE + 1], |ui, p| {
        let shape = p.o1_shape;
        plot_fn(ui, 80.0, "Shape", |x| morph(shape, x) * 0.9);
    });
    changed |= section(
        ui,
        track,
        "OSC 2 / NOISE",
        p,
        &(idx::O2_WAVE..=idx::NOISE).collect::<Vec<_>>(),
        none,
    );
    changed |= section(
        ui,
        track,
        "FILTER",
        p,
        &(idx::F_TYPE..=idx::F_LFO).collect::<Vec<_>>(),
        |ui, p| {
            let two = p.f_type >= 0.5;
            let (fc, res) = (p.cutoff, p.res);
            plot_response(ui, 100.0, if two { "LP II" } else { "LP I" }, fc, |f| {
                let x = f / fc;
                if two {
                    // Cascade of a plain and a resonant 12 dB stage.
                    filter_mag(0, x, 0.1) * filter_mag(0, x, res)
                } else {
                    filter_mag(0, x, res * 0.99)
                }
            });
        },
    );
    changed |= section(
        ui,
        track,
        "ENV 1 (AMP)",
        p,
        &(idx::A1..idx::E2_MODE).collect::<Vec<_>>(),
        |ui, p| plot_adsr(ui, 80.0, p.a1, p.d1, p.s1, p.r1, false),
    );
    let cycle = p.e2_mode >= 0.5;
    let mut e2: Vec<usize> = vec![idx::E2_MODE, idx::E2_MODE + 1, idx::E2_MODE + 2];
    if !cycle {
        e2.push(idx::S2);
    }
    e2.extend([idx::S2 + 1, idx::S2 + 2, idx::E2_SHAPE]);
    changed |= section(ui, track, "ENV 2", p, &e2, |ui, p| {
        plot_adsr(ui, 80.0, p.a2, p.d2, p.s2, p.r2, p.e2_mode >= 0.5)
    });
    changed |= section(
        ui,
        track,
        "LFO",
        p,
        &(idx::LFO_SHAPE..=idx::LFO_AMP).collect::<Vec<_>>(),
        |ui, p| plot_lfo(ui, 60.0, p.lfo_shape.round().max(0.0) as usize),
    );
    let mut g: Vec<usize> = vec![idx::DRIFT, idx::VOICE_MODE];
    if p.voice_mode >= 1.5 {
        g.push(idx::SPREAD);
    }
    g.extend(idx::SPREAD + 1..=idx::VOLUME);
    changed |= section(ui, track, "GLOBAL", p, &g, none);
    changed
}
