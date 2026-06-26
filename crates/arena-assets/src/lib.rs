//! # arena-assets
//!
//! The bridge from **content** (`arena-content`) + **generators** (`arena-procgen`,
//! `arena-anim`) to **GPU-ready data** — the layer whose whole job is to make the
//! renderer trivial.
//!
//! The client never wants to think about *how* a wisp's mesh is grown, how its skin
//! weights are derived, how its material's noise becomes a texture, or how its rig is
//! built. It wants: *"give me what I bind to draw `mob.wisp`."* This crate answers
//! that. It **bakes** a [`ContentPack`] into an [`AssetBundle`] — skinned meshes,
//! textures, materials, shaders, rigs — indexed by lightweight [`handle`]s, and hands
//! the client a single [`AssetServer`] that resolves an entity to its draw set and
//! manages the hot-reload swap when content changes under a live match.
//!
//! ## The trivial-rendering contract
//!
//! ```ignore
//! // once, when a pack arrives (off the hot path):
//! let bundle = AssetBundle::bake(&pack, BakeQuality::high());
//! let mut assets = AssetServer::new(bundle);
//!
//! // per visible creature, once:
//! let visual = assets.mob_visual("mob.wisp").unwrap();
//! let mut animator = assets.bundle().rig(visual.rig).animator();
//!
//! // per frame, per creature: feed motion, get bones, draw.
//! animator.update(loco_input, dt);
//! let bones = animator.skinning_matrices();          // -> bone uniform buffer
//! let mesh = assets.bundle().mesh(visual.mesh);      // -> already-uploaded VBO/IBO
//! // bind material(visual.material) + bones + mesh and issue the draw. That's it.
//! ```
//!
//! Nothing here touches `wgpu`: every payload in [`render_asset`] is plain CPU data the
//! client uploads 1:1. So a headless server, a unit test, and the browser all bake the
//! exact same assets — and because the bakers are deterministic, two clients agree on
//! every vertex and bone.
//!
//! ## Module map
//!
//! - [`handle`]       — typed indices + content-addressed [`handle::AssetKey`].
//! - [`render_asset`] — the GPU-ready payload structs (the "upload me" types).
//! - [`rig`]          — auto-rig a creature, aligned to its procgen body.
//! - [`skin`]         — bind mesh vertices to rig bones (procedural skin weights).
//! - [`bake`]         — per-definition bakers + [`bake::BakeQuality`].
//! - [`bundle`]       — bake a whole pack into an [`AssetBundle`].
//! - [`server`]       — the [`AssetServer`]: live bundle + hot-reload swap.
//!
//! [`ContentPack`]: arena_content::pack::ContentPack

pub mod bake;
pub mod bundle;
pub mod handle;
pub mod render_asset;
pub mod rig;
pub mod server;
pub mod skin;

pub use bake::BakeQuality;
pub use bundle::{AssetBundle, ItemVisual, MobVisual};
pub use handle::{
    AssetKey, AssetKind, MaterialHandle, MeshHandle, RigHandle, ShaderHandle, TextureHandle,
};
pub use render_asset::{MaterialAsset, RigAsset, ShaderAsset, SkinnedMesh, TextureAsset};
pub use server::AssetServer;
