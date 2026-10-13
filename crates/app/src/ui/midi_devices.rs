//! MIDI effects in the Device view (left of the instrument) and a MIDI
//! track's input routing.

use eframe::egui::{self, vec2, Color32, RichText, Sense};
use tessera_engine::fx::rack::Rack;
use tessera_engine::midi_fx::{MidiDevice, MidiDeviceKind, MidiDeviceParams};
use tessera_engine::model::{Id, MidiFrom, MidiInput, Monitor};

use super::devices::param_control;
use super::racks::{self, RackAct};
use super::{theme, widgets};
use crate::app::TesseraApp;
use crate::racks::RackRef;

/// MIDI effect panels share the audio effects' look with a different accent.
const MIDI_ACCENT: Color32 = Color32::from_rgb(0x7f, 0xc8, 0xff);
fn panel_bg() -> Color32 {
    theme::gray(0x2b)
}
fn panel_off() -> Color32 {
    theme::gray(0x26)
}

enum Act {
    Param(Id, usize, f32),
    On(Id, bool),
    Move(Id, i32),
    Remove(Id),
    Reset(Id),
    AddToChain(Id, MidiDeviceKind),
    Rack(Vec<RackAct>),
}

/// "MIDI From", channel and monitor controls for the Device view header.
pub fn input_controls(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let Some(t) = app.project.track(track) else { return };
    let mut input: MidiInput = t.midi_input.clone();
    let mut monitor = t.input.monitor;
    let before = (input.clone(), monitor);
    ui.label(RichText::new("MIDI From").small().color(theme::text_dim()));
    let label = match &input.from {
        MidiFrom::AllIns => "All Ins".to_string(),
        MidiFrom::Port(p) => p.clone(),
        MidiFrom::None => "No Input".to_string(),
    };
    egui::ComboBox::from_id_salt(("midi_from", track))
        .width(90.0)
        .selected_text(label)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut input.from, MidiFrom::AllIns, "All Ins");
            for p in &app.midi.ports {
                ui.selectable_value(&mut input.from, MidiFrom::Port(p.clone()), p);
            }
            ui.selectable_value(&mut input.from, MidiFrom::None, "No Input");
        });
    egui::ComboBox::from_id_salt(("midi_ch", track))
        .width(58.0)
        .selected_text(match input.channel {
            None => "All Ch".to_string(),
            Some(c) => format!("Ch {}", c + 1),
        })
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut input.channel, None, "All Channels");
            for c in 0..16u8 {
                ui.selectable_value(&mut input.channel, Some(c), format!("Ch {}", c + 1));
            }
        });
    ui.label(RichText::new("Monitor").small().color(theme::text_dim()));
    for m in [Monitor::In, Monitor::Auto, Monitor::Off] {
        let tip = match m {
            Monitor::In => "Always play incoming MIDI",
            Monitor::Auto => "Play incoming MIDI when armed, or when selected and no track is armed",
            Monitor::Off => "Ignore incoming MIDI",
        };
        if widgets::toggle(ui, monitor == m, m.label(), theme::ACCENT, 34.0)
            .on_hover_text(tip)
            .clicked()
        {
            monitor = m;
        }
    }
    if (input.clone(), monitor) != before {
        if monitor != before.1 {
            if let Some(t) = app.project.track_mut(track) {
                t.input.monitor = monitor;
            }
            app.send_track_input(track);
        }
        app.set_midi_input(track, input);
    }
}

/// The "+ MIDI Effect" menu.
pub fn add_menu(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let mut add = None;
    ui.menu_button("+ MIDI Effect", |ui| {
        for kind in MidiDeviceKind::ALL {
            if ui.button(kind.name()).on_hover_text(kind.description()).clicked() {
                add = Some(kind);
                ui.close();
            }
        }
    });
    if let Some(kind) = add {
        app.add_midi_device(track, kind);
    }
}

/// Draws the track's MIDI effects, left to right.
pub fn chain(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let devices: Vec<MidiDevice> = app
        .project
        .track(track)
        .map(|t| t.midi_devices.clone())
        .unwrap_or_default();
    let rack = app.project.track(track).and_then(|t| t.midi_rack.clone());
    let mut acts = Vec::new();
    for (i, d) in devices.iter().enumerate() {
        panel(ui, d, i, devices.len(), rack.as_ref(), &mut acts);
        if let (MidiDeviceParams::Rack(_), Some(r)) = (d.params, rack.as_ref()) {
            if r.show_chains {
                if let Some(c) = r.chains.get(r.selected_chain) {
                    egui::Frame::new()
                        .fill(Color32::from_rgb(0x22, 0x22, 0x22))
                        .corner_radius(3.0)
                        .inner_margin(egui::Margin::same(4))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&c.name).small().color(theme::TEXT_DIM));
                                ui.horizontal_top(|ui| {
                                    for (j, cd) in c.midi_devices.iter().enumerate() {
                                        panel(ui, cd, j, c.midi_devices.len(), None, &mut acts);
                                    }
                                    ui.menu_button(RichText::new("+").small(), |ui| {
                                        for kind in MidiDeviceKind::ALL {
                                            if kind == MidiDeviceKind::Rack {
                                                continue;
                                            }
                                            if ui.button(kind.name()).on_hover_text(kind.description()).clicked() {
                                                acts.push(Act::AddToChain(c.id, kind));
                                                ui.close();
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text("Add a MIDI effect to this chain");
                                });
                            });
                        });
                }
            }
        }
    }
    let all: Vec<MidiDevice> = devices
        .iter()
        .copied()
        .chain(
            rack.iter()
                .flat_map(|r| r.chains.iter().flat_map(|c| c.midi_devices.iter().copied())),
        )
        .collect();
    for a in acts {
        match a {
            Act::Param(d, i, v) => app.set_midi_device_param(track, d, i, v),
            Act::On(d, on) => app.set_midi_device_on(track, d, on),
            Act::Move(d, delta) => app.move_midi_device(track, d, delta),
            Act::Remove(d) => app.remove_midi_device(track, d),
            Act::AddToChain(c, kind) => app.add_midi_device_to_chain(track, c, kind),
            Act::Rack(ra) => racks::apply(app, RackRef::Midi(track), ra),
            Act::Reset(d) => {
                if let Some(kind) = all.iter().find(|x| x.id == d).map(|x| x.kind()) {
                    app.set_midi_device_params(track, d, kind.default_params());
                }
            }
        }
    }
}

fn panel(ui: &mut egui::Ui, d: &MidiDevice, index: usize, count: usize, rack: Option<&Rack>, acts: &mut Vec<Act>) {
    let id = d.id;
    egui::Frame::new()
        .fill(if d.on { panel_bg() } else { panel_off() })
        .corner_radius(3.0)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    if widgets::toggle(ui, d.on, "", MIDI_ACCENT, 14.0)
                        .on_hover_text("Device on/off")
                        .clicked()
                    {
                        acts.push(Act::On(id, !d.on));
                    }
                    let title = RichText::new(d.kind().name().to_uppercase()).small().color(if d.on {
                        MIDI_ACCENT
                    } else {
                        theme::text_dim()
                    });
                    let t = ui.add(egui::Label::new(title).sense(Sense::click()));
                    t.on_hover_text(d.kind().description()).context_menu(|ui| {
                        if ui.button("Reset to defaults").clicked() {
                            acts.push(Act::Reset(id));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            acts.push(Act::Remove(id));
                            ui.close();
                        }
                    });
                    ui.add_space(8.0);
                    let small = |ui: &mut egui::Ui, text: &str, enabled: bool, tip: &str| {
                        ui.add_enabled(enabled, egui::Button::new(RichText::new(text).small()).small())
                            .on_hover_text(tip)
                            .clicked()
                    };
                    if small(ui, "<", index > 0, "Move left") {
                        acts.push(Act::Move(id, -1));
                    }
                    if small(ui, ">", index + 1 < count, "Move right") {
                        acts.push(Act::Move(id, 1));
                    }
                    if small(ui, "x", true, "Delete device") {
                        acts.push(Act::Remove(id));
                    }
                    if let (MidiDeviceParams::Rack(_), Some(r)) = (d.params, rack) {
                        ui.add_space(6.0);
                        let mut ra = Vec::new();
                        racks::view_toggles(ui, r, &mut ra);
                        if !ra.is_empty() {
                            acts.push(Act::Rack(ra));
                        }
                    }
                });
                ui.add_space(2.0);
                if let (MidiDeviceParams::Rack(p), Some(r)) = (d.params, rack) {
                    let mut ra = Vec::new();
                    ui.horizontal_top(|ui| {
                        if r.show_macros {
                            racks::macros(ui, id, r, &p, &mut ra);
                        }
                        if r.show_chains {
                            racks::chain_list(ui, id, r, false, &mut ra);
                        }
                        if r.show_zones {
                            racks::zone_editor(ui, id, r, Some(&p), true, &mut ra);
                        }
                    });
                    if !ra.is_empty() {
                        acts.push(Act::Rack(ra));
                    }
                    return;
                }
                let info = d.params.info();
                let visible: Vec<usize> = (0..info.len()).filter(|&i| d.params.is_relevant(i)).collect();
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
                    for col in visible.chunks(2) {
                        ui.vertical(|ui| {
                            for &i in col {
                                if let Some(v) = param_control(ui, (id, i), &info[i], d.params.get(i)) {
                                    acts.push(Act::Param(id, i, v));
                                }
                            }
                        });
                    }
                });
            });
        });
}
