//! Key Commands: a searchable reference of every keyboard shortcut and
//! mouse gesture (Help ▸ Key Commands…, or F1).

use eframe::egui::{self, Key, Modifiers, RichText};

use super::theme;
use crate::app::TesseraApp;

/// Ctrl on Windows and Linux, Cmd on macOS.
const MOD: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

/// (group, [(keys, action)]). `Mod` is replaced by Ctrl or Cmd.
pub const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "Transport",
        &[
            ("Space", "Play / Stop"),
            ("Shift + Space", "Continue from the stop position"),
            ("F9", "Arrangement Record"),
        ],
    ),
    (
        "Views",
        &[
            ("Tab", "Switch between Session and Arrangement view"),
            ("Shift + Tab", "Switch the detail view between Clip and Device"),
            ("Shift + ?", "Show or hide the Info View"),
            ("F1", "Show or hide this Key Commands window"),
            ("Mod + Alt + B / L / O", "Show or hide the Browser / Detail View / Overview"),
            ("Mod + Alt + M / I / S / R", "Show or hide the Mixer / In/Out / Sends / Returns"),
            ("Mod + Alt + X / D", "Show or hide the Crossfader / Track Delay"),
            ("Mod + Alt + V / E", "Show or hide the Velocity lane / Note Expression editor"),
            ("Mod + Alt + T / G", "Show or hide the Song Tempo lane / Groove Pool"),
            ("F11", "Full screen"),
            ("Esc", "Close this window or the tour"),
        ],
    ),
    (
        "File",
        &[
            ("Mod + N", "New project"),
            ("Mod + O", "Open a project"),
            ("Mod + S", "Save"),
            ("Mod + Shift + S", "Save As"),
            ("Mod + Shift + R", "Export Audio"),
            ("Mod + Shift + E", "Export the selected MIDI clip"),
            ("Mod + ,", "Preferences"),
            ("Mod + Q", "Quit (asks to save changes)"),
        ],
    ),
    (
        "Editing",
        &[
            (
                "Delete / Backspace",
                "Delete the selected note, the time selection, or else the selected clip",
            ),
            ("Mod + Z", "Undo"),
            ("Mod + Shift + Z / Mod + Y", "Redo"),
            ("Mod + Alt + Z", "Show or hide the History panel"),
            ("Mod + E", "Split clips at the time selection or playhead"),
            ("Mod + J", "Consolidate the time selection or clip (Arrangement)"),
            ("Mod + G / Mod + Shift + G", "Group the selected tracks / ungroup"),
            ("Mod + Shift + T", "Insert an audio track"),
            ("Mod + Shift + I", "Capture and Insert Scene"),
            ("Mod + Shift + C", "Capture MIDI"),
            (
                "Mod + D",
                "Duplicate the selected Session clip into the next empty slot",
            ),
        ],
    ),
    (
        "Piano Roll",
        &[
            ("B", "Toggle draw mode"),
            ("Mod + A / Esc", "Select all notes / deselect"),
            (
                "Up / Down (+ Shift)",
                "Transpose selected notes by a semitone (an octave)",
            ),
            ("Left / Right", "Move selected notes by the grid"),
            ("Shift + Left / Right", "Shorten / lengthen selected notes by the grid"),
            ("Mod + D", "Duplicate the selected notes"),
            ("Mod + C / X / V", "Copy / cut / paste notes"),
            ("Mod + J", "Legato"),
            (
                "Mod + U / Mod + Shift + U",
                "Quantize (the selected notes, or a whole selected clip) / quantize settings",
            ),
            ("Mod + 1 / 2", "Narrower / wider grid"),
        ],
    ),
    ("MIDI", &[("Mod + M", "MIDI Map Mode on / off (Esc leaves it)")]),
    (
        "Computer MIDI Keyboard",
        &[
            (
                "A S D F G H J K",
                "White keys C to C of the selected MIDI track's instrument",
            ),
            ("W E T Y U", "Black keys"),
            ("Z / X", "Octave down / up"),
        ],
    ),
    (
        "Mouse",
        &[
            (
                "Double-click a clip or slot ▶",
                "Launch a clip; on an empty MIDI slot, create a clip",
            ),
            (
                "Right-click",
                "Context menu for slots, scenes, tracks, clips and devices",
            ),
            ("Drag a knob or fader", "Change the value"),
            ("Shift + drag a knob or fader", "Fine adjustment"),
            ("Double-click a knob or fader", "Reset to the default"),
            (
                "Alt + drag (Arrangement, envelopes)",
                "Move without snapping to the grid",
            ),
            ("Alt + drag an envelope line", "Curve the segment"),
            ("Mod + mouse wheel (Arrangement)", "Zoom"),
            ("Shift + mouse wheel (Arrangement)", "Scroll sideways"),
            ("Double-click the loop brace", "Switch the Arrangement loop on or off"),
            ("Double-click a warp marker / breakpoint", "Delete it"),
        ],
    ),
];

/// Handles the window's own shortcuts. Call from the global key handler.
pub fn handle_keys(app: &mut TesseraApp, ctx: &egui::Context) {
    let consume = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
    if consume(Key::F1) {
        app.chrome.toggle_key_commands();
    }
    // Shift + / types "?"; both spellings arrive depending on the platform.
    if consume(Key::Questionmark) || ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::Slash)) {
        app.chrome.show_info = !app.chrome.show_info;
    }
    if (app.chrome.show_keys || app.chrome.tour_step.is_some()) && consume(Key::Escape) {
        app.chrome.show_keys = false;
        if app.chrome.tour_step.is_some() {
            app.chrome.end_tour();
        }
    }
}

/// Rows of `GROUPS` matching `query` (case-insensitive, on keys, action or
/// group), with `Mod` spelled out.
pub fn filtered(query: &str) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let q = query.trim().to_lowercase();
    GROUPS
        .iter()
        .filter_map(|(group, rows)| {
            let group_hit = !q.is_empty() && group.to_lowercase().contains(&q);
            let rows: Vec<(String, &'static str)> = rows
                .iter()
                .map(|(k, a)| (k.replace("Mod", MOD), *a))
                .filter(|(k, a)| {
                    q.is_empty() || group_hit || k.to_lowercase().contains(&q) || a.to_lowercase().contains(&q)
                })
                .collect();
            (!rows.is_empty()).then_some((*group, rows))
        })
        .collect()
}

pub fn window(app: &mut TesseraApp, ctx: &egui::Context) {
    if !app.chrome.show_keys {
        return;
    }
    let mut open = true;
    egui::Window::new("Key Commands")
        .open(&mut open)
        .default_size([460.0, 520.0])
        .default_pos(ctx.content_rect().center() - egui::vec2(230.0, 260.0))
        .collapsible(false)
        .show(ctx, |ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut app.chrome.keys_search)
                    .hint_text("Search shortcuts")
                    .desired_width(f32::INFINITY),
            );
            if app.chrome.focus_keys_search {
                search.request_focus();
                app.chrome.focus_keys_search = false;
            }
            ui.add_space(4.0);
            let groups = filtered(&app.chrome.keys_search);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if groups.is_empty() {
                    ui.label(RichText::new("No shortcut matches.").italics().color(theme::text_dim()));
                }
                for (group, rows) in groups {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(group.to_uppercase())
                            .small()
                            .strong()
                            .color(theme::ACCENT),
                    );
                    egui::Grid::new(("keys", group))
                        .num_columns(2)
                        .min_col_width(170.0)
                        .spacing([16.0, 4.0])
                        .striped(true)
                        .show(ui, |ui| {
                            for (keys, action) in rows {
                                ui.label(RichText::new(keys).strong().color(theme::text()));
                                ui.label(RichText::new(action).color(theme::text_dim()));
                                ui.end_row();
                            }
                        });
                }
            });
        });
    if !open {
        app.chrome.show_keys = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_filters_rows_and_groups() {
        let all = filtered("");
        assert_eq!(all.len(), GROUPS.len());
        let play = filtered("play");
        assert!(play
            .iter()
            .any(|(g, rows)| *g == "Transport" && rows.iter().any(|(k, _)| k == "Space")));
        // A group name matches all its rows.
        let midi = filtered("computer midi");
        assert_eq!(midi.len(), 1);
        assert_eq!(midi[0].1.len(), 3);
        assert!(filtered("no such shortcut").is_empty());
        // The platform modifier is spelled out.
        assert!(filtered("save as")[0].1[0].0.contains(MOD));
    }
}
