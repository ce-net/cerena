//! Mesh data and surface extraction.
//!
//! We turn a scalar field (an SDF or a noise field) into a triangle mesh by sampling
//! it on a regular grid and meshing the `iso` level set.
//!
//! ## Why Surface Nets, not Marching Cubes
//!
//! Marching Cubes needs the canonical 256-entry edge/triangle lookup tables and tends
//! to produce sharp, triangle-soup surfaces. We instead use **naive Surface Nets**, a
//! dual-contouring method:
//!
//! * It places **one vertex per grid cell** that straddles the surface, positioned at
//!   the average of the field's zero-crossings on that cell's edges. Because the
//!   vertex floats to where the surface actually is (rather than snapping to edge
//!   midpoints), the result is smooth — exactly the organic look Cerena wants.
//! * Its tables are trivial (just the 12 cube edges and 8 corners), so the whole
//!   algorithm is short and auditable.
//! * Adjacent cells share vertices by construction, so the mesh is already welded and
//!   watertight-ish.
//!
//! Normals are taken from the **field gradient** (central differences), not from face
//! geometry, which keeps shading curvature-continuous across the whole surface.
//!
//! [`marching_cubes`] is provided as a thin alias for callers that expect that name;
//! it dispatches to [`surface_nets`].

use arena_protocol::world::Aabb;
use glam::Vec3;
use serde::{Deserialize, Serialize};

/// A renderable mesh: parallel vertex attribute arrays plus a triangle index buffer.
/// This is pure data — `arena-client` uploads it to GPU buffers; nothing here touches
/// a graphics API.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mesh {
    /// Vertex positions in the mesh's local/world space.
    pub positions: Vec<[f32; 3]>,
    /// Per-vertex normals (unit length), from the field gradient.
    pub normals: Vec<[f32; 3]>,
    /// Per-vertex UVs (planar XZ projection by default; client may use triplanar).
    pub uvs: Vec<[f32; 2]>,
    /// Per-vertex linear RGBA colour. Defaults to white; world-gen tints by biome.
    pub colors: Vec<[f32; 4]>,
    /// Triangle indices (three per triangle).
    pub indices: Vec<u32>,
}

impl Mesh {
    /// Number of triangles.
    pub fn tri_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// True if no geometry was produced (the field never crossed the iso level inside
    /// the bounds at this resolution).
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Recompute vertex normals from face geometry (area-weighted). Used after a
    /// smoothing pass, where the original gradient normals no longer match the moved
    /// vertices.
    pub fn recompute_normals(&mut self) {
        let mut acc = vec![Vec3::ZERO; self.positions.len()];
        for tri in self.indices.chunks_exact(3) {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let pa = Vec3::from_array(self.positions[a]);
            let pb = Vec3::from_array(self.positions[b]);
            let pc = Vec3::from_array(self.positions[c]);
            // Un-normalised cross product is area-weighted, so larger faces dominate.
            let n = (pb - pa).cross(pc - pa);
            acc[a] += n;
            acc[b] += n;
            acc[c] += n;
        }
        for (i, n) in acc.into_iter().enumerate() {
            self.normals[i] = n.normalize_or_zero().to_array();
        }
    }

    /// Optional Laplacian smoothing pass: nudge every vertex toward the average of its
    /// neighbours, then rebuild normals. A couple of iterations softens the faceting
    /// of low-resolution extractions into a more organic surface. Surface Nets already
    /// welds shared vertices, so this is purely a softening step, not a weld.
    pub fn weld_and_smooth(&mut self) {
        const ITERATIONS: usize = 2;
        const LAMBDA: f32 = 0.5;

        // Build a neighbour adjacency list from the triangle edges.
        let mut neighbours: Vec<Vec<u32>> = vec![Vec::new(); self.positions.len()];
        let add = |a: u32, b: u32, nb: &mut Vec<Vec<u32>>| {
            if !nb[a as usize].contains(&b) {
                nb[a as usize].push(b);
            }
        };
        for tri in self.indices.chunks_exact(3) {
            let (a, b, c) = (tri[0], tri[1], tri[2]);
            add(a, b, &mut neighbours);
            add(a, c, &mut neighbours);
            add(b, a, &mut neighbours);
            add(b, c, &mut neighbours);
            add(c, a, &mut neighbours);
            add(c, b, &mut neighbours);
        }

        for _ in 0..ITERATIONS {
            let mut next = self.positions.clone();
            for (i, nbrs) in neighbours.iter().enumerate() {
                if nbrs.is_empty() {
                    continue;
                }
                let mut avg = Vec3::ZERO;
                for &j in nbrs {
                    avg += Vec3::from_array(self.positions[j as usize]);
                }
                avg /= nbrs.len() as f32;
                let cur = Vec3::from_array(self.positions[i]);
                next[i] = cur.lerp(avg, LAMBDA).to_array();
            }
            self.positions = next;
        }

        self.recompute_normals();
    }
}

// Cube corner bit-layout: corner index `b` has offset (b&1, (b>>1)&1, (b>>2)&1).
// So corners 0..7 map to the eight (0/1, 0/1, 0/1) lattice points of a unit cell.
const CORNER: [[usize; 3]; 8] = [
    [0, 0, 0], // 0
    [1, 0, 0], // 1
    [0, 1, 0], // 2
    [1, 1, 0], // 3
    [0, 0, 1], // 4
    [1, 0, 1], // 5
    [0, 1, 1], // 6
    [1, 1, 1], // 7
];

// The 12 edges connect corners that differ in exactly one bit.
const EDGE: [[usize; 2]; 12] = [
    [0, 1], [2, 3], [4, 5], [6, 7], // edges along X
    [0, 2], [1, 3], [4, 6], [5, 7], // edges along Y
    [0, 4], [1, 5], [2, 6], [3, 7], // edges along Z
];

/// Extract the `iso` level set of `field` inside `bounds`, sampling on a `res^3` grid
/// of cells, using naive Surface Nets. Returns a smooth, shared-vertex mesh with
/// gradient normals. See the module docs for why this method is used.
pub fn surface_nets(field: &dyn Fn(Vec3) -> f32, bounds: Aabb, res: usize, iso: f32) -> Mesh {
    let mut mesh = Mesh::default();
    if res == 0 {
        return mesh;
    }

    let min = bounds.min;
    let size = bounds.max - bounds.min;
    let n = res; // cells per axis
    let s = res + 1; // sample points per axis

    // Pre-sample the field at every grid corner. idx(i,j,k) flattens the lattice.
    let idx = |i: usize, j: usize, k: usize| (k * s + j) * s + i;
    let corner_pos = |i: usize, j: usize, k: usize| -> Vec3 {
        min + Vec3::new(i as f32 / n as f32, j as f32 / n as f32, k as f32 / n as f32) * size
    };
    let mut samples = vec![0.0f32; s * s * s];
    for k in 0..s {
        for j in 0..s {
            for i in 0..s {
                samples[idx(i, j, k)] = field(corner_pos(i, j, k));
            }
        }
    }

    // Gradient step: half a cell, used for central-difference normals.
    let eps = (size.max_element() / n as f32) * 0.5;
    let gradient = |p: Vec3| -> Vec3 {
        let dx = field(p + Vec3::X * eps) - field(p - Vec3::X * eps);
        let dy = field(p + Vec3::Y * eps) - field(p - Vec3::Y * eps);
        let dz = field(p + Vec3::Z * eps) - field(p - Vec3::Z * eps);
        // The field increases outward, so +gradient is the outward normal.
        Vec3::new(dx, dy, dz).normalize_or_zero()
    };

    // One vertex per surface-straddling cell. -1 = no vertex in that cell.
    let cidx = |x: usize, y: usize, z: usize| (z * n + y) * n + x;
    let mut cell_vert = vec![-1i32; n * n * n];

    // Pass 1: place vertices.
    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                // Gather the 8 corner values for this cell.
                let mut corner_val = [0.0f32; 8];
                let mut mask = 0u8;
                for (b, off) in CORNER.iter().enumerate() {
                    let v = samples[idx(x + off[0], y + off[1], z + off[2])];
                    corner_val[b] = v;
                    if v < iso {
                        mask |= 1 << b;
                    }
                }
                // Entirely inside or entirely outside -> no surface here.
                if mask == 0 || mask == 0xFF {
                    continue;
                }

                // Average the zero-crossings on the straddled edges to place the vertex.
                let mut sum = Vec3::ZERO;
                let mut count = 0.0f32;
                for &[a, b] in EDGE.iter() {
                    let inside_a = (mask >> a) & 1;
                    let inside_b = (mask >> b) & 1;
                    if inside_a == inside_b {
                        continue;
                    }
                    let va = corner_val[a];
                    let vb = corner_val[b];
                    let denom = vb - va;
                    let t = if denom.abs() > f32::EPSILON {
                        (iso - va) / denom
                    } else {
                        0.5
                    };
                    let ca = Vec3::new(CORNER[a][0] as f32, CORNER[a][1] as f32, CORNER[a][2] as f32);
                    let cb = Vec3::new(CORNER[b][0] as f32, CORNER[b][1] as f32, CORNER[b][2] as f32);
                    sum += ca + (cb - ca) * t; // crossing, in local cell space [0,1]
                    count += 1.0;
                }
                let local = sum / count.max(1.0);

                // Local cell-space -> world.
                let lattice = Vec3::new(x as f32, y as f32, z as f32) + local;
                let world = min + (lattice / n as f32) * size;

                let vid = mesh.positions.len() as i32;
                cell_vert[cidx(x, y, z)] = vid;
                mesh.positions.push(world.to_array());
                mesh.normals.push(gradient(world).to_array());
                // Planar XZ UV; client may switch to triplanar for organic meshes.
                let uv = [
                    (world.x - min.x) / size.x.max(f32::EPSILON),
                    (world.z - min.z) / size.z.max(f32::EPSILON),
                ];
                mesh.uvs.push(uv);
                mesh.colors.push([1.0, 1.0, 1.0, 1.0]);
            }
        }
    }

    // Helper to fetch a cell's vertex index, if it has one.
    let vert_at = |cv: &[i32], x: usize, y: usize, z: usize| -> Option<u32> {
        let v = cv[cidx(x, y, z)];
        if v >= 0 { Some(v as u32) } else { None }
    };

    // Pass 2: stitch quads. For each minimal grid edge (the edge at a cell's base
    // corner along +X / +Y / +Z) that the surface crosses, the four cells sharing that
    // edge each own a vertex; join them into a quad. This is the dual of the crossing.
    let push_quad = |mesh: &mut Mesh, v: [u32; 4], flip: bool| {
        // Two triangles around the loop v0-v1-v2-v3. `flip` orients by which side is
        // inside so front faces point outward.
        if flip {
            mesh.indices.extend_from_slice(&[v[0], v[2], v[1]]);
            mesh.indices.extend_from_slice(&[v[0], v[3], v[2]]);
        } else {
            mesh.indices.extend_from_slice(&[v[0], v[1], v[2]]);
            mesh.indices.extend_from_slice(&[v[0], v[2], v[3]]);
        }
    };

    for z in 0..n {
        for y in 0..n {
            for x in 0..n {
                // Only consider cells that actually produced a vertex.
                if cell_vert[cidx(x, y, z)] < 0 {
                    continue;
                }
                let base_inside = samples[idx(x, y, z)] < iso;

                // --- edge along +X, shared by cells differing in y,z ---
                if y >= 1 && z >= 1 {
                    let other = samples[idx(x + 1, y, z)] < iso;
                    if base_inside != other {
                        if let (Some(a), Some(b), Some(c), Some(d)) = (
                            vert_at(&cell_vert, x, y - 1, z - 1),
                            vert_at(&cell_vert, x, y, z - 1),
                            vert_at(&cell_vert, x, y, z),
                            vert_at(&cell_vert, x, y - 1, z),
                        ) {
                            push_quad(&mut mesh, [a, b, c, d], base_inside);
                        }
                    }
                }

                // --- edge along +Y, shared by cells differing in x,z ---
                if x >= 1 && z >= 1 {
                    let other = samples[idx(x, y + 1, z)] < iso;
                    if base_inside != other {
                        if let (Some(a), Some(b), Some(c), Some(d)) = (
                            vert_at(&cell_vert, x - 1, y, z - 1),
                            vert_at(&cell_vert, x, y, z - 1),
                            vert_at(&cell_vert, x, y, z),
                            vert_at(&cell_vert, x - 1, y, z),
                        ) {
                            push_quad(&mut mesh, [a, b, c, d], !base_inside);
                        }
                    }
                }

                // --- edge along +Z, shared by cells differing in x,y ---
                if x >= 1 && y >= 1 {
                    let other = samples[idx(x, y, z + 1)] < iso;
                    if base_inside != other {
                        if let (Some(a), Some(b), Some(c), Some(d)) = (
                            vert_at(&cell_vert, x - 1, y - 1, z),
                            vert_at(&cell_vert, x, y - 1, z),
                            vert_at(&cell_vert, x, y, z),
                            vert_at(&cell_vert, x - 1, y, z),
                        ) {
                            push_quad(&mut mesh, [a, b, c, d], base_inside);
                        }
                    }
                }
            }
        }
    }

    mesh
}

/// Compatibility alias for callers that expect a `marching_cubes` entry point. Cerena
/// extracts surfaces with Surface Nets (see module docs); this simply forwards.
pub fn marching_cubes(field: &dyn Fn(Vec3) -> f32, bounds: Aabb, res: usize, iso: f32) -> Mesh {
    surface_nets(field, bounds, res, iso)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_field_extracts_a_unit_surface() {
        // A unit sphere SDF: inside negative, surface at radius 1.
        let field = |p: Vec3| p.length() - 1.0;
        let bounds = Aabb::new(Vec3::splat(-1.5), Vec3::splat(1.5));
        let mesh = surface_nets(&field, bounds, 24, 0.0);

        assert!(!mesh.is_empty(), "sphere should produce geometry");
        assert!(mesh.tri_count() > 0, "sphere should produce triangles");

        // Every extracted vertex should sit roughly on the unit sphere.
        for v in &mesh.positions {
            let r = Vec3::from_array(*v).length();
            assert!(r > 0.6 && r < 1.4, "vertex radius {r} should be near 1.0");
        }
    }

    #[test]
    fn empty_field_extracts_nothing() {
        // A field that is positive everywhere never crosses iso=0.
        let field = |_p: Vec3| 5.0;
        let bounds = Aabb::new(Vec3::splat(-1.0), Vec3::splat(1.0));
        let mesh = surface_nets(&field, bounds, 8, 0.0);
        assert!(mesh.is_empty());
    }
}
