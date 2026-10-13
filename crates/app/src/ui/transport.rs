//! Top bar: file menu, tempo, time signature, transport, loop, view switch.

use eframe::egui::{self, pos2, vec2, Color32, RichText, Sense, Stroke, StrokeKind};
use tessera_engine::midi::mapping::MapTarget;
use tessera_engine::time::Quantization;
use tessera_engine::Command;

use super::{theme, widgets};
use crate::app::{DetailMode, TesseraApp, View};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    // Narrow windows get the loop, punch, locator and launch controls on a
    // second row.
    let wide = ui.available_width() > 1700.0;
    ui.horizontal(|ui| {
        ui.menu_button("File", |ui| {
            if ui.button("New").clicked() {
                app.new_project();
                ui.close();
            }
            if ui.button("Open Demo Song").clicked() {
                app.load_demo();
                ui.close();
            }
            if ui.button("Open…").clicked() {
                ui.close();
                app.open_dialog();
            }
            if ui.button("Save").clicked() {
                ui.close();
                app.save();
            }
            if ui.button("Save As…").clicked() {
                ui.close();
                app.save_as();
            }
            ui.separator();
            if ui.button("Export Audio…").clicked() {
                ui.close();
                app.export_dialog();
            }
            ui.separator();
            if ui.button("MIDI Settings…").clicked() {
                app.show_midi_prefs = true;
                ui.close();
            }
        });
        edit_menu(app, ui);
        ui.menu_button("Create", |ui| {
            if ui.button("MIDI Track (Poly Synth)").clicked() {
                app.add_midi_track();
                ui.close();
            }
            if ui.button("MIDI Track (Drum Kit)").clicked() {
                app.add_drum_track();
                ui.close();
            }
            if ui.button("Audio Track").clicked() {
                app.add_audio_track();
                ui.close();
            }
            if ui.button("Return Track").clicked() {
                app.add_return_track(None);
                ui.close();
            }
            if ui.button("Group Track").clicked() {
                app.add_group_track();
                ui.close();
            }
            if ui.button("Scene").clicked() {
                app.label_edit("Insert Scene");
                app.project.add_scene();
                app.changed();
                ui.close();
            }
            if ui.button("Capture and Insert Scene").clicked() {
                app.capture_scene();
                ui.close();
            }
            ui.separator();
            if ui.button("Add Locator at Playhead").clicked() {
                let b = app.song_beat();
                app.add_locator_at(b);
                ui.close();
            }
        });
        ui.separator();

        // Tempo.
        let mut tempo = app.project.tempo;
        let r = ui.add(
            egui::DragValue::new(&mut tempo)
                .range(20.0..=999.0)
                .speed(0.2)
                .fixed_decimals(2)
                .suffix(" BPM"),
        );
        app.tempo_editing = r.dragged() || r.has_focus();
        if r.changed() {
            app.set_tempo(tempo);
        }
        if ui.small_button("TAP").on_hover_text("Tap tempo").clicked() {
            tap_tempo(app, ui);
        }

        let mut bpb = app.project.beats_per_bar;
        let mut unit = app.project.beat_unit;
        let r = ui
            .add(egui::DragValue::new(&mut bpb).range(1..=32).speed(0.05))
            .on_hover_text("Time signature (song start)");
        egui::ComboBox::from_id_salt("beat_unit")
            .selected_text(format!("/ {unit}"))
            .width(34.0)
            .show_ui(ui, |ui| {
                for d in [1, 2, 4, 8, 16] {
                    ui.selectable_value(&mut unit, d, format!("{d}"));
                }
            });
        if r.changed() || unit != app.project.beat_unit {
            app.label_edit("Time Signature");
            app.project.beats_per_bar = bpb;
            app.project.beat_unit = unit;
            app.send(Command::SetSignature(bpb, unit));
            app.changed();
        }

        let metro = app.project.metronome;
        if widgets::toggle(ui, metro, "●", theme::ACCENT, 22.0)
            .on_hover_text("Metronome")
            .clicked()
        {
            app.project.metronome = !metro;
            app.send(Command::SetMetronome(!metro));
        }
        ui.separator();

        // Count-in before Arrangement recording: click to cycle.
        let count_in = app.project.count_in_bars;
        let label = match count_in {
            0 => "Count".to_string(),
            n => format!("{n} Bar"),
        };
        if widgets::toggle(ui, count_in > 0, &label, theme::ACCENT, 44.0)
            .on_hover_text("Count-in before recording: none, 1, 2 or 4 bars (click to change)")
            .clicked()
        {
            let next = match count_in {
                0 => 1,
                1 => 2,
                2 => 4,
                _ => 0,
            };
            app.project.count_in_bars = next;
            app.send(Command::SetCountIn(next));
            app.touch();
        }
        ui.separator();

        // Position display (or the count-in countdown).
        let beat = app.song_beat();
        let count = app.engine.shared.record.count_in.load();
        let position = if count > 0.0 {
            format!("Count-in {}", count.ceil() as i64)
        } else {
            app.time_map().format_position(beat)
        };
        ui.label(RichText::new(position).monospace().size(15.0).color(theme::TEXT));

        // Play / stop.
        let playing = app.is_playing();
        let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 20.0), Sense::click());
        ui.painter().rect(
            rect,
            2.0,
            if playing {
                theme::dim(theme::PLAY, 0.5)
            } else {
                Color32::from_rgb(0x48, 0x48, 0x48)
            },
            Stroke::NONE,
            StrokeKind::Inside,
        );
        widgets::triangle(
            ui.painter(),
            rect.center(),
            11.0,
            if playing { theme::PLAY } else { theme::TEXT },
        );
        if resp.on_hover_text("Play (Space)").clicked() {
            if app.midi_learn.is_some() {
                app.learn_touch(MapTarget::Play);
            } else {
                app.send(Command::Play);
            }
        }
        let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 20.0), Sense::click());
        ui.painter().rect(
            rect,
            2.0,
            Color32::from_rgb(0x48, 0x48, 0x48),
            Stroke::NONE,
            StrokeKind::Inside,
        );
        widgets::stop_square(ui.painter(), rect.center(), 9.0, theme::TEXT);
        if resp.on_hover_text("Stop (press twice to return to start)").clicked() {
            if app.midi_learn.is_some() {
                app.learn_touch(MapTarget::Stop);
            } else {
                app.send(Command::Stop);
            }
        }
        // Arrangement record.
        let rec = app.arrangement_recording();
        let blink = (ui.input(|i| i.time) * 3.0).fract() < 0.5;
        let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 20.0), Sense::click());
        ui.painter().rect(
            rect,
            2.0,
            if rec {
                theme::dim(theme::ARM, 0.55)
            } else {
                Color32::from_rgb(0x48, 0x48, 0x48)
            },
            Stroke::NONE,
            StrokeKind::Inside,
        );
        let dot = if rec && (blink || app.is_playing()) {
            theme::ARM
        } else if resp.hovered() {
            Color32::from_rgb(0xff, 0x80, 0x80)
        } else {
            theme::TEXT
        };
        ui.painter().circle_filled(rect.center(), 5.0, dot);
        if resp
            .on_hover_text("Arrangement Record (F9): records armed audio tracks into the Arrangement")
            .clicked()
        {
            app.toggle_arrangement_record();
        }
        let r = widgets::toggle(ui, false, "+Rec", theme::ARM, 36.0)
            .on_hover_text("Session Record: armed tracks record into the selected scene (or the next free slot)");
        if r.clicked() {
            app.session_record();
        }
        let r = widgets::toggle(ui, false, "Capture", theme::ACCENT, 52.0)
            .on_hover_text("Capture MIDI (Ctrl+Shift+C): turns what you just played into a clip");
        if r.clicked() {
            let now = ui.input(|i| i.time);
            app.capture_midi(now);
        }
        let od = app.project.midi_overdub;
        if widgets::toggle(ui, od, "OVR", theme::ARM, 32.0)
            .on_hover_text("MIDI Arrangement Overdub: new MIDI recordings add to the notes already there")
            .clicked()
        {
            app.label_edit("MIDI Arrangement Overdub");
            app.project.midi_overdub = !od;
            app.touch();
        }

        if wide {
            song_controls(app, ui);
        }

        // Right side: master meter, view switch.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let (rect, _) = ui.allocate_exact_size(vec2(48.0, 14.0), Sense::hover());
            let l = app.engine.shared.master_meter_l.load();
            let r = app.engine.shared.master_meter_r.load();
            horizontal_meter(ui.painter(), rect, l, r);

            midi_buttons(app, ui);
            ui.separator();
            if widgets::toggle(ui, app.detail == DetailMode::Device, "Device", theme::ACCENT, 50.0).clicked() {
                app.detail = DetailMode::Device;
            }
            if widgets::toggle(ui, app.detail == DetailMode::Clip, "Clip", theme::ACCENT, 36.0).clicked() {
                app.detail = DetailMode::Clip;
            }
            ui.separator();
            let r = widgets::toggle(ui, app.view == View::Arrangement, "Arrangement", theme::ACCENT, 84.0)
                .on_hover_text("Tab (drag a clip here to drop it in the Arrangement)");
            if r.clicked() || dragging_clip_over(&r) {
                app.view = View::Arrangement;
            }
            let r = widgets::toggle(ui, app.view == View::Session, "Session", theme::ACCENT, 60.0)
                .on_hover_text("Tab (drag a clip here to drop it in a Session slot)");
            if r.clicked() || dragging_clip_over(&r) {
                app.view = View::Session;
            }
            if widgets::toggle(ui, app.show_browser, "Browser", theme::ACCENT, 56.0).clicked() {
                app.show_browser = !app.show_browser;
            }
        });
    });
    if !wide {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            song_controls(app, ui);
        });
    }
}

/// Loop and punch, locators, launch quantization, Back to Arrangement and
/// the automation buttons.
fn song_controls(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.separator();
    // Loop.
    let lp = app.project.loop_enabled;
    if widgets::toggle(ui, lp, "Loop", theme::ACCENT, 40.0).clicked() {
        app.project.loop_enabled = !lp;
        app.send_loop();
    }
    let mut start_bar = app.project.loop_start / app.project.beats_per_bar as f64 + 1.0;
    let mut len_bars = app.project.loop_length / app.project.beats_per_bar as f64;
    let r1 = ui
        .add(
            egui::DragValue::new(&mut start_bar)
                .range(1.0..=999.0)
                .speed(0.05)
                .fixed_decimals(0),
        )
        .on_hover_text("Loop start (bar)");
    let r2 = ui
        .add(
            egui::DragValue::new(&mut len_bars)
                .range(1.0..=256.0)
                .speed(0.05)
                .fixed_decimals(0),
        )
        .on_hover_text("Loop length (bars)");
    if r1.changed() || r2.changed() {
        let bpb = app.project.beats_per_bar as f64;
        app.project.loop_start = (start_bar.round() - 1.0) * bpb;
        app.project.loop_length = len_bars.round().max(1.0) * bpb;
        app.send_loop();
    }
    let (pin, pout) = (app.project.punch_in, app.project.punch_out);
    if widgets::toggle(ui, pin, "In", theme::ARM, 24.0)
        .on_hover_text("Punch In: Arrangement recording starts at the loop start")
        .clicked()
    {
        app.set_punch(!pin, pout);
    }
    if widgets::toggle(ui, pout, "Out", theme::ARM, 28.0)
        .on_hover_text("Punch Out: Arrangement recording stops at the loop end")
        .clicked()
    {
        app.set_punch(pin, !pout);
    }

    ui.separator();
    // Locators.
    if ui.small_button("◀").on_hover_text("Previous locator").clicked() {
        app.jump_locator(false);
    }
    if ui
        .small_button("Set")
        .on_hover_text("Add a locator at the playhead")
        .clicked()
    {
        let b = app.song_beat();
        app.add_locator_at(b);
    }
    if ui.small_button("▶").on_hover_text("Next locator").clicked() {
        app.jump_locator(true);
    }

    ui.separator();
    let mut q = app.project.launch_quantization;
    egui::ComboBox::from_id_salt("quant")
        .selected_text(q.label())
        .width(62.0)
        .show_ui(ui, |ui| {
            for opt in Quantization::ALL {
                ui.selectable_value(&mut q, opt, opt.label());
            }
        })
        .response
        .on_hover_text("Launch quantization");
    if q != app.project.launch_quantization {
        app.project.launch_quantization = q;
        app.send(Command::SetQuantization(q));
        app.touch();
    }

    let any_session = app
        .engine
        .shared
        .tracks
        .iter()
        .take(app.project.tracks.len())
        .any(|t| t.session_active.load(std::sync::atomic::Ordering::Relaxed));
    let r = widgets::toggle(ui, any_session, "Back to Arr.", theme::ACCENT, 72.0)
        .on_hover_text("Return all tracks to Arrangement playback");
    if r.clicked() {
        app.send(Command::BackToArrangement);
    }
    automation_buttons(app, ui);
}

/// A clip being dragged over a view button switches to that view, so clips
/// can travel between the Session and the Arrangement.
fn dragging_clip_over(r: &egui::Response) -> bool {
    r.contains_pointer() && egui::DragAndDrop::has_payload_of_type::<crate::workflow::ClipDrag>(&r.ctx)
}

/// MIDI Map Mode toggle, MIDI settings and the MIDI input activity light.
fn midi_buttons(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let learning = app.midi_learn.is_some();
    let r = widgets::toggle(ui, learning, "MIDI", theme::MIDI_MAP, 40.0).on_hover_text(
        "MIDI Map Mode (Ctrl+M): move a control, then a knob or pad on your controller.\n\
         Right-click for MIDI Settings (Ctrl+,).",
    );
    if r.clicked() {
        app.toggle_midi_learn();
    }
    if r.secondary_clicked() {
        app.show_midi_prefs = !app.show_midi_prefs;
    }
    // A dot in the button's corner lights up whenever any MIDI arrives.
    let now = ui.input(|i| i.time);
    let recent = app.midi.activity.values().any(|t| now - t < 0.15);
    if recent {
        ui.painter()
            .circle_filled(r.rect.right_top() + vec2(-4.0, 4.0), 2.5, theme::PLAY);
    }
}

/// Automation Arm and Re-Enable Automation.
fn automation_buttons(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.separator();
    let arm = app.automation.arm;
    if widgets::toggle(ui, arm, "Auto", theme::ARM, 40.0)
        .on_hover_text("Automation Arm: moving a control while playing records automation")
        .clicked()
    {
        app.automation.arm = !arm;
    }
    // Only shown when something was overridden by hand, like Ableton's
    // lit Re-Enable Automation button.
    if app.automation_overridden()
        && widgets::toggle(ui, true, "Re-Enable", theme::ACCENT, 64.0)
            .on_hover_text("Some automation was overridden by hand. Click to follow it again.")
            .clicked()
    {
        app.reenable_automation();
    }
    if app.tempo_overridden()
        && widgets::toggle(ui, true, "Tempo", theme::ACCENT, 48.0)
            .on_hover_text("The tempo was changed by hand. Click to follow the Song Tempo automation again.")
            .clicked()
    {
        app.follow_tempo_automation();
    }
}

type EditItem = (&'static str, &'static str, fn(&mut TesseraApp));

/// Edit menu: undo history, track and clip commands, Groove Pool.
fn edit_menu(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.menu_button("Edit", |ui| {
        let undo = app.work.history.undo_label().map(|l| format!("Undo {l}"));
        if ui
            .add_enabled(
                undo.is_some(),
                egui::Button::new(undo.unwrap_or("Undo".into())).shortcut_text("Ctrl+Z"),
            )
            .clicked()
        {
            app.undo();
            ui.close();
        }
        let redo = app.work.history.redo_label().map(|l| format!("Redo {l}"));
        if ui
            .add_enabled(
                redo.is_some(),
                egui::Button::new(redo.unwrap_or("Redo".into())).shortcut_text("Ctrl+Shift+Z"),
            )
            .clicked()
        {
            app.redo();
            ui.close();
        }
        if ui.button("History…").clicked() {
            app.work.show_history = true;
            ui.close();
        }
        ui.separator();
        let items: [EditItem; 6] = [
            ("Split", "Ctrl+E", TesseraApp::split),
            ("Consolidate", "Ctrl+J", TesseraApp::consolidate),
            ("Group Tracks", "Ctrl+G", TesseraApp::group_selected),
            ("Duplicate Tracks", "", TesseraApp::duplicate_tracks),
            ("Delete Tracks", "", TesseraApp::delete_tracks),
            ("Capture and Insert Scene", "Ctrl+Shift+I", TesseraApp::capture_scene),
        ];
        for (label, keys, action) in items {
            if ui.add(egui::Button::new(label).shortcut_text(keys)).clicked() {
                action(app);
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Groove Pool…").clicked() {
            app.work.show_grooves = true;
            ui.close();
        }
        if ui
            .add(egui::Button::new("Song Tempo Lane").selected(app.work.show_tempo_lane))
            .clicked()
        {
            app.work.show_tempo_lane = !app.work.show_tempo_lane;
            app.view = View::Arrangement;
            ui.close();
        }
    });
}

fn horizontal_meter(p: &egui::Painter, rect: egui::Rect, l: f32, r: f32) {
    p.rect_filled(rect, 1.0, Color32::from_rgb(0x1c, 0x1c, 0x1c));
    let h = (rect.height() - 1.0) / 2.0;
    for (i, level) in [l, r].into_iter().enumerate() {
        let y0 = rect.top() + i as f32 * (h + 1.0);
        let db = if level > 0.0 { 20.0 * level.log10() } else { -70.0 };
        let pos = ((db + 60.0) / 66.0).clamp(0.0, 1.0);
        let color = if level >= 1.0 {
            theme::METER_HIGH
        } else {
            theme::METER_LOW
        };
        p.rect_filled(
            egui::Rect::from_min_max(pos2(rect.left(), y0), pos2(rect.left() + pos * rect.width(), y0 + h)),
            0.0,
            color,
        );
    }
}

fn tap_tempo(app: &mut TesseraApp, ui: &egui::Ui) {
    let now = ui.input(|i| i.time);
    let id = egui::Id::new("tap_tempo");
    let mut taps: Vec<f64> = ui.data_mut(|d| d.get_temp(id).unwrap_or_default());
    taps.retain(|t| now - t < 2.5);
    taps.push(now);
    if taps.len() >= 2 {
        let span = taps.last().unwrap() - taps.first().unwrap();
        let bpm = 60.0 * (taps.len() - 1) as f64 / span;
        app.set_tempo(bpm.round());
    }
    if taps.len() > 8 {
        taps.remove(0);
    }
    ui.data_mut(|d| d.insert_temp(id, taps));
}
