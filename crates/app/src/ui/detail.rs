//! Detail view at the bottom: the clip editor (piano roll / waveform) or the
//! selected track's device.

use eframe::egui::{self, vec2, Sense};
use tessera_engine::dsp::drums;
use tessera_engine::dsp::synth::Waveform;
use tessera_engine::model::{Id, Instrument};
use tessera_engine::Command;

use super::widgets::{self, Scale};
use super::{clip_envelopes, piano_roll, theme, warp};
use crate::app::{ClipRef, DetailMode, TesseraApp};
use crate::automation::ClipTab;

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    match app.detail {
        DetailMode::Clip => clip_view(app, ui),
        DetailMode::Device => device_view(app, ui),
    }
}

pub(super) fn hint(ui: &mut egui::Ui, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(egui::RichText::new(text).color(theme::text_dim()));
    });
}

// ----- clip view -----------------------------------------------------------------

fn clip_view(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let Some(r) = app.selected_clip else {
        hint(
            ui,
            "Select a clip to edit it. Double-click an empty MIDI slot to create one.",
        );
        return;
    };
    if app.clip(r).is_none() {
        hint(
            ui,
            "Empty slot. Double-click to create a MIDI clip, or drop a WAV file on an audio track.",
        );
        return;
    }
    let is_midi = app.clip(r).map(|c| c.is_midi()).unwrap_or(false);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(190.0, ui.available_height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                clip_tabs(app, ui, is_midi);
                egui::ScrollArea::vertical()
                    .id_salt("clip_props_scroll")
                    .show(ui, |ui| {
                        clip_properties(app, ui, r);
                        super::clip_launch::draw(app, ui, r);
                    });
            },
        );
        ui.separator();
        match app.automation.clip_tab {
            ClipTab::Envelopes => {
                ui.vertical(|ui| clip_envelopes::draw(app, ui, r));
            }
            ClipTab::Main if is_midi => piano_roll::draw(app, ui, r),
            ClipTab::Main => {
                ui.vertical(|ui| warp::editor(app, ui, r));
            }
        }
    });
}

/// Switches the clip view between notes/sample and envelopes.
fn clip_tabs(app: &mut TesseraApp, ui: &mut egui::Ui, is_midi: bool) {
    ui.horizontal(|ui| {
        let tab = app.automation.clip_tab;
        let main = if is_midi { "Notes" } else { "Sample" };
        if widgets::toggle(ui, tab == ClipTab::Main, main, theme::ACCENT, 56.0).clicked() {
            app.automation.clip_tab = ClipTab::Main;
        }
        if widgets::toggle(ui, tab == ClipTab::Envelopes, "Envelopes", theme::ACCENT, 70.0).clicked() {
            app.automation.clip_tab = ClipTab::Envelopes;
        }
    });
}

fn clip_properties(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let mut changed = false;
    let mut launch = false;
    let mut to_arr = false;
    let bpb = app.project.beats_per_bar as f64;
    let grooves: Vec<(Id, String)> = app.project.grooves.iter().map(|g| (g.id, g.name.clone())).collect();
    let mut groove_choice: Option<Option<Id>> = None;
    let mut commit = false;
    let mut open_pool = false;
    let Some(clip) = app.clip_mut(r) else { return };
    let color = theme::rgb(clip.color);
    let (stripe, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
    ui.painter().rect_filled(stripe, 1.0, color);
    changed |= ui.text_edit_singleline(&mut clip.name).changed();
    egui::Grid::new("clip_props")
        .num_columns(2)
        .spacing(vec2(6.0, 4.0))
        .show(ui, |ui| {
            ui.label("Length");
            let mut bars = clip.length / bpb;
            if ui
                .add(
                    egui::DragValue::new(&mut bars)
                        .range(0.25..=256.0)
                        .speed(0.05)
                        .max_decimals(2)
                        .suffix(" bars"),
                )
                .changed()
            {
                clip.length = (bars * bpb * 4.0).round().max(1.0) / 4.0;
                changed = true;
            }
            ui.end_row();
            ui.label("Loop");
            changed |= ui.checkbox(&mut clip.looping, "").changed();
            ui.end_row();
            // MIDI clips have no gain (velocities set their level).
            if !clip.is_midi() {
                ui.label("Gain");
                changed |= ui
                    .add(
                        egui::DragValue::new(&mut clip.gain_db)
                            .range(-70.0..=24.0)
                            .speed(0.1)
                            .suffix(" dB"),
                    )
                    .on_hover_text("Clip gain (drag, or double-click to type)")
                    .changed();
                ui.end_row();
            }
        });
    // Groove Pool: the groove this clip plays with.
    if clip.is_midi() {
        ui.horizontal(|ui| {
            let current = clip
                .groove
                .and_then(|id| grooves.iter().find(|(g, _)| *g == id))
                .map(|(_, n)| n.as_str())
                .unwrap_or("None");
            egui::ComboBox::from_id_salt(("clip_groove", clip.id))
                .selected_text(current)
                .width(100.0)
                .show_ui(ui, |ui| {
                    if ui.selectable_label(clip.groove.is_none(), "None").clicked() {
                        groove_choice = Some(None);
                    }
                    for (id, name) in &grooves {
                        if ui.selectable_label(clip.groove == Some(*id), name).clicked() {
                            groove_choice = Some(Some(*id));
                        }
                    }
                })
                .response
                .on_hover_text("Groove (from the Groove Pool)");
            commit = ui
                .add_enabled(clip.groove.is_some(), egui::Button::new("Commit").small())
                .on_hover_text("Write the groove into the notes")
                .clicked();
            open_pool = ui.small_button("Pool").on_hover_text("Open the Groove Pool").clicked();
        });
    }
    // Quantize, Duplicate and the other note tools are on the piano roll's toolbar.
    if let Some(notes) = clip.notes_mut() {
        if ui
            .small_button("Clear")
            .on_hover_text("Delete every note in the clip")
            .clicked()
        {
            notes.clear();
            changed = true;
        }
    }
    if let ClipRef::Slot { .. } = r {
        ui.horizontal(|ui| {
            launch = ui.button("▶ Launch").clicked();
            to_arr = ui.button("To Arrangement").clicked();
        });
    }
    if changed {
        app.changed();
    }
    if let Some(g) = groove_choice {
        app.set_clip_groove(r, g);
    }
    if commit {
        app.commit_groove(r);
    }
    if open_pool {
        app.work.show_grooves = true;
    }
    if let ClipRef::Slot { track, scene } = r {
        if launch {
            app.send(Command::LaunchClip { track, slot: scene });
        }
        if to_arr {
            app.slot_to_arrangement(track, scene);
        }
    }
}

// ----- device view ---------------------------------------------------------------

fn device_view(app: &mut TesseraApp, ui: &mut egui::Ui) {
    super::devices::draw(app, ui);
}

pub(super) fn instrument_picker(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let current = match app.project.track(track).map(|t| t.instrument) {
        Some(Instrument::Plugin(_)) => app
            .project
            .plugin(track)
            .map(|p| p.plugin.name.clone())
            .unwrap_or_default(),
        Some(i) => i.name().to_string(),
        None => String::new(),
    };
    let plugins: Vec<_> = app.plugins.plugins().into_iter().filter(|p| p.instrument).collect();
    let mut choice: Option<Instrument> = None;
    let mut plugin_choice = None;
    egui::ComboBox::from_id_salt(("instrument", track))
        .selected_text(current)
        .show_ui(ui, |ui| {
            for p in tessera_engine::library::instruments() {
                if ui.selectable_label(false, p.name).clicked() {
                    choice = Some(p.instrument);
                }
            }
            ui.separator();
            for p in tessera_engine::library::sounds()
                .into_iter()
                .chain(tessera_engine::library::drum_kits())
            {
                if ui
                    .selectable_label(false, format!("{}: {}", p.category, p.name))
                    .clicked()
                {
                    choice = Some(p.instrument);
                }
            }
            if !plugins.is_empty() {
                ui.separator();
                for p in &plugins {
                    if ui
                        .selectable_label(false, format!("{} ({})", p.name, p.format.label()))
                        .clicked()
                    {
                        plugin_choice = Some(p.clone());
                    }
                }
            }
        });
    if let Some(i) = choice {
        app.set_instrument(track, i);
    }
    if let Some(p) = plugin_choice {
        app.add_plugin(track, &p);
    }
}

pub(super) fn device_box(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(theme::gray(0x2b))
        .corner_radius(3.0)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(egui::RichText::new(title).small().color(theme::ACCENT));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    add(ui);
                });
            });
        });
}

fn fmt_hz(v: f32) -> String {
    if v >= 1000.0 {
        format!("{:.1} kHz", v / 1000.0)
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

pub(super) fn synth_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let Some(Instrument::Synth(mut p)) = app.project.track(track).map(|t| t.instrument) else {
        return;
    };
    let before = p;
    device_box(ui, "OSCILLATORS", |ui| {
        ui.vertical(|ui| {
            for (label, w) in [("Osc 1", &mut p.osc1), ("Osc 2", &mut p.osc2)] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).small());
                    egui::ComboBox::from_id_salt((label, track))
                        .width(64.0)
                        .selected_text(w.label())
                        .show_ui(ui, |ui| {
                            for opt in Waveform::ALL {
                                ui.selectable_value(w, opt, opt.label());
                            }
                        });
                });
            }
        });
        widgets::knob(
            ui,
            "Semi",
            &mut p.osc2_semitones,
            -24.0,
            24.0,
            0.0,
            Scale::Linear,
            |v| format!("{:+.0} st", v.round()),
        );
        p.osc2_semitones = p.osc2_semitones.round();
        widgets::knob(ui, "Detune", &mut p.osc2_detune, -50.0, 50.0, 0.0, Scale::Linear, |v| {
            format!("{v:+.0} ct")
        });
        widgets::knob(ui, "Mix", &mut p.osc_mix, 0.0, 1.0, 0.5, Scale::Linear, fmt_pct);
    });
    device_box(ui, "FILTER", |ui| {
        widgets::knob(ui, "Cutoff", &mut p.cutoff, 30.0, 18000.0, 1800.0, Scale::Log, fmt_hz);
        widgets::knob(ui, "Res", &mut p.resonance, 0.0, 4.0, 0.9, Scale::Linear, |v| {
            format!("{v:.2}")
        });
        widgets::knob(ui, "Env Amt", &mut p.filter_env, -4.0, 6.0, 2.0, Scale::Linear, |v| {
            format!("{v:+.1} oct")
        });
    });
    device_box(ui, "FILTER ENVELOPE", |ui| {
        widgets::knob(ui, "Attack", &mut p.f_attack, 0.001, 5.0, 0.002, Scale::Log, fmt_time);
        widgets::knob(ui, "Decay", &mut p.f_decay, 0.005, 5.0, 0.35, Scale::Log, fmt_time);
        widgets::knob(ui, "Sustain", &mut p.f_sustain, 0.0, 1.0, 0.2, Scale::Linear, fmt_pct);
        widgets::knob(ui, "Release", &mut p.f_release, 0.005, 8.0, 0.3, Scale::Log, fmt_time);
    });
    device_box(ui, "AMP ENVELOPE", |ui| {
        widgets::knob(ui, "Attack", &mut p.attack, 0.001, 5.0, 0.005, Scale::Log, fmt_time);
        widgets::knob(ui, "Decay", &mut p.decay, 0.005, 5.0, 0.3, Scale::Log, fmt_time);
        widgets::knob(ui, "Sustain", &mut p.sustain, 0.0, 1.0, 0.7, Scale::Linear, fmt_pct);
        widgets::knob(ui, "Release", &mut p.release, 0.005, 8.0, 0.25, Scale::Log, fmt_time);
    });
    device_box(ui, "OUTPUT", |ui| {
        widgets::knob(ui, "Volume", &mut p.volume_db, -40.0, 6.0, -10.0, Scale::Linear, |v| {
            format!("{v:.1} dB")
        });
    });
    if p != before {
        if let Some(t) = app.project.track_mut(track) {
            t.instrument = Instrument::Synth(p);
        }
        app.send_instrument(track);
    }
}

pub(super) fn drum_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let Some(Instrument::DrumKit(mut p)) = app.project.track(track).map(|t| t.instrument) else {
        return;
    };
    let before = p;
    let mut hit: Option<u8> = None;
    device_box(ui, "PADS", |ui| {
        egui::Grid::new(("pads", track)).spacing(vec2(3.0, 3.0)).show(ui, |ui| {
            for (i, (note, name)) in drums::PADS.iter().enumerate() {
                if ui
                    .add_sized(vec2(70.0, 22.0), egui::Button::new(egui::RichText::new(*name).small()))
                    .clicked()
                {
                    hit = Some(*note);
                }
                if i % 5 == 4 {
                    ui.end_row();
                }
            }
        });
    });
    device_box(ui, "KICK", |ui| {
        widgets::knob(ui, "Tune", &mut p.kick_tune, 30.0, 90.0, 50.0, Scale::Linear, fmt_hz);
        widgets::knob(ui, "Decay", &mut p.kick_decay, 0.05, 2.0, 0.45, Scale::Log, fmt_time);
    });
    device_box(ui, "SNARE", |ui| {
        widgets::knob(
            ui,
            "Tone",
            &mut p.snare_tone,
            100.0,
            400.0,
            190.0,
            Scale::Linear,
            fmt_hz,
        );
        widgets::knob(ui, "Decay", &mut p.snare_decay, 0.03, 1.0, 0.18, Scale::Log, fmt_time);
    });
    device_box(ui, "HATS", |ui| {
        widgets::knob(ui, "Decay", &mut p.hat_decay, 0.01, 0.5, 0.05, Scale::Log, fmt_time);
    });
    device_box(ui, "OUTPUT", |ui| {
        widgets::knob(ui, "Volume", &mut p.volume_db, -40.0, 6.0, -4.0, Scale::Linear, |v| {
            format!("{v:.1} dB")
        });
    });
    if p != before {
        if let Some(t) = app.project.track_mut(track) {
            t.instrument = Instrument::DrumKit(p);
        }
        app.send_instrument(track);
    }
    if let Some(n) = hit {
        let now = ui.input(|i| i.time);
        app.audition(track, n, now);
    }
}
