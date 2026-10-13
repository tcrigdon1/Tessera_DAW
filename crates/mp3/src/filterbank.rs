//! Hybrid analysis filterbank: 32-band polyphase (cosine-modulated) filterbank
//! followed by an 18-line MDCT per subband (long blocks only) and the
//! aliasing-reduction butterflies.
//!
//! The prototype low-pass of the polyphase bank is designed here (a
//! Kaiser-windowed sinc, tuned to be power complementary across the subband
//! crossovers) rather than taken from a table; it has the same structure,
//! length, phase and normalisation as the window the decoder uses for
//! synthesis, so the two combine to a near-transparent filterbank. The two
//! design parameters were tuned so that the residual aliasing against a
//! standard decoder's synthesis window stays below about -48 dB for tones
//! anywhere in a subband (and around -80 dB near band centres and edges).

use std::f64::consts::PI;
use std::sync::OnceLock;

/// Number of MDCT lines per granule and channel.
pub const GRANULE: usize = 576;

/// Kaiser beta of the prototype low-pass.
const PROTO_BETA: f64 = 10.7;
/// Cutoff of the prototype low-pass relative to pi/64 (half a subband).
const PROTO_CUTOFF: f64 = 1.1445;

/// Aliasing-reduction coefficients c_i from the standard.
const ALIAS_C: [f64; 8] = [-0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037];

pub struct Tables {
    /// Analysis window (512 taps, sign pattern of the standard's C[i] included).
    pub window: [f32; 512],
    /// Polyphase matrixing coefficients M[k][i] = cos((2k+1)(i-16)pi/64).
    pub matrix: [[f32; 64]; 32],
    /// Forward MDCT kernel including the sine window and 1/9 normalisation:
    /// X[m] = sum_i mdct[m][i] * z[i].
    pub mdct: [[f32; 36]; 18],
    pub cs: [f32; 8],
    pub ca: [f32; 8],
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..200 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Prototype low-pass h[n], n = 0..512, symmetric around n = 256 with h[0] = 0,
/// normalised to a DC gain of 2 (the convention of the standard's analysis window).
pub fn prototype() -> [f64; 512] {
    const L: usize = 511;
    let beta = PROTO_BETA;
    let wc = PROTO_CUTOFF * PI / 64.0;
    let i0b = bessel_i0(beta);
    let mut h = [0.0f64; 512];
    for (j, hj) in h.iter_mut().enumerate().skip(1) {
        let i = j - 1; // 0..511
        let t = j as f64 - 256.0;
        let x = 2.0 * i as f64 / (L - 1) as f64 - 1.0;
        let kaiser = bessel_i0(beta * (1.0 - x * x).max(0.0).sqrt()) / i0b;
        let sinc = if t == 0.0 { wc / PI } else { (wc * t).sin() / (PI * t) };
        *hj = sinc * kaiser;
    }
    let sum: f64 = h.iter().sum();
    for v in h.iter_mut() {
        *v *= 2.0 / sum;
    }
    h
}

fn build_tables() -> Tables {
    let h = prototype();
    let mut window = [0.0f32; 512];
    for (i, w) in window.iter_mut().enumerate() {
        let sign = if (i / 64) % 2 == 1 { -1.0 } else { 1.0 };
        *w = (h[i] * sign) as f32;
    }
    let mut matrix = [[0.0f32; 64]; 32];
    for (k, row) in matrix.iter_mut().enumerate() {
        for (i, m) in row.iter_mut().enumerate() {
            *m = ((2 * k + 1) as f64 * (i as f64 - 16.0) * PI / 64.0).cos() as f32;
        }
    }
    let mut mdct = [[0.0f32; 36]; 18];
    for (m, row) in mdct.iter_mut().enumerate() {
        for (i, v) in row.iter_mut().enumerate() {
            let win = (PI / 36.0 * (i as f64 + 0.5)).sin();
            let c = (PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * m + 1) as f64).cos();
            *v = (win * c / 9.0) as f32;
        }
    }
    let mut cs = [0.0f32; 8];
    let mut ca = [0.0f32; 8];
    for i in 0..8 {
        let d = (1.0 + ALIAS_C[i] * ALIAS_C[i]).sqrt();
        cs[i] = (1.0 / d) as f32;
        ca[i] = (ALIAS_C[i] / d) as f32;
    }
    Tables {
        window,
        matrix,
        mdct,
        cs,
        ca,
    }
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(build_tables)
}

/// Dot product with eight independent accumulators (lets the compiler use SIMD).
#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let mut acc = [0.0f32; 8];
    let (ca, ra) = a.as_chunks::<8>();
    let (cb, rb) = b.as_chunks::<8>();
    let tail: f32 = ra.iter().zip(rb).map(|(x, y)| x * y).sum();
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..8 {
            acc[l] += x[l] * y[l];
        }
    }
    acc.iter().sum::<f32>() + tail
}

/// Per-channel analysis state.
pub struct ChannelAnalyzer {
    /// hist[i] = x[t - i] (newest sample at index 0).
    hist: [f32; 512],
    /// Subband samples of the previous granule, [subband][time].
    prev: [[f32; 18]; 32],
}

impl Default for ChannelAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

impl ChannelAnalyzer {
    pub fn new() -> Self {
        Self {
            hist: [0.0; 512],
            prev: [[0.0; 18]; 32],
        }
    }

    /// Polyphase analysis of 32 new input samples (chronological order).
    fn subband_step(&mut self, t: &Tables, input: &[f32], out: &mut [f32; 32]) {
        self.hist.copy_within(0..480, 32);
        for (j, &s) in input.iter().enumerate() {
            self.hist[31 - j] = s;
        }
        let mut y = [0.0f32; 64];
        for j in 0..8 {
            let w = &t.window[64 * j..64 * j + 64];
            let h = &self.hist[64 * j..64 * j + 64];
            for i in 0..64 {
                y[i] += w[i] * h[i];
            }
        }
        for (k, o) in out.iter_mut().enumerate() {
            *o = dot(&t.matrix[k], &y);
        }
    }

    /// Runs the polyphase bank over 576 input samples and returns the 18 x 32
    /// subband samples as [subband][time], with the frequency inversion of odd
    /// subbands applied.
    fn analyze_granule(&mut self, t: &Tables, input: &[f32; GRANULE]) -> [[f32; 18]; 32] {
        let mut sb = [[0.0f32; 18]; 32];
        let mut out = [0.0f32; 32];
        for time in 0..18 {
            self.subband_step(t, &input[time * 32..time * 32 + 32], &mut out);
            for k in 0..32 {
                let inv = if k % 2 == 1 && time % 2 == 1 { -1.0 } else { 1.0 };
                sb[k][time] = out[k] * inv;
            }
        }
        sb
    }

    /// Feeds a granule without producing MDCT output (used to run the
    /// analysis one granule ahead so the MDCT adds no extra latency).
    pub fn prime(&mut self, input: &[f32; GRANULE]) {
        let t = tables();
        self.prev = self.analyze_granule(t, input);
    }

    /// Analyses the next 576 input samples and produces the 576 MDCT lines of
    /// the block formed by the previous and the new subband granule.
    pub fn granule(&mut self, input: &[f32; GRANULE], xr: &mut [f32; GRANULE]) {
        let t = tables();
        let cur = self.analyze_granule(t, input);
        for k in 0..32 {
            let mut z = [0.0f32; 36];
            z[..18].copy_from_slice(&self.prev[k]);
            z[18..].copy_from_slice(&cur[k]);
            for m in 0..18 {
                xr[k * 18 + m] = dot(&t.mdct[m], &z);
            }
        }
        self.prev = cur;
        // Aliasing reduction (inverse of the decoder's butterflies).
        for k in 1..32 {
            for i in 0..8 {
                let lo = k * 18 - 1 - i;
                let hi = k * 18 + i;
                let a = xr[lo];
                let b = xr[hi];
                xr[lo] = a * t.cs[i] + b * t.ca[i];
                xr[hi] = b * t.cs[i] - a * t.ca[i];
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A straightforward decoder-side reconstruction (alias butterflies,
    /// IMDCT, overlap-add, frequency inversion, polyphase synthesis with the
    /// window 32*C) used to check that the analysis side is invertible.
    struct Synth {
        overlap: [[f64; 18]; 32],
        v: Vec<f64>,
    }

    impl Synth {
        fn new() -> Self {
            Self {
                overlap: [[0.0; 18]; 32],
                v: vec![0.0; 1024],
            }
        }

        fn granule(&mut self, xr_in: &[f32; GRANULE], out: &mut Vec<f64>) {
            let t = tables();
            let mut xr: Vec<f64> = xr_in.iter().map(|&v| v as f64).collect();
            for k in 1..32 {
                for i in 0..8 {
                    let lo = k * 18 - 1 - i;
                    let hi = k * 18 + i;
                    let (bu, bd) = (xr[lo], xr[hi]);
                    xr[lo] = bu * t.cs[i] as f64 - bd * t.ca[i] as f64;
                    xr[hi] = bd * t.cs[i] as f64 + bu * t.ca[i] as f64;
                }
            }
            let mut sb = [[0.0f64; 32]; 18];
            for k in 0..32 {
                let mut z = [0.0f64; 36];
                for (i, zi) in z.iter_mut().enumerate() {
                    let mut s = 0.0;
                    for m in 0..18 {
                        s += xr[k * 18 + m] * (PI / 72.0 * (2 * i + 1 + 18) as f64 * (2 * m + 1) as f64).cos();
                    }
                    *zi = s * (PI / 36.0 * (i as f64 + 0.5)).sin();
                }
                for time in 0..18 {
                    let mut v = z[time] + self.overlap[k][time];
                    if k % 2 == 1 && time % 2 == 1 {
                        v = -v;
                    }
                    sb[time][k] = v;
                    self.overlap[k][time] = z[time + 18];
                }
            }
            let h = prototype();
            for s in sb.iter() {
                self.v.copy_within(0..960, 64);
                for i in 0..64 {
                    let mut acc = 0.0;
                    for (k, &sk) in s.iter().enumerate() {
                        acc += ((16 + i) as f64 * (2 * k + 1) as f64 * PI / 64.0).cos() * sk;
                    }
                    self.v[i] = acc;
                }
                let mut u = [0.0f64; 512];
                for i in 0..8 {
                    for j in 0..32 {
                        u[64 * i + j] = self.v[128 * i + j];
                        u[64 * i + 32 + j] = self.v[128 * i + 96 + j];
                    }
                }
                for j in 0..32 {
                    let mut acc = 0.0;
                    for i in 0..16 {
                        let n = j + 32 * i;
                        let sign = if (n / 64) % 2 == 1 { -1.0 } else { 1.0 };
                        acc += u[n] * 32.0 * h[n] * sign;
                    }
                    out.push(acc);
                }
            }
        }
    }

    #[test]
    fn analysis_is_invertible() {
        let n = 576 * 40;
        let input: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / 44100.0;
                (0.5 * (2.0 * PI * 1000.0 * t).sin() + 0.3 * (2.0 * PI * 5512.5 * t).sin()) as f32
            })
            .collect();
        let mut an = ChannelAnalyzer::new();
        let mut syn = Synth::new();
        let mut out = Vec::new();
        let mut g = [0.0f32; GRANULE];
        g.copy_from_slice(&input[..GRANULE]);
        an.prime(&g);
        let mut xr = [0.0f32; GRANULE];
        for gi in 1..n / GRANULE {
            g.copy_from_slice(&input[gi * GRANULE..(gi + 1) * GRANULE]);
            an.granule(&g, &mut xr);
            syn.granule(&xr, &mut out);
        }
        // Find the delay and check reconstruction quality.
        let mut best = (0usize, f64::MAX);
        for d in 0..1200 {
            let err: f64 = (2000..out.len() - 2000)
                .filter(|&i| i >= d)
                .map(|i| (out[i] - input[i - d] as f64).powi(2))
                .sum();
            if err < best.1 {
                best = (d, err);
            }
        }
        let d = best.0;
        let sig: f64 = (2000..out.len() - 2000).map(|i| (input[i - d] as f64).powi(2)).sum();
        let snr = 10.0 * (sig / best.1).log10();
        assert_eq!(d, 481, "unexpected filterbank delay");
        assert!(snr > 50.0, "reconstruction SNR too low: {snr:.1} dB");
    }
}
