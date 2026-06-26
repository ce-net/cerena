//! Bakers: pure functions turning one content definition into its GPU-ready
//! [`crate::render_asset`] payload. These do the heavy lifting (run the procgen
//! generators, skin the mesh, build the rig); [`crate::bundle`] orchestrates them and
//! wires up cross-asset handles.

use glam::Vec3;

use arena_content::item::ItemDef;
use arena_content::material::ShaderDef;
use arena_content::mob::MobDef;
use arena_procgen::mesh::{surface_nets, Mesh};
use arena_procgen::sdf::{op_union_smooth, sd_capsule, sd_sphere};
use arena_protocol::world::Aabb;

use crate::render_asset::{RigAsset, ShaderAsset, SkinnedMesh};
use crate::rig;
use crate::skin;

/// Mesh/texture detail for a bake. The same content bakes at any quality; a server or
/// a low-end phone just picks coarser numbers.
#[derive(Debug, Clone, Copy)]
pub struct BakeQuality {
    /// Surface-Nets grid resolution for creature/item meshes (higher = finer).
    pub mesh_res: usize,
    /// Square texture size baked per material (albedo + normal).
    pub texture_size: u32,
}

impl Default for BakeQuality {
    fn default() -> Self {
        Self { mesh_res: 28, texture_size: 256 }
    }
}

impl BakeQuality {
    /// A cheap preset for headless servers / tests (small meshes, tiny textures).
    pub fn low() -> Self {
        Self { mesh_res: 16, texture_size: 64 }
    }
    /// A high-detail preset for capable clients.
    pub fn high() -> Self {
        Self { mesh_res: 40, texture_size: 512 }
    }
}

/// Bake a creature: grow its mesh, build its rig, skin the mesh to the rig. Returns the
/// skinned mesh and the rig asset (skeleton + inverse binds + animator config).
pub fn bake_creature(mob: &MobDef, quality: BakeQuality) -> (SkinnedMesh, RigAsset) {
    let mesh = arena_procgen::creature::generate_creature_mesh(mob, quality.mesh_res);
    let skeleton = rig::build_skeleton(mob);
    let (joints, weights) = skin::skin_mesh(&mesh.positions, &skeleton);

    let mut sm = SkinnedMesh::from_mesh(&mesh);
    sm.joints = joints;
    sm.weights = weights;

    let inverse_bind = skeleton.inverse_bind_matrices();
    let config = rig::build_config(mob);
    let rig_asset = RigAsset { skeleton, inverse_bind, config };
    (sm, rig_asset)
}

/// Bake a static world/inventory mesh for an item. Items carry no `mesh_seed`, so we
/// derive a stable seed from the item id and grow a small emblem: an elongated form
/// for staves, a rounder one for orbs/relics, jittered per item so no two look alike.
pub fn bake_item_mesh(item: &ItemDef, quality: BakeQuality) -> SkinnedMesh {
    let seed = fnv1a(item.id.as_str());
    let mesh = item_emblem_mesh(seed, quality.mesh_res.min(24));
    SkinnedMesh::from_mesh(&mesh)
}

/// Bake a shader definition into a compile-ready asset (a straight, validated
/// pass-through of its WGSL + stage + params).
pub fn bake_shader(sh: &ShaderDef) -> ShaderAsset {
    ShaderAsset { stage: sh.stage, source: sh.source.clone(), params: sh.params.clone() }
}

// --- helpers ---------------------------------------------------------------

/// Grow a small organic "emblem" mesh from a seed: a core sphere fused with a couple
/// of seeded lobes/spurs via smooth-min, so the item silhouette varies but always
/// reads as one organic piece (matching Cerena's art direction).
fn item_emblem_mesh(seed: u64, res: usize) -> Mesh {
    // Derive a few stable parameters from the seed without any RNG dependency.
    let f = |shift: u32, lo: f32, hi: f32| {
        let bits = (seed >> shift) & 0xFFFF;
        lo + (bits as f32 / 65535.0) * (hi - lo)
    };
    let core_r = f(0, 0.35, 0.6);
    let elong = f(16, 0.6, 1.6); // >1 = staff-like, <1 = squat orb
    let spur_len = f(32, 0.2, 0.9);
    let spur_ang = f(48, 0.0, std::f32::consts::TAU);
    let blend = 0.2;

    let spur_dir = Vec3::new(spur_ang.cos(), 0.4, spur_ang.sin()).normalize_or_zero();
    let field = move |p: Vec3| -> f32 {
        // A vertically-stretched core capsule + one angled spur.
        let core = sd_capsule(p, Vec3::new(0.0, -elong, 0.0), Vec3::new(0.0, elong, 0.0), core_r);
        let spur = sd_capsule(
            p,
            Vec3::ZERO,
            spur_dir * (spur_len + 0.6),
            core_r * 0.45,
        );
        let cap = sd_sphere(p - Vec3::new(0.0, elong, 0.0), core_r * 0.7);
        op_union_smooth(op_union_smooth(core, spur, blend), cap, blend)
    };
    let bound = elong.max(core_r) + spur_len + 0.6;
    let bounds = Aabb::new(Vec3::splat(-bound - 0.2), Vec3::splat(bound + 0.2));
    surface_nets(&field, bounds, res, 0.0)
}

/// FNV-1a over a string → a stable 64-bit seed. Deterministic and dependency-free, so
/// the same item id always grows the same emblem on every machine.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::ids::MobId;

    fn mob(seed: u32) -> MobDef {
        MobDef {
            id: MobId::new("mob.t"),
            name: "t".into(),
            max_health: 100.0,
            move_speed: 4.0,
            abilities: vec![],
            xp_reward: 0,
            loot_table: vec![],
            material: None,
            scale: 1.0,
            aggressive: false,
            mesh_seed: seed,
        }
    }

    #[test]
    fn creature_bake_skins_every_vertex() {
        let (sm, rig) = bake_creature(&mob(0x7002), BakeQuality::low());
        assert!(!sm.positions.is_empty());
        // A creature rig is non-trivial, so the mesh should be skinned 1:1.
        assert!(sm.is_skinned(), "creature mesh should carry bone weights");
        assert_eq!(sm.joints.len(), sm.positions.len());
        assert_eq!(rig.inverse_bind.len(), rig.skeleton.len());
    }

    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv1a("item.ember_staff"), fnv1a("item.ember_staff"));
        assert_ne!(fnv1a("item.ember_staff"), fnv1a("item.frost_staff"));
    }
}
