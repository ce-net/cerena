//! Asset handles and content-addressed keys.
//!
//! A handle is a small `Copy` index into one of the [`crate::bundle::AssetBundle`]'s
//! arrays. The renderer uploads each referenced asset to the GPU exactly once and
//! thereafter refers to it by handle — that indirection is what keeps per-frame draw
//! code trivial (look up handle → bind cached GPU resource) and what lets a hot-reload
//! swap the whole asset set behind stable handles.
//!
//! [`AssetKey`] is the *content-addressed* identity used by the on-demand cache: the
//! asset kind + a content id + the pack hash. Same key ⇒ same bytes ⇒ reuse the bake;
//! a new pack hash ⇒ a fresh key ⇒ a fresh bake (old ones evicted).

use serde::{Deserialize, Serialize};

/// Typed indices into an [`crate::bundle::AssetBundle`]. Separate newtypes (rather than
/// a generic `Handle<T>`) keep them `Copy` with no trait-bound friction and stop a
/// mesh handle being passed where a texture handle is expected.
macro_rules! handle {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name(pub u32);
        impl $name {
            pub fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

handle!(MeshHandle, "Index of a baked mesh in the bundle.");
handle!(TextureHandle, "Index of a baked texture in the bundle.");
handle!(MaterialHandle, "Index of a baked material in the bundle.");
handle!(ShaderHandle, "Index of a baked shader in the bundle.");
handle!(RigHandle, "Index of a baked skeleton + animator template in the bundle.");

/// The kind of asset a key addresses. Distinguishes otherwise-identical content ids
/// (a mob has both a mesh and a rig keyed off the same `MobId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetKind {
    Mesh,
    Texture,
    NormalMap,
    Material,
    Shader,
    Rig,
}

/// A content-addressed asset identity for the on-demand cache. Two keys are equal iff
/// they would bake to identical bytes, so a cache keyed on this never serves stale or
/// cross-pack data.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetKey {
    pub kind: AssetKind,
    /// The content id (e.g. `"mob.wisp"`, `"material.lava"`) or a synthetic id for
    /// ad-hoc bakes (`"seed:0x7001"`).
    pub id: String,
    /// The content pack's hash (its identity on the mesh). Folding this in means a
    /// content swap invalidates every key automatically.
    pub pack_hash: String,
    /// A discriminator for variants of the same id (LOD level, palette, etc.).
    pub variant: u32,
}

impl AssetKey {
    pub fn new(kind: AssetKind, id: impl Into<String>, pack_hash: impl Into<String>) -> Self {
        Self { kind, id: id.into(), pack_hash: pack_hash.into(), variant: 0 }
    }

    /// Same key with an explicit variant (LOD / palette).
    pub fn variant(mut self, v: u32) -> Self {
        self.variant = v;
        self
    }
}
