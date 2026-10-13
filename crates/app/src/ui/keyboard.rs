//! Global keyboard shortcuts and the computer MIDI keyboard.

use eframe::egui::{self, Key, Modifiers};
use tessera_engine::model::TrackKind;
use tessera_engine::Command;

use crate::app::{ClipRef, DetailMode, TesseraApp, View};

/// Computer keyboard to semitone offset, Ableton-style (A = C).
const NOTE_KEYS: [(Key, i32); 13] = [
    (Key::A, 0),
    (Key::W, 1),
    (Key::S, 2),
    (Key::E, 3),
    (Key::D, 4),
    (Key::F, 5),
    (Key::T, 6),
    (Key::G, 7),
    (Key::Y, 8),
    (Key::H, 9),
    (Key::U, 10),
    (Key::J, 11),
    (Key::K, 12),
];

pub fn handle(app: &mut TesseraApp, ctx: &egui::Context) {
    // Leave keys alone while typing in a text field.
    if ctx.text_edit_focused() {
        return;
    }
    // Tab and Space belong to the DAW, not to egui's widget focus traversal.
    if let Some(id) = ctx.memory(|m| m.focused()) {
        ctx.memory_mut(|m| m.surrender_focus(id));
    }
    crate::ui::key_commands::handle_keys(app, ctx);
    // Note editing keys while the piano roll shows a clip (Delete and
    // Ctrl+D act on selected notes there, before the clip shortcuts below).
    super::piano_roll::handle_keys(app, ctx);
    let cmd = Modifiers::COMMAND;
    let shift = Modifiers::SHIFT;
    let consume = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));

    if consume(cmd | shift, Key::S) {
        app.save_as();
    }
    if consume(cmd, Key::S) {
        app.save();
    }
    if consume(cmd, Key::O) {
        app.request(crate::app::PendingAction::Open(None));
    }
    if consume(cmd, Key::N) {
        app.request(crate::app::PendingAction::New);
    }
    if consume(cmd, Key::Comma) {
        let open = app.prefs_ui.open;
        app.prefs_ui.open = !open;
    }
    if consume(cmd | shift, Key::R) {
        app.export_dialog();
    }
    if consume(cmd | shift, Key::E) {
        app.export_midi_clip_dialog();
    }
    if consume(cmd, Key::Q) {
        app.request(crate::app::PendingAction::Quit);
    }
    if consume(cmd, Key::M) {
        app.toggle_midi_learn();
    }
    if app.midi_learn.is_some() && consume(Modifiers::NONE, Key::Escape) {
        app.toggle_midi_learn();
    }
    if consume(shift, Key::Space) {
        app.send(Command::Continue);
    }
    if consume(Modifiers::NONE, Key::Space) {
        app.toggle_play();
    }
    if consume(Modifiers::NONE, Key::F9) {
        app.toggle_arrangement_record();
    }
    if consume(shift, Key::Tab) {
        app.detail = match app.detail {
            DetailMode::Clip => DetailMode::Device,
            DetailMode::Device => DetailMode::Clip,
        };
    }
    if consume(Modifiers::NONE, Key::Tab) {
        app.view = match app.view {
            View::Session => View::Arrangement,
            View::Arrangement => View::Session,
        };
    }
    // Undo / redo (the History panel lists the steps).
    if consume(cmd | shift, Key::Z) || consume(cmd, Key::Y) {
        app.redo();
    }
    if consume(cmd, Key::Z) {
        app.undo();
    }
    if consume(cmd, Key::G) {
        app.group_selected();
    }
    if consume(cmd | shift, Key::G) {
        if let Some(t) = app.selected_track {
            app.ungroup(t);
        }
    }
    if consume(cmd, Key::E) {
        app.split();
    }
    if consume(cmd, Key::J) {
        app.consolidate();
    }
    if consume(cmd | shift, Key::I) {
        app.capture_scene();
    }
    if consume(cmd | shift, Key::C) {
        let now = ctx.input(|i| i.time);
        app.capture_midi(now);
    }
    if consume(cmd | shift, Key::T) {
        app.add_audio_track();
    }
    if consume(cmd | Modifiers::ALT, Key::Z) {
        app.work.show_history = !app.work.show_history;
    }
    if (consume(Modifiers::NONE, Key::Delete) || consume(Modifiers::NONE, Key::Backspace)) && !app.delete_selection() {
        if let Some(r) = app.selected_clip {
            app.label_edit("Delete Clip");
            app.delete_clip(r);
        }
    }
    if consume(cmd, Key::D) {
        if let Some(ClipRef::Slot { track, scene }) = app.selected_clip {
            app.duplicate_slot(track, scene);
        }
    }

    // Computer MIDI keyboard on the selected MIDI track.
    if ctx.input(|i| i.modifiers.command || i.modifiers.alt) {
        return;
    }
    if consume(Modifiers::NONE, Key::Z) {
        app.keyboard_octave = (app.keyboard_octave - 1).max(0);
    }
    if consume(Modifiers::NONE, Key::X) {
        app.keyboard_octave = (app.keyboard_octave + 1).min(9);
    }
    let Some(track) = app.selected_track else { return };
    if app.project.track(track).map(|t| t.kind) != Some(TrackKind::Midi) {
        return;
    }
    let events: Vec<(Key, bool, bool)> = ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                egui::Event::Key {
                    key, pressed, repeat, ..
                } => Some((*key, *pressed, *repeat)),
                _ => None,
            })
            .collect()
    });
    for (key, pressed, repeat) in events {
        let Some(&(_, semi)) = NOTE_KEYS.iter().find(|(k, _)| *k == key) else {
            continue;
        };
        if pressed && !repeat {
            let pitch = (app.keyboard_octave * 12 + semi).clamp(0, 127) as u8;
            if !app.held_keys.iter().any(|(k, _, _)| *k == key) {
                app.held_keys.push((key, track, pitch));
                let now = ctx.input(|i| i.time);
                app.capture_note(track, pitch, 100, true, now);
                app.send(Command::NoteOn {
                    track,
                    pitch,
                    velocity: 100,
                });
            }
        } else if !pressed {
            if let Some(pos) = app.held_keys.iter().position(|(k, _, _)| *k == key) {
                let (_, t, pitch) = app.held_keys.remove(pos);
                let now = ctx.input(|i| i.time);
                app.capture_note(t, pitch, 0, false, now);
                app.send(Command::NoteOff { track: t, pitch });
            }
        }
    }
}
