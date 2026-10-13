//! Left-hand browser: categories (Sounds, Drums, Instruments, Audio Effects,
//! Samples, Places), a folder tree for user folders, sample preview and drag
//! and drop of devices and samples onto the rest of the app.

use std::path::{Path, PathBuf};

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};
use tessera_engine::fx::DeviceKind;
use tessera_engine::library::{self, Preset};

use super::theme;
use crate::app::TesseraApp;
use crate::browser::{file_label, BrowserItem, Category, DropTarget};

const ROW_H: f32 = 18.0;
const INDENT: f32 = 12.0;

/// What a row asked for this frame.
enum RowAct {
    Select(String),
    Preview(PathBuf),
    Load(BrowserItem),
    AddPlace,
    RemovePlace(PathBuf),
    RackFromFolder(PathBuf),
    Toggle(PathBuf),
    RescanPlugins,
    AddPluginFolder,
    RemovePluginFolder(PathBuf),
}

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let mut acts: Vec<RowAct> = Vec::new();
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("BROWSER").small().color(theme::TEXT_DIM));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("⟳").on_hover_text("Rescan folders").clicked() {
                app.browser.refresh();
            }
        });
    });
    ui.add(
        egui::TextEdit::singleline(&mut app.browser.search)
            .hint_text("Search")
            .desired_width(f32::INFINITY),
    );
    ui.add_space(2.0);

    // Category list.
    for cat in Category::ALL {
        let on = app.browser.category == cat;
        let text = egui::RichText::new(cat.label()).color(if on { Color32::BLACK } else { theme::TEXT });
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
        if on {
            ui.painter().rect_filled(rect, 2.0, theme::ACCENT);
        } else if resp.hovered() {
            ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(0x44, 0x44, 0x44));
        }
        ui.painter().text(
            pos2(rect.left() + 6.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            text.text(),
            egui::FontId::proportional(12.0),
            if on { Color32::BLACK } else { theme::TEXT },
        );
        if resp.clicked() {
            app.browser.category = cat;
        }
    }
    ui.separator();

    // Preview strip at the bottom.
    egui::Panel::bottom("browser_preview")
        .frame(egui::Frame::new().inner_margin(egui::Margin::symmetric(0, 4)))
        .show(ui, |ui| preview_strip(app, ui));

    egui::ScrollArea::vertical()
        .id_salt("browser_items")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let q = app.browser.search.trim().to_lowercase();
            match app.browser.category {
                Category::Sounds => preset_groups(app, ui, &library::sounds(), &q, &mut acts),
                Category::Drums => {
                    preset_groups(app, ui, &library::drum_kits(), &q, &mut acts);
                    if let Some(dir) = app.browser.factory_dir.clone() {
                        ui.add_space(4.0);
                        group_label(ui, "Drum Hits");
                        folder_contents(app, ui, &dir.join("Drums"), &q, 1, &mut acts);
                    }
                }
                Category::Instruments => preset_groups(app, ui, &library::instruments(), &q, &mut acts),
                Category::AudioEffects => {
                    for kind in DeviceKind::ALL {
                        if !q.is_empty() && !kind.name().to_lowercase().contains(&q) {
                            continue;
                        }
                        let key = format!("fx:{}", kind.name());
                        item_row(
                            app,
                            ui,
                            &key,
                            kind.name(),
                            0,
                            BrowserItem::Effect(kind),
                            kind.description(),
                            &mut acts,
                        );
                    }
                }
                Category::Plugins => plugin_list(app, ui, &q, &mut acts),
                Category::Samples => match app.browser.factory_dir.clone() {
                    Some(dir) => folder_contents(app, ui, &dir, &q, 0, &mut acts),
                    None => {
                        ui.label(
                            egui::RichText::new("The factory library could not be written.")
                                .small()
                                .color(theme::TEXT_DIM),
                        );
                    }
                },
                Category::Places => {
                    let places = app.browser.places.clone();
                    for place in places {
                        folder_row(app, ui, &place, 0, true, &mut acts);
                        if app.browser.expanded.contains(&place) || !q.is_empty() {
                            folder_contents(app, ui, &place, &q, 1, &mut acts);
                        }
                    }
                    ui.add_space(6.0);
                    if ui.button("+ Add Folder…").clicked() {
                        acts.push(RowAct::AddPlace);
                    }
                }
            }
        });

    for a in acts {
        match a {
            RowAct::Select(k) => app.browser.selected = Some(k),
            RowAct::Preview(p) => app.preview_sample(&p),
            RowAct::Load(item) => app.load_item_on_selected(&item),
            RowAct::AddPlace => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    app.browser.add_place(dir);
                }
            }
            RowAct::RemovePlace(p) => app.browser.remove_place(&p),
            RowAct::RackFromFolder(dir) => {
                let files: Vec<PathBuf> = app
                    .browser
                    .list(&dir)
                    .iter()
                    .filter(|e| !e.is_dir)
                    .map(|e| e.path.clone())
                    .collect();
                if files.is_empty() {
                    app.status = "No WAV files in that folder".into();
                } else {
                    app.drum_rack_from_samples(&file_label(&dir), &files);
                }
            }
            RowAct::RescanPlugins => app.plugins.start_scan(),
            RowAct::AddPluginFolder => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    app.plugins.add_folder(dir);
                }
            }
            RowAct::RemovePluginFolder(p) => app.plugins.remove_folder(&p),
            RowAct::Toggle(p) => {
                if !app.browser.expanded.remove(&p) {
                    app.browser.expanded.insert(p);
                }
            }
        }
    }
}

/// Installed VST3 and CLAP plug-ins, instruments first, plus scan controls.
fn plugin_list(app: &mut TesseraApp, ui: &mut egui::Ui, q: &str, acts: &mut Vec<RowAct>) {
    if let Some((done, total, current)) = app.plugins.scanning() {
        ui.label(
            egui::RichText::new(format!("Scanning {done}/{total} {current}"))
                .small()
                .color(theme::TEXT_DIM),
        );
        ui.ctx().request_repaint();
    }
    let plugins = app.plugins.plugins();
    for (instrument, title) in [(true, "Instruments"), (false, "Audio Effects")] {
        let items: Vec<_> = plugins
            .iter()
            .filter(|p| p.instrument == instrument)
            .filter(|p| q.is_empty() || p.name.to_lowercase().contains(q) || p.vendor.to_lowercase().contains(q))
            .collect();
        if items.is_empty() {
            continue;
        }
        group_label(ui, title);
        for p in items {
            let key = format!("plugin:{}:{}:{}", p.format.label(), p.path.display(), p.id);
            let tip = format!(
                "{} · {} {}\n{}",
                p.vendor,
                p.format.label(),
                p.version,
                p.path.display()
            );
            item_row(
                app,
                ui,
                &key,
                &p.name,
                0,
                BrowserItem::Plugin(Box::new(p.clone())),
                &tip,
                acts,
            );
        }
    }
    if plugins.is_empty() && app.plugins.scanning().is_none() {
        ui.label(
            egui::RichText::new("No VST3 or CLAP plug-ins found.")
                .small()
                .color(theme::TEXT_DIM),
        );
    }
    let failures: Vec<String> = app
        .plugins
        .cache
        .failures()
        .map(|e| format!("{}: {}", file_label(&e.path), e.error.clone().unwrap_or_default()))
        .collect();
    if !failures.is_empty() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(format!("{} files could not be loaded", failures.len()))
                .small()
                .color(theme::TEXT_DIM),
        )
        .on_hover_text(failures.join("\n"));
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Rescan").clicked() {
            acts.push(RowAct::RescanPlugins);
        }
        if ui.button("+ Add Folder…").clicked() {
            acts.push(RowAct::AddPluginFolder);
        }
    });
    for dir in app.plugins.folders.clone() {
        let resp = ui.label(
            egui::RichText::new(dir.display().to_string())
                .small()
                .color(theme::TEXT_DIM),
        );
        resp.context_menu(|ui| {
            if ui.button("Stop scanning this folder").clicked() {
                acts.push(RowAct::RemovePluginFolder(dir.clone()));
                ui.close();
            }
        });
    }
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "Tessera hosts VST3 and CLAP plug-ins. VST2 isn't supported; install the VST3 or CLAP version instead.",
        )
        .small()
        .color(theme::TEXT_DIM),
    )
    .on_hover_text(
        app.plugins
            .all_folders()
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

fn group_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(3.0);
    ui.label(egui::RichText::new(text).small().strong().color(theme::TEXT_DIM));
    ui.add_space(1.0);
}

/// Presets grouped by their category, filtered by the search text.
fn preset_groups(app: &mut TesseraApp, ui: &mut egui::Ui, presets: &[Preset], q: &str, acts: &mut Vec<RowAct>) {
    let mut cats: Vec<&str> = presets.iter().map(|p| p.category).collect();
    cats.dedup();
    let mut seen = Vec::new();
    for cat in cats {
        if seen.contains(&cat) {
            continue;
        }
        seen.push(cat);
        let items: Vec<&Preset> = presets
            .iter()
            .filter(|p| p.category == cat)
            .filter(|p| q.is_empty() || p.name.to_lowercase().contains(q) || cat.to_lowercase().contains(q))
            .collect();
        if items.is_empty() {
            continue;
        }
        let single_group = presets.iter().all(|p| p.category == presets[0].category);
        if !single_group {
            group_label(ui, cat);
        }
        for p in items {
            let key = format!("preset:{}/{}", p.category, p.name);
            item_row(
                app,
                ui,
                &key,
                p.name,
                0,
                BrowserItem::Instrument {
                    name: p.name.to_string(),
                    instrument: Box::new(p.instrument),
                },
                p.instrument.name(),
                acts,
            );
        }
    }
}

/// A draggable row. Click selects, double-click loads onto the selected track.
#[allow(clippy::too_many_arguments)]
fn item_row(
    app: &TesseraApp,
    ui: &mut egui::Ui,
    key: &str,
    label: &str,
    depth: usize,
    item: BrowserItem,
    tip: &str,
    acts: &mut Vec<RowAct>,
) {
    let selected = app.browser.selected.as_deref() == Some(key);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click_and_drag());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 2.0, Color32::from_rgb(0x55, 0x55, 0x55));
    } else if resp.hovered() {
        p.rect_filled(rect, 2.0, Color32::from_rgb(0x3f, 0x3f, 0x3f));
    }
    let x = rect.left() + 4.0 + depth as f32 * INDENT;
    let icon_color = match item {
        BrowserItem::Instrument { .. } => theme::ACCENT,
        BrowserItem::Effect(_) => theme::SOLO,
        BrowserItem::Sample(_) => theme::METER_LOW,
        BrowserItem::Plugin(ref info) if info.instrument => theme::ACCENT,
        BrowserItem::Plugin(_) => theme::SOLO,
    };
    if let BrowserItem::Plugin(info) = &item {
        p.text(
            pos2(rect.right() - 4.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            info.format.label(),
            egui::FontId::proportional(9.0),
            theme::TEXT_DIM,
        );
    }
    p.circle_filled(pos2(x + 3.0, rect.center().y), 2.5, icon_color);
    p.text(
        pos2(x + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        theme::TEXT,
    );
    let hint = "Drag onto a track, clip slot or the Device view · Double-click: load on the selected track";
    let resp = resp.on_hover_text(if tip.is_empty() {
        hint.to_string()
    } else {
        format!("{tip}\n{hint}")
    });
    if resp.drag_started() {
        egui::DragAndDrop::set_payload(ui.ctx(), item.clone());
    }
    if resp.clicked() {
        acts.push(RowAct::Select(key.to_string()));
        if let BrowserItem::Sample(path) = &item {
            if app.browser.auto_preview {
                acts.push(RowAct::Preview(path.clone()));
            }
        }
    }
    if resp.double_clicked() {
        acts.push(RowAct::Load(item));
    }
}

/// A folder row with a disclosure triangle.
fn folder_row(app: &TesseraApp, ui: &mut egui::Ui, dir: &Path, depth: usize, is_place: bool, acts: &mut Vec<RowAct>) {
    let open = app.browser.expanded.contains(dir);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 2.0, Color32::from_rgb(0x3f, 0x3f, 0x3f));
    }
    let x = rect.left() + 4.0 + depth as f32 * INDENT;
    let c = pos2(x + 4.0, rect.center().y);
    let tri = if open {
        vec![
            pos2(c.x - 4.0, c.y - 2.0),
            pos2(c.x + 4.0, c.y - 2.0),
            pos2(c.x, c.y + 3.0),
        ]
    } else {
        vec![
            pos2(c.x - 2.0, c.y - 4.0),
            pos2(c.x + 3.0, c.y),
            pos2(c.x - 2.0, c.y + 4.0),
        ]
    };
    p.add(egui::Shape::convex_polygon(tri, theme::TEXT_DIM, Stroke::NONE));
    let name = if is_place {
        dir.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| dir.display().to_string())
    } else {
        file_label(dir)
    };
    p.text(
        pos2(x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        name,
        egui::FontId::proportional(12.0),
        if is_place { theme::TEXT } else { theme::TEXT_DIM },
    );
    let resp = if is_place {
        resp.on_hover_text(dir.display().to_string())
    } else {
        resp
    };
    if resp.clicked() {
        acts.push(RowAct::Toggle(dir.to_path_buf()));
    }
    resp.context_menu(|ui| {
        if ui.button("Create Drum Rack from folder").clicked() {
            acts.push(RowAct::RackFromFolder(dir.to_path_buf()));
            ui.close();
        }
        if is_place && ui.button("Remove from Places").clicked() {
            acts.push(RowAct::RemovePlace(dir.to_path_buf()));
            ui.close();
        }
    });
}

/// The contents of a folder: sub-folders (expandable) and audio files. With a
/// search query, a flat list of matching files instead.
fn folder_contents(app: &mut TesseraApp, ui: &mut egui::Ui, dir: &Path, q: &str, depth: usize, acts: &mut Vec<RowAct>) {
    if !q.is_empty() {
        let mut found = Vec::new();
        app.browser.search_files(dir, q, &mut found, 4);
        for f in found {
            sample_row(app, ui, &f, depth, acts);
        }
        return;
    }
    let entries = app.browser.list(dir).to_vec();
    if entries.is_empty() && depth > 0 {
        ui.label(egui::RichText::new("   (no WAV files)").small().color(theme::TEXT_DIM));
    }
    for e in entries {
        if e.is_dir {
            folder_row(app, ui, &e.path, depth, false, acts);
            if app.browser.expanded.contains(&e.path) {
                folder_contents(app, ui, &e.path, q, depth + 1, acts);
            }
        } else {
            sample_row(app, ui, &e.path, depth, acts);
        }
    }
}

fn sample_row(app: &TesseraApp, ui: &mut egui::Ui, path: &Path, depth: usize, acts: &mut Vec<RowAct>) {
    let key = format!("file:{}", path.display());
    item_row(
        app,
        ui,
        &key,
        &file_label(path),
        depth,
        BrowserItem::Sample(path.to_path_buf()),
        "",
        acts,
    );
}

/// Preview toggle and the waveform of the last previewed sample.
fn preview_strip(app: &mut TesseraApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let on = app.browser.auto_preview;
        if super::widgets::toggle(ui, on, "Preview", theme::ACCENT, 58.0)
            .on_hover_text("Play samples when you click them")
            .clicked()
        {
            app.browser.auto_preview = !on;
            if on {
                app.stop_preview();
            }
        }
        if app.browser.previewing.is_some() && ui.small_button("■").on_hover_text("Stop preview").clicked() {
            app.stop_preview();
        }
    });
    let Some((path, data)) = app.browser.previewing.clone() else {
        return;
    };
    let peaks = app.waveform_peaks(&data);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 36.0), Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, theme::BG_VIEW);
    draw_peaks(&p, rect, &peaks, theme::METER_LOW);
    if resp.on_hover_text("Click to play again").clicked() {
        app.preview_sample(&path);
    }
    ui.label(
        egui::RichText::new(format!(
            "{} · {:.2} s · {} Hz",
            file_label(&path),
            data.duration_secs(),
            data.sample_rate
        ))
        .small()
        .color(theme::TEXT_DIM),
    );
}

/// Draws a peak envelope (0..1 values) filling `rect`.
pub fn draw_peaks(p: &egui::Painter, rect: Rect, peaks: &[f32], color: Color32) {
    if peaks.is_empty() {
        return;
    }
    let w = rect.width().max(1.0) as usize;
    let mid = rect.center().y;
    let h = rect.height() * 0.48;
    for x in 0..w {
        let a = x * peaks.len() / w;
        let b = ((x + 1) * peaks.len() / w).max(a + 1).min(peaks.len());
        let v = peaks[a..b].iter().fold(0.0f32, |m, v| m.max(*v)).min(1.0);
        let px = rect.left() + x as f32 + 0.5;
        p.line_segment([pos2(px, mid - v * h), pos2(px, mid + v * h)], Stroke::new(1.0, color));
    }
}

// ----- drag and drop -------------------------------------------------------------

/// True while a browser item is being dragged.
pub fn dragging(ctx: &egui::Context) -> bool {
    egui::DragAndDrop::has_payload_of_type::<BrowserItem>(ctx)
}

/// Checks whether a browser item is being dragged over `rect`, highlighting
/// it, and returns the item if it was released there this frame.
pub fn drop_on(ui: &egui::Ui, rect: Rect) -> Option<BrowserItem> {
    let ctx = ui.ctx();
    if !dragging(ctx) {
        return None;
    }
    let pos = ctx.pointer_latest_pos()?;
    if !rect.contains(pos) || !ui.clip_rect().contains(pos) {
        return None;
    }
    let frame = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("browser_drop_hit"), frame));
    ui.painter()
        .rect_stroke(rect, 2.0, Stroke::new(2.0, theme::ACCENT), StrokeKind::Inside);
    if ctx.input(|i| i.pointer.any_released()) {
        egui::DragAndDrop::take_payload::<BrowserItem>(ctx).map(|item| (*item).clone())
    } else {
        None
    }
}

/// Draws the dragged item's name next to the pointer.
pub fn drag_preview(ctx: &egui::Context) {
    let Some(item) = egui::DragAndDrop::payload::<BrowserItem>(ctx) else {
        return;
    };
    let Some(pos) = ctx.pointer_latest_pos() else { return };
    egui::Area::new(egui::Id::new("browser_drag"))
        .order(egui::Order::Tooltip)
        .fixed_pos(pos + vec2(12.0, 8.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(Color32::from_rgb(0x50, 0x50, 0x50))
                .corner_radius(3.0)
                .inner_margin(egui::Margin::symmetric(6, 3))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new(item.label()).color(theme::TEXT));
                });
        });
}

/// Applies a drop that landed on empty space in the main view.
/// Only applies when no other drop target is under the pointer.
pub fn drop_on_empty(app: &mut TesseraApp, ui: &egui::Ui, rect: Rect) {
    let ctx = ui.ctx();
    let frame = ctx.cumulative_pass_nr();
    let hit = ctx.data(|d| d.get_temp::<u64>(egui::Id::new("browser_drop_hit")));
    if hit == Some(frame) {
        return;
    }
    if let Some(item) = drop_on(ui, rect) {
        app.drop_item(&item, DropTarget::NewTrack);
    }
}
