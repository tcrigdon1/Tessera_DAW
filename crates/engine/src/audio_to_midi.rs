//! Offline audio to MIDI conversion, in the spirit of Ableton's "Convert
//! Melody / Harmony / Drums to New MIDI Track", plus the slice points used by
//! "Slice to New MIDI Track". Runs on the UI thread (allocates).
//!
//! * Melody: monophonic pitch tracking with YIN, segmented into notes on
//!   pitch changes and onsets.
//! * Harmony: per segment between onsets, a harmonic-sum pitch picker over an
//!   averaged magnitude spectrum that cancels the overtones of each pitch it
//!   accepts.
//! * Drums: onsets classified as kick, snare or closed hi-hat by band energy.

use crate::analysis::detect_transients;
use crate::model::{AudioData, Note};

/// Which kind of material to convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertMode {
    /// A monophonic line (voice, bass, lead).
    Melody,
    /// Polyphonic material (chords, piano, pads).
    Harmony,
    /// Percussion, mapped to kick (36), snare (38) and closed hat (42).
    Drums,
}

/// Levels below this (linear RMS) are treated as silence.
const ABS_GATE: f32 = 3e-4;
/// Frames quieter than this fraction of the loudest frame are ignored.
const REL_GATE: f32 = 0.03;
/// Analysis hop for level and pitch tracking, in seconds.
const HOP_SECS: f64 = 0.01;
/// Length of a drum hit, in beats (a sixteenth note).
const DRUM_LENGTH: f64 = 0.25;
/// Most pitches Harmony emits per segment.
const MAX_VOICES: usize = 6;

/// General MIDI kick drum.
const KICK: u8 = 36;
/// General MIDI acoustic snare.
const SNARE: u8 = 38;
/// General MIDI closed hi-hat.
const CLOSED_HAT: u8 = 42;

/// A detected note before it is mapped to beats.
struct Event {
    pitch: u8,
    /// Loudness measure used for velocity (relative to the loudest event).
    level: f32,
    start: f64,
    end: f64,
    /// A fixed length in beats instead of `end`.
    fixed_length: Option<f64>,
}

/// Converts audio to notes. `secs_to_beat` maps a time in the file (seconds)
/// to a clip position in beats (callers pass the clip's warp mapping, or
/// `secs * tempo / 60`). Returned notes are sorted by start, in beats.
pub fn convert(data: &AudioData, mode: ConvertMode, secs_to_beat: &dyn Fn(f64) -> f64) -> Vec<Note> {
    if data.frames() == 0 || data.sample_rate == 0 {
        return Vec::new();
    }
    let x = mono(data);
    let sr = data.sample_rate as f64;
    let transients = detect_transients(data);
    let events = match mode {
        ConvertMode::Melody => melody(&x, sr, &transients),
        ConvertMode::Harmony => harmony(&x, sr, &transients),
        ConvertMode::Drums => drums(&x, sr, &transients),
    };
    finish(events, secs_to_beat)
}

/// Slice start points in seconds for "Slice to New MIDI Track": by transients
/// (`None`) or by a fixed beat division in beats (`Some(div)`, using the
/// `secs_to_beat`/`beat_to_secs` mapping), capped at `max` slices, always
/// starting at 0.0, ascending and unique.
pub fn slice_points(
    data: &AudioData,
    division: Option<f64>,
    max: usize,
    secs_to_beat: &dyn Fn(f64) -> f64,
    beat_to_secs: &dyn Fn(f64) -> f64,
) -> Vec<f64> {
    let max = max.max(1);
    let dur = data.duration_secs();
    let mut out = vec![0.0];
    // Points closer than this to the previous one are dropped.
    let min_gap = 0.01;
    let push = |t: f64, out: &mut Vec<f64>| {
        if t.is_finite() && t < dur && out.last().is_some_and(|&last| t - last >= min_gap) {
            out.push(t);
        }
    };
    match division {
        Some(div) if div.is_finite() && div > 0.0 => {
            let first = secs_to_beat(0.0);
            let last = secs_to_beat(dur);
            if first.is_finite() && last.is_finite() {
                let mut k = (first / div).floor() + 1.0;
                while k * div < last && out.len() < max {
                    push(beat_to_secs(k * div), &mut out);
                    k += 1.0;
                }
            }
        }
        Some(_) => {}
        None => {
            for t in detect_transients(data) {
                if out.len() >= max {
                    break;
                }
                push(t, &mut out);
            }
        }
    }
    out.truncate(max);
    out
}

/// Maps events to notes: velocities relative to the loudest event, positions
/// through `secs_to_beat`, clipped to non-negative beats and sorted.
fn finish(events: Vec<Event>, secs_to_beat: &dyn Fn(f64) -> f64) -> Vec<Note> {
    let max_level = events.iter().map(|e| e.level).fold(0.0f32, f32::max);
    let mut notes = Vec::with_capacity(events.len());
    for e in events {
        let mut start = secs_to_beat(e.start);
        let mut length = match e.fixed_length {
            Some(len) => len,
            None => secs_to_beat(e.end) - start,
        };
        if start < 0.0 {
            length += start;
            start = 0.0;
        }
        if !start.is_finite() || !length.is_finite() || length <= 1e-6 {
            continue;
        }
        notes.push(Note {
            pitch: e.pitch.min(127),
            velocity: velocity(e.level, max_level),
            start,
            length,
        });
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    notes
}

/// Maps a level to a MIDI velocity: the loudest level gives 120, and each
/// decibel below it takes off 2.
fn velocity(level: f32, max_level: f32) -> u8 {
    if max_level <= 0.0 || level <= 0.0 {
        return 1;
    }
    let db = 20.0 * (level / max_level).log10();
    (120.0 + 2.0 * db).round().clamp(1.0, 127.0) as u8
}

fn mono(data: &AudioData) -> Vec<f32> {
    data.left.iter().zip(&data.right).map(|(l, r)| 0.5 * (l + r)).collect()
}

/// RMS of `x[start..start + len]`, treating samples past the end as silence.
fn rms_of(x: &[f32], start: usize, len: usize) -> f32 {
    if len == 0 {
        return 0.0;
    }
    let end = (start + len).min(x.len());
    let sum: f32 = x.get(start..end).unwrap_or(&[]).iter().map(|v| v * v).sum();
    (sum / len as f32).sqrt()
}

/// RMS per hop-sized frame, the loudest frame, and the silence gate.
fn levels(x: &[f32], hop: usize) -> (Vec<f32>, f32) {
    let frames = x.len().div_ceil(hop);
    let rms: Vec<f32> = (0..frames).map(|f| rms_of(x, f * hop, hop)).collect();
    let max = rms.iter().copied().fold(0.0f32, f32::max);
    (rms, (max * REL_GATE).max(ABS_GATE))
}

fn midi_to_hz(pitch: f64) -> f64 {
    440.0 * 2f64.powf((pitch - 69.0) / 12.0)
}

fn hz_to_midi(hz: f64) -> f64 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// In-place iterative radix-2 FFT. `re.len()` must be a power of two. The
/// inverse transform is not scaled.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && im.len() == n);
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * std::f64::consts::PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for chunk in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let a = chunk + k;
                let b = a + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let next = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = next;
            }
        }
        len <<= 1;
    }
}

/// Magnitude spectrum (bins `0..n/2`) of `x[start..start + len]` under a Hann
/// window, zero padded to `n`.
fn magnitude(x: &[f32], start: usize, len: usize, n: usize) -> Vec<f64> {
    let len = len.min(n);
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    for (i, v) in re.iter_mut().enumerate().take(len) {
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / len as f64).cos();
        *v = x.get(start + i).copied().unwrap_or(0.0) as f64 * w;
    }
    fft(&mut re, &mut im, false);
    re.iter()
        .zip(&im)
        .take(n / 2)
        .map(|(r, i)| (r * r + i * i).sqrt())
        .collect()
}

// ---------------------------------------------------------------- melody --

/// A run of analysis frames with one pitch (or none), `start..end`.
#[derive(Debug, Clone, Copy)]
struct Run {
    pitch: Option<i32>,
    start: usize,
    end: usize,
}

impl Run {
    fn len(&self) -> usize {
        self.end - self.start
    }
}

/// YIN pitch tracker. Returns the rounded MIDI pitch per hop-sized frame, or
/// `None` where the frame is unvoiced or quiet. The YIN difference function is
/// computed through an FFT cross-correlation.
fn track_pitch(x: &[f32], sr: f64, hop: usize, rms: &[f32], gate: f32) -> Vec<Option<i32>> {
    const THRESHOLD: f64 = 0.15;
    let w = ((sr * 0.023).round() as usize).max(32);
    let tau_min = ((sr / 2000.0).floor() as usize).max(2);
    let tau_max = (sr / 50.0).ceil() as usize;
    let l = w + tau_max;
    let n = (l + w).next_power_of_two();
    let mut ar = vec![0.0; n];
    let mut ai = vec![0.0; n];
    let mut br = vec![0.0; n];
    let mut bi = vec![0.0; n];
    let mut energy = vec![0.0; l + 1];
    let mut cmnd = vec![1.0; tau_max + 2];
    let mut out = Vec::with_capacity(rms.len());
    for (f, &level) in rms.iter().enumerate() {
        // The integration window starts half a window before the frame.
        let s = (f * hop + hop / 2).saturating_sub(w / 2);
        if level < gate || rms_of(x, s, w) < gate {
            out.push(None);
            continue;
        }
        ar.fill(0.0);
        ai.fill(0.0);
        bi.fill(0.0);
        br.fill(0.0);
        for i in 0..l {
            let v = x.get(s + i).copied().unwrap_or(0.0) as f64;
            br[i] = v;
            if i < w {
                ar[i] = v;
            }
            energy[i + 1] = energy[i] + v * v;
        }
        fft(&mut ar, &mut ai, false);
        fft(&mut br, &mut bi, false);
        // conj(A) * B gives the cross-correlation at positive lags.
        for k in 0..n {
            let (r, i) = (ar[k] * br[k] + ai[k] * bi[k], ar[k] * bi[k] - ai[k] * br[k]);
            ar[k] = r;
            ai[k] = i;
        }
        fft(&mut ar, &mut ai, true);
        let e0 = energy[w];
        let mut running = 0.0;
        cmnd[0] = 1.0;
        for tau in 1..=tau_max {
            let corr = ar[tau] / n as f64;
            let d = (e0 + energy[tau + w] - energy[tau] - 2.0 * corr).max(0.0);
            running += d;
            cmnd[tau] = if running > 0.0 { d * tau as f64 / running } else { 1.0 };
        }
        let mut found = None;
        let mut tau = tau_min;
        while tau <= tau_max {
            if cmnd[tau] < THRESHOLD {
                while tau < tau_max && cmnd[tau + 1] < cmnd[tau] {
                    tau += 1;
                }
                found = Some(tau);
                break;
            }
            tau += 1;
        }
        let pitch = found.and_then(|tau| {
            let mut t = tau as f64;
            if tau > 1 && tau < tau_max {
                let (a, b, c) = (cmnd[tau - 1], cmnd[tau], cmnd[tau + 1]);
                let d = a - 2.0 * b + c;
                if d.abs() > 1e-12 {
                    t += 0.5 * (a - c) / d;
                }
            }
            let hz = sr / t;
            (50.0..=2000.0).contains(&hz).then(|| hz_to_midi(hz).round() as i32)
        });
        out.push(pitch);
    }
    out
}

/// Run-length encodes per-frame pitches.
fn runs(pitch: &[Option<i32>]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for (f, &p) in pitch.iter().enumerate() {
        match out.last_mut() {
            Some(r) if r.pitch == p => r.end = f + 1,
            _ => out.push(Run {
                pitch: p,
                start: f,
                end: f + 1,
            }),
        }
    }
    out
}

/// Joins neighbouring runs of the same pitch.
fn join_equal(runs: Vec<Run>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::with_capacity(runs.len());
    for r in runs {
        match out.last_mut() {
            Some(last) if last.pitch == r.pitch => last.end = r.end,
            _ => out.push(r),
        }
    }
    out
}

fn melody(x: &[f32], sr: f64, transients: &[f64]) -> Vec<Event> {
    let hop = ((sr * HOP_SECS).round() as usize).max(1);
    let (rms, gate) = levels(x, hop);
    if rms.iter().all(|&r| r < ABS_GATE) {
        return Vec::new();
    }
    let pitch = track_pitch(x, sr, hop, &rms, gate);
    // Pitch changes must be held this long (frames) to start a new note.
    let min_hold = ((0.06 / HOP_SECS).round() as usize).max(1);
    // Unvoiced gaps up to this long (frames) inside a note are bridged.
    let max_gap = ((0.03 / HOP_SECS).round() as usize).max(1);

    let mut rs = runs(&pitch);
    // Bridge tiny gaps between frames of the same pitch.
    for i in 1..rs.len().saturating_sub(1) {
        if rs[i].pitch.is_none()
            && rs[i].len() <= max_gap
            && rs[i - 1].pitch.is_some()
            && rs[i - 1].pitch == rs[i + 1].pitch
        {
            rs[i].pitch = rs[i - 1].pitch;
        }
    }
    let mut rs = join_equal(rs);
    // Absorb short pitch excursions (transitions, glitches) into a neighbour.
    let mut i = 0;
    while i < rs.len() {
        let r = rs[i];
        if r.pitch.is_some() && r.len() < min_hold {
            if i > 0 && rs[i - 1].pitch.is_some() {
                rs[i - 1].end = r.end;
                rs.remove(i);
                continue;
            }
            if i + 1 < rs.len() && rs[i + 1].pitch.is_some() {
                rs[i + 1].start = r.start;
                rs.remove(i);
                continue;
            }
        }
        i += 1;
    }
    let rs = join_equal(rs);

    // Frame boundaries in seconds.
    let at = |f: usize| (f * hop) as f64 / sr;
    let mut notes: Vec<(i32, f64, f64)> = Vec::new();
    for r in rs.iter() {
        let Some(p) = r.pitch else { continue };
        let mut start = at(r.start);
        let end = at(r.end).min(x.len() as f64 / sr);
        // Snap the start to a nearby onset.
        if let Some(&t) = transients
            .iter()
            .filter(|&&t| t >= start - 0.06 && t <= start + 0.03 && t < end)
            .min_by(|a, b| (*a - start).abs().total_cmp(&(*b - start).abs()))
        {
            start = t;
        }
        if let Some(prev) = notes.last_mut() {
            prev.2 = prev.2.min(start);
        }
        // Repeated notes: split at onsets inside the note.
        let mut s = start;
        for &t in transients {
            if t > s + 0.06 && t < end - 0.06 {
                notes.push((p, s, t));
                s = t;
            }
        }
        notes.push((p, s, end));
    }
    notes
        .into_iter()
        .filter(|&(_, s, e)| e - s >= 0.05)
        .map(|(p, s, e)| {
            let a = (s * sr) as usize;
            let b = ((e * sr) as usize).max(a + 1);
            Event {
                pitch: p.clamp(0, 127) as u8,
                level: rms_of(x, a, b - a),
                start: s,
                end: e,
                fixed_length: None,
            }
        })
        .collect()
}

// --------------------------------------------------------------- harmony --

/// A spectral peak: interpolated frequency (Hz) and magnitude.
#[derive(Debug, Clone, Copy)]
struct Peak {
    hz: f64,
    amp: f64,
}

/// Picks local maxima of a magnitude spectrum, refined by parabolic
/// interpolation of the log magnitude.
fn peaks(mag: &[f64], sr: f64, n: usize) -> Vec<Peak> {
    let max = mag.iter().copied().fold(0.0, f64::max);
    if max <= 0.0 {
        return Vec::new();
    }
    let floor = max * 0.005;
    let mut out = Vec::new();
    for k in 2..mag.len().saturating_sub(1) {
        let m = mag[k];
        if m <= floor || m <= mag[k - 1] || m < mag[k + 1] {
            continue;
        }
        let (a, b, c) = (mag[k - 1].max(1e-12).ln(), m.ln(), mag[k + 1].max(1e-12).ln());
        let d = a - 2.0 * b + c;
        let p = if d.abs() > 1e-12 {
            (0.5 * (a - c) / d).clamp(-0.5, 0.5)
        } else {
            0.0
        };
        out.push(Peak {
            hz: (k as f64 + p) * sr / n as f64,
            amp: (b - 0.25 * (a - c) * p).exp(),
        });
    }
    out
}

/// Pitches (36..=96) in a magnitude spectrum, strongest first: a harmonic-sum
/// salience picks one pitch at a time and cancels its partials before the
/// next is picked, which keeps overtones from being reported as notes.
fn pick_pitches(mag: &[f64], sr: f64, n: usize) -> Vec<u8> {
    const HARMONICS: usize = 10;
    let tol = 2f64.powf(0.5 / 12.0);
    let mut peaks = peaks(mag, sr, n);
    let max_amp = peaks.iter().map(|p| p.amp).fold(0.0, f64::max);
    if max_amp <= 0.0 {
        return Vec::new();
    }
    // Index of the strongest peak within half a semitone of `hz`.
    let find = |peaks: &[Peak], hz: f64| -> Option<usize> {
        peaks
            .iter()
            .enumerate()
            .filter(|(_, p)| p.amp > 0.0 && p.hz > hz / tol && p.hz < hz * tol)
            .max_by(|a, b| a.1.amp.total_cmp(&b.1.amp))
            .map(|(i, _)| i)
    };
    let nyquist = sr * 0.45;
    let mut out = Vec::new();
    let mut first = 0.0;
    while out.len() < MAX_VOICES {
        let mut best: Option<(u8, f64)> = None;
        for pitch in 36u8..=96 {
            if out.contains(&pitch) {
                continue;
            }
            let f0 = midi_to_hz(pitch as f64);
            let Some(fund) = find(&peaks, f0) else { continue };
            if peaks[fund].amp < max_amp * 0.05 {
                continue;
            }
            let mut salience = 0.0;
            let mut weight = 1.0;
            for h in 1..=HARMONICS {
                let hz = f0 * h as f64;
                if hz > nyquist {
                    break;
                }
                if let Some(i) = find(&peaks, hz) {
                    salience += weight * peaks[i].amp;
                }
                weight *= 0.85;
            }
            if best.is_none_or(|(_, s)| salience > s) {
                best = Some((pitch, salience));
            }
        }
        let Some((pitch, salience)) = best else { break };
        if out.is_empty() {
            first = salience;
        } else if salience < first * 0.2 {
            break;
        }
        out.push(pitch);
        // Cancel this pitch's partials, assuming a 1/h spectral envelope.
        let f0 = midi_to_hz(pitch as f64);
        let Some(fund) = find(&peaks, f0) else { break };
        let a1 = peaks[fund].amp;
        peaks[fund].amp = 0.0;
        for h in 2..=HARMONICS {
            let hz = f0 * h as f64;
            if hz > nyquist {
                break;
            }
            if let Some(i) = find(&peaks, hz) {
                peaks[i].amp = (peaks[i].amp - 1.5 * a1 / h as f64).max(0.0);
            }
        }
    }
    out
}

/// Average magnitude spectrum over `x[start..end]` (up to 8 frames).
fn averaged_spectrum(x: &[f32], start: usize, end: usize, n: usize) -> Vec<f64> {
    let len = end.saturating_sub(start);
    if len <= n {
        return magnitude(x, start, len, n);
    }
    let frames = ((len - n) / (n / 2) + 1).clamp(1, 8);
    let span = len - n;
    let mut acc = vec![0.0; n / 2];
    for i in 0..frames {
        let s = if frames == 1 {
            start
        } else {
            start + span * i / (frames - 1)
        };
        for (a, m) in acc.iter_mut().zip(magnitude(x, s, n, n)) {
            *a += m / frames as f64;
        }
    }
    acc
}

fn harmony(x: &[f32], sr: f64, transients: &[f64]) -> Vec<Event> {
    let hop = ((sr * HOP_SECS).round() as usize).max(1);
    let (rms, gate) = levels(x, hop);
    if rms.iter().all(|&r| r < ABS_GATE) {
        return Vec::new();
    }
    let n = ((sr * 0.093) as usize).next_power_of_two().max(1024);
    let dur = x.len() as f64 / sr;
    let mut bounds = vec![0.0];
    for &t in transients {
        if t > bounds[bounds.len() - 1] + 0.03 && t < dur {
            bounds.push(t);
        }
    }
    bounds.push(dur);
    let mut out = Vec::new();
    for seg in bounds.windows(2) {
        let (s0, s1) = ((seg[0] * sr) as usize, ((seg[1] * sr) as usize).min(x.len()));
        let (f0, f1) = (s0 / hop, s1.div_ceil(hop).min(rms.len()));
        let Some(first) = (f0..f1).find(|&f| rms[f] >= gate) else {
            continue;
        };
        let last = (f0..f1).rev().find(|&f| rms[f] >= gate).unwrap_or(first);
        let start = (first * hop).max(s0);
        let end = ((last + 1) * hop).min(s1);
        if end <= start + hop {
            continue;
        }
        // Skip the attack when there is room for it.
        let skip = (0.02 * sr) as usize;
        let from = if end - start > n + skip { start + skip } else { start };
        let mag = averaged_spectrum(x, from, end, n);
        let level = rms_of(x, start, end - start);
        for pitch in pick_pitches(&mag, sr, n) {
            out.push(Event {
                pitch,
                level,
                start: start as f64 / sr,
                end: end as f64 / sr,
                fixed_length: None,
            });
        }
    }
    out
}

// ----------------------------------------------------------------- drums --

/// Classifies a drum hit from its spectrum right after the onset.
fn classify_hit(mag: &[f64], sr: f64, n: usize) -> u8 {
    let (mut low, mut mid, mut upper, mut high) = (0.0, 0.0, 0.0, 0.0);
    for (k, m) in mag.iter().enumerate().skip(1) {
        let hz = k as f64 * sr / n as f64;
        let e = m * m;
        if hz < 150.0 {
            low += e;
        } else if hz < 2000.0 {
            mid += e;
        } else if hz < 5000.0 {
            upper += e;
        } else {
            high += e;
        }
    }
    let total = low + mid + upper + high;
    if total <= 0.0 {
        return SNARE;
    }
    if low / total >= 0.4 && low >= mid {
        KICK
    } else if high / total >= 0.5 && mid / total < 0.25 {
        CLOSED_HAT
    } else {
        SNARE
    }
}

fn drums(x: &[f32], sr: f64, transients: &[f64]) -> Vec<Event> {
    let n = ((sr * 0.046) as usize).next_power_of_two().max(512);
    let attack = (0.03 * sr) as usize;
    let pre = (0.002 * sr) as usize;
    let mut hits = Vec::new();
    for &t in transients {
        let s = (t * sr) as usize;
        let end = (s + attack).min(x.len());
        let peak = x.get(s..end).unwrap_or(&[]).iter().fold(0.0f32, |m, v| m.max(v.abs()));
        if peak < ABS_GATE * 4.0 {
            continue;
        }
        let mag = magnitude(x, s.saturating_sub(pre), n, n);
        hits.push((t, peak, classify_hit(&mag, sr, n)));
    }
    let loudest = hits.iter().map(|h| h.1).fold(0.0f32, f32::max);
    hits.into_iter()
        .filter(|h| h.1 >= loudest * REL_GATE)
        .map(|(t, peak, pitch)| Event {
            pitch,
            level: peak,
            start: t,
            end: t,
            fixed_length: Some(DRUM_LENGTH),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44100;

    fn audio(v: Vec<f32>) -> AudioData {
        AudioData {
            sample_rate: SR,
            left: v.clone(),
            right: v,
        }
    }

    /// 120 BPM.
    fn beats(secs: f64) -> f64 {
        secs * 2.0
    }

    fn secs(beats: f64) -> f64 {
        beats / 2.0
    }

    /// Deterministic noise in -1..1.
    struct Lcg(u32);

    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
            (self.0 >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        }
    }

    /// Adds a tone with `partials` harmonics (amplitude 1/2^h) to `v`.
    fn add_tone(v: &mut [f32], pitch: f64, start: f64, len: f64, amp: f32, partials: usize) {
        let hz = midi_to_hz(pitch);
        let s = (start * SR as f64) as usize;
        let n = (len * SR as f64) as usize;
        let fade = (0.003 * SR as f64) as usize;
        for i in 0..n {
            let Some(out) = v.get_mut(s + i) else { break };
            let env = (i.min(n - i) as f32 / fade as f32).min(1.0);
            let t = i as f64 / SR as f64;
            let mut y = 0.0;
            for h in 0..partials {
                let a = 0.5f64.powi(h as i32);
                y += a * (2.0 * std::f64::consts::PI * hz * (h + 1) as f64 * t).sin();
            }
            *out += amp * env * y as f32;
        }
    }

    fn assert_sorted_non_negative(notes: &[Note]) {
        for w in notes.windows(2) {
            assert!(w[0].start <= w[1].start, "{notes:?}");
        }
        for n in notes {
            assert!(n.start >= 0.0 && n.length > 0.0, "{n:?}");
            assert!((1..=127).contains(&n.velocity), "{n:?}");
        }
    }

    #[test]
    fn fft_matches_a_known_spectrum() {
        let n = 64;
        let mut re: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * 5.0 * i as f64 / n as f64).cos())
            .collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        assert!((re[5] - 32.0).abs() < 1e-9 && (re[59] - 32.0).abs() < 1e-9);
        assert!(re[3].abs() < 1e-9 && im[5].abs() < 1e-9);
        fft(&mut re, &mut im, true);
        assert!((re[1] / n as f64 - (2.0 * std::f64::consts::PI * 5.0 / 64.0).cos()).abs() < 1e-9);
    }

    #[test]
    fn melody_finds_the_notes() {
        let mut v = vec![0.0; (1.0 * SR as f64) as usize];
        for (i, p) in [60.0, 64.0, 67.0].iter().enumerate() {
            add_tone(&mut v, *p, i as f64 * 0.25, 0.22, 0.5, 1);
        }
        let notes = convert(&audio(v), ConvertMode::Melody, &beats);
        assert_sorted_non_negative(&notes);
        let pitches: Vec<u8> = notes.iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, vec![60, 64, 67], "{notes:?}");
        for (n, want) in notes.iter().zip([0.0, 0.5, 1.0]) {
            assert!((n.start - want).abs() < 0.06, "{notes:?}");
            assert!((n.length - 0.44).abs() < 0.1, "{notes:?}");
        }
        assert!(notes.iter().all(|n| n.velocity >= 110), "{notes:?}");
    }

    #[test]
    fn melody_velocity_follows_level() {
        let mut v = vec![0.0; (0.6 * SR as f64) as usize];
        add_tone(&mut v, 57.0, 0.0, 0.25, 0.5, 2);
        add_tone(&mut v, 57.0, 0.3, 0.25, 0.1, 2);
        let notes = convert(&audio(v), ConvertMode::Melody, &beats);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes.iter().all(|n| n.pitch == 57));
        assert!(
            notes[0].velocity >= 110 && notes[1].velocity < notes[0].velocity - 15,
            "{notes:?}"
        );
    }

    #[test]
    fn harmony_finds_a_triad() {
        let mut v = vec![0.0; (1.0 * SR as f64) as usize];
        for p in [60.0, 64.0, 67.0] {
            add_tone(&mut v, p, 0.0, 0.9, 0.15, 4);
        }
        let notes = convert(&audio(v), ConvertMode::Harmony, &beats);
        assert_sorted_non_negative(&notes);
        let pitches: Vec<u8> = notes.iter().map(|n| n.pitch).collect();
        for p in [60, 64, 67] {
            assert!(pitches.contains(&p), "{notes:?}");
        }
        assert!(notes.len() <= MAX_VOICES, "{notes:?}");
        assert_eq!(notes.len(), 3, "overtones reported as notes: {notes:?}");
        for n in &notes {
            assert!(n.start < 0.05 && (n.length - 1.8).abs() < 0.1, "{n:?}");
        }
    }

    #[test]
    fn harmony_follows_chord_changes() {
        let mut v = vec![0.0; (1.2 * SR as f64) as usize];
        for p in [60.0, 64.0, 67.0] {
            add_tone(&mut v, p, 0.0, 0.5, 0.15, 3);
        }
        for p in [57.0, 60.0, 64.0] {
            add_tone(&mut v, p, 0.6, 0.5, 0.15, 3);
        }
        let notes = convert(&audio(v), ConvertMode::Harmony, &beats);
        let first: Vec<u8> = notes.iter().filter(|n| n.start < 0.5).map(|n| n.pitch).collect();
        let second: Vec<u8> = notes.iter().filter(|n| n.start > 1.0).map(|n| n.pitch).collect();
        for p in [60, 64, 67] {
            assert!(first.contains(&p), "{notes:?}");
        }
        for p in [57, 60, 64] {
            assert!(second.contains(&p), "{notes:?}");
        }
    }

    #[test]
    fn drums_are_classified() {
        let mut v = vec![0.0f32; (1.5 * SR as f64) as usize];
        let mut rng = Lcg(1);
        let sr = SR as f64;
        // Kick at 0.0 s.
        for (i, out) in v.iter_mut().enumerate().take((0.3 * sr) as usize) {
            let t = i as f64 / sr;
            *out += (0.9 * (-t / 0.08).exp() * (2.0 * std::f64::consts::PI * 55.0 * t).sin()) as f32;
        }
        // Snare at 0.5 s.
        let s = (0.5 * sr) as usize;
        for i in 0..(0.2 * sr) as usize {
            let t = i as f64 / sr;
            let env = (-t / 0.05).exp() as f32;
            let body = (2.0 * std::f64::consts::PI * 200.0 * t).sin() as f32;
            v[s + i] += env * (0.4 * rng.next() + 0.5 * body);
        }
        // Closed hat at 1.0 s: twice-differenced noise.
        let s = (1.0 * sr) as usize;
        let (mut p1, mut p2) = (0.0f32, 0.0f32);
        for i in 0..(0.08 * sr) as usize {
            let n = rng.next();
            let d1 = n - p1;
            let d2 = d1 - p2;
            p1 = n;
            p2 = d1;
            let env = (-(i as f32) / (0.015 * sr as f32)).exp();
            v[s + i] += 0.2 * env * d2;
        }
        let notes = convert(&audio(v), ConvertMode::Drums, &beats);
        assert_sorted_non_negative(&notes);
        let pitches: Vec<u8> = notes.iter().map(|n| n.pitch).collect();
        assert_eq!(pitches, vec![KICK, SNARE, CLOSED_HAT], "{notes:?}");
        for (n, want) in notes.iter().zip([0.0, 1.0, 2.0]) {
            assert!((n.start - want).abs() <= 0.05, "{notes:?}");
            assert_eq!(n.length, DRUM_LENGTH);
        }
    }

    #[test]
    fn silence_gives_no_notes() {
        let data = audio(vec![0.0; SR as usize]);
        for mode in [ConvertMode::Melody, ConvertMode::Harmony, ConvertMode::Drums] {
            assert!(convert(&data, mode, &beats).is_empty(), "{mode:?}");
        }
        let empty = audio(Vec::new());
        assert!(convert(&empty, ConvertMode::Melody, &beats).is_empty());
        assert_eq!(slice_points(&empty, None, 16, &beats, &secs), vec![0.0]);
    }

    #[test]
    fn negative_positions_are_clipped() {
        let mut v = vec![0.0; (0.6 * SR as f64) as usize];
        add_tone(&mut v, 60.0, 0.0, 0.25, 0.5, 1);
        add_tone(&mut v, 62.0, 0.3, 0.25, 0.5, 1);
        // The clip starts 0.3 s into the file.
        let notes = convert(&audio(v), ConvertMode::Melody, &|s| beats(s - 0.3));
        assert_sorted_non_negative(&notes);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert_eq!(notes[0].pitch, 62);
    }

    #[test]
    fn slices_by_division() {
        let data = audio(vec![0.0; 2 * SR as usize]);
        let p = slice_points(&data, Some(1.0), 64, &beats, &secs);
        assert_eq!(p, vec![0.0, 0.5, 1.0, 1.5]);
        let p = slice_points(&data, Some(0.5), 3, &beats, &secs);
        assert_eq!(p, vec![0.0, 0.25, 0.5]);
        assert_eq!(slice_points(&data, Some(0.0), 8, &beats, &secs), vec![0.0]);
    }

    #[test]
    fn slices_by_transients() {
        let mut v = vec![0.0f32; 2 * SR as usize];
        let mut rng = Lcg(7);
        for k in 0..4 {
            let s = (k as f64 * 0.5 * SR as f64) as usize;
            for i in 0..(0.05 * SR as f64) as usize {
                let env = (-(i as f32) / (0.01 * SR as f32)).exp();
                v[s + i] = 0.8 * env * rng.next();
            }
        }
        let data = audio(v);
        let p = slice_points(&data, None, 64, &beats, &secs);
        assert_eq!(p.len(), 4, "{p:?}");
        for (t, want) in p.iter().zip([0.0, 0.5, 1.0, 1.5]) {
            assert!((t - want).abs() < 0.01, "{p:?}");
        }
        assert_eq!(p[0], 0.0);
        assert_eq!(slice_points(&data, None, 2, &beats, &secs).len(), 2);
    }
}
