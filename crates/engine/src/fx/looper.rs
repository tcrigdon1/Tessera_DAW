//! Looper: records the input into a 60-second stereo buffer and plays it back
//! as a loop, with overdubbing, reverse, varispeed and tempo following.
//!
//! The *State* parameter (Stop / Record / Play / Overdub) is driven by the
//! panel's transport buttons. A change of state is a request: with
//! quantization it takes effect on the next beat or bar while the song plays,
//! otherwise immediately. The runtime keeps its own state too, because it can
//! move on by itself (a fixed *Record Length* switches to playback when it
//! is reached; *Song Control* starts and stops playback with the transport).
//!
//! *Clear* is a toggle the panel sets to 1 and back to 0; the runtime erases
//! the loop on each 0 -> 1 edge.
//!
//! The runtime publishes what it is actually doing ([`LooperStatus`]) in the
//! slots of its [`scope`] tap, so the panel follows its own transitions.

use std::sync::Arc;

use super::scope::{self, Scope};
use super::{device_params, FxContext, ParamKind, Scale, Unit};
use crate::model::Id;

const LIN: ParamKind = ParamKind::Continuous(Scale::Linear);
const STATES: ParamKind = ParamKind::Choice(&["Stop", "Record", "Play", "Overdub"]);
const QUANT: ParamKind = ParamKind::Choice(&["None", "Beat", "Bar"]);
const SONG: ParamKind = ParamKind::Choice(&["Off", "Start", "Start & Stop"]);
const TEMPO: ParamKind = ParamKind::Choice(&["None", "Follow Song"]);
const LENGTHS: ParamKind = ParamKind::Choice(&["Free", "1 Bar", "2 Bars", "4 Bars", "8 Bars", "16 Bars"]);
const MONITOR: ParamKind = ParamKind::Choice(&["Always", "Never", "While Not Playing"]);

device_params! {
    pub struct LooperParams {
        state: "State", 0.0, 3.0, 0.0, STATES, Unit::Plain;
        quantize: "Quantization", 0.0, 2.0, 2.0, QUANT, Unit::Plain;
        song_control: "Song Control", 0.0, 2.0, 0.0, SONG, Unit::Plain;
        tempo_control: "Tempo Control", 0.0, 1.0, 1.0, TEMPO, Unit::Plain;
        rec_length: "Record Length", 0.0, 5.0, 0.0, LENGTHS, Unit::Plain;
        feedback: "Feedback", 0.0, 1.0, 1.0, LIN, Unit::Percent;
        reverse: "Reverse", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
        speed: "Speed", -36.0, 36.0, 0.0, LIN, Unit::Semitones;
        monitor: "Monitor", 0.0, 2.0, 0.0, MONITOR, Unit::Plain;
        mix: "Dry/Wet", 0.0, 1.0, 0.5, LIN, Unit::Percent;
        clear: "Clear", 0.0, 1.0, 0.0, ParamKind::Toggle, Unit::Plain;
    }
}

/// Parameter indices used by the panel.
pub const STATE: usize = 0;
pub const CLEAR: usize = 10;

pub const STATE_STOP: f32 = 0.0;
pub const STATE_RECORD: f32 = 1.0;
pub const STATE_PLAY: f32 = 2.0;
pub const STATE_OVERDUB: f32 = 3.0;

/// Scope slots of the published [`LooperStatus`].
mod slot {
    pub const STATE: usize = 0;
    pub const PENDING: usize = 1;
    pub const SEEN: usize = 2;
    pub const LENGTH: usize = 3;
    pub const POSITION: usize = 4;
}

/// What the runtime is doing, as last published from the audio thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LooperStatus {
    /// Current state as a `STATE_*` value.
    pub state: f32,
    /// A state change waiting for its quantization point.
    pub pending: Option<f32>,
    /// The State parameter value the runtime last saw.
    pub seen: f32,
    /// Loop length in seconds (the length so far while recording).
    pub length: f32,
    /// Playback position in seconds.
    pub position: f32,
}

impl LooperStatus {
    fn read(s: &Scope) -> Self {
        let pending = s.slots[slot::PENDING].load();
        Self {
            state: s.slots[slot::STATE].load(),
            pending: (pending >= 0.0).then_some(pending),
            seen: s.slots[slot::SEEN].load(),
            length: s.slots[slot::LENGTH].load(),
            position: s.slots[slot::POSITION].load(),
        }
    }

    /// The value the State parameter should be set to when the runtime has
    /// moved on by itself (fixed length reached, Clear, Song Control), so
    /// that the next press of a button is a change again. `None` while the
    /// runtime has not seen `param` yet or a request is pending.
    pub fn resync(&self, param: f32) -> Option<f32> {
        let param = param.round();
        (self.pending.is_none() && self.seen == param && self.state != param).then_some(self.state)
    }
}

/// The live status of a Looper device, if its runtime exists. UI thread.
pub fn status(device: Id) -> Option<LooperStatus> {
    scope::find(device).map(|s| LooperStatus::read(&s))
}

const BUFFER_SECONDS: f32 = 60.0;
const BARS: [f64; 6] = [0.0, 1.0, 2.0, 4.0, 8.0, 16.0];

fn choice(v: f32, n: usize) -> usize {
    (v.round().max(0.0) as usize).min(n - 1)
}

impl super::EffectParams for LooperParams {
    fn presets() -> Vec<(&'static str, Self)> {
        let d = Self::default();
        vec![
            (
                "Live Looping (Bar Sync)",
                Self {
                    quantize: 2.0,
                    tempo_control: 1.0,
                    ..d
                },
            ),
            (
                "Free Loop",
                Self {
                    quantize: 0.0,
                    tempo_control: 0.0,
                    ..d
                },
            ),
            (
                "Fading Overdubs",
                Self {
                    feedback: 0.7,
                    rec_length: 3.0,
                    ..d
                },
            ),
            (
                "Reverse Octave Down",
                Self {
                    reverse: 1.0,
                    speed: -12.0,
                    mix: 0.6,
                    ..d
                },
            ),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Stopped,
    Recording,
    Playing,
    Overdubbing,
}

impl Mode {
    /// The State parameter choice of this mode.
    fn index(self) -> usize {
        match self {
            Mode::Stopped => 0,
            Mode::Recording => 1,
            Mode::Playing => 2,
            Mode::Overdubbing => 3,
        }
    }
}

pub struct Looper {
    sr: f32,
    buf: [Vec<f32>; 2],
    mode: Mode,
    /// Loop length in samples (0 = empty).
    len: usize,
    /// Write position while recording.
    rec_pos: usize,
    /// Playback position in samples.
    pos: f64,
    /// Tempo while the loop was recorded (for Follow Song).
    rec_tempo: f64,
    /// Last State parameter seen, to detect presses.
    last_state: Option<usize>,
    /// A state change waiting for its quantization point.
    pending: Option<usize>,
    last_clear: Option<bool>,
    was_playing: bool,
    wet_gain: f32,
    dry_gain: f32,
    scope: Option<Arc<Scope>>,
}

impl Looper {
    pub fn new(sample_rate: f32) -> Self {
        let n = (BUFFER_SECONDS * sample_rate) as usize + 4;
        Self {
            sr: sample_rate,
            buf: [vec![0.0; n], vec![0.0; n]],
            mode: Mode::Stopped,
            len: 0,
            rec_pos: 0,
            pos: 0.0,
            rec_tempo: 120.0,
            last_state: None,
            pending: None,
            last_clear: None,
            was_playing: false,
            wet_gain: 0.0,
            dry_gain: 1.0,
            scope: None,
        }
    }

    /// Stops playback; the recorded loop is kept.
    pub fn reset(&mut self) {
        if self.mode == Mode::Recording {
            self.len = 0;
        }
        self.mode = Mode::Stopped;
        self.pending = None;
        self.last_state = None;
        self.pos = 0.0;
        self.wet_gain = 0.0;
        self.publish();
    }

    /// Stores the current status in the scope slots. Wait-free.
    fn publish(&self) {
        let Some(s) = &self.scope else { return };
        let (len, pos) = match self.mode {
            Mode::Recording => (self.rec_pos, self.rec_pos as f64),
            Mode::Stopped => (self.len, 0.0),
            _ => (self.len, self.pos),
        };
        s.slots[slot::STATE].store(self.mode.index() as f32);
        s.slots[slot::PENDING].store(self.pending.map_or(-1.0, |p| p as f32));
        s.slots[slot::SEEN].store(self.last_state.map_or(-1.0, |p| p as f32));
        s.slots[slot::LENGTH].store(len as f32 / self.sr);
        s.slots[slot::POSITION].store((pos / self.sr as f64) as f32);
    }

    fn capacity(&self) -> usize {
        self.buf[0].len() - 4
    }

    fn bar_len(ctx: &FxContext, sr: f32) -> f64 {
        60.0 / ctx.tempo.clamp(1.0, 999.0) * sr as f64 * ctx.beats_per_bar.max(1) as f64
    }

    /// Ends a recording: snaps the length to whole bars when following the
    /// song tempo and smooths the loop seam.
    fn finish_recording(&mut self, p: &LooperParams, ctx: &FxContext) {
        let mut len = self.rec_pos;
        if choice(p.tempo_control, 2) == 1 && choice(p.rec_length, 6) == 0 && len > 0 {
            let bar = Self::bar_len(ctx, self.sr);
            let bars = (len as f64 / bar).round().max(1.0);
            let snapped = ((bars * bar) as usize).min(self.capacity());
            for c in 0..2 {
                for v in &mut self.buf[c][len.min(snapped)..snapped] {
                    *v = 0.0;
                }
            }
            len = snapped;
        }
        self.len = len;
        if len > 0 {
            let fade = ((0.002 * self.sr) as usize).min(len / 8);
            for c in 0..2 {
                for k in 0..fade {
                    let g = k as f32 / fade as f32;
                    self.buf[c][k] *= g;
                    self.buf[c][len - 1 - k] *= g;
                }
            }
        }
        self.pos = 0.0;
    }

    fn start_recording(&mut self, ctx: &FxContext) {
        self.mode = Mode::Recording;
        self.rec_pos = 0;
        self.len = 0;
        self.rec_tempo = ctx.tempo.clamp(1.0, 999.0);
    }

    fn apply(&mut self, target: usize, p: &LooperParams, ctx: &FxContext) {
        let was_rec = self.mode == Mode::Recording;
        match target {
            1 => {
                if !was_rec {
                    self.start_recording(ctx);
                }
            }
            2 | 3 => {
                if was_rec {
                    self.finish_recording(p, ctx);
                } else if self.mode == Mode::Stopped {
                    self.pos = 0.0;
                }
                if self.len == 0 {
                    // Overdubbing an empty looper records a new loop.
                    if target == 3 {
                        self.start_recording(ctx);
                    } else {
                        self.mode = Mode::Stopped;
                    }
                } else {
                    self.mode = if target == 2 { Mode::Playing } else { Mode::Overdubbing };
                }
            }
            _ => {
                if was_rec {
                    self.finish_recording(p, ctx);
                }
                self.mode = Mode::Stopped;
                self.pos = 0.0;
            }
        }
    }

    pub fn process(&mut self, p: &LooperParams, left: &mut [f32], right: &mut [f32], ctx: &FxContext) {
        // Clear on a rising edge of the Clear toggle.
        let clear = p.clear >= 0.5;
        if self.last_clear == Some(false) && clear {
            self.len = 0;
            self.rec_pos = 0;
            self.mode = Mode::Stopped;
            self.pending = None;
        }
        self.last_clear = Some(clear);

        // State presses.
        let state = choice(p.state, 4);
        match self.last_state {
            None => {
                // First block after creation or a reset: a saved Record
                // request is not replayed, Play and Overdub resume the loop.
                self.last_state = Some(state);
                if state >= 2 {
                    self.pending = Some(state);
                }
            }
            Some(s) if s != state => {
                self.last_state = Some(state);
                // Asking for the current state cancels a pending change.
                self.pending = (state != self.mode.index()).then_some(state);
            }
            _ => {}
        }

        // Song control follows the transport.
        let song = choice(p.song_control, 3);
        if ctx.playing && !self.was_playing && song >= 1 && self.len > 0 && self.mode == Mode::Stopped {
            self.mode = Mode::Playing;
            self.pos = 0.0;
        }
        if !ctx.playing && self.was_playing && song == 2 && self.mode != Mode::Recording {
            self.mode = Mode::Stopped;
            self.pos = 0.0;
        }
        self.was_playing = ctx.playing;

        let quant = if ctx.playing { choice(p.quantize, 3) } else { 0 };
        let spb = 60.0 / ctx.tempo.clamp(1.0, 999.0) * self.sr as f64;
        let q_beats = match quant {
            1 => 1.0,
            2 => ctx.beats_per_bar.max(1) as f64,
            _ => 0.0,
        };
        let fixed_bars = BARS[choice(p.rec_length, 6)];
        let fixed_len = if fixed_bars > 0.0 {
            ((fixed_bars * Self::bar_len(ctx, self.sr)) as usize).min(self.capacity())
        } else {
            self.capacity()
        };
        let mut rate = 2f64.powf(p.speed.clamp(-36.0, 36.0) as f64 / 12.0);
        if choice(p.tempo_control, 2) == 1 {
            rate *= ctx.tempo.clamp(1.0, 999.0) / self.rec_tempo;
        }
        if p.reverse >= 0.5 {
            rate = -rate;
        }
        let feedback = p.feedback.clamp(0.0, 1.0);
        let mix = p.mix.clamp(0.0, 1.0);
        let wet_level = (2.0 * mix).min(1.0);
        let dry_level = (2.0 * (1.0 - mix)).min(1.0);
        let monitor = choice(p.monitor, 3);
        let coef = super::smoothing(0.003, self.sr);

        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            if let Some(target) = self.pending {
                let due = if q_beats > 0.0 {
                    // This sample is the first one at or after a quantization point.
                    let b = ctx.song_beat + i as f64 / spb;
                    let into = b.rem_euclid(q_beats);
                    into < 1.0 / spb
                } else {
                    true
                };
                if due {
                    self.pending = None;
                    self.apply(target, p, ctx);
                }
            }

            let x = [*l, *r];
            let mut wet = [0.0f32; 2];
            match self.mode {
                Mode::Stopped => {}
                Mode::Recording => {
                    for c in 0..2 {
                        self.buf[c][self.rec_pos] = x[c];
                    }
                    self.rec_pos += 1;
                    if self.rec_pos >= fixed_len {
                        self.finish_recording(p, ctx);
                        self.mode = Mode::Playing;
                    }
                }
                Mode::Playing | Mode::Overdubbing => {
                    let len = self.len.max(1);
                    let i0 = (self.pos.floor() as usize).min(len - 1);
                    let i1 = if i0 + 1 >= len { 0 } else { i0 + 1 };
                    let fr = (self.pos - i0 as f64) as f32;
                    for c in 0..2 {
                        let a = self.buf[c][i0];
                        let b = self.buf[c][i1];
                        wet[c] = a + (b - a) * fr;
                    }
                    if self.mode == Mode::Overdubbing {
                        let w = if fr < 0.5 { i0 } else { i1 };
                        for c in 0..2 {
                            let v = self.buf[c][w] * feedback + x[c];
                            // Keep runaway overdubs bounded.
                            self.buf[c][w] = v.clamp(-8.0, 8.0);
                        }
                    }
                    self.pos += rate;
                    let lenf = len as f64;
                    if self.pos >= lenf {
                        self.pos %= lenf;
                    } else if self.pos < 0.0 {
                        self.pos = (self.pos % lenf + lenf) % lenf;
                    }
                }
            }

            let playing = matches!(self.mode, Mode::Playing | Mode::Overdubbing);
            let wet_t = if playing { wet_level } else { 0.0 };
            let mon = match monitor {
                0 => true,
                1 => false,
                _ => !playing,
            };
            let dry_t = if mon { dry_level } else { 0.0 };
            self.wet_gain = wet_t + (self.wet_gain - wet_t) * coef;
            self.dry_gain = dry_t + (self.dry_gain - dry_t) * coef;
            *l = x[0] * self.dry_gain + wet[0] * self.wet_gain;
            *r = x[1] * self.dry_gain + wet[1] * self.wet_gain;
        }
        self.publish();
    }
}

impl super::EffectRuntime for Looper {
    fn attach(&mut self, device: Id) {
        let s = scope::tap(device);
        s.set_sample_rate(self.sr);
        self.scope = Some(s);
        self.publish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fx::EffectRuntime;

    const SR: f32 = 48000.0;

    fn sine(n: usize, f: f32) -> Vec<f32> {
        (0..n)
            .map(|i| (i as f32 * f * std::f32::consts::TAU / SR).sin() * 0.5)
            .collect()
    }

    fn block(lp: &mut Looper, p: &LooperParams, input: &[f32], ctx: &FxContext) -> Vec<f32> {
        let mut l = input.to_vec();
        let mut r = input.to_vec();
        for (a, b) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            lp.process(p, a, b, ctx);
        }
        assert!(l.iter().all(|x| x.is_finite()));
        l
    }

    fn free() -> LooperParams {
        LooperParams {
            quantize: 0.0,
            tempo_control: 0.0,
            monitor: 1.0,
            mix: 1.0,
            ..Default::default()
        }
    }

    #[test]
    fn records_then_plays_back_the_loop() {
        let ctx = FxContext::new(SR, 120.0);
        let mut lp = Looper::new(SR);
        let mut p = free();
        block(&mut lp, &p, &[0.0; 9600], &ctx);
        let n = 12000;
        let rec = sine(n, 440.0);
        p.state = STATE_RECORD;
        let out = block(&mut lp, &p, &rec, &ctx);
        // Monitoring off and nothing playing: silence while recording.
        assert!(out.iter().all(|x| x.abs() < 1e-6));
        p.state = STATE_PLAY;
        let out = block(&mut lp, &p, &vec![0.0; 3 * n], &ctx);
        let fade = 96;
        for k in (2000..3 * n).step_by(13) {
            let j = k % n;
            if j < fade || j >= n - fade {
                continue;
            }
            assert!((out[k] - rec[j]).abs() < 1e-3, "{k}: {} vs {}", out[k], rec[j]);
        }
        // Stop silences it.
        p.state = STATE_STOP;
        let out = block(&mut lp, &p, &vec![0.0; 4096], &ctx);
        assert!(out[1000..].iter().all(|x| x.abs() < 1e-4));
    }

    #[test]
    fn overdub_sums_layers() {
        let ctx = FxContext::new(SR, 120.0);
        let mut lp = Looper::new(SR);
        let mut p = free();
        block(&mut lp, &p, &[0.0; 512], &ctx);
        let n = 9600;
        let a = sine(n, 300.0);
        p.state = STATE_RECORD;
        block(&mut lp, &p, &a, &ctx);
        p.state = STATE_OVERDUB;
        block(&mut lp, &p, &a, &ctx);
        p.state = STATE_PLAY;
        let out = block(&mut lp, &p, &vec![0.0; 2 * n], &ctx);
        for k in (1000..2 * n).step_by(17) {
            let j = k % n;
            if !(200..n - 200).contains(&j) {
                continue;
            }
            assert!((out[k] - 2.0 * a[j]).abs() < 2e-3, "{k}: {} vs {}", out[k], 2.0 * a[j]);
        }
    }

    #[test]
    fn fixed_length_and_clear() {
        let ctx = FxContext::new(SR, 120.0);
        let mut lp = Looper::new(SR);
        let mut p = LooperParams {
            rec_length: 1.0,
            ..free()
        };
        block(&mut lp, &p, &[0.0; 512], &ctx);
        p.state = STATE_RECORD;
        // One bar at 120 BPM = 96000 samples; record a bit longer.
        let input = sine(100000, 500.0);
        let out = block(&mut lp, &p, &input, &ctx);
        assert_eq!(lp.len, 96000);
        assert_eq!(lp.mode, Mode::Playing);
        assert!(out[99000..].iter().any(|x| x.abs() > 0.1));
        p.clear = 1.0;
        let out = block(&mut lp, &p, &vec![0.0; 4096], &ctx);
        assert_eq!(lp.len, 0);
        assert!(out[1000..].iter().all(|x| x.abs() < 1e-4));
    }

    #[test]
    fn quantized_start_waits_for_the_bar() {
        let mut lp = Looper::new(SR);
        let mut p = LooperParams {
            quantize: 2.0,
            tempo_control: 0.0,
            ..free()
        };
        let spb = 24000.0;
        let mut pos = 0usize;
        let step = |lp: &mut Looper, p: &LooperParams, pos: &mut usize| {
            let mut l = vec![0.25f32; 512];
            let mut r = l.clone();
            let ctx = FxContext::new(SR, 120.0).with_transport(true, *pos as f64 / spb, 4, false);
            lp.process(p, &mut l, &mut r, &ctx);
            *pos += 512;
        };
        step(&mut lp, &p, &mut pos);
        p.state = STATE_RECORD;
        while pos < 96000 - 512 {
            step(&mut lp, &p, &mut pos);
            assert_eq!(lp.mode, Mode::Stopped);
        }
        step(&mut lp, &p, &mut pos);
        step(&mut lp, &p, &mut pos);
        assert_eq!(lp.mode, Mode::Recording);
        assert!(lp.rec_pos > 0 && lp.rec_pos <= 1024);
    }

    #[test]
    fn speed_and_reverse_stay_bounded() {
        let ctx = FxContext::new(SR, 120.0);
        let mut lp = Looper::new(SR);
        let mut p = LooperParams {
            speed: 36.0,
            reverse: 1.0,
            tempo_control: 1.0,
            ..free()
        };
        block(&mut lp, &p, &[0.0; 1], &ctx);
        p.state = STATE_RECORD;
        block(&mut lp, &p, &sine(300, 1000.0), &ctx);
        p.state = STATE_OVERDUB;
        for _ in 0..20 {
            let out = block(&mut lp, &p, &vec![1.0; 4800], &ctx);
            assert!(out.iter().all(|x| x.abs() <= 8.5));
        }
        p.speed = -36.0;
        let out = block(&mut lp, &p, &[0.0; 1], &ctx);
        assert!(out[0].is_finite());
    }

    #[test]
    fn status_follows_the_runtime_not_the_parameter() {
        let id: Id = 0x100b_0001;
        let ctx = FxContext::new(SR, 120.0);
        let mut lp = Looper::new(SR);
        lp.attach(id);
        let mut p = LooperParams {
            rec_length: 1.0,
            ..free()
        };
        block(&mut lp, &p, &[0.0; 512], &ctx);
        p.state = STATE_RECORD;
        block(&mut lp, &p, &sine(48000, 500.0), &ctx);
        let st = status(id).unwrap();
        assert_eq!(st.state, STATE_RECORD);
        assert!((st.length - 1.0).abs() < 0.02, "{}", st.length);
        assert_eq!(st.resync(p.state), None);
        // The fixed length (one bar, 2 s) switches to playback by itself.
        block(&mut lp, &p, &sine(60000, 500.0), &ctx);
        let st = status(id).unwrap();
        assert_eq!(st.state, STATE_PLAY);
        assert!((st.length - 2.0).abs() < 1e-3);
        assert!((st.position - 0.25).abs() < 0.02, "{}", st.position);
        // The panel resyncs the stale Record parameter; that is not a press.
        assert_eq!(st.resync(p.state), Some(STATE_PLAY));
        p.state = STATE_PLAY;
        block(&mut lp, &p, &[0.0; 512], &ctx);
        assert_eq!(lp.mode, Mode::Playing);
        // So pressing Record again records a new loop.
        p.state = STATE_RECORD;
        block(&mut lp, &p, &[0.0; 512], &ctx);
        assert_eq!(status(id).unwrap().state, STATE_RECORD);
        // Clear stops the looper whatever the parameter says.
        p.state = STATE_PLAY;
        block(&mut lp, &p, &sine(4800, 500.0), &ctx);
        p.clear = 1.0;
        block(&mut lp, &p, &[0.0; 512], &ctx);
        let st = status(id).unwrap();
        assert_eq!((st.state, st.length), (STATE_STOP, 0.0));
        assert_eq!(st.resync(p.state), Some(STATE_STOP));
    }

    #[test]
    fn pending_requests_are_published_and_block_resync() {
        let id: Id = 0x100b_0002;
        let mut lp = Looper::new(SR);
        lp.attach(id);
        let mut p = LooperParams {
            quantize: 2.0,
            ..free()
        };
        let ctx = FxContext::new(SR, 120.0).with_transport(true, 0.5, 4, false);
        block(&mut lp, &p, &[0.0; 512], &ctx);
        p.state = STATE_RECORD;
        block(&mut lp, &p, &[0.0; 512], &ctx);
        let st = status(id).unwrap();
        assert_eq!(
            (st.state, st.pending, st.seen),
            (STATE_STOP, Some(STATE_RECORD), STATE_RECORD)
        );
        assert_eq!(st.resync(p.state), None);
        // Pressing the current state again cancels the request.
        p.state = STATE_STOP;
        block(&mut lp, &p, &[0.0; 512], &ctx);
        assert_eq!(status(id).unwrap().pending, None);
        // A parameter the runtime has not seen yet is never resynced.
        assert_eq!(status(id).unwrap().resync(STATE_PLAY), None);
    }
}
