//! A [`ContentPack`]: the versioned, content-addressed bundle of every definition.
//!
//! A pack is the unit of hot-reload. The designer edits definitions, builds a pack,
//! and publishes its bytes as a ce-net blob; its [`ContentPack::hash`] is its
//! identity. Authorities and clients fetch by hash, validate, and swap.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ContentError,
    ability::AbilityDef,
    item::ItemDef,
    material::{MaterialDef, ShaderDef},
    mission::MissionDef,
    mob::MobDef,
    movement::MovementModeDef,
    spell::SpellDef,
    status::StatusEffectDef,
    tech::TechTree,
    worldgen::WorldGenParams,
};

/// Every piece of designer-authored data for one game build. Collections are flat
/// vectors keyed by each def's stable `id`; [`crate::registry::ContentRegistry`]
/// indexes them for O(1) lookup at swap time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPack {
    /// Human label for the pack (e.g. "0.4.2 — frost rework").
    pub label: String,
    pub spells: Vec<SpellDef>,
    pub items: Vec<ItemDef>,
    pub abilities: Vec<AbilityDef>,
    pub tech: TechTree,
    pub statuses: Vec<StatusEffectDef>,
    pub movement_modes: Vec<MovementModeDef>,
    pub materials: Vec<MaterialDef>,
    pub shaders: Vec<ShaderDef>,
    pub worldgen: WorldGenParams,
    pub mobs: Vec<MobDef>,
    pub missions: Vec<MissionDef>,
}

impl ContentPack {
    /// Deterministic content hash (hex sha256 over the bincode bytes). This is the
    /// pack's identity on the mesh; clients verify what they fetched matches the
    /// hash a coordinator announced.
    pub fn hash(&self) -> String {
        let bytes = bincode::serialize(self).unwrap_or_default();
        let mut h = Sha256::new();
        h.update(&bytes);
        hex_lower(&h.finalize())
    }

    /// Serialize for transport / blob storage.
    pub fn encode(&self) -> Result<Vec<u8>, ContentError> {
        bincode::serialize(self).map_err(|e| ContentError::Decode(e.to_string()))
    }

    /// Decode and verify against an expected hash (the one the coordinator
    /// announced). Rejects a pack whose bytes do not hash to `expected`.
    pub fn decode_verified(bytes: &[u8], expected: &str) -> Result<Self, ContentError> {
        let pack: ContentPack =
            bincode::deserialize(bytes).map_err(|e| ContentError::Decode(e.to_string()))?;
        let got = pack.hash();
        if got != expected {
            return Err(ContentError::HashMismatch {
                expected: expected.to_string(),
                got,
            });
        }
        Ok(pack)
    }

    /// Structural validation run before a pack is accepted for swap. Catches
    /// dangling references (an ability pointing at a missing spell, a tech node
    /// granting a missing item) that would otherwise surface mid-match.
    pub fn validate(&self) -> Result<(), ContentError> {
        use std::collections::HashSet;
        let spell_ids: HashSet<_> = self.spells.iter().map(|s| s.id.clone()).collect();
        let item_ids: HashSet<_> = self.items.iter().map(|i| i.id.clone()).collect();
        let status_ids: HashSet<_> = self.statuses.iter().map(|s| s.id.clone()).collect();

        for a in &self.abilities {
            if !spell_ids.contains(&a.spell) {
                return Err(ContentError::Invalid(format!(
                    "ability {} references missing spell {}",
                    a.id, a.spell
                )));
            }
        }
        for node in &self.tech.nodes {
            for it in &node.unlock_items {
                if !item_ids.contains(it) {
                    return Err(ContentError::Invalid(format!(
                        "tech node {} unlocks missing item {}",
                        node.id, it
                    )));
                }
            }
        }
        // Spell ops that reference a status must resolve.
        for s in &self.spells {
            check_status_refs(&s.root, &status_ids, &s.id.0)?;
        }
        Ok(())
    }

    /// An empty pack (used as a safe default before the first real pack loads).
    pub fn empty() -> Self {
        Self {
            label: "empty".into(),
            spells: vec![],
            items: vec![],
            abilities: vec![],
            tech: TechTree::default(),
            statuses: vec![],
            movement_modes: vec![],
            materials: vec![],
            shaders: vec![],
            worldgen: WorldGenParams::default(),
            mobs: vec![],
            missions: vec![],
        }
    }
}

fn check_status_refs(
    op: &crate::spell::EffectOp,
    statuses: &std::collections::HashSet<crate::ids::StatusId>,
    spell: &str,
) -> Result<(), ContentError> {
    if let crate::spell::EffectOp::ApplyStatus { status, .. } = op {
        if !statuses.contains(status) {
            return Err(ContentError::Invalid(format!(
                "spell {spell} applies missing status {status}"
            )));
        }
    }
    for c in op.children() {
        check_status_refs(c, statuses, spell)?;
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
