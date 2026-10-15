//! User preferences, saved between runs in `preferences.json` in the app
//! folder (see [`crate::browser::app_dir`]).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tessera_engine::export::{AudioFormat, BitDepth, Dither};
use tessera_engine::midi::edit::{Quantize, RecordQuantize};
use tessera_engine::time::Quantization;
use tessera_engine::warp::WarpMode;

use crate::browser::app_dir;
use crate::ui::theme::Theme;
use crate::ui::view_menu::ViewPanels;

/// How many projects File → Open Recent remembers.
pub const MAX_RECENT: usize = 10;

/// Audio device settings. `None` means the system default.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Audio driver type (cpal host), e.g. "ALSA", "JACK", "WASAPI", "ASIO".
    pub driver: Option<String>,
    pub output_device: Option<String>,
    pub input_device: Option<String>,
    pub sample_rate: Option<u32>,
    /// Frames per buffer.
    pub buffer_size: Option<u32>,
    /// First output channel of the stereo master pair (0 = outputs 1/2).
    pub output_offset: u16,
    /// First input channel of the stereo input pair (0 = inputs 1/2).
    pub input_offset: u16,
    /// No audio input at all.
    pub input_off: bool,
}

/// Colour set for new tracks and clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ClipPalette {
    #[default]
    Classic,
    Pastel,
    Vivid,
    Earth,
}

impl ClipPalette {
    pub const ALL: [ClipPalette; 4] = [
        ClipPalette::Classic,
        ClipPalette::Pastel,
        ClipPalette::Vivid,
        ClipPalette::Earth,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ClipPalette::Classic => "Classic",
            ClipPalette::Pastel => "Pastel",
            ClipPalette::Vivid => "Vivid",
            ClipPalette::Earth => "Earth",
        }
    }

    pub fn colors(self) -> &'static [[u8; 3]] {
        match self {
            ClipPalette::Classic => &tessera_engine::model::TRACK_COLORS,
            ClipPalette::Pastel => &[
                [255, 199, 206],
                [255, 214, 165],
                [253, 255, 182],
                [202, 255, 191],
                [155, 246, 255],
                [160, 196, 255],
                [189, 178, 255],
                [255, 198, 255],
            ],
            ClipPalette::Vivid => &[
                [255, 59, 48],
                [255, 149, 0],
                [255, 214, 10],
                [52, 199, 89],
                [0, 199, 190],
                [10, 132, 255],
                [94, 92, 230],
                [255, 55, 95],
            ],
            ClipPalette::Earth => &[
                [190, 120, 80],
                [204, 170, 100],
                [150, 160, 90],
                [100, 140, 110],
                [110, 150, 160],
                [140, 120, 160],
                [170, 110, 120],
                [160, 140, 120],
            ],
        }
    }

    /// Colour for the `n`th track.
    pub fn color(self, n: usize) -> [u8; 3] {
        let c = self.colors();
        c[n % c.len()]
    }
}

/// The last Export Audio settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportPrefs {
    pub format: AudioFormat,
    pub sample_rate: u32,
    pub bit_depth: BitDepth,
    pub dither: Dither,
    pub normalize: bool,
    pub mp3_bitrate_kbps: u32,
    pub include_return_and_master_fx: bool,
    pub render_as_loop: bool,
    /// Seconds of tail (reverb, release) rendered after the range.
    pub tail_secs: f64,
    pub folder: Option<PathBuf>,
}

impl Default for ExportPrefs {
    fn default() -> Self {
        ExportPrefs {
            format: AudioFormat::Wav,
            sample_rate: 44100,
            bit_depth: BitDepth::Int24,
            dither: Dither::Triangular,
            normalize: false,
            mp3_bitrate_kbps: 320,
            include_return_and_master_fx: true,
            render_as_loop: false,
            tail_secs: 0.0,
            folder: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    // Look/Feel
    pub theme: Theme,
    /// UI zoom, 0.5 ..= 2.0.
    pub zoom: f32,
    pub clip_palette: ClipPalette,
    pub tooltips: bool,
    pub show_info_view: bool,

    // Audio
    pub audio: AudioConfig,

    // Record / Warp / Launch
    pub count_in_bars: u32,
    pub launch_quantization: Quantization,
    /// Warp files of 2 seconds or more when they are dropped or imported.
    pub auto_warp_long_samples: bool,
    /// Warp mode for new clips; `None` lets auto-warp choose.
    pub default_warp_mode: Option<WarpMode>,
    /// Default length of new MIDI clips, in bars.
    pub new_clip_bars: u32,

    // File / Folder
    /// Where new projects are saved by default.
    pub project_folder: Option<PathBuf>,
    /// Your library: presets, clips, templates (the browser lists it).
    pub library_path: Option<PathBuf>,
    /// Minutes between autosaves; 0 turns autosave off.
    pub autosave_minutes: u32,
    pub recent: Vec<PathBuf>,

    // Plug-ins
    /// Open a plug-in's editor window as soon as it is added to a track.
    pub auto_open_plugin_windows: bool,

    // Windows
    /// Show the other main view (Session or Arrangement) in its own window,
    /// so the two can sit on separate monitors.
    pub second_window: bool,
    /// Where the second window was last: outer x, y and inner width, height.
    pub second_window_rect: Option<[f32; 4]>,

    /// Which panels and mixer sections the View menu shows.
    pub view: ViewPanels,

    // MIDI editing
    /// The last Quantize settings (Ctrl+Shift+U); Ctrl+U applies them.
    pub quantize: Quantize,
    /// Quantize to the piano roll's current grid instead of `quantize.grid`.
    pub quantize_follow_grid: bool,
    /// Edit > Record Quantization: snaps recorded MIDI notes.
    pub record_quantize: RecordQuantize,

    // Other
    pub tour_done: bool,
    pub export: ExportPrefs,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            theme: Theme::Dark,
            zoom: 1.0,
            clip_palette: ClipPalette::Classic,
            tooltips: true,
            show_info_view: true,
            audio: AudioConfig::default(),
            count_in_bars: 0,
            launch_quantization: Quantization::Bar,
            auto_warp_long_samples: true,
            default_warp_mode: None,
            new_clip_bars: 1,
            project_folder: None,
            library_path: None,
            autosave_minutes: 2,
            recent: Vec::new(),
            auto_open_plugin_windows: true,
            second_window: false,
            second_window_rect: None,
            view: ViewPanels::default(),
            quantize: Quantize::default(),
            quantize_follow_grid: true,
            record_quantize: RecordQuantize::Off,
            tour_done: false,
            export: ExportPrefs::default(),
        }
    }
}

fn prefs_path() -> Option<PathBuf> {
    app_dir().map(|d| d.join("preferences.json"))
}

impl Preferences {
    /// Loads saved preferences (defaults if there are none or they can't be
    /// read).
    pub fn load() -> Preferences {
        prefs_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| Self::from_json(&t))
            .unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Preferences {
        let mut p: Preferences = serde_json::from_str(text).unwrap_or_default();
        p.sanitize();
        p
    }

    fn sanitize(&mut self) {
        if !self.zoom.is_finite() {
            self.zoom = 1.0;
        }
        self.zoom = self.zoom.clamp(0.5, 2.0);
        self.new_clip_bars = self.new_clip_bars.clamp(1, 64);
        self.recent.truncate(MAX_RECENT);
        if ![1, 2, 4].contains(&self.count_in_bars) {
            self.count_in_bars = 0;
        }
        let q = &mut self.quantize;
        if !(q.grid.is_finite() && (1.0 / 64.0..=16.0).contains(&q.grid)) {
            q.grid = 0.25;
        }
        q.tuplet = q.tuplet.sanitized();
        q.nested = if q.tuplet.is_straight() {
            tessera_engine::rhythm::Tuplet::STRAIGHT
        } else {
            q.nested.sanitized()
        };
        for v in [&mut q.strength, &mut q.swing, &mut q.range, &mut q.humanize] {
            *v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let Some(path) = prefs_path() else {
            return Ok(());
        };
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        // Write next to it and rename so a crash never leaves half a file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    /// Puts `path` at the top of the recent projects.
    pub fn add_recent(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(MAX_RECENT);
    }

    /// The user library folder (default: `Tessera Library` in Documents or
    /// the home folder).
    pub fn library(&self) -> Option<PathBuf> {
        if let Some(p) = &self.library_path {
            return Some(p.clone());
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)?;
        let docs = home.join("Documents");
        Some(if docs.is_dir() { docs } else { home }.join("Tessera Library"))
    }

    /// Folder of project templates (in the library).
    pub fn templates_dir(&self) -> Option<PathBuf> {
        self.library().map(|l| l.join("Templates"))
    }

    /// The template File → New starts from, when it exists.
    pub fn default_template(&self) -> Option<PathBuf> {
        self.templates_dir().map(|d| d.join("Default.tessera"))
    }

    /// Folder for clips saved from the app (Export MIDI Clip, Save to Library).
    pub fn clips_dir(&self) -> Option<PathBuf> {
        self.library().map(|l| l.join("Clips"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_defaults() {
        let p = Preferences::from_json(r#"{"zoom": 1.5, "theme": "Light"}"#);
        assert_eq!(p.zoom, 1.5);
        assert_eq!(p.theme, Theme::Light);
        assert_eq!(p.autosave_minutes, 2);
        assert!(p.tooltips);
    }

    #[test]
    fn garbage_gives_defaults_and_values_are_clamped() {
        assert_eq!(Preferences::from_json("not json"), Preferences::default());
        let p = Preferences::from_json(r#"{"zoom": 9.0, "count_in_bars": 3, "new_clip_bars": 0}"#);
        assert_eq!(p.zoom, 2.0);
        assert_eq!(p.count_in_bars, 0);
        assert_eq!(p.new_clip_bars, 1);
    }

    #[test]
    fn round_trip() {
        let mut p = Preferences {
            theme: Theme::HighContrast,
            ..Default::default()
        };
        p.audio.buffer_size = Some(256);
        p.export.format = AudioFormat::Flac;
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(Preferences::from_json(&json), p);
    }

    #[test]
    fn recent_projects_are_unique_and_capped() {
        let mut p = Preferences::default();
        for i in 0..15 {
            p.add_recent(Path::new(&format!("/s/{i}.tessera")));
        }
        p.add_recent(Path::new("/s/3.tessera"));
        assert_eq!(p.recent.len(), MAX_RECENT);
        assert_eq!(p.recent[0], PathBuf::from("/s/3.tessera"));
        assert_eq!(p.recent.iter().filter(|r| r.ends_with("3.tessera")).count(), 1);
    }

    #[test]
    fn palettes_wrap() {
        for pal in ClipPalette::ALL {
            let n = pal.colors().len();
            assert_eq!(pal.color(0), pal.color(n));
        }
    }
}
