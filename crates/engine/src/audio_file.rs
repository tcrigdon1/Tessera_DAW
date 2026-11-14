//! Decoding audio files: WAV, AIFF, FLAC, MP3 and Ogg Vorbis.
//!
//! [`AudioData::load`](crate::model::AudioData::load) picks the decoder from
//! the file's first bytes (so a mislabelled file still opens), falling back to
//! the extension. Everything comes back as 32-bit float stereo; mono files
//! are copied to both sides and channels past the second are dropped.

use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use crate::model::AudioData;

/// File extensions Tessera can import, lower case and without the dot.
pub const EXTENSIONS: &[&str] = &["wav", "wave", "aif", "aiff", "aifc", "flac", "mp3", "ogg", "oga"];

/// The formats in plain words, for messages.
pub const FORMAT_NAMES: &str = "WAV, AIFF, FLAC, MP3 and Ogg Vorbis";

/// True when the path's extension is one Tessera can import.
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Wav,
    Aiff,
    Flac,
    Mp3,
    Vorbis,
}

impl Format {
    pub fn name(self) -> &'static str {
        match self {
            Format::Wav => "WAV",
            Format::Aiff => "AIFF",
            Format::Flac => "FLAC",
            Format::Mp3 => "MP3",
            Format::Vorbis => "Ogg Vorbis",
        }
    }
}

/// Why a file couldn't be imported.
#[derive(Debug)]
pub enum AudioLoadError {
    Io(std::io::Error),
    /// Not a format Tessera reads (the extension, e.g. "m4a", if any).
    Unsupported(String),
    /// The file looked like `format` but couldn't be decoded.
    Decode(Format, String),
    /// The file decoded but holds no audio.
    Empty,
}

impl fmt::Display for AudioLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioLoadError::Io(e) => write!(f, "{e}"),
            AudioLoadError::Unsupported(ext) if ext.is_empty() => {
                write!(f, "this isn't an audio file Tessera can read ({FORMAT_NAMES})")
            }
            AudioLoadError::Unsupported(ext) => write!(
                f,
                ".{ext} files aren't supported yet. Tessera can import {FORMAT_NAMES}"
            ),
            AudioLoadError::Decode(format, e) => write!(f, "the {} file is damaged or unusual ({e})", format.name()),
            AudioLoadError::Empty => write!(f, "the file holds no audio"),
        }
    }
}

impl std::error::Error for AudioLoadError {}

impl From<std::io::Error> for AudioLoadError {
    fn from(e: std::io::Error) -> Self {
        AudioLoadError::Io(e)
    }
}

impl From<hound::Error> for AudioLoadError {
    fn from(e: hound::Error) -> Self {
        match e {
            hound::Error::IoError(e) => AudioLoadError::Io(e),
            e => AudioLoadError::Decode(Format::Wav, e.to_string()),
        }
    }
}

/// Recognises a format from the first bytes of a file.
pub fn sniff(head: &[u8]) -> Option<Format> {
    let at = |i: usize, tag: &[u8]| head.get(i..i + tag.len()) == Some(tag);
    if (at(0, b"RIFF") || at(0, b"RIFX") || at(0, b"RF64")) && at(8, b"WAVE") {
        Some(Format::Wav)
    } else if at(0, b"FORM") && (at(8, b"AIFF") || at(8, b"AIFC")) {
        Some(Format::Aiff)
    } else if at(0, b"fLaC") {
        Some(Format::Flac)
    } else if at(0, b"OggS") {
        Some(Format::Vorbis)
    } else if at(0, b"ID3") || (head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0) {
        Some(Format::Mp3)
    } else {
        None
    }
}

fn format_from_extension(path: &Path) -> Option<Format> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "wav" | "wave" => Format::Wav,
        "aif" | "aiff" | "aifc" => Format::Aiff,
        "flac" => Format::Flac,
        "mp3" => Format::Mp3,
        "ogg" | "oga" => Format::Vorbis,
        _ => return None,
    })
}

/// Decodes any supported audio file.
pub fn load(path: &Path) -> Result<AudioData, AudioLoadError> {
    let mut file = File::open(path)?;
    let mut head = [0u8; 12];
    let n = read_up_to(&mut file, &mut head)?;
    file.rewind()?;
    let format = match sniff(&head[..n]).or_else(|| format_from_extension(path)) {
        Some(f) => f,
        None => {
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            return Err(AudioLoadError::Unsupported(ext));
        }
    };
    let reader = BufReader::new(file);
    let data = match format {
        Format::Wav => wav(reader)?,
        Format::Aiff => aiff(reader)?,
        Format::Flac => flac(reader)?,
        Format::Mp3 => mp3(reader)?,
        Format::Vorbis => vorbis(reader)?,
    };
    if data.frames() == 0 {
        return Err(AudioLoadError::Empty);
    }
    Ok(data)
}

fn read_up_to(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Splits interleaved samples into left and right, replacing NaN and
/// infinity (which would poison every effect after them) with silence.
pub(crate) fn deinterleave(samples: &[f32], channels: usize, sample_rate: u32) -> AudioData {
    let ch = channels.max(1);
    let frames = samples.len() / ch;
    let clean = |v: f32| if v.is_finite() { v } else { 0.0 };
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for f in samples.chunks_exact(ch) {
        let l = clean(f[0]);
        left.push(l);
        right.push(if ch > 1 { clean(f[1]) } else { l });
    }
    AudioData {
        sample_rate,
        left,
        right,
    }
}

fn check_rate(format: Format, rate: u32) -> Result<u32, AudioLoadError> {
    if rate == 0 {
        Err(AudioLoadError::Decode(format, "sample rate of zero".into()))
    } else {
        Ok(rate)
    }
}

fn wav<R: Read>(r: R) -> Result<AudioData, AudioLoadError> {
    let mut reader = hound::WavReader::new(r)?;
    let spec = reader.spec();
    let rate = check_rate(Format::Wav, spec.sample_rate)?;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.saturating_sub(1))) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    Ok(deinterleave(&samples, spec.channels as usize, rate))
}

fn flac<R: Read>(r: R) -> Result<AudioData, AudioLoadError> {
    let err = |e: claxon::Error| match e {
        claxon::Error::IoError(e) => AudioLoadError::Io(e),
        e => AudioLoadError::Decode(Format::Flac, e.to_string()),
    };
    let mut reader = claxon::FlacReader::new(r).map_err(err)?;
    let info = reader.streaminfo();
    let rate = check_rate(Format::Flac, info.sample_rate)?;
    let scale = 1.0 / (1u64 << info.bits_per_sample.saturating_sub(1)) as f32;
    let samples: Vec<f32> = reader
        .samples()
        .map(|s| s.map(|v| v as f32 * scale))
        .collect::<Result<_, _>>()
        .map_err(err)?;
    Ok(deinterleave(&samples, info.channels as usize, rate))
}

fn mp3<R: Read + Seek>(r: R) -> Result<AudioData, AudioLoadError> {
    let err = |e: nanomp3::Error| AudioLoadError::Decode(Format::Mp3, format!("{e:?}"));
    let mut reader = nanomp3::Reader::<_, f32>::new(r).map_err(err)?;
    let mut samples = Vec::new();
    let mut channels = 0;
    let mut rate = 0;
    while let Some(frame) = reader.read_frame().map_err(err)? {
        samples.extend_from_slice(frame);
        if channels == 0 {
            rate = reader.sample_rate();
            channels = reader.channels().map_or(2, |c| c as usize);
        }
    }
    if samples.is_empty() {
        return Err(AudioLoadError::Decode(Format::Mp3, "no MPEG audio frames found".into()));
    }
    let rate = check_rate(Format::Mp3, rate)?;
    Ok(deinterleave(&samples, channels, rate))
}

fn vorbis<R: Read + Seek>(r: R) -> Result<AudioData, AudioLoadError> {
    let err = |e: lewton::VorbisError| AudioLoadError::Decode(Format::Vorbis, e.to_string());
    let mut reader = lewton::inside_ogg::OggStreamReader::new(r).map_err(err)?;
    let channels = reader.ident_hdr.audio_channels as usize;
    let rate = check_rate(Format::Vorbis, reader.ident_hdr.audio_sample_rate)?;
    let mut samples = Vec::new();
    while let Some(packet) = reader
        .read_dec_packet_generic::<lewton::samples::InterleavedSamples<f32>>()
        .map_err(err)?
    {
        samples.extend_from_slice(&packet.samples);
    }
    Ok(deinterleave(&samples, channels, rate))
}

// ----- AIFF / AIFF-C ---------------------------------------------------------

fn aiff<R: Read>(mut r: R) -> Result<AudioData, AudioLoadError> {
    let bad = |m: &str| AudioLoadError::Decode(Format::Aiff, m.to_string());
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes)?;
    if bytes.len() < 12 || &bytes[0..4] != b"FORM" {
        return Err(bad("missing FORM header"));
    }
    let aifc = &bytes[8..12] == b"AIFC";
    let mut pos = 12;
    let mut comm: Option<(usize, u32, u16, f64, [u8; 4])> = None;
    let mut sound: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_be_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = &bytes[pos + 8..(pos + 8).saturating_add(len).min(bytes.len())];
        match id {
            b"COMM" if body.len() >= 18 => {
                let channels = u16::from_be_bytes([body[0], body[1]]) as usize;
                let frames = u32::from_be_bytes(body[2..6].try_into().unwrap());
                let bits = u16::from_be_bytes([body[6], body[7]]);
                let rate = extended_to_f64(body[8..18].try_into().unwrap());
                let compression = if aifc && body.len() >= 22 {
                    body[18..22].try_into().unwrap()
                } else {
                    *b"NONE"
                };
                comm = Some((channels, frames, bits, rate, compression));
            }
            b"SSND" if body.len() >= 8 => {
                let offset = u32::from_be_bytes(body[0..4].try_into().unwrap()) as usize;
                sound = Some(body.get(8 + offset..).unwrap_or(&[]));
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        pos = pos.saturating_add(8 + len + (len & 1));
    }
    let (channels, frames, bits, rate, compression) = comm.ok_or_else(|| bad("missing COMM chunk"))?;
    let data = sound.unwrap_or(&[]);
    if channels == 0 {
        return Err(bad("no channels"));
    }
    if !(1.0..=1.0e6).contains(&rate) {
        return Err(bad("bad sample rate"));
    }
    let little = match &compression {
        b"NONE" | b"twos" | b"in24" | b"in32" => false,
        b"sowt" => true,
        b"fl32" | b"FL32" => {
            let samples: Vec<f32> = data
                .chunks_exact(4)
                .map(|b| f32::from_be_bytes(b.try_into().unwrap()))
                .collect();
            return Ok(trim(deinterleave(&samples, channels, rate.round() as u32), frames));
        }
        b"fl64" | b"FL64" => {
            let samples: Vec<f32> = data
                .chunks_exact(8)
                .map(|b| f64::from_be_bytes(b.try_into().unwrap()) as f32)
                .collect();
            return Ok(trim(deinterleave(&samples, channels, rate.round() as u32), frames));
        }
        other => {
            return Err(bad(&format!(
                "compression \"{}\" isn't supported",
                String::from_utf8_lossy(other).trim()
            )))
        }
    };
    if bits == 0 || bits > 32 {
        return Err(bad("bad bit depth"));
    }
    let width = bits.div_ceil(8) as usize;
    let scale = 1.0 / (1u64 << (width * 8 - 1)) as f32;
    let samples: Vec<f32> = data
        .chunks_exact(width)
        .map(|b| {
            // Sign-extend the big- (or little-) endian integer from the top.
            let mut v: i32 = 0;
            for i in 0..width {
                let byte = if little { b[width - 1 - i] } else { b[i] };
                v = (v << 8) | byte as i32;
            }
            let shift = 32 - width * 8;
            ((v << shift) >> shift) as f32 * scale
        })
        .collect();
    Ok(trim(deinterleave(&samples, channels, rate.round() as u32), frames))
}

/// Keeps no more than the frame count the header promises.
fn trim(mut d: AudioData, frames: u32) -> AudioData {
    let n = frames as usize;
    if n > 0 && d.left.len() > n {
        d.left.truncate(n);
        d.right.truncate(n);
    }
    d
}

/// The 80-bit IEEE 754 extended float AIFF uses for its sample rate.
fn extended_to_f64(b: [u8; 10]) -> f64 {
    let sign = if b[0] & 0x80 != 0 { -1.0 } else { 1.0 };
    let exponent = (((b[0] & 0x7F) as i32) << 8) | b[1] as i32;
    let mantissa = u64::from_be_bytes(b[2..10].try_into().unwrap());
    if exponent == 0 && mantissa == 0 {
        return 0.0;
    }
    if exponent == 0x7FFF {
        return f64::NAN;
    }
    sign * mantissa as f64 * 2f64.powi(exponent - 16383 - 63)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{encode_aiff, encode_flac, BitDepth, Dither};

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tessera-audio-file-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn f64_to_extended(v: f64) -> [u8; 10] {
        // Enough for positive whole-number sample rates.
        let e = v.log2().floor() as i32;
        let mantissa = (v / 2f64.powi(e - 63)) as u64;
        let mut b = [0u8; 10];
        let exp = (e + 16383) as u16;
        b[0..2].copy_from_slice(&exp.to_be_bytes());
        b[2..10].copy_from_slice(&mantissa.to_be_bytes());
        b
    }

    fn aiff_bytes(samples: &[i16], channels: u16, rate: f64) -> Vec<u8> {
        let frames = samples.len() as u32 / channels as u32;
        let mut comm = Vec::new();
        comm.extend_from_slice(&channels.to_be_bytes());
        comm.extend_from_slice(&frames.to_be_bytes());
        comm.extend_from_slice(&16u16.to_be_bytes());
        comm.extend_from_slice(&f64_to_extended(rate));
        let mut ssnd = vec![0u8; 8];
        for s in samples {
            ssnd.extend_from_slice(&s.to_be_bytes());
        }
        let mut body = b"AIFF".to_vec();
        for (id, chunk) in [(b"COMM", &comm), (b"SSND", &ssnd)] {
            body.extend_from_slice(id);
            body.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
            body.extend_from_slice(chunk);
        }
        let mut out = b"FORM".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    #[test]
    fn extended_floats_round_trip() {
        for rate in [8000.0, 22050.0, 44100.0, 48000.0, 96000.0, 192000.0] {
            assert_eq!(extended_to_f64(f64_to_extended(rate)), rate);
        }
    }

    #[test]
    fn reads_aiff() {
        let path = tmp("stereo.aiff");
        std::fs::write(&path, aiff_bytes(&[16384, -16384, 0, 32767], 2, 48000.0)).unwrap();
        let d = load(&path).unwrap();
        assert_eq!(d.sample_rate, 48000);
        assert_eq!(d.left, [0.5, 0.0]);
        assert_eq!(d.right[0], -0.5);
        assert!((d.right[1] - 1.0).abs() < 1e-4);
    }

    #[test]
    fn reads_what_aiff_export_writes() {
        let stereo: Vec<f32> = (0..1000).flat_map(|i| [i as f32 / 2000.0, -0.25]).collect();
        for depth in BitDepth::ALL {
            let path = tmp(&format!("export-{}.aif", depth.bits()));
            std::fs::write(&path, encode_aiff(&stereo, 96000, depth, Dither::None)).unwrap();
            let d = load(&path).unwrap();
            assert_eq!((d.sample_rate, d.frames()), (96000, 1000), "{depth:?}");
            assert!((d.left[500] - 0.25).abs() < 1e-3, "{depth:?} {}", d.left[500]);
            assert!((d.right[999] + 0.25).abs() < 1e-3, "{depth:?}");
        }
    }

    #[test]
    fn reads_flac() {
        let path = tmp("tone.flac");
        let pcm: Vec<f32> = (0..4410).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let stereo: Vec<f32> = pcm.iter().flat_map(|&s| [s, -s]).collect();
        let bytes = encode_flac(&stereo, 44100, BitDepth::Int16, Dither::None).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let d = load(&path).unwrap();
        assert_eq!((d.sample_rate, d.frames()), (44100, 4410));
        for i in [0, 100, 4000] {
            assert!((d.left[i] - pcm[i]).abs() < 1e-3);
            assert!((d.right[i] + pcm[i]).abs() < 1e-3);
        }
    }

    #[test]
    fn reads_mp3_from_our_encoder() {
        let path = tmp("tone.mp3");
        let rate = 44100;
        let pcm: Vec<f32> = (0..rate)
            .flat_map(|i| {
                let s = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.5;
                [s, s]
            })
            .collect();
        let bytes = tessera_mp3::encode(&pcm, rate as u32, 192).unwrap();
        std::fs::write(&path, bytes).unwrap();
        let d = load(&path).unwrap();
        assert_eq!(d.sample_rate, 44100);
        assert!(d.frames() >= rate, "{}", d.frames());
        let peak = d.left.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((0.3..0.7).contains(&peak), "{peak}");
    }

    #[test]
    fn mislabelled_files_open_by_content() {
        let path = tmp("really-a-wav.mp3");
        crate::render::write_wav(&path, &[0.25, 0.25, -0.25, -0.25], 44100).unwrap();
        let d = load(&path).unwrap();
        assert_eq!(d.frames(), 2);
    }

    #[test]
    fn unknown_files_say_which_formats_work() {
        let path = tmp("song.m4a");
        std::fs::write(&path, b"\0\0\0 ftypM4A junk junk").unwrap();
        let e = load(&path).unwrap_err().to_string();
        assert!(e.contains(".m4a") && e.contains("MP3"), "{e}");
        let path = tmp("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert!(matches!(load(&path), Err(AudioLoadError::Unsupported(_))));
    }

    #[test]
    fn damaged_files_are_errors_not_panics() {
        for (name, head) in [
            ("a.wav", &b"RIFF\0\0\0\0WAVEfmt "[..]),
            ("a.aiff", b"FORM\0\0\0\x04AIFF"),
            ("a.flac", b"fLaC\0\0"),
            ("a.mp3", b"ID3\x04\0\0\0\0\0\x05xx"),
            ("a.ogg", b"OggS\0\x02"),
        ] {
            let path = tmp(name);
            std::fs::write(&path, head).unwrap();
            assert!(load(&path).is_err(), "{name}");
        }
    }
}
