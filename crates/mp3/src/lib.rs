//! `tessera-mp3`: a small, dependency-free MPEG-1 Layer III (MP3) CBR encoder.
//!
//! The design is deliberately simple (no psychoacoustic model):
//!
//! * 32-band polyphase analysis filterbank followed by an 18-line MDCT per
//!   subband (long blocks only) and the aliasing-reduction butterflies,
//! * joint stereo with per-frame mid/side switching,
//! * a rate loop that searches the global gain so that the Huffman-coded
//!   granule fits its share of the frame (all scalefactors are zero),
//! * big-values / count1 partitioning with exhaustive region and table choice,
//! * no bit reservoir (`main_data_begin` is always 0) and no CRC.
//!
//! The output is a plain sequence of MPEG audio frames (no ID3 or Xing tag).
//! Decoders see the audio delayed by [`DECODER_DELAY`] samples; the encoder
//! emits enough frames to cover the whole input including that delay.

mod bitstream;
mod filterbank;
mod quantize;
mod tables;

use bitstream::BitWriter;
use filterbank::{ChannelAnalyzer, GRANULE};
use quantize::{force_table, quantize_granule, write_granule, GranuleCode, MAX_PART23};
use tables::{BITRATES, SAMPLE_RATES, SFB_LONG};

/// Samples per channel in one MPEG-1 Layer III frame.
pub const FRAME_SAMPLES: usize = 1152;

/// Delay (in samples per channel) between the input and the output of a
/// standard decoder for streams produced by this encoder.
pub const DECODER_DELAY: usize = 481;

/// Sample rates accepted by [`encode`].
pub const SUPPORTED_SAMPLE_RATES: [u32; 3] = [32000, 44100, 48000];
/// Bitrates (kbps) accepted by [`encode`].
pub const SUPPORTED_BITRATES: [u32; 5] = [128, 160, 192, 256, 320];

/// Size in bytes of the frame header plus the MPEG-1 stereo side information.
const HEADER_AND_SIDE_BYTES: usize = 4 + 32;

/// Encodes interleaved stereo f32 samples (-1..1) to an MP3 byte stream (joint/normal stereo CBR).
pub fn encode(samples: &[f32], sample_rate: u32, bitrate_kbps: u32) -> Result<Vec<u8>, String> {
    encode_impl(samples, sample_rate, bitrate_kbps, bitrate_kbps, None)
}

/// Number of frames [`encode`] produces for `frames_in` stereo sample frames.
pub fn frame_count(frames_in: usize) -> usize {
    if frames_in == 0 {
        0
    } else {
        (frames_in + DECODER_DELAY).div_ceil(FRAME_SAMPLES)
    }
}

/// Testing hook used to verify every Huffman table against a decoder.
///
/// Quantises exactly as [`encode`] would at `quant_kbps`, but writes frames of
/// `bitrate_kbps` (which must be larger, to leave room) and, when `table` is
/// given, codes every region that table can represent with it. The decoded
/// audio must therefore be identical for every `table`.
#[doc(hidden)]
pub fn encode_testing_tables(
    samples: &[f32],
    sample_rate: u32,
    bitrate_kbps: u32,
    quant_kbps: u32,
    table: Option<u32>,
) -> Result<Vec<u8>, String> {
    if let Some(t) = table {
        if !(1..=31).contains(&t) || t == 4 || t == 14 {
            return Err(format!("invalid table {t}"));
        }
    }
    if !SUPPORTED_BITRATES.contains(&quant_kbps) || quant_kbps > bitrate_kbps {
        return Err(format!("invalid quantisation bitrate {quant_kbps}"));
    }
    encode_impl(samples, sample_rate, bitrate_kbps, quant_kbps, table)
}

/// Frame length in bytes for the given parameters and padding bit.
pub fn frame_bytes(sample_rate: u32, bitrate_kbps: u32, padding: bool) -> usize {
    (144_000 * bitrate_kbps / sample_rate) as usize + usize::from(padding)
}

fn encode_impl(
    samples: &[f32],
    sample_rate: u32,
    bitrate_kbps: u32,
    quant_kbps: u32,
    force: Option<u32>,
) -> Result<Vec<u8>, String> {
    let sr_index = match sample_rate {
        44100 => 0,
        48000 => 1,
        32000 => 2,
        _ => {
            return Err(format!(
                "unsupported sample rate {sample_rate} Hz (MPEG-1 supports 32000, 44100, 48000)"
            ))
        }
    };
    debug_assert_eq!(SAMPLE_RATES[sr_index], sample_rate);
    if !SUPPORTED_BITRATES.contains(&bitrate_kbps) {
        return Err(format!(
            "unsupported bitrate {bitrate_kbps} kbps (supported: 128, 160, 192, 256, 320)"
        ));
    }
    let br_index = BITRATES
        .iter()
        .position(|&b| b == bitrate_kbps)
        .expect("bitrate in table") as u32;
    if !samples.len().is_multiple_of(2) {
        return Err("interleaved stereo input must have an even number of samples".into());
    }
    let n = samples.len() / 2;
    let frames = frame_count(n);
    let sfb = &SFB_LONG[sr_index];

    let fetch = |ch: usize, granule: usize, out: &mut [f32; GRANULE]| {
        for (i, o) in out.iter_mut().enumerate() {
            let idx = granule * GRANULE + i;
            let v = if idx < n { samples[2 * idx + ch] } else { 0.0 };
            *o = if v.is_finite() { v.clamp(-1.0, 1.0) } else { 0.0 };
        }
    };

    let mut analyzers = [ChannelAnalyzer::new(), ChannelAnalyzer::new()];
    let mut buf = [0.0f32; GRANULE];
    for (ch, an) in analyzers.iter_mut().enumerate() {
        fetch(ch, 0, &mut buf);
        an.prime(&buf);
    }

    let slot_rem = (144_000 * bitrate_kbps) % sample_rate;
    let mut slot_acc = 0u32;
    let mut out = Vec::with_capacity(frames * (frame_bytes(sample_rate, bitrate_kbps, true)));
    // xr[granule][channel]
    let mut xr = [[[0.0f32; GRANULE]; 2]; 2];
    let mut granule_index = 0usize;
    let mut gain_hint: [Option<u32>; 2] = [None, None];
    for _ in 0..frames {
        // Padding: keep the average frame length exact (needed at 44.1 kHz).
        slot_acc += slot_rem;
        let padding = slot_acc >= sample_rate;
        if padding {
            slot_acc -= sample_rate;
        }
        let len = frame_bytes(sample_rate, bitrate_kbps, padding);

        for gr_xr in xr.iter_mut() {
            granule_index += 1;
            for (ch, an) in analyzers.iter_mut().enumerate() {
                fetch(ch, granule_index, &mut buf);
                an.granule(&buf, &mut gr_xr[ch]);
            }
        }

        // Mid/side decision for the whole frame.
        let (mut em, mut es) = (0.0f64, 0.0f64);
        for [left, right] in xr.iter() {
            for (&l, &r) in left.iter().zip(right.iter()) {
                let (l, r) = (l as f64, r as f64);
                em += (l + r) * (l + r);
                es += (l - r) * (l - r);
            }
        }
        let ms = em.min(es) < 0.25 * em.max(es);
        if ms {
            let s = std::f32::consts::FRAC_1_SQRT_2;
            for [left, right] in xr.iter_mut() {
                for (l, r) in left.iter_mut().zip(right.iter_mut()) {
                    let (a, b) = (*l, *r);
                    *l = (a + b) * s;
                    *r = (a - b) * s;
                }
            }
        }

        // Quantise, sharing the frame's main-data bits between the granules.
        let main_bits = if quant_kbps == bitrate_kbps {
            (len - HEADER_AND_SIDE_BYTES) * 8
        } else {
            (frame_bytes(sample_rate, quant_kbps, false) - HEADER_AND_SIDE_BYTES) * 8
        };
        let mut remaining = main_bits as u32;
        let mut codes: [[Option<GranuleCode>; 2]; 2] = [[None, None], [None, None]];
        for gr in 0..2 {
            let share = remaining / (2 - gr as u32);
            let energy = |ch: usize| xr[gr][ch].iter().map(|v| v * v).sum::<f32>();
            // Quantise the quieter channel first; the louder one gets what is left.
            let order = if energy(0) <= energy(1) { [0, 1] } else { [1, 0] };
            let mut left = share;
            for (k, &ch) in order.iter().enumerate() {
                let budget = if k == 0 { left / 2 } else { left }.min(MAX_PART23);
                let mut code = quantize_granule(&xr[gr][ch], budget, sfb, gain_hint[ch]);
                if code.part2_3_length > 0 {
                    gain_hint[ch] = Some(code.global_gain);
                }
                left -= code.part2_3_length;
                remaining -= code.part2_3_length;
                if let Some(t) = force {
                    force_table(&mut code, sfb, t);
                    if code.part2_3_length > MAX_PART23 {
                        return Err("forced table does not fit a granule".into());
                    }
                }
                codes[gr][ch] = Some(code);
            }
        }
        let codes = codes.map(|g| g.map(|c| c.expect("granule quantised")));

        let mut w = BitWriter::new();
        // Header
        w.put(0x7FF, 11);
        w.put(0b11, 2); // MPEG-1
        w.put(0b01, 2); // Layer III
        w.put(1, 1); // no CRC
        w.put(br_index, 4);
        w.put(sr_index as u32, 2);
        w.put(u32::from(padding), 1);
        w.put(0, 1); // private
        w.put(0b01, 2); // joint stereo
        w.put(if ms { 0b10 } else { 0b00 }, 2); // mode extension: MS on/off, intensity off
        w.put(0, 1); // copyright
        w.put(1, 1); // original
        w.put(0, 2); // emphasis
                     // Side information
        w.put(0, 9); // main_data_begin (no bit reservoir)
        w.put(0, 3); // private bits
        w.put(0, 8); // scfsi for both channels
        for gr in codes.iter() {
            for c in gr.iter() {
                w.put(c.part2_3_length, 12);
                w.put(c.big_values, 9);
                w.put(c.global_gain, 8);
                w.put(0, 4); // scalefac_compress
                w.put(0, 1); // window_switching_flag
                for t in c.table_select {
                    w.put(t, 5);
                }
                w.put(c.region0_count, 4);
                w.put(c.region1_count, 3);
                w.put(0, 1); // preflag
                w.put(0, 1); // scalefac_scale
                w.put(c.count1table_select, 1);
            }
        }
        debug_assert_eq!(w.bit_len(), HEADER_AND_SIDE_BYTES * 8);
        for gr in codes.iter() {
            for c in gr.iter() {
                write_granule(&mut w, c, sfb);
            }
        }
        let mut bytes = w.into_bytes();
        if bytes.len() > len {
            return Err("internal error: main data overflows the frame".into());
        }
        bytes.resize(len, 0);
        out.extend_from_slice(&bytes);
    }
    Ok(out)
}

/// Minimal parsed view of an MPEG-1 Layer III frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub padding: bool,
    /// Header mode field (0 stereo, 1 joint stereo, 2 dual channel, 3 mono).
    pub mode: u8,
    pub mode_extension: u8,
}

impl FrameHeader {
    /// Parses a 4-byte MPEG-1 Layer III header; returns `None` if it is not one.
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
            return None;
        }
        if (b[1] >> 3) & 3 != 3 || (b[1] >> 1) & 3 != 1 {
            return None;
        }
        let bri = (b[2] >> 4) as usize;
        let sri = ((b[2] >> 2) & 3) as usize;
        if bri == 0 || bri == 15 || sri == 3 {
            return None;
        }
        Some(Self {
            bitrate_kbps: BITRATES[bri],
            sample_rate: SAMPLE_RATES[sri],
            padding: (b[2] >> 1) & 1 == 1,
            mode: b[3] >> 6,
            mode_extension: (b[3] >> 4) & 3,
        })
    }

    /// Length of the frame in bytes.
    pub fn frame_len(&self) -> usize {
        frame_bytes(self.sample_rate, self.bitrate_kbps, self.padding)
    }
}

/// Splits an MP3 byte stream into frame headers by walking frame lengths.
/// Returns an error if a frame does not start with a valid header.
pub fn parse_frames(data: &[u8]) -> Result<Vec<FrameHeader>, String> {
    let mut pos = 0;
    let mut out = Vec::new();
    while pos < data.len() {
        let h = FrameHeader::parse(&data[pos..]).ok_or_else(|| format!("no frame header at byte {pos}"))?;
        pos += h.frame_len();
        if pos > data.len() {
            return Err("truncated final frame".into());
        }
        out.push(h);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(n: usize, sr: u32) -> Vec<f32> {
        let mut s = Vec::with_capacity(2 * n);
        let mut seed = 12345u32;
        for i in 0..n {
            let t = i as f32 / sr as f32;
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.05;
            let sine = 0.5 * (2.0 * std::f32::consts::PI * 1000.0 * t).sin();
            s.push(sine + noise);
            s.push(0.3 * (2.0 * std::f32::consts::PI * 440.0 * t).sin() - noise);
        }
        s
    }

    #[test]
    fn rejects_unsupported_parameters() {
        assert!(encode(&[0.0; 4], 22050, 128).is_err());
        assert!(encode(&[0.0; 4], 44100, 64).is_err());
        assert!(encode(&[0.0; 4], 44100, 224).is_err());
        assert!(encode(&[0.0; 3], 44100, 128).is_err());
        assert_eq!(encode(&[], 44100, 128).unwrap().len(), 0);
    }

    #[test]
    fn frame_lengths_match_spec() {
        assert_eq!(frame_bytes(44100, 128, false), 417);
        assert_eq!(frame_bytes(44100, 128, true), 418);
        assert_eq!(frame_bytes(48000, 128, false), 384);
        assert_eq!(frame_bytes(32000, 128, false), 576);
        assert_eq!(frame_bytes(44100, 320, false), 1044);
        assert_eq!(frame_bytes(48000, 320, false), 960);
    }

    #[test]
    fn output_parses_into_expected_frames() {
        for &sr in &SUPPORTED_SAMPLE_RATES {
            for &br in &SUPPORTED_BITRATES {
                let n = sr as usize / 2 + 77;
                let data = encode(&signal(n, sr), sr, br).unwrap();
                let frames = parse_frames(&data).unwrap();
                assert_eq!(frames.len(), frame_count(n));
                assert!(frames.len() * FRAME_SAMPLES >= n + DECODER_DELAY);
                assert!(frames.len() * FRAME_SAMPLES < n + DECODER_DELAY + FRAME_SAMPLES);
                for f in &frames {
                    assert_eq!(f.sample_rate, sr);
                    assert_eq!(f.bitrate_kbps, br);
                    assert_eq!(f.mode, 1);
                    assert!(f.mode_extension == 0 || f.mode_extension == 2);
                }
                // Average bitrate is exact (padding at 44.1 kHz).
                let secs = frames.len() as f64 * FRAME_SAMPLES as f64 / sr as f64;
                let kbps = data.len() as f64 * 8.0 / secs / 1000.0;
                assert!((kbps - br as f64).abs() < 0.5, "average bitrate {kbps} for {br}");
                if sr != 44100 {
                    assert!(frames.iter().all(|f| !f.padding));
                }
            }
        }
    }

    #[test]
    fn padding_follows_fractional_slots() {
        // 44.1 kHz / 128 kbps: 417.959... bytes per frame on average.
        let n = 1152 * 49;
        let data = encode(&vec![0.0; 2 * n], 44100, 128).unwrap();
        let frames = parse_frames(&data).unwrap();
        let padded = frames.iter().filter(|f| f.padding).count();
        let expected = (frames.len() as f64 * (144_000.0 * 128.0 / 44100.0 - 417.0)).floor() as usize;
        assert!(padded == expected || padded == expected + 1, "{padded} vs {expected}");
    }

    #[test]
    fn header_bits() {
        let data = encode(&signal(2000, 48000), 48000, 192).unwrap();
        // sync, MPEG-1, Layer III, no CRC
        assert_eq!(data[0], 0xFF);
        assert_eq!(data[1], 0xFB);
        // bitrate index 11 (192 kbps), 48 kHz (01), no padding, private 0
        assert_eq!(data[2], 0b1011_0100);
        // joint stereo, copyright 0, original 1, emphasis none
        assert_eq!(data[3] & 0b1100_1111, 0b0100_0100);
        // main_data_begin is 0 (no bit reservoir)
        assert_eq!(data[4], 0);
        assert_eq!(data[5] >> 7, 0);
    }

    #[test]
    fn silence_is_valid() {
        let data = encode(&vec![0.0; 2 * 10_000], 32000, 128).unwrap();
        assert_eq!(parse_frames(&data).unwrap().len(), frame_count(10_000));
    }
}
