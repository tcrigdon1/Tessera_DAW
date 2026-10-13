//! Session clip launching: launch modes, per-clip launch quantization,
//! Repeat and follow actions. Runs on the audio thread.

use super::{EngineClip, EngineTrack, Queued, QueuedAction, TrackRuntime};
use crate::dsp::Rng;
use crate::model::{FollowKind, LaunchMode};
use crate::time::Quantization;

/// Smallest Repeat interval (for clips without launch quantization).
const MIN_REPEAT: f64 = 0.25;

fn launch_mode(t: &EngineTrack, slot: usize) -> Option<LaunchMode> {
    Some(t.slots.get(slot)?.as_ref()?.launch.mode)
}

/// The launch button of `slot` was pressed at session beat `now`.
pub(super) fn press(t: &mut EngineTrack, slot: usize, now: f64, global: Quantization, bpb: f64) {
    let Some(clip) = t.slots.get(slot).and_then(|c| c.as_ref()) else {
        // An empty slot stops the track (at the global quantization).
        t.rt.queued = Some(Queued {
            action: QueuedAction::Launch(slot),
            at: global.next_boundary(now, bpb),
        });
        return;
    };
    let q = clip.launch.quantization.unwrap_or(global);
    let at = q.next_boundary(now, bpb);
    let launching = matches!(
        t.rt.queued,
        Some(Queued {
            action: QueuedAction::Launch(s),
            ..
        }) if s == slot
    );
    let action = match clip.launch.mode {
        LaunchMode::Toggle if t.rt.playing == Some(slot) && !launching => QueuedAction::Stop,
        LaunchMode::Toggle if launching => QueuedAction::Stop,
        LaunchMode::Repeat => {
            let grid = q.beats(bpb).unwrap_or(MIN_REPEAT).max(MIN_REPEAT / 4.0);
            t.rt.repeat = Some((slot, at + grid, grid));
            QueuedAction::Launch(slot)
        }
        _ => QueuedAction::Launch(slot),
    };
    t.rt.queued = Some(Queued { action, at });
}

/// The launch button of `slot` was released: Gate and Repeat clips stop.
pub(super) fn release(t: &mut EngineTrack, slot: usize, now: f64, global: Quantization, bpb: f64) {
    let Some(mode) = launch_mode(t, slot) else { return };
    if !matches!(mode, LaunchMode::Gate | LaunchMode::Repeat) {
        return;
    }
    if t.rt.repeat.is_some_and(|(s, ..)| s == slot) {
        t.rt.repeat = None;
    }
    let queued_here = matches!(
        t.rt.queued,
        Some(Queued {
            action: QueuedAction::Launch(s),
            ..
        }) if s == slot
    );
    if t.rt.playing == Some(slot) || queued_here {
        let q = t.slots[slot]
            .as_ref()
            .and_then(|c| c.launch.quantization)
            .unwrap_or(global);
        t.rt.queued = Some(Queued {
            action: QueuedAction::Stop,
            at: q.next_boundary(now, bpb),
        });
    }
}

/// Beats from launch to the clip's follow action, if it has one.
pub(super) fn follow_time(clip: &EngineClip) -> Option<f64> {
    let f = clip.launch.follow;
    if !f.enabled {
        return None;
    }
    let t = if f.linked { clip.region.first_pass() } else { f.time };
    Some(t.max(1.0 / 64.0))
}

/// Queues the next Repeat restart or follow action if it falls before
/// session beat `until`. Call only when nothing else is queued.
pub(super) fn schedule(rt: &mut TrackRuntime, slots: &[Option<EngineClip>], until: f64) {
    if let Some((slot, next, grid)) = rt.repeat {
        if next < until {
            rt.repeat = Some((slot, next + grid, grid));
            rt.queued = Some(Queued {
                action: QueuedAction::Launch(slot),
                at: next,
            });
            return;
        }
    }
    let Some(at) = rt.follow_at else { return };
    if at >= until {
        return;
    }
    rt.follow_at = None;
    let Some(cur) = rt.playing.or(rt.last_slot) else { return };
    let Some(Some(clip)) = slots.get(cur) else { return };
    let f = clip.launch.follow;
    let kind = if rt.rng.next_unit() < f.chance_a { f.a } else { f.b };
    if let Some(action) = follow_target(kind, cur, slots, &mut rt.rng) {
        rt.queued = Some(Queued { action, at });
    }
}

/// What a follow action does from slot `cur`. Follow actions stay inside the
/// clip's group: the clips around it with no empty slot in between.
pub(super) fn follow_target(
    kind: FollowKind,
    cur: usize,
    slots: &[Option<EngineClip>],
    rng: &mut Rng,
) -> Option<QueuedAction> {
    let filled = |i: usize| slots.get(i).is_some_and(|s| s.is_some());
    let mut first = cur;
    while first > 0 && filled(first - 1) {
        first -= 1;
    }
    let mut last = cur;
    while filled(last + 1) {
        last += 1;
    }
    let n = last - first + 1;
    let pick = |rng: &mut Rng, k: usize| ((rng.next_unit() * k as f32) as usize).min(k - 1);
    let slot = match kind {
        FollowKind::NoAction => return None,
        FollowKind::Stop => return Some(QueuedAction::Stop),
        FollowKind::Again => cur,
        FollowKind::Previous => {
            if cur > first {
                cur - 1
            } else {
                last
            }
        }
        FollowKind::Next => {
            if cur < last {
                cur + 1
            } else {
                first
            }
        }
        FollowKind::First => first,
        FollowKind::Last => last,
        FollowKind::Any => first + pick(rng, n),
        FollowKind::Other => {
            if n == 1 {
                cur
            } else {
                // A random clip of the others: skip over the current one.
                let k = first + pick(rng, n - 1);
                if k >= cur {
                    k + 1
                } else {
                    k
                }
            }
        }
    };
    Some(QueuedAction::Launch(slot))
}
