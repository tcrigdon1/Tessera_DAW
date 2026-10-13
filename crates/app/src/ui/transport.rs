//! Top bar: file menu, tempo, time signature, transport, loop, view switch.

use eframe::egui::{self, pos2, vec2, Color32, RichText, Sense, Stroke, StrokeKind};
use tessera_engine::time::{format_position, Quantization};
use tessera_engine::Command;

use super::{theme, widgets};
use crate::app::{DetailMode, TesseraApp, View};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
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
        });
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

        // Count-in before Arrangement recording.
        let mut count_in = app.project.count_in_bars;
        egui::ComboBox::from_id_salt("count_in")
            .width(64.0)
            .selected_text(match count_in {
                0 => "No count".to_string(),
                1 => "1 Bar".to_string(),
                n => format!("{n} Bars"),
            })
            .show_ui(ui, |ui| {
                for (n, l) in [(0, "None"), (1, "1 Bar"), (2, "2 Bars"), (4, "4 Bars")] {
                    ui.selectable_value(&mut count_in, n, l);
                }
            })
            .response
            .on_hover_text("Count-in before recording");
        if count_in != app.project.count_in_bars {
            app.project.count_in_bars = count_in;
            app.send(Command::SetCountIn(count_in));
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
            app.send(Command::Play);
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
        ui.label(RichText::new("Quantize").color(theme::TEXT_DIM));
        let mut q = app.project.launch_quantization;
        egui::ComboBox::from_id_salt("quant")
            .selected_text(q.label())
            .width(70.0)
            .show_ui(ui, |ui| {
                for opt in Quantization::ALL {
                    ui.selectable_value(&mut q, opt, opt.label());
                }
            });
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
        let r = widgets::toggle(ui, any_session, "Back to Arr.", theme::ACCENT, 76.0)
            .on_hover_text("Return all tracks to Arrangement playback");
        if r.clicked() {
            app.send(Command::BackToArrangement);
        }

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
