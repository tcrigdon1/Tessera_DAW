//! Device view panels for the Analog, Electric Piano, Mallet, Simple and
//! Granular instruments.
//!
//! Like the other instrument panels, each reads a copy of the track's
//! instrument params, edits the copy and, when anything changed, writes it
//! back and sends it to the engine. [`knob_section`] draws a titled box of
//! generic controls for a list of parameter indices, so a panel can be a
//! handful of sections plus any custom displays.

pub mod analog;
pub mod electric;
pub mod granular;
pub mod mallet;
pub mod simple;

use eframe::egui;
use tessera_engine::fx::Params;
use tessera_engine::model::{Id, Instrument};

use super::detail::device_box;
use crate::app::TesseraApp;

/// A titled box of controls for `indices` of `p`, in columns of two.
/// Returns true when a value changed.
pub fn knob_section<P: Params>(ui: &mut egui::Ui, salt: Id, title: &str, p: &mut P, indices: &[usize]) -> bool {
    let mut changed = false;
    let info = P::info();
    device_box(ui, title, |ui| {
        for col in indices.chunks(2) {
            ui.vertical(|ui| {
                for &i in col {
                    let Some(inf) = info.get(i) else { continue };
                    if let Some(v) = super::devices::param_control(ui, (salt, i), inf, p.get_param(i)) {
                        p.set_param(i, v);
                        changed = true;
                    }
                }
            });
        }
    });
    changed
}

/// Reads the track's instrument, lets `edit` change a copy and sends it
/// back when it changed.
pub fn edit_instrument(app: &mut TesseraApp, track: Id, edit: impl FnOnce(&mut TesseraApp, &mut Instrument) -> bool) {
    let Some(mut inst) = app.project.track(track).map(|t| t.instrument) else {
        return;
    };
    if edit(app, &mut inst) {
        if let Some(t) = app.project.track_mut(track) {
            t.instrument = inst;
        }
        app.send_instrument(track);
    }
}
