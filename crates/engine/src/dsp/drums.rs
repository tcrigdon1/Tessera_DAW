//! "Drum Kit": a synthesized drum machine mapped to General MIDI drum notes.

use serde::{Deserialize, Serialize};

use super::envelope::exp_coef;
use super::filter::Svf;
use super::Rng;
use crate::model::db_to_gain;

pub const KICK: u8 = 36;
pub const RIM: u8 = 37;
pub const SNARE: u8 = 38;
pub const CLAP: u8 = 39;
pub const CLOSED_HAT: u8 = 42;
pub const LOW_TOM: u8 = 45;
pub const OPEN_HAT: u8 = 46;
pub const MID_TOM: u8 = 47;
pub const CRASH: u8 = 49;
pub const HIGH_TOM: u8 = 50;

/// Pads shown in the drum editor, low to high.
pub const PADS: [(u8, &str); 10] = [
    (KICK, "Kick"),
    (RIM, "Rim"),
    (SNARE, "Snare"),
    (CLAP, "Clap"),
    (CLOSED_HAT, "Closed Hat"),
    (LOW_TOM, "Low Tom"),
    (OPEN_HAT, "Open Hat"),
    (MID_TOM, "Mid Tom"),
    (CRASH, "Crash"),
    (HIGH_TOM, "High Tom"),
];

pub fn pad_name(note: u8) -> Option<&'static str> {
    PADS.iter().find(|(n, _)| *n == note).map(|(_, s)| *s)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DrumKitParams {
    pub kick_tune: f32,
    pub kick_decay: f32,
    pub snare_tone: f32,
    pub snare_decay: f32,
    pub hat_decay: f32,
    pub volume_db: f32,
}

impl Default for DrumKitParams {
    fn default() -> Self {
        Self {
            kick_tune: 50.0,
            kick_decay: 0.45,
            snare_tone: 190.0,
            snare_decay: 0.18,
            hat_decay: 0.05,
            volume_db: -4.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Piece {
    Kick,
    Snare,
    Clap,
    Rim,
    Hat { open: bool },
    Tom(f32),
    Crash,
}

#[derive(Debug, Clone)]
struct Hit {
    active: bool,
    piece: Piece,
    note: u8,
    gain: f32,
    t: f32,
    phase: f32,
    env: f32,
    noise_env: f32,
    filter: Svf,
}

impl Hit {
    fn idle() -> Self {
        Self {
            active: false,
            piece: Piece::Kick,
            note: 0,
            gain: 0.0,
            t: 0.0,
            phase: 0.0,
            env: 0.0,
            noise_env: 0.0,
            filter: Svf::default(),
        }
    }
}

pub struct DrumKit {
    sample_rate: f32,
    hits: [Hit; 16],
    rng: Rng,
    next: usize,
}

impl DrumKit {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            hits: std::array::from_fn(|_| Hit::idle()),
            rng: Rng::new(0x1234_5678),
            next: 0,
        }
    }

    pub fn active_hits(&self) -> usize {
        self.hits.iter().filter(|h| h.active).count()
    }

    pub fn note_on(&mut self, note: u8, velocity: u8) {
        let piece = match note {
            35 | KICK => Piece::Kick,
            RIM => Piece::Rim,
            SNARE | 40 => Piece::Snare,
            CLAP => Piece::Clap,
            CLOSED_HAT | 44 => Piece::Hat { open: false },
            OPEN_HAT => Piece::Hat { open: true },
            41 | LOW_TOM => Piece::Tom(90.0),
            MID_TOM | 48 => Piece::Tom(130.0),
            HIGH_TOM => Piece::Tom(180.0),
            CRASH | 51 | 57 => Piece::Crash,
            _ => return,
        };
        // A closed hat chokes an open one.
        if let Piece::Hat { open: false } = piece {
            for h in &mut self.hits {
                if h.active && h.piece == (Piece::Hat { open: true }) {
                    h.active = false;
                }
            }
        }
        // Retrigger the same note on the same voice, otherwise round-robin.
        let idx = self
            .hits
            .iter()
            .position(|h| h.active && h.note == note)
            .or_else(|| self.hits.iter().position(|h| !h.active))
            .unwrap_or_else(|| {
                self.next = (self.next + 1) % self.hits.len();
                self.next
            });
        let h = &mut self.hits[idx];
        *h = Hit::idle();
        h.active = true;
        h.piece = piece;
        h.note = note;
        let v = velocity as f32 / 127.0;
        h.gain = v * v;
        h.env = 1.0;
        h.noise_env = 1.0;
    }

    pub fn panic(&mut self) {
        for h in &mut self.hits {
            h.active = false;
        }
    }

    pub fn render(&mut self, p: &DrumKitParams, left: &mut [f32], right: &mut [f32]) {
        let sr = self.sample_rate;
        let dt = 1.0 / sr;
        let out = db_to_gain(p.volume_db);
        for h in self.hits.iter_mut() {
            if !h.active {
                continue;
            }
            for i in 0..left.len() {
                let s = match h.piece {
                    Piece::Kick => {
                        let f = p.kick_tune + 110.0 * (-h.t * 28.0).exp();
                        h.phase = (h.phase + f * dt) % 1.0;
                        h.env *= exp_coef(p.kick_decay, sr);
                        let click = self.rng.next_bipolar() * (-h.t * 400.0).exp() * 0.3;
                        ((h.phase * std::f32::consts::TAU).sin() + click) * h.env * 1.2
                    }
                    Piece::Snare => {
                        let f = p.snare_tone + 60.0 * (-h.t * 40.0).exp();
                        h.phase = (h.phase + f * dt) % 1.0;
                        h.env *= exp_coef(p.snare_decay * 0.6, sr);
                        h.noise_env *= exp_coef(p.snare_decay, sr);
                        let n = self.rng.next_bipolar();
                        let hp = h.filter.process(n, 1500.0, 0.7, sr).high;
                        (h.phase * std::f32::consts::TAU).sin() * h.env * 0.5 + hp * h.noise_env * 0.6
                    }
                    Piece::Clap => {
                        // Three quick bursts followed by a tail.
                        let burst = if h.t < 0.03 {
                            ((h.t * 100.0).fract() < 0.5) as i32 as f32
                        } else {
                            1.0
                        };
                        if h.t > 0.03 {
                            h.noise_env *= exp_coef(0.15, sr);
                        }
                        let n = self.rng.next_bipolar();
                        let bp = h.filter.process(n, 1200.0, 2.0, sr).band;
                        bp * h.noise_env * burst * 1.2
                    }
                    Piece::Rim => {
                        h.phase = (h.phase + 1700.0 * dt) % 1.0;
                        h.env *= exp_coef(0.03, sr);
                        (h.phase * std::f32::consts::TAU).sin() * h.env * 0.5
                    }
                    Piece::Hat { open } => {
                        let decay = if open { p.hat_decay * 6.0 } else { p.hat_decay };
                        h.noise_env *= exp_coef(decay, sr);
                        let n = self.rng.next_bipolar();
                        let hp = h.filter.process(n, 7500.0, 0.9, sr).high;
                        hp * h.noise_env * 0.35
                    }
                    Piece::Tom(base) => {
                        let f = base + base * 0.6 * (-h.t * 20.0).exp();
                        h.phase = (h.phase + f * dt) % 1.0;
                        h.env *= exp_coef(0.4, sr);
                        (h.phase * std::f32::consts::TAU).sin() * h.env * 0.8
                    }
                    Piece::Crash => {
                        h.noise_env *= exp_coef(1.4, sr);
                        let n = self.rng.next_bipolar();
                        let hp = h.filter.process(n, 5000.0, 0.6, sr).high;
                        hp * h.noise_env * 0.3
                    }
                };
                let s = s * h.gain * out;
                left[i] += s;
                right[i] += s;
                h.t += dt;
                if h.env < 1e-4 && h.noise_env < 1e-4 || h.t > 4.0 {
                    h.active = false;
                    break;
                }
                // Pieces that use only one envelope: let the unused one decay too.
                match h.piece {
                    Piece::Kick | Piece::Rim | Piece::Tom(_) => h.noise_env = 0.0,
                    Piece::Clap | Piece::Hat { .. } | Piece::Crash => h.env = 0.0,
                    Piece::Snare => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_pad_makes_sound_and_ends() {
        for (note, name) in PADS {
            let mut d = DrumKit::new(48000.0);
            let p = DrumKitParams::default();
            d.note_on(note, 120);
            let mut l = vec![0.0; 2400];
            let mut r = vec![0.0; 2400];
            d.render(&p, &mut l, &mut r);
            let peak = l.iter().fold(0.0f32, |a, b| a.max(b.abs()));
            assert!(peak > 0.01, "{name} peak {peak}");
            for _ in 0..100 {
                l.fill(0.0);
                r.fill(0.0);
                d.render(&p, &mut l, &mut r);
            }
            assert_eq!(d.active_hits(), 0, "{name} never ended");
        }
    }
}
