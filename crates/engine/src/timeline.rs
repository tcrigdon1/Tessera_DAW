//! Song time: tempo automation on the master track and time signature
//! changes in the Arrangement.
//!
//! Positions stay in beats (quarter notes). A [`TimeMap`] answers "what is
//! the tempo here", "which bar is this" and "how many seconds in is this
//! beat"; the engine keeps one in each snapshot and queries it on the audio
//! thread (allocation-free), and the UI uses it for the ruler and export.

use serde::{Deserialize, Serialize};

use crate::automation::Envelope;
use crate::model::Project;

/// The master track's Song Tempo envelope. Values are normalised 0..1 and
/// mapped linearly onto `min..max` BPM, like Ableton's tempo lane range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TempoLane {
    pub envelope: Envelope,
    pub min: f64,
    pub max: f64,
}

impl Default for TempoLane {
    fn default() -> Self {
        Self {
            envelope: Envelope::default(),
            min: 60.0,
            max: 200.0,
        }
    }
}

impl TempoLane {
    pub fn is_active(&self) -> bool {
        !self.envelope.is_empty()
    }

    /// Normalised envelope value for a tempo.
    pub fn normalize(&self, bpm: f64) -> f32 {
        let span = (self.max - self.min).max(1e-9);
        (((bpm - self.min) / span) as f32).clamp(0.0, 1.0)
    }

    /// Tempo for a normalised envelope value.
    pub fn denormalize(&self, n: f32) -> f64 {
        (self.min + (self.max - self.min) * n.clamp(0.0, 1.0) as f64).clamp(20.0, 999.0)
    }
}

/// A time signature change. It always starts on a bar line of the
/// signature before it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SignatureChange {
    /// Position in beats.
    pub beat: f64,
    pub numerator: u32,
    pub denominator: u32,
}

impl SignatureChange {
    /// Bar length in quarter-note beats.
    pub fn bar_beats(&self) -> f64 {
        bar_beats(self.numerator, self.denominator)
    }
}

/// Bar length in quarter-note beats for a signature.
pub fn bar_beats(numerator: u32, denominator: u32) -> f64 {
    numerator.max(1) as f64 * 4.0 / denominator.clamp(1, 32) as f64
}

/// One stretch of constant signature, precomputed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Meter {
    /// Beat where this signature starts.
    pub beat: f64,
    /// Index of the bar (0-based) that starts at `beat`.
    pub bar: i64,
    pub numerator: u32,
    pub denominator: u32,
}

impl Meter {
    pub fn bar_beats(&self) -> f64 {
        bar_beats(self.numerator, self.denominator)
    }
}

/// Tempo and meter over song time.
#[derive(Debug, Clone, PartialEq)]
pub struct TimeMap {
    /// Tempo when there is no automation.
    pub tempo: f64,
    pub lane: Option<TempoLane>,
    /// Signature stretches, first one at beat 0, sorted.
    pub meters: Vec<Meter>,
}

impl Default for TimeMap {
    fn default() -> Self {
        Self::new(120.0, None, 4, 4, &[])
    }
}

impl TimeMap {
    /// Builds a map from a starting signature and a list of changes (any
    /// order). Changes are moved onto the nearest bar line of the signature
    /// before them; duplicates at the same bar keep the last one.
    pub fn new(
        tempo: f64,
        lane: Option<TempoLane>,
        numerator: u32,
        denominator: u32,
        changes: &[SignatureChange],
    ) -> Self {
        let mut sorted: Vec<SignatureChange> = changes.iter().copied().filter(|c| c.beat.is_finite()).collect();
        sorted.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        let mut meters = vec![Meter {
            beat: 0.0,
            bar: 0,
            numerator: numerator.max(1),
            denominator: denominator.max(1),
        }];
        for c in sorted {
            let last = *meters.last().expect("never empty");
            let bb = last.bar_beats();
            let bars = ((c.beat - last.beat) / bb).round().max(0.0) as i64;
            let m = Meter {
                beat: last.beat + bars as f64 * bb,
                bar: last.bar + bars,
                numerator: c.numerator.max(1),
                denominator: c.denominator.max(1),
            };
            if bars == 0 {
                *meters.last_mut().expect("never empty") = Meter { bar: last.bar, ..m };
            } else {
                meters.push(m);
            }
        }
        Self {
            tempo: tempo.clamp(20.0, 999.0),
            lane: lane.filter(|l| l.is_active()),
            meters,
        }
    }

    pub fn from_project(p: &Project) -> Self {
        Self::new(
            p.tempo,
            Some(p.tempo_lane.clone()),
            p.beats_per_bar,
            p.beat_unit,
            &p.signatures,
        )
    }

    pub fn has_tempo_automation(&self) -> bool {
        self.lane.is_some()
    }

    pub fn has_signature_changes(&self) -> bool {
        self.meters.len() > 1
    }

    /// Tempo at a beat. Allocation-free.
    #[inline]
    pub fn tempo_at(&self, beat: f64) -> f64 {
        match &self.lane {
            Some(l) => l
                .envelope
                .value_at(beat)
                .map(|n| l.denormalize(n))
                .unwrap_or(self.tempo),
            None => self.tempo,
        }
    }

    /// The signature in effect at a beat. Allocation-free.
    #[inline]
    pub fn meter_at(&self, beat: f64) -> Meter {
        let i = self.meters.partition_point(|m| m.beat <= beat + 1e-9);
        self.meters[i.saturating_sub(1)]
    }

    /// Bar index (0-based) and beat inside that bar.
    pub fn bar_of(&self, beat: f64) -> (i64, f64) {
        let m = self.meter_at(beat);
        let bb = m.bar_beats();
        let rel = beat - m.beat;
        let bars = (rel / bb + 1e-9).floor();
        (m.bar + bars as i64, rel - bars * bb)
    }

    /// Beat where bar `bar` (0-based) starts.
    pub fn bar_start(&self, bar: i64) -> f64 {
        let i = self.meters.partition_point(|m| m.bar <= bar);
        let m = self.meters[i.saturating_sub(1)];
        m.beat + (bar - m.bar) as f64 * m.bar_beats()
    }

    /// The first bar line at or after `beat`.
    pub fn next_bar(&self, beat: f64) -> f64 {
        let (bar, inside) = self.bar_of(beat);
        if inside < 1e-9 {
            beat
        } else {
            self.bar_start(bar + 1)
        }
    }

    /// Seconds from beat 0 to `beat`, following the tempo automation.
    pub fn secs_at(&self, beat: f64) -> f64 {
        if self.lane.is_none() || beat <= 0.0 {
            return beat * 60.0 / self.tempo;
        }
        // Midpoint rule over 1/16 notes is exact for steps and close
        // enough for ramps.
        const STEP: f64 = 0.25;
        let mut secs = 0.0;
        let mut b = 0.0;
        while b < beat {
            let e = (b + STEP).min(beat);
            secs += (e - b) * 60.0 / self.tempo_at((b + e) * 0.5);
            b = e;
        }
        secs
    }

    /// The beat reached after `secs` seconds from beat 0.
    pub fn beat_at_secs(&self, secs: f64) -> f64 {
        if self.lane.is_none() || secs <= 0.0 {
            return secs * self.tempo / 60.0;
        }
        const STEP: f64 = 0.25;
        let mut t = 0.0;
        let mut b = 0.0;
        loop {
            let dt = STEP * 60.0 / self.tempo_at(b + STEP * 0.5);
            if t + dt >= secs {
                return b + STEP * (secs - t) / dt;
            }
            t += dt;
            b += STEP;
        }
    }

    /// "bar.beat.sixteenth" for a position, counting bars from 1, with the
    /// beat counted in the signature's own unit.
    pub fn format_position(&self, beat: f64) -> String {
        let (bar, inside) = self.bar_of(beat.max(0.0));
        let m = self.meter_at(beat.max(0.0));
        let unit = 4.0 / m.denominator.max(1) as f64;
        let b = (inside / unit + 1e-9).floor();
        let six = ((inside - b * unit) / 0.25 + 1e-9).floor();
        format!("{}.{}.{}", bar + 1, b as i64 + 1, six as i64 + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_tempo_and_meter() {
        let m = TimeMap::new(120.0, None, 4, 4, &[]);
        assert_eq!(m.tempo_at(10.0), 120.0);
        assert!((m.secs_at(8.0) - 4.0).abs() < 1e-9);
        assert!((m.beat_at_secs(4.0) - 8.0).abs() < 1e-9);
        assert_eq!(m.bar_of(9.0), (2, 1.0));
        assert_eq!(m.bar_start(3), 12.0);
        assert_eq!(m.format_position(5.25), "2.2.2");
    }

    #[test]
    fn signature_changes_move_bar_lines() {
        // 4/4 for two bars, then 3/4, then 7/8 from bar 5.
        let changes = [
            SignatureChange {
                beat: 8.2,
                numerator: 3,
                denominator: 4,
            },
            SignatureChange {
                beat: 14.0,
                numerator: 7,
                denominator: 8,
            },
        ];
        let m = TimeMap::new(120.0, None, 4, 4, &changes);
        assert_eq!(m.meters.len(), 3);
        assert_eq!(m.meters[1].beat, 8.0);
        assert_eq!(m.bar_start(2), 8.0);
        assert_eq!(m.bar_start(3), 11.0);
        assert_eq!(m.bar_start(4), 14.0);
        assert_eq!(m.bar_start(5), 17.5);
        assert_eq!(m.bar_of(12.0), (3, 1.0));
        assert_eq!(m.meter_at(15.0).numerator, 7);
        assert_eq!(m.next_bar(14.5), 17.5);
        assert_eq!(m.format_position(14.5), "5.2.1");
    }

    #[test]
    fn tempo_automation_changes_time() {
        let mut lane = TempoLane::default();
        // 120 BPM for 4 beats, then a jump to 60 BPM.
        let n120 = lane.normalize(120.0);
        let n60 = lane.normalize(60.0);
        lane.envelope.insert(0.0, n120);
        lane.envelope.insert(4.0, n120);
        lane.envelope.insert(4.0 + 1e-6, n60);
        let m = TimeMap::new(100.0, Some(lane), 4, 4, &[]);
        assert!((m.tempo_at(2.0) - 120.0).abs() < 1e-6);
        assert!((m.tempo_at(6.0) - 60.0).abs() < 1e-3);
        // 4 beats at 120 = 2 s, 2 beats at 60 = 2 s.
        assert!((m.secs_at(6.0) - 4.0).abs() < 1e-3);
        assert!((m.beat_at_secs(4.0) - 6.0).abs() < 1e-3);
    }

    #[test]
    fn empty_lane_is_ignored() {
        let m = TimeMap::new(90.0, Some(TempoLane::default()), 4, 4, &[]);
        assert!(!m.has_tempo_automation());
        assert_eq!(m.tempo_at(3.0), 90.0);
    }
}
