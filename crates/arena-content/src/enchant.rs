//! Enchants and runewords: the top of the customisation stack.
//!
//! - An [`EnchantDef`] is a permanent layer the player applies to one item (at most one
//!   enchant per item) at the forge, spending reagents. It adds stats and/or a proc on
//!   top of everything else — the final ~5% min-max.
//! - A [`RunewordDef`] is a named *sequence of runes*: socket exactly those rune gems,
//!   in order, into an eligible base, and the individual gem stats are **replaced** by a
//!   far stronger combined bonus (a granted spell, a build-defining proc). This is the
//!   classic "the runes are worthless apart, legendary together" chase.

use serde::{Deserialize, Serialize};

use crate::ids::{EnchantId, RunewordId, SpellId};
use crate::item::{EquipSlot, ItemTrigger, StatMods};

/// A permanent applied enchant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnchantDef {
    pub id: EnchantId,
    pub name: String,
    /// Slots this enchant may be applied to. Empty = any equippable slot.
    #[serde(default)]
    pub slots: Vec<EquipSlot>,
    /// Minimum item level to apply.
    #[serde(default)]
    pub level_req: u32,
    #[serde(default)]
    pub mods: StatMods,
    #[serde(default)]
    pub proc: Option<ItemTrigger>,
    /// Whether this enchant is shown as a leading word in the item name.
    #[serde(default)]
    pub name_word: Option<String>,
}

/// A runeword recipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunewordDef {
    pub id: RunewordId,
    pub name: String,
    /// The exact ordered sequence of rune symbols (see [`crate::gem::GemDef::rune_symbol`])
    /// that must be socketed, in order, to activate the word.
    pub sequence: Vec<String>,
    /// Eligible base slots (e.g. a weapon runeword only activates in `Weapon`/`Staff`).
    pub base_slots: Vec<EquipSlot>,
    /// Minimum item level for the word to function.
    #[serde(default)]
    pub level_req: u32,
    /// The combined bonus, replacing the sockets' individual gem stats while active.
    #[serde(default)]
    pub mods: StatMods,
    #[serde(default)]
    pub triggers: Vec<ItemTrigger>,
    #[serde(default)]
    pub grants_spells: Vec<SpellId>,
}

impl RunewordDef {
    /// Does `socketed` (the ordered rune symbols actually in the item) activate this
    /// word in an item of `slot`? Requires an exact ordered match of the full sequence.
    pub fn matches(&self, slot: EquipSlot, socketed: &[String]) -> bool {
        (self.base_slots.is_empty() || self.base_slots.contains(&slot))
            && socketed.len() == self.sequence.len()
            && socketed.iter().zip(&self.sequence).all(|(a, b)| a == b)
    }
}
