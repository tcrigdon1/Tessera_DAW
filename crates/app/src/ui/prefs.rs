//! The Preferences window (Ctrl/Cmd + ,), with tabs like Ableton's:
//! Look/Feel, Audio, Record/Warp/Launch, File/Folder, Plug-Ins and
//! Licenses/Maintenance.

use std::path::PathBuf;

use eframe::egui::{self, RichText};
use tessera_engine::time::Quantization;
use tessera_engine::warp::WarpMode;

use super::theme::{self, Theme};
use crate::app::TesseraApp;
use crate::audio::{self, DeviceList, BUFFER_SIZES};
use crate::prefs::{AudioConfig, ClipPalette};
use crate::project_files;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    LookFeel,
    Audio,
    Midi,
    RecordWarpLaunch,
    FileFolder,
    Plugins,
    Licenses,
}

impl Tab {
    const ALL: [Tab; 7] = [
        Tab::LookFeel,
        Tab::Audio,
        Tab::Midi,
        Tab::RecordWarpLaunch,
        Tab::FileFolder,
        Tab::Plugins,
        Tab::Licenses,
    ];

    fn label(self) -> &'static str {
        match self {
            Tab::LookFeel => "Look/Feel",
            Tab::Audio => "Audio",
            Tab::Midi => "Link/MIDI",
            Tab::RecordWarpLaunch => "Record/Warp/Launch",
            Tab::FileFolder => "File/Folder",
            Tab::Plugins => "Plug-Ins",
            Tab::Licenses => "Licenses/Maintenance",
        }
    }
}

/// Sample rates and buffer range the device offers, for the config they
/// were read for.
type Capabilities = (AudioConfig, Vec<u32>, Option<(u32, u32)>, audio::InputCapabilities);

#[derive(Default)]
pub struct PrefsUi {
    pub open: bool,
    pub tab: Tab,
    /// Audio settings being edited (applied with "Apply").
    audio_edit: Option<AudioConfig>,
    devices: Option<(Option<String>, DeviceList)>,
    capabilities: Option<Capabilities>,
    drivers: Vec<String>,
    /// Crash log shown in the Licenses/Maintenance tab.
    crash_log: Option<(PathBuf, String)>,
}

impl PrefsUi {
    pub fn show(&mut self, tab: Option<Tab>) {
        self.open = true;
        if let Some(t) = tab {
            self.tab = t;
        }
    }
}

pub fn draw(app: &mut TesseraApp, ctx: &egui::Context) {
    if !app.prefs_ui.open {
        return;
    }
    let mut open = true;
    let before = app.prefs.clone();
    egui::Window::new("Preferences")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([620.0, 440.0])
        .show(ctx, |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(150.0);
                    for t in Tab::ALL {
                        if ui.selectable_label(app.prefs_ui.tab == t, t.label()).clicked() {
                            app.prefs_ui.tab = t;
                        }
                    }
                });
                ui.separator();
                ui.vertical(|ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| match app.prefs_ui.tab {
                            Tab::LookFeel => look_feel(app, ui),
                            Tab::Audio => audio_tab(app, ui),
                            Tab::Midi => super::midi_prefs::body(app, ui),
                            Tab::RecordWarpLaunch => record_warp_launch(app, ui),
                            Tab::FileFolder => file_folder(app, ui),
                            Tab::Plugins => plugins_tab(app, ui),
                            Tab::Licenses => licenses(app, ui),
                        });
                });
            });
        });
    app.prefs_ui.open = open;
    if app.prefs != before {
        app.save_prefs();
    }
}

fn heading(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).strong().color(theme::text()));
    ui.add_space(2.0);
}

fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [170.0, 18.0],
            egui::Label::new(RichText::new(label).color(theme::text_dim())),
        );
        add(ui);
    });
}

fn look_feel(app: &mut TesseraApp, ui: &mut egui::Ui) {
    heading(ui, "Theme");
    let mut t = app.prefs.theme;
    row(ui, "Theme", |ui| {
        egui::ComboBox::from_id_salt("pref_theme")
            .selected_text(t.label())
            .show_ui(ui, |ui| {
                for opt in Theme::ALL {
                    ui.selectable_value(&mut t, opt, opt.label());
                }
            });
    });
    if t != app.prefs.theme {
        app.prefs.theme = t;
        theme::apply_theme(ui.ctx(), t);
    }
    let mut zoom = (app.prefs.zoom * 100.0).round();
    row(ui, "Zoom Display", |ui| {
        let r = ui.add(egui::Slider::new(&mut zoom, 50.0..=200.0).step_by(10.0).suffix(" %"));
        if r.drag_stopped() || (r.changed() && !r.dragged()) {
            app.prefs.zoom = zoom / 100.0;
            ui.ctx().set_zoom_factor(app.prefs.zoom);
        }
    });
    row(ui, "Track colours", |ui| {
        let mut p = app.prefs.clip_palette;
        egui::ComboBox::from_id_salt("pref_palette")
            .selected_text(p.label())
            .show_ui(ui, |ui| {
                for opt in ClipPalette::ALL {
                    ui.selectable_value(&mut p, opt, opt.label());
                }
            });
        app.prefs.clip_palette = p;
        for c in p.colors() {
            let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, theme::rgb(*c));
        }
    });
    if ui
        .button("Recolour all tracks with this palette")
        .on_hover_text("New tracks always use the palette; this also recolours the tracks of this set")
        .clicked()
    {
        let pal = app.prefs.clip_palette;
        for (i, t) in app.project.tracks.iter_mut().enumerate() {
            t.color = pal.color(i);
            for c in t.slots.iter_mut().flatten() {
                c.color = t.color;
            }
            for a in &mut t.arrangement {
                a.clip.color = t.color;
            }
        }
        app.changed();
    }
    heading(ui, "Help");
    row(ui, "Show tooltips", |ui| {
        ui.checkbox(&mut app.prefs.tooltips, "");
    });
    row(ui, "Show Info View", |ui| {
        ui.checkbox(&mut app.prefs.show_info_view, "");
    });
    row(ui, "Welcome tour", |ui| {
        if ui.button("Show the tour again").clicked() {
            app.prefs.tour_done = false;
        }
    });
}

fn audio_tab(app: &mut TesseraApp, ui: &mut egui::Ui) {
    let st = &mut app.prefs_ui;
    if st.drivers.is_empty() {
        st.drivers = audio::drivers();
    }
    let mut cfg = st.audio_edit.clone().unwrap_or_else(|| app.prefs.audio.clone());
    if st.devices.as_ref().map(|(d, _)| d != &cfg.driver).unwrap_or(true) {
        st.devices = Some((cfg.driver.clone(), audio::list_devices(cfg.driver.as_deref())));
    }
    let cap_key = AudioConfig {
        sample_rate: None,
        buffer_size: None,
        ..cfg.clone()
    };
    if st.capabilities.as_ref().map(|(c, ..)| c != &cap_key).unwrap_or(true) {
        let (rates, buffers) = audio::output_capabilities(&cap_key);
        let input = audio::input_capabilities(&cap_key);
        st.capabilities = Some((cap_key, rates, buffers, input));
    }
    let devices = st.devices.as_ref().map(|(_, d)| d.clone()).unwrap_or_default();
    let (rates, buffers, input_caps) = st
        .capabilities
        .as_ref()
        .map(|(_, r, b, i)| (r.clone(), *b, i.clone()))
        .unwrap_or_default();
    let drivers = st.drivers.clone();

    heading(ui, "Audio Device");
    let pick = |ui: &mut egui::Ui, id: &str, value: &mut Option<String>, options: &[String], default: &str| {
        let text = value.clone().unwrap_or_else(|| default.to_string());
        egui::ComboBox::from_id_salt(id)
            .selected_text(text)
            .width(260.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(value, None, default);
                for o in options {
                    ui.selectable_value(value, Some(o.clone()), o);
                }
            });
    };
    let driver_before = cfg.driver.clone();
    row(ui, "Driver Type", |ui| {
        pick(ui, "pref_driver", &mut cfg.driver, &drivers, "System default")
    });
    if cfg.driver != driver_before {
        // Device names belong to a driver: start from its defaults.
        cfg.output_device = None;
        cfg.input_device = None;
        cfg.output_offset = 0;
        cfg.input_offset = 0;
    }
    if audio::is_exclusive(cfg.driver.as_deref()) {
        row(ui, "", |ui| {
            ui.label(
                RichText::new(
                    "Exclusive mode: Tessera has the device to itself (other apps go quiet) at the buffer size below",
                )
                .color(theme::text_dim()),
            );
        });
    }
    if cfg!(windows) && !audio::ASIO_BUILT {
        row(ui, "", |ui| {
            ui.label(
                RichText::new("ASIO drivers appear here in a build with ASIO turned on (see the README)")
                    .color(theme::text_dim()),
            );
        });
    }
    row(ui, "Audio Output Device", |ui| {
        pick(
            ui,
            "pref_out",
            &mut cfg.output_device,
            &devices.outputs,
            "Default output",
        )
    });
    row(ui, "Audio Input Device", |ui| {
        ui.add_enabled_ui(!cfg.input_off, |ui| {
            pick(ui, "pref_in", &mut cfg.input_device, &devices.inputs, "Default input")
        });
        ui.checkbox(&mut cfg.input_off, "No input");
    });
    if !cfg.input_off {
        row(ui, "Input supports", |ui| {
            let text = match &input_caps.error {
                Some(e) => e.clone(),
                None => format!(
                    "{} Hz · up to {} channels · {}",
                    input_caps
                        .rates
                        .iter()
                        .map(|r| r.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    input_caps.max_channels,
                    input_caps.formats.join(", ")
                ),
            };
            ui.label(RichText::new(text).color(theme::text_dim()))
                .on_hover_text("Any of these works: Tessera converts the input to the sample rate it runs at");
        });
    }
    row(ui, "Output channels", |ui| {
        let n = app.audio.output_channels.max(2);
        let mut off = cfg.output_offset;
        egui::ComboBox::from_id_salt("pref_out_ch")
            .selected_text(format!("{}/{}", off + 1, off + 2))
            .show_ui(ui, |ui| {
                for o in (0..n.saturating_sub(1)).step_by(2) {
                    ui.selectable_value(&mut off, o, format!("{}/{}", o + 1, o + 2));
                }
            });
        cfg.output_offset = off;
        ui.label(RichText::new(format!("of {n}")).color(theme::text_dim()));
    });
    row(ui, "Input channels", |ui| {
        let n = app.audio.input_channels.max(2);
        let mut off = cfg.input_offset;
        egui::ComboBox::from_id_salt("pref_in_ch")
            .selected_text(format!("{}/{}", off + 1, off + 2))
            .show_ui(ui, |ui| {
                for o in (0..n.saturating_sub(1)).step_by(2) {
                    ui.selectable_value(&mut off, o, format!("{}/{}", o + 1, o + 2));
                }
            });
        cfg.input_offset = off;
        ui.label(RichText::new(format!("of {}", app.audio.input_channels)).color(theme::text_dim()));
    });

    heading(ui, "Sample Rate and Latency");
    row(ui, "Sample Rate", |ui| {
        let text = cfg
            .sample_rate
            .map(|r| format!("{r} Hz"))
            .unwrap_or_else(|| "Device default".into());
        egui::ComboBox::from_id_salt("pref_sr")
            .selected_text(text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.sample_rate, None, "Device default");
                for r in &rates {
                    ui.selectable_value(&mut cfg.sample_rate, Some(*r), format!("{r} Hz"));
                }
            });
    });
    row(ui, "Buffer Size", |ui| {
        let text = cfg
            .buffer_size
            .map(|b| format!("{b} samples"))
            .unwrap_or_else(|| "Device default".into());
        egui::ComboBox::from_id_salt("pref_buf")
            .selected_text(text)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut cfg.buffer_size, None, "Device default");
                for b in BUFFER_SIZES {
                    if buffers.is_none_or(|(lo, hi)| (lo..=hi).contains(&b)) {
                        ui.selectable_value(&mut cfg.buffer_size, Some(b), format!("{b} samples"));
                    }
                }
            });
    });
    let sr = app.audio.sample_rate as f64;
    let input_lat = app
        .engine
        .shared
        .record
        .io_latency
        .load(std::sync::atomic::Ordering::Relaxed) as f64;
    row(ui, "Output Latency", |ui| {
        ui.label(format!("{:.1} ms", app.audio.output_latency_ms()));
    });
    row(ui, "Round-trip Latency", |ui| {
        ui.label(if input_lat > 0.0 {
            format!("{:.1} ms (input + output, estimated)", input_lat * 1000.0 / sr)
        } else {
            "— (no input)".into()
        })
        .on_hover_text("How long a sound takes from the input jack, through Tessera, to the output. Recordings are shifted back by this much.");
    });

    heading(ui, "Running Now");
    let a = &app.audio;
    row(ui, "Driver", |ui| {
        ui.label(if a.driver.is_empty() { "—" } else { &a.driver });
    });
    row(ui, "Output", |ui| {
        ui.label(format!("{} · {} ch", a.device_name, a.output_channels));
    });
    row(ui, "Sample Rate", |ui| {
        ui.label(format!("{} Hz", a.sample_rate));
    });
    row(ui, "Buffer", |ui| {
        let text = match a.running_buffer() {
            Some(b) => format!("{b} samples ({:.1} ms)", b as f64 * 1000.0 / sr),
            None => "device default".into(),
        };
        let input = a.input_frames.load(std::sync::atomic::Ordering::Relaxed);
        if input > 0 && a.input_rate > 0 && Some(input) != a.running_buffer() {
            ui.label(format!(
                "{text} · input {input} samples ({:.1} ms)",
                input as f64 * 1000.0 / a.input_rate as f64
            ));
        } else {
            ui.label(text);
        }
    });
    row(ui, "Input", |ui| {
        let a = &app.audio;
        match (&a.input_name, &a.input_error) {
            (Some(name), _) => {
                let mut text = format!(
                    "{name} · {} Hz · {} ch · {}",
                    a.input_rate, a.input_channels, a.input_format
                );
                if a.input_rate != a.sample_rate {
                    text += &format!(" · converted to {} Hz", a.sample_rate);
                }
                ui.label(text);
            }
            (None, Some(e)) => {
                ui.colored_label(theme::ARM, format!("Could not be opened: {e}"));
            }
            (None, None) => {
                ui.label(RichText::new("Off").color(theme::text_dim()));
            }
        }
    });
    if let Some(r) = &app.audio.fallback_reason {
        ui.colored_label(theme::ARM, format!("Your settings could not be used: {r}"));
    }

    heading(ui, "Test");
    row(ui, "Test Tone", |ui| {
        if ui
            .button("Play 440 Hz")
            .on_hover_text("Two seconds at -18 dBFS")
            .clicked()
        {
            app.test_tone();
        }
        if ui.button("Stop").clicked() {
            app.send(tessera_engine::Command::StopPreview);
        }
    });
    row(ui, "CPU Usage", |ui| {
        ui.label(format!("{:.0} %", app.engine.shared.cpu_load.load() * 100.0));
    });

    ui.add_space(8.0);
    let changed = cfg != app.prefs.audio;
    ui.horizontal(|ui| {
        if ui.add_enabled(changed, egui::Button::new("Apply")).clicked() {
            app.prefs.audio = cfg.clone();
            app.prefs_ui.audio_edit = None;
            app.save_prefs();
            app.restart_audio();
        } else if changed {
            app.prefs_ui.audio_edit = Some(cfg.clone());
        }
        if ui.add_enabled(changed, egui::Button::new("Revert")).clicked() {
            app.prefs_ui.audio_edit = None;
        }
        if ui.button("Refresh devices").clicked() {
            app.prefs_ui.devices = None;
            app.prefs_ui.capabilities = None;
            app.prefs_ui.drivers.clear();
        }
    });
}

fn record_warp_launch(app: &mut TesseraApp, ui: &mut egui::Ui) {
    heading(ui, "Record");
    row(ui, "Count-in (new sets)", |ui| {
        let mut c = app.prefs.count_in_bars;
        egui::ComboBox::from_id_salt("pref_count")
            .selected_text(if c == 0 { "None".into() } else { format!("{c} Bar") })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut c, 0, "None");
                for n in [1, 2, 4] {
                    ui.selectable_value(&mut c, n, format!("{n} Bar"));
                }
            });
        app.prefs.count_in_bars = c;
    });
    heading(ui, "Warp");
    row(ui, "Auto-Warp Long Samples", |ui| {
        ui.checkbox(&mut app.prefs.auto_warp_long_samples, "")
            .on_hover_text("Warp audio files of 2 seconds or more to the set's tempo when they are added");
    });
    row(ui, "Default Warp Mode", |ui| {
        let mut m = app.prefs.default_warp_mode;
        egui::ComboBox::from_id_salt("pref_warp")
            .selected_text(m.map(|m| m.label()).unwrap_or("Automatic"))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut m, None, "Automatic");
                for opt in WarpMode::ALL {
                    ui.selectable_value(&mut m, Some(opt), opt.label());
                }
            });
        app.prefs.default_warp_mode = m;
    });
    heading(ui, "Launch");
    row(ui, "Default Launch Quantization", |ui| {
        let mut q = app.prefs.launch_quantization;
        egui::ComboBox::from_id_salt("pref_quant")
            .selected_text(q.label())
            .show_ui(ui, |ui| {
                for opt in Quantization::ALL {
                    ui.selectable_value(&mut q, opt, opt.label());
                }
            });
        app.prefs.launch_quantization = q;
    });
    row(ui, "New MIDI clip length", |ui| {
        ui.add(
            egui::DragValue::new(&mut app.prefs.new_clip_bars)
                .range(1..=64)
                .suffix(" bar(s)"),
        );
    });
    ui.label(
        RichText::new(
            "Count-in and launch quantization apply to new sets; change the current set in the transport bar.",
        )
        .small()
        .color(theme::text_dim()),
    );
}

fn folder_row(ui: &mut egui::Ui, label: &str, value: &mut Option<PathBuf>, default: Option<PathBuf>) {
    row(ui, label, |ui| {
        let shown = value
            .clone()
            .or(default)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "—".into());
        ui.label(RichText::new(shown).monospace().small());
        if ui.small_button("Browse…").clicked() {
            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                *value = Some(d);
            }
        }
        if value.is_some() && ui.small_button("Default").clicked() {
            *value = None;
        }
    });
}

fn file_folder(app: &mut TesseraApp, ui: &mut egui::Ui) {
    heading(ui, "Saving");
    row(ui, "Autosave every", |ui| {
        let mut m = app.prefs.autosave_minutes;
        egui::ComboBox::from_id_salt("pref_autosave")
            .selected_text(if m == 0 { "Off".into() } else { format!("{m} min") })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut m, 0, "Off");
                for n in [1, 2, 5, 10, 15, 30] {
                    ui.selectable_value(&mut m, n, format!("{n} min"));
                }
            });
        app.prefs.autosave_minutes = m;
    });
    ui.label(
        RichText::new(
            "Autosaves let Tessera offer to recover your set if it quits unexpectedly. \
             They never replace your own project file.",
        )
        .small()
        .color(theme::text_dim()),
    );
    folder_row(ui, "Default project folder", &mut app.prefs.project_folder, None);
    heading(ui, "Library");
    let default_lib = {
        let mut p = app.prefs.clone();
        p.library_path = None;
        p.library()
    };
    folder_row(ui, "Library location", &mut app.prefs.library_path, default_lib);
    if let Some(t) = app.prefs.templates_dir() {
        row(ui, "Templates", |ui| {
            ui.label(RichText::new(t.display().to_string()).monospace().small());
        });
    }
    row(ui, "Default Set", |ui| {
        let exists = app.prefs.default_template().is_some_and(|p| p.is_file());
        if ui.button("Save Current Set as Default").clicked() {
            app.save_as_default_set();
        }
        if exists && ui.button("Remove").clicked() {
            if let Some(p) = app.prefs.default_template() {
                let _ = std::fs::remove_file(p);
            }
        }
    });
    heading(ui, "Recent");
    row(ui, "Recent sets", |ui| {
        ui.label(format!("{}", app.prefs.recent.len()));
        if ui.small_button("Clear").clicked() {
            app.prefs.recent.clear();
        }
    });
}

fn plugins_tab(app: &mut TesseraApp, ui: &mut egui::Ui) {
    heading(ui, "Plug-In Windows");
    row(ui, "Auto-Open Plug-In Windows", |ui| {
        ui.checkbox(&mut app.prefs.auto_open_plugin_windows, "")
            .on_hover_text("Open a plug-in's own window as soon as you add it to a track");
    });
    heading(ui, "Plug-In Sources");
    ui.label(
        RichText::new("VST3 and CLAP plug-ins in these folders are scanned on start.")
            .small()
            .color(theme::text_dim()),
    );
    let user: Vec<PathBuf> = app.plugins.folders.clone();
    for f in app.plugins.all_folders() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(f.display().to_string()).monospace().small());
            if user.contains(&f) {
                if ui.small_button("Remove").clicked() {
                    app.plugins.remove_folder(&f);
                }
            } else {
                ui.label(RichText::new("(standard)").small().color(theme::text_dim()));
            }
        });
    }
    ui.horizontal(|ui| {
        if ui.button("Add Folder…").clicked() {
            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                app.plugins.add_folder(d);
            }
        }
        if ui.button("Rescan").clicked() {
            app.plugins.start_scan();
        }
    });
    match app.plugins.scanning() {
        Some((done, total, current)) => {
            ui.label(format!("Scanning {done}/{total}: {current}"));
        }
        None => {
            ui.label(format!("{} plug-ins installed", app.plugins.plugins().len()));
        }
    }
}

fn licenses(app: &mut TesseraApp, ui: &mut egui::Ui) {
    heading(ui, "About");
    ui.label(format!("Tessera {}", env!("CARGO_PKG_VERSION")));
    ui.label(
        RichText::new(
            "An original digital audio workstation. MIT License.\n\
             Built with egui/eframe, cpal, hound, rtrb, serde, rfd, flacenc and midly; \
             plug-in hosting uses the vst3 and clap-sys bindings. MP3 export uses Tessera's own encoder.",
        )
        .small(),
    );
    if let Some(dir) = crate::browser::app_dir() {
        row(ui, "Settings folder", |ui| {
            ui.label(RichText::new(dir.display().to_string()).monospace().small());
        });
    }
    heading(ui, "Crash Logs");
    let logs = project_files::crash_logs();
    if logs.is_empty() {
        ui.label(RichText::new("No crashes recorded.").color(theme::text_dim()));
    } else {
        for l in logs.iter().take(10) {
            ui.horizontal(|ui| {
                let name = l
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if ui.small_button("View").clicked() {
                    let text = std::fs::read_to_string(l).unwrap_or_default();
                    app.prefs_ui.crash_log = Some((l.clone(), text));
                }
                ui.label(RichText::new(name).monospace().small());
            });
        }
        if ui.button("Delete All Crash Logs").clicked() {
            for l in &logs {
                let _ = std::fs::remove_file(l);
            }
            app.prefs_ui.crash_log = None;
        }
    }
    if let Some((path, text)) = &app.prefs_ui.crash_log {
        ui.separator();
        ui.label(RichText::new(path.display().to_string()).small());
        let mut t = text.as_str();
        egui::ScrollArea::vertical()
            .max_height(180.0)
            .id_salt("crash_text")
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut t)
                        .code_editor()
                        .desired_width(f32::INFINITY),
                );
            });
    }
    heading(ui, "Maintenance");
    ui.horizontal(|ui| {
        if ui.button("Reset All Preferences").clicked() {
            let recent = std::mem::take(&mut app.prefs.recent);
            app.prefs = crate::prefs::Preferences {
                recent,
                tour_done: true,
                ..Default::default()
            };
            theme::apply_theme(ui.ctx(), app.prefs.theme);
            ui.ctx().set_zoom_factor(app.prefs.zoom);
        }
        if ui.button("Clear Waveform Cache").clicked() {
            app.peak_cache.clear();
        }
    });
}
