//! Top bar: file menu, tempo, time signature, transport, loop, view switch.

use eframe::egui::{self, pos2, vec2, Color32, RichText, Sense, Stroke, StrokeKind};
use tessera_engine::midi::edit::RecordQuantize;
use tessera_engine::midi::mapping::MapTarget;
use tessera_engine::time::Quantization;
use tessera_engine::Command;

use super::info_view::InfoExt;
use super::{theme, widgets};
use crate::app::{DetailMode, TesseraApp, View};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    // Narrow windows get the loop, punch, locator and launch controls on a
    // second row.
    let wide = ui.available_width() > 1700.0;
    ui.horizontal(|ui| {
        ui.menu_button("File", |ui| file_menu(app, ui));
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
        super::view_menu::menu(app, ui);
        super::chrome::help_menu(app, ui);
        ui.separator();

        // Tempo.
        let mut tempo = app.project.tempo;
        let r = ui.add(
            egui::DragValue::new(&mut tempo)
                .range(20.0..=999.0)
                .speed(0.2)
                .fixed_decimals(2)
                .suffix(" BPM"),
        )
        .info(
            "Tempo",
            "The song tempo in beats per minute (20 to 999). Drag up or down, or double-click to type a value. Scenes can change it when launched.",
        );
        app.tempo_editing = r.dragged() || r.has_focus();
        if r.changed() {
            app.set_tempo(tempo);
        }
        if ui
            .small_button("TAP")
            .info("Tap Tempo", "Click in time with the music, at least twice, to set the tempo.").clicked() {
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
            .info("Metronome", "Clicks on every beat while the transport runs, with an accent on the first beat of each bar.")
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
            .info(
                "Count-In",
                "Bars of metronome count-in before Arrangement Record starts: none, 1, 2 or 4. Click to change.",
            )
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
        ui.label(RichText::new(position).monospace().size(15.0).color(theme::text()))
            .info(
                "Song Position",
                "Where the Arrangement playhead is, in bars, beats and sixteenths. Click the Arrangement ruler to move it.",
            );

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
        if resp
            .info(
                "Play",
                "Starts playback from the start marker (Space). Shift + Space continues from where playback stopped.",
            )
            .clicked()
        {
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
            theme::gray(0x48),
            Stroke::NONE,
            StrokeKind::Inside,
        );
        widgets::stop_square(ui.painter(), rect.center(), 9.0, theme::text());
        if resp
            .info(
                "Stop",
                "Stops playback (Space). Click again while stopped to return the start marker to the beginning.",
            )
            .clicked()
        {
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
            .info(
                "Arrangement Record",
                "Records armed audio tracks into the Arrangement from the playhead (F9), after the count-in if one is set. Each loop pass makes a new take.",
            )
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
            let (rect, meter) = ui.allocate_exact_size(vec2(48.0, 14.0), Sense::hover());
            let l = app.engine.shared.master_meter_l.load();
            let r = app.engine.shared.master_meter_r.load();
            horizontal_meter(ui.painter(), rect, l, r);
            meter.info(
                "Master Level",
                "Peak level of the master output, left and right. It turns red when the signal clips at 0 dB.",
            );

            midi_buttons(app, ui);
            ui.separator();
            if widgets::toggle(ui, app.detail == DetailMode::Device, "Device", theme::ACCENT, 46.0)
                .info(
                    "Device View",
                    "Shows the selected track's instrument and effects in the detail area (Shift + Tab).",
                )
                .clicked()
            {
                app.detail = DetailMode::Device;
            }
            if widgets::toggle(ui, app.detail == DetailMode::Clip, "Clip", theme::ACCENT, 36.0)
                .info(
                    "Clip View",
                    "Shows the selected clip in the detail area: the piano roll for MIDI, the sample editor for audio (Shift + Tab).",
                )
                .clicked()
            {
                app.detail = DetailMode::Clip;
            }
            ui.separator();
            let r = widgets::toggle(ui, app.view == View::Arrangement, "Arrange", theme::ACCENT, 60.0).info(
                "Arrangement View",
                "The song as a timeline of clips and automation, left to right. Tab switches views. Drag a clip here to drop it in the Arrangement.",
            );
            if r.clicked() || dragging_clip_over(&r) {
                app.view = View::Arrangement;
            }
            let r = widgets::toggle(ui, app.view == View::Session, "Session", theme::ACCENT, 60.0).info(
                "Session View",
                "A grid of clips to launch and combine live, with scenes in rows and the mixer below. Tab switches views. Drag a clip here to drop it in a Session slot.",
            );
            if r.clicked() || dragging_clip_over(&r) {
                app.view = View::Session;
            }
            if widgets::toggle(ui, app.show_browser, "Browse", theme::ACCENT, 50.0)
                .info(
                    "Browser",
                    "Shows or hides the Browser of instruments, effects, plug-ins and samples on the left.",
                )
                .clicked()
            {
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
    if widgets::toggle(ui, lp, "Loop", theme::ACCENT, 40.0)
        .info(
            "Arrangement Loop",
            "Repeats the part of the Arrangement under the loop brace. Set it with the two boxes on the right or by dragging the brace above the ruler.",
        )
        .clicked()
    {
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
        .info(
            "Loop Start",
            "The bar where the Arrangement loop starts. Drag or double-click to type.",
        );
    let r2 = ui
        .add(
            egui::DragValue::new(&mut len_bars)
                .range(1.0..=256.0)
                .speed(0.05)
                .fixed_decimals(0),
        )
        .info("Loop Length", "Length of the Arrangement loop in bars.");
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
        .info(
            "Launch Quantization",
            "When launched clips and scenes start: right away (None) or on the next 1/8, 1/4, 1/2 note, bar, 2 or 4 bars.",
        );
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
    let r = widgets::toggle(ui, any_session, "◀ Arr", theme::ACCENT, 44.0).info(
        "Back to Arrangement",
        "Lights up when Session clips play over the Arrangement. Click to stop them and hear the Arrangement again on every track.",
    );
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
    if ui.button("MIDI Settings…").clicked() {
        app.show_midi_prefs = true;
        ui.close();
    }
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

/// MIDI Map Mode toggle, MIDI settings and the MIDI input activity light.
fn midi_buttons(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let learning = app.midi_learn.is_some();
    let r = widgets::toggle(ui, learning, "MIDI", theme::MIDI_MAP, 40.0).on_hover_text(
        "MIDI Map Mode (Ctrl+M): move a control, then a knob or pad on your controller.\n\
         Right-click for MIDI Settings (also in Preferences → Link/MIDI).",
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
        .info(
            "Automation Arm",
            "While on, moving a fader, knob or parameter during playback records automation into the playing Session clip or the Arrangement.",
        )
        .clicked()
    {
        app.automation.arm = !arm;
    }
    // Only shown when something was overridden by hand, like Ableton's
    // lit Re-Enable Automation button.
    if app.automation_overridden()
        && widgets::toggle(ui, true, "Re-Enable", theme::ACCENT, 64.0)
            .info(
                "Re-Enable Automation",
                "Some automated controls were moved by hand and stopped following their automation. Click to make them follow it again.",
            )
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
        let midi_clip = app.selected_clip.filter(|&r| app.clip(r).is_some_and(|c| c.is_midi()));
        if ui
            .add_enabled(
                midi_clip.is_some(),
                egui::Button::new("Quantize").shortcut_text("Ctrl+U"),
            )
            .on_hover_text("Quantize the selected MIDI clip's notes (or the selected notes) with the last settings.")
            .clicked()
        {
            app.quantize_command();
            ui.close();
        }
        if ui
            .add(egui::Button::new("Quantize Settings…").shortcut_text("Ctrl+Shift+U"))
            .clicked()
        {
            app.piano.quantize_open = true;
            ui.close();
        }
        ui.menu_button("Record Quantization", |ui| {
            for rq in RecordQuantize::ALL {
                if ui
                    .add(egui::Button::new(rq.label()).selected(app.prefs.record_quantize == rq))
                    .on_hover_text("Snaps the starts of MIDI notes as they are recorded, keeping their lengths.")
                    .clicked()
                {
                    app.prefs.record_quantize = rq;
                    app.save_prefs();
                    app.status = format!("Record Quantization: {}", rq.label());
                    ui.close();
                }
            }
        });
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
