//! Standard MIDI File import and export (via the `midly` crate).
//!
//! Positions are in beats (quarter notes), as everywhere in the project.

use midly::num::{u15, u24, u28, u4, u7};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

use crate::model::{Clip, Note};

/// Ticks per quarter note of exported files.
pub const EXPORT_PPQ: u16 = 480;

/// Writes a MIDI clip as a format 0 Standard MIDI File (480 ppq) with the
/// clip name as track name, a tempo and a time signature. The end of the
/// track is at the clip's length, so the loop length survives a round trip.
pub fn export_clip(clip: &Clip, tempo: f64, beats_per_bar: u32, beat_unit: u32) -> Vec<u8> {
    let ppq = EXPORT_PPQ as f64;
    let to_tick = |beats: f64| (beats.max(0.0) * ppq).round() as u64;

    // (tick, order, event); offs sort before ons at the same tick.
    let mut events: Vec<(u64, u8, MidiMessage)> = Vec::new();
    for n in clip.notes() {
        let key = u7::new(n.pitch.min(127));
        let on = to_tick(n.start);
        let off = to_tick(n.start + n.length).max(on + 1);
        events.push((
            on,
            1,
            MidiMessage::NoteOn {
                key,
                vel: u7::new(n.velocity.clamp(1, 127)),
            },
        ));
        events.push((off, 0, MidiMessage::NoteOff { key, vel: u7::new(64) }));
    }
    events.sort_by_key(|e| (e.0, e.1, key_of(&e.2)));

    let name = clip.name.as_bytes();
    let us_per_quarter = (60_000_000.0 / tempo.clamp(1.0, 999.0)).round() as u32;
    let denom_pow = beat_unit.max(1).ilog2().min(255) as u8;
    let mut track: Vec<TrackEvent> = vec![
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(name)),
        },
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us_per_quarter.min(0xFF_FFFF)))),
        },
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TimeSignature(
                beats_per_bar.clamp(1, 255) as u8,
                denom_pow,
                24,
                8,
            )),
        },
    ];
    let mut last = 0u64;
    for (tick, _, message) in events {
        track.push(TrackEvent {
            delta: u28::new((tick - last).min(0x0FFF_FFFF) as u32),
            kind: TrackEventKind::Midi {
                channel: u4::new(0),
                message,
            },
        });
        last = tick;
    }
    let end = to_tick(clip.length).max(last);
    track.push(TrackEvent {
        delta: u28::new((end - last).min(0x0FFF_FFFF) as u32),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });

    let mut smf = Smf::new(Header::new(Format::SingleTrack, Timing::Metrical(u15::new(EXPORT_PPQ))));
    smf.tracks.push(track);
    let mut out = Vec::new();
    smf.write_std(&mut out).expect("writing to a Vec cannot fail");
    out
}

fn key_of(m: &MidiMessage) -> u8 {
    match m {
        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => key.as_int(),
        _ => 0,
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImportedMidi {
    /// First tempo in the file, in BPM.
    pub tempo: Option<f64>,
    /// First time signature (numerator, denominator).
    pub time_signature: Option<(u32, u32)>,
    pub tracks: Vec<ImportedTrack>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ImportedTrack {
    pub name: String,
    pub notes: Vec<Note>,
    /// Length in beats, rounded up to whole bars.
    pub length: f64,
}

/// Reads a Standard MIDI File (format 0 or 1, any resolution). Tracks without
/// notes are dropped; a format 0 file using several channels is split into
/// one track per channel.
pub fn import(bytes: &[u8]) -> Result<ImportedMidi, String> {
    let smf = Smf::parse(bytes).map_err(|e| format!("not a valid MIDI file: {e}"))?;
    if smf.header.format == Format::Sequential {
        return Err("format 2 MIDI files are not supported".into());
    }

    let mut tempo = None;
    let mut time_signature = None;
    for t in &smf.tracks {
        for e in t {
            match e.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) if tempo.is_none() && us.as_int() > 0 => {
                    tempo = Some(60_000_000.0 / us.as_int() as f64);
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(n, d, _, _)) if time_signature.is_none() => {
                    time_signature = Some((n.max(1) as u32, 1u32 << d.min(31)));
                }
                _ => {}
            }
        }
    }

    // Ticks to beats.
    let beats_per_tick = match smf.header.timing {
        Timing::Metrical(ppq) => 1.0 / ppq.as_int().max(1) as f64,
        Timing::Timecode(fps, sub) => {
            let ticks_per_sec = fps.as_f32() as f64 * sub.max(1) as f64;
            tempo.unwrap_or(120.0) / 60.0 / ticks_per_sec
        }
    };
    let bar = match time_signature {
        Some((n, d)) => n as f64 * 4.0 / d as f64,
        None => 4.0,
    };
    let split_channels = smf.header.format == Format::SingleTrack;

    let mut tracks = Vec::new();
    for (ti, t) in smf.tracks.iter().enumerate() {
        let mut name = String::new();
        // Per channel: notes, and open notes per key (FIFO of start tick, velocity).
        let mut notes: [Vec<(u64, u64, u8, u8)>; 16] = Default::default();
        let mut open: Vec<Vec<(u64, u8)>> = vec![Vec::new(); 16 * 128];
        let mut tick = 0u64;
        for e in t {
            tick += e.delta.as_int() as u64;
            match e.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(n)) if name.is_empty() => {
                    name = String::from_utf8_lossy(n).trim().to_string();
                }
                TrackEventKind::Meta(MetaMessage::InstrumentName(n)) if name.is_empty() => {
                    name = String::from_utf8_lossy(n).trim().to_string();
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int() as usize;
                    let (key, on_vel) = match message {
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => (key.as_int(), Some(vel.as_int())),
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => (key.as_int(), None),
                        _ => continue,
                    };
                    let slot = &mut open[ch * 128 + key as usize];
                    match on_vel {
                        Some(v) => slot.push((tick, v)),
                        None => {
                            if !slot.is_empty() {
                                let (start, v) = slot.remove(0);
                                notes[ch].push((start, tick, key, v));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        // Notes still held at the end of the track end there.
        for (i, slot) in open.iter().enumerate() {
            for &(start, v) in slot {
                notes[i / 128].push((start, tick.max(start + 1), (i % 128) as u8, v));
            }
        }

        let to_track = |raw: Vec<(u64, u64, u8, u8)>, name: String| {
            let mut notes: Vec<Note> = raw
                .into_iter()
                .map(|(s, e, pitch, velocity)| Note {
                    pitch,
                    velocity,
                    start: s as f64 * beats_per_tick,
                    length: (e.saturating_sub(s)).max(1) as f64 * beats_per_tick,
                })
                .collect();
            notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.pitch.cmp(&b.pitch)));
            let end = notes.iter().map(|n| n.start + n.length).fold(0.0f64, f64::max);
            let length = ((end / bar - 1e-9).ceil().max(1.0)) * bar;
            ImportedTrack { name, notes, length }
        };

        let used: Vec<usize> = (0..16).filter(|&c| !notes[c].is_empty()).collect();
        if used.is_empty() {
            continue;
        }
        let base = if name.is_empty() {
            format!("Track {}", ti + 1)
        } else {
            name
        };
        if split_channels && used.len() > 1 {
            for c in used {
                let raw = std::mem::take(&mut notes[c]);
                tracks.push(to_track(raw, format!("{base} (Ch {})", c + 1)));
            }
        } else {
            let raw: Vec<_> = notes.iter_mut().flat_map(std::mem::take).collect();
            tracks.push(to_track(raw, base));
        }
    }

    Ok(ImportedMidi {
        tempo,
        time_signature,
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ClipContent;

    fn clip(notes: Vec<Note>, length: f64) -> Clip {
        Clip {
            name: "Lead Line".into(),
            length,
            content: ClipContent::Midi { notes },
            ..Default::default()
        }
    }

    fn n(pitch: u8, start: f64, length: f64, velocity: u8) -> Note {
        Note {
            pitch,
            velocity,
            start,
            length,
        }
    }

    #[test]
    fn export_import_round_trip() {
        let notes = vec![
            n(60, 0.0, 1.0, 100),
            n(64, 0.0, 0.5, 90),
            n(67, 1.333, 0.25, 127),
            // Same pitch re-struck right as the previous one ends.
            n(60, 1.0, 0.5, 70),
            n(72, 6.75, 1.0, 1),
        ];
        let bytes = export_clip(&clip(notes.clone(), 8.0), 128.0, 6, 8);
        assert_eq!(&bytes[..4], b"MThd");
        let m = import(&bytes).unwrap();
        assert!((m.tempo.unwrap() - 128.0).abs() < 0.01);
        assert_eq!(m.time_signature, Some((6, 8)));
        assert_eq!(m.tracks.len(), 1);
        let t = &m.tracks[0];
        assert_eq!(t.name, "Lead Line");
        assert_eq!(t.notes.len(), notes.len());
        let tick = 1.0 / EXPORT_PPQ as f64;
        for want in &notes {
            let got = t
                .notes
                .iter()
                .find(|g| g.pitch == want.pitch && (g.start - want.start).abs() <= tick)
                .unwrap_or_else(|| panic!("missing {want:?}"));
            assert_eq!(got.velocity, want.velocity);
            assert!((got.length - want.length).abs() <= tick, "{got:?} vs {want:?}");
        }
        // 6/8 bars are 3 beats; notes end at 7.75 → 9 beats.
        assert_eq!(t.length, 9.0);
    }

    #[test]
    fn empty_clip_imports_as_no_tracks() {
        let bytes = export_clip(&clip(Vec::new(), 4.0), 120.0, 4, 4);
        let m = import(&bytes).unwrap();
        assert_eq!(m.tempo, Some(120.0));
        assert_eq!(m.time_signature, Some((4, 4)));
        assert!(m.tracks.is_empty());
    }

    fn ev(delta: u32, kind: TrackEventKind) -> TrackEvent {
        TrackEvent {
            delta: u28::new(delta),
            kind,
        }
    }

    fn on(ch: u8, key: u8, vel: u8) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: u4::new(ch),
            message: MidiMessage::NoteOn {
                key: u7::new(key),
                vel: u7::new(vel),
            },
        }
    }

    fn off(ch: u8, key: u8) -> TrackEventKind<'static> {
        TrackEventKind::Midi {
            channel: u4::new(ch),
            message: MidiMessage::NoteOff {
                key: u7::new(key),
                vel: u7::new(0),
            },
        }
    }

    fn eot() -> TrackEventKind<'static> {
        TrackEventKind::Meta(MetaMessage::EndOfTrack)
    }

    fn write(smf: &Smf) -> Vec<u8> {
        let mut v = Vec::new();
        smf.write_std(&mut v).unwrap();
        v
    }

    #[test]
    fn imports_multi_track_format_1() {
        // 96 ppq; tempo track, a drum track, an empty track and a bass track
        // that uses note-on velocity 0 as note-off.
        let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(96))));
        smf.tracks.push(vec![
            ev(0, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000)))),
            ev(0, TrackEventKind::Meta(MetaMessage::TimeSignature(3, 2, 24, 8))),
            ev(0, eot()),
        ]);
        smf.tracks.push(vec![
            ev(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Drums"))),
            ev(0, on(9, 36, 100)),
            ev(24, off(9, 36)),
            ev(72, on(9, 38, 80)),
            ev(48, off(9, 38)),
            ev(0, eot()),
        ]);
        smf.tracks.push(vec![
            ev(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Empty"))),
            ev(0, eot()),
        ]);
        smf.tracks.push(vec![
            ev(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Bass"))),
            ev(192, on(1, 40, 90)),
            ev(288, on(1, 40, 0)),
            ev(0, eot()),
        ]);
        let m = import(&write(&smf)).unwrap();
        assert_eq!(m.tempo, Some(120.0));
        assert_eq!(m.time_signature, Some((3, 4)));
        assert_eq!(m.tracks.len(), 2);
        let d = &m.tracks[0];
        assert_eq!(d.name, "Drums");
        assert_eq!(d.notes, vec![n(36, 0.0, 0.25, 100), n(38, 1.0, 0.5, 80)]);
        assert_eq!(d.length, 3.0);
        let b = &m.tracks[1];
        assert_eq!(b.name, "Bass");
        assert_eq!(b.notes, vec![n(40, 2.0, 3.0, 90)]);
        assert_eq!(b.length, 6.0);
    }

    #[test]
    fn format_0_is_split_by_channel() {
        let mut smf = Smf::new(Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))));
        smf.tracks.push(vec![
            ev(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Song"))),
            ev(0, on(0, 60, 100)),
            ev(0, on(2, 48, 100)),
            ev(480, off(0, 60)),
            // Never released: ends with the track.
            ev(0, on(0, 62, 50)),
            ev(960, off(2, 48)),
            ev(0, eot()),
        ]);
        let m = import(&write(&smf)).unwrap();
        assert_eq!(m.tempo, None);
        assert_eq!(m.tracks.len(), 2);
        assert_eq!(m.tracks[0].name, "Song (Ch 1)");
        assert_eq!(m.tracks[0].notes, vec![n(60, 0.0, 1.0, 100), n(62, 1.0, 2.0, 50)]);
        assert_eq!(m.tracks[1].name, "Song (Ch 3)");
        assert_eq!(m.tracks[1].notes, vec![n(48, 0.0, 3.0, 100)]);
        assert_eq!(m.tracks[1].length, 4.0);
    }

    #[test]
    fn rejects_garbage() {
        assert!(import(b"not midi").is_err());
    }
}
