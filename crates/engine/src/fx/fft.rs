//! Iterative radix-2 FFT for analysis displays and partitioned convolution.
//!
//! A plan ([`Fft`] for complex data, [`RealFft`] for real signals) is built
//! once on the UI thread with its twiddle factors and bit-reversal table;
//! transforms then run in place on caller-provided buffers and never
//! allocate, so they are safe on the audio thread.
//!
//! Conventions: the forward transform is unnormalised
//! (`X[k] = sum x[n] e^(-2 pi i k n / N)`) and the inverse divides by `N`, so
//! `inverse(forward(x)) == x`.

use std::f64::consts::PI;
use std::ops::{Add, Mul, Sub};

/// A complex number in single precision.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Complex {
    pub re: f32,
    pub im: f32,
}

impl Complex {
    pub const ZERO: Complex = Complex { re: 0.0, im: 0.0 };

    #[inline]
    pub const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    #[inline]
    pub fn conj(self) -> Self {
        Self::new(self.re, -self.im)
    }

    #[inline]
    pub fn norm_sqr(self) -> f32 {
        self.re * self.re + self.im * self.im
    }

    #[inline]
    pub fn abs(self) -> f32 {
        self.norm_sqr().sqrt()
    }

    #[inline]
    pub fn scale(self, s: f32) -> Self {
        Self::new(self.re * s, self.im * s)
    }

    /// Multiplication by `i`.
    #[inline]
    fn mul_i(self) -> Self {
        Self::new(-self.im, self.re)
    }
}

impl Add for Complex {
    type Output = Complex;
    #[inline]
    fn add(self, o: Complex) -> Complex {
        Complex::new(self.re + o.re, self.im + o.im)
    }
}

impl Sub for Complex {
    type Output = Complex;
    #[inline]
    fn sub(self, o: Complex) -> Complex {
        Complex::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for Complex {
    type Output = Complex;
    #[inline]
    fn mul(self, o: Complex) -> Complex {
        Complex::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}

/// Plan for complex transforms of one power-of-two size.
#[derive(Debug, Clone)]
pub struct Fft {
    n: usize,
    /// `e^(-2 pi i k / n)` for `k < n / 2`.
    twiddles: Vec<Complex>,
    /// Bit-reversed index of every position.
    rev: Vec<u32>,
}

impl Fft {
    /// A plan for `n`-point transforms. `n` is rounded up to a power of two
    /// (at least 1). Allocates; UI thread only.
    pub fn new(n: usize) -> Self {
        let n = n.max(1).next_power_of_two();
        let bits = n.trailing_zeros();
        let twiddles = (0..n / 2)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                Complex::new(a.cos() as f32, a.sin() as f32)
            })
            .collect();
        let rev = (0..n as u32)
            .map(|i| if bits == 0 { 0 } else { i.reverse_bits() >> (32 - bits) })
            .collect();
        Self { n, twiddles, rev }
    }

    /// Transform size.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// In-place forward transform of `buf[..len()]`.
    pub fn forward(&self, buf: &mut [Complex]) {
        self.transform(buf, false);
    }

    /// In-place inverse transform of `buf[..len()]`, scaled by `1 / len()`.
    pub fn inverse(&self, buf: &mut [Complex]) {
        self.transform(buf, true);
        let s = 1.0 / self.n as f32;
        for v in &mut buf[..self.n] {
            *v = v.scale(s);
        }
    }

    fn transform(&self, buf: &mut [Complex], inverse: bool) {
        let n = self.n;
        assert!(buf.len() >= n, "fft buffer too short");
        let buf = &mut buf[..n];
        for i in 0..n {
            let j = self.rev[i] as usize;
            if j > i {
                buf.swap(i, j);
            }
        }
        let mut half = 1;
        while half < n {
            let step = n / (half * 2);
            for start in (0..n).step_by(half * 2) {
                let (lo, hi) = buf[start..start + 2 * half].split_at_mut(half);
                for k in 0..half {
                    let w = self.twiddles[k * step];
                    let w = if inverse { w.conj() } else { w };
                    let a = lo[k];
                    let b = hi[k] * w;
                    lo[k] = a + b;
                    hi[k] = a - b;
                }
            }
            half *= 2;
        }
    }
}

/// Plan for transforms of real signals of a power-of-two size `n >= 2`,
/// computed with an `n / 2`-point complex transform. The spectrum holds the
/// `n / 2 + 1` non-negative frequency bins.
#[derive(Debug, Clone)]
pub struct RealFft {
    n: usize,
    half: Fft,
    /// `e^(-2 pi i k / n)` for `k <= n / 4`.
    post: Vec<Complex>,
}

impl RealFft {
    /// Allocates; UI thread only.
    pub fn new(n: usize) -> Self {
        let n = n.max(2).next_power_of_two();
        let post = (0..=n / 4)
            .map(|k| {
                let a = -2.0 * PI * k as f64 / n as f64;
                Complex::new(a.cos() as f32, a.sin() as f32)
            })
            .collect();
        Self {
            n,
            half: Fft::new(n / 2),
            post,
        }
    }

    /// Signal length.
    pub fn len(&self) -> usize {
        self.n
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Number of spectrum bins (`len() / 2 + 1`).
    pub fn bins(&self) -> usize {
        self.n / 2 + 1
    }

    /// Spectrum of `input[..len()]` into `out[..bins()]`.
    pub fn forward(&self, input: &[f32], out: &mut [Complex]) {
        let h = self.n / 2;
        assert!(input.len() >= self.n && out.len() > h, "real fft buffers too short");
        // Pack even samples as real and odd samples as imaginary parts.
        for k in 0..h {
            out[k] = Complex::new(input[2 * k], input[2 * k + 1]);
        }
        self.half.forward(&mut out[..h]);
        // Split the packed spectrum into the spectra of the even (E) and odd
        // (O) samples and combine them: X[k] = E + W^k O and
        // X[h - k] = conj(E - W^k O).
        let z0 = out[0];
        out[0] = Complex::new(z0.re + z0.im, 0.0);
        out[h] = Complex::new(z0.re - z0.im, 0.0);
        for k in 1..=h / 2 {
            let m = h - k;
            let zk = out[k];
            let zm = out[m];
            let e = (zk + zm.conj()).scale(0.5);
            // (zk - conj zm) / 2i
            let d = (zk - zm.conj()).scale(0.5);
            let o = Complex::new(d.im, -d.re);
            let wo = self.post[k] * o;
            out[k] = e + wo;
            if m != k {
                out[m] = (e - wo).conj();
            }
        }
    }

    /// Signal of the spectrum `spec[..bins()]` into `out[..len()]`. `spec` is
    /// used as scratch space and left modified.
    pub fn inverse(&self, spec: &mut [Complex], out: &mut [f32]) {
        let h = self.n / 2;
        assert!(spec.len() > h && out.len() >= self.n, "real fft buffers too short");
        let x0 = spec[0].re;
        let xh = spec[h].re;
        spec[0] = Complex::new(0.5 * (x0 + xh), 0.5 * (x0 - xh));
        for k in 1..=h / 2 {
            let m = h - k;
            let xk = spec[k];
            let xm = spec[m];
            let e = (xk + xm.conj()).scale(0.5);
            let o = ((xk - xm.conj()) * self.post[k].conj()).scale(0.5);
            spec[k] = e + o.mul_i();
            if m != k {
                spec[m] = e.conj() + o.conj().mul_i();
            }
        }
        self.half.inverse(&mut spec[..h]);
        for k in 0..h {
            out[2 * k] = spec[k].re;
            out[2 * k + 1] = spec[k].im;
        }
    }
}

/// Periodic Hann window of length `n` (for spectral analysis).
pub fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos()) as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn naive_dft(x: &[Complex], inverse: bool) -> Vec<Complex> {
        let n = x.len();
        let sign = if inverse { 1.0 } else { -1.0 };
        (0..n)
            .map(|k| {
                let mut acc = (0.0f64, 0.0f64);
                for (j, v) in x.iter().enumerate() {
                    let a = sign * 2.0 * PI * (k * j % n) as f64 / n as f64;
                    let (s, c) = a.sin_cos();
                    acc.0 += v.re as f64 * c - v.im as f64 * s;
                    acc.1 += v.re as f64 * s + v.im as f64 * c;
                }
                let s = if inverse { 1.0 / n as f64 } else { 1.0 };
                Complex::new((acc.0 * s) as f32, (acc.1 * s) as f32)
            })
            .collect()
    }

    fn signal(n: usize, seed: u32) -> Vec<Complex> {
        let mut rng = crate::dsp::Rng::new(seed);
        (0..n)
            .map(|_| Complex::new(rng.next_bipolar(), rng.next_bipolar()))
            .collect()
    }

    fn max_err(a: &[Complex], b: &[Complex]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (*x - *y).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn complex_fft_matches_naive_dft() {
        for n in [1usize, 2, 4, 8, 16, 64, 256, 1024] {
            let x = signal(n, n as u32 + 3);
            let fft = Fft::new(n);
            let mut buf = x.clone();
            fft.forward(&mut buf);
            let want = naive_dft(&x, false);
            let tol = 4e-4 * (n as f32).sqrt();
            assert!(max_err(&buf, &want) < tol, "forward n={n}: {}", max_err(&buf, &want));
            let mut inv = want.clone();
            fft.inverse(&mut inv);
            assert!(max_err(&inv, &x) < 1e-4, "inverse n={n}");
            let naive_inv = naive_dft(&want, true);
            assert!(max_err(&naive_inv, &x) < 1e-4);
        }
    }

    #[test]
    fn real_fft_matches_naive_dft() {
        for n in [2usize, 4, 8, 32, 512, 4096] {
            let x: Vec<f32> = signal(n, 9 + n as u32).iter().map(|c| c.re).collect();
            let as_complex: Vec<Complex> = x.iter().map(|&r| Complex::new(r, 0.0)).collect();
            let want = naive_dft(&as_complex, false);
            let rfft = RealFft::new(n);
            assert_eq!(rfft.bins(), n / 2 + 1);
            let mut spec = vec![Complex::ZERO; rfft.bins()];
            rfft.forward(&x, &mut spec);
            let tol = 4e-4 * (n as f32).sqrt();
            let err = max_err(&spec, &want[..n / 2 + 1]);
            assert!(err < tol, "real forward n={n}: {err}");
            let mut back = vec![0.0f32; n];
            rfft.inverse(&mut spec, &mut back);
            let err = x.iter().zip(&back).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            assert!(err < 1e-4, "real inverse n={n}: {err}");
        }
    }

    #[test]
    fn sine_lands_in_its_bin() {
        let n = 1024;
        let rfft = RealFft::new(n);
        let x: Vec<f32> = (0..n)
            .map(|i| (2.0 * PI * 37.0 * i as f64 / n as f64).sin() as f32)
            .collect();
        let mut spec = vec![Complex::ZERO; rfft.bins()];
        rfft.forward(&x, &mut spec);
        let peak = (0..spec.len())
            .max_by(|&a, &b| spec[a].abs().total_cmp(&spec[b].abs()))
            .unwrap();
        assert_eq!(peak, 37);
        assert!((spec[37].abs() - n as f32 / 2.0).abs() < 0.01 * n as f32);
    }
}
