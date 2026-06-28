//! Procedural creature and summon meshes.
//!
//! Mobs are not modelled by hand: each [`MobDef`] carries a `mesh_seed`, and we grow a
//! body from it. The body is a handful of SDF primitives — a torso capsule, a head
//! sphere, and a few limbs — fused with **smooth-min** ([`crate::sdf::op_union_smooth`])
//! and then roughened with noise displacement. Smooth-min is what makes the parts read
//! as one grown organism rather than a stack of separate shapes, and the per-seed
//! jitter is why two wisps, wraiths, or golems sharing a def still look like distinct
//! individuals rather than clones.
//!
//! All of it is deterministic in `mesh_seed`, so server and clients grow the same mob.

use arena_content::mob::MobDef;
use arena_protocol::world::Aabb;
use glam::Vec3;

use crate::mesh::{surface_nets, Mesh};
use crate::noise_eval::Rng;
use crate::sdf::{displaced, op_union_smooth, sd_capsule, sd_sphere};

/// One SDF body part. Kept tiny so a creature is just a list of these, blended.
enum Part {
    Sphere { center: Vec3, radius: f32 },
    Capsule { a: Vec3, b: Vec3, radius: f32 },
}

impl Part {
    fn distance(&self, p: Vec3) -> f32 {
        match *self {
            Part::Sphere { center, radius } => sd_sphere(p - center, radius),
            Part::Capsule { a, b, radius } => sd_capsule(p, a, b, radius),
        }
    }
}

/// Grow the list of body parts for a creature from its seed. Built in a roughly unit-
/// sized local space (about `[-1, 1]`); the caller scales to `MobDef::scale`.
fn build_body(seed: u32) -> Vec<Part> {
    let mut rng = Rng::new(seed as u64 ^ 0xC0FF_EE17_BEEF_0001);
    let mut parts = Vec::new();

    // Torso: a vertical capsule, the spine of the creature. Slight per-seed variation
    // in girth and height makes silhouettes differ.
    let torso_r = rng.range(0.28, 0.42);
    let torso_h = rng.range(0.35, 0.55);
    parts.push(Part::Capsule {
        a: Vec3::new(0.0, -torso_h, 0.0),
        b: Vec3::new(0.0, torso_h, 0.0),
        radius: torso_r,
    });

    // Head: a sphere perched above the torso, offset a touch for character.
    let head_r = rng.range(0.22, 0.34);
    parts.push(Part::Sphere {
        center: Vec3::new(rng.range(-0.06, 0.06), torso_h + head_r * 0.7, 0.0),
        radius: head_r,
    });

    // Limbs: 2..5 capsules radiating from the torso at seeded angles, so creatures
    // range from squat bipeds to many-armed wraiths.
    let limb_count = 2 + (rng.next_u64() % 4) as usize;
    for _ in 0..limb_count {
        let angle = rng.range(0.0, std::f32::consts::TAU);
        let pitch = rng.range(-0.6, 0.4);
        let len = rng.range(0.4, 0.8);
        let dir = Vec3::new(angle.cos() * pitch.cos(), pitch.sin(), angle.sin() * pitch.cos());
        let attach = Vec3::new(0.0, rng.range(-torso_h * 0.5, torso_h * 0.5), 0.0);
        parts.push(Part::Capsule {
            a: attach,
            b: attach + dir * len,
            radius: rng.range(0.08, 0.16),
        });
    }

    parts
}

/// Generate the organic mesh for a mob. The body parts are smooth-blended into one SDF
/// and surface-displaced for skin detail, then extracted and scaled by `mob.scale`.
/// `res` is the Surface Nets grid resolution (higher = finer skin).
pub fn generate_creature_mesh(mob: &MobDef, res: usize) -> Mesh {
    let parts = build_body(mob.mesh_seed);

    // Blend radius scales the fillet between parts — the heart of the organic look.
    let blend = 0.18;
    let body = move |p: Vec3| -> f32 {
        let mut d = f32::INFINITY;
        for part in &parts {
            // smin(INF, x) == x, so the first part initialises the field cleanly.
            d = op_union_smooth(d, part.distance(p), blend);
        }
        d
    };

    // Add fine noise so the skin is bumpy/organic, not a smooth blob.
    let field = displaced(body, 0.04, 6.0, mob.mesh_seed ^ 0x5EED_5C1D);

    // Bounds comfortably enclose the unit-space body plus displacement.
    let bounds = Aabb::new(Vec3::splat(-1.4), Vec3::splat(1.4));
    let mut mesh = surface_nets(&field, bounds, res, 0.0);

    // Scale to the mob's visual size. Uniform scaling preserves the gradient normals.
    let s = if mob.scale > 0.0 { mob.scale } else { 1.0 };
    for p in &mut mesh.positions {
        p[0] *= s;
        p[1] *= s;
        p[2] *= s;
    }

    mesh
}

/// Grow the mesh for a **player avatar**, in the exact local space the renderer and
/// the sim's collision capsule share: the body is centred on the entity's `pos`
/// (the capsule centre), Y-up, total height `2 * STAND_HALF_HEIGHT` (1.8 m) and
/// `PLAYER_RADIUS` (0.4 m) thick — see `arena_sim::movement`. Keeping the visual
/// hull matched to the collision hull means a remote player's body is drawn where it
/// actually is hit, so what you see is what you shoot.
///
/// Unlike a mob, every player shares one silhouette (a smooth capsule torso with a
/// perched head), so the result is seed-free and built once at client start; the
/// per-player team colour comes from the renderer's instance tint, not the mesh, so
/// the vertices are left white. `res` is the Surface Nets grid resolution.
pub fn player_mesh(res: usize) -> Mesh {
    // Capsule centre at the origin. Spine endpoints sit `radius` in from each cap so
    // the total height is exactly 1.8 m (`hh` above and below centre).
    let hh = 0.9_f32; // STAND_HALF_HEIGHT
    let r = 0.4_f32; // PLAYER_RADIUS
    let spine = (hh - r).max(0.0); // 0.5
    let torso = Part::Capsule {
        a: Vec3::new(0.0, -spine, 0.0),
        b: Vec3::new(0.0, spine * 0.6, 0.0),
        radius: r,
    };
    // A head perched just under the top of the capsule, so the silhouette reads as a
    // figure rather than a pill. Kept inside the capsule's height so it never pokes
    // past the collision hull.
    let head = Part::Sphere {
        center: Vec3::new(0.0, hh - 0.32, 0.0),
        radius: 0.3,
    };
    let parts = [torso, head];

    let blend = 0.16;
    let body = move |p: Vec3| -> f32 {
        let mut d = f32::INFINITY;
        for part in &parts {
            d = op_union_smooth(d, part.distance(p), blend);
        }
        d
    };
    // A touch of displacement so the surface isn't a sterile primitive — but far less
    // than a mob's, to keep the humanoid silhouette clean.
    let field = displaced(body, 0.02, 5.0, 0x9173_0A5E);

    // Bounds enclose the 1.8 m-tall body plus the head and displacement margin.
    let bounds = Aabb::new(Vec3::new(-0.7, -1.1, -0.7), Vec3::new(0.7, 1.1, 0.7));
    surface_nets(&field, bounds, res, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::ids::MobId;

    fn test_mob(mesh_seed: u32, scale: f32) -> MobDef {
        MobDef {
            id: MobId::new("mob.test"),
            name: "test".into(),
            max_health: 100.0,
            move_speed: 4.0,
            abilities: vec![],
            xp_reward: 10,
            loot_table: vec![],
            material: None,
            scale,
            aggressive: false,
            mesh_seed,
        }
    }

    #[test]
    fn creature_mesh_is_non_empty() {
        let mesh = generate_creature_mesh(&test_mob(0x1234, 1.0), 24);
        assert!(!mesh.is_empty(), "a creature body should produce geometry");
        assert!(mesh.tri_count() > 0);
    }

    #[test]
    fn player_mesh_is_non_empty_and_capsule_sized() {
        let mesh = player_mesh(20);
        assert!(!mesh.is_empty(), "the player avatar should produce geometry");
        // The body must fit inside the collision hull it represents: 1.8 m tall,
        // 0.4 m radius, centred on the origin (allow a small displacement margin).
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for p in &mesh.positions {
            lo = lo.min(p[1]);
            hi = hi.max(p[1]);
            assert!(p[0].abs() < 0.55 && p[2].abs() < 0.55, "body wider than the capsule");
        }
        assert!(lo > -1.05 && hi < 1.05, "body taller than the capsule");
    }

    #[test]
    fn creature_mesh_is_deterministic() {
        let a = generate_creature_mesh(&test_mob(0xABCD, 1.5), 20);
        let b = generate_creature_mesh(&test_mob(0xABCD, 1.5), 20);
        assert_eq!(a.positions.len(), b.positions.len());
        if !a.positions.is_empty() {
            assert_eq!(a.positions[0], b.positions[0]);
        }
    }
}
