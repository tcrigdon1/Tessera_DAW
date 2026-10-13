//! Return tracks fed by post-fader sends, and their mix into the master.

use crate::fx::{FxChain, FxContext, MAX_RETURNS};
use crate::model::{db_to_gain, pan_gains, Id, MixerParams, ReturnTrack};
use crate::MAX_BLOCK;

/// A return track on the audio thread.
pub struct EngineReturn {
    pub id: Id,
    pub mixer: MixerParams,
    pub chain: FxChain,
    pub(super) buf_l: Vec<f32>,
    pub(super) buf_r: Vec<f32>,
    pub(super) gain_l: f32,
    pub(super) gain_r: f32,
    pub(super) meter_l: f32,
    pub(super) meter_r: f32,
}

impl EngineReturn {
    /// Allocates; UI thread only.
    pub fn from_model(r: &ReturnTrack, sample_rate: f32) -> Self {
        Self {
            id: r.id,
            mixer: r.mixer,
            chain: FxChain::new(&r.devices, sample_rate),
            buf_l: vec![0.0; MAX_BLOCK],
            buf_r: vec![0.0; MAX_BLOCK],
            gain_l: 0.0,
            gain_r: 0.0,
            meter_l: 0.0,
            meter_r: 0.0,
        }
    }

    /// Takes over DSP state, gain ramps and meters from the previous snapshot.
    pub(super) fn adopt(&mut self, old: &mut EngineReturn) {
        self.chain.adopt(&mut old.chain);
        self.gain_l = old.gain_l;
        self.gain_r = old.gain_r;
        self.meter_l = old.meter_l;
        self.meter_r = old.meter_r;
    }
}

/// Send levels in dB to linear gains, padded with zeros.
pub(super) fn send_gains(sends_db: &[f32]) -> [f32; MAX_RETURNS] {
    let mut g = [0.0; MAX_RETURNS];
    for (o, db) in g.iter_mut().zip(sends_db.iter()) {
        *o = db_to_gain(*db);
    }
    g
}

/// Adds a track's post-fader output to the returns it sends to, ramping each
/// send from its previous gain to the new one across the segment.
pub(super) fn add_sends(
    returns: &mut [EngineReturn],
    target: &[f32; MAX_RETURNS],
    current: &mut [f32; MAX_RETURNS],
    l: &[f32],
    r: &[f32],
    offset: usize,
) {
    let frames = l.len();
    let inv = 1.0 / frames.max(1) as f32;
    for (k, ret) in returns.iter_mut().enumerate().take(MAX_RETURNS) {
        let (g0, g1) = (current[k], target[k]);
        current[k] = g1;
        if g0 == 0.0 && g1 == 0.0 {
            continue;
        }
        let (bl, br) = (
            &mut ret.buf_l[offset..offset + frames],
            &mut ret.buf_r[offset..offset + frames],
        );
        for i in 0..frames {
            let g = g0 + (g1 - g0) * (i + 1) as f32 * inv;
            bl[i] += l[i] * g;
            br[i] += r[i] * g;
        }
    }
}

pub(super) fn clear_returns(returns: &mut [EngineReturn], n: usize) {
    for ret in returns {
        ret.buf_l[..n].fill(0.0);
        ret.buf_r[..n].fill(0.0);
    }
}

/// Runs each return's devices and mixes it into the master bus.
pub(super) fn mix_returns(
    returns: &mut [EngineReturn],
    master_l: &mut [f32],
    master_r: &mut [f32],
    ctx: &FxContext,
    meter_decay: f32,
) {
    let n = master_l.len();
    let inv = 1.0 / n.max(1) as f32;
    for ret in returns {
        let (bl, br) = (&mut ret.buf_l[..n], &mut ret.buf_r[..n]);
        ret.chain.process(bl, br, ctx);
        let (pl, pr) = pan_gains(ret.mixer.pan);
        let g = if ret.mixer.mute {
            0.0
        } else {
            db_to_gain(ret.mixer.volume_db)
        };
        let (tl, tr) = (g * pl, g * pr);
        let (sl, sr) = (ret.gain_l, ret.gain_r);
        let mut peak_l = 0.0f32;
        let mut peak_r = 0.0f32;
        for i in 0..n {
            let t = (i + 1) as f32 * inv;
            let l = bl[i] * (sl + (tl - sl) * t);
            let r = br[i] * (sr + (tr - sr) * t);
            peak_l = peak_l.max(l.abs());
            peak_r = peak_r.max(r.abs());
            master_l[i] += l;
            master_r[i] += r;
        }
        ret.gain_l = tl;
        ret.gain_r = tr;
        ret.meter_l = peak_l.max(ret.meter_l * meter_decay);
        ret.meter_r = peak_r.max(ret.meter_r * meter_decay);
    }
}
