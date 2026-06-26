//! Spawn rules — how the open world populates and evolves, as hot-reloadable data.
//!
//! The "mystery world" of Cerena is not hand-placed; the authority spawns creatures
//! according to a list of [`SpawnRuleDef`]s. Each rule says *what* mob, *how* it
//! arrives (a steady trickle, scheduled waves, on entering a zone, on an objective),
//! *how many* may live at once, *where* (biome filter), how it scales with level, and
//! *what it drops*. Because the rules are data, the designer can repopulate or rebalance
//! the entire world live: turn the lowlands peaceful, unleash a wraith invasion on the
//! hollows, or spawn a guardian when players close on an objective — no code, no restart.

use serde::{Deserialize, Serialize};

use crate::ids::{LootTableId, MobId, SpawnRuleId};

/// What drives a spawn rule to fire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SpawnTrigger {
    /// Maintain population continuously: top up toward `max_alive` every `interval_s`.
    Continuous { interval_s: f32 },
    /// Spawn `wave_size` at once, repeating every `interval_s` (horde / invasion).
    Wave { wave_size: u32, interval_s: f32 },
    /// Spawn when a player first enters the rule's zone (ambush / guardian).
    OnZoneEnter,
    /// Spawn when an objective becomes active / is engaged.
    OnObjective,
}

/// A single data-driven spawn rule the authority evaluates while running the world.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRuleDef {
    pub id: SpawnRuleId,
    pub name: String,
    /// The creature to spawn (resolved against the active pack's mobs).
    pub mob: MobId,
    /// What makes this rule fire.
    pub trigger: SpawnTrigger,
    /// Cap on simultaneously-alive mobs from this rule. The authority multiplies this
    /// by [`crate::tuning::TuningConfig::mob_spawn_density`] for a global density knob.
    pub max_alive: u32,
    /// Biome tags this rule applies to (matched against world-gen biome names). Empty
    /// = any biome.
    pub biome_filter: Vec<String>,
    /// Per-player-level scaling applied to the spawned mob's health/damage so the
    /// world stays threatening as players advance.
    pub level_scaling: f32,
    /// Loot table these spawns roll on death (overrides the mob's own table when set),
    /// so the same creature can be made more/less rewarding by zone.
    pub loot_table: Option<LootTableId>,
}
