//! Entity interpolation for remote players.
//!
//! Remote entities are authoritative-only: we never predict them because we don't
//! know their intent. Snapshots arrive at 20 Hz, but we render at display rate
//! (60+ fps), so naively snapping to the latest snapshot would look choppy.
//! Instead we render the remote world *slightly in the past* — at
//! `render_tick = estimated_server_tick - INTERP_DELAY_TICKS` — and linearly
//! interpolate each entity between the two authoritative samples that bracket that
//! render time. The deliberate delay guarantees there are almost always two
//! samples to interpolate between; if the buffer runs dry we hold the nearest
//! known state rather than extrapolate (extrapolation overshoots and rubber-bands).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::f32::consts::PI;

use arena_protocol::{EntityId, TICKS_PER_SNAPSHOT, Tick, entity::EntityState};
use glam::Vec3;

/// How far behind the estimated server tick we render remote entities, in ticks.
/// ~2 snapshots' worth: long enough to always bracket the render time even with
/// one dropped/late snapshot, short enough that remote players don't feel laggy.
pub const INTERP_DELAY_TICKS: f32 = 2.0 * TICKS_PER_SNAPSHOT as f32;

/// How many authoritative samples to retain. Comfortably more than the delay so a
/// burst of late packets still finds a bracket.
pub const INTERP_HISTORY: usize = 32;

/// One buffered authoritative frame: the snapshot tick and the remote entities it
/// carried (the local player is excluded by the caller).
#[derive(Debug, Clone)]
struct Sample {
    tick: Tick,
    entities: HashMap<EntityId, EntityState>,
}

/// A time-ordered ring of recent authoritative remote-entity states, sampled by
/// fractional render tick.
#[derive(Debug, Default)]
pub struct InterpolationBuffer {
    /// Oldest first, ticks strictly increasing.
    samples: VecDeque<Sample>,
}

impl InterpolationBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Push the remote entities from a snapshot at `tick`. Out-of-order (stale)
    /// snapshots are dropped — we only ever move forward in time. The local player
    /// must already have been filtered out by the caller.
    pub fn push(&mut self, tick: Tick, entities: HashMap<EntityId, EntityState>) {
        if let Some(last) = self.samples.back() {
            if tick <= last.tick {
                return; // reordered or duplicate; keep the monotonic invariant
            }
        }
        self.samples.push_back(Sample { tick, entities });
        while self.samples.len() > INTERP_HISTORY {
            self.samples.pop_front();
        }
    }

    /// The newest buffered tick, if any (diagnostics / render gating).
    pub fn latest_tick(&self) -> Option<Tick> {
        self.samples.back().map(|s| s.tick)
    }

    /// Sample the remote world at fractional render tick `render_tick_f`.
    ///
    /// - Before the earliest sample: hold the earliest (just-joined / cold buffer).
    /// - After the latest sample: hold the latest (we never extrapolate forward).
    /// - Otherwise: lerp position/velocity and shortest-arc-lerp yaw/pitch between
    ///   the two bracketing samples.
    pub fn sample(&self, render_tick_f: f32) -> HashMap<EntityId, EntityState> {
        if self.samples.is_empty() {
            return HashMap::new();
        }
        let first = self.samples.front().unwrap();
        let last = self.samples.back().unwrap();

        if render_tick_f <= first.tick as f32 {
            return first.entities.clone();
        }
        if render_tick_f >= last.tick as f32 {
            return last.entities.clone();
        }

        // Find the bracket [a, b] with a.tick <= render < b.tick. The buffer is
        // small (<= INTERP_HISTORY) so a linear scan is cheap and branch-friendly.
        let mut a = first;
        let mut b = last;
        for window in self.samples.iter().zip(self.samples.iter().skip(1)) {
            let (lo, hi) = window;
            if (lo.tick as f32) <= render_tick_f && render_tick_f < hi.tick as f32 {
                a = lo;
                b = hi;
                break;
            }
        }

        let span = (b.tick - a.tick) as f32;
        let alpha = if span > 0.0 {
            ((render_tick_f - a.tick as f32) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let mut out = HashMap::with_capacity(a.entities.len());
        for (id, sa) in &a.entities {
            let state = match b.entities.get(id) {
                Some(sb) => lerp_state(sa, sb, alpha),
                // Present in `a` but not `b` (left view between samples): hold `a`.
                None => sa.clone(),
            };
            out.insert(*id, state);
        }
        out
    }
}

/// Linearly interpolate the continuous fields of an entity; discrete fields
/// (health, flags, team, weapon, kind, owner) are taken from the older sample `a`
/// so they change on snapshot boundaries rather than smearing.
fn lerp_state(a: &EntityState, b: &EntityState, t: f32) -> EntityState {
    let mut s = a.clone();
    s.pos = a.pos.lerp(b.pos, t);
    s.vel = a.vel.lerp(b.vel, t);
    s.yaw = lerp_angle(a.yaw, b.yaw, t);
    // Pitch is bounded to [-pi/2, pi/2] and never wraps, so a plain lerp is right.
    s.pitch = a.pitch + (b.pitch - a.pitch) * t;
    s
}

/// Shortest-arc interpolation between two angles in radians. Avoids the full-circle
/// spin you'd get from a naive lerp when an entity turns across the +/-pi seam.
fn lerp_angle(a: f32, b: f32, t: f32) -> f32 {
    // Wrap the delta into (-pi, pi] then walk a fraction of it.
    let mut delta = (b - a) % (2.0 * PI);
    if delta > PI {
        delta -= 2.0 * PI;
    } else if delta < -PI {
        delta += 2.0 * PI;
    }
    a + delta * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::{
        entity::{EntityFlags, EntityKind},
        world::Team,
    };

    fn ent(id: EntityId, x: f32, yaw: f32) -> EntityState {
        EntityState {
            id,
            kind: EntityKind::Player,
            pos: Vec3::new(x, 0.0, 0.0),
            vel: Vec3::ZERO,
            yaw,
            pitch: 0.0,
            flags: EntityFlags::default(),
            team: Team::None,
            health: 100,
            armor: 0,
            weapon: 0,
            owner: String::new(),
        }
    }

    #[test]
    fn midpoint_lerps_halfway() {
        let mut buf = InterpolationBuffer::new();
        let mut a = HashMap::new();
        a.insert(7, ent(7, 0.0, 0.0));
        let mut b = HashMap::new();
        b.insert(7, ent(7, 10.0, 0.0));
        buf.push(0, a);
        buf.push(10, b);

        let mid = buf.sample(5.0);
        let e = mid.get(&7).unwrap();
        assert!((e.pos.x - 5.0).abs() < 1e-4, "got {}", e.pos.x);
    }

    #[test]
    fn holds_last_when_render_ahead_of_buffer() {
        let mut buf = InterpolationBuffer::new();
        let mut a = HashMap::new();
        a.insert(1, ent(1, 3.0, 0.0));
        buf.push(10, a);
        // Render time past the newest sample → hold last, never extrapolate.
        let s = buf.sample(99.0);
        assert!((s.get(&1).unwrap().pos.x - 3.0).abs() < 1e-6);
    }

    #[test]
    fn yaw_takes_short_arc_across_seam() {
        // From +170deg to -170deg is a 20deg step the short way, not 340deg.
        let from = 170f32.to_radians();
        let to = (-170f32).to_radians();
        let mid = lerp_angle(from, to, 0.5);
        // Halfway should be at +/-180deg (== +/-pi), not near 0.
        assert!(mid.abs() > 175f32.to_radians(), "mid={} rad", mid);
    }
}
