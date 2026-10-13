//! The MIDI Settings window (input ports and controller mappings) and the
//! MIDI Map Mode overlay.

use eframe::egui::{self, vec2, Color32, RichText};
use tessera_engine::midi::{note_name, MidiKind};

use super::theme;
use crate::app::TesseraApp;

pub fn draw(app: &mut TesseraApp, ctx: &egui::Context) {
    if app.midi_learn.is_some() {
        map_mode_overlay(app, ctx);
    }
    if !app.show_midi_prefs {
        return;
    }
    let mut open = true;
    egui::Window::new("MIDI Settings")
        .open(&mut open)
        .default_width(460.0)
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            ports(app, ui);
            ui.add_space(8.0);
            ui.separator();
            mappings(app, ui);
        });
    if !open {
        app.show_midi_prefs = false;
    }
}

fn ports(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Input Ports").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Rescan").on_hover_text("Look for new MIDI devices").clicked() {
                app.midi.rescan();
            }
        });
    });
    ui.label(
        RichText::new(
            "Track: notes play armed MIDI tracks (or the selected track when none is armed). \
             Remote: knobs, faders and pads can be mapped to controls.",
        )
        .small()
        .color(theme::TEXT_DIM),
    );
    if let Some(e) = &app.midi.error {
        ui.label(RichText::new(e).small().color(theme::METER_HIGH));
    }
    if app.midi.ports.is_empty() {
        ui.label(RichText::new("No MIDI input devices found. Connect one and click Rescan.").color(theme::TEXT_DIM));
        return;
    }
    let now = ui.input(|i| i.time);
    let mut change: Option<(String, bool, bool)> = None;
    egui::Grid::new("midi_ports")
        .num_columns(4)
        .spacing(vec2(12.0, 4.0))
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Port").small().color(theme::TEXT_DIM));
            ui.label(RichText::new("Track").small().color(theme::TEXT_DIM));
            ui.label(RichText::new("Remote").small().color(theme::TEXT_DIM));
            ui.label("");
            ui.end_row();
            for name in app.midi.ports.clone() {
                let (mut track, mut remote) = app
                    .midi
                    .port_setting(&name)
                    .map(|p| (p.track, p.remote))
                    .unwrap_or((true, true));
                ui.label(&name);
                let a = ui.checkbox(&mut track, "").changed();
                let b = ui.checkbox(&mut remote, "").changed();
                if a || b {
                    change = Some((name.clone(), track, remote));
                }
                let id = tessera_engine::midi::port_id(&name);
                let active = app.midi.activity.get(&id).is_some_and(|t| now - t < 0.2);
                let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    rect.center(),
                    4.0,
                    if active {
                        theme::PLAY
                    } else {
                        Color32::from_rgb(0x40, 0x40, 0x40)
                    },
                );
                ui.end_row();
            }
        });
    if let Some((name, track, remote)) = change {
        app.midi.set_port(&name, track, remote);
    }
    if let Some(m) = app.midi.last {
        let what = match m.kind() {
            MidiKind::NoteOn { pitch, velocity } => format!("Note On {} vel {velocity}", note_name(pitch)),
            MidiKind::NoteOff { pitch } => format!("Note Off {}", note_name(pitch)),
            MidiKind::Cc { cc, value } => format!("CC {cc} = {value}"),
            MidiKind::PitchBend(v) => format!("Pitch Bend {v}"),
            MidiKind::Other => format!("Status {:02X}", m.status),
        };
        ui.label(
            RichText::new(format!("Last message: Ch {} {what}", m.channel() + 1))
                .small()
                .color(theme::TEXT_DIM),
        );
    }
}

fn mappings(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("MIDI Mappings").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let learning = app.midi_learn.is_some();
            let label = if learning { "Exit Map Mode" } else { "Map Mode (Ctrl+M)" };
            if ui.button(label).clicked() {
                app.toggle_midi_learn();
            }
        });
    });
    if app.project.midi_mappings.is_empty() {
        ui.label(
            RichText::new(
                "No mappings yet. Turn on Map Mode, move a control on screen (a knob, fader, \
                 button or clip), then move a knob or press a pad on your controller.",
            )
            .small()
            .color(theme::TEXT_DIM),
        );
        return;
    }
    let mut remove: Option<usize> = None;
    let mut edits: Vec<(usize, f32, f32)> = Vec::new();
    egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
        egui::Grid::new("midi_maps")
            .num_columns(5)
            .spacing(vec2(10.0, 4.0))
            .striped(true)
            .show(ui, |ui| {
                for h in ["Controller", "Control", "Min", "Max", ""] {
                    ui.label(RichText::new(h).small().color(theme::TEXT_DIM));
                }
                ui.end_row();
                let maps = app.project.midi_mappings.clone();
                for (i, m) in maps.iter().enumerate() {
                    ui.label(m.source.label());
                    ui.label(app.target_name(m.target));
                    let (mut lo, mut hi) = (m.min * 100.0, m.max * 100.0);
                    let button = m.target.is_button();
                    let a = ui
                        .add_enabled(
                            !button,
                            egui::DragValue::new(&mut lo).range(0.0..=100.0).speed(0.5).suffix(" %"),
                        )
                        .changed();
                    let b = ui
                        .add_enabled(
                            !button,
                            egui::DragValue::new(&mut hi).range(0.0..=100.0).speed(0.5).suffix(" %"),
                        )
                        .changed();
                    if a || b {
                        edits.push((i, lo / 100.0, hi / 100.0));
                    }
                    if ui.small_button("x").on_hover_text("Delete mapping").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
    });
    for (i, lo, hi) in edits {
        if let Some(m) = app.project.midi_mappings.get_mut(i) {
            m.min = lo;
            m.max = hi;
            app.touch();
        }
    }
    if let Some(i) = remove {
        app.project.midi_mappings.remove(i);
        app.touch();
    }
    if ui.button("Delete All Mappings").clicked() {
        app.project.midi_mappings.clear();
        app.touch();
    }
}

/// Tints the window and explains what to do while MIDI Map Mode is on.
fn map_mode_overlay(app: &TesseraApp, ctx: &egui::Context) {
    let rect = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("midi_map")));
    let tint = Color32::from_rgba_unmultiplied(0x8a, 0x6c, 0xff, 22);
    painter.rect_filled(rect, 0.0, tint);
    painter.rect_stroke(
        rect.shrink(1.0),
        0.0,
        egui::Stroke::new(2.0, theme::MIDI_MAP),
        egui::StrokeKind::Inside,
    );
    let text = match app.midi_learn.as_ref().and_then(|l| l.target) {
        Some(t) => format!(
            "MIDI Map Mode: now move a knob or press a pad on your controller for {}",
            app.target_name(t)
        ),
        None => "MIDI Map Mode: move a control or click a clip, then a knob or pad on your controller (Ctrl+M to exit)"
            .to_string(),
    };
    let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), Color32::WHITE);
    let pos = egui::pos2(rect.center().x - galley.size().x / 2.0, rect.top() + 34.0);
    let bg = egui::Rect::from_min_size(pos, galley.size()).expand2(vec2(10.0, 5.0));
    painter.rect_filled(bg, 4.0, theme::dim(theme::MIDI_MAP, 0.7));
    painter.galley(pos, galley, Color32::WHITE);
}
