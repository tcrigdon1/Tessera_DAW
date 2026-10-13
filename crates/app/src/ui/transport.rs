//! Top bar: file menu, tempo, time signature, transport, loop, view switch.

use eframe::egui::{self, pos2, vec2, Color32, RichText, Sense, Stroke, StrokeKind};
use tessera_engine::time::{format_position, Quantization};
use tessera_engine::Command;

use super::{theme, widgets};
use crate::app::{DetailMode, TesseraApp, View};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.menu_button("File", |ui| file_menu(app, ui));
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
            if ui.button("Scene").clicked() {
                app.project.add_scene();
                app.changed();
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
        let r = ui.add(egui::DragValue::new(&mut bpb).range(1..=32).speed(0.05));
        ui.label(format!("/ {}", app.project.beat_unit));
        if r.changed() {
            app.project.beats_per_bar = bpb;
            app.send(Command::SetTimeSignature(bpb));
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
            app.dirty = true;
        }
        ui.separator();

        // Position display (or the count-in countdown).
        let beat = app.song_beat();
        let count = app.engine.shared.record.count_in.load();
        let position = if count > 0.0 {
            format!("Count-in {}", count.ceil() as i64)
        } else {
            format_position(beat, app.project.beats_per_bar)
        };
        ui.label(RichText::new(position).monospace().size(15.0).color(theme::text()));

        // Play / stop.
        let playing = app.is_playing();
        let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 20.0), Sense::click());
        ui.painter().rect(
            rect,
            2.0,
            if playing {
                theme::dim(theme::PLAY, 0.5)
            } else {
                theme::gray(0x48)
            },
            Stroke::NONE,
            StrokeKind::Inside,
        );
        widgets::triangle(
            ui.painter(),
            rect.center(),
            11.0,
            if playing { theme::PLAY } else { theme::text() },
        );
        if resp.on_hover_text("Play (Space)").clicked() {
            app.send(Command::Play);
        }
        let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 20.0), Sense::click());
        ui.painter()
            .rect(rect, 2.0, theme::gray(0x48), Stroke::NONE, StrokeKind::Inside);
        widgets::stop_square(ui.painter(), rect.center(), 9.0, theme::text());
        if resp.on_hover_text("Stop (press twice to return to start)").clicked() {
            app.send(Command::Stop);
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
                theme::gray(0x48)
            },
            Stroke::NONE,
            StrokeKind::Inside,
        );
        let dot = if rec && (blink || app.is_playing()) {
            theme::ARM
        } else if resp.hovered() {
            Color32::from_rgb(0xff, 0x80, 0x80)
        } else {
            theme::text()
        };
        ui.painter().circle_filled(rect.center(), 5.0, dot);
        if resp
            .on_hover_text("Arrangement Record (F9): records armed audio tracks into the Arrangement")
            .clicked()
        {
            app.toggle_arrangement_record();
        }

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
            app.dirty = true;
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

        // Right side: master meter, view switch.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(60.0, 14.0), Sense::hover());
            let l = app.engine.shared.master_meter_l.load();
            let r = app.engine.shared.master_meter_r.load();
            horizontal_meter(ui.painter(), rect, l, r);

            ui.separator();
            if widgets::toggle(ui, app.detail == DetailMode::Device, "Device", theme::ACCENT, 50.0).clicked() {
                app.detail = DetailMode::Device;
            }
            if widgets::toggle(ui, app.detail == DetailMode::Clip, "Clip", theme::ACCENT, 36.0).clicked() {
                app.detail = DetailMode::Clip;
            }
            ui.separator();
            if widgets::toggle(ui, app.view == View::Arrangement, "Arrangement", theme::ACCENT, 84.0)
                .on_hover_text("Tab")
                .clicked()
            {
                app.view = View::Arrangement;
            }
            if widgets::toggle(ui, app.view == View::Session, "Session", theme::ACCENT, 60.0)
                .on_hover_text("Tab")
                .clicked()
            {
                app.view = View::Session;
            }
            if widgets::toggle(ui, app.show_browser, "Browser", theme::ACCENT, 56.0).clicked() {
                app.show_browser = !app.show_browser;
            }
        });
    });
}

fn file_menu(app: &mut TesseraApp, ui: &mut egui::Ui) {
    use crate::app::PendingAction;
    let cmd = |k: &str| format!("{}{k}", if cfg!(target_os = "macos") { "Cmd+" } else { "Ctrl+" });
    if ui.add(egui::Button::new("New Set").shortcut_text(cmd("N"))).clicked() {
        ui.close();
        app.request(PendingAction::New);
    }
    let templates = app
        .prefs
        .templates_dir()
        .map(|d| crate::project_files::list_templates(&d))
        .unwrap_or_default();
    ui.menu_button("New from Template", |ui| {
        if templates.is_empty() {
            ui.label(RichText::new("No templates yet: use Save as Template…").color(theme::text_dim()));
        }
        for t in &templates {
            if ui.button(crate::browser::file_label(t)).clicked() {
                ui.close();
                app.request(PendingAction::NewFromTemplate(t.clone()));
            }
        }
    });
    if ui.add(egui::Button::new("Open…").shortcut_text(cmd("O"))).clicked() {
        ui.close();
        app.request(PendingAction::Open(None));
    }
    let recent = app.prefs.recent.clone();
    ui.add_enabled_ui(!recent.is_empty(), |ui| {
        ui.menu_button("Open Recent", |ui| {
            for p in &recent {
                let r = ui
                    .button(crate::browser::file_label(p))
                    .on_hover_text(p.display().to_string());
                if r.clicked() {
                    ui.close();
                    app.request(PendingAction::Open(Some(p.clone())));
                }
            }
            ui.separator();
            if ui.button("Clear Recent").clicked() {
                app.prefs.recent.clear();
                app.save_prefs();
                ui.close();
            }
        });
    });
    if ui.button("Open Demo Song").clicked() {
        ui.close();
        app.request(PendingAction::OpenDemo);
    }
    ui.separator();
    if ui.add(egui::Button::new("Save").shortcut_text(cmd("S"))).clicked() {
        ui.close();
        app.save();
    }
    if ui
        .add(egui::Button::new("Save As…").shortcut_text(cmd("Shift+S")))
        .clicked()
    {
        ui.close();
        app.save_as();
    }
    if ui.button("Save a Copy as Template…").clicked() {
        ui.close();
        app.file_dialogs.template_name = Some(app.project.name.clone());
    }
    if ui
        .button("Save as Default Set")
        .on_hover_text("New sets will start from this one")
        .clicked()
    {
        ui.close();
        app.save_as_default_set();
    }
    if ui
        .button("Collect All and Save")
        .on_hover_text("Copy every audio file the set uses into its project folder, then save")
        .clicked()
    {
        ui.close();
        app.collect_all_and_save();
    }
    ui.separator();
    if ui.button("Import MIDI File…").clicked() {
        ui.close();
        app.import_midi_dialog();
    }
    if ui
        .add(egui::Button::new("Export Audio…").shortcut_text(cmd("Shift+R")))
        .clicked()
    {
        ui.close();
        app.export_dialog();
    }
    if ui
        .add(egui::Button::new("Export MIDI Clip…").shortcut_text(cmd("Shift+E")))
        .clicked()
    {
        ui.close();
        app.export_midi_clip_dialog();
    }
    ui.separator();
    if ui
        .add(egui::Button::new("Preferences…").shortcut_text(cmd(",")))
        .clicked()
    {
        ui.close();
        app.prefs_ui.show(None);
    }
    if ui.add(egui::Button::new("Quit").shortcut_text(cmd("Q"))).clicked() {
        ui.close();
        app.request(PendingAction::Quit);
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
}

fn horizontal_meter(p: &egui::Painter, rect: egui::Rect, l: f32, r: f32) {
    p.rect_filled(rect, 1.0, theme::gray(0x1c));
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
