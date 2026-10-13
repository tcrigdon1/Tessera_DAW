//! Browser preview: plays a sample straight to the output (after the master
//! track), the way a browser "cue" does, without touching the project.

use std::sync::Arc;

use rtrb::Producer;

use crate::dsp::sample::read_hermite;
use crate::model::AudioData;

/// Preview level (about -6 dB) so a loud sample doesn't jump out.
const PREVIEW_GAIN: f32 = 0.5;

pub(super) struct Previewer {
    data: Option<Arc<AudioData>>,
    pos: f64,
    /// Fade-out progress when stopped (1 = full level).
    fade: f32,
    stopping: bool,
    /// Finished samples go back to the UI thread to be freed there.
    garbage: Producer<Arc<AudioData>>,
}

impl Previewer {
    pub(super) fn new(garbage: Producer<Arc<AudioData>>) -> Self {
        Self {
            data: None,
            pos: 0.0,
            fade: 1.0,
            stopping: false,
            garbage,
        }
    }

    pub(super) fn is_playing(&self) -> bool {
        self.data.is_some()
    }

    pub(super) fn start(&mut self, data: Arc<AudioData>) {
        self.retire();
        self.data = Some(data);
        self.pos = 0.0;
        self.fade = 1.0;
        self.stopping = false;
    }

    pub(super) fn stop(&mut self) {
        if self.data.is_some() {
            self.stopping = true;
        }
    }

    fn retire(&mut self) {
        if let Some(old) = self.data.take() {
            if let Err(rtrb::PushError::Full(old)) = self.garbage.push(old) {
                // Never free on the audio thread.
                std::mem::forget(old);
            }
        }
    }

    /// Adds the preview to the output buffers.
    pub(super) fn mix(&mut self, left: &mut [f32], right: &mut [f32], sample_rate: f64) {
        let Some(data) = self.data.as_ref() else { return };
        let step = data.sample_rate as f64 / sample_rate;
        let n = data.frames() as f64;
        let fade_step = 1.0 / (0.01 * sample_rate as f32);
        let mut done = false;
        for i in 0..left.len() {
            if self.pos >= n || self.fade <= 0.0 {
                done = true;
                break;
            }
            let (l, r) = read_hermite(data, self.pos);
            left[i] += l * PREVIEW_GAIN * self.fade;
            right[i] += r * PREVIEW_GAIN * self.fade;
            self.pos += step;
            if self.stopping {
                self.fade -= fade_step;
            }
        }
        if done {
            self.retire();
        }
    }
}
