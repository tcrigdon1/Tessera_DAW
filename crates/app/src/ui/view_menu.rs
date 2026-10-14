//! The View menu: a checklist of the panels, editors and mixer sections
//! that can be shown or hidden, with their shortcuts (Ctrl+Alt+letter, like
//! Ableton). The choices are saved with the preferences.

use eframe::egui::{self, Key, Modifiers, RichText};
use serde::{Deserialize, Serialize};

use super::theme;
use crate::app::{DetailMode, TesseraApp, View};

/// Panels and mixer sections the View menu shows or hides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewPanels {
    /// The Clip / Device view at the bottom (Ctrl+Alt+L).
    pub detail: bool,
    /// The whole-song strip above the Arrangement (Ctrl+Alt+O).
    pub overview: bool,
    /// The Session view's mixer (Ctrl+Alt+M).
    pub mixer: bool,
    /// The output routing row of the mixer (Ctrl+Alt+I).
    pub io: bool,
    /// Send knobs (Ctrl+Alt+S).
    pub sends: bool,
    /// Return tracks (Ctrl+Alt+R).
    pub returns: bool,
    /// Crossfader assign buttons and the crossfader (Ctrl+Alt+X).
    pub crossfader: bool,
    /// Track delay boxes (Ctrl+Alt+D).
    pub track_delay: bool,
    /// Per-note pitch and pressure lanes in the piano roll (Ctrl+Alt+E).
    pub note_expression: bool,
    pub status_bar: bool,
}

impl Default for ViewPanels {
    fn default() -> Self {
        ViewPanels {
            detail: true,
            overview: true,
            mixer: true,
            io: true,
            sends: true,
            returns: true,
            crossfader: true,
            track_delay: true,
            note_expression: false,
            status_bar: true,
        }
    }
}

/// Something the View menu toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Browser,
    Detail,
    Info,
    Overview,
    StatusBar,
    Mixer,
    Io,
    Sends,
    Returns,
    Crossfader,
    TrackDelay,
    Velocity,
    NoteExpression,
    MidiMap,
    TempoLane,
    History,
    GroovePool,
    KeyCommands,
    FullScreen,
}

/// (toggle, label, shortcut, key with Ctrl+Alt).
type Entry = (Toggle, &'static str, &'static str, Option<Key>);

const PANELS: &[Entry] = &[
    (Toggle::Browser, "Browser", "Ctrl+Alt+B", Some(Key::B)),
    (Toggle::Detail, "Detail View", "Ctrl+Alt+L", Some(Key::L)),
    (Toggle::Info, "Info View", "Shift+?", None),
    (Toggle::Overview, "Overview", "Ctrl+Alt+O", Some(Key::O)),
    (Toggle::StatusBar, "Status Bar", "", None),
];

const MIXER: &[Entry] = &[
    (Toggle::Mixer, "Mixer", "Ctrl+Alt+M", Some(Key::M)),
    (Toggle::Io, "In/Out", "Ctrl+Alt+I", Some(Key::I)),
    (Toggle::Sends, "Sends", "Ctrl+Alt+S", Some(Key::S)),
    (Toggle::Returns, "Returns", "Ctrl+Alt+R", Some(Key::R)),
    (Toggle::Crossfader, "Crossfader", "Ctrl+Alt+X", Some(Key::X)),
    (Toggle::TrackDelay, "Track Delay", "Ctrl+Alt+D", Some(Key::D)),
];

const EDITORS: &[Entry] = &[
    (Toggle::Velocity, "Velocity Lane", "Ctrl+Alt+V", Some(Key::V)),
    (
        Toggle::NoteExpression,
        "Note Expression Editor",
        "Ctrl+Alt+E",
        Some(Key::E),
    ),
    (Toggle::MidiMap, "MIDI Map Mode", "Ctrl+M", None),
    (Toggle::TempoLane, "Song Tempo Lane", "Ctrl+Alt+T", Some(Key::T)),
];

const WINDOWS: &[Entry] = &[
    (Toggle::History, "History", "Ctrl+Alt+Z", None),
    (Toggle::GroovePool, "Groove Pool", "Ctrl+Alt+G", Some(Key::G)),
    (Toggle::KeyCommands, "Key Commands", "F1", None),
    (Toggle::FullScreen, "Full Screen", "F11", None),
];

/// What each toggle does, for the menu's hover text.
fn hint(t: Toggle) -> &'static str {
    match t {
        Toggle::Browser => "Instruments, effects, samples, clips and your library on the left.",
        Toggle::Detail => "The Clip and Device views at the bottom of the window.",
        Toggle::Info => "Describes whatever the mouse is over, in the bottom-left corner.",
        Toggle::Overview => "A strip above the Arrangement showing the whole song; click or drag it to scroll.",
        Toggle::StatusBar => "Messages, CPU and the audio device at the very bottom.",
        Toggle::Mixer => "Volume, pan and meters under the Session clip slots.",
        Toggle::Io => "Where each track's audio goes.",
        Toggle::Sends => "Send knobs to the return tracks.",
        Toggle::Returns => "The return tracks' columns in the Session view.",
        Toggle::Crossfader => "Crossfader A/B assign buttons and the crossfader under the master.",
        Toggle::TrackDelay => "Delay or advance a track by up to 100 ms.",
        Toggle::Velocity => "The velocity lane under the piano roll.",
        Toggle::NoteExpression => {
            "Per-note pitch bend and pressure lanes under the piano roll. Select a note and draw its curves."
        }
        Toggle::MidiMap => "Click a control, then move a knob on your MIDI controller to map it.",
        Toggle::TempoLane => "The Song Tempo automation lane at the top of the Arrangement.",
        Toggle::History => "Every undo step; click one to go back to it.",
        Toggle::GroovePool => "Grooves you can apply to clips for swing and feel.",
        Toggle::KeyCommands => "Every keyboard shortcut, searchable.",
        Toggle::FullScreen => "Fills the screen with Tessera.",
    }
}

impl TesseraApp {
    /// Whether a View menu item is ticked.
    pub fn view_shown(&self, t: Toggle) -> bool {
        let v = &self.prefs.view;
        match t {
            Toggle::Browser => self.show_browser,
            Toggle::Detail => v.detail,
            Toggle::Info => self.chrome.show_info,
            Toggle::Overview => v.overview,
            Toggle::StatusBar => v.status_bar,
            Toggle::Mixer => v.mixer,
            Toggle::Io => v.io,
            Toggle::Sends => v.sends,
            Toggle::Returns => v.returns,
            Toggle::Crossfader => v.crossfader,
            Toggle::TrackDelay => v.track_delay,
            Toggle::Velocity => self.piano.show_velocity,
            Toggle::NoteExpression => v.note_expression,
            Toggle::MidiMap => self.midi_learn.is_some(),
            Toggle::TempoLane => self.work.show_tempo_lane,
            Toggle::History => self.work.show_history,
            Toggle::GroovePool => self.work.show_grooves,
            Toggle::KeyCommands => self.chrome.show_keys,
            Toggle::FullScreen => self.full_screen,
        }
    }

    /// Shows or hides a View menu item; panel choices are saved.
    pub fn toggle_view(&mut self, t: Toggle, ctx: &egui::Context) {
        let on = !self.view_shown(t);
        let v = &mut self.prefs.view;
        let mut saved = true;
        match t {
            Toggle::Detail => v.detail = on,
            Toggle::Overview => v.overview = on,
            Toggle::StatusBar => v.status_bar = on,
            Toggle::Mixer => v.mixer = on,
            Toggle::Io => v.io = on,
            Toggle::Sends => v.sends = on,
            Toggle::Returns => v.returns = on,
            Toggle::Crossfader => v.crossfader = on,
            Toggle::TrackDelay => v.track_delay = on,
            Toggle::NoteExpression => v.note_expression = on,
            _ => saved = false,
        }
        match t {
            Toggle::Browser => self.show_browser = on,
            Toggle::Info => self.chrome.show_info = on,
            Toggle::Velocity => self.piano.show_velocity = on,
            Toggle::MidiMap => self.toggle_midi_learn(),
            Toggle::TempoLane => {
                self.work.show_tempo_lane = on;
                if on {
                    self.view = View::Arrangement;
                }
            }
            Toggle::History => self.work.show_history = on,
            Toggle::GroovePool => self.work.show_grooves = on,
            Toggle::KeyCommands => self.chrome.toggle_key_commands(),
            Toggle::FullScreen => {
                self.full_screen = on;
                ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
            }
            _ => {}
        }
        // The editors live in the Detail View: showing one shows it too.
        if on && matches!(t, Toggle::Velocity | Toggle::NoteExpression) {
            self.prefs.view.detail = true;
            self.detail = DetailMode::Clip;
            saved = true;
        }
        if saved {
            self.save_prefs();
        }
    }
}

/// A menu row with a tick when `on`, the label and its shortcut.
fn check_item(ui: &mut egui::Ui, on: bool, label: &str, keys: &str) -> egui::Response {
    let tick = RichText::new("✔").color(if on { theme::ACCENT } else { egui::Color32::TRANSPARENT });
    ui.add(egui::Button::new((tick, label)).shortcut_text(keys))
}

fn entries(app: &mut TesseraApp, ui: &mut egui::Ui, list: &[Entry]) {
    for &(t, label, keys, _) in list {
        let on = app.view_shown(t);
        if check_item(ui, on, label, keys).on_hover_text(hint(t)).clicked() {
            app.toggle_view(t, ui.ctx());
        }
    }
}

/// The View menu.
pub fn menu(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.menu_button("View", |ui| {
        ui.set_min_width(250.0);
        for (view, label) in [(View::Arrangement, "Arrangement View"), (View::Session, "Session View")] {
            if check_item(ui, app.view == view, label, "").clicked() {
                app.view = view;
                ui.close();
            }
        }
        if check_item(ui, false, "Switch Views", "Tab")
            .on_hover_text("With the second window open, this swaps what the two windows show.")
            .clicked()
        {
            app.view = app.view.other();
            ui.close();
        }
        if check_item(ui, app.second_window.open, "Second Window", "Ctrl+Shift+W")
            .on_hover_text(
                "Opens the other view in its own window, so Session and Arrangement can sit on separate monitors.",
            )
            .clicked()
        {
            app.toggle_second_window();
            ui.close();
        }
        ui.separator();
        entries(app, ui, PANELS);
        // Which half of the Detail View shows.
        for (mode, label) in [
            (DetailMode::Clip, "    Clip View"),
            (DetailMode::Device, "    Device View"),
        ] {
            let on = app.prefs.view.detail && app.detail == mode;
            if check_item(ui, on, label, "Shift+Tab").clicked() {
                app.detail = mode;
                if !app.prefs.view.detail {
                    app.toggle_view(Toggle::Detail, ui.ctx());
                }
            }
        }
        ui.separator();
        section(ui, "Mixer");
        entries(app, ui, MIXER);
        ui.separator();
        section(ui, "Editors");
        entries(app, ui, EDITORS);
        ui.separator();
        section(ui, "Windows");
        entries(app, ui, WINDOWS);
        ui.separator();
        if ui
            .button("Show All Panels")
            .on_hover_text("Turns every panel and mixer section back on.")
            .clicked()
        {
            let expression = app.prefs.view.note_expression;
            app.prefs.view = ViewPanels {
                note_expression: expression,
                ..ViewPanels::default()
            };
            app.show_browser = true;
            app.save_prefs();
            ui.close();
        }
    });
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.label(
        RichText::new(title.to_uppercase())
            .small()
            .strong()
            .color(theme::text_dim()),
    );
}

/// Ctrl+Alt+letter and F11. Call from the global key handler.
pub fn handle_keys(app: &mut TesseraApp, ctx: &egui::Context) {
    let cmd_alt = Modifiers::COMMAND | Modifiers::ALT;
    for list in [PANELS, MIXER, EDITORS, WINDOWS] {
        for &(t, _, _, key) in list {
            if let Some(k) = key {
                if ctx.input_mut(|i| i.consume_key(cmd_alt, k)) {
                    app.toggle_view(t, ctx);
                }
            }
        }
    }
    if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F11)) {
        app.toggle_view(Toggle::FullScreen, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_are_unique() {
        let keys: Vec<Key> = [PANELS, MIXER, EDITORS, WINDOWS]
            .iter()
            .flat_map(|l| l.iter().filter_map(|e| e.3))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_by_key(|k| k.name());
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len());
        // Ctrl+Alt+Z stays the History shortcut of the global handler.
        assert!(!keys.contains(&Key::Z));
    }

    #[test]
    fn old_preferences_get_every_panel() {
        let v: ViewPanels = serde_json::from_str("{}").unwrap();
        assert_eq!(v, ViewPanels::default());
        let v: ViewPanels = serde_json::from_str(r#"{"sends": false}"#).unwrap();
        assert!(!v.sends && v.mixer);
    }
}
