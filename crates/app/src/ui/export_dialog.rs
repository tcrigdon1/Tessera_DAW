//! File → Export Audio/Video (Ctrl/Cmd + Shift + R): what to render (the
//! master, every track and return as stems, or one track), which part of the
//! Arrangement, and the file format. Rendering runs in the background with a
//! progress bar.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use eframe::egui::{self, RichText};
use tessera_engine::export::{self, AudioFormat, BitDepth, Dither, EncodeSettings};
use tessera_engine::model::{Id, Project};
use tessera_engine::render::{self, RenderOptions, RenderSource};
use tessera_plugin_host::PluginInstance;

use super::theme;
use crate::app::TesseraApp;
use crate::prefs::ExportPrefs;

/// Sample rates offered for export.
pub const EXPORT_RATES: [u32; 6] = [22050, 32000, 44100, 48000, 88200, 96000];
const MP3_RATES: [u32; 3] = [32000, 44100, 48000];
const MP3_BITRATES: [u32; 5] = [128, 160, 192, 256, 320];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendered {
    Master,
    AllTracks,
    Track(Id),
}

/// A render running in the background.
struct Job {
    done: Arc<AtomicUsize>,
    total: usize,
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<Result<Vec<PathBuf>, String>>,
}

pub struct ExportUi {
    pub open: bool,
    pub settings: ExportPrefs,
    pub rendered: Rendered,
    /// Range in bars (start is 1-based like the ruler).
    pub start_bar: f64,
    pub length_bars: f64,
    job: Option<Job>,
    pub last_error: Option<String>,
}

impl Default for ExportUi {
    fn default() -> Self {
        ExportUi {
            open: false,
            settings: ExportPrefs::default(),
            rendered: Rendered::Master,
            start_bar: 1.0,
            length_bars: 8.0,
            job: None,
            last_error: None,
        }
    }
}

/// The default range: the loop when it's on, otherwise the whole song.
pub fn default_range(p: &Project) -> (f64, f64) {
    let bpb = p.beats_per_bar.max(1) as f64;
    if p.loop_enabled {
        (p.loop_start / bpb + 1.0, (p.loop_length / bpb).max(1.0))
    } else {
        let end = p.arrangement_length().max(bpb);
        (1.0, (end / bpb).ceil())
    }
}

impl ExportUi {
    pub fn open(&mut self, project: &Project, prefs: &ExportPrefs) {
        if self.job.is_some() {
            self.open = true;
            return;
        }
        self.open = true;
        self.settings = prefs.clone();
        let (s, l) = default_range(project);
        self.start_bar = s;
        self.length_bars = l;
        self.last_error = None;
        if let Rendered::Track(id) = self.rendered {
            if project.track(id).is_none() && !project.returns.iter().any(|r| r.id == id) {
                self.rendered = Rendered::Master;
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn encode_settings(&self) -> EncodeSettings {
        let s = &self.settings;
        EncodeSettings {
            format: s.format,
            sample_rate: s.sample_rate,
            bit_depth: s.bit_depth,
            dither: s.dither,
            normalize: s.normalize,
            mp3_bitrate_kbps: s.mp3_bitrate_kbps,
        }
    }
}

/// File name for a stem: "<song> <stem name>.<ext>", without characters
/// file systems refuse.
pub fn stem_file_name(song: &str, stem: &str, ext: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if "/\\:*?\"<>|".contains(c) || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect::<String>()
            .trim()
            .to_string()
    };
    format!("{} {}.{ext}", clean(song), clean(stem))
}

pub fn draw(app: &mut TesseraApp, ctx: &egui::Context) {
    poll_job(app);
    if !app.export_ui.open {
        return;
    }
    let mut open = true;
    egui::Window::new("Export Audio")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .show(ctx, |ui| body(app, ui));
    if !open && !app.export_ui.busy() {
        app.export_ui.open = false;
    }
}

fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [150.0, 18.0],
            egui::Label::new(RichText::new(label).color(theme::text_dim())),
        );
        add(ui);
    });
}

fn body(app: &mut TesseraApp, ui: &mut egui::Ui) {
    if let Some(job) = &app.export_ui.job {
        let done = job.done.load(Ordering::Relaxed);
        ui.label(format!("Rendering {} of {}…", (done + 1).min(job.total), job.total));
        ui.add(egui::ProgressBar::new(done as f32 / job.total.max(1) as f32).show_percentage());
        if ui.button("Cancel").clicked() {
            job.cancel.store(true, Ordering::Relaxed);
        }
        return;
    }
    let p = &app.project;
    let bpb = p.beats_per_bar.max(1) as f64;
    let ex = &mut app.export_ui;

    ui.label(RichText::new("Selection").strong());
    row(ui, "Rendered Track", |ui| {
        let name = |r: Rendered| match r {
            Rendered::Master => "Master".to_string(),
            Rendered::AllTracks => "All Individual Tracks".to_string(),
            Rendered::Track(id) => p
                .track(id)
                .map(|t| t.name.clone())
                .or_else(|| p.returns.iter().find(|r| r.id == id).map(|r| r.name.clone()))
                .unwrap_or_default(),
        };
        egui::ComboBox::from_id_salt("exp_track")
            .selected_text(name(ex.rendered))
            .width(200.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut ex.rendered, Rendered::Master, "Master");
                ui.selectable_value(&mut ex.rendered, Rendered::AllTracks, "All Individual Tracks");
                ui.separator();
                for t in &p.tracks {
                    ui.selectable_value(&mut ex.rendered, Rendered::Track(t.id), &t.name);
                }
                for r in &p.returns {
                    ui.selectable_value(&mut ex.rendered, Rendered::Track(r.id), &r.name);
                }
            });
    });
    row(ui, "Render Start", |ui| {
        ui.add(
            egui::DragValue::new(&mut ex.start_bar)
                .range(1.0..=9999.0)
                .speed(0.05)
                .fixed_decimals(0)
                .prefix("bar "),
        );
        if ui.small_button("Loop").on_hover_text("Use the loop brace").clicked() {
            ex.start_bar = p.loop_start / bpb + 1.0;
            ex.length_bars = (p.loop_length / bpb).max(1.0);
        }
        if ui
            .small_button("Song")
            .on_hover_text("From the start to the end of the last clip")
            .clicked()
        {
            ex.start_bar = 1.0;
            ex.length_bars = (p.arrangement_length().max(bpb) / bpb).ceil();
        }
    });
    row(ui, "Render Length", |ui| {
        ui.add(
            egui::DragValue::new(&mut ex.length_bars)
                .range(1.0..=9999.0)
                .speed(0.05)
                .fixed_decimals(0)
                .suffix(" bars"),
        );
        let secs = ex.length_bars * bpb * 60.0 / p.tempo;
        ui.label(RichText::new(format!("{}:{:04.1}", (secs / 60.0) as u32, secs % 60.0)).color(theme::text_dim()));
    });

    ui.separator();
    ui.label(RichText::new("Rendering Options").strong());
    let s = &mut ex.settings;
    row(ui, "Include Return and Master Effects", |ui| {
        ui.checkbox(&mut s.include_return_and_master_fx, "").on_hover_text(
            "For individual tracks: render each one as if soloed, through its returns and the master chain",
        );
    });
    row(ui, "Render as Loop", |ui| {
        ui.checkbox(&mut s.render_as_loop, "")
            .on_hover_text("Adds the tail (reverb, delay) at the end back onto the start so the file loops seamlessly");
    });
    row(ui, "Tail", |ui| {
        ui.add(
            egui::DragValue::new(&mut s.tail_secs)
                .range(0.0..=30.0)
                .speed(0.05)
                .fixed_decimals(1)
                .suffix(" s"),
        )
        .on_hover_text("Extra time rendered after the range for reverb and release tails");
    });
    row(ui, "Normalize", |ui| {
        ui.checkbox(&mut s.normalize, "")
            .on_hover_text("Raise (or lower) the level so the loudest peak hits 0 dB");
    });

    ui.separator();
    ui.label(RichText::new("Encoding").strong());
    row(ui, "File Type", |ui| {
        egui::ComboBox::from_id_salt("exp_fmt")
            .selected_text(s.format.label())
            .show_ui(ui, |ui| {
                for f in AudioFormat::ALL {
                    ui.selectable_value(&mut s.format, f, f.label());
                }
            });
    });
    let mp3 = s.format == AudioFormat::Mp3;
    if mp3 && !MP3_RATES.contains(&s.sample_rate) {
        s.sample_rate = 44100;
    }
    row(ui, "Sample Rate", |ui| {
        egui::ComboBox::from_id_salt("exp_sr")
            .selected_text(format!("{} Hz", s.sample_rate))
            .show_ui(ui, |ui| {
                let rates: &[u32] = if mp3 { &MP3_RATES } else { &EXPORT_RATES };
                for r in rates {
                    ui.selectable_value(&mut s.sample_rate, *r, format!("{r} Hz"));
                }
            });
    });
    if mp3 {
        row(ui, "Bit Rate", |ui| {
            egui::ComboBox::from_id_salt("exp_br")
                .selected_text(format!("{} kbps", s.mp3_bitrate_kbps))
                .show_ui(ui, |ui| {
                    for b in MP3_BITRATES {
                        ui.selectable_value(&mut s.mp3_bitrate_kbps, b, format!("{b} kbps"));
                    }
                });
        });
    } else {
        let flac = s.format == AudioFormat::Flac;
        if flac && s.bit_depth == BitDepth::Float32 {
            s.bit_depth = BitDepth::Int24;
        }
        row(ui, "Bit Depth", |ui| {
            egui::ComboBox::from_id_salt("exp_bd")
                .selected_text(s.bit_depth.label())
                .show_ui(ui, |ui| {
                    for b in BitDepth::ALL {
                        if !(flac && b == BitDepth::Float32) {
                            ui.selectable_value(&mut s.bit_depth, b, b.label());
                        }
                    }
                });
        });
        row(ui, "Dither Options", |ui| {
            ui.add_enabled_ui(s.bit_depth != BitDepth::Float32, |ui| {
                egui::ComboBox::from_id_salt("exp_dither")
                    .selected_text(s.dither.label())
                    .show_ui(ui, |ui| {
                        for d in Dither::ALL {
                            ui.selectable_value(&mut s.dither, d, d.label());
                        }
                    });
            });
        });
    }
    if let Some(e) = &ex.last_error {
        ui.colored_label(theme::ARM, e);
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.button("Export…").clicked() {
            start_export(app);
        }
        if ui.button("Cancel").clicked() {
            app.export_ui.open = false;
        }
    });
}

/// What to render and where, decided on the UI thread.
fn plan(app: &TesseraApp, target: &Path) -> Vec<(RenderOptions, PathBuf)> {
    let ex = &app.export_ui;
    let p = &app.project;
    let bpb = p.beats_per_bar.max(1) as f64;
    let base = RenderOptions {
        sample_rate: ex.settings.sample_rate,
        start: (ex.start_bar.round().max(1.0) - 1.0) * bpb,
        length: ex.length_bars.round().max(1.0) * bpb,
        source: RenderSource::Master,
        include_return_and_master_fx: ex.settings.include_return_and_master_fx,
        render_as_loop: ex.settings.render_as_loop,
        tail_secs: ex.settings.tail_secs,
    };
    let ext = ex.settings.format.extension();
    match ex.rendered {
        Rendered::Master => vec![(base, target.to_path_buf())],
        Rendered::Track(id) => {
            let source = if p.track(id).is_some() {
                RenderSource::Track(id)
            } else {
                RenderSource::Return(id)
            };
            vec![(RenderOptions { source, ..base }, target.to_path_buf())]
        }
        Rendered::AllTracks => {
            let mut v: Vec<(RenderOptions, PathBuf)> = render::stem_sources(p)
                .into_iter()
                .map(|(source, name)| {
                    let file = target.join(stem_file_name(&p.name, &name, ext));
                    (RenderOptions { source, ..base.clone() }, file)
                })
                .collect();
            // And the full mix alongside, like Ableton.
            v.push((base, target.join(stem_file_name(&p.name, "Master", ext))));
            v
        }
    }
}

fn start_export(app: &mut TesseraApp) {
    let ext = app.export_ui.settings.format.extension();
    let stems = app.export_ui.rendered == Rendered::AllTracks;
    let mut dialog = rfd::FileDialog::new();
    if let Some(dir) = app
        .export_ui
        .settings
        .folder
        .clone()
        .or_else(|| app.path.as_ref().and_then(|p| p.parent().map(Path::to_path_buf)))
    {
        dialog = dialog.set_directory(dir);
    }
    let target = if stems {
        dialog.set_title("Choose a folder for the stems").pick_folder()
    } else {
        let stem = match app.export_ui.rendered {
            Rendered::Track(id) => app
                .project
                .track(id)
                .map(|t| t.name.clone())
                .or_else(|| app.project.returns.iter().find(|r| r.id == id).map(|r| r.name.clone())),
            _ => None,
        };
        let name = match stem {
            Some(s) => stem_file_name(&app.project.name, &s, ext),
            None => format!("{}.{ext}", app.project.name),
        };
        dialog
            .add_filter(app.export_ui.settings.format.label(), &[ext])
            .set_file_name(name)
            .save_file()
            .map(|mut p| {
                if p.extension().is_none() {
                    p.set_extension(ext);
                }
                p
            })
    };
    let Some(target) = target else { return };
    let folder = if stems {
        target.clone()
    } else {
        target.parent().map(Path::to_path_buf).unwrap_or_default()
    };
    app.export_ui.settings.folder = Some(folder);
    app.prefs.export = app.export_ui.settings.clone();
    app.save_prefs();

    let jobs = plan(app, &target);
    let enc = app.export_ui.encode_settings();
    // Plug-ins get fresh offline instances at the export sample rate.
    app.plugins.store_states(&mut app.project);
    let instances = app.plugins.export_instances(&app.project, enc.sample_rate as f32);
    let project = app.project.clone();
    let done = Arc::new(AtomicUsize::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let total = jobs.len();
    let (d, c) = (done.clone(), cancel.clone());
    let thread = std::thread::Builder::new()
        .name("tessera-export".into())
        .spawn(move || run(project, instances, jobs, enc, d, c))
        .expect("spawn export thread");
    app.export_ui.job = Some(Job {
        done,
        total,
        cancel,
        thread,
    });
    app.status = "Exporting…".into();
}

/// Renders and encodes every job (export thread).
fn run(
    project: Project,
    instances: Vec<(Id, PluginInstance)>,
    jobs: Vec<(RenderOptions, PathBuf)>,
    enc: EncodeSettings,
    done: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<PathBuf>, String> {
    let map = instances
        .iter()
        .map(|(id, i)| (*id, i.processor() as Arc<dyn tessera_engine::plugin::PluginProcessor>))
        .collect();
    let result = tessera_engine::plugin::with_processors(map, || {
        let mut written = Vec::new();
        for (opts, file) in jobs {
            if cancel.load(Ordering::Relaxed) {
                return Err("Export cancelled".to_string());
            }
            let samples = render::render(&project, &opts);
            export::encode_file(&file, &samples, &enc).map_err(|e| format!("{}: {e}", file.display()))?;
            written.push(file);
            done.fetch_add(1, Ordering::Relaxed);
        }
        Ok(written)
    });
    drop(instances);
    result
}

fn poll_job(app: &mut TesseraApp) {
    let finished = app.export_ui.job.as_ref().is_some_and(|j| j.thread.is_finished());
    if !finished {
        return;
    }
    let Some(job) = app.export_ui.job.take() else { return };
    match job.thread.join() {
        Ok(Ok(files)) => {
            app.status = match files.as_slice() {
                [one] => format!("Exported {}", one.display()),
                many => format!(
                    "Exported {} files to {}",
                    many.len(),
                    many.first()
                        .and_then(|f| f.parent())
                        .map(|d| d.display().to_string())
                        .unwrap_or_default()
                ),
            };
            app.export_ui.open = false;
        }
        Ok(Err(e)) => {
            app.status = format!("Export failed: {e}");
            app.export_ui.last_error = Some(e);
        }
        Err(_) => {
            app.status = "Export failed: the render crashed".into();
            app.export_ui.last_error = Some("The render crashed (see Preferences → Licenses/Maintenance)".into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stem_names_are_file_safe() {
        assert_eq!(stem_file_name("My Song", "1 Drums", "wav"), "My Song 1 Drums.wav");
        assert_eq!(stem_file_name("a/b", "c:d?", "flac"), "a_b c_d_.flac");
    }

    #[test]
    fn range_defaults_to_loop_or_song() {
        let mut p = tessera_engine::demo::demo_project();
        p.loop_enabled = true;
        p.loop_start = 8.0;
        p.loop_length = 16.0;
        assert_eq!(default_range(&p), (3.0, 4.0));
        p.loop_enabled = false;
        let (s, l) = default_range(&p);
        assert_eq!(s, 1.0);
        assert!(l * 4.0 >= p.arrangement_length());
    }
}
