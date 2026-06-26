//! The [`AssetServer`]: holds the live [`AssetBundle`] and manages the **hot-reload
//! swap**. It is the single object the renderer talks to.
//!
//! When a new content pack arrives, the client bakes a fresh bundle off the hot path
//! and [`AssetServer::stage`]s it; at a safe boundary (between frames) it calls
//! [`AssetServer::apply_staged`], and every subsequent lookup resolves against the new
//! assets. Because handles are bundle-local, the renderer just re-uploads on a
//! generation bump — there is no partial, torn asset state.
//!
//! An [`AdHocCache`] is provided for meshes that aren't part of a pack (one-off
//! projectile/VFX shapes generated from a seed), keyed content-addressably so repeated
//! requests reuse the bake.

use std::collections::HashMap;

use crate::bundle::{AssetBundle, ItemVisual, MobVisual};
use crate::handle::AssetKey;
use crate::render_asset::SkinnedMesh;

/// Owns the active asset bundle and a staged replacement for hot-reload.
#[derive(Debug)]
pub struct AssetServer {
    active: AssetBundle,
    staged: Option<AssetBundle>,
    /// Bumped every time the active bundle is replaced, so the renderer knows to
    /// re-upload its GPU mirror of the assets.
    generation: u64,
    adhoc: AdHocCache,
}

impl AssetServer {
    /// Create a server around an initial bundle.
    pub fn new(initial: AssetBundle) -> Self {
        Self { active: initial, staged: None, generation: 0, adhoc: AdHocCache::default() }
    }

    /// The live bundle (the draw-time lookup surface).
    pub fn bundle(&self) -> &AssetBundle {
        &self.active
    }

    /// The pack hash currently in effect.
    pub fn pack_hash(&self) -> &str {
        &self.active.pack_hash
    }

    /// Generation counter — changes exactly when the active bundle is swapped, so the
    /// renderer can lazily re-upload only when it differs from what it last saw.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Stage a freshly-baked bundle to become active at the next [`apply_staged`]. A
    /// no-op-friendly fast path: if it matches the active pack hash, drop it (nothing
    /// changed). Baking happens off this call; staging is just a hand-off.
    ///
    /// [`apply_staged`]: AssetServer::apply_staged
    pub fn stage(&mut self, bundle: AssetBundle) {
        if bundle.pack_hash == self.active.pack_hash {
            return;
        }
        self.staged = Some(bundle);
    }

    /// True if a different bundle is waiting to go live.
    pub fn has_staged(&self) -> bool {
        self.staged.is_some()
    }

    /// Promote the staged bundle to active (call between frames). Returns the new
    /// generation if a swap happened, else `None`. The ad-hoc cache is cleared because
    /// its keys fold in the (now-stale) pack hash.
    pub fn apply_staged(&mut self) -> Option<u64> {
        if let Some(next) = self.staged.take() {
            self.active = next;
            self.generation += 1;
            self.adhoc.clear();
            Some(self.generation)
        } else {
            None
        }
    }

    // --- convenience pass-throughs to the active bundle --------------------

    /// Draw set for a creature (see [`AssetBundle::mob_visual`]).
    pub fn mob_visual(&self, mob_id: &str) -> Option<MobVisual> {
        self.active.mob_visual(mob_id)
    }

    /// Draw set for an item (see [`AssetBundle::item_visual`]).
    pub fn item_visual(&self, item_id: &str) -> Option<ItemVisual> {
        self.active.item_visual(item_id)
    }

    /// Get (or lazily bake + cache) an ad-hoc mesh from a generator, keyed
    /// content-addressably. For projectile/VFX shapes that aren't pack assets.
    pub fn adhoc_mesh<F>(&mut self, key: AssetKey, bake: F) -> &SkinnedMesh
    where
        F: FnOnce() -> SkinnedMesh,
    {
        self.adhoc.get_or_insert_with(key, bake)
    }
}

/// A tiny content-addressed cache for meshes outside any pack. Keys fold in the pack
/// hash, so a content swap (which clears this cache) can never serve stale geometry.
#[derive(Debug, Default)]
pub struct AdHocCache {
    meshes: HashMap<AssetKey, SkinnedMesh>,
}

impl AdHocCache {
    /// Fetch the cached mesh for `key`, baking and inserting it on a miss.
    pub fn get_or_insert_with<F>(&mut self, key: AssetKey, bake: F) -> &SkinnedMesh
    where
        F: FnOnce() -> SkinnedMesh,
    {
        self.meshes.entry(key).or_insert_with(bake)
    }

    /// Drop everything (called on a content swap).
    pub fn clear(&mut self) {
        self.meshes.clear();
    }

    pub fn len(&self) -> usize {
        self.meshes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.meshes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bake::BakeQuality;
    use crate::handle::{AssetKind, AssetKey};
    use crate::render_asset::SkinnedMesh;
    use arena_content::{default_pack, pack::ContentPack};

    #[test]
    fn swap_bumps_generation_and_changes_lookup() {
        let pack = default_pack();
        let server_bundle = AssetBundle::bake(&pack, BakeQuality::low());
        let mut server = AssetServer::new(server_bundle);
        assert_eq!(server.generation(), 0);

        // Staging the same pack is a no-op.
        server.stage(AssetBundle::bake(&pack, BakeQuality::low()));
        assert!(!server.has_staged());

        // Staging an empty (different-hash) pack swaps on apply.
        let empty = ContentPack::empty();
        server.stage(AssetBundle::bake(&empty, BakeQuality::low()));
        assert!(server.has_staged());
        assert_eq!(server.apply_staged(), Some(1));
        assert!(server.mob_visual("mob.wisp").is_none(), "empty pack has no mobs");
    }

    #[test]
    fn adhoc_cache_reuses_bakes() {
        let pack = default_pack();
        let mut server = AssetServer::new(AssetBundle::bake(&pack, BakeQuality::low()));
        let key = AssetKey::new(AssetKind::Mesh, "seed:0x1234", server.pack_hash().to_string());
        let mut bakes = 0;
        for _ in 0..3 {
            server.adhoc_mesh(key.clone(), || {
                bakes += 1;
                SkinnedMesh::default()
            });
        }
        assert_eq!(bakes, 1, "an ad-hoc mesh should bake once and cache");
    }
}
