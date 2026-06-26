//! GPU-ready CPU payloads — the "upload me" structs.
//!
//! Each type here is exactly what a renderer maps 1:1 onto GPU resources: a
//! [`SkinnedMesh`] becomes vertex + index buffers, a [`TextureAsset`] becomes a 2D
//! texture, a [`MaterialAsset`] becomes a bind group, a [`ShaderAsset`] becomes a
//! pipeline. **No graphics types leak in here** — the crate stays `wgpu`-free and
//! `wasm`-clean, so the same bake runs on a headless server, in a test, or in the
//! browser. The client's `mesh_gpu` is then a thin, mechanical uploader.

use glam::Mat4;

use arena_anim::skeleton::Skeleton;
use arena_anim::state::{Animator, AnimatorConfig};
use arena_procgen::mesh::Mesh;
use arena_procgen::texture::TextureData;

use crate::handle::{ShaderHandle, TextureHandle};

/// A renderable mesh with optional skinning. For static meshes (items, terrain chunks,
/// projectile fx) `joints`/`weights` are empty and the renderer binds the rigid path;
/// for creatures they carry up to four bone influences per vertex.
#[derive(Debug, Clone, Default)]
pub struct SkinnedMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub colors: Vec<[f32; 4]>,
    pub indices: Vec<u32>,
    /// Up to four bone indices per vertex (into the rig's skeleton). Empty = static.
    pub joints: Vec<[u16; 4]>,
    /// Bone weights matching `joints`, summing to ~1 per vertex. Empty = static.
    pub weights: Vec<[f32; 4]>,
}

impl SkinnedMesh {
    /// Wrap a procgen [`Mesh`] as a static (un-skinned) renderable mesh.
    pub fn from_mesh(mesh: &Mesh) -> Self {
        Self {
            positions: mesh.positions.clone(),
            normals: mesh.normals.clone(),
            uvs: mesh.uvs.clone(),
            colors: mesh.colors.clone(),
            indices: mesh.indices.clone(),
            joints: Vec::new(),
            weights: Vec::new(),
        }
    }

    /// True if this mesh carries bone influences (drawn with the skinned pipeline).
    pub fn is_skinned(&self) -> bool {
        !self.joints.is_empty() && self.joints.len() == self.positions.len()
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn tri_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// A 2D texture: tightly-packed RGBA8 pixels plus dimensions and a colour-space hint.
#[derive(Debug, Clone, Default)]
pub struct TextureAsset {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
    /// True for albedo/colour maps (sample/upload as sRGB); false for data maps
    /// (normal maps, masks) which must stay linear.
    pub srgb: bool,
}

impl TextureAsset {
    /// Adopt a procgen [`TextureData`] as a colour (`srgb`) or data (`linear`) texture.
    pub fn from_texture(t: TextureData, srgb: bool) -> Self {
        Self { width: t.width, height: t.height, rgba: t.rgba, srgb }
    }
}

/// PBR surface parameters + the texture/shader handles a material binds. Mirrors the
/// designer-facing [`arena_content::material::MaterialDef`] but resolved to concrete
/// baked-asset handles, so the renderer needs zero content lookups at draw time.
#[derive(Debug, Clone, Copy)]
pub struct MaterialAsset {
    pub albedo: TextureHandle,
    pub normal: TextureHandle,
    pub roughness: f32,
    pub metallic: f32,
    pub emissive: f32,
    pub emissive_color: [f32; 3],
    pub triplanar_scale: f32,
    pub displacement: f32,
    /// Custom surface shader, if the material names one; else the renderer's default.
    pub shader: Option<ShaderHandle>,
}

/// A shader program ready to compile: the raw WGSL plus its render stage and the
/// designer-exposed scalar uniforms (name + default).
#[derive(Debug, Clone)]
pub struct ShaderAsset {
    pub stage: arena_content::material::ShaderStage,
    pub source: String,
    pub params: Vec<(String, f32)>,
}

/// A baked rig: the skeleton, its cached inverse-bind matrices, and the animator
/// configuration tuned for this creature. The renderer (or sim) spins up a live
/// [`Animator`] from this per instance with [`RigAsset::animator`].
#[derive(Debug, Clone)]
pub struct RigAsset {
    pub skeleton: Skeleton,
    pub inverse_bind: Vec<Mat4>,
    pub config: AnimatorConfig,
}

impl RigAsset {
    /// Instantiate a fresh, independent [`Animator`] for one creature instance. Cheap
    /// relative to a bake (clones plain data); call it per spawned entity.
    pub fn animator(&self) -> Animator {
        Animator::new(self.skeleton.clone(), self.inverse_bind.clone(), self.config)
    }
}
