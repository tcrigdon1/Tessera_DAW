//! Left-hand browser. For now it lists the built-in instruments and lets you
//! create tracks from them; samples and effects come with the browser
//! milestone on the roadmap.

use eframe::egui;
use tessera_engine::dsp::drums::DrumKitParams;
use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::model::{Instrument, TrackKind};

use super::theme;
use crate::app::TesseraApp;

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.label(egui::RichText::new("BROWSER").small().color(theme::TEXT_DIM));
    ui.separator();
    ui.label(egui::RichText::new("Instruments").strong());
    let items: [(&str, Instrument); 4] = [
        ("Poly Synth", Instrument::Synth(SynthParams::default())),
        ("Poly Synth · Bass", Instrument::Synth(SynthParams::bass())),
        ("Poly Synth · Pad", Instrument::Synth(SynthParams::pad())),
        ("Drum Kit", Instrument::DrumKit(DrumKitParams::default())),
    ];
    for (name, inst) in items {
        let r = ui
            .selectable_label(false, name)
            .on_hover_text("Click: new MIDI track · Right-click: load on the selected track");
        if r.clicked() {
            let id = app.add_track(TrackKind::Midi, inst);
            let n = app.project.tracks.len();
            if let Some(t) = app.project.track_mut(id) {
                t.name = format!("{n} {name}");
            }
        }
        r.context_menu(|ui| {
            if ui.button("Load on selected track").clicked() {
                if let Some(t) = app.selected_track {
                    app.set_instrument(t, inst);
                }
                ui.close();
            }
        });
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Tracks").strong());
    if ui.selectable_label(false, "Audio Track").clicked() {
        app.add_audio_track();
    }
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Audio Effects").strong());
    ui.label(egui::RichText::new("Coming soon").small().color(theme::TEXT_DIM));
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Samples").strong());
    ui.label(
        egui::RichText::new("Drop WAV files onto an audio track's clip slot.")
            .small()
            .color(theme::TEXT_DIM),
    );
}
