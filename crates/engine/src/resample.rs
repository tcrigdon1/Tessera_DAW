//! Streaming sample-rate conversion for live input.
//!
//! An audio interface does not always run at the engine's rate (Windows
//! shared mode, for one, only offers the rate set in the sound control
//! panel). [`StreamResampler`] converts a stereo stream frame by frame with
//! cubic (Catmull-Rom) interpolation. When it converts down it first runs an
//! 8th-order low-pass at just under the new Nyquist frequency so content
//! above it does not fold back as aliasing. It never allocates, so it can
//! run inside a device callback.

/// One RBJ low-pass biquad section (transposed direct form II).
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn lowpass(cutoff: f64, rate: f64, q: f64) -> Biquad {
        let w = 2.0 * std::f64::consts::PI * cutoff / rate;
        let (sin, cos) = w.sin_cos();
        let alpha = sin / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b1 = (1.0 - cos) / a0;
        Biquad {
            b0: (b1 * 0.5) as f32,
            b1: b1 as f32,
            b2: (b1 * 0.5) as f32,
            a1: (-2.0 * cos / a0) as f32,
            a2: ((1.0 - alpha) / a0) as f32,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Converts a stereo stream from one sample rate to another.
#[derive(Debug, Clone)]
pub struct StreamResampler {
    /// Input frames per output frame.
    step: f64,
    /// Where the next output frame lies, in input frames after `hist[1]`.
    pos: f64,
    /// The last four input frames, oldest first.
    hist: [[f32; 2]; 4],
    /// Anti-alias filter per channel (four sections), when converting down.
    lowpass: Option<[[Biquad; 4]; 2]>,
}

impl StreamResampler {
    pub fn new(from_rate: u32, to_rate: u32) -> StreamResampler {
        let from = from_rate.max(1) as f64;
        let to = to_rate.max(1) as f64;
        let lowpass = (to < from).then(|| {
            // Butterworth: four sections with these Qs make an 8th-order filter.
            let cutoff = to * 0.43;
            let ch = [0.509_795_6, 0.601_344_9, 0.899_976_1, 2.562_915_4].map(|q| Biquad::lowpass(cutoff, from, q));
            [ch, ch]
        });
        StreamResampler {
            step: from / to,
            pos: 1.0,
            hist: [[0.0; 2]; 4],
            lowpass,
        }
    }

    /// True when the rates match and frames pass through unchanged.
    pub fn is_passthrough(&self) -> bool {
        self.step == 1.0
    }

    /// Feeds one input frame; `out` receives every output frame it completes
    /// (none, one or several, depending on the ratio).
    #[inline]
    pub fn push(&mut self, mut l: f32, mut r: f32, mut out: impl FnMut(f32, f32)) {
        if self.is_passthrough() {
            out(l, r);
            return;
        }
        if let Some([fl, fr]) = &mut self.lowpass {
            l = fl.iter_mut().fold(l, |x, b| b.run(x));
            r = fr.iter_mut().fold(r, |x, b| b.run(x));
        }
        self.hist = [self.hist[1], self.hist[2], self.hist[3], [l, r]];
        self.pos -= 1.0;
        while self.pos < 1.0 {
            let t = self.pos.max(0.0) as f32;
            let h = &self.hist;
            out(
                cubic(h[0][0], h[1][0], h[2][0], h[3][0], t),
                cubic(h[0][1], h[1][1], h[2][1], h[3][1], t),
            );
            self.pos += self.step;
        }
    }

    /// Forgets the history (after the stream restarts).
    pub fn reset(&mut self) {
        self.pos = 1.0;
        self.hist = [[0.0; 2]; 4];
        if let Some(lp) = &mut self.lowpass {
            for b in lp.iter_mut().flatten() {
                b.z1 = 0.0;
                b.z2 = 0.0;
            }
        }
    }
}

/// Catmull-Rom interpolation between `y1` and `y2` at `t` in 0..1.
#[inline]
fn cubic(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let a = -0.5 * y0 + 1.5 * y1 - 1.5 * y2 + 0.5 * y3;
    let b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c = -0.5 * y0 + 0.5 * y2;
    ((a * t + b) * t + c) * t + y1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn convert(from: u32, to: u32, input: &[f32]) -> Vec<f32> {
        let mut rs = StreamResampler::new(from, to);
        let mut out = Vec::new();
        for &x in input {
            rs.push(x, -x, |l, r| {
                assert_eq!(l, -r);
                out.push(l);
            });
        }
        out
    }

    fn sine(freq: f64, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin() as f32)
            .collect()
    }

    /// Amplitude of `freq` in `x`, measured by correlation.
    fn level(x: &[f32], freq: f64, rate: u32) -> f64 {
        let (mut s, mut c) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let p = 2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64;
            s += *v as f64 * p.sin();
            c += *v as f64 * p.cos();
        }
        2.0 * (s * s + c * c).sqrt() / x.len() as f64
    }

    #[test]
    fn same_rate_passes_through() {
        let x = sine(1000.0, 48000, 256);
        assert_eq!(convert(48000, 48000, &x), x);
    }

    #[test]
    fn output_length_follows_the_ratio() {
        for (from, to) in [
            (48000, 44100),
            (44100, 48000),
            (96000, 44100),
            (22050, 48000),
            (192000, 48000),
        ] {
            let n = from as usize; // one second
            let out = convert(from, to, &vec![0.0; n]);
            let diff = out.len() as i64 - to as i64;
            assert!(diff.abs() <= 2, "{from} -> {to}: {} frames", out.len());
        }
    }

    #[test]
    fn a_tone_keeps_its_pitch_and_level() {
        for (from, to) in [(48000, 44100), (44100, 48000), (96000, 48000), (32000, 44100)] {
            let out = convert(from, to, &sine(1000.0, from, from as usize));
            // Skip the filter's settling time.
            let out = &out[1000..];
            let at = level(out, 1000.0, to);
            assert!((at - 1.0).abs() < 0.03, "{from} -> {to}: level {at}");
            let off = level(out, 1100.0, to);
            assert!(off < 0.02, "{from} -> {to}: leaks {off}");
        }
    }

    #[test]
    fn converting_down_suppresses_aliases() {
        // 30 kHz at 96 kHz would alias to 14.1 kHz at 44.1 kHz.
        let out = convert(96000, 44100, &sine(30000.0, 96000, 96000));
        let alias = level(&out[1000..], 14100.0, 44100);
        assert!(alias < 0.05, "alias {alias}");
    }
}
