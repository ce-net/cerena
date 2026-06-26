//! Signed-distance field toolkit — the organic-shape builder.
//!
//! A signed-distance function (SDF) returns, for any point, the distance to the
//! nearest surface: negative inside the shape, positive outside, zero on the surface.
//! We build all of Cerena's organic geometry by combining SDF primitives with
//! *smooth* operators and then extracting the `iso = 0` surface in [`crate::mesh`].
//!
//! ## Why smooth-min makes everything organic
//!
//! A plain union of two SDFs is `min(a, b)` — but that leaves a hard crease exactly
//! where the two shapes meet. [`smin`] (polynomial smooth-min) instead blends the two
//! distance fields over a radius `k`, rounding the join into a smooth fillet. Build a
//! creature from a few capsules joined with `smin` and it reads as one grown body,
//! not welded parts. This single trick is the reason Cerena has "non-sharp shapes".
//!
//! Convention used throughout the crate: **inside is negative, outside is positive,
//! and the field increases outward**, so the surface gradient points out of the
//! shape. [`crate::mesh`] relies on this to compute outward normals.

use glam::{Vec2, Vec3};

/// A boxed scalar field over space. Surface extraction consumes `&dyn Fn(Vec3) -> f32`
/// directly, but this alias documents the shape of the things we pass around.
pub type Field<'a> = dyn Fn(Vec3) -> f32 + 'a;

/// Polynomial smooth-minimum. Blends `a` and `b` over a radius `k`: for `k -> 0` it
/// degenerates to `a.min(b)`, but for `k > 0` it rounds the transition. The result is
/// always `<= a.min(b)` near the blend, which is what carves the smooth fillet.
pub fn smin(a: f32, b: f32, k: f32) -> f32 {
    if k <= 0.0 {
        return a.min(b);
    }
    // h in [0, 1] measures how close a and b are relative to the blend radius.
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

/// Smooth-maximum, the dual of [`smin`]. Used for smooth intersection / subtraction.
pub fn smax(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, -b, k)
}

// --- Primitives (all centred at the origin / explicit positions) --------------

/// Sphere of radius `r` centred at the origin.
pub fn sd_sphere(p: Vec3, r: f32) -> f32 {
    p.length() - r
}

/// Capsule: a cylinder of radius `r` with hemispherical caps, from `a` to `b`. The
/// natural primitive for limbs and tendrils — no hard ends to break the organic look.
pub fn sd_capsule(p: Vec3, a: Vec3, b: Vec3, r: f32) -> f32 {
    let pa = p - a;
    let ba = b - a;
    // Project p onto the segment, clamped to its ends.
    let h = (pa.dot(ba) / ba.dot(ba).max(f32::EPSILON)).clamp(0.0, 1.0);
    (pa - ba * h).length() - r
}

/// Rounded box: half-extents `b`, corner radius `r`. Even our "boxy" shapes are
/// rounded so nothing reads as a hard-edged voxel.
pub fn sd_round_box(p: Vec3, b: Vec3, r: f32) -> f32 {
    let q = p.abs() - b;
    q.max(Vec3::ZERO).length() + q.max_element().min(0.0) - r
}

/// Torus in the XZ plane: `major` ring radius, `minor` tube radius.
pub fn sd_torus(p: Vec3, major: f32, minor: f32) -> f32 {
    let q = Vec2::new(Vec2::new(p.x, p.z).length() - major, p.y);
    q.length() - minor
}

/// Plane with unit normal `n` at signed offset `h` from the origin.
pub fn sd_plane(p: Vec3, n: Vec3, h: f32) -> f32 {
    p.dot(n) + h
}

// --- Operators (smooth blends keep joins organic) ------------------------------

/// Smooth union: the two shapes merge with a rounded fillet of radius `k`.
pub fn op_union_smooth(a: f32, b: f32, k: f32) -> f32 {
    smin(a, b, k)
}

/// Smooth subtraction: carve `b` out of `a` with a rounded lip of radius `k`.
pub fn op_subtract_smooth(a: f32, b: f32, k: f32) -> f32 {
    smax(a, -b, k)
}

/// Smooth intersection: keep only where both shapes overlap, rounded by `k`.
pub fn op_intersect_smooth(a: f32, b: f32, k: f32) -> f32 {
    smax(a, b, k)
}

/// Wrap a field so its surface is pushed in and out by fractal noise — the cheap way
/// to add high-frequency organic detail (bark, scales, pores) to an otherwise smooth
/// shape. Subtracting the noise from the distance moves the `iso = 0` surface outward
/// where the noise is positive, giving bumps without changing the base silhouette.
pub fn displaced<'a>(
    field: impl Fn(Vec3) -> f32 + 'a,
    amplitude: f32,
    frequency: f32,
    seed: u32,
) -> impl Fn(Vec3) -> f32 + 'a {
    move |p: Vec3| field(p) - amplitude * crate::noise_eval::fbm(p * frequency, 4, 2.0, 0.5, seed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smin_is_smaller_than_min_in_the_blend() {
        // When a and b are within k of each other, the smooth-min dips below the hard
        // min — that dip is the rounded fillet that makes joins organic.
        let (a, b, k) = (0.5f32, 0.6f32, 0.4f32);
        assert!(
            smin(a, b, k) < a.min(b),
            "smin should round below the hard min near the blend"
        );
        // Far apart relative to k, smin collapses back to the hard min.
        assert!((smin(0.0, 10.0, 0.4) - 0.0).abs() < 1e-5);
    }

    #[test]
    fn sphere_sdf_signs() {
        assert!(sd_sphere(Vec3::ZERO, 1.0) < 0.0, "centre is inside");
        assert!(sd_sphere(Vec3::new(2.0, 0.0, 0.0), 1.0) > 0.0, "far is outside");
    }
}
