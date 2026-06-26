//! Procedural spell VFX — organic effect meshes and particle descriptors.
//!
//! Spell visuals are generated per element so the world stays cohesive without an
//! artist hand-modelling every effect. Each element grows a distinct organic shape
//! (fire blooms upward, void coils into a torus, life sprouts tendrils) from SDF
//! primitives fused with smooth-min and roughened by noise, exactly like creatures.
//!
//! Alongside the mesh, [`spell_emitter`] returns an [`EmitterDesc`]: a small bundle of
//! particle parameters the client feeds to its GPU particle system. We do not simulate
//! particles here (that is real-time, client-side) — we only describe them.

use arena_protocol::world::Aabb;
use glam::Vec3;

use crate::mesh::{surface_nets, Mesh};
use crate::noise_eval::Rng;
use crate::sdf::{displaced, op_union_smooth, sd_capsule, sd_sphere, sd_torus};

/// Particle-emitter parameters for a spell, derived from its element. The client drives
/// its particle system from these; this crate never spawns or simulates particles.
#[derive(Debug, Clone, Copy)]
pub struct EmitterDesc {
    /// Particles spawned per second.
    pub rate: f32,
    /// Seconds each particle lives.
    pub lifetime: f32,
    /// Particle tint (linear RGBA).
    pub color: [f32; 4],
    /// Initial particle speed (m/s).
    pub speed: f32,
    /// Emission cone half-angle (radians); larger = more scattered.
    pub spread: f32,
    /// Particle size (metres).
    pub size: f32,
}

/// Normalise an element name for matching (lowercase, trimmed). Keeps the match arms
/// tolerant of content authored as "Fire", "fire", "FIRE", etc.
fn element_key(element: &str) -> String {
    element.trim().to_lowercase()
}

/// Build the emitter descriptor for an element. The `seed` lets two casts of the same
/// element differ slightly in rate/size without changing their character.
pub fn spell_emitter(element: &str, seed: u32) -> EmitterDesc {
    let mut rng = Rng::new(seed as u64 ^ 0xE171_7E55_0000_0001);
    let jitter = rng.range(0.9, 1.1);

    // Per-element archetypes. Colours are linear RGBA.
    let (rate, lifetime, color, speed, spread, size) = match element_key(element).as_str() {
        "fire" | "flame" | "ember" => (240.0, 0.8, [1.0, 0.45, 0.1, 1.0], 6.0, 0.5, 0.12),
        "frost" | "ice" | "cold" => (120.0, 1.4, [0.6, 0.85, 1.0, 0.9], 3.0, 0.25, 0.16),
        "void" | "shadow" | "dark" => (160.0, 1.2, [0.4, 0.15, 0.55, 0.85], 4.0, 0.7, 0.14),
        "life" | "nature" | "verdant" => (100.0, 1.8, [0.4, 0.9, 0.45, 0.9], 2.0, 0.6, 0.18),
        "arcane" | "mana" => (180.0, 1.0, [0.55, 0.6, 1.0, 0.9], 5.0, 0.4, 0.13),
        _ => (140.0, 1.0, [1.0, 1.0, 1.0, 0.9], 4.0, 0.5, 0.14),
    };

    EmitterDesc {
        rate: rate * jitter,
        lifetime,
        color,
        speed,
        spread,
        size: size * jitter,
    }
}

/// One blended SDF part of an effect shape.
enum Part {
    Sphere { center: Vec3, radius: f32 },
    Capsule { a: Vec3, b: Vec3, radius: f32 },
    Torus { major: f32, minor: f32 },
}

impl Part {
    fn distance(&self, p: Vec3) -> f32 {
        match *self {
            Part::Sphere { center, radius } => sd_sphere(p - center, radius),
            Part::Capsule { a, b, radius } => sd_capsule(p, a, b, radius),
            Part::Torus { major, minor } => sd_torus(p, major, minor),
        }
    }
}

/// Grow the organic shape for an element. Built in roughly unit space.
fn build_effect(element: &str, seed: u32) -> Vec<Part> {
    let mut rng = Rng::new(seed as u64 ^ 0xF00D_BEEF_0000_0002);
    let mut parts = Vec::new();

    match element_key(element).as_str() {
        // Fire: a bloom of shrinking spheres rising upward, like a flame tongue.
        "fire" | "flame" | "ember" => {
            let steps = 4;
            for i in 0..steps {
                let t = i as f32 / steps as f32;
                parts.push(Part::Sphere {
                    center: Vec3::new(rng.range(-0.08, 0.08), -0.4 + t * 1.0, rng.range(-0.08, 0.08)),
                    radius: 0.35 * (1.0 - t * 0.6),
                });
            }
        }
        // Void: a coiled torus around a dark core — a portal-like ring.
        "void" | "shadow" | "dark" => {
            parts.push(Part::Torus { major: 0.6, minor: 0.18 });
            parts.push(Part::Sphere { center: Vec3::ZERO, radius: 0.3 });
        }
        // Life/nature: a central bud sprouting tendrils outward and up.
        "life" | "nature" | "verdant" => {
            parts.push(Part::Sphere { center: Vec3::new(0.0, -0.2, 0.0), radius: 0.3 });
            let tendrils = 3 + (rng.next_u64() % 3) as usize;
            for _ in 0..tendrils {
                let angle = rng.range(0.0, std::f32::consts::TAU);
                let dir = Vec3::new(angle.cos() * 0.6, rng.range(0.4, 1.0), angle.sin() * 0.6);
                parts.push(Part::Capsule {
                    a: Vec3::new(0.0, -0.2, 0.0),
                    b: dir,
                    radius: rng.range(0.06, 0.1),
                });
            }
        }
        // Frost / arcane / default: a faceted-but-rounded cluster of spheres.
        _ => {
            let blobs = 3 + (rng.next_u64() % 3) as usize;
            for _ in 0..blobs {
                parts.push(Part::Sphere {
                    center: Vec3::new(
                        rng.range(-0.3, 0.3),
                        rng.range(-0.3, 0.3),
                        rng.range(-0.3, 0.3),
                    ),
                    radius: rng.range(0.2, 0.35),
                });
            }
        }
    }

    parts
}

/// Generate an organic mesh for a spell effect, parameterised by element. The parts are
/// smooth-blended and noise-displaced so the effect reads as a living, flowing shape
/// rather than hard geometry. `res` is the Surface Nets resolution.
pub fn generate_spell_mesh(element: &str, seed: u32, res: usize) -> Mesh {
    let parts = build_effect(element, seed);

    let blend = 0.2;
    let shape = move |p: Vec3| -> f32 {
        let mut d = f32::INFINITY;
        for part in &parts {
            d = op_union_smooth(d, part.distance(p), blend);
        }
        d
    };

    // Stronger displacement than creatures: VFX shapes want a turbulent, wispy surface.
    let field = displaced(shape, 0.06, 7.0, seed ^ 0x5E11_F00D);

    let bounds = Aabb::new(Vec3::splat(-1.3), Vec3::splat(1.3));
    surface_nets(&field, bounds, res, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spell_mesh_is_non_empty() {
        for element in ["fire", "void", "life", "frost"] {
            let mesh = generate_spell_mesh(element, 1, 20);
            assert!(!mesh.is_empty(), "{element} effect should produce geometry");
        }
    }

    #[test]
    fn emitter_differs_by_element() {
        let fire = spell_emitter("fire", 0);
        let frost = spell_emitter("frost", 0);
        // Fire is faster and hotter-coloured than frost — the descriptors must differ.
        assert!(fire.speed > frost.speed);
        assert_ne!(fire.color, frost.color);
    }
}
