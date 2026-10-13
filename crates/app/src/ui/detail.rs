//! Detail view at the bottom: the clip editor (piano roll / waveform) or the
//! selected track's device.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};
use tessera_engine::dsp::drums::{self, DrumKitParams};
use tessera_engine::dsp::synth::{SynthParams, Waveform};
use tessera_engine::model::{ClipContent, Id, Instrument, Note, TrackKind};
use tessera_engine::Command;

use super::widgets::{self, Scale};
use super::{session, theme};
use crate::app::{ClipRef, DetailMode, NoteDrag, TesseraApp};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    match app.detail {
        DetailMode::Clip => clip_view(app, ui),
        DetailMode::Device => device_view(app, ui),
    }
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.centered_and_justified(|ui| {
        ui.label(egui::RichText::new(text).color(theme::TEXT_DIM));
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
                clip_properties(app, ui, r);
            },
        );
        ui.separator();
        if is_midi {
            piano_roll(app, ui, r);
        } else {
            waveform(app, ui, r);
        }
    });
}

fn clip_properties(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let mut changed = false;
    let mut launch = false;
    let mut to_arr = false;
    let bpb = app.project.beats_per_bar as f64;
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
            ui.label("Gain");
            changed |= ui
                .add(
                    egui::DragValue::new(&mut clip.gain_db)
                        .range(-70.0..=24.0)
                        .speed(0.1)
                        .suffix(" dB"),
                )
                .changed();
            ui.end_row();
        });
    if let Some(notes) = clip.notes_mut() {
        ui.horizontal_wrapped(|ui| {
            if ui
                .small_button("Quantize")
                .on_hover_text("Snap note starts to 1/16")
                .clicked()
            {
                for n in notes.iter_mut() {
                    n.start = (n.start * 4.0).round() / 4.0;
                }
                changed = true;
            }
            if ui.small_button("Clear").clicked() {
                notes.clear();
                changed = true;
            }
        });
        let len = clip.length;
        if ui
            .small_button("Duplicate Loop")
            .on_hover_text("Double the clip and copy its notes")
            .clicked()
        {
            if let Some(notes) = clip.notes_mut() {
                let copy: Vec<Note> = notes
                    .iter()
                    .map(|n| Note {
                        start: n.start + len,
                        ..*n
                    })
                    .collect();
                notes.extend(copy);
            }
            clip.length = len * 2.0;
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
    if let ClipRef::Slot { track, scene } = r {
        if launch {
            app.send(Command::LaunchClip { track, slot: scene });
        }
        if to_arr {
            app.slot_to_arrangement(track, scene);
        }
    }
}

const ROW_H: f32 = 11.0;

fn piano_roll(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let track_id = r.track();
    let is_drums = matches!(
        app.project.track(track_id).map(|t| t.instrument),
        Some(Instrument::DrumKit(_))
    );
    let area = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(area, Sense::hover());
    let _ = resp;
    let key_w = if is_drums { 78.0 } else { 44.0 };
    let grid_rect = Rect::from_min_max(pos2(area.left() + key_w, area.top()), area.max);
    let Some(clip) = app.clip(r) else { return };
    let len = clip.length.max(0.25);
    let color = theme::rgb(clip.color);
    let notes: Vec<Note> = clip.notes().to_vec();

    // Rows: pitches from top to bottom.
    let rows: Vec<u8> = if is_drums {
        drums::PADS.iter().rev().map(|(p, _)| *p).collect()
    } else {
        if app.piano.centred_on != Some(r) {
            app.piano.centred_on = Some(r);
            let top = notes.iter().map(|n| n.pitch as i32).max().unwrap_or(72) + 6;
            app.piano.top_pitch = top.clamp(24, 127);
        }
        if ui.rect_contains_pointer(area) {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            if dy.abs() > 0.0 {
                app.piano.top_pitch = (app.piano.top_pitch + (dy / ROW_H).round() as i32).clamp(12, 127);
            }
        }
        let count = (area.height() / ROW_H).floor() as i32;
        (0..count)
            .map(|i| app.piano.top_pitch - i)
            .filter(|p| (0..=127).contains(p))
            .map(|p| p as u8)
            .collect()
    };
    if rows.is_empty() {
        return;
    }
    let row_h = if is_drums {
        (area.height() / rows.len() as f32).min(24.0)
    } else {
        ROW_H
    };
    let row_of = |pitch: u8| rows.iter().position(|&p| p == pitch);
    let ppb = grid_rect.width() / len as f32;
    let x_of = |b: f64| grid_rect.left() + b as f32 * ppb;
    let beat_of = |x: f32| ((x - grid_rect.left()) / ppb) as f64;

    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme::BG_VIEW);
    // Rows and keys.
    for (i, &pitch) in rows.iter().enumerate() {
        let y = area.top() + i as f32 * row_h;
        let black = matches!(pitch % 12, 1 | 3 | 6 | 8 | 10);
        let row_rect = Rect::from_min_max(pos2(grid_rect.left(), y), pos2(grid_rect.right(), y + row_h));
        painter.rect_filled(
            row_rect,
            0.0,
            if !is_drums && black {
                Color32::from_rgb(0x26, 0x26, 0x26)
            } else {
                Color32::from_rgb(0x2e, 0x2e, 0x2e)
            },
        );
        painter.hline(
            grid_rect.x_range(),
            y + row_h,
            Stroke::new(1.0, Color32::from_rgb(0x22, 0x22, 0x22)),
        );
        let key = Rect::from_min_max(pos2(area.left(), y), pos2(grid_rect.left() - 1.0, y + row_h));
        let label = if is_drums {
            drums::pad_name(pitch).unwrap_or("").to_string()
        } else if pitch % 12 == 0 {
            format!("C{}", pitch as i32 / 12 - 2)
        } else {
            String::new()
        };
        painter.rect_filled(
            key,
            0.0,
            if !is_drums && black {
                Color32::from_rgb(0x20, 0x20, 0x20)
            } else {
                Color32::from_rgb(0xc8, 0xc8, 0xc8)
            },
        );
        if !label.is_empty() {
            painter.text(
                pos2(key.left() + 3.0, key.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(row_h.clamp(8.0, 11.0)),
                Color32::BLACK,
            );
        }
        let key_resp = ui.interact(key, ui.id().with(("key", pitch)), Sense::click());
        if key_resp.clicked() {
            let now = ui.input(|i| i.time);
            app.audition(track_id, pitch, now);
        }
    }
    // Beat grid.
    let bpb = app.project.beats_per_bar as f64;
    let mut b = 0.0;
    while b <= len + 1e-9 {
        let is_bar = (b / bpb).fract().abs() < 1e-9;
        let is_beat = b.fract().abs() < 1e-9;
        let c = if is_bar {
            theme::GRID_BAR
        } else if is_beat {
            Color32::from_rgb(0x44, 0x44, 0x44)
        } else {
            Color32::from_rgb(0x36, 0x36, 0x36)
        };
        painter.vline(
            x_of(b),
            area.top()..=area.top() + rows.len() as f32 * row_h,
            Stroke::new(1.0, c),
        );
        b += 0.25;
    }

    // Notes.
    let note_rect = |n: &Note| -> Option<Rect> {
        let row = row_of(n.pitch)?;
        let y = area.top() + row as f32 * row_h;
        Some(Rect::from_min_max(
            pos2(x_of(n.start), y + 1.0),
            pos2(x_of(n.start + n.length).max(x_of(n.start) + 3.0), y + row_h - 1.0),
        ))
    };
    for (i, n) in notes.iter().enumerate() {
        if let Some(nr) = note_rect(n) {
            let selected = app.piano.selected_note == Some(i);
            let shade = 0.55 + 0.45 * (n.velocity as f32 / 127.0);
            painter.rect(
                nr,
                1.0,
                theme::dim(color, shade),
                Stroke::new(1.0, if selected { theme::SELECT } else { Color32::BLACK }),
                StrokeKind::Inside,
            );
        }
    }

    // Playhead for a playing session clip.
    if let ClipRef::Slot { track, scene } = r {
        if let Some(ti) = app.project.track_index(track) {
            if let Some(s) = session::track_state(app, ti, track) {
                if s.playing_slot.load(std::sync::atomic::Ordering::Relaxed) == scene as i32 {
                    let x = x_of(s.clip_position.load());
                    painter.vline(x, area.y_range(), Stroke::new(1.0, theme::SELECT));
                }
            }
        }
    }

    // Interaction.
    let resp = ui.interact(grid_rect, ui.id().with("piano_grid"), Sense::click_and_drag());
    let pos = resp.interact_pointer_pos().or(resp.hover_pos());
    let hit =
        |p: egui::Pos2| -> Option<usize> { notes.iter().position(|n| note_rect(n).is_some_and(|r| r.contains(p))) };
    let pitch_at = |p: egui::Pos2| -> Option<u8> {
        let row = ((p.y - area.top()) / row_h).floor();
        if row < 0.0 {
            return None;
        }
        rows.get(row as usize).copied()
    };
    let now = ui.input(|i| i.time);
    if let Some(p) = resp.hover_pos() {
        if let Some(i) = hit(p) {
            if let Some(nr) = note_rect(&notes[i]) {
                if (p.x - nr.right()).abs() < 5.0 {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }
            }
        }
    }
    let mut new_notes: Option<Vec<Note>> = None;
    let mut commit = false;

    if resp.secondary_clicked() || resp.double_clicked() {
        if let Some(i) = pos.and_then(hit) {
            let mut v = notes.clone();
            v.remove(i);
            new_notes = Some(v);
            app.piano.selected_note = None;
            commit = true;
        }
    } else if resp.drag_started() {
        if let Some(p) = pos {
            match hit(p) {
                Some(i) => {
                    app.piano.selected_note = Some(i);
                    let nr = note_rect(&notes[i]).unwrap_or(Rect::NOTHING);
                    app.piano.drag = if (p.x - nr.right()).abs() < 5.0 {
                        Some(NoteDrag::Resize { index: i })
                    } else {
                        Some(NoteDrag::Move {
                            index: i,
                            grab_beat: beat_of(p.x),
                            orig_start: notes[i].start,
                        })
                    };
                }
                None => {
                    // Draw a new note and stretch it while dragging.
                    if let Some(pitch) = pitch_at(p) {
                        let start = (beat_of(p.x) * 4.0).floor() / 4.0;
                        if start >= 0.0 && start < len {
                            let mut v = notes.clone();
                            v.push(Note {
                                pitch,
                                velocity: 100,
                                start,
                                length: 0.25,
                            });
                            app.piano.selected_note = Some(v.len() - 1);
                            app.piano.drag = Some(NoteDrag::Resize { index: v.len() - 1 });
                            new_notes = Some(v);
                            app.audition(track_id, pitch, now);
                        }
                    }
                }
            }
        }
    } else if resp.clicked() {
        if let Some(p) = pos {
            match hit(p) {
                Some(i) => {
                    app.piano.selected_note = Some(i);
                    app.audition(track_id, notes[i].pitch, now);
                }
                None => {
                    if let Some(pitch) = pitch_at(p) {
                        let start = (beat_of(p.x) * 4.0).floor() / 4.0;
                        if start >= 0.0 && start < len {
                            let mut v = notes.clone();
                            v.push(Note {
                                pitch,
                                velocity: 100,
                                start,
                                length: app.piano.last_length.min(len - start),
                            });
                            app.piano.selected_note = Some(v.len() - 1);
                            new_notes = Some(v);
                            commit = true;
                            app.audition(track_id, pitch, now);
                        }
                    }
                }
            }
        }
    }

    if resp.dragged() {
        if let (Some(drag), Some(p)) = (app.piano.drag, pos) {
            let mut v = new_notes.take().unwrap_or_else(|| notes.clone());
            match drag {
                NoteDrag::Move {
                    index,
                    grab_beat,
                    orig_start,
                } => {
                    if let Some(n) = v.get_mut(index) {
                        let delta = ((beat_of(p.x) - grab_beat) * 4.0).round() / 4.0;
                        n.start = (orig_start + delta).clamp(0.0, (len - 0.25).max(0.0));
                        let row = ((p.y - area.top()) / row_h).floor() as i32;
                        let new_row = row.clamp(0, rows.len() as i32 - 1);
                        let pitch = rows[new_row as usize];
                        if pitch != n.pitch {
                            n.pitch = pitch;
                            app.audition(track_id, pitch, now);
                        }
                    }
                }
                NoteDrag::Resize { index } => {
                    if let Some(n) = v.get_mut(index) {
                        let end = ((beat_of(p.x) * 4.0).round() / 4.0).min(len);
                        n.length = (end - n.start).max(0.25);
                        app.piano.last_length = n.length;
                    }
                }
            }
            new_notes = Some(v);
        }
    }
    if resp.drag_stopped() {
        app.piano.drag = None;
        commit = true;
    }

    if let Some(v) = new_notes {
        if let Some(n) = app.clip_mut(r).and_then(|c| c.notes_mut()) {
            *n = v;
        }
        app.dirty = true;
    }
    if commit {
        app.changed();
    }
}

fn waveform(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let Some(clip) = app.clip(r).cloned() else { return };
    let ClipContent::Audio(audio) = &clip.content else {
        return;
    };
    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme::BG_VIEW);
    let Some(data) = audio.data.clone() else {
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            format!("Missing audio file: {}", audio.path),
            egui::FontId::proportional(12.0),
            theme::ARM,
        );
        return;
    };
    let peaks = app.waveform_peaks(&data);
    let color = theme::rgb(clip.color);
    let cy = area.center().y;
    let h = area.height() / 2.0 - 10.0;
    let n = peaks.len();
    let secs_per_beat = 60.0 / app.project.tempo;
    let clip_secs = clip.length * secs_per_beat;
    let dur = data.duration_secs().max(1e-6);
    let shown = clip_secs.max(dur);
    let mut x = area.left();
    while x < area.right() {
        let t = ((x - area.left()) / area.width()) as f64 * shown;
        if t < dur {
            let v = peaks[((t / dur) * n as f64) as usize % n].min(1.0);
            painter.vline(x, (cy - v * h)..=(cy + v * h), Stroke::new(1.0, color));
        }
        x += 1.0;
    }
    let end_x = area.left() + (clip_secs / shown) as f32 * area.width();
    painter.vline(end_x, area.y_range(), Stroke::new(1.0, theme::ACCENT));
    painter.text(
        pos2(area.left() + 6.0, area.top() + 10.0),
        egui::Align2::LEFT_CENTER,
        format!(
            "{} · {} Hz · {:.2} s · warping is on the roadmap (plays at original speed)",
            audio.path, data.sample_rate, dur
        ),
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
}

// ----- device view ---------------------------------------------------------------

fn device_view(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let Some(track_id) = app.selected_track else {
        hint(ui, "Select a track to see its devices.");
        return;
    };
    let Some(track) = app.project.track(track_id) else {
        return;
    };
    let kind = track.kind;
    let name = track.name.clone();
    let color = theme::rgb(track.color);
    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(6.0, 18.0), Sense::hover());
        ui.painter().rect_filled(r, 1.0, color);
        ui.label(egui::RichText::new(name).strong());
        if kind == TrackKind::Midi {
            ui.separator();
            instrument_picker(app, ui, track_id);
        }
    });
    ui.separator();
    egui::ScrollArea::horizontal().id_salt("devices").show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let instrument = app.project.track(track_id).map(|t| t.instrument);
            match instrument {
                Some(Instrument::Synth(_)) => synth_device(app, ui, track_id),
                Some(Instrument::DrumKit(_)) => drum_device(app, ui, track_id),
                _ => {
                    ui.label(
                        egui::RichText::new(
                            "Audio track. Effects (EQ, compressor, reverb, delay…) are on the roadmap.",
                        )
                        .color(theme::TEXT_DIM),
                    );
                }
            }
        });
    });
}

fn instrument_picker(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    let current = app.project.track(track).map(|t| t.instrument.name()).unwrap_or("");
    let mut choice: Option<Instrument> = None;
    egui::ComboBox::from_id_salt(("instrument", track))
        .selected_text(current)
        .show_ui(ui, |ui| {
            if ui.selectable_label(false, "Poly Synth: Init").clicked() {
                choice = Some(Instrument::Synth(SynthParams::default()));
            }
            if ui.selectable_label(false, "Poly Synth: Bass").clicked() {
                choice = Some(Instrument::Synth(SynthParams::bass()));
            }
            if ui.selectable_label(false, "Poly Synth: Pad").clicked() {
                choice = Some(Instrument::Synth(SynthParams::pad()));
            }
            if ui.selectable_label(false, "Drum Kit").clicked() {
                choice = Some(Instrument::DrumKit(DrumKitParams::default()));
            }
        });
    if let Some(i) = choice {
        app.set_instrument(track, i);
    }
}

fn device_box(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(Color32::from_rgb(0x2b, 0x2b, 0x2b))
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

fn synth_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
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

fn drum_device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
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
