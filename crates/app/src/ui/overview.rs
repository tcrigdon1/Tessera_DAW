//! Arrangement Overview (View > Overview): the whole song in a thin strip
//! above the Arrangement, with a frame around the part on screen. Click or
//! drag in it to scroll there.

use eframe::egui::{self, pos2, Color32, Rect, Sense, Stroke, StrokeKind};

use super::theme;
use crate::app::TesseraApp;

pub const OVERVIEW_H: f32 = 30.0;

/// Song length shown, in beats: past the last clip and the loop, at least
/// 32 bars.
pub fn song_span(app: &TesseraApp) -> f64 {
    let bar = app.project.beats_per_bar.max(1) as f64;
    let last = app
        .project
        .tracks
        .iter()
        .flat_map(|t| t.arrangement.iter().map(|c| c.start + c.length))
        .fold(0.0f64, f64::max)
        .max(app.project.loop_start + app.project.loop_length)
        .max(app.song_beat());
    ((last / bar).ceil() + 4.0).max(32.0) * bar
}

/// Draws the strip in `rect`; `head_w` lines it up with the track headers
/// and `visible` is how many beats the Arrangement shows.
pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, rect: Rect, head_w: f32, visible: f64) {
    let strip = Rect::from_min_max(
        pos2(rect.left() + head_w, rect.top() + 2.0),
        pos2(rect.right() - 2.0, rect.bottom() - 3.0),
    );
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0.0, theme::bg_bar());
    p.text(
        pos2(rect.left() + 6.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Overview",
        egui::FontId::proportional(10.0),
        theme::text_dim(),
    );
    if strip.width() < 20.0 {
        return;
    }
    p.rect_filled(strip, 2.0, theme::bg_view());
    let span = song_span(app);
    let x_of = |b: f64| strip.left() + (b / span) as f32 * strip.width();

    // Clips, one thin row per track.
    let tracks = &app.project.tracks;
    let row_h = (strip.height() / tracks.len().max(1) as f32).clamp(1.0, 6.0);
    for (i, t) in tracks.iter().enumerate() {
        let y = strip.top() + i as f32 * row_h;
        if y + row_h > strip.bottom() + 0.5 {
            break;
        }
        for c in &t.arrangement {
            let r = Rect::from_min_max(
                pos2(x_of(c.start), y),
                pos2(x_of(c.start + c.length).max(x_of(c.start) + 1.0), y + row_h - 0.5),
            );
            p.rect_filled(r, 0.0, theme::rgb(c.clip.color));
        }
    }
    // Loop and playhead.
    if app.project.loop_enabled {
        let (a, b) = (app.project.loop_start, app.project.loop_start + app.project.loop_length);
        p.hline(x_of(a)..=x_of(b), strip.top() + 0.5, Stroke::new(2.0, theme::ACCENT));
    }
    let ph = x_of(app.song_beat());
    p.vline(ph, strip.y_range(), Stroke::new(1.0, Color32::WHITE));

    // The part on screen.
    let s = app.arrangement.scroll_beats;
    let frame = Rect::from_min_max(
        pos2(x_of(s), strip.top()),
        pos2(x_of(s + visible).min(strip.right()), strip.bottom()),
    );
    p.rect_stroke(frame, 1.0, Stroke::new(1.0, theme::text()), StrokeKind::Inside);

    let resp = ui
        .interact(strip, ui.id().with("arrangement_overview"), Sense::click_and_drag())
        .on_hover_text("The whole song. Click or drag to scroll the Arrangement there.");
    if resp.clicked() || resp.dragged() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let beat = ((pos.x - strip.left()) / strip.width()) as f64 * span;
            app.arrangement.scroll_beats = (beat - visible / 2.0).max(0.0);
        }
    }
}
