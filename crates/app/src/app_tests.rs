//! Tests that drive a whole app headlessly: random edit sequences drawing
//! every view in between (deleting what is selected or open in an editor,
//! undo and redo across deletions, scenes and returns coming and going,
//! save/load round trips), and keyboard shortcuts.

use eframe::egui;
use tessera_engine::fx::DeviceKind;
use tessera_engine::model::{Id, Project};

use crate::app::{ClipRef, DetailMode, TesseraApp};

/// Small deterministic generator so failures replay.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn track(&mut self, app: &TesseraApp) -> Option<Id> {
        let n = app.project.tracks.len();
        (n > 0).then(|| app.project.tracks[self.below(n)].id)
    }
}

fn frame(app: &mut TesseraApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0))),
        time: Some(time),
        events: [vec![egui::Event::ModifiersChanged(modifiers)], events].concat(),
        ..Default::default()
    };
    let mut out = ctx.run_ui(input, |ui| {
        crate::ui::keyboard::handle(app, ui.ctx());
        crate::ui::draw(app, ui);
    });
    out.textures_delta.clear();
    app.flush();
    app.commit_history(false, time);
}

/// Key presses that never open a file dialog or write files.
const KEYS: &[(egui::Key, egui::Modifiers)] = {
    use egui::{Key, Modifiers as M};
    &[
        (Key::Delete, M::NONE),
        (Key::Backspace, M::NONE),
        (Key::Z, M::COMMAND),
        (Key::Z, M::COMMAND.plus(M::SHIFT)),
        (Key::Y, M::COMMAND),
        (Key::D, M::COMMAND),
        (Key::E, M::COMMAND),
        (Key::J, M::COMMAND),
        (Key::G, M::COMMAND),
        (Key::G, M::COMMAND.plus(M::SHIFT)),
        (Key::A, M::COMMAND),
        (Key::C, M::COMMAND),
        (Key::X, M::COMMAND),
        (Key::V, M::COMMAND),
        (Key::U, M::COMMAND),
        (Key::I, M::COMMAND.plus(M::SHIFT)),
        (Key::ArrowUp, M::NONE),
        (Key::ArrowDown, M::SHIFT),
        (Key::ArrowLeft, M::NONE),
        (Key::ArrowRight, M::SHIFT),
        (Key::Tab, M::NONE),
        (Key::Tab, M::SHIFT),
        (Key::Escape, M::NONE),
        (Key::Num1, M::COMMAND),
        (Key::Num2, M::COMMAND),
    ]
};

fn random_edit(app: &mut TesseraApp, rng: &mut Rng) {
    let scenes = app.project.scenes.len();
    match rng.below(30) {
        0 => {
            app.add_midi_track();
        }
        1 => {
            app.add_audio_track();
        }
        2 => {
            app.add_drum_track();
        }
        3 => {
            app.add_group_track();
        }
        4 | 5 => {
            if let Some(t) = rng.track(app) {
                app.label_edit("Delete Track");
                app.delete_track(t);
            }
        }
        6 => {
            if let Some(t) = rng.track(app) {
                app.toggle_track_selection(t);
            }
        }
        7 => app.group_selected(),
        8 => {
            if let Some(t) = rng.track(app) {
                app.ungroup(t);
            }
        }
        9 => app.delete_tracks(),
        10 => {
            let at = rng.below(scenes + 1);
            app.project.insert_scene(at);
            app.changed();
        }
        11 => {
            // As the Session view's Delete Scene does.
            app.project.remove_scene(rng.below(scenes));
            app.selected_clip = None;
            let n = app.project.scenes.len();
            app.work.selected_scene = app.work.selected_scene.min(n.saturating_sub(1));
            app.changed();
        }
        12 | 13 => {
            if let Some(t) = rng.track(app) {
                app.create_clip_in_slot(t, rng.below(scenes));
            }
        }
        14 => {
            if let Some(t) = rng.track(app) {
                app.create_arrangement_clip(t, rng.below(64) as f64);
            }
        }
        15 => {
            if let Some(t) = rng.track(app) {
                let scene = rng.below(scenes);
                if app.clip(ClipRef::Slot { track: t, scene }).is_some() {
                    app.select_clip(ClipRef::Slot { track: t, scene });
                    app.detail = DetailMode::Clip;
                }
            }
        }
        16 => {
            if let Some(r) = app.selected_clip {
                app.delete_clip(r);
            }
        }
        17 => {
            if let Some(ClipRef::Slot { track, scene }) = app.selected_clip {
                app.duplicate_slot(track, scene);
                app.slot_to_arrangement(track, scene);
            }
        }
        18 | 19 => app.undo(),
        20 => app.redo(),
        21 => {
            app.add_return_track(None);
        }
        22 => {
            if !app.project.returns.is_empty() {
                let r = app.project.returns[rng.below(app.project.returns.len())].id;
                app.delete_return(r);
            }
        }
        23 => {
            if let Some(t) = rng.track(app) {
                let kind = DeviceKind::ALL[rng.below(DeviceKind::ALL.len())];
                app.add_device(t, kind);
                app.show_devices(t);
            }
        }
        24 => {
            if let Some(t) = rng.track(app) {
                let devices: Vec<Id> = app
                    .project
                    .track(t)
                    .map(|t| t.devices.iter().map(|d| d.id).collect())
                    .unwrap_or_default();
                if !devices.is_empty() {
                    let d = devices[rng.below(devices.len())];
                    app.devices.selected = Some((t, d));
                    match rng.below(4) {
                        0 => app.delete_device(t, d),
                        1 => app.duplicate_device(t, d),
                        2 => app.group_into_rack(t, d),
                        _ => {
                            if app.copy_device(t, d) {
                                app.paste_device(t, Some(d));
                            }
                        }
                    }
                }
            }
        }
        25 => {
            // Select some notes, including ones past the end.
            let n = rng.below(12);
            app.piano.selection = (0..n).map(|_| rng.below(40)).collect();
        }
        26 => {
            if let Some(t) = rng.track(app) {
                app.nudge_track(t, if rng.below(2) == 0 { -1 } else { 1 });
                app.move_track(t, rng.below(app.project.tracks.len() + 1));
            }
        }
        27 => app.view = app.view.other(),
        28 => {
            app.detail = match app.detail {
                DetailMode::Clip => DetailMode::Device,
                DetailMode::Device => DetailMode::Clip,
            }
        }
        _ => {
            // Save and load back, keeping the selection as it was.
            let json = app.project.save_json().expect("save");
            let p = Project::from_json(&json).expect("load");
            let (track, clip) = (app.selected_track, app.selected_clip);
            app.load_project(p, None);
            app.selected_track = track;
            app.selected_clip = clip;
        }
    }
}

fn run(seed: u64, steps: usize) {
    let mut app = TesseraApp::for_tests();
    let ctx = egui::Context::default();
    let mut rng = Rng(seed.max(1));
    for step in 0..steps {
        let time = step as f64 / 60.0;
        random_edit(&mut app, &mut rng);
        let events = if rng.below(3) == 0 {
            let (key, modifiers) = KEYS[rng.below(KEYS.len())];
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            }]
        } else {
            Vec::new()
        };
        // And a click or drag somewhere, the second time round through the
        // frame so the press and release land on what is drawn.
        let mut events = events;
        if std::env::var_os("TESSERA_FUZZ_CLICKS").is_some() && rng.below(2) == 0 {
            let p = egui::pos2(rng.below(1400) as f32, rng.below(900) as f32);
            let q = p + egui::vec2(rng.below(200) as f32 - 100.0, rng.below(120) as f32 - 60.0);
            let button = if rng.below(4) == 0 {
                egui::PointerButton::Secondary
            } else {
                egui::PointerButton::Primary
            };
            let press = |pos, pressed| egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(
                &mut app,
                &ctx,
                time - 0.004,
                vec![egui::Event::PointerMoved(p), press(p, true)],
                egui::Modifiers::NONE,
            );
            let end = if rng.below(3) == 0 { q } else { p };
            events.extend([egui::Event::PointerMoved(end), press(end, false)]);
        }
        let modifiers = match &events[..] {
            [egui::Event::Key { modifiers, .. }] => *modifiers,
            _ => egui::Modifiers::NONE,
        };
        frame(&mut app, &ctx, time, events, modifiers);
        // Draw the other view too, as the second window would.
        if step % 5 == 0 {
            app.view = app.view.other();
            frame(&mut app, &ctx, time + 0.008, Vec::new(), egui::Modifiers::NONE);
        }
    }
    // Whatever happened, the set still saves and loads to the same thing.
    let json = app.project.save_json().expect("save");
    let again = Project::from_json(&json)
        .expect("load")
        .save_json()
        .expect("save again");
    assert_eq!(json, again, "save/load round trip changed the project (seed {seed})");
    assert!(app
        .project
        .tracks
        .iter()
        .all(|t| t.slots.len() == app.project.scenes.len()));
}

/// `TESSERA_FUZZ=<seeds>` runs longer (1500 steps per seed).
#[test]
fn random_edits_never_panic() {
    let (seeds, steps) = match std::env::var("TESSERA_FUZZ").ok().and_then(|s| s.parse().ok()) {
        Some(n) => (n, 1500),
        None => (6, 400),
    };
    for seed in 1..=seeds {
        run(seed * 7919, steps);
    }
}

fn press(app: &mut TesseraApp, ctx: &egui::Context, key: egui::Key, modifiers: egui::Modifiers) {
    let event = egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    };
    frame(app, ctx, 0.0, vec![event], modifiers);
}

#[test]
fn ctrl_alt_z_shows_history_without_undoing() {
    use egui::{Key, Modifiers as M};
    let mut app = TesseraApp::for_tests();
    let ctx = egui::Context::default();
    let tracks = app.project.tracks.len();
    app.label_edit("Insert MIDI Track");
    app.add_midi_track();
    app.commit_history(false, 0.0);
    press(&mut app, &ctx, Key::Z, M::COMMAND | M::ALT);
    assert!(app.work.show_history);
    assert_eq!(app.project.tracks.len(), tracks + 1, "Ctrl+Alt+Z undid");
}

#[test]
fn ctrl_shift_g_ungroups() {
    use egui::{Key, Modifiers as M};
    use tessera_engine::model::TrackKind;
    let mut app = TesseraApp::for_tests();
    let ctx = egui::Context::default();
    let groups = |app: &TesseraApp| app.project.tracks.iter().filter(|t| t.kind == TrackKind::Group).count();
    let a = app.add_midi_track();
    app.toggle_track_selection(a);
    app.group_selected();
    let before = groups(&app);
    let g = app.selected_track.unwrap();
    app.select_track(g);
    press(&mut app, &ctx, Key::G, M::COMMAND | M::SHIFT);
    assert_eq!(groups(&app), before - 1);
    // On a member, it ungroups the member's group.
    let b = app.add_midi_track();
    app.toggle_track_selection(b);
    app.group_selected();
    app.select_track(b);
    let before = groups(&app);
    press(&mut app, &ctx, Key::G, M::COMMAND | M::SHIFT);
    assert_eq!(groups(&app), before - 1);
}

#[test]
fn device_shortcuts_leave_ctrl_shift_ones_alone() {
    use egui::{Key, Modifiers as M};
    let mut app = TesseraApp::for_tests();
    let ctx = egui::Context::default();
    let t = app.add_audio_track();
    app.add_device(t, DeviceKind::ALL[0]);
    app.show_devices(t);
    let d = app.project.track(t).unwrap().devices[0].id;
    app.devices.selected = Some((t, d));
    let chain = |app: &TesseraApp| app.project.track(t).unwrap().devices.len();
    press(&mut app, &ctx, Key::C, M::COMMAND | M::SHIFT);
    assert!(app.devices.clipboard.is_none(), "Ctrl+Shift+C copied the device");
    press(&mut app, &ctx, Key::G, M::COMMAND | M::SHIFT);
    assert!(
        app.project.track(t).unwrap().devices[0].rack.is_none(),
        "Ctrl+Shift+G grouped the device"
    );
    press(&mut app, &ctx, Key::D, M::COMMAND);
    assert_eq!(chain(&app), 2, "Ctrl+D still duplicates");
}
