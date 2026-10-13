//! Info View: a small panel in the bottom-left corner that explains the
//! control under the mouse, like Ableton Live's.
//!
//! Controls describe themselves with [`InfoExt::info`]. When the Info View is
//! hidden (and tooltips are on) the same text shows as a tooltip instead.

use std::collections::HashMap;

use eframe::egui::{self, Rect};
use tessera_engine::model::TrackKind;

use super::theme;
use crate::app::TesseraApp;

/// Shown when nothing with a description is hovered.
pub const HINT: &str = "Hover over a control to see what it does.";

/// Width of the Info View panel.
const WIDTH: f32 = 210.0;

/// The description of the hovered control, written by [`InfoExt::info`].
#[derive(Clone, Default)]
struct Hovered {
    frame: u64,
    title: String,
    body: String,
}

fn hovered_id() -> egui::Id {
    egui::Id::new("tessera_info_hovered")
}

fn tooltips_id() -> egui::Id {
    egui::Id::new("tessera_info_tooltips")
}

fn anchors_id() -> egui::Id {
    egui::Id::new("tessera_info_anchors")
}

/// Controls the first-run tour points at, by their Info View title. Their
/// screen rectangles are remembered each frame (see [`anchor`]).
pub const ANCHOR_TITLES: &[&str] = &["Play", "Session View", "Browser Search", "Track Volume", "Help Menu"];

/// Attaches an Info View description to a control.
pub trait InfoExt {
    /// Shows `title` and `body` in the Info View while the control is
    /// hovered (or as a tooltip when the Info View is hidden).
    fn info(self, title: &str, body: &str) -> egui::Response;
}

impl InfoExt for egui::Response {
    fn info(self, title: &str, body: &str) -> egui::Response {
        let ctx = self.ctx.clone();
        if ANCHOR_TITLES.contains(&title) {
            set_anchor(&ctx, title, self.rect, false);
        }
        if !self.hovered() {
            return self;
        }
        let frame = ctx.cumulative_frame_nr();
        ctx.data_mut(|d| {
            d.insert_temp(
                hovered_id(),
                Hovered {
                    frame,
                    title: title.to_string(),
                    body: body.to_string(),
                },
            )
        });
        let tooltips = ctx.data(|d| d.get_temp::<bool>(tooltips_id())).unwrap_or(true);
        if tooltips {
            let (title, body) = (title.to_string(), body.to_string());
            self.on_hover_ui(move |ui| {
                ui.set_max_width(280.0);
                ui.label(egui::RichText::new(&title).strong());
                ui.label(&body);
            })
        } else {
            self
        }
    }
}

/// Called once at the start of every frame.
pub(crate) fn begin_frame(ctx: &egui::Context, tooltips: bool) {
    ctx.data_mut(|d| {
        d.insert_temp(tooltips_id(), tooltips);
        d.insert_temp(anchors_id(), HashMap::<String, Rect>::new());
    });
}

/// Remembers where a named element is on screen this frame (the first one
/// wins unless `replace` is set).
pub(crate) fn set_anchor(ctx: &egui::Context, name: &str, rect: Rect, replace: bool) {
    ctx.data_mut(|d| {
        let map = d.get_temp_mut_or_default::<HashMap<String, Rect>>(anchors_id());
        if replace || !map.contains_key(name) {
            map.insert(name.to_string(), rect);
        }
    });
}

/// Where a named element was drawn this frame.
pub(crate) fn anchor(ctx: &egui::Context, name: &str) -> Option<Rect> {
    ctx.data(|d| d.get_temp::<HashMap<String, Rect>>(anchors_id()))
        .and_then(|m| m.get(name).copied())
}

/// The description hovered this frame or the last one.
pub fn current(ctx: &egui::Context) -> Option<(String, String)> {
    let now = ctx.cumulative_frame_nr();
    let h: Hovered = ctx.data(|d| d.get_temp(hovered_id()))?;
    (h.frame + 1 >= now).then_some((h.title, h.body))
}

/// Draws the Info View on the left of `ui` (the detail area) when it is
/// switched on. Call before drawing the detail view into the same `ui`.
pub fn panel(app: &mut TesseraApp, ui: &mut egui::Ui) {
    set_anchor(ui.ctx(), "Detail View", ui.max_rect(), true);
    if !app.chrome.show_info {
        return;
    }
    let resp = egui::Panel::left("info_view")
        .resizable(false)
        .exact_size(WIDTH)
        .frame(
            egui::Frame::new()
                .fill(theme::BG_BAR)
                .corner_radius(3.0)
                .inner_margin(egui::Margin::same(8)),
        )
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("INFO").small().color(theme::TEXT_DIM));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("×")
                        .info(
                            "Hide Info View",
                            "Hides the Info View. Show it again with Shift + ? or the Help menu.",
                        )
                        .clicked()
                    {
                        app.chrome.show_info = false;
                    }
                });
            });
            ui.separator();
            match current(ui.ctx()) {
                Some((title, body)) => {
                    ui.label(egui::RichText::new(title).strong().color(theme::TEXT));
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new(body).color(theme::TEXT_DIM));
                }
                None => {
                    ui.label(egui::RichText::new(HINT).italics().color(theme::TEXT_DIM));
                }
            }
        });
    set_anchor(ui.ctx(), "Info View", resp.response.rect, true);
}

// ----- descriptions shared by several views ------------------------------------

pub const TRACK_TITLE: &str = "Click to select the track and show its devices. Right-click to rename, duplicate, delete or recolour it, or to insert a new track. Drop instruments, effects and samples here.";
pub const RETURN_TITLE: &str = "A return track, fed by every track's send knob with the same letter. Click to show its effects; right-click to rename, add or delete return tracks.";
pub const MASTER_TITLE: &str = "The master track: everything you hear goes through it. Click to show its effects; right-click to insert a return track.";
pub const CLIP_LAUNCH: &str = "Starts the clip at the next launch quantization boundary and stops the track's other clip. Double-clicking the clip does the same.";
pub const SLOT_RECORD: &str = "Records the armed audio track's input into this slot from the next launch boundary. Click the slot again to stop and play the new clip.";
pub const CLIP_STOP: &str = "Stops the clip playing on this track at the next launch quantization boundary.";
pub const SCENE: &str = "A scene is a row of clips that launch together. Double-click to launch it; right-click to rename it, give it a tempo, or insert and delete scenes.";
pub const SCENE_LAUNCH: &str = "Launches every clip in this row and stops the clips on tracks with an empty slot here. Changes the tempo if the scene has one.";
pub const STOP_ALL: &str = "Stops every Session clip at the next launch quantization boundary.";
pub const ACTIVATOR: &str = "Track Activator: switch it off to mute the track. The number is the track's position.";
pub const SOLO: &str = "Solo: hear only this track (and other soloed tracks).";
pub const ARM: &str = "Arm the track for recording. An armed audio track monitors its input (with Monitor Auto) and records with Arrangement Record or a slot's record button.";
pub const VOLUME: &str = "The track's volume in dB (-inf to +6). Drag or double-click to type a value.";
pub const LOOP_BRACE: &str = "The Arrangement loop. Drag the brace to move it, drag its ends to resize it, double-click to switch the loop on or off.";
pub const ARRANGEMENT_CLIP: &str = "Click to edit the clip below. Drag to move it (Alt bypasses the grid), drag its right edge to make it longer or shorter, right-click to rename, duplicate, recolour or delete it.";

/// Description of a Session clip slot.
pub fn slot_text(has_clip: bool, kind: TrackKind, can_record: bool) -> &'static str {
    match (has_clip, kind) {
        (true, _) => "A clip. Click to select and edit it below, double-click or click its triangle to launch it, right-click to duplicate, delete or copy it to the Arrangement.",
        (false, TrackKind::Midi) => "An empty clip slot. Double-click to create a MIDI clip, or drop an instrument, effect or sample here.",
        (false, TrackKind::Audio) if can_record => "An empty slot on an armed track. Click the dot to record into it, double-click to import a WAV file, or drop a sample here.",
        (false, TrackKind::Audio) => "An empty clip slot. Double-click to import a WAV file or drop a sample here. Arm the track to record into it.",
    }
}
