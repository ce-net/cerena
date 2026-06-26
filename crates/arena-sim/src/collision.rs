//! Collision: capsule-vs-AABB sliding movement, world raycasts, and ray-vs-player
//! tests for hit detection.
//!
//! Players are modelled as **vertical capsules** (a cylinder capped by two
//! hemispheres). A capsule slides cleanly along walls and over the stepped ramps
//! in the test arena, and — crucially — gives a fair, well-defined target volume
//! for hitscan with a distinct head region for headshots.
//!
//! ## Conventions
//!
//! - A player's `pos` is the **centre** of its capsule. `half_height` is half the
//!   total standing/crouched height; `radius` is the cylinder radius.
//! - The capsule axis is always **vertical** (`+Y`). Players never tilt, so the
//!   ray-vs-capsule test specialises to a vertical cylinder + two end caps, which
//!   is both cheaper and easier to keep deterministic than a general capsule.

use glam::Vec3;

use arena_protocol::world::{Aabb, Team};

/// Height of the head region, measured down from the top of the capsule. A hit
/// landing within this band counts as a headshot.
pub const HEAD_HEIGHT_M: f32 = 0.3;

/// Result of sweeping a capsule through the world for one movement step.
#[derive(Debug, Clone, Copy)]
pub struct MoveResult {
    /// Final, de-penetrated position (capsule centre).
    pub pos: Vec3,
    /// Velocity after sliding (components into contact surfaces removed).
    pub vel: Vec3,
    /// True if the capsule is resting on a surface that is "floor enough" to
    /// stand and jump from (a contact normal pointing mostly up).
    pub on_ground: bool,
    /// The most-vertical contact normal encountered this step, if any. Handy for
    /// callers that want to know what they bumped (slope handling, telemetry).
    pub hit_normal: Option<Vec3>,
}

/// A snapshot of a player's capsule at a tick, kept in the world's history ring so
/// hitscan can be lag-compensated against where targets *were*.
#[derive(Debug, Clone, Copy)]
pub struct CapsuleSample {
    /// Capsule centre at the sampled tick.
    pub pos: Vec3,
    pub half_height: f32,
    pub radius: f32,
    pub team: Team,
    pub alive: bool,
}

/// Sweep a vertical capsule from `pos` with velocity `vel` over `dt`, sliding along
/// static `brushes`. Returns the resolved position/velocity and whether we ended up
/// grounded.
///
/// We integrate in a few substeps so a fast-moving player can't tunnel through a
/// thin wall in a single frame, and after each substep we iterate a short
/// de-penetration loop: find the deepest overlap, push out along its normal, and
/// kill the velocity component heading into that surface (slide, don't stop).
pub fn resolve_move(
    mut pos: Vec3,
    mut vel: Vec3,
    dt: f32,
    half_height: f32,
    radius: f32,
    brushes: &[Aabb],
) -> MoveResult {
    // 4 substeps is plenty at arena speeds (<15 m/s) vs 1 m thick walls.
    const SUBSTEPS: u32 = 4;
    // A handful of de-penetration passes resolves a player wedged in a corner
    // (two walls at once) without an expensive full solver.
    const DEPEN_PASSES: u32 = 4;

    let sdt = dt / SUBSTEPS as f32;
    let mut on_ground = false;
    let mut hit_normal = None;

    for _ in 0..SUBSTEPS {
        pos += vel * sdt;

        for _ in 0..DEPEN_PASSES {
            // Resolve the single deepest contact each pass; iterating handles the
            // rest. This keeps the push-out stable (no fighting normals).
            let mut deepest: Option<(Vec3, f32)> = None;
            for b in brushes {
                if let Some((n, depth)) = capsule_aabb_penetration(pos, half_height, radius, b) {
                    if deepest.map_or(true, |(_, d)| depth > d) {
                        deepest = Some((n, depth));
                    }
                }
            }
            match deepest {
                Some((n, depth)) => {
                    pos += n * depth;
                    // Remove only the velocity heading *into* the surface so the
                    // player slides along it instead of sticking.
                    let into = vel.dot(n);
                    if into < 0.0 {
                        vel -= n * into;
                    }
                    // A mostly-upward normal means we're standing on something.
                    if n.y > 0.7 {
                        on_ground = true;
                    }
                    // Track the most-vertical contact for the caller.
                    if hit_normal.map_or(true, |hn: Vec3| n.y > hn.y) {
                        hit_normal = Some(n);
                    }
                }
                None => break,
            }
        }
    }

    MoveResult {
        pos,
        vel,
        on_ground,
        hit_normal,
    }
}

/// The two sphere-centres of a vertical capsule given its centre, half-height and
/// radius. The segment between them is the capsule's spine.
fn capsule_segment(center: Vec3, half_height: f32, radius: f32) -> (Vec3, Vec3) {
    // The hemispherical caps eat `radius` off each end of the total height.
    let h = (half_height - radius).max(0.0);
    (center - Vec3::Y * h, center + Vec3::Y * h)
}

/// Penetration of a vertical capsule (centre `pos`) into an AABB, if any. Returns
/// `(normal, depth)` where `normal` points *out of* the box and `depth` is how far
/// to move along it to separate them.
///
/// Method: find the closest point on the capsule's vertical spine and the closest
/// point on the box, treat that as a sphere-vs-point test of radius `radius`. The
/// spine is vertical so X/Z of the closest spine point are just the capsule's X/Z;
/// only the Y has to be chosen against the box's Y extent.
fn capsule_aabb_penetration(
    pos: Vec3,
    half_height: f32,
    radius: f32,
    b: &Aabb,
) -> Option<(Vec3, f32)> {
    let (a, top) = capsule_segment(pos, half_height, radius);

    // Closest box point in X/Z is independent of Y (box X/Z ranges are constant).
    let qx = pos.x.clamp(b.min.x, b.max.x);
    let qz = pos.z.clamp(b.min.z, b.max.z);
    let (py, qy) = closest_y(a.y, top.y, b.min.y, b.max.y);

    let p = Vec3::new(pos.x, py, pos.z); // closest point on the spine
    let q = Vec3::new(qx, qy, qz); // closest point on the box
    let diff = p - q;
    let dist_sq = diff.length_squared();

    if dist_sq > 1e-12 {
        let dist = dist_sq.sqrt();
        if dist < radius {
            return Some((diff / dist, radius - dist));
        }
        None
    } else {
        // Spine point is *inside* the box: push out along the box face nearest to
        // `p`. Rare at our step sizes, but must be handled so a wedged player is
        // ejected rather than trapped. Depth includes the full radius.
        let (n, face) = min_face_exit(p, b);
        Some((n, face + radius))
    }
}

/// Pick the Y on the spine `[ya, yb]` and the Y on the box `[ymin, ymax]` that are
/// mutually closest. If the ranges overlap the distance is zero.
fn closest_y(ya: f32, yb: f32, ymin: f32, ymax: f32) -> (f32, f32) {
    if yb < ymin {
        // Whole spine sits below the box.
        (yb, ymin)
    } else if ya > ymax {
        // Whole spine sits above the box.
        (ya, ymax)
    } else {
        // Overlap: any shared Y works; clamp the overlap midpoint onto both.
        let mid = (ya.max(ymin) + yb.min(ymax)) * 0.5;
        let py = mid.clamp(ya, yb);
        (py, py.clamp(ymin, ymax))
    }
}

/// For a point known to be inside the box, the shortest push to the nearest face:
/// returns the outward normal and the distance to that face.
fn min_face_exit(p: Vec3, b: &Aabb) -> (Vec3, f32) {
    let (nx, dx) = if p.x - b.min.x < b.max.x - p.x {
        (Vec3::NEG_X, p.x - b.min.x)
    } else {
        (Vec3::X, b.max.x - p.x)
    };
    let (ny, dy) = if p.y - b.min.y < b.max.y - p.y {
        (Vec3::NEG_Y, p.y - b.min.y)
    } else {
        (Vec3::Y, b.max.y - p.y)
    };
    let (nz, dz) = if p.z - b.min.z < b.max.z - p.z {
        (Vec3::NEG_Z, p.z - b.min.z)
    } else {
        (Vec3::Z, b.max.z - p.z)
    };
    if dx <= dy && dx <= dz {
        (nx, dx)
    } else if dy <= dz {
        (ny, dy)
    } else {
        (nz, dz)
    }
}

/// Cast a ray against the static world. Returns the nearest hit within `max_dist`
/// as `(t, normal)` where the hit point is `origin + dir * t`. `dir` must be
/// normalised so `t` is a real distance. Used to block hitscan and projectiles on
/// geometry between shooter and target.
pub fn raycast_aabbs(
    origin: Vec3,
    dir: Vec3,
    max_dist: f32,
    brushes: &[Aabb],
) -> Option<(f32, Vec3)> {
    let mut best: Option<(f32, Vec3)> = None;
    for b in brushes {
        if let Some((t, n)) = ray_aabb(origin, dir, b) {
            if t >= 0.0 && t <= max_dist && best.map_or(true, |(bt, _)| t < bt) {
                best = Some((t, n));
            }
        }
    }
    best
}

/// Ray-vs-AABB via the slab method. Returns the entry `t` and the face normal at
/// entry. Handles axis-parallel rays (zero direction component) without dividing
/// by zero.
fn ray_aabb(origin: Vec3, dir: Vec3, b: &Aabb) -> Option<(f32, Vec3)> {
    let o = [origin.x, origin.y, origin.z];
    let d = [dir.x, dir.y, dir.z];
    let mn = [b.min.x, b.min.y, b.min.z];
    let mx = [b.max.x, b.max.y, b.max.z];

    let mut tmin = f32::NEG_INFINITY;
    let mut tmax = f32::INFINITY;
    let mut axis = 0usize;
    let mut sign = -1.0f32;

    for a in 0..3 {
        if d[a].abs() < 1e-8 {
            // Ray parallel to this slab: miss unless the origin is already within it.
            if o[a] < mn[a] || o[a] > mx[a] {
                return None;
            }
        } else {
            let inv = 1.0 / d[a];
            let mut t1 = (mn[a] - o[a]) * inv;
            let mut t2 = (mx[a] - o[a]) * inv;
            let mut s = -1.0; // entering through the min face by default
            if t1 > t2 {
                core::mem::swap(&mut t1, &mut t2);
                s = 1.0; // entering through the max face
            }
            if t1 > tmin {
                tmin = t1;
                axis = a;
                sign = s;
            }
            if t2 < tmax {
                tmax = t2;
            }
            if tmin > tmax {
                return None;
            }
        }
    }

    // Entry is tmin if we start outside; if tmin < 0 the origin is inside and the
    // first surface ahead is the exit at tmax.
    let t = if tmin >= 0.0 { tmin } else { tmax };
    if t < 0.0 {
        return None;
    }
    let mut n = Vec3::ZERO;
    match axis {
        0 => n.x = sign,
        1 => n.y = sign,
        _ => n.z = sign,
    }
    Some((t, n))
}

/// Ray-vs-(vertical) capsule for player hit detection. `capsule_base` is the
/// **feet** position (bottom of the capsule); the capsule's total height is
/// `2 * half_height`. `dir` must be normalised.
///
/// Returns `(t, point, is_head)`: the hit distance, the world hit point, and
/// whether it landed in the head band (top [`HEAD_HEIGHT_M`]).
pub fn ray_capsule(
    origin: Vec3,
    dir: Vec3,
    capsule_base: Vec3,
    half_height: f32,
    radius: f32,
) -> Option<(f32, Vec3, bool)> {
    let total_h = 2.0 * half_height;
    // Sphere-centre heights of the two caps.
    let a_y = capsule_base.y + radius;
    let b_y = capsule_base.y + total_h - radius;
    let (cx, cz) = (capsule_base.x, capsule_base.z);

    let mut best: Option<f32> = None;

    // Body: ray vs the infinite vertical cylinder (radius `radius` about the X/Z
    // axis), accepting only hits whose Y falls between the cap centres.
    let ox = origin.x - cx;
    let oz = origin.z - cz;
    let a = dir.x * dir.x + dir.z * dir.z;
    if a > 1e-8 {
        let b = 2.0 * (ox * dir.x + oz * dir.z);
        let c = ox * ox + oz * oz - radius * radius;
        let disc = b * b - 4.0 * a * c;
        if disc >= 0.0 {
            let sq = disc.sqrt();
            for t in [(-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a)] {
                if t >= 0.0 {
                    let y = origin.y + t * dir.y;
                    if y >= a_y && y <= b_y {
                        best = Some(best.map_or(t, |bt| bt.min(t)));
                    }
                }
            }
        }
    }

    // Caps: ray vs the two end spheres.
    for cy in [a_y, b_y] {
        if let Some(t) = ray_sphere(origin, dir, Vec3::new(cx, cy, cz), radius) {
            best = Some(best.map_or(t, |bt| bt.min(t)));
        }
    }

    let t = best?;
    let point = origin + dir * t;
    let head_min = capsule_base.y + total_h - HEAD_HEIGHT_M;
    Some((t, point, point.y >= head_min))
}

/// Nearest non-negative ray-sphere intersection distance, if the ray hits.
fn ray_sphere(origin: Vec3, dir: Vec3, center: Vec3, radius: f32) -> Option<f32> {
    let m = origin - center;
    let b = m.dot(dir);
    let c = m.length_squared() - radius * radius;
    // Ray starts outside and points away: no hit.
    if c > 0.0 && b > 0.0 {
        return None;
    }
    let disc = b * b - c;
    if disc < 0.0 {
        return None;
    }
    let t = -b - disc.sqrt();
    Some(t.max(0.0)) // clamp to 0 if the origin is inside the sphere
}

/// A near-vertical wall the capsule is touching (within a small skin), if any. Used
/// by parkour modes (wall-run, climb) that key off a horizontal contact normal.
/// Returns an outward, mostly-horizontal normal.
pub fn wall_contact(
    pos: Vec3,
    half_height: f32,
    radius: f32,
    brushes: &[Aabb],
) -> Option<Vec3> {
    // Probe with a slightly fattened capsule so a wall we are sliding along still
    // registers even when the base capsule is just shy of touching it.
    let probe = radius + 0.15;
    let mut best: Option<Vec3> = None;
    for b in brushes {
        if let Some((n, _depth)) = capsule_aabb_penetration(pos, half_height, probe, b) {
            if n.y.abs() < 0.5 {
                // Prefer the most horizontal normal we find.
                if best.map_or(true, |bn: Vec3| n.y.abs() < bn.y.abs()) {
                    best = Some(n);
                }
            }
        }
    }
    best
}

/// Translate a capsule by `delta`, stopping at the first blocking geometry. Used by
/// instantaneous moves (blink, teleport, dash positioning) that should not phase
/// through walls. Returns the furthest unobstructed position along `delta`.
pub fn clamp_translation(
    pos: Vec3,
    delta: Vec3,
    half_height: f32,
    radius: f32,
    brushes: &[Aabb],
) -> Vec3 {
    let dist = delta.length();
    if dist < 1e-6 {
        return pos;
    }
    // Step in increments of half the radius so we cannot skip a thin wall.
    let steps = (dist / (radius * 0.5)).ceil().max(1.0) as u32;
    let step = delta / steps as f32;
    let mut p = pos;
    for _ in 0..steps {
        let next = p + step;
        let blocked = brushes
            .iter()
            .any(|b| capsule_aabb_penetration(next, half_height, radius, b).is_some());
        if blocked {
            break;
        }
        p = next;
    }
    p
}
