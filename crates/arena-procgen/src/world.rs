//! World generation — terrain, collision, biomes, and structure placement.
//!
//! The world is a regular grid of zones (see [`arena_protocol::world::ZoneId`]). Each
//! zone is generated independently and deterministically from a single
//! [`WorldGenParams`] recipe, so the authoritative server and every client compute the
//! *same* terrain without exchanging geometry:
//!
//! * The **server** calls [`generate_zone_collision`] to get a coarse `Vec<Aabb>` it
//!   can use for physics — cheap box columns, no full mesh needed.
//! * **Clients** call [`generate_zone_mesh`] (Surface Nets over the zone's SDF) plus
//!   the material system for the visible surface.
//!
//! Both are seeded identically, so they agree on where the ground is.
//!
//! ## Organic terrain, not voxels
//!
//! Terrain height comes from layered fbm (continents) plus ridged noise (mountains).
//! Caves are carved by *subtracting* a 3D noise field from the solid with a smooth
//! operator ([`crate::sdf::op_subtract_smooth`]), so cave mouths are rounded rather
//! than blocky. The whole zone is one continuous SDF — there are no hard voxel faces.

use arena_content::worldgen::{BiomeDef, StructureDef, WorldGenParams};
use arena_protocol::world::{Aabb, ZoneId, WORLD_CEIL_M, WORLD_FLOOR_M, ZONE_SIZE_M};
use glam::Vec3;

use crate::mesh::{surface_nets, Mesh};
use crate::noise_eval::{fbm, sample_layer, Rng};
use crate::sdf::op_subtract_smooth;

/// Normalised (0..1) terrain height at a world (x, z), before mapping to metres. This
/// is the value biomes band against. Continents set the base; mountains add ridged
/// relief on top.
fn terrain_height01(params: &WorldGenParams, x: f32, z: f32) -> f32 {
    let scale = params.world_scale.max(1.0);
    // Horizontal sampling only — terrain height is a 2D field.
    let np = Vec3::new(x / scale, 0.0, z / scale);

    let continent = sample_layer(&params.continent, np); // ~[-1, 1]
    let mountains = sample_layer(&params.mountains, np); // ridged, ~[0, 1]

    let base = (continent * 0.5 + 0.5).clamp(0.0, 1.0);
    // Mountains pile relief on top of the continent base.
    (base * 0.7 + mountains.max(0.0) * 0.3).clamp(0.0, 1.0)
}

/// Terrain surface height in metres at a world (x, z).
fn terrain_height(params: &WorldGenParams, x: f32, z: f32) -> f32 {
    WORLD_FLOOR_M + terrain_height01(params, x, z) * (WORLD_CEIL_M - WORLD_FLOOR_M)
}

/// Build the terrain SDF for a zone as a closure borrowing `params`. The field is
/// negative below the surface (solid), positive above (air), with caves subtracted.
/// `iso = 0` is the ground surface, and the field increases upward so normals point
/// out of the ground.
///
/// The `_zone` argument is accepted for symmetry / future per-zone variation; the
/// field itself is world-space and seamless across zone borders.
pub fn zone_field<'a>(
    params: &'a WorldGenParams,
    _zone: ZoneId,
) -> impl Fn(Vec3) -> f32 + 'a {
    move |p: Vec3| {
        let ty = terrain_height(params, p.x, p.z);
        // Solid below terrain: negative inside, positive above. This is our base SDF.
        let solid = p.y - ty;

        // Caves: a 3D noise field hollows out rock where it exceeds the threshold.
        let cscale = params.world_scale.max(1.0);
        let cave = sample_layer(&params.caves, p / cscale); // ~[-1, 1]
        let cave01 = (cave * 0.5 + 0.5).clamp(0.0, 1.0);
        // A signed field that is negative *inside* a cave void (cave01 > threshold).
        let half_span = (WORLD_CEIL_M - WORLD_FLOOR_M) * 0.5;
        let cave_void = (params.cave_threshold - cave01) * half_span;

        // Subtract the void from the solid with a smooth lip so cave mouths are round.
        op_subtract_smooth(solid, cave_void, 6.0)
    }
}

/// The world-space AABB of a zone, from world floor to ceiling.
fn zone_bounds(zone: ZoneId) -> Aabb {
    let x0 = zone.x as f32 * ZONE_SIZE_M;
    let z0 = zone.z as f32 * ZONE_SIZE_M;
    Aabb::new(
        Vec3::new(x0, WORLD_FLOOR_M, z0),
        Vec3::new(x0 + ZONE_SIZE_M, WORLD_CEIL_M, z0 + ZONE_SIZE_M),
    )
}

/// Generate the renderable mesh for a zone by extracting its terrain SDF at `res^3`
/// resolution. Vertices are tinted by their biome so the raw mesh already carries a
/// mood even before the client applies the full material. Clients use this; the server
/// does not need it.
pub fn generate_zone_mesh(params: &WorldGenParams, zone: ZoneId, res: usize) -> Mesh {
    let field = zone_field(params, zone);
    let mut mesh = surface_nets(&field, zone_bounds(zone), res, 0.0);

    // Tint each vertex by its biome's fog colour as a cheap base colour. The client
    // overlays the biome's actual surface material on top via triplanar mapping.
    for (i, p) in mesh.positions.iter().enumerate() {
        let pos = Vec3::from_array(*p);
        let biome = biome_at(params, pos);
        let c = biome.fog_color;
        mesh.colors[i] = [c[0], c[1], c[2], 1.0];
    }

    mesh
}

/// Produce a coarse AABB approximation of a zone's terrain surface for the server's
/// physics. We sample a heightfield grid and emit one box column per cell, from the
/// world floor up to the terrain height. This is far cheaper than the full mesh and
/// good enough for broad-phase collision; caves are not represented (acceptable for a
/// conservative ground collider).
pub fn generate_zone_collision(params: &WorldGenParams, zone: ZoneId) -> Vec<Aabb> {
    const GRID: usize = 16; // columns per axis
    let mut out = Vec::with_capacity(GRID * GRID);

    let x0 = zone.x as f32 * ZONE_SIZE_M;
    let z0 = zone.z as f32 * ZONE_SIZE_M;
    let step = ZONE_SIZE_M / GRID as f32;

    for iz in 0..GRID {
        for ix in 0..GRID {
            // Sample the height at the column centre.
            let cx = x0 + (ix as f32 + 0.5) * step;
            let cz = z0 + (iz as f32 + 0.5) * step;
            let h = terrain_height(params, cx, cz);

            let min = Vec3::new(x0 + ix as f32 * step, WORLD_FLOOR_M, z0 + iz as f32 * step);
            let max = Vec3::new(
                x0 + (ix as f32 + 1.0) * step,
                h.max(WORLD_FLOOR_M + 0.01),
                z0 + (iz as f32 + 1.0) * step,
            );
            out.push(Aabb::new(min, max));
        }
    }

    out
}

/// Pick the biome at a world position. We band by terrain height, and where several
/// biomes overlap a height band we choose deterministically with a low-frequency
/// selection noise — so biome boundaries wander organically rather than following
/// straight contour lines. Falls back to the nearest band if none contains the height.
///
/// Panics only if `params.biomes` is empty, which a valid world never is.
pub fn biome_at(params: &WorldGenParams, pos: Vec3) -> &BiomeDef {
    assert!(!params.biomes.is_empty(), "world must define at least one biome");
    let h01 = terrain_height01(params, pos.x, pos.z);

    // Biomes whose height band covers this point.
    let candidates: Vec<usize> = (0..params.biomes.len())
        .filter(|&i| {
            let b = &params.biomes[i];
            h01 >= b.height_min && h01 <= b.height_max
        })
        .collect();

    if candidates.is_empty() {
        // Nearest band centre.
        let mut best = 0usize;
        let mut best_d = f32::INFINITY;
        for (i, b) in params.biomes.iter().enumerate() {
            let centre = (b.height_min + b.height_max) * 0.5;
            let d = (centre - h01).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        return &params.biomes[best];
    }

    // Choose among overlapping candidates with selection noise.
    let sel = fbm(
        Vec3::new(pos.x * params.biome_freq / 100.0, 0.0, pos.z * params.biome_freq / 100.0),
        2,
        2.0,
        0.5,
        params.seed as u32 ^ 0xB10E_B10E,
    );
    let t = (sel * 0.5 + 0.5).clamp(0.0, 0.999);
    let pick = candidates[(t * candidates.len() as f32) as usize];
    &params.biomes[pick]
}

/// Mix a master seed with a zone and a sub-stream index into one deterministic u64.
fn mix_seed(seed: u64, zone: ZoneId, stream: usize) -> u64 {
    let mut h = seed;
    h ^= (zone.x as i64 as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h = h.rotate_left(17) ^ (zone.z as i64 as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h = h.rotate_left(23) ^ (stream as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    h
}

/// Deterministically scatter structures across a zone. Each [`StructureDef`] gets a
/// target count from its `frequency` and a Poisson-ish rejection pass (reject points
/// too close together) so instances spread out rather than clumping. Positions are
/// snapped to the terrain surface. Returns `(world_position, def)` pairs.
///
/// Deterministic: the per-structure RNG is keyed by (master seed, zone, structure
/// index), so every machine scatters them identically.
pub fn place_structures<'a>(
    params: &'a WorldGenParams,
    zone: ZoneId,
) -> Vec<(Vec3, &'a StructureDef)> {
    let mut out = Vec::new();
    let x0 = zone.x as f32 * ZONE_SIZE_M;
    let z0 = zone.z as f32 * ZONE_SIZE_M;
    // "frequency" reads as instances per ~100 m^2 cell of the zone.
    let area_units = (ZONE_SIZE_M / 10.0).powi(2);

    for (si, def) in params.structures.iter().enumerate() {
        let target = (def.frequency * area_units).round().max(0.0) as usize;
        if target == 0 {
            continue;
        }
        // Minimum spacing keeps the scatter even.
        let min_dist = ZONE_SIZE_M / ((target as f32).sqrt() + 1.0);
        let min_dist_sq = min_dist * min_dist;

        let mut rng = Rng::new(mix_seed(params.seed, zone, si));
        let mut placed: Vec<Vec3> = Vec::new();
        let attempts = target * 4 + 8;

        for _ in 0..attempts {
            if placed.len() >= target {
                break;
            }
            let px = x0 + rng.next_f32() * ZONE_SIZE_M;
            let pz = z0 + rng.next_f32() * ZONE_SIZE_M;

            // Reject if too close to an already-placed instance (horizontal distance).
            let too_close = placed.iter().any(|q| {
                let dx = q.x - px;
                let dz = q.z - pz;
                dx * dx + dz * dz < min_dist_sq
            });
            if too_close {
                continue;
            }

            let py = terrain_height(params, px, pz);
            let pos = Vec3::new(px, py, pz);
            placed.push(pos);
            out.push((pos, def));
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_returns_box_columns() {
        let params = WorldGenParams::default();
        let boxes = generate_zone_collision(&params, ZoneId::new(0, 0));
        assert!(!boxes.is_empty(), "collision should emit ground columns");
        // Each column should be a valid (min <= max) box.
        for b in &boxes {
            assert!(b.max.y >= b.min.y);
        }
    }

    #[test]
    fn zone_mesh_is_deterministic() {
        // Same params + zone must yield identical geometry on any machine, so the
        // server and clients agree on the world.
        let params = WorldGenParams::default();
        let zone = ZoneId::new(0, 0);
        let a = generate_zone_mesh(&params, zone, 16);
        let b = generate_zone_mesh(&params, zone, 16);
        assert_eq!(a.positions.len(), b.positions.len());
        if !a.positions.is_empty() {
            assert_eq!(a.positions[0], b.positions[0]);
        }
    }

    #[test]
    fn biome_lookup_returns_a_biome() {
        let params = WorldGenParams::default();
        let _ = biome_at(&params, Vec3::new(10.0, 0.0, 20.0));
    }
}
