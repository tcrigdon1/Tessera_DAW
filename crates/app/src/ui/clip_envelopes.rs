//! Clip envelope editor: automation that lives inside a clip and loops with
//! it.

use std::sync::atomic::Ordering;

use eframe::egui::{self, pos2, vec2, Sense, Stroke};
use tessera_engine::automation::{lane, lane_mut, AutomationTarget};

use super::{envelope, session, theme};
use crate::app::{ClipRef, TesseraApp};

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui, r: ClipRef) {
    let Some(track) = app.project.track(r.track()) else {
        return;
    };
    let track_id = track.id;
    let targets = app.automation_target_menu(track_id);
    let Some(clip) = app.clip(r) else { return };
    let len = clip.length.max(0.25);
    let color = theme::rgb(clip.color);
    let with_points: Vec<AutomationTarget> = clip
        .envelopes
        .iter()
        .filter(|l| !l.envelope.is_empty())
        .map(|l| l.target)
        .collect();
    let target = app
        .automation
        .clip_target
        .filter(|t| targets.iter().any(|(x, ..)| x == t))
        .or_else(|| with_points.first().copied())
        .unwrap_or(AutomationTarget::Volume);
    let mut env = lane(&clip.envelopes, target)
        .map(|l| l.envelope.clone())
        .unwrap_or_default();
    let overridden = lane(&clip.envelopes, target).is_some_and(|l| l.overridden);
    let spec = app.automation_target_spec(track_id, target);

    let mut chosen = target;
    let mut clear = false;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Envelope").color(theme::text_dim()));
        egui::ComboBox::from_id_salt("clip_env_target")
            .selected_text(app.automation_target_name(track_id, target))
            .width(150.0)
            .show_ui(ui, |ui| {
                let mut last = None;
                for (t, group, name) in &targets {
                    envelope::group_heading(ui, &mut last, group);
                    let mut name = name.clone();
                    if with_points.contains(t) {
                        name.push_str("  •");
                    }
                    ui.selectable_value(&mut chosen, *t, name);
                }
            });
        clear = ui
            .add_enabled(!env.is_empty(), egui::Button::new("Clear").small())
            .clicked();
        ui.label(
            egui::RichText::new("Clip envelopes loop with the clip. • marks parameters with an envelope.")
                .small()
                .color(theme::text_dim()),
        );
    });
    app.automation.clip_target = Some(chosen);

    let area = ui.available_rect_before_wrap();
    ui.allocate_rect(area, Sense::hover());
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, theme::bg_view());
    let rect = area.shrink2(vec2(4.0, 4.0));
    let ppb = rect.width() / len as f32;
    let bpb = app.project.beats_per_bar as f64;
    let mut b = 0.0;
    while b <= len + 1e-9 {
        let is_bar = (b / bpb).fract().abs() < 1e-9;
        let c = if is_bar {
            theme::grid_bar()
        } else if b.fract().abs() < 1e-9 {
            theme::gray(0x44)
        } else {
            theme::gray(0x30)
        };
        let x = rect.left() + b as f32 * ppb;
        painter.vline(x, rect.y_range(), Stroke::new(1.0, c));
        if is_bar {
            painter.text(
                pos2(x + 3.0, rect.top() + 6.0),
                egui::Align2::LEFT_CENTER,
                format!("{}", (b / bpb) as i64 + 1),
                egui::FontId::proportional(10.0),
                theme::text_dim(),
            );
        }
        b += 0.25;
    }

    // Playhead inside the clip.
    let playhead = match r {
        ClipRef::Slot { track, scene } => app.project.track_index(track).and_then(|ti| {
            let s = session::track_state(app, ti, track)?;
            (s.playing_slot.load(Ordering::Relaxed) == scene as i32).then(|| s.clip_position.load())
        }),
        ClipRef::Arrangement { track, id } => {
            let song = app.song_beat();
            app.project
                .track(track)
                .and_then(|t| t.arrangement.iter().find(|a| a.id == id))
                .filter(|a| app.is_playing() && song >= a.start && song < a.end())
                .map(|a| (song - a.start).rem_euclid(len))
        }
    };

    let view = envelope::View {
        rect,
        beat0: 0.0,
        px_per_beat: ppb,
        grid: 0.25,
        min_beat: 0.0,
        max_beat: Some(len),
        clip: area,
    };
    let resp = envelope::edit(
        ui,
        egui::Id::new(("clip_env", r.track(), clip_key(r), target)),
        &mut env,
        view,
        spec,
        color,
        overridden,
        playhead,
    );
    if resp.changed || clear {
        app.label_edit(if clear {
            "Clear Clip Envelope"
        } else {
            "Edit Clip Envelope"
        });
        if clear {
            env.points.clear();
        }
        if let Some(c) = app.clip_mut(r) {
            if env.is_empty() {
                c.envelopes.retain(|l| l.target != target);
            } else {
                lane_mut(&mut c.envelopes, target).envelope = env;
            }
        }
        app.changed();
    }
}

fn clip_key(r: ClipRef) -> u64 {
    match r {
        ClipRef::Slot { scene, .. } => scene as u64,
        ClipRef::Arrangement { id, .. } => id | (1 << 63),
    }
}
