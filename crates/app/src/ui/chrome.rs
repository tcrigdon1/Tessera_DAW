//! Application chrome around the views: the Help menu, the About and Rename
//! windows, extra context-menu items for tracks, clips and devices, the
//! startup splash and the first-run tour.

use eframe::egui::{self, pos2, vec2, Align2, Color32, Id as EguiId, Rect, RichText, Sense, Stroke, StrokeKind};
use tessera_engine::automation::AutomationTarget;
use tessera_engine::dsp::synth::SynthParams;
use tessera_engine::model::{Id, Instrument, TrackKind};

use super::info_view::{self, InfoExt};
use super::theme;
use crate::app::{ClipRef, DetailMode, TesseraApp, View};

/// How long the splash stays up, in seconds (the last part fades out).
const SPLASH_SECS: f64 = 1.5;
const SPLASH_FADE: f64 = 0.4;

/// Track and clip colours offered in context menus (two rows of seven).
pub const PALETTE: [[u8; 3]; 14] = [
    [255, 148, 166],
    [255, 165, 41],
    [204, 153, 39],
    [247, 244, 124],
    [191, 251, 0],
    [26, 255, 47],
    [37, 255, 168],
    [92, 255, 232],
    [139, 197, 255],
    [84, 128, 228],
    [146, 167, 255],
    [195, 152, 255],
    [216, 108, 228],
    [255, 54, 54],
];

/// UI state of the chrome (one field on [`TesseraApp`]).
pub struct ChromeState {
    /// Info View in the bottom-left corner.
    pub show_info: bool,
    /// Show control descriptions as tooltips while the Info View is hidden.
    pub tooltips: bool,
    /// Key Commands window.
    pub show_keys: bool,
    pub keys_search: String,
    pub(crate) focus_keys_search: bool,
    /// About Tessera window.
    pub show_about: bool,
    /// The first-run tour has been finished or skipped. Persist this.
    pub tour_done: bool,
    /// Current tour step while the tour runs.
    pub tour_step: Option<usize>,
    /// Start the tour by itself when `tour_done` is false (off with
    /// `TESSERA_NO_TOUR`).
    tour_auto: bool,
    /// The tour step whose target has been brought into view.
    tour_prepared: Option<usize>,
    /// Splash: shown until this time (set on the first frame), or never.
    splash: Splash,
    /// Open Rename window: what is renamed and the text being edited.
    rename: Option<(RenameTarget, String)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Splash {
    Pending,
    Until(f64),
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenameTarget {
    Track(Id),
    Clip(ClipRef),
}

impl Default for ChromeState {
    fn default() -> Self {
        ChromeState {
            show_info: true,
            tooltips: true,
            show_keys: false,
            keys_search: String::new(),
            focus_keys_search: false,
            show_about: false,
            tour_done: false,
            tour_step: None,
            tour_prepared: None,
            tour_auto: std::env::var_os("TESSERA_NO_TOUR").is_none(),
            splash: if std::env::var_os("TESSERA_NO_SPLASH").is_some() {
                Splash::Done
            } else {
                Splash::Pending
            },
            rename: None,
        }
    }
}

impl ChromeState {
    /// Starts (or restarts) the first-run tour.
    pub fn start_tour(&mut self) {
        self.tour_step = Some(0);
        self.tour_prepared = None;
    }

    /// Ends the tour and remembers that it has been seen.
    pub fn end_tour(&mut self) {
        self.tour_step = None;
        self.tour_done = true;
    }

    pub fn toggle_key_commands(&mut self) {
        self.show_keys = !self.show_keys;
        self.focus_keys_search = self.show_keys;
    }

    fn splash_active(&self) -> bool {
        self.splash != Splash::Done
    }
}

// ----- frame hooks -------------------------------------------------------------

/// Call at the start of [`super::draw`].
pub fn begin_frame(app: &mut TesseraApp, ctx: &egui::Context) {
    info_view::begin_frame(ctx, app.chrome.tooltips && !app.chrome.show_info);
}

/// Call at the end of [`super::draw`]: windows, overlays and the actions
/// queued by context menus.
pub fn end_frame(app: &mut TesseraApp, ctx: &egui::Context) {
    for a in take_actions(ctx) {
        app.apply_chrome_action(a);
    }
    super::key_commands::window(app, ctx);
    about_window(app, ctx);
    rename_window(app, ctx);
    if app.chrome.splash_active() {
        splash(app, ctx);
    } else if app.chrome.tour_step.is_none() && !app.chrome.tour_done && app.chrome.tour_auto {
        app.chrome.tour_auto = false;
        app.chrome.start_tour();
    }
    if !app.chrome.splash_active() {
        tour(app, ctx);
    }
}

// ----- Help menu ---------------------------------------------------------------

pub fn help_menu(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let r = ui.menu_button("Help", |ui| {
        ui.checkbox(&mut app.chrome.show_info, "Info View  (Shift + ?)");
        ui.add_enabled_ui(!app.chrome.show_info, |ui| {
            ui.checkbox(&mut app.chrome.tooltips, "Tooltips")
                .on_disabled_hover_text("Descriptions show in the Info View while it is open");
        });
        ui.separator();
        if ui.add(egui::Button::new("Key Commands…").shortcut_text("F1")).clicked() {
            app.chrome.show_keys = true;
            app.chrome.focus_keys_search = true;
            ui.close();
        }
        if ui.button("Show Tour").clicked() {
            app.chrome.start_tour();
            ui.close();
        }
        ui.separator();
        if ui.button("About Tessera").clicked() {
            app.chrome.show_about = true;
            ui.close();
        }
    });
    r.response.info(
        "Help Menu",
        "Info View and tooltips, the list of key commands, the tour of Tessera and the About box.",
    );
}

fn about_window(app: &mut TesseraApp, ctx: &egui::Context) {
    if !app.chrome.show_about {
        return;
    }
    let mut open = true;
    egui::Window::new("About Tessera")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                let (mark, _) = ui.allocate_exact_size(vec2(36.0, 36.0), Sense::hover());
                tiles(ui.painter(), mark, 1.0);
                ui.vertical(|ui| {
                    ui.label(RichText::new("Tessera").size(22.0).strong().color(theme::text()));
                    ui.label(RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION"))).color(theme::text_dim()));
                });
            });
            ui.add_space(6.0);
            ui.label("An original digital audio workstation written in Rust.");
            ui.label(RichText::new("Released under the MIT license.").color(theme::text_dim()));
            ui.add_space(6.0);
            ui.label(RichText::new("BUILT WITH").small().strong().color(theme::ACCENT));
            for (name, what, url) in CREDITS {
                ui.horizontal(|ui| {
                    ui.hyperlink_to(*name, *url);
                    ui.label(RichText::new(*what).color(theme::text_dim()));
                });
            }
        });
    if !open {
        app.chrome.show_about = false;
    }
}

const CREDITS: &[(&str, &str, &str)] = &[
    ("egui / eframe", "user interface", "https://github.com/emilk/egui"),
    ("cpal", "audio input and output", "https://github.com/RustAudio/cpal"),
    ("hound", "WAV files", "https://github.com/ruuda/hound"),
    ("vst3", "VST3 bindings", "https://crates.io/crates/vst3"),
    ("clap-sys", "CLAP bindings", "https://crates.io/crates/clap-sys"),
    ("rtrb", "lock-free ring buffers", "https://crates.io/crates/rtrb"),
    ("rfd", "file dialogs", "https://crates.io/crates/rfd"),
];

// ----- context menus -----------------------------------------------------------

/// What a context-menu item asks for. Menus are drawn while the views hold
/// borrows of the project, so the request waits in egui's temp data until
/// [`end_frame`].
#[derive(Clone, Debug)]
pub enum ChromeAction {
    Rename(RenameTarget),
    DuplicateTrack(Id),
    DeleteTrack(Id),
    TrackColor(Id, [u8; 3]),
    InsertTrack { after: Id, kind: TrackKind },
    ClipColor(ClipRef, [u8; 3]),
    DuplicateArrangementClip { track: Id, id: Id },
}

fn actions_id() -> EguiId {
    EguiId::new("tessera_chrome_actions")
}

pub fn queue(ctx: &egui::Context, a: ChromeAction) {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Vec<ChromeAction>>(actions_id()).push(a));
}

fn take_actions(ctx: &egui::Context) -> Vec<ChromeAction> {
    ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<Vec<ChromeAction>>(actions_id())))
}

/// Track header menu items: rename (unless the menu edits the name inline),
/// duplicate, delete (unless the menu has its own), colour and insert track.
pub fn track_menu_items(ui: &mut egui::Ui, track: Id, rename: bool, delete: bool) {
    let ctx = ui.ctx().clone();
    if rename && ui.button("Rename…").clicked() {
        queue(&ctx, ChromeAction::Rename(RenameTarget::Track(track)));
        ui.close();
    }
    if ui.button("Duplicate Track").clicked() {
        queue(&ctx, ChromeAction::DuplicateTrack(track));
        ui.close();
    }
    if delete && ui.button("Delete Track").clicked() {
        queue(&ctx, ChromeAction::DeleteTrack(track));
        ui.close();
    }
    ui.separator();
    if let Some(c) = palette(ui) {
        queue(&ctx, ChromeAction::TrackColor(track, c));
        ui.close();
    }
    ui.separator();
    if ui.button("Insert MIDI Track").clicked() {
        queue(
            &ctx,
            ChromeAction::InsertTrack {
                after: track,
                kind: TrackKind::Midi,
            },
        );
        ui.close();
    }
    if ui.button("Insert Audio Track").clicked() {
        queue(
            &ctx,
            ChromeAction::InsertTrack {
                after: track,
                kind: TrackKind::Audio,
            },
        );
        ui.close();
    }
}

/// Extra items for an Arrangement clip's menu (it already has Delete).
pub fn arrangement_clip_menu_items(ui: &mut egui::Ui, track: Id, id: Id) {
    let ctx = ui.ctx().clone();
    let r = ClipRef::Arrangement { track, id };
    if ui.button("Rename…").clicked() {
        queue(&ctx, ChromeAction::Rename(RenameTarget::Clip(r)));
        ui.close();
    }
    if ui.button("Duplicate").clicked() {
        queue(&ctx, ChromeAction::DuplicateArrangementClip { track, id });
        ui.close();
    }
    ui.separator();
    if let Some(c) = palette(ui) {
        queue(&ctx, ChromeAction::ClipColor(r, c));
        ui.close();
    }
    ui.separator();
}

/// A 7 × 2 grid of colour swatches. Returns the clicked colour.
pub fn palette(ui: &mut egui::Ui) -> Option<[u8; 3]> {
    let mut picked = None;
    ui.label(RichText::new("Colour").small().color(theme::text_dim()));
    for row in PALETTE.chunks(7) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for &c in row {
                let (rect, resp) = ui.allocate_exact_size(vec2(16.0, 14.0), Sense::click());
                ui.painter().rect_filled(rect, 2.0, theme::rgb(c));
                if resp.hovered() {
                    ui.painter()
                        .rect_stroke(rect, 2.0, Stroke::new(1.5, theme::select()), StrokeKind::Outside);
                }
                if resp.clicked() {
                    picked = Some(c);
                }
            }
        });
    }
    picked
}

impl TesseraApp {
    pub fn apply_chrome_action(&mut self, a: ChromeAction) {
        match a {
            ChromeAction::Rename(target) => {
                let name = match target {
                    RenameTarget::Track(id) => self.project.track(id).map(|t| t.name.clone()),
                    RenameTarget::Clip(r) => self.clip(r).map(|c| c.name.clone()),
                };
                if let Some(name) = name {
                    self.chrome.rename = Some((target, name));
                }
            }
            ChromeAction::DuplicateTrack(id) => {
                self.duplicate_track(id);
            }
            ChromeAction::DeleteTrack(id) => self.delete_track(id),
            ChromeAction::TrackColor(id, c) => {
                if let Some(t) = self.project.track_mut(id) {
                    t.color = c;
                    self.changed();
                }
            }
            ChromeAction::InsertTrack { after, kind } => {
                let id = match kind {
                    TrackKind::Midi => self.add_track(kind, Instrument::Synth(SynthParams::default())),
                    TrackKind::Audio => self.add_audio_track(),
                };
                self.move_track_after(id, after);
            }
            ChromeAction::ClipColor(r, c) => {
                if let Some(clip) = self.clip_mut(r) {
                    clip.color = c;
                    self.changed();
                }
            }
            ChromeAction::DuplicateArrangementClip { track, id } => self.duplicate_arrangement_clip(track, id),
        }
    }

    /// Moves track `id` to just after track `after`.
    pub fn move_track_after(&mut self, id: Id, after: Id) {
        let Some(from) = self.project.track_index(id) else {
            return;
        };
        let t = self.project.tracks.remove(from);
        let to = self
            .project
            .track_index(after)
            .map(|i| i + 1)
            .unwrap_or(self.project.tracks.len());
        self.project.tracks.insert(to, t);
        self.changed();
    }

    /// Copies a track with its clips, devices, automation and plug-in
    /// states, and puts the copy right after it. Returns the new track's id.
    pub fn duplicate_track(&mut self, id: Id) -> Option<Id> {
        let index = self.project.track_index(id)?;
        // Take the running plug-ins' current state along.
        self.plugins.store_states(&mut self.project);
        let mut t = self.project.tracks[index].clone();
        let new_id = self.project.alloc_id();
        let mut plugin_slots = Vec::new();
        if matches!(t.instrument, Instrument::Plugin(_)) {
            if let Some(mut slot) = self.project.plugin(id).cloned() {
                slot.slot = new_id;
                plugin_slots.push(slot);
            }
        }
        t.id = new_id;
        // Devices get new ids; automation addressing them follows.
        let mut device_ids = Vec::new();
        for d in &mut t.devices {
            let old = d.id;
            d.id = self.project.alloc_id();
            device_ids.push((old, d.id));
            if let Some(mut slot) = self.project.plugin(old).cloned() {
                slot.slot = d.id;
                plugin_slots.push(slot);
            }
        }
        let remap = |target: &mut AutomationTarget| {
            if let AutomationTarget::Device { device, .. } = target {
                if let Some(&(_, new)) = device_ids.iter().find(|(old, _)| old == device) {
                    *device = new;
                }
            }
        };
        for lane in &mut t.automation {
            remap(&mut lane.target);
        }
        for clip in t.slots.iter_mut().flatten() {
            clip.id = self.project.alloc_id();
            for e in &mut clip.envelopes {
                remap(&mut e.target);
            }
        }
        for a in &mut t.arrangement {
            a.id = self.project.alloc_id();
            a.clip.id = self.project.alloc_id();
            for e in &mut a.clip.envelopes {
                remap(&mut e.target);
            }
        }
        // A copy that records over the original's input is rarely wanted.
        t.mixer.arm = false;
        self.project.tracks.insert(index + 1, t);
        self.project.plugins.extend(plugin_slots);
        self.selected_track = Some(new_id);
        self.status = "Track duplicated".into();
        self.changed();
        Some(new_id)
    }

    /// Copies an Arrangement clip to just after itself.
    pub fn duplicate_arrangement_clip(&mut self, track: Id, id: Id) {
        let Some(mut a) = self
            .project
            .track(track)
            .and_then(|t| t.arrangement.iter().find(|a| a.id == id))
            .cloned()
        else {
            return;
        };
        a.start += a.length;
        a.id = self.project.alloc_id();
        a.clip.id = self.project.alloc_id();
        let new = a.id;
        if let Some(t) = self.project.track_mut(track) {
            t.arrangement.push(a);
            t.sort_arrangement();
        }
        self.select_clip(ClipRef::Arrangement { track, id: new });
        self.changed();
    }
}

fn rename_window(app: &mut TesseraApp, ctx: &egui::Context) {
    let Some((target, mut text)) = app.chrome.rename.take() else {
        return;
    };
    let mut open = true;
    let mut cancel = false;
    let mut done = false;
    let title = match target {
        RenameTarget::Track(_) => "Rename Track",
        RenameTarget::Clip(_) => "Rename Clip",
    };
    egui::Window::new(title)
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, vec2(0.0, -80.0))
        .show(ctx, |ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(240.0));
            if !r.has_focus() && !r.lost_focus() {
                r.request_focus();
            }
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.horizontal(|ui| {
                if ui.button("OK").clicked() || enter {
                    done = true;
                }
                if ui.button("Cancel").clicked() {
                    cancel = true;
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                cancel = true;
            }
        });
    if done {
        match target {
            RenameTarget::Track(id) => {
                if let Some(t) = app.project.track_mut(id) {
                    t.name = text;
                }
            }
            RenameTarget::Clip(r) => {
                if let Some(c) = app.clip_mut(r) {
                    c.name = text;
                }
            }
        }
        app.changed();
    } else if open && !cancel {
        app.chrome.rename = Some((target, text));
    }
}

// ----- splash ------------------------------------------------------------------

/// A little mosaic, Tessera's mark.
fn tiles(p: &egui::Painter, rect: Rect, alpha: f32) {
    let n = 4;
    let gap = 2.0;
    let size = (rect.width().min(rect.height()) - gap * (n - 1) as f32) / n as f32;
    for row in 0..n {
        for col in 0..n {
            let c = PALETTE[(row * 3 + col * 5) % PALETTE.len()];
            let r = Rect::from_min_size(
                pos2(
                    rect.left() + col as f32 * (size + gap),
                    rect.top() + row as f32 * (size + gap),
                ),
                vec2(size, size),
            );
            p.rect_filled(r, 1.5, theme::rgb(c).gamma_multiply(alpha));
        }
    }
}

fn splash(app: &mut TesseraApp, ctx: &egui::Context) {
    let now = ctx.input(|i| i.time);
    let until = match app.chrome.splash {
        Splash::Pending => {
            app.chrome.splash = Splash::Until(now + SPLASH_SECS);
            now + SPLASH_SECS
        }
        Splash::Until(t) => t,
        Splash::Done => return,
    };
    if now >= until {
        app.chrome.splash = Splash::Done;
        return;
    }
    let alpha = ((until - now) / SPLASH_FADE).min(1.0) as f32;
    let screen = ctx.content_rect();
    egui::Area::new(EguiId::new("tessera_splash"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .show(ctx, |ui| {
            let resp = ui.allocate_rect(screen, Sense::click());
            let p = ui.painter();
            p.rect_filled(screen, 0.0, Color32::from_black_alpha((170.0 * alpha) as u8));
            let card = Rect::from_center_size(screen.center(), vec2(380.0, 170.0));
            p.rect(
                card,
                6.0,
                theme::bg_panel().gamma_multiply(alpha),
                Stroke::new(1.0, theme::dim(theme::ACCENT, 0.7).gamma_multiply(alpha)),
                StrokeKind::Inside,
            );
            tiles(
                p,
                Rect::from_min_size(pos2(card.left() + 28.0, card.top() + 34.0), vec2(72.0, 72.0)),
                alpha,
            );
            let x = card.left() + 124.0;
            p.text(
                pos2(x, card.top() + 52.0),
                Align2::LEFT_CENTER,
                "Tessera",
                egui::FontId::proportional(40.0),
                theme::text().gamma_multiply(alpha),
            );
            p.text(
                pos2(x + 2.0, card.top() + 88.0),
                Align2::LEFT_CENTER,
                format!("Version {}", env!("CARGO_PKG_VERSION")),
                egui::FontId::proportional(13.0),
                theme::text_dim().gamma_multiply(alpha),
            );
            let dots = ".".repeat(((now * 3.0) as usize % 3) + 1);
            p.text(
                pos2(card.left() + 28.0, card.bottom() - 26.0),
                Align2::LEFT_CENTER,
                format!("Loading{dots}"),
                egui::FontId::proportional(12.0),
                theme::ACCENT.gamma_multiply(alpha),
            );
            p.text(
                pos2(card.right() - 16.0, card.bottom() - 26.0),
                Align2::RIGHT_CENTER,
                "Click to continue",
                egui::FontId::proportional(10.0),
                theme::text_dim().gamma_multiply(alpha * 0.8),
            );
            if resp.clicked() {
                app.chrome.splash = Splash::Done;
            }
        });
}

// ----- first-run tour ----------------------------------------------------------

struct Step {
    /// Info View title of the control the callout points at.
    anchor: &'static str,
    title: &'static str,
    body: &'static str,
}

const STEPS: &[Step] = &[
    Step {
        anchor: "Play",
        title: "Transport",
        body: "Tempo, time signature, metronome, play, stop and Arrangement Record live up here, with the loop switch and the launch quantization for clips. Space plays and stops.",
    },
    Step {
        anchor: "Session View",
        title: "Session and Arrangement",
        body: "Session view is a grid of clips you launch live; Arrangement view is a timeline. Switch between them here or with Tab.",
    },
    Step {
        anchor: "Browser Search",
        title: "Browser",
        body: "Instruments, drum kits, effects, plug-ins and samples. Drag anything onto a track, a clip slot or the Device view, or double-click it to load it on the selected track.",
    },
    Step {
        anchor: "Detail View",
        title: "Clip and Device view",
        body: "The selected clip opens here in the piano roll or sample editor. Shift + Tab shows the track's instrument and effects instead.",
    },
    Step {
        anchor: "Track Volume",
        title: "Mixer",
        body: "Every track has a volume fader with a meter, pan, sends to the return tracks, and track activator, solo and arm buttons. Right-click a track's title for more.",
    },
    Step {
        anchor: "Info View",
        title: "Info View",
        body: "Point at any control and this corner says what it does. Shift + ? shows or hides it.",
    },
    Step {
        anchor: "Help Menu",
        title: "Help",
        body: "Key Commands (F1) lists every shortcut. You can take this tour again from here at any time.",
    },
];

/// Gets the app ready to show a step's target.
fn prepare_step(app: &mut TesseraApp, step: usize) {
    match STEPS[step].anchor {
        "Track Volume" => app.view = View::Session,
        "Browser Search" => app.show_browser = true,
        "Info View" => app.chrome.show_info = true,
        "Detail View" if app.selected_clip.is_some() => app.detail = DetailMode::Clip,
        _ => {}
    }
}

fn tour(app: &mut TesseraApp, ctx: &egui::Context) {
    let Some(step) = app.chrome.tour_step else { return };
    if step >= STEPS.len() {
        app.chrome.end_tour();
        return;
    }
    if app.chrome.tour_prepared != Some(step) {
        app.chrome.tour_prepared = Some(step);
        prepare_step(app, step);
    }
    let s = &STEPS[step];
    let screen = ctx.content_rect();
    let target = info_view::anchor(ctx, s.anchor);

    // Outline the target.
    if let Some(r) = target {
        let p = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, EguiId::new("tour_outline")));
        let pulse = 0.6 + 0.4 * ((ctx.input(|i| i.time) * 3.0).sin() as f32 * 0.5 + 0.5);
        p.rect_stroke(
            r.expand(3.0),
            4.0,
            Stroke::new(2.0, theme::ACCENT.gamma_multiply(pulse)),
            StrokeKind::Outside,
        );
    }

    // Place the callout beside the target, towards the middle of the screen.
    let r = target.unwrap_or(Rect::from_center_size(screen.center(), vec2(0.0, 0.0)));
    let below = r.center().y < screen.center().y;
    let left = r.center().x < screen.center().x;
    let x = if left { r.left() } else { r.right() };
    // Narrow targets on the left edge (Browser, Info View) get the callout
    // beside them so it does not cover them.
    let beside = r.left() < screen.width() * 0.2 && r.width() < 320.0;
    let (pivot, pos) = match (below, left) {
        _ if beside && below => (Align2::LEFT_TOP, pos2(r.right() + 12.0, r.top())),
        _ if beside => (Align2::LEFT_BOTTOM, pos2(r.right() + 12.0, r.bottom())),
        (true, true) => (Align2::LEFT_TOP, pos2(x, r.bottom() + 10.0)),
        (true, false) => (Align2::RIGHT_TOP, pos2(x, r.bottom() + 10.0)),
        (false, true) => (Align2::LEFT_BOTTOM, pos2(x, r.top() - 10.0)),
        (false, false) => (Align2::RIGHT_BOTTOM, pos2(x, r.top() - 10.0)),
    };
    let pos = if target.is_none() { screen.center() } else { pos };
    let pivot = if target.is_none() { Align2::CENTER_CENTER } else { pivot };

    let mut next: Option<Option<usize>> = None;
    egui::Window::new(RichText::new(s.title).strong())
        .id(EguiId::new("tessera_tour"))
        .title_bar(true)
        .collapsible(false)
        .resizable(false)
        .order(egui::Order::Foreground)
        .pivot(pivot)
        .fixed_pos(pos)
        .fixed_size(vec2(280.0, 0.0))
        .show(ctx, |ui| {
            ui.label(s.body);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!("{} of {}", step + 1, STEPS.len()))
                        .small()
                        .color(theme::text_dim()),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let last = step + 1 == STEPS.len();
                    if ui.button(if last { "Done" } else { "Next" }).clicked() {
                        next = Some((!last).then_some(step + 1));
                    }
                    if step > 0 && ui.button("Back").clicked() {
                        next = Some(Some(step - 1));
                    }
                    if !last && ui.button("Skip").clicked() {
                        next = Some(None);
                    }
                });
            });
        });
    match next {
        Some(Some(n)) => app.chrome.tour_step = Some(n),
        Some(None) => app.chrome.end_tour(),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tour_steps_point_at_known_anchors() {
        for s in STEPS {
            let known =
                info_view::ANCHOR_TITLES.contains(&s.anchor) || ["Detail View", "Info View"].contains(&s.anchor);
            assert!(known, "{} has no anchor", s.anchor);
        }
    }

    #[test]
    fn palette_has_fourteen_distinct_colours() {
        for (i, a) in PALETTE.iter().enumerate() {
            assert!(PALETTE[i + 1..].iter().all(|b| b != a));
        }
    }

    #[test]
    fn tour_start_and_end() {
        let mut c = ChromeState::default();
        assert!(!c.tour_done);
        c.start_tour();
        assert_eq!(c.tour_step, Some(0));
        c.end_tour();
        assert!(c.tour_done && c.tour_step.is_none());
    }
}
