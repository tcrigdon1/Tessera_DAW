//! The Polyrhythm window (piano roll "Poly"): stacked layers of evenly
//! spaced hits written into the clip, built-in presets, and metric
//! modulation of the song tempo.

use std::sync::OnceLock;

use eframe::egui::{self, vec2};
use tessera_engine::midi::note_name;
use tessera_engine::rhythm::{self, PolyLayer, PolyPreset, SpanUnit};

use super::super::theme;
use super::toolbar::tuplet_combo;
use crate::app::{ClipRef, TesseraApp};

/// Most layers a stack can have.
const MAX_LAYERS: usize = 8;

fn presets() -> &'static [PolyPreset] {
    static P: OnceLock<Vec<PolyPreset>> = OnceLock::new();
    P.get_or_init(rhythm::presets)
}

/// The MIDI clip the generator writes into: the one in the piano roll, or
/// the selected clip.
fn target(app: &TesseraApp) -> Option<ClipRef> {
    super::active_clip(app).or(app.selected_clip.filter(|&r| app.clip(r).is_some_and(|c| c.is_midi())))
}

/// How long until every layer is back on its first hit at once, in beats
/// (`None` past 64 bars).
fn realign(layers: &[PolyLayer], bpb: f64) -> Option<f64> {
    // Spans in 1/32 beats are whole numbers for every meter offered.
    let units: Vec<u64> = layers
        .iter()
        .filter(|l| l.on)
        .map(|l| (l.span(bpb) * 32.0).round().max(1.0) as u64)
        .collect();
    let gcd = |mut a: u64, mut b: u64| {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    let mut l = 1u64;
    for u in units {
        l = l / gcd(l, u) * u;
        if l as f64 / 32.0 > bpb * 64.0 {
            return None;
        }
    }
    Some(l as f64 / 32.0)
}

fn bars_text(beats: f64, bpb: f64) -> String {
    let bars = beats / bpb;
    if (bars - bars.round()).abs() < 1e-9 {
        let b = bars.round();
        format!("{b} bar{}", if b == 1.0 { "" } else { "s" })
    } else {
        format!("{} beats", (beats * 1000.0).round() / 1000.0)
    }
}

pub(super) fn window(app: &mut TesseraApp, ctx: &egui::Context) {
    if !app.piano.poly_open {
        return;
    }
    let target = target(app);
    let (bpb, clip_len, clip_name) = match target.and_then(|r| app.clip(r)) {
        Some(c) => (
            c.bar_beats(app.project.beats_per_bar, app.project.beat_unit),
            Some(c.length),
            c.name.clone(),
        ),
        None => (app.project.beats_per_bar.max(1) as f64, None, String::new()),
    };
    let pads = target.and_then(|r| super::super::sample_panels::drum_rows(app, r.track()));
    let tempo = app.project.tempo;
    let mut open = true;
    let mut generate = false;
    let mut new_tempo = None;
    egui::Window::new("Polyrhythm")
        .open(&mut open)
        .default_pos(ctx.content_rect().center() - vec2(260.0, 200.0))
        .collapsible(false)
        .resizable(false)
        .default_width(520.0)
        .show(ctx, |ui| {
            let st = &mut app.piano;
            ui.horizontal(|ui| {
                ui.label("Preset");
                let name = st
                    .poly_preset
                    .and_then(|i| presets().get(i))
                    .map(|p| p.name)
                    .unwrap_or("Custom");
                egui::ComboBox::from_id_salt("poly_preset")
                    .width(220.0)
                    .selected_text(name)
                    .show_ui(ui, |ui| {
                        for (i, p) in presets().iter().enumerate() {
                            if ui
                                .selectable_label(st.poly_preset == Some(i), p.name)
                                .on_hover_text(p.about)
                                .clicked()
                            {
                                st.poly_preset = Some(i);
                                st.poly_layers = p.layers.clone();
                            }
                        }
                    });
            });
            if let Some(p) = st.poly_preset.and_then(|i| presets().get(i)) {
                ui.label(egui::RichText::new(p.about).small().color(theme::text_dim()));
            }
            ui.add_space(4.0);

            let before = st.poly_layers.clone();
            let mut remove = None;
            egui::Grid::new("poly_layers")
                .num_columns(9)
                .spacing(vec2(6.0, 4.0))
                .show(ui, |ui| {
                    for h in ["", "Sound", "Hits", "Across", "Split", "Fill", "Velocity", "Accent", ""] {
                        ui.label(egui::RichText::new(h).small().color(theme::text_dim()));
                    }
                    ui.end_row();
                    for (i, l) in st.poly_layers.iter_mut().enumerate() {
                        ui.checkbox(&mut l.on, "").on_hover_text("Play this layer");
                        pitch_picker(ui, i, &mut l.pitch, pads.as_deref());
                        ui.add(egui::DragValue::new(&mut l.pulses).range(1..=64).speed(0.05))
                            .on_hover_text("Evenly spaced hits per cycle: the first number of 5:4");
                        ui.horizontal(|ui| {
                            ui.add(egui::DragValue::new(&mut l.across).range(1..=64).speed(0.05))
                                .on_hover_text("Cycle length: the second number of 5:4");
                            egui::ComboBox::from_id_salt(("poly_unit", i))
                                .width(78.0)
                                .selected_text(l.unit.label(l.across != 1))
                                .show_ui(ui, |ui| {
                                    for u in SpanUnit::ALL {
                                        ui.selectable_value(&mut l.unit, u, u.label(l.across != 1));
                                    }
                                });
                        });
                        ui.add(egui::DragValue::new(&mut l.subdivide).range(1..=16).speed(0.05).prefix("× "))
                            .on_hover_text(
                                "Nested tuplets: splits every hit into this many. 3 hits split × 5 puts quintuplets inside triplets; the in-between hits play as ghost notes.",
                            );
                        ui.horizontal(|ui| {
                            let steps = l.steps();
                            ui.add(
                                egui::DragValue::new(&mut l.hits)
                                    .range(0..=steps)
                                    .speed(0.05)
                                    .custom_formatter(|v, _| if v == 0.0 { "All".into() } else { format!("{v}") }),
                            )
                            .on_hover_text("Euclidean fill: play only this many of the hits, spread as evenly as possible (All plays every hit)");
                            ui.add_enabled(
                                l.hits > 0 && l.hits < steps,
                                egui::DragValue::new(&mut l.rotate).range(0..=steps.saturating_sub(1)).speed(0.05).prefix("↻"),
                            )
                            .on_hover_text("Rotate the Euclidean pattern");
                        });
                        ui.add(egui::DragValue::new(&mut l.velocity).range(1..=127).speed(0.3));
                        ui.checkbox(&mut l.accent, "").on_hover_text("Accent the first hit of every cycle");
                        if ui.small_button("×").on_hover_text("Remove the layer").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
            if let Some(i) = remove {
                st.poly_layers.remove(i);
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(st.poly_layers.len() < MAX_LAYERS, egui::Button::new("+ Layer"))
                    .clicked()
                {
                    let pitch = pads
                        .as_ref()
                        .and_then(|p| p.iter().rev().nth(st.poly_layers.len()).map(|(n, _)| *n))
                        .unwrap_or(36);
                    st.poly_layers.push(PolyLayer {
                        pitch,
                        ..PolyLayer::default()
                    });
                }
                let summary: Vec<String> = st.poly_layers.iter().filter(|l| l.on).map(|l| l.describe()).collect();
                ui.label(egui::RichText::new(summary.join("  ·  ")).small().color(theme::text_dim()));
            });
            if st.poly_layers != before {
                st.poly_preset = None;
            }
            if let Some(beats) = realign(&st.poly_layers, bpb) {
                let text = if beats <= bpb + 1e-9 && (bpb / beats).fract() == 0.0 {
                    "Every layer meets on each downbeat.".to_string()
                } else {
                    format!("All layers line up again every {}.", bars_text(beats, bpb))
                };
                ui.label(egui::RichText::new(text).small().color(theme::text_dim()));
            } else {
                ui.label(
                    egui::RichText::new("The layers take more than 64 bars to line up again.")
                        .small()
                        .color(theme::text_dim()),
                );
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut st.poly_replace, "Replace notes on these sounds")
                    .on_hover_text("Clear the clip's notes on the layers' sounds first; other notes stay");
                let label = match clip_len {
                    Some(len) => format!("Write into \"{clip_name}\" ({})", bars_text(len, bpb)),
                    None => "Select a MIDI clip".to_string(),
                };
                generate = ui
                    .add_enabled(clip_len.is_some() && st.poly_layers.iter().any(|l| l.on), egui::Button::new(label))
                    .on_hover_text("Fills the clip's whole length; notes land on exact fractions of the beat")
                    .clicked();
            });

            ui.separator();
            ui.label(egui::RichText::new("Metric modulation").strong());
            ui.horizontal(|ui| {
                ui.label("Make");
                tuplet_combo(ui, "poly_modulation", &mut st.modulation, "Straight");
                let to = rhythm::modulated_tempo(tempo, st.modulation);
                ui.label(format!("the new beat: {tempo:.2} to {to:.2} BPM"));
                if ui
                    .add_enabled(!st.modulation.is_straight() && (20.0..=999.0).contains(&to), egui::Button::new("Modulate"))
                    .on_hover_text("Sets the song tempo so the tuplet's notes become the new straight notes. 3:2 turns eighth-note triplets into eighths.")
                    .clicked()
                {
                    st.tempo_before_modulation = Some(tempo);
                    new_tempo = Some(to);
                }
                if let Some(back) = st.tempo_before_modulation {
                    if ui
                        .button(format!("Snap back to {back:.2}"))
                        .on_hover_text("Return to the tempo before the last modulation")
                        .clicked()
                    {
                        st.tempo_before_modulation = None;
                        new_tempo = Some(back);
                    }
                }
            });
        });
    app.piano.poly_open = open;
    if let Some(t) = new_tempo {
        app.label_edit("Metric Modulation");
        app.set_tempo(t);
        app.changed();
    }
    if generate {
        if let Some(r) = target {
            write_layers(app, r, bpb);
        }
    }
}

/// A drum pad (by name) for drum tracks, a note number elsewhere.
fn pitch_picker(ui: &mut egui::Ui, i: usize, pitch: &mut u8, pads: Option<&[(u8, String)]>) {
    match pads {
        Some(pads) => {
            let name = pads
                .iter()
                .find(|(n, _)| n == pitch)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| note_name(*pitch));
            egui::ComboBox::from_id_salt(("poly_pitch", i))
                .width(96.0)
                .selected_text(name)
                .show_ui(ui, |ui| {
                    for (n, s) in pads.iter().rev() {
                        ui.selectable_value(pitch, *n, s);
                    }
                });
        }
        None => {
            ui.add(
                egui::DragValue::new(pitch)
                    .range(0..=127)
                    .speed(0.2)
                    .custom_formatter(|v, _| note_name(v as u8)),
            );
        }
    }
}

/// Writes the layers into the clip, from its start to its loop end.
fn write_layers(app: &mut TesseraApp, r: ClipRef, bpb: f64) {
    let layers = app.piano.poly_layers.clone();
    let replace = app.piano.poly_replace;
    app.label_edit("Generate Polyrhythm");
    super::apply(app, r, true, |notes, sel, len| {
        if replace {
            notes.retain(|n| !layers.iter().any(|l| l.on && l.pitch == n.pitch));
        }
        let first = notes.len();
        notes.extend(rhythm::generate(&layers, len, bpb));
        *sel = (first..notes.len()).collect();
    });
    app.status = "Wrote the polyrhythm into the clip".into();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_realign_at_the_least_common_cycle() {
        let three = PolyLayer {
            pulses: 3,
            across: 3,
            unit: SpanUnit::Sixteenth,
            ..PolyLayer::default()
        };
        let bar = PolyLayer {
            pulses: 4,
            across: 1,
            unit: SpanUnit::Bar,
            ..PolyLayer::default()
        };
        // 3/16 against a 4/4 bar: 3 bars.
        assert_eq!(realign(&[three, bar.clone()], 4.0), Some(12.0));
        assert_eq!(realign(&[bar], 4.0), Some(4.0));
    }
}
