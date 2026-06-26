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
    fn creature_mesh_is_deterministic() {
        let a = generate_creature_mesh(&test_mob(0xABCD, 1.5), 20);
        let b = generate_creature_mesh(&test_mob(0xABCD, 1.5), 20);
        assert_eq!(a.positions.len(), b.positions.len());
        if !a.positions.is_empty() {
            assert_eq!(a.positions[0], b.positions[0]);
        }
    }
}
