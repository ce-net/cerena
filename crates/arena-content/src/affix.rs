//! Affixes: the rollable magic properties that make two copies of the same base item
//! play differently. A dropped instance rolls up to its rarity's affix budget from the
//! pool, filtered by the item's slot and `affix_tags`.
//!
//! An affix is a *range*: `roll_lo`..`roll_hi` of [`StatMods`] plus an optional proc.
//! At drop time the forge rolls a `t in 0..1` per affix and lerps the stat block, so a
//! "Flaming" prefix can land anywhere from a weak to a god-tier roll — the engine of
//! ARPG chase. Naming a rolled item concatenates the highest-tier prefix word, the base
//! name, and the highest-tier suffix word: *"Flaming Ember Staff of the Bear"*.

use serde::{Deserialize, Serialize};

use crate::ids::AffixId;
use crate::item::{EquipSlot, ItemTrigger, StatMods};

/// Whether an affix reads as a leading adjective or a trailing "of the X" phrase. An
/// item may carry at most one *naming* prefix and one *naming* suffix, but several
/// non-naming affixes (their words are not shown, only their stats).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AffixKind {
    Prefix,
    Suffix,
}

/// One rollable property.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AffixDef {
    pub id: AffixId,
    pub kind: AffixKind,
    /// The word used in the item name (e.g. "Flaming", "of the Bear"). Empty = hidden.
    pub word: String,
    /// Tier 1 (lowliest) .. 8 (mythic). Higher tiers gate to higher item levels and
    /// roll bigger; magic-find biases the tier draw upward.
    pub tier: u8,
    /// Relative draw weight within the pool (after slot/tag/level filtering).
    pub weight: f32,
    /// Minimum item level required to roll this affix.
    pub level_req: u32,
    /// Slots this affix may appear on. Empty = any equippable slot.
    pub slots: Vec<EquipSlot>,
    /// Tags this affix belongs to; an item rolls only affixes sharing one of its
    /// `affix_tags` (plus the universal pool, tag `"universal"`).
    pub tags: Vec<String>,
    /// Low end of the stat roll (t = 0).
    pub roll_lo: StatMods,
    /// High end of the stat roll (t = 1).
    pub roll_hi: StatMods,
    /// An optional proc this affix grants (its magnitude does not roll; the stat block
    /// does). This is how a suffix like "of Storms" adds a chain-lightning-on-hit.
    pub proc: Option<ItemTrigger>,
}

impl AffixDef {
    /// Resolve the concrete stat block for a rolled `t in 0..1`.
    pub fn roll(&self, t: f32) -> StatMods {
        StatMods::lerp(&self.roll_lo, &self.roll_hi, t.clamp(0.0, 1.0))
    }

    /// Whether this affix may appear on an item in `slot` carrying `tags` at `ilvl`.
    pub fn eligible(&self, slot: EquipSlot, tags: &[String], ilvl: u32) -> bool {
        if ilvl < self.level_req {
            return false;
        }
        if !self.slots.is_empty() && !self.slots.contains(&slot) {
            return false;
        }
        if self.tags.iter().any(|t| t == "universal") {
            return true;
        }
        self.tags.iter().any(|t| tags.iter().any(|it| it == t))
    }
}

/// A terse builder so the gear pack reads as a table rather than 20-line literals.
pub fn affix(id: &str, kind: AffixKind, word: &str, tier: u8) -> AffixDef {
    AffixDef {
        id: AffixId::new(id),
        kind,
        word: word.to_string(),
        tier,
        weight: 100.0,
        level_req: (tier.saturating_sub(1) as u32) * 8,
        slots: Vec::new(),
        tags: vec!["universal".to_string()],
        roll_lo: StatMods::default(),
        roll_hi: StatMods::default(),
        proc: None,
    }
}

impl AffixDef {
    pub fn tagged(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn on_slots(mut self, slots: &[EquipSlot]) -> Self {
        self.slots = slots.to_vec();
        self
    }
    pub fn range(mut self, lo: StatMods, hi: StatMods) -> Self {
        self.roll_lo = lo;
        self.roll_hi = hi;
        self
    }
    pub fn weighted(mut self, w: f32) -> Self {
        self.weight = w;
        self
    }
    pub fn with_proc(mut self, p: ItemTrigger) -> Self {
        self.proc = Some(p);
        self
    }
}
