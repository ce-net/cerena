//! The tech tree — long-term progression from novice to archmage.
//!
//! A [`TechTree`] is a DAG of [`TechNode`]s grouped into branches (Pyromancy,
//! Cryomancy, Arcana, Mobility, ...). Unlocking a node spends skill points and, via
//! its [`TechEffect`]s, grants abilities/movement, unlocks recipes, multiplies stats,
//! or — crucially — *raises the spell-authoring [`crate::spell::Budget`]* so experts
//! can craft elaborate custom spells novices cannot. All data, all hot-reloadable.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::ids::{AbilityId, ItemId, MaterialId, MovementModeId, SpellId, TechNodeId};
use crate::item::StatMods;

/// What unlocking a tech node grants. The progression system applies each effect to
/// the player when the node is purchased.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TechEffect {
    /// Make an ability available to equip.
    UnlockAbility(AbilityId),
    /// Make a movement mode available.
    UnlockMovement(MovementModeId),
    /// Make a crafting recipe (the recipe on this item) craftable.
    UnlockRecipe(ItemId),
    /// Permanent additive stat modifier.
    StatMult(StatMods),
    /// Raise the player's spell-authoring budget (lets experts build deeper, more
    /// complex custom spells). Adds to `Budget::max_complexity` / `max_depth`.
    RaiseSpellBudget { complexity: u32, depth: u32 },
    /// Directly grant a finished spell.
    GrantSpell(SpellId),
}

/// One node in the tech tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechNode {
    pub id: TechNodeId,
    pub name: String,
    pub description: String,
    /// The branch this node belongs to (e.g. "Pyromancy"); for UI grouping.
    pub branch: String,
    /// Depth within the branch (0 = root tier).
    pub tier: u32,
    /// Skill points required to unlock.
    pub cost_skill_points: u32,
    /// Nodes that must already be unlocked.
    pub prereqs: Vec<TechNodeId>,
    /// Effects applied when unlocked.
    pub effects: Vec<TechEffect>,
    /// Items this node makes available (validated against the pack's items).
    pub unlock_items: Vec<ItemId>,
    /// Material for the node's icon.
    pub icon_material: Option<MaterialId>,
}

/// The full tech tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TechTree {
    pub nodes: Vec<TechNode>,
}

impl Default for TechTree {
    fn default() -> Self {
        Self { nodes: vec![] }
    }
}

impl TechTree {
    /// Look up a node by id.
    pub fn node(&self, id: &TechNodeId) -> Option<&TechNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    /// True if every prerequisite of `node` is in `unlocked`.
    pub fn prereqs_met(&self, node: &TechNode, unlocked: &HashSet<TechNodeId>) -> bool {
        node.prereqs.iter().all(|p| unlocked.contains(p))
    }

    /// Every node whose prerequisites are met but which is not yet unlocked — i.e.
    /// the frontier a player can currently spend points on.
    pub fn available(&self, unlocked: &HashSet<TechNodeId>) -> Vec<&TechNode> {
        self.nodes
            .iter()
            .filter(|n| !unlocked.contains(&n.id) && self.prereqs_met(n, unlocked))
            .collect()
    }
}
