//! Reusable deterministic physics primitives, shared across the simulation.
//!
//! `arena-sim` already has the player-specific locomotion ([`crate::movement`]) and the
//! geometry queries ([`crate::collision`]). This module fills the gap between them with
//! the *general* pieces every other system reaches for:
//!
//! - [`SpatialHash`] — a uniform-grid broadphase for O(1)-ish neighbour queries among
//!   thousands of entities. Reused by area-of-interest culling, the `Area`/`Cone` spell
//!   target selection, and mob separation.
//! - [`integrate_body`] — a semi-implicit kinematic integrator (gravity + drag +
//!   collision slide) for any non-player body: mobs, physics pickups, debris.
//! - [`step_projectile`] — ballistic projectile advance (gravity, drag, homing) with
//!   world-collision detection. The substrate under the spell VM's `Projectile` op.
//! - steering ([`seek`]/[`arrive`]/[`flee`]/[`separation`]) — desired-velocity helpers
//!   for mob AI.
//! - [`exp_smooth`] / [`exp_smooth_v3`] — frame-rate-independent critically-damped
//!   smoothing for cameras, reconciliation easing, and anything that should ease.
//!
//! Everything is **deterministic** (no clocks, no RNG) and **`wasm32`-clean**, matching
//! the rest of the sim, so an authority and a predicting client compute the same
//! trajectories and the same neighbour sets.

use std::collections::HashMap;

use glam::Vec3;

use arena_protocol::world::Aabb;

use crate::collision;

// ===========================================================================
// Spatial hash — uniform-grid broadphase for neighbour queries.
// ===========================================================================

/// A uniform spatial hash over points in 3D. Insert `(id, position)` pairs, then query
/// a sphere to get the ids whose points fall inside it — without scanning every entity.
/// Cell size should be ~the typical query radius for the best bucket occupancy.
#[derive(Debug, Clone)]
pub struct SpatialHash {
    cell: f32,
    inv_cell: f32,
    buckets: HashMap<(i32, i32, i32), Vec<(u32, Vec3)>>,
}

impl SpatialHash {
    /// Create an empty hash with the given cell size (world units).
    pub fn new(cell: f32) -> Self {
        let cell = cell.max(0.01);
        Self { cell, inv_cell: 1.0 / cell, buckets: HashMap::new() }
    }

    /// Build a hash directly from an iterator of `(id, position)`.
    pub fn from_points(cell: f32, points: impl IntoIterator<Item = (u32, Vec3)>) -> Self {
        let mut h = Self::new(cell);
        for (id, p) in points {
            h.insert(id, p);
        }
        h
    }

    fn key(&self, p: Vec3) -> (i32, i32, i32) {
        (
            (p.x * self.inv_cell).floor() as i32,
            (p.y * self.inv_cell).floor() as i32,
            (p.z * self.inv_cell).floor() as i32,
        )
    }

    /// Insert an entity at a position.
    pub fn insert(&mut self, id: u32, pos: Vec3) {
        self.buckets.entry(self.key(pos)).or_default().push((id, pos));
    }

    /// Empty the hash, keeping its allocated buckets for reuse next tick.
    pub fn clear(&mut self) {
        for v in self.buckets.values_mut() {
            v.clear();
        }
    }

    /// Ids whose points lie within `radius` of `center`. Scans only the cells the
    /// sphere overlaps, then exact-distance filters. Order is unspecified.
    pub fn query_radius(&self, center: Vec3, radius: f32) -> Vec<u32> {
        let mut out = Vec::new();
        self.for_each_in_radius(center, radius, |id, _| out.push(id));
        out
    }

    /// Visit every `(id, pos)` within `radius` of `center`, allocation-free. The hot
    /// path for AoI and area-effect target selection.
    pub fn for_each_in_radius(&self, center: Vec3, radius: f32, mut f: impl FnMut(u32, Vec3)) {
        let r2 = radius * radius;
        let lo = self.key(center - Vec3::splat(radius));
        let hi = self.key(center + Vec3::splat(radius));
        for cx in lo.0..=hi.0 {
            for cy in lo.1..=hi.1 {
                for cz in lo.2..=hi.2 {
                    if let Some(bucket) = self.buckets.get(&(cx, cy, cz)) {
                        for &(id, p) in bucket {
                            if (p - center).length_squared() <= r2 {
                                f(id, p);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Number of inserted points (across all buckets).
    pub fn len(&self) -> usize {
        self.buckets.values().map(|v| v.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.buckets.values().all(|v| v.is_empty())
    }
}

// ===========================================================================
// Kinematic body integration — mobs, pickups, debris.
// ===========================================================================

/// A simple dynamic body: a vertical capsule with a position and velocity. Players use
/// the richer [`crate::movement`] controller; everything else (mobs, thrown items,
/// gibs) can ride this.
#[derive(Debug, Clone, Copy)]
pub struct KinematicBody {
    pub pos: Vec3,
    pub vel: Vec3,
    pub radius: f32,
    pub half_height: f32,
}

/// Integration tunables for a [`KinematicBody`].
#[derive(Debug, Clone, Copy)]
pub struct BodyParams {
    /// Downward acceleration (negative). 0 for a floating/flying body.
    pub gravity: f32,
    /// Linear drag per second (0 = frictionless, ~2..8 = quick settle).
    pub drag: f32,
    /// Optional horizontal speed clamp (`None` = unclamped).
    pub max_speed: Option<f32>,
}

impl Default for BodyParams {
    fn default() -> Self {
        Self { gravity: -20.0, drag: 0.0, max_speed: None }
    }
}

/// Advance a body one step: apply gravity + drag, optionally clamp horizontal speed,
/// then sweep it through the static world (sliding off geometry). Returns whether it
/// ended the step grounded. Mutates `body` in place.
pub fn integrate_body(
    body: &mut KinematicBody,
    params: &BodyParams,
    dt: f32,
    brushes: &[Aabb],
    terrain: Option<&crate::map::Terrain>,
) -> bool {
    body.vel.y += params.gravity * dt;
    if params.drag > 0.0 {
        // Implicit-ish damping: always stable, never reverses the sign.
        let k = 1.0 / (1.0 + params.drag * dt);
        body.vel.x *= k;
        body.vel.z *= k;
    }
    if let Some(max) = params.max_speed {
        let horiz = Vec3::new(body.vel.x, 0.0, body.vel.z);
        let sp = horiz.length();
        if sp > max && sp > 1e-5 {
            let s = max / sp;
            body.vel.x *= s;
            body.vel.z *= s;
        }
    }
    let res = collision::resolve_move(body.pos, body.vel, dt, body.half_height, body.radius, brushes, terrain);
    body.pos = res.pos;
    body.vel = res.vel;
    res.on_ground
}

// ===========================================================================
// Ballistics — projectiles (the substrate under the spell `Projectile` op).
// ===========================================================================

/// A flying projectile: position, velocity, size, and its flight modifiers.
#[derive(Debug, Clone, Copy)]
pub struct Projectile {
    pub pos: Vec3,
    pub vel: Vec3,
    pub radius: f32,
    /// Downward acceleration (0 = straight-flying bolt; >0 = arcing meteor/grenade).
    pub gravity: f32,
    /// Air drag per second.
    pub drag: f32,
    /// Homing strength toward the steer target (0 = dumb-fire, ~0.5..1 = strong lock).
    pub homing: f32,
    /// Remaining lifetime (seconds); the projectile expires at 0.
    pub lifetime: f32,
}

/// The outcome of one [`step_projectile`] call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BallisticHit {
    /// Still in flight.
    Flying,
    /// Struck the static world at `point` with surface `normal`.
    World { point: Vec3, normal: Vec3 },
    /// Lifetime ran out this step (detonate at the current position).
    Expired,
}

/// Advance a projectile one step. Applies gravity/drag, steers toward `steer_target`
/// (if homing), then sweeps the travel segment against the world; if it crosses
/// geometry it reports a [`BallisticHit::World`] at the impact. Decrements lifetime and
/// reports [`BallisticHit::Expired`] when it runs out (so the caller fires the on-hit /
/// timeout effect). Mutates `proj` in place.
pub fn step_projectile(
    proj: &mut Projectile,
    steer_target: Option<Vec3>,
    dt: f32,
    brushes: &[Aabb],
) -> BallisticHit {
    // Lifetime first: an expired projectile detonates where it is.
    proj.lifetime -= dt;
    if proj.lifetime <= 0.0 {
        return BallisticHit::Expired;
    }

    // Homing: bend velocity toward the target while preserving speed, so a homing bolt
    // curves rather than snaps. Deterministic (pure vector math).
    if proj.homing > 0.0 {
        if let Some(t) = steer_target {
            let speed = proj.vel.length();
            if speed > 1e-4 {
                let desired = (t - proj.pos).normalize_or_zero() * speed;
                let blended = proj.vel.lerp(desired, (proj.homing * dt * 6.0).clamp(0.0, 1.0));
                proj.vel = blended.normalize_or_zero() * speed;
            }
        }
    }

    // `gravity` is stored as the magnitude of downward acceleration, so subtract it.
    proj.vel.y -= proj.gravity * dt;
    if proj.drag > 0.0 {
        proj.vel *= 1.0 / (1.0 + proj.drag * dt);
    }

    // Sweep the step segment against the world.
    let step = proj.vel * dt;
    let dist = step.length();
    if dist > 1e-6 {
        let dir = step / dist;
        if let Some((t, n)) = collision::raycast_aabbs(proj.pos, dir, dist + proj.radius, brushes) {
            let point = proj.pos + dir * t;
            proj.pos = point;
            return BallisticHit::World { point, normal: n };
        }
    }
    proj.pos += step;
    BallisticHit::Flying
}

// ===========================================================================
// Steering — desired-velocity helpers for mob AI.
// ===========================================================================

/// Steer toward `target` at up to `max_speed`: returns the desired *velocity*. The
/// classic "seek". Combine with the mob's current velocity for smooth turning.
pub fn seek(pos: Vec3, target: Vec3, max_speed: f32) -> Vec3 {
    (target - pos).normalize_or_zero() * max_speed
}

/// Steer away from `threat` at up to `max_speed`.
pub fn flee(pos: Vec3, threat: Vec3, max_speed: f32) -> Vec3 {
    (pos - threat).normalize_or_zero() * max_speed
}

/// Like [`seek`] but eases to a stop within `slow_radius` of the target, so a mob
/// doesn't jitter or overshoot when it arrives.
pub fn arrive(pos: Vec3, target: Vec3, max_speed: f32, slow_radius: f32) -> Vec3 {
    let to = target - pos;
    let dist = to.length();
    if dist < 1e-4 {
        return Vec3::ZERO;
    }
    let speed = if dist < slow_radius {
        max_speed * (dist / slow_radius.max(1e-4))
    } else {
        max_speed
    };
    to / dist * speed
}

/// A separation push away from nearby crowd members, so a pack doesn't pile into one
/// point. `neighbours` are the other bodies' positions; returns a desired velocity
/// scaled by `strength`, weighted by closeness (closer = stronger push).
pub fn separation(pos: Vec3, neighbours: &[Vec3], desired_spacing: f32, strength: f32) -> Vec3 {
    let mut push = Vec3::ZERO;
    let sp = desired_spacing.max(1e-3);
    for &n in neighbours {
        let away = pos - n;
        let d = away.length();
        if d > 1e-4 && d < sp {
            // Inverse-falloff so the push grows sharply as bodies overlap.
            push += away / d * (1.0 - d / sp);
        }
    }
    if push.length_squared() > 1e-8 {
        push.normalize_or_zero() * strength
    } else {
        Vec3::ZERO
    }
}

// ===========================================================================
// Smoothing — frame-rate-independent easing (camera, reconciliation, fx).
// ===========================================================================

/// Critically-damped exponential smoothing of a scalar toward `target` with a given
/// `half_life` (seconds to close half the gap). Frame-rate independent: the result for
/// a fixed total time is the same whether stepped in one big `dt` or many small ones.
pub fn exp_smooth(current: f32, target: f32, half_life: f32, dt: f32) -> f32 {
    let a = 1.0 - (-dt * std::f32::consts::LN_2 / half_life.max(1e-4)).exp();
    current + (target - current) * a
}

/// [`exp_smooth`] for a `Vec3` (per-axis). For camera positions, look targets, etc.
pub fn exp_smooth_v3(current: Vec3, target: Vec3, half_life: f32, dt: f32) -> Vec3 {
    let a = 1.0 - (-dt * std::f32::consts::LN_2 / half_life.max(1e-4)).exp();
    current + (target - current) * a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spatial_hash_finds_only_nearby() {
        let hash = SpatialHash::from_points(
            2.0,
            [
                (1, Vec3::new(0.0, 0.0, 0.0)),
                (2, Vec3::new(1.0, 0.0, 0.0)),
                (3, Vec3::new(50.0, 0.0, 0.0)),
            ],
        );
        let mut near = hash.query_radius(Vec3::ZERO, 3.0);
        near.sort_unstable();
        assert_eq!(near, vec![1, 2], "distant point 3 must be excluded");
        assert_eq!(hash.len(), 3);
    }

    #[test]
    fn body_falls_under_gravity() {
        let mut body = KinematicBody { pos: Vec3::new(0.0, 10.0, 0.0), vel: Vec3::ZERO, radius: 0.4, half_height: 0.9 };
        let params = BodyParams::default();
        let before = body.pos.y;
        integrate_body(&mut body, &params, 1.0 / 64.0, &[], None);
        assert!(body.pos.y < before, "an unsupported body should fall");
    }

    #[test]
    fn projectile_expires_then_hits_world() {
        // Expiry path.
        let mut p = Projectile { pos: Vec3::ZERO, vel: Vec3::X * 10.0, radius: 0.2, gravity: 0.0, drag: 0.0, homing: 0.0, lifetime: 0.0 };
        assert_eq!(step_projectile(&mut p, None, 1.0 / 64.0, &[]), BallisticHit::Expired);

        // World-hit path: a wall at x = 1.
        let wall = Aabb::new(Vec3::new(1.0, -5.0, -5.0), Vec3::new(1.2, 5.0, 5.0));
        let mut q = Projectile { pos: Vec3::ZERO, vel: Vec3::X * 100.0, radius: 0.1, gravity: 0.0, drag: 0.0, homing: 0.0, lifetime: 5.0 };
        match step_projectile(&mut q, None, 1.0 / 64.0, &[wall]) {
            BallisticHit::World { point, .. } => assert!((point.x - 1.0).abs() < 0.2, "hit near the wall face, got {point:?}"),
            other => panic!("expected a world hit, got {other:?}"),
        }
    }

    #[test]
    fn arrive_slows_near_target() {
        let far = arrive(Vec3::ZERO, Vec3::new(100.0, 0.0, 0.0), 5.0, 3.0).length();
        let near = arrive(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), 5.0, 3.0).length();
        assert!((far - 5.0).abs() < 1e-4, "full speed when far");
        assert!(near < far, "slower when within the slow radius");
    }

    #[test]
    fn exp_smooth_is_framerate_independent() {
        // One big step vs many small steps over the same total time should agree.
        let one = exp_smooth(0.0, 1.0, 0.2, 0.5);
        let mut many = 0.0;
        for _ in 0..32 {
            many = exp_smooth(many, 1.0, 0.2, 0.5 / 32.0);
        }
        assert!((one - many).abs() < 1e-3, "smoothing should be framerate independent: {one} vs {many}");
    }
}
