//! Missions: procedural objectives and their rewards.
//!
//! A [`MissionDef`] is a list of [`Objective`]s plus payouts (xp, items, tech points).
//! The sim's quest system tracks progress against the objectives; because missions
//! are data they can be generated, tuned, and hot-reloaded freely.

use serde::{Deserialize, Serialize};

use crate::ids::{ItemId, MissionId, MobId, SpellId};

/// A single trackable objective. The quest system advances each as the relevant
/// game event fires; a mission completes when all its objectives are satisfied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Objective {
    /// Kill `count` creatures; `mob = None` means any creature.
    Kill { mob: Option<MobId>, count: u32 },
    /// Gather `count` of an item.
    Collect { item: ItemId, count: u32 },
    /// Reach within `radius` metres of a world `point`.
    ReachPoint { point: [f32; 3], radius: f32 },
    /// Stay alive for `seconds`.
    Survive { seconds: f32 },
    /// Cast `count` spells; `spell = None` means any spell.
    CastSpell { spell: Option<SpellId>, count: u32 },
    /// Discover `zone_count` distinct world zones.
    Explore { zone_count: u32 },
}

/// A mission definition: objectives plus rewards and gating.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MissionDef {
    pub id: MissionId,
    pub name: String,
    /// Briefing / flavour text.
    pub description: String,
    /// All objectives that must be completed.
    pub objectives: Vec<Objective>,
    /// Experience granted on completion.
    pub xp_reward: u32,
    /// Item payouts as `(item, quantity)`.
    pub item_rewards: Vec<(ItemId, u16)>,
    /// Skill / tech points granted on completion.
    pub tech_points: u32,
    /// Minimum character level to accept.
    pub level_req: u32,
    /// Whether the mission can be taken again after completion.
    pub repeatable: bool,
}
