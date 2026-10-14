//! The second window: the Session or Arrangement view in its own OS window,
//! so the two views can sit on separate monitors (View → Second Window,
//! Ctrl+Shift+W). It always shows the view the main window is not showing,
//! so Tab and the Arrange/Session buttons swap the two.

use eframe::egui::{self, ViewportBuilder, ViewportId};

use super::{browser, keyboard, theme};
use crate::app::{TesseraApp, View};
use crate::prefs::Preferences;

/// Whether the second window is open and where it goes.
pub struct SecondWindow {
    pub open: bool,
    /// Position and size it opens at. Kept fixed while it is open: egui
    /// re-applies builder fields that change, which would fight the user
    /// moving the window.
    initial: Option<[f32; 4]>,
    /// Where it is now (outer x, y, inner width, height).
    current: Option<[f32; 4]>,
}

impl SecondWindow {
    pub fn from_prefs(prefs: &Preferences) -> SecondWindow {
        SecondWindow {
            open: prefs.second_window,
            initial: prefs.second_window_rect,
            current: prefs.second_window_rect,
        }
    }

    /// Writes the open state and the last position into the preferences.
    pub fn remember(&self, prefs: &mut Preferences) {
        prefs.second_window = self.open;
        if self.current.is_some() {
            prefs.second_window_rect = self.current;
        }
    }
}

impl TesseraApp {
    /// Opens or closes the second window and remembers the choice.
    pub fn toggle_second_window(&mut self) {
        let w = &mut self.second_window;
        w.open = !w.open;
        if w.open {
            // Reopen where it was last closed.
            w.initial = w.current;
        }
        self.second_window.remember(&mut self.prefs);
        self.save_prefs();
    }
}

fn title(view: View) -> &'static str {
    match view {
        View::Session => "Session - Tessera",
        View::Arrangement => "Arrangement - Tessera",
    }
}

/// Shows the second window while it is open. Call once per frame from the
/// main window.
pub fn draw(app: &mut TesseraApp, ctx: &egui::Context) {
    if !app.second_window.open {
        return;
    }
    let view = app.view.other();
    let mut builder = ViewportBuilder::default()
        .with_title(title(view))
        .with_min_inner_size([600.0, 400.0])
        .with_drag_and_drop(true);
    match app.second_window.initial {
        Some([x, y, w, h]) => {
            builder = builder.with_position([x, y]).with_inner_size([w, h]);
        }
        None => builder = builder.with_inner_size([1200.0, 760.0]),
    }
    // Immediate, so it draws with the same app state as the main window.
    ctx.show_viewport_immediate(
        ViewportId::from_hash_of("tessera_second_window"),
        builder,
        |ui, _class| {
            let ctx = ui.ctx().clone();
            if ctx.input(|i| i.viewport().close_requested()) {
                app.second_window.open = false;
            }
            if let Some((pos, size)) = ctx.input(|i| {
                let v = i.viewport();
                Some((v.outer_rect?.min, v.inner_rect?.size()))
            }) {
                app.second_window.current = Some([pos.x, pos.y, size.x, size.y]);
            }
            // Shortcuts work in whichever window has focus.
            keyboard::handle(app, &ctx);
            egui::CentralPanel::no_frame()
                .frame(theme::panel_frame(theme::bg_view()))
                .show(ui, |ui| super::main_view(app, ui, view));
            browser::drag_preview(&ctx);
            // Playheads and meters animate here too.
            ctx.request_repaint();
        },
    );
    if !app.second_window.open {
        app.second_window.remember(&mut app.prefs);
        app.save_prefs();
    }
}
