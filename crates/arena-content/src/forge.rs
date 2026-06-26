//! Global forge configuration: the tunable numbers behind upgrading, reforging,
//! socketing, and enchanting. Per-item upgrade scaling lives on the item
//! ([`crate::item::UpgradeProfile`]); this is the *house rules* every forge obeys, so
//! it hot-reloads with the rest of the pack and the economy can be retuned live.

use serde::{Deserialize, Serialize};

use crate::ids::ItemId;

/// House rules for the forge. All of these are read by `arena_sim::forge`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForgeConfig {
    /// Essence cost multiplier applied per current upgrade level (cost climbs as you
    /// push a `+N` higher).
    pub upgrade_cost_growth: f32,
    /// Above an item's `safe_until`, a failed upgrade has this chance to *also* shave a
    /// level (otherwise a failure just burns the essence). 0 = forgiving, 1 = brutal.
    pub downgrade_on_fail: f32,
    /// Reagent + count to **reforge** (reroll all random affixes) one item.
    pub reforge_cost: (ItemId, u16),
    /// Reagent + count to **imprint** (lock one affix so the next reforge keeps it).
    pub imprint_cost: (ItemId, u16),
    /// Reagent + count to **add a socket** (up to the rarity budget).
    pub socket_cost: (ItemId, u16),
    /// Reagent + count to **unsocket** a gem without destroying it (cheaper than re-fusing).
    pub unsocket_cost: (ItemId, u16),
    /// Quality cap (0..100). Quality adds a flat % to the item's base mods and is raised
    /// by polishing reagents; it never resets on reforge.
    pub quality_cap: u8,
    /// Per-point quality contribution to the base stat multiplier (0.004 = +0.4%/point,
    /// so a perfect 100-quality item carries +40% on its base before affixes).
    pub quality_stat_per_point: f32,
    /// How strongly magic-find shifts a drop up the rarity ladder. Effective extra
    /// "promotion rolls" = `magic_find * mf_to_promote`.
    pub mf_to_promote: f32,
    /// How strongly magic-find biases the affix *tier* draw upward.
    pub mf_to_tier: f32,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            upgrade_cost_growth: 0.5,
            downgrade_on_fail: 0.25,
            reforge_cost: (ItemId::new("item.essence.chaos"), 1),
            imprint_cost: (ItemId::new("item.essence.binding"), 1),
            socket_cost: (ItemId::new("item.essence.boring"), 1),
            unsocket_cost: (ItemId::new("item.essence.solvent"), 1),
            quality_cap: 100,
            quality_stat_per_point: 0.004,
            mf_to_promote: 0.9,
            mf_to_tier: 1.4,
        }
    }
}
