//! The factory MIDI packs shown under Clips in the Browser: chord
//! progressions, basslines, melodies and drum patterns in common styles.
//!
//! Every clip is generated here from Tessera's own patterns (nothing is
//! copied from other packs), so the packs can be shipped and used in any
//! music without restriction. They are written as `.mid` files on first
//! start, like the factory samples.

use crate::model::Note;

/// One generated clip.
pub struct PackClip {
    /// Sub-folder, e.g. `Chords/Pop`.
    pub folder: &'static str,
    pub name: String,
    /// Tempo saved in the file (the style's usual tempo).
    pub tempo: f64,
    /// Length in beats.
    pub length: f64,
    pub notes: Vec<Note>,
}

#[derive(Clone, Copy)]
enum Q {
    Maj,
    Min,
    Maj7,
    Min7,
    Dom7,
    Maj9,
    Min9,
    Dom9,
    Sus4,
}

impl Q {
    fn intervals(self) -> &'static [u8] {
        match self {
            Q::Maj => &[0, 4, 7],
            Q::Min => &[0, 3, 7],
            Q::Maj7 => &[0, 4, 7, 11],
            Q::Min7 => &[0, 3, 7, 10],
            Q::Dom7 => &[0, 4, 7, 10],
            Q::Maj9 => &[0, 4, 7, 11, 14],
            Q::Min9 => &[0, 3, 7, 10, 14],
            Q::Dom9 => &[0, 4, 7, 10, 14],
            Q::Sus4 => &[0, 5, 7],
        }
    }

    /// Third, fifth and seventh above the root that a bassline can use.
    fn third(self) -> u8 {
        match self {
            Q::Min | Q::Min7 | Q::Min9 => 3,
            Q::Sus4 => 5,
            _ => 4,
        }
    }

    fn seventh(self) -> u8 {
        match self {
            Q::Maj7 | Q::Maj9 => 11,
            _ => 10,
        }
    }
}

/// A chord of a progression: semitones above the key's tonic and quality.
type Step = (u8, Q);

struct Progression {
    style: &'static str,
    name: &'static str,
    minor: bool,
    tempo: f64,
    chords: &'static [Step],
    /// (start, length) hits within one bar.
    rhythm: &'static [(f64, f64)],
}

const SUSTAIN: &[(f64, f64)] = &[(0.0, 4.0)];
const POP_PULSE: &[(f64, f64)] = &[(0.0, 1.5), (1.5, 1.0), (2.5, 1.5)];
const OFFBEAT_STABS: &[(f64, f64)] = &[(0.5, 0.25), (1.5, 0.25), (2.5, 0.25), (3.5, 0.25)];
const LOFI_COMP: &[(f64, f64)] = &[(0.0, 2.5), (2.75, 1.25)];
const NEO_PUSH: &[(f64, f64)] = &[(0.0, 1.5), (1.75, 2.25)];
const GOSPEL_HITS: &[(f64, f64)] = &[(0.0, 1.0), (1.0, 0.5), (2.0, 1.0), (3.0, 1.0)];
const HALF: &[(f64, f64)] = &[(0.0, 2.0), (2.0, 2.0)];

use Q::*;

const PROGRESSIONS: &[Progression] = &[
    Progression {
        style: "Pop",
        name: "I-V-vi-IV",
        minor: false,
        tempo: 110.0,
        chords: &[(0, Maj), (7, Maj), (9, Min), (5, Maj)],
        rhythm: POP_PULSE,
    },
    Progression {
        style: "Pop",
        name: "vi-IV-I-V",
        minor: false,
        tempo: 100.0,
        chords: &[(9, Min), (5, Maj), (0, Maj), (7, Maj)],
        rhythm: SUSTAIN,
    },
    Progression {
        style: "Pop",
        name: "IV-V-iii-vi",
        minor: false,
        tempo: 120.0,
        chords: &[(5, Maj7), (7, Dom7), (4, Min7), (9, Min)],
        rhythm: HALF,
    },
    Progression {
        style: "Jazz",
        name: "ii-V-I-vi",
        minor: false,
        tempo: 120.0,
        chords: &[(2, Min7), (7, Dom7), (0, Maj7), (9, Min7)],
        rhythm: NEO_PUSH,
    },
    Progression {
        style: "Neo Soul",
        name: "IVmaj9-iii7-vi9-V9",
        minor: false,
        tempo: 84.0,
        chords: &[(5, Maj9), (4, Min7), (9, Min9), (7, Dom9)],
        rhythm: NEO_PUSH,
    },
    Progression {
        style: "Lo-Fi",
        name: "ii9-V9-Imaj9-vi9",
        minor: false,
        tempo: 80.0,
        chords: &[(2, Min9), (7, Dom9), (0, Maj9), (9, Min9)],
        rhythm: LOFI_COMP,
    },
    Progression {
        style: "Gospel",
        name: "I-iii7-IV-iv",
        minor: false,
        tempo: 76.0,
        chords: &[(0, Maj), (4, Min7), (5, Maj), (5, Min)],
        rhythm: GOSPEL_HITS,
    },
    Progression {
        style: "House",
        name: "i7-iv7-VImaj7-v7 Stabs",
        minor: true,
        tempo: 124.0,
        chords: &[(0, Min7), (5, Min7), (8, Maj7), (7, Min7)],
        rhythm: OFFBEAT_STABS,
    },
    Progression {
        style: "Trap",
        name: "i-VI-III-VII",
        minor: true,
        tempo: 140.0,
        chords: &[(0, Min), (8, Maj), (3, Maj), (10, Maj)],
        rhythm: SUSTAIN,
    },
    Progression {
        style: "Cinematic",
        name: "i-iv-VI-V",
        minor: true,
        tempo: 90.0,
        chords: &[(0, Min), (5, Min), (8, Maj), (7, Maj)],
        rhythm: SUSTAIN,
    },
    Progression {
        style: "Cinematic",
        name: "i-VII-VI-V",
        minor: true,
        tempo: 96.0,
        chords: &[(0, Min), (10, Maj), (8, Maj), (7, Sus4)],
        rhythm: HALF,
    },
    Progression {
        style: "Blues",
        name: "12-Bar I7-IV7-V7",
        minor: false,
        tempo: 96.0,
        chords: &[
            (0, Dom7),
            (5, Dom7),
            (0, Dom7),
            (0, Dom7),
            (5, Dom7),
            (5, Dom7),
            (0, Dom7),
            (0, Dom7),
            (7, Dom7),
            (5, Dom7),
            (0, Dom7),
            (7, Dom7),
        ],
        rhythm: POP_PULSE,
    },
];

const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B"];

fn note(pitch: u8, velocity: u8, start: f64, length: f64) -> Note {
    Note {
        pitch,
        velocity,
        start,
        length,
    }
}

/// The chord's pitches, moved by octaves so they sit near `centre` (simple
/// voice leading: each chord stays close to the last one).
fn voice(tonic: u8, step: Step, centre: f64) -> Vec<u8> {
    let (deg, q) = step;
    let root = tonic + deg;
    let mut best: Vec<u8> = Vec::new();
    let mut best_d = f64::MAX;
    // Try each inversion in a couple of octaves.
    let base: Vec<u8> = q.intervals().iter().map(|i| root + i).collect();
    for inv in 0..base.len() {
        let mut v: Vec<u8> = base.clone();
        for p in v.iter_mut().take(inv) {
            *p += 12;
        }
        for shift in [-24i32, -12, 0, 12] {
            let w: Vec<u8> = v.iter().map(|&p| (p as i32 + shift).clamp(0, 127) as u8).collect();
            let mean = w.iter().map(|&p| p as f64).sum::<f64>() / w.len() as f64;
            let d = (mean - centre).abs() + if w[0] < 48 { 6.0 } else { 0.0 };
            if d < best_d {
                best_d = d;
                best = w;
            }
        }
    }
    best.sort_unstable();
    best
}

fn chord_clip(p: &Progression, tonic: u8) -> PackClip {
    let mut notes = Vec::new();
    let mut centre = 64.0;
    for (bar, &step) in p.chords.iter().enumerate() {
        let v = voice(tonic, step, centre);
        centre = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
        let at = bar as f64 * 4.0;
        for (i, &(start, len)) in p.rhythm.iter().enumerate() {
            let vel = if i == 0 { 96 } else { 84 };
            for &pitch in &v {
                notes.push(note(pitch, vel, at + start, len));
            }
        }
        // A low root under sustained chords fills them out.
        if p.rhythm.len() <= 2 {
            let mut root = tonic + step.0;
            while root >= 52 {
                root -= 12;
            }
            notes.push(note(root, 90, at, 4.0));
        }
    }
    let key = format!(
        "{}{}",
        NOTE_NAMES[(tonic % 12) as usize],
        if p.minor { "m" } else { "" }
    );
    PackClip {
        folder: chord_folder(p.style),
        name: format!("{} {} - {key}", p.style, p.name),
        tempo: p.tempo,
        length: p.chords.len() as f64 * 4.0,
        notes,
    }
}

fn chord_folder(style: &str) -> &'static str {
    match style {
        "Pop" => "Chords/Pop",
        "Jazz" => "Chords/Jazz",
        "Neo Soul" => "Chords/Neo Soul",
        "Lo-Fi" => "Chords/Lo-Fi",
        "Gospel" => "Chords/Gospel",
        "House" => "Chords/House",
        "Trap" => "Chords/Trap",
        "Cinematic" => "Chords/Cinematic",
        _ => "Chords/Blues",
    }
}

// ----- basslines ------------------------------------------------------------------

/// Bass notes for one bar: (start, length, semitones above the chord root,
/// velocity). Offsets of 12 are the octave.
type BassBar = &'static [(f64, f64, i8, u8)];

struct Bassline {
    name: &'static str,
    tempo: f64,
    /// Key tonic as a MIDI note in the bass register.
    tonic: u8,
    chords: &'static [Step],
    bar: BassBar,
    /// Offsets 3 and 10 become the chord's own third and seventh.
    chordal: bool,
}

const BASSLINES: &[Bassline] = &[
    Bassline {
        name: "Pop Root Eighths - C",
        tempo: 110.0,
        tonic: 36,
        chords: &[(0, Maj), (7, Maj), (9, Min), (5, Maj)],
        bar: &[
            (0.0, 0.45, 0, 104),
            (0.5, 0.45, 0, 80),
            (1.0, 0.45, 0, 92),
            (1.5, 0.45, 0, 80),
            (2.0, 0.45, 0, 100),
            (2.5, 0.45, 0, 80),
            (3.0, 0.45, 0, 92),
            (3.5, 0.45, 7, 84),
        ],
        chordal: false,
    },
    Bassline {
        name: "House Offbeat - Am",
        tempo: 124.0,
        tonic: 33,
        chords: &[(0, Min7), (5, Min7), (8, Maj7), (7, Min7)],
        bar: &[
            (0.5, 0.35, 0, 110),
            (1.5, 0.35, 0, 100),
            (2.5, 0.35, 0, 110),
            (3.5, 0.25, 12, 96),
        ],
        chordal: false,
    },
    Bassline {
        name: "Disco Octaves - Am",
        tempo: 120.0,
        tonic: 33,
        chords: &[(0, Min7), (5, Min7), (8, Maj7), (7, Min7)],
        bar: &[
            (0.0, 0.4, 0, 108),
            (0.5, 0.4, 12, 92),
            (1.0, 0.4, 0, 104),
            (1.5, 0.4, 12, 92),
            (2.0, 0.4, 0, 104),
            (2.5, 0.4, 12, 92),
            (3.0, 0.4, 0, 104),
            (3.5, 0.4, 12, 92),
        ],
        chordal: false,
    },
    Bassline {
        name: "Trap 808 - Am",
        tempo: 140.0,
        tonic: 33,
        chords: &[(0, Min), (8, Maj), (3, Maj), (10, Maj)],
        bar: &[(0.0, 1.4, 0, 120), (1.5, 0.5, 0, 100), (2.5, 1.5, 0, 112)],
        chordal: false,
    },
    Bassline {
        name: "Funk Sixteenths - Em",
        tempo: 100.0,
        tonic: 28,
        chords: &[(0, Min7), (0, Min7), (5, Dom7), (7, Dom7)],
        bar: &[
            (0.0, 0.2, 0, 116),
            (0.5, 0.2, 12, 88),
            (0.75, 0.2, 0, 70),
            (1.25, 0.2, 10, 96),
            (1.75, 0.2, 12, 92),
            (2.0, 0.2, 0, 110),
            (2.75, 0.2, 7, 90),
            (3.0, 0.2, 10, 96),
            (3.5, 0.2, 12, 100),
            (3.75, 0.2, 7, 76),
        ],
        chordal: true,
    },
    Bassline {
        name: "Walking ii-V-I-vi - C",
        tempo: 120.0,
        tonic: 36,
        chords: &[(2, Min7), (7, Dom7), (0, Maj7), (9, Min7)],
        bar: &[
            (0.0, 0.9, 0, 100),
            (1.0, 0.9, 3, 88),
            (2.0, 0.9, 7, 92),
            (3.0, 0.9, 10, 86),
        ],
        chordal: true,
    },
    Bassline {
        name: "Dembow Bounce - Dm",
        tempo: 95.0,
        tonic: 38,
        chords: &[(0, Min), (10, Maj), (8, Maj), (7, Maj)],
        bar: &[
            (0.0, 0.7, 0, 112),
            (0.75, 0.25, 0, 90),
            (1.5, 0.45, 7, 96),
            (2.0, 0.7, 0, 108),
            (2.75, 0.25, 0, 90),
            (3.5, 0.45, 12, 96),
        ],
        chordal: false,
    },
    Bassline {
        name: "Synthwave Pulse - Am",
        tempo: 108.0,
        tonic: 33,
        chords: &[(0, Min), (8, Maj), (3, Maj), (10, Maj)],
        bar: &[
            (0.0, 0.22, 0, 108),
            (0.25, 0.22, 0, 76),
            (0.5, 0.22, 0, 92),
            (0.75, 0.22, 0, 76),
            (1.0, 0.22, 0, 100),
            (1.25, 0.22, 0, 76),
            (1.5, 0.22, 0, 92),
            (1.75, 0.22, 0, 76),
            (2.0, 0.22, 0, 104),
            (2.25, 0.22, 0, 76),
            (2.5, 0.22, 0, 92),
            (2.75, 0.22, 0, 76),
            (3.0, 0.22, 0, 100),
            (3.25, 0.22, 0, 76),
            (3.5, 0.22, 12, 92),
            (3.75, 0.22, 0, 76),
        ],
        chordal: false,
    },
    Bassline {
        name: "Dub Steps - Gm",
        tempo: 75.0,
        tonic: 31,
        chords: &[(0, Min), (0, Min), (5, Min), (7, Min)],
        bar: &[
            (0.0, 1.0, 0, 112),
            (1.5, 0.5, 0, 92),
            (2.0, 1.0, 7, 100),
            (3.5, 0.5, 10, 88),
        ],
        chordal: true,
    },
    Bassline {
        name: "Neo Soul Glide - F",
        tempo: 84.0,
        tonic: 29,
        chords: &[(5, Maj9), (4, Min7), (9, Min9), (7, Dom9)],
        bar: &[
            (0.0, 1.5, 0, 100),
            (1.75, 1.25, 7, 88),
            (3.25, 0.5, 10, 80),
            (3.75, 0.25, 11, 72),
        ],
        chordal: true,
    },
];

fn bass_clip(b: &Bassline) -> PackClip {
    let mut notes = Vec::new();
    for (bar, &(deg, q)) in b.chords.iter().enumerate() {
        let root = b.tonic + deg;
        // Keep the line in the bass register.
        let root = if root > 45 { root - 12 } else { root };
        for &(start, len, off, vel) in b.bar {
            let off = match (b.chordal, off) {
                (true, 3) => q.third() as i8,
                (true, 10) => q.seventh() as i8,
                _ => off,
            };
            let pitch = (root as i8 + off).clamp(0, 127) as u8;
            notes.push(note(pitch, vel, bar as f64 * 4.0 + start, len));
        }
    }
    PackClip {
        folder: "Basslines",
        name: b.name.to_string(),
        tempo: b.tempo,
        length: b.chords.len() as f64 * 4.0,
        notes,
    }
}

// ----- melodies and arpeggios -----------------------------------------------------

/// An arpeggio over a progression: chord tones in `order` (indexes into the
/// voiced chord, wrapping an octave up), one every `step` beats.
fn arp_clip(name: &str, tempo: f64, tonic: u8, chords: &[Step], step: f64, order: &[usize], gate: f64) -> PackClip {
    let mut notes = Vec::new();
    let mut centre = 70.0;
    for (bar, &c) in chords.iter().enumerate() {
        let v = voice(tonic, c, centre);
        centre = v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64;
        let n = (4.0 / step).round() as usize;
        for i in 0..n {
            let idx = order[i % order.len()];
            let pitch = v[idx % v.len()] + 12 * (idx / v.len()) as u8;
            let vel = if i % 4 == 0 { 104 } else { 82 };
            notes.push(note(pitch, vel, bar as f64 * 4.0 + i as f64 * step, step * gate));
        }
    }
    PackClip {
        folder: "Melodies",
        name: name.to_string(),
        tempo,
        length: chords.len() as f64 * 4.0,
        notes,
    }
}

/// A written melody: (start, length, pitch).
fn melody_clip(name: &str, tempo: f64, bars: f64, line: &[(f64, f64, u8)]) -> PackClip {
    PackClip {
        folder: "Melodies",
        name: name.to_string(),
        tempo,
        length: bars * 4.0,
        notes: line
            .iter()
            .enumerate()
            .map(|(i, &(s, l, p))| note(p, if i % 2 == 0 { 100 } else { 88 }, s, l))
            .collect(),
    }
}

fn melodies() -> Vec<PackClip> {
    let pop = [(0, Maj), (7, Maj), (9, Min), (5, Maj)];
    let minor = [(0, Min), (8, Maj), (3, Maj), (10, Maj)];
    let ballad = [(9, Min), (5, Maj), (0, Maj), (7, Maj)];
    let mut v = vec![
        arp_clip("Arp Up 16ths - C", 120.0, 60, &pop, 0.25, &[0, 1, 2, 3], 0.8),
        arp_clip(
            "Arp Up-Down 8ths - C",
            100.0,
            60,
            &ballad,
            0.5,
            &[0, 1, 2, 3, 2, 1],
            0.9,
        ),
        arp_clip(
            "Trance Gate Arp - Am",
            138.0,
            57,
            &minor,
            0.25,
            &[0, 2, 1, 3, 2, 4],
            0.5,
        ),
        arp_clip("Broken Chord Pluck - Am", 110.0, 57, &minor, 0.5, &[0, 2, 1, 2], 0.6),
    ];
    // C major pentatonic hook over I-V-vi-IV.
    v.push(melody_clip(
        "Pentatonic Hook - C",
        110.0,
        4.0,
        &[
            (0.0, 0.5, 76),
            (0.5, 0.5, 79),
            (1.0, 1.0, 81),
            (2.0, 0.5, 79),
            (2.5, 1.5, 76),
            (4.0, 0.5, 74),
            (4.5, 0.5, 76),
            (5.0, 1.0, 79),
            (6.0, 2.0, 74),
            (8.0, 0.5, 72),
            (8.5, 0.5, 74),
            (9.0, 1.0, 76),
            (10.0, 0.5, 79),
            (10.5, 1.5, 81),
            (12.0, 1.0, 79),
            (13.0, 0.5, 76),
            (13.5, 0.5, 74),
            (14.0, 2.0, 72),
        ],
    ));
    // A minor riff over i-VI-III-VII.
    v.push(melody_clip(
        "Minor Pluck Riff - Am",
        128.0,
        2.0,
        &[
            (0.0, 0.25, 69),
            (0.5, 0.25, 72),
            (0.75, 0.25, 76),
            (1.5, 0.25, 74),
            (2.0, 0.5, 72),
            (2.75, 0.25, 71),
            (3.0, 0.5, 67),
            (4.0, 0.25, 69),
            (4.5, 0.25, 72),
            (4.75, 0.25, 76),
            (5.5, 0.25, 79),
            (6.0, 0.5, 77),
            (6.75, 0.25, 76),
            (7.0, 1.0, 74),
        ],
    ));
    // Lazy lo-fi lead in C, behind the beat.
    v.push(melody_clip(
        "Lo-Fi Lead - C",
        80.0,
        4.0,
        &[
            (0.25, 1.0, 76),
            (1.5, 0.5, 74),
            (2.0, 1.5, 71),
            (4.25, 0.75, 74),
            (5.0, 0.5, 72),
            (5.5, 2.0, 69),
            (8.25, 1.0, 72),
            (9.5, 0.5, 74),
            (10.0, 1.0, 76),
            (11.0, 0.5, 79),
            (12.25, 1.5, 77),
            (14.0, 2.0, 76),
        ],
    ));
    // Call-and-response trap bell in A minor (harmonic minor colour).
    v.push(melody_clip(
        "Trap Bell - Am",
        140.0,
        2.0,
        &[
            (0.0, 0.5, 81),
            (0.75, 0.25, 80),
            (1.0, 0.5, 81),
            (1.5, 0.5, 76),
            (2.5, 0.5, 72),
            (3.0, 1.0, 71),
            (4.0, 0.5, 81),
            (4.75, 0.25, 83),
            (5.0, 0.5, 84),
            (5.5, 0.5, 83),
            (6.0, 2.0, 76),
        ],
    ));
    v
}

// ----- drums --------------------------------------------------------------------

const KICK: u8 = 36;
const RIM: u8 = 37;
const SNARE: u8 = 38;
const CLAP: u8 = 39;
const LOW_TOM: u8 = 41;
const CLOSED_HAT: u8 = 42;
const MID_TOM: u8 = 45;
const OPEN_HAT: u8 = 46;
const HIGH_TOM: u8 = 48;
const CRASH: u8 = 49;
const RIDE: u8 = 51;

/// One drum lane: a General MIDI note (the Drum Rack's pad layout) and a
/// bar of steps. `X` is an accent, `x` a normal hit, `o` a ghost note and
/// `.` a rest. The number of characters sets the grid (16 = sixteenths,
/// 32 = thirty-seconds, 12 = eighth triplets).
type Lane = (u8, &'static str);

struct Groove {
    name: &'static str,
    tempo: f64,
    /// Lanes of the main bar, played `bars` times.
    lanes: &'static [Lane],
    bars: usize,
    /// Lanes replacing the main bar's in the last bar (a fill), if any.
    last: &'static [Lane],
    /// Delays every other sixteenth by this fraction of a sixteenth.
    swing: f64,
}

const GROOVES: &[Groove] = &[
    Groove {
        name: "House 124",
        tempo: 124.0,
        lanes: &[
            (KICK, "X...x...x...x..."),
            (CLAP, "....x.......x..."),
            (CLOSED_HAT, "o.o.o.o.o.o.o.o."),
            (OPEN_HAT, "..x...x...x...x."),
        ],
        bars: 2,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Techno 130",
        tempo: 130.0,
        lanes: &[
            (KICK, "X...x...x...x..."),
            (CLOSED_HAT, "..x...x...x...x."),
            (RIM, "...o..o....o..o."),
            (CLAP, "....x.......x..."),
            (RIDE, "o.o.o.o.o.o.o.o."),
        ],
        bars: 2,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Boom Bap 90",
        tempo: 90.0,
        lanes: &[
            (KICK, "X.......x.x....."),
            (SNARE, "....X.......X..."),
            (CLOSED_HAT, "x.x.x.x.x.x.x.xo"),
        ],
        bars: 2,
        last: &[
            (KICK, "X.......x..x..x."),
            (SNARE, "....X.......X.o."),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x."),
        ],
        swing: 0.5,
    },
    Groove {
        name: "Trap 140",
        tempo: 140.0,
        lanes: &[
            (KICK, "X......x..x....."),
            (CLAP, "........X......."),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x.x.x.x.x.xxxxx.x."),
        ],
        bars: 2,
        last: &[
            (KICK, "X......x..x...x."),
            (CLAP, "........X......."),
            (CLOSED_HAT, "x.x.x.x.xxx.x.x.x.x.x.x.xxxxxxxx"),
        ],
        swing: 0.0,
    },
    Groove {
        name: "Drum and Bass 174",
        tempo: 174.0,
        lanes: &[
            (KICK, "X.........x....."),
            (SNARE, "....X..o....X..o"),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x."),
        ],
        bars: 2,
        last: &[
            (KICK, "X.........x....."),
            (SNARE, "....X..o..o.XoXx"),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x."),
        ],
        swing: 0.0,
    },
    Groove {
        name: "Breakbeat 130",
        tempo: 130.0,
        lanes: &[
            (KICK, "X.x.......xx...."),
            (SNARE, "....X..o.o..X..o"),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x."),
        ],
        bars: 2,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Dembow 95",
        tempo: 95.0,
        lanes: &[
            (KICK, "X...x...X...x..."),
            (SNARE, "...x..x....x..x."),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x."),
        ],
        bars: 2,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Rock 120",
        tempo: 120.0,
        lanes: &[
            (KICK, "X.....x.x......."),
            (SNARE, "....X.......X..."),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x."),
        ],
        bars: 2,
        last: &[
            (KICK, "X.....x.x......."),
            (SNARE, "....X.......xxxx"),
            (CLOSED_HAT, "x.x.x.x.x.x....."),
        ],
        swing: 0.0,
    },
    Groove {
        name: "Funk 100",
        tempo: 100.0,
        lanes: &[
            (KICK, "X..x..x...x..x.."),
            (SNARE, "....X..o.o..X..o"),
            (CLOSED_HAT, "xoxoxoxoxoxoxoxo"),
            (OPEN_HAT, "..............x."),
        ],
        bars: 2,
        last: &[],
        swing: 0.25,
    },
    Groove {
        name: "Lo-Fi 80",
        tempo: 80.0,
        lanes: &[
            (KICK, "X......x..x....."),
            (SNARE, "....x.......x..."),
            (CLOSED_HAT, "o.o.o.o.o.o.o.o."),
            (RIM, "..............o."),
        ],
        bars: 2,
        last: &[],
        swing: 0.6,
    },
    Groove {
        name: "Afrobeats 105",
        tempo: 105.0,
        lanes: &[
            (KICK, "X.....x...x....."),
            (RIM, "...x..x....x..x."),
            (CLOSED_HAT, "x.xxx.xxx.xxx.xx"),
            (CLAP, "....x.......x..."),
        ],
        bars: 2,
        last: &[],
        swing: 0.15,
    },
    Groove {
        name: "UK Garage 2-Step 132",
        tempo: 132.0,
        lanes: &[
            (KICK, "X.........x....."),
            (SNARE, "....X.......X..."),
            (CLOSED_HAT, "..x...x...x...x."),
            (RIM, ".o.....o.o......"),
        ],
        bars: 2,
        last: &[],
        swing: 0.55,
    },
    Groove {
        name: "Disco 120",
        tempo: 120.0,
        lanes: &[
            (KICK, "X...x...x...x..."),
            (CLAP, "....x.......x..."),
            (OPEN_HAT, "..x...x...x...x."),
            (CLOSED_HAT, "o.o.o.o.o.o.o.o."),
        ],
        bars: 2,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Pop 100",
        tempo: 100.0,
        lanes: &[
            (KICK, "X.......x.x....."),
            (SNARE, "....X.......X..."),
            (CLAP, "....x.......x..."),
            (CLOSED_HAT, "x.x.x.x.x.x.x.x."),
        ],
        bars: 2,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Half-Time Shuffle 85",
        tempo: 85.0,
        lanes: &[
            (KICK, "X.....x..x.."),
            (SNARE, "......X....."),
            (CLOSED_HAT, "x.ox.ox.ox.o"),
        ],
        bars: 2,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Fill - Toms 120",
        tempo: 120.0,
        lanes: &[
            (SNARE, "X.x.x.x........."),
            (HIGH_TOM, "........x.x....."),
            (MID_TOM, "............x.x."),
            (LOW_TOM, "..............xx"),
        ],
        bars: 1,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Fill - Snare Build 128",
        tempo: 128.0,
        lanes: &[(SNARE, "o...o...o.o.o.o.xxxxxxxxXXXXXXXX"), (KICK, "x...x...x...x...")],
        bars: 1,
        last: &[],
        swing: 0.0,
    },
    Groove {
        name: "Fill - Crash Downbeat",
        tempo: 120.0,
        lanes: &[(KICK, "X..............."), (CRASH, "X...............")],
        bars: 1,
        last: &[],
        swing: 0.0,
    },
];

fn groove_clip(g: &Groove) -> PackClip {
    let mut notes = Vec::new();
    for bar in 0..g.bars {
        let lanes = if bar + 1 == g.bars && !g.last.is_empty() {
            g.last
        } else {
            g.lanes
        };
        for &(pitch, steps) in lanes {
            let n = steps.chars().count().max(1);
            let step = 4.0 / n as f64;
            for (i, c) in steps.chars().enumerate() {
                let vel = match c {
                    'X' => 124,
                    'x' => 100,
                    'o' => 62,
                    _ => continue,
                };
                let mut start = i as f64 * step;
                // Swing the off-beat sixteenths (only on a sixteenth grid).
                if n == 16 && i % 2 == 1 {
                    start += g.swing * step * 0.5;
                }
                notes.push(note(pitch, vel, bar as f64 * 4.0 + start, step.min(0.25)));
            }
        }
    }
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
    PackClip {
        folder: if g.name.starts_with("Fill") {
            "Drums/Fills"
        } else {
            "Drums"
        },
        name: g.name.to_string(),
        tempo: g.tempo,
        length: g.bars as f64 * 4.0,
        notes,
    }
}

/// Every clip of the factory MIDI packs.
pub fn factory_clips() -> Vec<PackClip> {
    let mut out = Vec::new();
    for p in PROGRESSIONS {
        // Two keys each: C and G major, or A and E minor.
        let keys: [u8; 2] = if p.minor { [57, 52] } else { [60, 55] };
        for k in keys {
            out.push(chord_clip(p, k));
        }
    }
    out.extend(BASSLINES.iter().map(bass_clip));
    out.extend(melodies());
    out.extend(GROOVES.iter().map(groove_clip));
    // Nothing hangs past the end of its clip (swung last steps).
    for c in out.iter_mut() {
        let end = c.length;
        for n in c.notes.iter_mut() {
            n.length = n.length.min(end - n.start);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_are_full_named_uniquely_and_in_range() {
        let clips = factory_clips();
        assert!(clips.len() >= 50, "{}", clips.len());
        let mut names: Vec<String> = clips.iter().map(|c| format!("{}/{}", c.folder, c.name)).collect();
        names.sort();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate names");
        for c in &clips {
            assert!(!c.notes.is_empty(), "{}", c.name);
            assert!(c.length > 0.0 && c.length % 4.0 == 0.0, "{}", c.name);
            for note in &c.notes {
                assert!(note.start >= 0.0 && note.start < c.length, "{}: {:?}", c.name, note);
                assert!(note.length > 0.0 && note.start + note.length <= c.length + 1e-9);
                assert!(note.pitch <= 108 && note.velocity > 0 && note.velocity <= 127);
            }
            assert!(!c.name.contains('/'), "{}", c.name);
        }
    }

    #[test]
    fn chords_follow_the_progression() {
        let clips = factory_clips();
        let pop = clips.iter().find(|c| c.name == "Pop I-V-vi-IV - C").unwrap();
        // Second bar is G major: every note is G, B or D.
        let bar2: Vec<u8> = pop
            .notes
            .iter()
            .filter(|n| (4.0..8.0).contains(&n.start))
            .map(|n| n.pitch % 12)
            .collect();
        assert!(!bar2.is_empty());
        assert!(bar2.iter().all(|p| [7, 11, 2].contains(p)), "{bar2:?}");
        let drums = clips.iter().find(|c| c.name == "House 124").unwrap();
        assert_eq!(drums.notes.iter().filter(|n| n.pitch == KICK).count(), 8);
    }
}
