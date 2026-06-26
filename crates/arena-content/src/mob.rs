//! Mobs: non-player creatures and summons.
//!
//! A [`MobDef`] is pure data: health, speed, the abilities it casts, what it drops,
//! and the seed that grows its procedural body. The sim's AI/combat systems and the
//! summon op (`EffectOp::Summon`) both resolve mobs by [`MobId`]. World-gen biomes
//! reference mobs by id for spawning.

use serde::{Deserialize, Serialize};

use crate::ids::{AbilityId, ItemId, MaterialId, MobId};

/// A creature / summon archetype.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobDef {
    pub id: MobId,
    pub name: String,
    /// Starting / maximum health.
    pub max_health: f32,
    /// Movement speed in m/s.
    pub move_speed: f32,
    /// Abilities the mob's AI may cast (resolved against the active pack).
    pub abilities: Vec<AbilityId>,
    /// Experience awarded to the killer.
    pub xp_reward: u32,
    /// Drops as `(item, drop chance 0..1)`.
    pub loot_table: Vec<(ItemId, f32)>,
    /// Material skinning the creature's mesh.
    pub material: Option<MaterialId>,
    /// Visual scale multiplier.
    pub scale: f32,
    /// Whether it attacks players on sight (vs. neutral until provoked).
    pub aggressive: bool,
    /// Seed driving the procedural organic mesh grown in `arena-procgen` — two mobs
    /// sharing a def but different seeds would look like siblings, not clones.
    pub mesh_seed: u32,
}
