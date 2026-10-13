//! Warping: playing audio clips in time with the project tempo.
//!
//! A clip's [`WarpSettings`] map clip beats to positions in the source file
//! through warp markers. Between two markers the mapping is linear; outside
//! the markers it continues at the tempo of the nearest segment. With fewer
//! than two markers the clip's own tempo ([`WarpSettings::bpm`]) defines it.
//!
//! The engine then reads the source along that mapping using one of the warp
//! modes:
//!
//! * **Beats** slices the audio at transients and starts every slice at its
//!   warped position, playing it at the original speed. Slowing down leaves
//!   short gaps (with fades); speeding up cuts slices short. Best for drums.
//! * **Tones** is a granular time-stretch: overlapping Hann-windowed grains
//!   taken from the warped position. Preserves pitch; good for monophonic and
//!   tonal material.
//! * **Complex** is WSOLA: like Tones, but each grain is shifted by up to a
//!   few milliseconds to line up with the waveform of the previous grain,
//!   which removes most of the phasiness of plain granular stretching. Best
//!   for full mixes.
//! * **Re-Pitch** resamples: tempo and pitch change together, like a turntable.
//!
//! All playback functions here are allocation-free.

use serde::{Deserialize, Serialize};

use crate::model::AudioData;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WarpMode {
    #[default]
    Beats,
    Tones,
    Complex,
    RePitch,
}

impl WarpMode {
    pub const ALL: [WarpMode; 4] = [WarpMode::Beats, WarpMode::Tones, WarpMode::Complex, WarpMode::RePitch];

    pub fn label(self) -> &'static str {
        match self {
            WarpMode::Beats => "Beats",
            WarpMode::Tones => "Tones",
            WarpMode::Complex => "Complex",
            WarpMode::RePitch => "Re-Pitch",
        }
    }
}

/// Pins a position in the source file (seconds) to a clip beat.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WarpMarker {
    pub beat: f64,
    pub secs: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WarpSettings {
    /// When off, the clip plays at the file's own speed regardless of tempo.
    pub enabled: bool,
    pub mode: WarpMode,
    /// Sorted, strictly increasing in both beat and seconds.
    pub markers: Vec<WarpMarker>,
    /// Tempo of the source material, used where markers don't define it.
    pub bpm: f64,
    /// Transposition in semitones (ignored in Re-Pitch mode, where the pitch
    /// follows the tempo).
    pub transpose: f32,
    /// Detected transient positions in seconds (see [`crate::analysis`]).
    pub transients: Vec<f64>,
}

impl Default for WarpSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            mode: WarpMode::Beats,
            markers: Vec::new(),
            bpm: 120.0,
            transpose: 0.0,
            transients: Vec::new(),
        }
    }
}

/// Minimum distance between warp markers.
const MIN_MARKER_BEATS: f64 = 1.0 / 64.0;
const MIN_MARKER_SECS: f64 = 0.002;

impl WarpSettings {
    fn secs_per_beat(&self) -> f64 {
        60.0 / self.bpm.clamp(5.0, 999.0)
    }

    /// Source position in seconds for a clip beat.
    pub fn beat_to_secs(&self, beat: f64) -> f64 {
        beat_to_secs(&self.markers, self.secs_per_beat(), beat)
    }

    /// Clip beat for a source position in seconds.
    pub fn secs_to_beat(&self, secs: f64) -> f64 {
        secs_to_beat(&self.markers, self.secs_per_beat(), secs)
    }

    /// Tempo of the source around `beat` (the "segment BPM").
    pub fn bpm_at(&self, beat: f64) -> f64 {
        let d = 1e-3;
        let s = self.beat_to_secs(beat + d) - self.beat_to_secs(beat);
        if s <= 0.0 {
            self.bpm
        } else {
            60.0 * d / s
        }
    }

    /// Adds a marker at `beat`, pinning whatever audio currently plays there.
    /// Returns its index, or `None` if it would sit on top of another marker.
    pub fn add_marker(&mut self, beat: f64) -> Option<usize> {
        let secs = self.beat_to_secs(beat);
        if self
            .markers
            .iter()
            .any(|m| (m.beat - beat).abs() < MIN_MARKER_BEATS || (m.secs - secs).abs() < MIN_MARKER_SECS)
        {
            return None;
        }
        let i = self.markers.partition_point(|m| m.beat < beat);
        self.markers.insert(i, WarpMarker { beat, secs });
        Some(i)
    }

    /// Moves marker `index` to `beat` (the audio under it moves along),
    /// keeping it between its neighbours.
    pub fn move_marker(&mut self, index: usize, beat: f64) {
        if index >= self.markers.len() {
            return;
        }
        let lo = index
            .checked_sub(1)
            .map(|i| self.markers[i].beat + MIN_MARKER_BEATS)
            .unwrap_or(f64::NEG_INFINITY);
        let hi = self
            .markers
            .get(index + 1)
            .map(|m| m.beat - MIN_MARKER_BEATS)
            .unwrap_or(f64::INFINITY);
        self.markers[index].beat = beat.clamp(lo, hi.max(lo));
    }

    pub fn remove_marker(&mut self, index: usize) {
        if index < self.markers.len() {
            self.markers.remove(index);
        }
    }

    /// Changes the clip tempo, rescaling marker positions around the first
    /// marker so the relative timing is kept.
    pub fn set_bpm(&mut self, bpm: f64) {
        let bpm = bpm.clamp(5.0, 999.0);
        let ratio = bpm / self.bpm;
        if let Some(first) = self.markers.first().copied() {
            for m in self.markers.iter_mut() {
                m.beat = first.beat + (m.beat - first.beat) * ratio;
            }
        }
        self.bpm = bpm;
    }

    /// Pitch ratio from the transpose setting.
    pub fn rate(&self) -> f64 {
        2f64.powf(self.transpose.clamp(-48.0, 48.0) as f64 / 12.0)
    }
}

/// Linear map through markers. `spb` (source seconds per beat) is used with
/// fewer than two markers.
pub fn beat_to_secs(markers: &[WarpMarker], spb: f64, beat: f64) -> f64 {
    match markers.len() {
        0 => beat * spb,
        1 => markers[0].secs + (beat - markers[0].beat) * spb,
        n => {
            let i = markers.partition_point(|m| m.beat <= beat).clamp(1, n - 1);
            let (a, b) = (markers[i - 1], markers[i]);
            let slope = (b.secs - a.secs) / (b.beat - a.beat).max(1e-12);
            a.secs + (beat - a.beat) * slope
        }
    }
}

pub fn secs_to_beat(markers: &[WarpMarker], spb: f64, secs: f64) -> f64 {
    match markers.len() {
        0 => secs / spb,
        1 => markers[0].beat + (secs - markers[0].secs) / spb,
        n => {
            let i = markers.partition_point(|m| m.secs <= secs).clamp(1, n - 1);
            let (a, b) = (markers[i - 1], markers[i]);
            let slope = (b.beat - a.beat) / (b.secs - a.secs).max(1e-12);
            a.beat + (secs - a.secs) * slope
        }
    }
}

// ----- playback -----------------------------------------------------------------

/// Grain length for Tones mode, in seconds.
const TONES_GRAIN: f64 = 0.070;
/// Grain length and alignment search range for Complex (WSOLA) mode.
const WSOLA_GRAIN: f64 = 0.046;
const WSOLA_TOLERANCE: f64 = 0.010;
const WSOLA_COMPARE: f64 = 0.012;
/// Fades used by Beats mode at slice ends.
const SLICE_FADE_OUT: f64 = 0.004;
const SLICE_FADE_IN: f64 = 0.0007;

/// One Beats-mode slice: starts at `beat` with source position `secs`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slice {
    pub beat: f64,
    pub secs: f64,
}

/// Audio-thread form of [`WarpSettings`], compiled on the UI thread.
#[derive(Debug, Clone)]
pub struct WarpPlayback {
    pub enabled: bool,
    pub mode: WarpMode,
    markers: Vec<WarpMarker>,
    spb: f64,
    rate: f64,
    slices: Vec<Slice>,
    duration: f64,
}

impl WarpPlayback {
    /// Compiles settings for a source of `duration` seconds. Allocates.
    pub fn new(w: &WarpSettings, duration: f64) -> Self {
        let spb = w.secs_per_beat();
        let mut slices = Vec::new();
        if w.enabled && w.mode == WarpMode::Beats {
            let mut points: Vec<f64> = w
                .transients
                .iter()
                .copied()
                .chain(w.markers.iter().map(|m| m.secs))
                .chain(std::iter::once(0.0))
                .filter(|s| *s >= 0.0 && *s < duration)
                .collect();
            points.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            points.dedup_by(|a, b| (*a - *b).abs() < 0.01);
            let mut beats: Vec<f64> = points.iter().map(|&s| secs_to_beat(&w.markers, spb, s)).collect();
            // Long stretches without transients still need slicing, or Beats
            // mode would not stretch them at all.
            let end_beat = secs_to_beat(&w.markers, spb, duration);
            beats.push(end_beat);
            let mut filled = Vec::with_capacity(beats.len() * 2);
            for pair in beats.windows(2) {
                let (b0, b1) = (pair[0], pair[1]);
                filled.push(b0);
                if b1 - b0 > 1.0 {
                    let mut b = (b0 / 0.5).floor() * 0.5 + 0.5;
                    while b < b1 - 0.25 {
                        if b > b0 + 0.25 {
                            filled.push(b);
                        }
                        b += 0.5;
                    }
                }
            }
            slices = filled
                .into_iter()
                .map(|beat| Slice {
                    beat,
                    secs: beat_to_secs(&w.markers, spb, beat),
                })
                .collect();
        }
        WarpPlayback {
            enabled: w.enabled,
            mode: w.mode,
            markers: w.markers.clone(),
            spb,
            rate: w.rate(),
            slices,
            duration,
        }
    }

    #[inline]
    pub fn beat_to_secs(&self, beat: f64) -> f64 {
        beat_to_secs(&self.markers, self.spb, beat)
    }

    pub fn slices(&self) -> &[Slice] {
        &self.slices
    }
}

/// Per-track state for Complex mode: the aligned start of recent grains.
#[derive(Debug, Clone, Default)]
pub struct WarpState {
    grains: [(usize, i64, f64); 4],
    next: usize,
}

impl WarpState {
    fn get(&self, key: usize, j: i64) -> Option<f64> {
        self.grains
            .iter()
            .find(|g| g.0 == key && g.1 == j && key != 0)
            .map(|g| g.2)
    }

    fn put(&mut self, key: usize, j: i64, start: f64) {
        self.grains[self.next] = (key, j, start);
        self.next = (self.next + 1) % self.grains.len();
    }
}

/// 4-point Hermite interpolation of the source at `secs`.
#[inline]
pub fn read(data: &AudioData, secs: f64) -> (f32, f32) {
    let pos = secs * data.sample_rate as f64;
    if pos < 0.0 {
        return (0.0, 0.0);
    }
    let i = pos as usize;
    let n = data.frames();
    if i + 1 >= n {
        return (0.0, 0.0);
    }
    let t = (pos - i as f64) as f32;
    let at = |v: &[f32], k: isize| -> f32 {
        let k = (i as isize + k).clamp(0, n as isize - 1) as usize;
        v[k]
    };
    let h = |v: &[f32]| -> f32 {
        let (y0, y1, y2, y3) = (at(v, -1), at(v, 0), at(v, 1), at(v, 2));
        let c1 = 0.5 * (y2 - y0);
        let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
        let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
        ((c3 * t + c2) * t + c1) * t + y1
    };
    (h(&data.left), h(&data.right))
}

/// Sample of a clip at clip beat `pos` (already wrapped for looping), with the
/// project at `spb_out` seconds per beat. `key` identifies the clip for the
/// Complex-mode grain cache.
#[inline]
pub fn sample_at(
    data: &AudioData,
    w: &WarpPlayback,
    pos: f64,
    spb_out: f64,
    key: usize,
    state: &mut WarpState,
) -> (f32, f32) {
    if !w.enabled {
        return read(data, pos * spb_out * w.rate);
    }
    match w.mode {
        WarpMode::RePitch => read(data, w.beat_to_secs(pos)),
        WarpMode::Tones => granular(data, w, pos, spb_out, TONES_GRAIN, |j| {
            // Grain j is centred on output beat (j + 1) * hop.
            w.beat_to_secs((j + 1) as f64 * TONES_GRAIN * 0.5 / spb_out)
        }),
        WarpMode::Complex => {
            let hop_out = WSOLA_GRAIN * 0.5;
            let hop_b = hop_out / spb_out;
            let k = (pos / hop_b).floor() as i64;
            // Make sure grain k-1 is aligned before grain k uses it.
            let s_prev = wsola_start(data, w, k - 1, hop_b, hop_out, key, state);
            let s_cur = wsola_start(data, w, k, hop_b, hop_out, key, state);
            granular(
                data,
                w,
                pos,
                spb_out,
                WSOLA_GRAIN,
                |j| if j == k { s_cur } else { s_prev },
            )
        }
        WarpMode::Beats => beats(data, w, pos, spb_out),
    }
}

/// Overlap-add of two Hann grains (50% overlap sums to one). Grain `j`
/// spans output beats `j * hop .. (j + 2) * hop`; `centre(j)` is the source
/// position played at its centre, where its window peaks.
#[inline]
fn granular(
    data: &AudioData,
    w: &WarpPlayback,
    pos: f64,
    spb_out: f64,
    grain: f64,
    centre: impl Fn(i64) -> f64,
) -> (f32, f32) {
    let hop_b = grain * 0.5 / spb_out;
    let k = (pos / hop_b).floor() as i64;
    let mut l = 0.0f32;
    let mut r = 0.0f32;
    for j in [k - 1, k] {
        let g0 = j as f64 * hop_b;
        let x = (pos - g0) / (2.0 * hop_b);
        if !(0.0..1.0).contains(&x) {
            continue;
        }
        let win = (std::f64::consts::PI * x).sin();
        let win = (win * win) as f32;
        let offset = (pos - g0 - hop_b) * spb_out;
        let (sl, sr) = read(data, centre(j) + offset * w.rate);
        l += sl * win;
        r += sr * win;
    }
    (l, r)
}

/// Aligned source position at the centre of WSOLA grain `j`.
fn wsola_start(
    data: &AudioData,
    w: &WarpPlayback,
    j: i64,
    hop_b: f64,
    hop_out: f64,
    key: usize,
    state: &mut WarpState,
) -> f64 {
    if let Some(s) = state.get(key, j) {
        return s;
    }
    let nominal = w.beat_to_secs((j + 1) as f64 * hop_b);
    let prev = state
        .get(key, j - 1)
        .unwrap_or_else(|| w.beat_to_secs(j as f64 * hop_b));
    // Where the previous grain's audio would naturally continue.
    let natural = prev + hop_out * w.rate;
    let start = align(data, natural, nominal);
    state.put(key, j, start);
    start
}

/// Searches around `nominal` for the source position whose waveform best
/// matches the audio at `target` (normalised cross-correlation).
fn align(data: &AudioData, target: f64, nominal: f64) -> f64 {
    let sr = data.sample_rate as f64;
    let n = data.frames() as isize;
    let t0 = (target * sr) as isize;
    let c0 = (nominal * sr) as isize;
    let tol = (WSOLA_TOLERANCE * sr) as isize;
    let len = (WSOLA_COMPARE * sr) as isize;
    if t0 < 0 || c0 - tol < 0 || t0 + len >= n || c0 + tol + len >= n {
        return nominal;
    }
    let mono = |i: isize| data.left[i as usize] + data.right[i as usize];
    let step = 2;
    let mut best = 0isize;
    let mut best_score = f32::NEG_INFINITY;
    let mut d = -tol;
    while d <= tol {
        let mut dot = 0.0f32;
        let mut energy = 1e-9f32;
        let mut i = 0;
        while i < len {
            let c = mono(c0 + d + i);
            dot += mono(t0 + i) * c;
            energy += c * c;
            i += step;
        }
        let score = dot / energy.sqrt();
        if score > best_score {
            best_score = score;
            best = d;
        }
        d += step;
    }
    nominal + best as f64 / sr
}

/// Beats mode: each slice starts at its warped beat and plays at the source
/// speed.
#[inline]
fn beats(data: &AudioData, w: &WarpPlayback, pos: f64, spb_out: f64) -> (f32, f32) {
    let s = &w.slices;
    if s.is_empty() {
        return read(data, w.beat_to_secs(pos));
    }
    let i = s.partition_point(|x| x.beat <= pos).saturating_sub(1);
    let slice = s[i];
    let elapsed = (pos - slice.beat) * spb_out;
    let src = slice.secs + elapsed * w.rate;
    let (next_beat, next_secs) = match s.get(i + 1) {
        Some(n) => (n.beat, n.secs),
        None => (f64::INFINITY, w.duration),
    };
    let gap = ((next_secs - src) / SLICE_FADE_OUT).clamp(0.0, 1.0);
    if gap <= 0.0 {
        return (0.0, 0.0);
    }
    let out = ((next_beat - pos) * spb_out / SLICE_FADE_OUT).clamp(0.0, 1.0);
    let fade_in = if i == 0 {
        1.0
    } else {
        (elapsed / SLICE_FADE_IN).clamp(0.0, 1.0)
    };
    let g = (gap * out * fade_in) as f32;
    let (l, r) = read(data, src);
    (l * g, r * g)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, secs: f64, sr: u32) -> AudioData {
        let n = (secs * sr as f64) as usize;
        let v: Vec<f32> = (0..n)
            .map(|i| (i as f64 / sr as f64 * freq * std::f64::consts::TAU).sin() as f32 * 0.5)
            .collect();
        AudioData {
            sample_rate: sr,
            left: v.clone(),
            right: v,
        }
    }

    /// Estimated frequency from zero crossings.
    fn freq_of(samples: &[f32], sr: f64) -> f64 {
        let crossings = samples.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        crossings as f64 / (samples.len() as f64 / sr)
    }

    fn render(data: &AudioData, w: &WarpSettings, tempo: f64, beats: f64, sr: f64) -> Vec<f32> {
        let p = WarpPlayback::new(w, data.duration_secs());
        let spb = 60.0 / tempo;
        let n = (beats * spb * sr) as usize;
        let mut st = WarpState::default();
        (0..n)
            .map(|i| sample_at(data, &p, i as f64 / sr / spb, spb, 1, &mut st).0)
            .collect()
    }

    #[test]
    fn marker_map_is_piecewise_linear_and_invertible() {
        let mut w = WarpSettings {
            bpm: 120.0,
            ..Default::default()
        };
        assert_eq!(w.beat_to_secs(4.0), 2.0);
        w.add_marker(0.0);
        w.add_marker(4.0);
        w.move_marker(1, 8.0); // stretch: 2 s of audio now spans 8 beats
        assert!((w.beat_to_secs(8.0) - 2.0).abs() < 1e-9);
        assert!((w.beat_to_secs(4.0) - 1.0).abs() < 1e-9);
        assert!((w.bpm_at(2.0) - 240.0).abs() < 1e-3);
        for b in [-1.0, 0.0, 3.3, 8.0, 12.0] {
            assert!((w.secs_to_beat(w.beat_to_secs(b)) - b).abs() < 1e-9);
        }
        assert!(w.add_marker(8.0).is_none(), "no duplicate markers");
        w.move_marker(0, 100.0);
        assert!(w.markers[0].beat < w.markers[1].beat, "markers keep their order");
    }

    #[test]
    fn set_bpm_rescales_markers() {
        let mut w = WarpSettings::default();
        w.add_marker(0.0);
        w.add_marker(4.0);
        w.set_bpm(60.0);
        assert!((w.markers[1].beat - 2.0).abs() < 1e-9);
        assert!((w.beat_to_secs(2.0) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn repitch_changes_pitch_with_tempo() {
        let sr = 48000.0;
        let data = sine(440.0, 2.0, 48000);
        let w = WarpSettings {
            enabled: true,
            mode: WarpMode::RePitch,
            bpm: 120.0,
            ..Default::default()
        };
        let out = render(&data, &w, 240.0, 3.0, sr);
        let f = freq_of(&out[1000..out.len() - 1000], sr);
        assert!((f - 880.0).abs() < 15.0, "double tempo doubles pitch, got {f}");
    }

    #[test]
    fn stretch_modes_keep_pitch_and_follow_tempo() {
        let sr = 48000.0;
        let data = sine(330.0, 3.0, 48000);
        // (Beats mode leaves gaps when slowing down; it has its own test.)
        for mode in [WarpMode::Tones, WarpMode::Complex] {
            let w = WarpSettings {
                enabled: true,
                mode,
                bpm: 120.0,
                ..Default::default()
            };
            // Half tempo: 4 beats of a 120 BPM file take 4 s instead of 2 s.
            let out = render(&data, &w, 60.0, 4.0, sr);
            assert_eq!(out.len(), 4 * 48000);
            let mid = &out[24000..out.len() - 24000];
            let f = freq_of(mid, sr);
            assert!((f - 330.0).abs() < 25.0, "{mode:?} keeps pitch, got {f}");
            let rms = (mid.iter().map(|s| s * s).sum::<f32>() / mid.len() as f32).sqrt();
            assert!(rms > 0.25, "{mode:?} stays loud (no cancelling), rms {rms}");
        }
    }

    #[test]
    fn transpose_shifts_pitch_without_changing_length() {
        let sr = 48000.0;
        let data = sine(300.0, 3.0, 48000);
        let w = WarpSettings {
            enabled: true,
            mode: WarpMode::Complex,
            bpm: 120.0,
            transpose: 12.0,
            ..Default::default()
        };
        let out = render(&data, &w, 120.0, 4.0, sr);
        let f = freq_of(&out[12000..out.len() - 12000], sr);
        assert!((f - 600.0).abs() < 30.0, "octave up, got {f}");
    }

    #[test]
    fn beats_mode_starts_slices_on_their_warped_beat() {
        // Clicks every 0.5 s (120 BPM); play at 60 BPM so each click lands
        // every second and is followed by silence.
        let sr = 48000;
        let mut v = vec![0.0f32; sr as usize * 2];
        for k in 0..4 {
            for i in 0..200 {
                v[k * 24000 + i] = 0.8 * (1.0 - i as f32 / 200.0);
            }
        }
        let data = AudioData {
            sample_rate: sr,
            left: v.clone(),
            right: v,
        };
        let w = WarpSettings {
            enabled: true,
            mode: WarpMode::Beats,
            bpm: 120.0,
            transients: vec![0.0, 0.5, 1.0, 1.5],
            ..Default::default()
        };
        let out = render(&data, &w, 60.0, 4.0, sr as f64);
        for k in 0..4 {
            let at = k * 48000;
            let peak = out[at..at + 400].iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(peak > 0.5, "click {k} at its beat, peak {peak}");
            let between = out[at + 2000..at + 46000].iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(between < 1e-3, "gap after click {k}, got {between}");
        }
    }

    #[test]
    fn unwarped_clips_play_at_source_speed() {
        let sr = 48000.0;
        let data = sine(440.0, 2.0, 48000);
        let w = WarpSettings::default();
        let out = render(&data, &w, 200.0, 4.0, sr);
        let f = freq_of(&out[1000..out.len() - 1000], sr);
        assert!((f - 440.0).abs() < 10.0, "got {f}");
    }

    #[test]
    fn settings_round_trip_and_default_when_missing() {
        let mut w = WarpSettings {
            enabled: true,
            mode: WarpMode::Complex,
            ..Default::default()
        };
        w.add_marker(1.0);
        let json = serde_json::to_string(&w).unwrap();
        let back: WarpSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
        let partial: WarpSettings = serde_json::from_str("{\"enabled\":true}").unwrap();
        assert!(partial.enabled);
        assert_eq!(partial.bpm, 120.0);
    }
}
