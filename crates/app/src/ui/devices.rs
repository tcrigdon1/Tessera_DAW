//! Device view: the selected track's instrument followed by its chain of
//! audio effects, left to right, like a rack at the bottom of the screen.

use eframe::egui::{self, pos2, vec2, Color32, Rect, Sense, Stroke, StrokeKind};
use tessera_engine::fx::eq::{self, EqParams};
use tessera_engine::fx::{Category, Device, DeviceKind, DeviceParams, ParamInfo, ParamKind, Scale};
use tessera_engine::model::{Id, InputChannels, Instrument, TrackInput, TrackKind, MASTER_BUS};

use super::plugin_panel::{self, PluginAct, PluginView};
use super::widgets;
use super::{detail, theme};
use crate::app::TesseraApp;

enum Act {
    Param(Id, usize, f32),
    On(Id, bool),
    Move(Id, i32),
    Remove(Id),
    Reset(Id, DeviceParams),
    Add(DeviceKind),
    Input(TrackInput),
    Plugin(PluginAct),
    AddPlugin(Box<tessera_plugin_host::PluginInfo>),
}

const PANEL_BG: Color32 = Color32::from_rgb(0x2b, 0x2b, 0x2b);
const PANEL_OFF: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);

pub fn draw(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let Some(bus) = app.selected_bus() else {
        detail::hint(ui, "Select a track to see its devices.");
        return;
    };
    let track = app.project.track(bus);
    let kind = track.map(|t| t.kind);
    let color = track
        .map(|t| theme::rgb(t.color))
        .or_else(|| {
            app.project
                .returns
                .iter()
                .find(|r| r.id == bus)
                .map(|r| theme::rgb(r.color))
        })
        .unwrap_or(Color32::from_rgb(0x5a, 0x5a, 0x5a));
    let input = track.map(|t| t.input);
    let instrument = track.map(|t| t.instrument);
    let mut acts: Vec<Act> = Vec::new();

    ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(6.0, 18.0), Sense::hover());
        ui.painter().rect_filled(r, 1.0, color);
        ui.label(egui::RichText::new(app.bus_name(bus)).strong());
        match kind {
            Some(TrackKind::Midi) => {
                ui.separator();
                detail::instrument_picker(app, ui, bus);
            }
            Some(TrackKind::Audio) => {
                if let Some(mut input) = input {
                    ui.separator();
                    ui.label(egui::RichText::new("Audio From").small().color(theme::TEXT_DIM));
                    let before = input;
                    egui::ComboBox::from_id_salt(("input", bus))
                        .width(70.0)
                        .selected_text(input.channels.label())
                        .show_ui(ui, |ui| {
                            for c in InputChannels::ALL {
                                ui.selectable_value(&mut input.channels, c, c.label());
                            }
                        });
                    ui.label(egui::RichText::new("Monitor").small().color(theme::TEXT_DIM));
                    for m in [
                        tessera_engine::model::Monitor::In,
                        tessera_engine::model::Monitor::Auto,
                        tessera_engine::model::Monitor::Off,
                    ] {
                        if widgets::toggle(ui, input.monitor == m, m.label(), theme::ACCENT, 34.0).clicked() {
                            input.monitor = m;
                        }
                    }
                    if input != before {
                        acts.push(Act::Input(input));
                    }
                    let connected = app
                        .engine
                        .shared
                        .record
                        .input_connected
                        .load(std::sync::atomic::Ordering::Relaxed);
                    if !connected {
                        ui.label(egui::RichText::new("No input device").small().color(theme::METER_HIGH));
                    }
                }
            }
            None => {}
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let effects: Vec<_> = app.plugins.plugins().into_iter().filter(|p| !p.instrument).collect();
            add_menu(ui, &effects, &mut acts);
        });
    });
    ui.separator();

    let devices: Vec<Device> = app.project.chain(bus).cloned().unwrap_or_default();
    let sr = app.engine.sample_rate();
    egui::ScrollArea::horizontal().id_salt(("devices", bus)).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            match instrument {
                Some(Instrument::Synth(_)) => detail::synth_device(app, ui, bus),
                Some(Instrument::Fm(_)) => super::synth_panels::fm_device(app, ui, bus),
                Some(Instrument::Wavetable(_)) => super::synth_panels::wavetable_device(app, ui, bus),
                Some(Instrument::DrumKit(_)) => detail::drum_device(app, ui, bus),
                Some(Instrument::Sampler(_)) => super::sample_panels::sampler_device(app, ui, bus),
                Some(Instrument::DrumRack(_)) => super::sample_panels::drum_rack_device(app, ui, bus),
                Some(Instrument::Analog(_)) => super::instrument_panels::analog::device(app, ui, bus),
                Some(Instrument::Electric(_)) => super::instrument_panels::electric::device(app, ui, bus),
                Some(Instrument::Mallet(_)) => super::instrument_panels::mallet::device(app, ui, bus),
                Some(Instrument::Simple(_)) => super::instrument_panels::simple::device(app, ui, bus),
                Some(Instrument::Granular(_)) => super::instrument_panels::granular::device(app, ui, bus),
                Some(Instrument::Plugin(_)) => {
                    if let Some(view) = PluginView::of(app, bus) {
                        egui::Frame::new()
                            .fill(PANEL_BG)
                            .corner_radius(3.0)
                            .inner_margin(egui::Margin::same(6))
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    let mut pacts = Vec::new();
                                    plugin_panel::body(ui, &view, &mut pacts);
                                    acts.extend(pacts.into_iter().map(Act::Plugin));
                                });
                            });
                    }
                }
                _ => {}
            }
            for (i, d) in devices.iter().enumerate() {
                let meter = app.engine.shared.device_meter(d.id);
                let view = matches!(d.params, DeviceParams::Plugin(_))
                    .then(|| PluginView::of(app, d.id))
                    .flatten();
                device_panel(ui, d, i, devices.len(), meter, sr, view.as_ref(), &mut acts);
            }
            drop_zone(
                ui,
                bus,
                devices.is_empty() && instrument.is_none_or(|i| i == Instrument::None),
                &mut acts,
            );
        });
    });

    // Browser items dropped anywhere else on the Device view.
    if let Some(item) = super::browser::drop_on(ui, ui.min_rect()) {
        app.drop_item(&item, crate::browser::DropTarget::Device(bus));
    }

    for a in acts {
        match a {
            Act::Param(d, i, v) => app.set_device_param(bus, d, i, v),
            Act::On(d, on) => app.set_device_on(bus, d, on),
            Act::Move(d, delta) => app.move_device(bus, d, delta),
            Act::Remove(d) => app.remove_device(bus, d),
            Act::Reset(d, p) => app.set_device_params(bus, d, p),
            Act::Add(kind) => app.add_device(bus, kind),
            Act::Plugin(p) => app.apply_plugin_act(p),
            Act::AddPlugin(info) => app.add_plugin(bus, &info),
            Act::Input(input) => {
                if let Some(t) = app.project.track_mut(bus) {
                    t.input = input;
                }
                app.send_track_input(bus);
            }
        }
    }
}

fn add_menu(ui: &mut egui::Ui, plugins: &[tessera_plugin_host::PluginInfo], acts: &mut Vec<Act>) {
    ui.menu_button("+ Add Effect", |ui| {
        effect_menu(ui, acts);
        if !plugins.is_empty() {
            ui.separator();
            ui.menu_button("Plug-Ins", |ui| {
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for p in plugins {
                        let label = format!("{} ({})", p.name, p.format.label());
                        if ui.button(label).on_hover_text(&p.vendor).clicked() {
                            acts.push(Act::AddPlugin(Box::new(p.clone())));
                            ui.close();
                        }
                    }
                });
            });
        }
    });
}

fn drop_zone(ui: &mut egui::Ui, bus: Id, empty: bool, acts: &mut Vec<Act>) {
    let h = ui.available_height().clamp(60.0, 190.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(if empty { 260.0 } else { 120.0 }, h), Sense::click());
    let hovered = resp.hovered();
    ui.painter().rect_stroke(
        rect.shrink(2.0),
        3.0,
        Stroke::new(1.0, if hovered { theme::TEXT_DIM } else { theme::GRID_BAR }),
        StrokeKind::Inside,
    );
    let text = if empty {
        if bus == MASTER_BUS {
            "Master: add effects with + Add Effect\nor from the browser"
        } else {
            "No devices. Add effects with\n+ Add Effect or from the browser"
        }
    } else {
        "+ Add Effect"
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(11.0),
        theme::TEXT_DIM,
    );
    let popup_id = ui.id().with(("drop_menu", bus));
    egui::Popup::menu(&resp).id(popup_id).show(|ui| effect_menu(ui, acts));
}

/// The built-in effects, one submenu per category.
fn effect_menu(ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    for cat in Category::ALL {
        let kinds = cat.kinds();
        if kinds.is_empty() {
            continue;
        }
        ui.menu_button(cat.label(), |ui| {
            for kind in kinds {
                if ui.button(kind.name()).on_hover_text(kind.description()).clicked() {
                    acts.push(Act::Add(kind));
                    ui.close();
                }
            }
        });
    }
}

fn to_widget_scale(s: Scale) -> widgets::Scale {
    match s {
        Scale::Linear => widgets::Scale::Linear,
        Scale::Log => widgets::Scale::Log,
    }
}

/// Draws one parameter control. Returns the new value when it changed.
pub(super) fn param_control(ui: &mut egui::Ui, salt: (Id, usize), info: &ParamInfo, value: f32) -> Option<f32> {
    let mut v = value;
    match info.kind {
        ParamKind::Continuous(scale) => {
            let changed = widgets::knob(
                ui,
                info.name,
                &mut v,
                info.min,
                info.max,
                info.default,
                to_widget_scale(scale),
                |x| info.format(x),
            );
            changed.then_some(v)
        }
        ParamKind::Toggle => {
            let mut out = None;
            ui.allocate_ui_with_layout(vec2(52.0, 62.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.set_min_size(vec2(52.0, 62.0));
                ui.add_space(4.0);
                ui.label(egui::RichText::new(info.name).size(10.0).color(theme::TEXT_DIM));
                ui.add_space(6.0);
                let on = v >= 0.5;
                if widgets::toggle(ui, on, if on { "On" } else { "Off" }, theme::ACCENT, 40.0).clicked() {
                    out = Some(if on { 0.0 } else { 1.0 });
                }
            });
            out
        }
        ParamKind::Choice(labels) => {
            let mut out = None;
            ui.allocate_ui_with_layout(vec2(76.0, 62.0), egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.set_min_size(vec2(76.0, 62.0));
                ui.add_space(4.0);
                ui.label(egui::RichText::new(info.name).size(10.0).color(theme::TEXT_DIM));
                ui.add_space(4.0);
                let cur = v.round().max(0.0) as usize;
                egui::ComboBox::from_id_salt(("choice", salt.0, salt.1))
                    .width(68.0)
                    .selected_text(labels.get(cur).copied().unwrap_or(""))
                    .show_ui(ui, |ui| {
                        for (i, l) in labels.iter().enumerate() {
                            if ui.selectable_label(i == cur, *l).clicked() {
                                out = Some(i as f32);
                            }
                        }
                    });
            });
            out
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn device_panel(
    ui: &mut egui::Ui,
    d: &Device,
    index: usize,
    count: usize,
    meter: Option<f32>,
    sample_rate: f32,
    plugin: Option<&PluginView>,
    acts: &mut Vec<Act>,
) {
    let id = d.id;
    egui::Frame::new()
        .fill(if d.on { PANEL_BG } else { PANEL_OFF })
        .corner_radius(3.0)
        .inner_margin(egui::Margin::same(6))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                // Title bar: activator, name, move and delete.
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    let r = widgets::toggle(ui, d.on, "", theme::ACCENT, 14.0).on_hover_text("Device on/off");
                    if r.clicked() {
                        acts.push(Act::On(id, !d.on));
                    }
                    let name = match plugin {
                        Some(v) => v.name.clone(),
                        None => d.kind().name().to_string(),
                    };
                    let title = egui::RichText::new(name.to_uppercase()).small().color(if d.on {
                        theme::ACCENT
                    } else {
                        theme::TEXT_DIM
                    });
                    let t = ui.add(egui::Label::new(title).sense(Sense::click()));
                    t.context_menu(|ui| {
                        if ui.button("Reset to defaults").clicked() {
                            acts.push(Act::Reset(id, d.kind().default_params()));
                            ui.close();
                        }
                        if ui.add_enabled(index > 0, egui::Button::new("Move Left")).clicked() {
                            acts.push(Act::Move(id, -1));
                            ui.close();
                        }
                        if ui
                            .add_enabled(index + 1 < count, egui::Button::new("Move Right"))
                            .clicked()
                        {
                            acts.push(Act::Move(id, 1));
                            ui.close();
                        }
                        if ui.button("Delete").clicked() {
                            acts.push(Act::Remove(id));
                            ui.close();
                        }
                    });
                    ui.add_space(8.0);
                    let small = |ui: &mut egui::Ui, text: &str, enabled: bool, tip: &str| {
                        ui.add_enabled(enabled, egui::Button::new(egui::RichText::new(text).small()).small())
                            .on_hover_text(tip)
                            .clicked()
                    };
                    if small(ui, "<", index > 0, "Move left") {
                        acts.push(Act::Move(id, -1));
                    }
                    if small(ui, ">", index + 1 < count, "Move right") {
                        acts.push(Act::Move(id, 1));
                    }
                    if small(ui, "x", true, "Delete device") {
                        acts.push(Act::Remove(id));
                    }
                });
                ui.add_space(2.0);
                match &d.params {
                    DeviceParams::Eq(p) => eq_body(ui, id, p, sample_rate, acts),
                    DeviceParams::Plugin(_) => match plugin {
                        Some(v) => {
                            let mut pacts = Vec::new();
                            plugin_panel::body(ui, v, &mut pacts);
                            acts.extend(pacts.into_iter().map(Act::Plugin));
                        }
                        None => {
                            ui.label(egui::RichText::new("Plug-in missing").small().color(theme::METER_HIGH));
                        }
                    },
                    params => {
                        let ctx = super::fx_panels::PanelCtx { id, meter, sample_rate };
                        let mut changes = Vec::new();
                        if !super::fx_panels::body(ui, &ctx, params, &mut changes) {
                            generic_body(ui, id, params, meter, acts);
                        }
                        acts.extend(changes.into_iter().map(|(i, v)| Act::Param(id, i, v)));
                    }
                }
            });
        });
}

/// Knobs laid out in columns of two.
#[allow(dead_code)]
pub(super) fn generic_body_changes(
    ui: &mut egui::Ui,
    id: Id,
    params: &DeviceParams,
    meter: Option<f32>,
    out: &mut Vec<(usize, f32)>,
) {
    let mut acts = Vec::new();
    generic_body(ui, id, params, meter, &mut acts);
    for a in acts {
        if let Act::Param(_, i, v) = a {
            out.push((i, v));
        }
    }
}

fn generic_body(ui: &mut egui::Ui, id: Id, params: &DeviceParams, meter: Option<f32>, acts: &mut Vec<Act>) {
    let info = params.info();
    let visible: Vec<usize> = (0..info.len()).filter(|&i| params.is_relevant(i)).collect();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing = vec2(2.0, 2.0);
        for col in visible.chunks(2) {
            ui.vertical(|ui| {
                for &i in col {
                    if let Some(v) = param_control(ui, (id, i), &info[i], params.get(i)) {
                        acts.push(Act::Param(id, i, v));
                    }
                }
            });
        }
        if let Some(gr) = meter {
            gain_reduction_meter(ui, gr);
        }
    });
}

/// Vertical gain reduction meter, 0 to -24 dB from the top.
pub(super) fn gain_reduction_meter(ui: &mut egui::Ui, gr_db: f32) {
    let (rect, resp) = ui.allocate_exact_size(vec2(14.0, 124.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 1.0, Color32::from_rgb(0x1c, 0x1c, 0x1c));
    let frac = (-gr_db / 24.0).clamp(0.0, 1.0);
    if frac > 0.0 {
        let bar = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + rect.height() * frac));
        p.rect_filled(bar, 1.0, theme::ACCENT);
    }
    for db in [6.0, 12.0, 18.0] {
        let y = rect.top() + rect.height() * db / 24.0;
        p.hline(rect.x_range(), y, Stroke::new(1.0, Color32::from_rgb(0x44, 0x44, 0x44)));
    }
    resp.on_hover_text(format!("Gain reduction {gr_db:.1} dB"));
}

const BAND_COLORS: [Color32; eq::BANDS] = [
    Color32::from_rgb(0xff, 0x6b, 0x6b),
    Color32::from_rgb(0xff, 0xa9, 0x4d),
    Color32::from_rgb(0xff, 0xe0, 0x66),
    Color32::from_rgb(0x8c, 0xe9, 0x9a),
    Color32::from_rgb(0x63, 0xe6, 0xbe),
    Color32::from_rgb(0x74, 0xc0, 0xfc),
    Color32::from_rgb(0xb1, 0x97, 0xfc),
    Color32::from_rgb(0xf7, 0x83, 0xac),
];

const EQ_MIN_HZ: f32 = 20.0;
const EQ_MAX_HZ: f32 = 20000.0;
const EQ_RANGE_DB: f32 = 18.0;

#[allow(clippy::needless_range_loop)]
fn eq_body(ui: &mut egui::Ui, id: Id, p: &EqParams, sample_rate: f32, acts: &mut Vec<Act>) {
    let sel_id = ui.id().with(("eq_band", id));
    let mut sel: usize = ui.data(|d| d.get_temp(sel_id)).unwrap_or(3);
    ui.horizontal_top(|ui| {
        // Response curve with draggable band handles.
        let (rect, resp) = ui.allocate_exact_size(vec2(300.0, 128.0), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 2.0, Color32::from_rgb(0x1e, 0x1e, 0x1e));
        let x_of =
            |f: f32| rect.left() + rect.width() * ((f / EQ_MIN_HZ).ln() / (EQ_MAX_HZ / EQ_MIN_HZ).ln()).clamp(0.0, 1.0);
        let f_of = |x: f32| {
            let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            EQ_MIN_HZ * (EQ_MAX_HZ / EQ_MIN_HZ).powf(t)
        };
        let y_of = |db: f32| rect.center().y - db.clamp(-EQ_RANGE_DB, EQ_RANGE_DB) / EQ_RANGE_DB * rect.height() * 0.45;
        let db_of = |y: f32| (rect.center().y - y) / (rect.height() * 0.45) * EQ_RANGE_DB;
        let grid = Stroke::new(1.0, Color32::from_rgb(0x33, 0x33, 0x33));
        for f in [50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10000.0] {
            painter.vline(x_of(f), rect.y_range(), grid);
        }
        for db in [-12.0, -6.0, 6.0, 12.0] {
            painter.hline(rect.x_range(), y_of(db), grid);
        }
        painter.hline(
            rect.x_range(),
            y_of(0.0),
            Stroke::new(1.0, Color32::from_rgb(0x4a, 0x4a, 0x4a)),
        );
        for (f, label) in [(100.0, "100"), (1000.0, "1k"), (10000.0, "10k")] {
            painter.text(
                pos2(x_of(f) + 2.0, rect.bottom() - 2.0),
                egui::Align2::LEFT_BOTTOM,
                label,
                egui::FontId::proportional(9.0),
                theme::TEXT_DIM,
            );
        }
        let pts: Vec<_> = (0..=150)
            .map(|i| {
                let x = rect.left() + rect.width() * i as f32 / 150.0;
                pos2(x, y_of(p.response_db(f_of(x), sample_rate)))
            })
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.5, theme::ACCENT)));

        let handle_pos = |b: usize| {
            let band = &p.bands[b];
            let db = if band.uses_gain() { band.gain } else { 0.0 };
            pos2(x_of(band.freq), y_of(db))
        };
        // Pick the band under the pointer (or the one being dragged).
        let drag_id = ui.id().with(("eq_drag", id));
        let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
        if resp.drag_started() || resp.clicked() {
            if let Some(pp) = pointer {
                let near = (0..eq::BANDS)
                    .map(|b| (b, handle_pos(b).distance(pp)))
                    .filter(|(_, d)| *d < 12.0)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((b, _)) = near {
                    sel = b;
                    ui.data_mut(|d| d.insert_temp(drag_id, Some(b)));
                } else {
                    ui.data_mut(|d| d.insert_temp::<Option<usize>>(drag_id, None));
                }
            }
        }
        let dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id)).flatten();
        if resp.dragged() {
            if let (Some(b), Some(pp)) = (dragging, pointer) {
                let band = &p.bands[b];
                if !band.is_on() {
                    acts.push(Act::Param(id, EqParams::index(b, 0), 1.0));
                }
                acts.push(Act::Param(id, EqParams::index(b, 2), f_of(pp.x)));
                if band.uses_gain() {
                    acts.push(Act::Param(id, EqParams::index(b, 3), db_of(pp.y)));
                }
            }
        }
        if resp.drag_stopped() {
            ui.data_mut(|d| d.insert_temp::<Option<usize>>(drag_id, None));
        }
        if resp.double_clicked() {
            let b = sel;
            acts.push(Act::Param(
                id,
                EqParams::index(b, 0),
                if p.bands[b].is_on() { 0.0 } else { 1.0 },
            ));
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                let q = p.bands[sel].q * (1.0 + scroll * 0.004);
                acts.push(Act::Param(id, EqParams::index(sel, 4), q));
            }
        }
        for b in 0..eq::BANDS {
            let c = handle_pos(b);
            let on = p.bands[b].is_on();
            let col = if on {
                BAND_COLORS[b]
            } else {
                Color32::from_rgb(0x66, 0x66, 0x66)
            };
            if b == sel {
                painter.circle_stroke(c, 9.0, Stroke::new(1.5, theme::SELECT));
            }
            painter.circle_filled(c, 7.0, theme::dim(col, if on { 0.9 } else { 0.6 }));
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                format!("{}", b + 1),
                egui::FontId::proportional(9.0),
                Color32::BLACK,
            );
        }
        resp.on_hover_text(
            "Drag a numbered point to move a band, scroll to change its Q, double-click to switch it on or off",
        );

        // Band selector and the selected band's controls.
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for b in 0..eq::BANDS {
                    let on = p.bands[b].is_on();
                    let r = widgets::toggle(ui, on, &format!("{}", b + 1), BAND_COLORS[b], 20.0)
                        .on_hover_text("Click: select · Double-click: on/off");
                    if r.clicked() {
                        sel = b;
                    }
                    if r.double_clicked() {
                        acts.push(Act::Param(id, EqParams::index(b, 0), if on { 0.0 } else { 1.0 }));
                    }
                }
            });
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let info = EqParams::INFO;
                for field in [0, 1, 2, 3, 4] {
                    let i = EqParams::index(sel, field);
                    if field == 3 && !p.bands[sel].uses_gain() {
                        continue;
                    }
                    if let Some(v) = param_control(ui, (id, i), &info[i], p.get(i)) {
                        acts.push(Act::Param(id, i, v));
                    }
                }
            });
            ui.horizontal(|ui| {
                let i = EqParams::OUTPUT;
                if let Some(v) = param_control(ui, (id, i), &EqParams::INFO[i], p.get(i)) {
                    acts.push(Act::Param(id, i, v));
                }
            });
        });
    });
    ui.data_mut(|d| d.insert_temp(sel_id, sel));
}
