//! Encoding rendered audio to files: WAV, AIFF, FLAC and (through a hook)
//! MP3, with optional dither and normalisation, like Live's Export Audio
//! dialog. Runs off the audio thread and may allocate.

use std::io::{self, Write};
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// File format of an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioFormat {
    Wav,
    Aiff,
    Flac,
    Mp3,
}

impl AudioFormat {
    pub const ALL: [AudioFormat; 4] = [AudioFormat::Wav, AudioFormat::Aiff, AudioFormat::Flac, AudioFormat::Mp3];

    pub fn label(self) -> &'static str {
        match self {
            AudioFormat::Wav => "WAV",
            AudioFormat::Aiff => "AIFF",
            AudioFormat::Flac => "FLAC",
            AudioFormat::Mp3 => "MP3",
        }
    }

    /// File extension without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            AudioFormat::Wav => "wav",
            AudioFormat::Aiff => "aif",
            AudioFormat::Flac => "flac",
            AudioFormat::Mp3 => "mp3",
        }
    }

    /// Whether the bit depth setting applies (MP3 ignores it).
    pub fn has_bit_depth(self) -> bool {
        self != AudioFormat::Mp3
    }
}

/// Sample format of PCM output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BitDepth {
    Int16,
    Int24,
    Float32,
}

impl BitDepth {
    pub const ALL: [BitDepth; 3] = [BitDepth::Int16, BitDepth::Int24, BitDepth::Float32];

    pub fn label(self) -> &'static str {
        match self {
            BitDepth::Int16 => "16-bit",
            BitDepth::Int24 => "24-bit",
            BitDepth::Float32 => "32-bit float",
        }
    }

    pub fn bits(self) -> u16 {
        match self {
            BitDepth::Int16 => 16,
            BitDepth::Int24 => 24,
            BitDepth::Float32 => 32,
        }
    }
}

/// Dither added when reducing to an integer bit depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dither {
    None,
    /// TPDF (triangular) dither of ±1 LSB.
    Triangular,
    /// TPDF dither with second-order error-feedback noise shaping, pushing
    /// the noise towards high frequencies.
    NoiseShaped,
}

impl Dither {
    pub const ALL: [Dither; 3] = [Dither::None, Dither::Triangular, Dither::NoiseShaped];

    pub fn label(self) -> &'static str {
        match self {
            Dither::None => "No Dither",
            Dither::Triangular => "Triangular",
            Dither::NoiseShaped => "Noise Shaped",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EncodeSettings {
    pub format: AudioFormat,
    /// Sample rate of `samples` (rendering already ran at this rate).
    pub sample_rate: u32,
    pub bit_depth: BitDepth,
    pub dither: Dither,
    /// Scale so the peak sits at 0 dBFS before encoding.
    pub normalize: bool,
    pub mp3_bitrate_kbps: u32,
}

impl Default for EncodeSettings {
    fn default() -> Self {
        Self {
            format: AudioFormat::Wav,
            sample_rate: 44100,
            bit_depth: BitDepth::Int24,
            dither: Dither::Triangular,
            normalize: false,
            mp3_bitrate_kbps: 320,
        }
    }
}

/// An MP3 encoder: interleaved stereo samples, sample rate, bitrate in kbps.
pub type Mp3Encoder = fn(&[f32], u32, u32) -> Result<Vec<u8>, String>;

/// MP3 encoder installed by the application. MP3 export fails with
/// [`io::ErrorKind::Unsupported`] while it is unset.
pub static MP3_ENCODER: OnceLock<Mp3Encoder> = OnceLock::new();

/// Scales `samples` so the absolute peak is 1.0 (0 dBFS, as Live does).
/// Returns the gain applied (1.0 for silence).
pub fn normalize(samples: &mut [f32]) -> f32 {
    let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    if peak <= 1e-9 || !peak.is_finite() {
        return 1.0;
    }
    let gain = 1.0 / peak;
    for s in samples.iter_mut() {
        *s *= gain;
    }
    gain
}

/// Encodes interleaved stereo `samples` into `path`.
pub fn encode_file(path: &Path, samples: &[f32], s: &EncodeSettings) -> io::Result<()> {
    let mut owned;
    let samples = if s.normalize {
        owned = samples.to_vec();
        normalize(&mut owned);
        &owned[..]
    } else {
        samples
    };
    let frames = samples.len() / 2;
    let samples = &samples[..frames * 2];
    match s.format {
        AudioFormat::Wav => write_wav(path, samples, s),
        AudioFormat::Aiff => {
            let bytes = encode_aiff(samples, s.sample_rate, s.bit_depth, s.dither);
            std::fs::write(path, bytes)
        }
        AudioFormat::Flac => {
            let bytes = encode_flac(samples, s.sample_rate, s.bit_depth, s.dither)?;
            std::fs::write(path, bytes)
        }
        AudioFormat::Mp3 => {
            let Some(enc) = MP3_ENCODER.get() else {
                return Err(io::Error::new(io::ErrorKind::Unsupported, "MP3 encoder not available"));
            };
            let bytes = enc(samples, s.sample_rate, s.mp3_bitrate_kbps).map_err(io::Error::other)?;
            std::fs::write(path, bytes)
        }
    }
}

fn write_wav(path: &Path, samples: &[f32], s: &EncodeSettings) -> io::Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: s.sample_rate,
        bits_per_sample: s.bit_depth.bits(),
        sample_format: if s.bit_depth == BitDepth::Float32 {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let to_io = |e: hound::Error| match e {
        hound::Error::IoError(e) => e,
        e => io::Error::other(e.to_string()),
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(to_io)?;
    if s.bit_depth == BitDepth::Float32 {
        for &x in samples {
            w.write_sample(x).map_err(to_io)?;
        }
    } else {
        for x in quantize(samples, s.bit_depth.bits() as u32, s.dither) {
            w.write_sample(x).map_err(to_io)?;
        }
    }
    w.finalize().map_err(to_io)
}

/// Converts interleaved stereo floats to integers of `bits` bits, with dither.
pub fn quantize(samples: &[f32], bits: u32, dither: Dither) -> Vec<i32> {
    let scale = (1i64 << (bits - 1)) as f64;
    let (lo, hi) = (-scale, scale - 1.0);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    // Error-feedback state per channel: last two quantisation errors.
    let mut err = [[0.0f64; 2]; 2];
    let mut out = Vec::with_capacity(samples.len());
    for (i, &x) in samples.iter().enumerate() {
        let ch = i & 1;
        let x = if x.is_finite() { x as f64 * scale } else { 0.0 };
        let v = match dither {
            Dither::NoiseShaped => x - 2.0 * err[ch][0] + err[ch][1],
            _ => x,
        };
        let d = match dither {
            Dither::None => 0.0,
            _ => rng.uniform() - rng.uniform(),
        };
        let y = (v + d).round().clamp(lo, hi);
        if dither == Dither::NoiseShaped {
            // Bounded so clipping cannot make the feedback loop run away.
            let e = (y - v).clamp(-4.0, 4.0);
            err[ch] = [e, err[ch][0]];
        }
        out.push(y as i32);
    }
    out
}

struct Rng(u64);

impl Rng {
    /// Uniform in [0, 1).
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// 80-bit IEEE 754 extended precision, big-endian (AIFF sample rate).
fn extended(value: f64) -> [u8; 10] {
    let mut out = [0u8; 10];
    if value <= 0.0 || !value.is_finite() {
        return out;
    }
    let mut exp = value.log2().floor() as i32;
    let mut mant = (value / 2f64.powi(exp) * (1u64 << 63) as f64) as u64;
    if mant >> 63 == 0 {
        // Rounding brought the value just under 2^exp.
        exp -= 1;
        mant = (value / 2f64.powi(exp) * (1u64 << 63) as f64) as u64;
    }
    let e = (exp + 16383) as u16;
    out[..2].copy_from_slice(&e.to_be_bytes());
    out[2..].copy_from_slice(&mant.to_be_bytes());
    out
}

/// Builds an AIFF file (AIFF-C with `fl32` for 32-bit float).
pub fn encode_aiff(samples: &[f32], sample_rate: u32, depth: BitDepth, dither: Dither) -> Vec<u8> {
    let frames = (samples.len() / 2) as u32;
    let float = depth == BitDepth::Float32;
    let bytes_per = depth.bits() as usize / 8;
    let mut data = Vec::with_capacity(samples.len() * bytes_per);
    if float {
        for &x in samples {
            data.extend_from_slice(&x.to_be_bytes());
        }
    } else {
        for v in quantize(samples, depth.bits() as u32, dither) {
            let b = v.to_be_bytes();
            data.extend_from_slice(&b[4 - bytes_per..]);
        }
    }
    if data.len() % 2 == 1 {
        data.push(0);
    }

    let mut comm = Vec::new();
    comm.extend_from_slice(&2i16.to_be_bytes());
    comm.extend_from_slice(&frames.to_be_bytes());
    comm.extend_from_slice(&(depth.bits() as i16).to_be_bytes());
    comm.extend_from_slice(&extended(sample_rate as f64));
    if float {
        comm.extend_from_slice(b"fl32");
        let name = b"32-bit floating point";
        comm.push(name.len() as u8);
        comm.extend_from_slice(name);
        if (name.len() + 1) % 2 == 1 {
            comm.push(0);
        }
    }

    let mut body = Vec::with_capacity(data.len() + 64);
    body.extend_from_slice(if float { b"AIFC" } else { b"AIFF" });
    if float {
        body.extend_from_slice(b"FVER");
        body.extend_from_slice(&4u32.to_be_bytes());
        body.extend_from_slice(&0xA280_5140u32.to_be_bytes());
    }
    body.extend_from_slice(b"COMM");
    body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    body.extend_from_slice(&comm);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&((8 + samples.len() * bytes_per) as u32).to_be_bytes());
    body.extend_from_slice(&0u32.to_be_bytes()); // offset
    body.extend_from_slice(&0u32.to_be_bytes()); // block size
    body.extend_from_slice(&data);

    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"FORM");
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Encodes FLAC (16 or 24 bit; 32-bit float is written as 24-bit).
pub fn encode_flac(samples: &[f32], sample_rate: u32, depth: BitDepth, dither: Dither) -> io::Result<Vec<u8>> {
    use flacenc::component::BitRepr;
    use flacenc::error::Verify;

    let bits = match depth {
        BitDepth::Int16 => 16,
        BitDepth::Int24 | BitDepth::Float32 => 24,
    };
    let ints = quantize(samples, bits, dither);
    let config = flacenc::config::Encoder::default()
        .into_verified()
        .map_err(|(_, e)| io::Error::other(format!("{e:?}")))?;
    let source = flacenc::source::MemSource::from_samples(&ints, 2, bits as usize, sample_rate as usize);
    let stream = flacenc::encode_with_fixed_block_size(&config, source, config.block_size)
        .map_err(|e| io::Error::other(format!("FLAC encoding failed: {e:?}")))?;
    let mut sink = flacenc::bitsink::ByteSink::new();
    stream
        .write(&mut sink)
        .map_err(|e| io::Error::other(format!("FLAC encoding failed: {e:?}")))?;
    let mut out = Vec::new();
    out.write_all(sink.as_slice())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frames: usize, amp: f32) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = (i as f32 * 0.05).sin() * amp;
            v.push(s);
            v.push(-s);
        }
        v
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("tessera_export_{}_{name}", std::process::id()))
    }

    #[test]
    fn normalize_scales_peak_to_unity() {
        let mut v = vec![0.25, -0.5, 0.1, 0.0];
        let g = normalize(&mut v);
        assert!((g - 2.0).abs() < 1e-6);
        assert_eq!(v[1], -1.0);
        let mut silent = vec![0.0; 8];
        assert_eq!(normalize(&mut silent), 1.0);
    }

    #[test]
    fn wav_round_trips_through_hound() {
        let input = sine(1000, 0.5);
        for depth in BitDepth::ALL {
            let path = tmp(&format!("{depth:?}.wav"));
            let s = EncodeSettings {
                bit_depth: depth,
                dither: Dither::None,
                sample_rate: 48000,
                ..Default::default()
            };
            encode_file(&path, &input, &s).unwrap();
            let mut r = hound::WavReader::open(&path).unwrap();
            let spec = r.spec();
            assert_eq!(spec.channels, 2);
            assert_eq!(spec.sample_rate, 48000);
            assert_eq!(spec.bits_per_sample, depth.bits());
            let decoded: Vec<f32> = match depth {
                BitDepth::Float32 => r.samples::<f32>().map(|x| x.unwrap()).collect(),
                _ => {
                    let scale = (1i64 << (depth.bits() - 1)) as f32;
                    r.samples::<i32>().map(|x| x.unwrap() as f32 / scale).collect()
                }
            };
            assert_eq!(decoded.len(), input.len());
            let tol = match depth {
                BitDepth::Int16 => 1.0 / 32000.0,
                BitDepth::Int24 => 1e-6,
                BitDepth::Float32 => 0.0,
            };
            for (a, b) in decoded.iter().zip(&input) {
                assert!((a - b).abs() <= tol, "{depth:?}: {a} vs {b}");
            }
            std::fs::remove_file(&path).ok();
        }
    }

    #[test]
    fn dither_stays_within_a_few_lsb() {
        let input = sine(4000, 0.3);
        for d in [Dither::Triangular, Dither::NoiseShaped] {
            let q = quantize(&input, 16, d);
            let mut diff_sum = 0.0;
            for (y, x) in q.iter().zip(&input) {
                let e = *y as f64 - *x as f64 * 32768.0;
                assert!(e.abs() <= 6.0, "{d:?} error {e}");
                diff_sum += e;
            }
            // Dither is zero-mean.
            assert!((diff_sum / q.len() as f64).abs() < 0.2);
            // It actually adds noise.
            assert_ne!(q, quantize(&input, 16, Dither::None));
        }
        // Silence stays silent without dither.
        assert!(quantize(&[0.0; 64], 24, Dither::None).iter().all(|&v| v == 0));
        // Full scale clips to the integer range.
        assert_eq!(quantize(&[1.0, -1.0], 16, Dither::None), vec![32767, -32768]);
    }

    #[test]
    fn extended_encodes_common_rates() {
        // 44100 Hz is 0x400E AC44 0000 0000 0000 in 80-bit extended.
        assert_eq!(extended(44100.0), [0x40, 0x0E, 0xAC, 0x44, 0, 0, 0, 0, 0, 0]);
        assert_eq!(extended(48000.0), [0x40, 0x0E, 0xBB, 0x80, 0, 0, 0, 0, 0, 0]);
        assert_eq!(extended(1.0), [0x3F, 0xFF, 0x80, 0, 0, 0, 0, 0, 0, 0]);
    }

    fn be32(b: &[u8]) -> u32 {
        u32::from_be_bytes([b[0], b[1], b[2], b[3]])
    }

    #[test]
    fn aiff_structure() {
        let input = sine(101, 0.5);
        let a = encode_aiff(&input, 44100, BitDepth::Int24, Dither::None);
        assert_eq!(&a[..4], b"FORM");
        assert_eq!(be32(&a[4..]) as usize, a.len() - 8);
        assert_eq!(&a[8..12], b"AIFF");
        assert_eq!(&a[12..16], b"COMM");
        assert_eq!(be32(&a[16..]), 18);
        assert_eq!(&a[20..22], &2i16.to_be_bytes());
        assert_eq!(be32(&a[22..]), 101);
        assert_eq!(&a[26..28], &24i16.to_be_bytes());
        assert_eq!(&a[28..38], &extended(44100.0));
        assert_eq!(&a[38..42], b"SSND");
        assert_eq!(be32(&a[42..]) as usize, 8 + 101 * 2 * 3);
        // 101 * 6 bytes is even, so no pad byte.
        assert_eq!(a.len(), 54 + 101 * 6);
        // First right sample is the negated first left sample (0 here),
        // second frame left sample is sin(0.05) * 0.5.
        let v = i32::from_be_bytes([a[54 + 6], a[55 + 6], a[56 + 6], 0]) >> 8;
        assert_eq!(v, ((0.05f32.sin() * 0.5) as f64 * 8388608.0).round() as i32);

        let f = encode_aiff(&input, 48000, BitDepth::Float32, Dither::Triangular);
        assert_eq!(&f[8..12], b"AIFC");
        assert_eq!(&f[12..16], b"FVER");
        assert_eq!(&f[24..28], b"COMM");
        let comm_len = be32(&f[28..]) as usize;
        assert_eq!(comm_len % 2, 0);
        assert_eq!(&f[32 + 18..32 + 22], b"fl32");
        let ssnd = 32 + comm_len;
        assert_eq!(&f[ssnd..ssnd + 4], b"SSND");
        let first = ssnd + 16;
        let x = f32::from_be_bytes([f[first + 8], f[first + 9], f[first + 10], f[first + 11]]);
        assert_eq!(x, input[2]);
        assert_eq!(be32(&f[4..]) as usize, f.len() - 8);

        // Odd-length data gets a pad byte that the chunk size leaves out.
        let o = encode_aiff(&sine(1, 0.1), 44100, BitDepth::Int24, Dither::None);
        assert_eq!(be32(&o[42..]), 14);
        assert_eq!(o.len(), 54 + 6);
        let o16 = encode_aiff(&sine(3, 0.1), 44100, BitDepth::Int16, Dither::None);
        assert_eq!(o16.len(), 54 + 12);
    }

    #[test]
    fn flac_structure() {
        let input = sine(10000, 0.5);
        for (depth, bits) in [(BitDepth::Int16, 16u8), (BitDepth::Int24, 24), (BitDepth::Float32, 24)] {
            let f = encode_flac(&input, 44100, depth, Dither::Triangular).unwrap();
            assert_eq!(&f[..4], b"fLaC");
            // STREAMINFO: block header type 0, length 34.
            assert_eq!(f[4] & 0x7F, 0);
            assert_eq!(&f[5..8], &[0, 0, 34]);
            let info = &f[8..42];
            let packed = u64::from_be_bytes(info[10..18].try_into().unwrap());
            let rate = (packed >> 44) as u32;
            let channels = ((packed >> 41) & 7) as u8 + 1;
            let bps = ((packed >> 36) & 31) as u8 + 1;
            let total = packed & ((1 << 36) - 1);
            assert_eq!(rate, 44100);
            assert_eq!(channels, 2);
            assert_eq!(bps, bits);
            assert_eq!(total, 10000);
            // Compressed smaller than raw PCM.
            assert!(f.len() < 10000 * 2 * bits as usize / 8);
        }
    }

    #[test]
    fn mp3_without_encoder_is_unsupported_and_files_are_written() {
        let input = sine(500, 0.25);
        if MP3_ENCODER.get().is_none() {
            let path = tmp("x.mp3");
            let s = EncodeSettings {
                format: AudioFormat::Mp3,
                ..Default::default()
            };
            let err = encode_file(&path, &input, &s).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        }
        for format in [AudioFormat::Aiff, AudioFormat::Flac] {
            let path = tmp(&format!("x.{}", format.extension()));
            let s = EncodeSettings {
                format,
                normalize: true,
                ..Default::default()
            };
            encode_file(&path, &input, &s).unwrap();
            assert!(std::fs::metadata(&path).unwrap().len() > 100);
            std::fs::remove_file(&path).ok();
        }
    }
}
