//! An [`AssetBundle`]: every renderable asset for one content pack, baked once and
//! indexed by handle. This is the artifact that *makes rendering trivial* — the client
//! bakes a bundle when a pack arrives, uploads each asset to the GPU once, and from
//! then on draws any entity by looking up its [`MobVisual`] / [`ItemVisual`] and
//! binding the cached resources behind those handles. No per-frame content resolution,
//! no per-frame generation.

use std::collections::HashMap;

use arena_content::pack::ContentPack;

use crate::bake::{self, BakeQuality};
use crate::handle::{MaterialHandle, MeshHandle, RigHandle, ShaderHandle, TextureHandle};
use crate::render_asset::{MaterialAsset, RigAsset, ShaderAsset, SkinnedMesh, TextureAsset};

/// Everything needed to draw one creature: its skinned mesh, its surface material (if
/// any), and the rig to animate it.
#[derive(Debug, Clone, Copy)]
pub struct MobVisual {
    pub mesh: MeshHandle,
    pub material: Option<MaterialHandle>,
    pub rig: RigHandle,
}

/// Everything needed to draw one item in the world or an inventory slot.
#[derive(Debug, Clone, Copy)]
pub struct ItemVisual {
    pub mesh: MeshHandle,
    pub material: Option<MaterialHandle>,
}

/// A fully-baked asset set for one [`ContentPack`]. Arrays are indexed by the handles
/// in [`crate::handle`]; the maps resolve content ids to those handles.
#[derive(Debug, Clone)]
pub struct AssetBundle {
    /// The pack label this was baked from (for debugging / the asset HUD).
    pub label: String,
    /// The pack's content hash — the bundle's identity, and the hot-reload key.
    pub pack_hash: String,

    pub meshes: Vec<SkinnedMesh>,
    pub textures: Vec<TextureAsset>,
    pub materials: Vec<MaterialAsset>,
    pub shaders: Vec<ShaderAsset>,
    pub rigs: Vec<RigAsset>,

    material_by_id: HashMap<String, MaterialHandle>,
    shader_by_id: HashMap<String, ShaderHandle>,
    mob_visuals: HashMap<String, MobVisual>,
    item_visuals: HashMap<String, ItemVisual>,
}

impl AssetBundle {
    /// Bake every asset a pack needs. Order matters: shaders → materials (which
    /// reference shader handles) → mobs/items (which reference material handles), so
    /// every handle a later asset stores already exists.
    pub fn bake(pack: &ContentPack, quality: BakeQuality) -> AssetBundle {
        let mut b = AssetBundle {
            label: pack.label.clone(),
            pack_hash: pack.hash(),
            meshes: Vec::new(),
            textures: Vec::new(),
            materials: Vec::new(),
            shaders: Vec::new(),
            rigs: Vec::new(),
            material_by_id: HashMap::new(),
            shader_by_id: HashMap::new(),
            mob_visuals: HashMap::new(),
            item_visuals: HashMap::new(),
        };

        // 1) Shaders.
        for sh in &pack.shaders {
            let h = ShaderHandle(b.shaders.len() as u32);
            b.shaders.push(bake::bake_shader(sh));
            b.shader_by_id.insert(sh.id.as_str().to_string(), h);
        }

        // 2) Materials (each bakes an albedo + a normal texture, then binds a shader).
        for mat in &pack.materials {
            let seed = seed32(mat.id.as_str());
            let albedo = TextureAsset::from_texture(
                arena_procgen::texture::synth_material_texture(mat, quality.texture_size, seed),
                true,
            );
            let normal = TextureAsset::from_texture(
                arena_procgen::texture::synth_normal_map(mat, quality.texture_size, seed),
                false,
            );
            let albedo_h = TextureHandle(b.textures.len() as u32);
            b.textures.push(albedo);
            let normal_h = TextureHandle(b.textures.len() as u32);
            b.textures.push(normal);

            let shader = mat
                .shader
                .as_ref()
                .and_then(|s| b.shader_by_id.get(s.as_str()).copied());

            let h = MaterialHandle(b.materials.len() as u32);
            b.materials.push(MaterialAsset {
                albedo: albedo_h,
                normal: normal_h,
                roughness: mat.roughness,
                metallic: mat.metallic,
                emissive: mat.emissive,
                emissive_color: mat.emissive_color,
                triplanar_scale: mat.triplanar_scale,
                displacement: mat.displacement,
                shader,
            });
            b.material_by_id.insert(mat.id.as_str().to_string(), h);
        }

        // 3) Mobs: mesh + rig + material.
        for mob in &pack.mobs {
            let (mesh, rig) = bake::bake_creature(mob, quality);
            let mesh_h = MeshHandle(b.meshes.len() as u32);
            b.meshes.push(mesh);
            let rig_h = RigHandle(b.rigs.len() as u32);
            b.rigs.push(rig);
            let material = mob
                .material
                .as_ref()
                .and_then(|m| b.material_by_id.get(m.as_str()).copied());
            b.mob_visuals
                .insert(mob.id.as_str().to_string(), MobVisual { mesh: mesh_h, material, rig: rig_h });
        }

        // 4) Items: a static emblem mesh + material.
        for item in &pack.items {
            let mesh = bake::bake_item_mesh(item, quality);
            let mesh_h = MeshHandle(b.meshes.len() as u32);
            b.meshes.push(mesh);
            let material = item
                .material
                .as_ref()
                .and_then(|m| b.material_by_id.get(m.as_str()).copied());
            b.item_visuals
                .insert(item.id.as_str().to_string(), ItemVisual { mesh: mesh_h, material });
        }

        b
    }

    // --- lookups (the draw-time API) ---------------------------------------

    /// The draw set for a creature, by mob id (`"mob.wisp"`).
    pub fn mob_visual(&self, mob_id: &str) -> Option<MobVisual> {
        self.mob_visuals.get(mob_id).copied()
    }

    /// The draw set for an item, by item id (`"item.ember_staff"`).
    pub fn item_visual(&self, item_id: &str) -> Option<ItemVisual> {
        self.item_visuals.get(item_id).copied()
    }

    /// Resolve a material handle by content id (e.g. for terrain surface materials).
    pub fn material_handle(&self, material_id: &str) -> Option<MaterialHandle> {
        self.material_by_id.get(material_id).copied()
    }

    /// Resolve a shader handle by content id.
    pub fn shader_handle(&self, shader_id: &str) -> Option<ShaderHandle> {
        self.shader_by_id.get(shader_id).copied()
    }

    pub fn mesh(&self, h: MeshHandle) -> &SkinnedMesh {
        &self.meshes[h.index()]
    }
    pub fn texture(&self, h: TextureHandle) -> &TextureAsset {
        &self.textures[h.index()]
    }
    pub fn material(&self, h: MaterialHandle) -> &MaterialAsset {
        &self.materials[h.index()]
    }
    pub fn shader(&self, h: ShaderHandle) -> &ShaderAsset {
        &self.shaders[h.index()]
    }
    pub fn rig(&self, h: RigHandle) -> &RigAsset {
        &self.rigs[h.index()]
    }

    /// Total baked asset count (handy for an asset HUD / budget telemetry).
    pub fn asset_count(&self) -> usize {
        self.meshes.len() + self.textures.len() + self.materials.len() + self.shaders.len() + self.rigs.len()
    }
}

/// FNV-1a folded to 32 bits — a stable per-id texture seed.
fn seed32(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for byte in s.bytes() {
        h ^= byte as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::default_pack;

    #[test]
    fn bundle_bakes_the_default_pack() {
        let pack = default_pack();
        let bundle = AssetBundle::bake(&pack, BakeQuality::low());

        // Every mob resolves to a full visual whose handles are in range.
        for mob in &pack.mobs {
            let v = bundle.mob_visual(mob.id.as_str()).expect("mob has a visual");
            assert!(v.mesh.index() < bundle.meshes.len());
            assert!(v.rig.index() < bundle.rigs.len());
            if let Some(m) = v.material {
                assert!(m.index() < bundle.materials.len());
            }
        }
        // Every item resolves too.
        for item in &pack.items {
            assert!(bundle.item_visual(item.id.as_str()).is_some());
        }
        // Materials produced two textures each (albedo + normal).
        assert_eq!(bundle.textures.len(), pack.materials.len() * 2);
        assert_eq!(bundle.pack_hash, pack.hash());
    }
}
