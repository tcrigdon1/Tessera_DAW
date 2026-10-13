//! Quantisation, the rate loop (global gain search) and Huffman coding of one
//! granule of one channel.
//!
//! No psychoacoustic model is used: all scalefactors are zero, so the noise
//! is shaped only by the non-uniform quantiser. The rate loop finds the
//! smallest global gain whose Huffman-coded size fits the bit budget.

use std::sync::OnceLock;

use crate::bitstream::BitWriter;
use crate::filterbank::GRANULE;
use crate::tables::{table_lane, COUNT1_A_CODES, COUNT1_A_LENS, LANES, LANE_16, LANE_24, LINBITS_16, LINBITS_24};

/// Largest magnitude representable with table 31/23 (15 + 2^13 - 1).
const MAX_QUANT: u32 = 15 + 8191;
/// Maximum value of the 12-bit part2_3_length field.
pub const MAX_PART23: u32 = 4095;
/// Number of distinct Huffman code tables ("lanes").
const NLANES: usize = 15;

/// The coded form of one granule/channel.
#[derive(Clone)]
pub struct GranuleCode {
    /// Quantised magnitudes.
    pub ix: [u32; GRANULE],
    /// Sign of each line (true = negative).
    pub neg: [bool; GRANULE],
    pub part2_3_length: u32,
    pub big_values: u32,
    pub global_gain: u32,
    pub table_select: [u32; 3],
    pub region0_count: u32,
    pub region1_count: u32,
    pub count1table_select: u32,
    pub count1: u32,
}

impl GranuleCode {
    fn silent() -> Self {
        Self {
            ix: [0; GRANULE],
            neg: [false; GRANULE],
            part2_3_length: 0,
            big_values: 0,
            global_gain: 210,
            table_select: [0; 3],
            region0_count: 0,
            region1_count: 0,
            count1table_select: 0,
            count1: 0,
        }
    }
}

/// Lengths of every lane's codeword for each (min(x,15), min(y,15)) pair;
/// zero where the pair is outside the table.
fn pair_lengths() -> &'static [[u16; 16]; 256] {
    static T: OnceLock<[[u16; 16]; 256]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0u16; 16]; 256];
        for x in 0..16 {
            for y in 0..16 {
                for (lane, tab) in LANES.iter().enumerate() {
                    if x < tab.xlen && y < tab.xlen {
                        t[x * 16 + y][lane] = u16::from(tab.lens[x * tab.xlen + y]);
                    }
                }
            }
        }
        t
    })
}

fn bits_needed(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// Result of choosing a table for a run of scalefactor bands.
#[derive(Clone, Copy)]
struct Choice {
    bits: u32,
    table: u32,
}

/// Per-band statistics of the big-values region.
#[derive(Clone, Copy)]
struct Band {
    cost: [u32; 16],
    max: u32,
    esc: u32,
}

/// Region/table layout produced by [`layout`].
struct Layout {
    bits: u32,
    big_values: u32,
    count1: u32,
    count1table_select: u32,
    table_select: [u32; 3],
    region0_count: u32,
    region1_count: u32,
}

fn choose_table(max: u32, esc: u32, cost: &[u32; 16], force: Option<u32>) -> Choice {
    if max == 0 {
        return Choice { bits: 0, table: 0 };
    }
    let need = if max >= 15 { bits_needed(max - 15) } else { 0 };
    let fam = |lane: usize, linbits: &[u32; 8], base: u32| -> Option<Choice> {
        let k = linbits.iter().position(|&l| l >= need.max(1))?;
        Some(Choice {
            bits: cost[lane] + esc * linbits[k],
            table: base + k as u32,
        })
    };
    if let Some(t) = force {
        let t = t as usize;
        let (lane, linbits) = table_lane(t);
        let ok = if linbits > 0 {
            linbits >= need.max(1)
        } else {
            (max as usize) < LANES[lane].xlen
        };
        if ok {
            return Choice {
                bits: cost[lane] + esc * linbits,
                table: t as u32,
            };
        }
    }
    let mut best = Choice {
        bits: u32::MAX,
        table: 0,
    };
    if max <= 15 {
        for (lane, tab) in LANES.iter().enumerate().take(13) {
            if (max as usize) < tab.xlen && cost[lane] < best.bits {
                best = Choice {
                    bits: cost[lane],
                    table: crate::tables::LANE_TABLES[lane] as u32,
                };
            }
        }
    }
    for c in [fam(LANE_16, &LINBITS_16, 16), fam(LANE_24, &LINBITS_24, 24)]
        .into_iter()
        .flatten()
    {
        if c.bits < best.bits {
            best = c;
        }
    }
    best
}

/// Partitions the quantised spectrum into big-values / count1 / zero regions,
/// chooses region boundaries and Huffman tables, and returns the bit count of
/// the Huffman-coded data (part 3; there is no part 2 since all scalefactors
/// are zero).
fn layout(ix: &[u32; GRANULE], sfb: &[usize; 23], force: Option<u32>) -> Layout {
    let mut end = GRANULE;
    while end >= 2 && ix[end - 1] == 0 && ix[end - 2] == 0 {
        end -= 2;
    }
    let rzero = end;
    while end >= 4 && ix[end - 1] <= 1 && ix[end - 2] <= 1 && ix[end - 3] <= 1 && ix[end - 4] <= 1 {
        end -= 4;
    }
    let bv_end = end;
    let count1 = (rzero - bv_end) / 4;

    // count1 region
    let mut bits_a = 0u32;
    let mut c1_signs = 0u32;
    for q in 0..count1 {
        let i = bv_end + 4 * q;
        let idx = (ix[i] * 8 + ix[i + 1] * 4 + ix[i + 2] * 2 + ix[i + 3]) as usize;
        bits_a += u32::from(COUNT1_A_LENS[idx]);
        c1_signs += ix[i] + ix[i + 1] + ix[i + 2] + ix[i + 3];
    }
    let bits_b = 4 * count1 as u32;
    let (c1_bits, c1_sel) = if bits_a <= bits_b { (bits_a, 0) } else { (bits_b, 1) };

    // big-values bands
    let nb = sfb.iter().position(|&b| b >= bv_end).unwrap_or(22);
    let lens = pair_lengths();
    let mut bands = [Band {
        cost: [0; 16],
        max: 0,
        esc: 0,
    }; 22];
    let mut bv_signs = 0u32;
    for b in 0..nb {
        let lo = sfb[b];
        let hi = sfb[b + 1].min(bv_end);
        let mut cost = [0u32; 16];
        let mut max = 0u32;
        let mut esc = 0u32;
        let mut i = lo;
        while i < hi {
            let (x, y) = (ix[i], ix[i + 1]);
            max = max.max(x).max(y);
            esc += u32::from(x >= 15) + u32::from(y >= 15);
            bv_signs += u32::from(x != 0) + u32::from(y != 0);
            let l = &lens[(x.min(15) * 16 + y.min(15)) as usize];
            for k in 0..16 {
                cost[k] += u32::from(l[k]);
            }
            i += 2;
        }
        bands[b] = Band { cost, max, esc };
    }

    // best table for each band range [s, e)
    let mut range = [Choice { bits: 0, table: 0 }; 23 * 23];
    for s in 0..nb {
        let mut cost = [0u32; 16];
        let mut max = 0;
        let mut esc = 0;
        for e in s + 1..=nb {
            let band = &bands[e - 1];
            for (c, b) in cost.iter_mut().zip(band.cost.iter()).take(NLANES) {
                *c += b;
            }
            max = max.max(band.max);
            esc += band.esc;
            range[s * 23 + e] = choose_table(max, esc, &cost, force);
        }
    }
    let get = |s: usize, e: usize| range[s * 23 + e];

    let mut best = (u32::MAX, 0u32, 0u32, [0u32; 3]);
    for r0 in 0..16usize {
        for r1 in 0..8usize {
            if r0 + r1 + 2 > 22 {
                continue;
            }
            let b1 = (r0 + 1).min(nb);
            let b2 = (r0 + r1 + 2).min(nb);
            let (c0, c1, c2) = (get(0, b1), get(b1, b2), get(b2, nb));
            let bits = c0.bits.saturating_add(c1.bits).saturating_add(c2.bits);
            if bits < best.0 {
                best = (bits, r0 as u32, r1 as u32, [c0.table, c1.table, c2.table]);
            }
            if b2 == nb && r1 > 0 {
                break; // larger region1 counts give the same split
            }
        }
    }
    if nb == 0 {
        best = (0, 0, 0, [0; 3]);
    }

    Layout {
        bits: best.0.saturating_add(bv_signs + c1_bits + c1_signs),
        big_values: (bv_end / 2) as u32,
        count1: count1 as u32,
        count1table_select: c1_sel,
        table_select: best.3,
        region0_count: best.1,
        region1_count: best.2,
    }
}

#[inline]
fn quantize_into(xr34: &[f32; GRANULE], gain: u32, ix: &mut [u32; GRANULE]) -> u32 {
    let step = (-0.1875 * (gain as f64 - 210.0)).exp2() as f32;
    let mut max = 0;
    for (q, &v) in ix.iter_mut().zip(xr34.iter()) {
        let r = (v * step + 0.4054) as u32;
        *q = r;
        max = max.max(r);
    }
    max
}

/// Quantises and lays out one granule so that it fits in `budget` bits.
pub fn quantize_granule(xr: &[f32; GRANULE], budget: u32, sfb: &[usize; 23], hint: Option<u32>) -> GranuleCode {
    let mut code = GranuleCode::silent();
    let mut xr34 = [0.0f32; GRANULE];
    let mut max34 = 0.0f32;
    for i in 0..GRANULE {
        let a = xr[i].abs();
        code.neg[i] = xr[i] < 0.0;
        let v = a.sqrt() * a.sqrt().sqrt();
        xr34[i] = v;
        max34 = max34.max(v);
    }
    if max34 <= 0.0 || !max34.is_finite() {
        return code;
    }
    // Lowest gain keeping every value within the escape range.
    let gmin = 210.0 - (16.0 / 3.0) * ((MAX_QUANT as f64 - 0.5) / max34 as f64).log2();
    let gmin = (gmin.ceil().max(0.0) as u32).min(255);
    let mut ix = [0u32; GRANULE];
    let fits = |gain: u32, ix: &mut [u32; GRANULE]| -> Option<Layout> {
        let max = quantize_into(&xr34, gain, ix);
        if max > MAX_QUANT {
            return None;
        }
        let l = layout(ix, sfb, None);
        (l.bits <= budget).then_some(l)
    };
    // Find the smallest gain that fits: bracket it by exponential steps from
    // the hint (usually the previous granule's gain), then bisect. Gains below
    // `lo` are known not to fit (or are below the limit); `hi` fits.
    let start = hint.unwrap_or(210).clamp(gmin, 255);
    let (mut lo, mut hi);
    let mut best: Option<(u32, Layout)>;
    if let Some(l) = fits(start, &mut ix) {
        best = Some((start, l));
        hi = start;
        lo = gmin;
        let mut step = 1;
        while hi > gmin {
            let probe = hi.saturating_sub(step).max(gmin);
            match fits(probe, &mut ix) {
                Some(l) => {
                    best = Some((probe, l));
                    hi = probe;
                    step *= 2;
                }
                None => {
                    lo = probe + 1;
                    break;
                }
            }
        }
    } else {
        lo = start + 1;
        let mut step = 1;
        loop {
            let probe = (start + step).min(255);
            match fits(probe, &mut ix) {
                Some(l) => {
                    best = Some((probe, l));
                    hi = probe;
                    break;
                }
                None if probe == 255 => {
                    // Gain 255 quantises any sane input to zero; this is
                    // unreachable in practice. Fall back to silence.
                    return GranuleCode::silent();
                }
                None => {
                    lo = probe + 1;
                    step *= 2;
                }
            }
        }
    }
    while lo < hi {
        let mid = (lo + hi) / 2;
        match fits(mid, &mut ix) {
            Some(l) => {
                best = Some((mid, l));
                hi = mid;
            }
            None => lo = mid + 1,
        }
    }
    let (gain, lay) = best.expect("a fitting gain was found");
    quantize_into(&xr34, gain, &mut ix);
    code.ix = ix;
    code.part2_3_length = lay.bits;
    code.big_values = lay.big_values;
    code.global_gain = gain;
    code.table_select = lay.table_select;
    code.region0_count = lay.region0_count;
    code.region1_count = lay.region1_count;
    code.count1table_select = lay.count1table_select;
    code.count1 = lay.count1;
    code
}

/// Re-chooses the Huffman tables of a quantised granule, using `table`
/// wherever it can represent a region (testing aid; may increase the size).
pub fn force_table(code: &mut GranuleCode, sfb: &[usize; 23], table: u32) {
    let l = layout(&code.ix, sfb, Some(table));
    code.part2_3_length = l.bits;
    code.table_select = l.table_select;
    code.region0_count = l.region0_count;
    code.region1_count = l.region1_count;
    code.count1table_select = l.count1table_select;
}

/// Writes the Huffman-coded main data of a granule (part 3).
pub fn write_granule(w: &mut BitWriter, code: &GranuleCode, sfb: &[usize; 23]) {
    let start = w.bit_len();
    let bv_end = 2 * code.big_values as usize;
    let r1 = sfb[(code.region0_count + 1) as usize].min(bv_end);
    let r2 = sfb[((code.region0_count + code.region1_count + 2) as usize).min(22)].min(bv_end);
    let mut i = 0;
    while i < bv_end {
        let region = if i < r1 {
            0
        } else if i < r2 {
            1
        } else {
            2
        };
        let table = code.table_select[region] as usize;
        if table != 0 {
            let (lane, linbits) = table_lane(table);
            let tab = &LANES[lane];
            let (x, y) = (code.ix[i], code.ix[i + 1]);
            let (cx, cy) = if linbits > 0 { (x.min(15), y.min(15)) } else { (x, y) };
            let idx = cx as usize * tab.xlen + cy as usize;
            w.put(u32::from(tab.codes[idx]), u32::from(tab.lens[idx]));
            for (v, neg) in [(x, code.neg[i]), (y, code.neg[i + 1])] {
                if linbits > 0 && v >= 15 {
                    w.put(v - 15, linbits);
                }
                if v != 0 {
                    w.put(u32::from(neg), 1);
                }
            }
        } else {
            debug_assert!(code.ix[i] == 0 && code.ix[i + 1] == 0);
        }
        i += 2;
    }
    for q in 0..code.count1 as usize {
        let i = bv_end + 4 * q;
        let v = &code.ix[i..i + 4];
        let idx = (v[0] * 8 + v[1] * 4 + v[2] * 2 + v[3]) as usize;
        if code.count1table_select == 0 {
            w.put(u32::from(COUNT1_A_CODES[idx]), u32::from(COUNT1_A_LENS[idx]));
        } else {
            w.put(15 - idx as u32, 4);
        }
        for (&val, &neg) in v.iter().zip(&code.neg[i..i + 4]) {
            if val != 0 {
                w.put(u32::from(neg), 1);
            }
        }
    }
    debug_assert_eq!(w.bit_len() - start, code.part2_3_length as usize, "bit count mismatch");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tables::SFB_LONG;

    fn test_spectrum(seed: u32, scale: f32) -> [f32; GRANULE] {
        let mut s = seed;
        let mut xr = [0.0f32; GRANULE];
        for (i, v) in xr.iter_mut().enumerate() {
            s = s.wrapping_mul(1664525).wrapping_add(1013904223);
            let r = (s >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            *v = r * scale / (1.0 + i as f32 / 20.0);
        }
        xr
    }

    #[test]
    fn counted_bits_match_written_bits_and_budget() {
        let sfb = &SFB_LONG[0];
        for (seed, scale, budget) in [
            (1, 1.0, 1500),
            (2, 0.01, 700),
            (3, 30.0, 4000),
            (4, 1e-4, 200),
            (5, 5.0, 100),
        ] {
            let xr = test_spectrum(seed, scale);
            for force in [None, Some(1), Some(5), Some(13), Some(15), Some(16), Some(24), Some(31)] {
                let mut code = quantize_granule(&xr, budget, sfb, None);
                assert!(code.part2_3_length <= budget);
                if let Some(t) = force {
                    force_table(&mut code, sfb, t);
                }
                assert!(code.big_values <= 288);
                assert!(code.ix.iter().all(|&v| v <= MAX_QUANT));
                let mut w = BitWriter::new();
                write_granule(&mut w, &code, sfb);
                assert_eq!(w.bit_len(), code.part2_3_length as usize);
            }
        }
    }

    #[test]
    fn silence_costs_nothing() {
        let code = quantize_granule(&[0.0; GRANULE], 1000, &SFB_LONG[1], None);
        assert_eq!(code.part2_3_length, 0);
        assert_eq!(code.big_values, 0);
        assert_eq!(code.count1, 0);
    }

    #[test]
    fn larger_budget_gives_finer_quantisation() {
        let xr = test_spectrum(7, 2.0);
        let a = quantize_granule(&xr, 600, &SFB_LONG[0], Some(100));
        let b = quantize_granule(&xr, 3000, &SFB_LONG[0], Some(250));
        assert!(b.global_gain < a.global_gain);
    }
}
