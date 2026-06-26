//! Live content hot-reload: recompile shaders and regenerate procedural assets
//! while ten thousand people are playing, with no client restart.
//!
//! The flow (the wire-shapes live in [`arena_content::hotreload`]):
//!
//! 1. the session coordinator publishes a [`arena_content::hotreload::ContentVersion`]
//!    (monotonic epoch + pack hash) on the control plane;
//! 2. [`crate::net`] receives it, the client fetches the pack blob, and stages it
//!    into its [`ContentRegistry`] ([`ContentRegistry::stage`]);
//! 3. at a safe frame boundary the client calls [`HotReload::apply`], which:
//!    - swaps the staged pack in atomically ([`ContentRegistry::apply_pending`]),
//!    - **recompiles the wgpu pipeline for every changed `ShaderDef`** and swaps it
//!      into the renderer (a designer's WGSL edit takes effect live), and
//!    - **regenerates the procedural texture/mesh for every changed `MaterialDef`**
//!      and re-uploads it.
//!
//! Because live game *state* references content by stable id, swapping a definition
//! changes behaviour and look without disturbing identity or progress. That is what
//! makes "tweak the shaders/materials while people play" safe.

use arena_content::ContentRegistry;
use arena_content::pack::ContentPack;

use crate::mesh_gpu;
use crate::render::Renderer;

/// Drives content swaps against the renderer. Holds no state of its own today (the
/// registry is the source of truth); it exists as the seam where graphics-side
/// regeneration hangs off a content swap, and to track the last applied epoch.
#[derive(Default)]
pub struct HotReload {
    /// The content epoch the GPU resources currently reflect.
    pub applied_epoch: u64,
}

impl HotReload {
    pub fn new() -> Self {
        Self { applied_epoch: 0 }
    }

    /// Stage a freshly-fetched pack for swap-at-boundary. Thin pass-through that
    /// keeps the staging policy in one place; validation/epoch checks live in
    /// [`ContentRegistry::stage`].
    pub fn stage(
        &self,
        registry: &mut ContentRegistry,
        epoch: u64,
        pack: ContentPack,
    ) -> Result<(), arena_content::ContentError> {
        registry.stage(epoch, pack)
    }

    /// Apply any staged content: swap the registry, then rebuild affected GPU
    /// resources. Call at a frame boundary (never mid-pass). A no-op if nothing is
    /// staged. Returns the new epoch if a swap occurred.
    ///
    /// NOTE: we regenerate *all* shaders and materials in the new pack rather than
    /// computing a precise diff. A pack swap is rare (a designer save), the work is
    /// a handful of pipeline compiles + texture bakes, and "rebuild everything" is
    /// the robust choice — a [`arena_content::hotreload::PackDiff`] could narrow it
    /// later if compile cost ever matters.
    pub fn apply(
        &mut self,
        registry: &mut ContentRegistry,
        renderer: &mut Renderer,
    ) -> Option<u64> {
        let epoch = registry.apply_pending()?;
        tracing::info!(
            "content swap -> epoch {epoch} ({})",
            registry.pack().label
        );

        // --- recompile shaders ---
        // Each surface shader becomes a wgpu pipeline; a compile failure logs and
        // keeps the previous/default pipeline so a bad edit can't black-screen play.
        for shader in &registry.pack().shaders {
            match renderer.build_pipeline(shader) {
                Some(pipeline) => {
                    tracing::info!("recompiled shader '{}' ({})", shader.name, shader.id);
                    renderer.install_surface_pipeline(shader.id.0.clone(), pipeline);
                }
                None => {
                    tracing::warn!(
                        "shader '{}' ({}) not installed (non-surface or compile failure)",
                        shader.name,
                        shader.id
                    );
                }
            }
        }

        // --- regenerate procedural materials ---
        // Re-bake each material's texture from procgen and re-upload it. The bake
        // itself is the procgen seam (see `synth_material_texture`).
        for material in &registry.pack().materials {
            if let Some(tex_data) = synth_material_texture(material) {
                let gpu_tex =
                    mesh_gpu::upload_texture(&renderer.gpu.device, &renderer.gpu.queue, &tex_data);
                renderer.install_material(material, gpu_tex);
                tracing::debug!("rebaked material '{}' ({})", material.name, material.id);
            }
        }

        // TODO: regenerate procedural *meshes* whose worldgen/material inputs changed
        //       (zone terrain via `arena_procgen::world::generate_zone_mesh`, creature
        //       meshes via `arena_procgen::creature`) and call
        //       `renderer.set_world_meshes` / `set_entity_mesh`. Driven from app.rs,
        //       which owns the seeds and zone ids.

        self.applied_epoch = epoch;
        Some(epoch)
    }
}

/// Bake a [`MaterialDef`] into RGBA texture data via `arena-procgen`.
///
/// This is the procedural-material seam. `arena_procgen::material::synth` evaluates
/// the material's noise layers + colour ramp into a tileable texture; wiring the
/// exact entry point lands with procgen. Returning `None` for now means a content
/// swap recompiles shaders (fully wired) and simply leaves material textures as-is
/// until procgen's synth API is available.
fn synth_material_texture(
    _def: &arena_content::material::MaterialDef,
) -> Option<arena_procgen::texture::TextureData> {
    // TODO(arena-procgen): replace with e.g.
    //   Some(arena_procgen::material::synth::bake(_def, BAKE_RES))
    None
}
