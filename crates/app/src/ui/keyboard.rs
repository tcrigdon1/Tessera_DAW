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
        app.open_dialog();
    }
    if consume(cmd, Key::N) {
        app.new_project();
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
    if consume(Modifiers::NONE, Key::Delete) || consume(Modifiers::NONE, Key::Backspace) {
        if let (Some(r), Some(i)) = (app.selected_clip, app.piano.selected_note) {
            if let Some(notes) = app.clip_mut(r).and_then(|c| c.notes_mut()) {
                if i < notes.len() {
                    notes.remove(i);
                }
            }
            app.piano.selected_note = None;
            app.changed();
        } else if let Some(r) = app.selected_clip {
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
                app.send(Command::NoteOn {
                    track,
                    pitch,
                    velocity: 100,
                });
            }
        } else if !pressed {
            if let Some(pos) = app.held_keys.iter().position(|(k, _, _)| *k == key) {
                let (_, t, pitch) = app.held_keys.remove(pos);
                app.send(Command::NoteOff { track: t, pitch });
            }
        }
    }
}
