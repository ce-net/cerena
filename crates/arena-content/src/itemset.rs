//! Gear sets: wearing several pieces of a matched set unlocks escalating bonuses.
//!
//! A [`SetDef`] lists its member items and a ladder of [`SetBonus`] thresholds. The
//! sim counts how many distinct set pieces a character has equipped and folds in every
//! bonus whose `pieces_required` is met — so a 2-piece grants a taste, the 6-piece the
//! signature payoff (a granted spell, a build-defining proc). Sets are the "aspirational
//! complete look" that pulls a player through the mid-game.

use serde::{Deserialize, Serialize};

use crate::ids::{ItemId, SetId, SpellId};
use crate::item::{ItemTrigger, StatMods};

/// One rung of a set's bonus ladder.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetBonus {
    /// How many distinct set pieces must be equipped for this bonus to apply.
    pub pieces_required: u8,
    /// Tooltip line ("(4) Pieces: +35% fire damage").
    pub description: String,
    /// Flat stats granted at this threshold.
    #[serde(default)]
    pub mods: StatMods,
    /// Procs granted at this threshold (the signature 6-piece effect).
    #[serde(default)]
    pub triggers: Vec<ItemTrigger>,
    /// Spells added to the spellbook at this threshold.
    #[serde(default)]
    pub grants_spells: Vec<SpellId>,
}

/// A named set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetDef {
    pub id: SetId,
    pub name: String,
    /// The full roster of items that count toward this set.
    pub pieces: Vec<ItemId>,
    /// Bonus ladder; the sim applies every entry whose threshold is met.
    pub bonuses: Vec<SetBonus>,
}

impl SetDef {
    /// All bonuses active at `equipped_count` equipped pieces.
    pub fn active_bonuses(&self, equipped_count: u8) -> impl Iterator<Item = &SetBonus> {
        self.bonuses
            .iter()
            .filter(move |b| equipped_count >= b.pieces_required)
    }
}
