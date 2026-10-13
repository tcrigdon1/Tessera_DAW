//! Device view panel of the Electric Piano.

use eframe::egui;
use tessera_engine::dsp::electric::ElectricParams;
use tessera_engine::model::{Id, Instrument};

use super::{edit_instrument, knob_section};
use crate::app::TesseraApp;

pub fn device(app: &mut TesseraApp, ui: &mut egui::Ui, track: Id) {
    edit_instrument(app, track, |_app, inst| {
        let Instrument::Electric(p) = inst else { return false };
        let all: Vec<usize> = (0..ElectricParams::COUNT).collect();
        knob_section(ui, track, "ELECTRIC PIANO", p, &all)
    });
}
