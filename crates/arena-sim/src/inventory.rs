//! Character inventory and equipment.
//!
//! An [`Inventory`] is the bag of [`ItemId`]s a character carries plus the items
//! they have equipped (one per body slot). Equipping an item is what grants its
//! [`StatMods`], its spells, its movement modes and its abilities — the inventory
//! is the join between hot-reloadable [`ItemDef`]s and a live character's
//! capabilities. Everything keys off stable ids, so re-tuning an item is a
//! hot-reload, never a save migration.
//!
//! Note: `arena_content::EquipSlot` is `Eq` but not `Hash`, so equipped items are a
//! small `Vec<(EquipSlot, ItemId)>` (at most one entry per slot) rather than a
//! `HashMap` — semantically the same, and it keeps the slot type untouched.

use serde::{Deserialize, Serialize};

use arena_content::ids::{AbilityId, ItemId, MovementModeId, SpellId};
use arena_content::item::{CraftRecipe, EquipSlot, ItemDef, StatMods};
use arena_content::registry::ContentRegistry;

/// What a character carries and wears.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    /// Carried stacks: `(item, quantity)`. Non-stackable items appear as `qty 1`.
    pub slots: Vec<(ItemId, u16)>,
    /// Equipped items, at most one per non-`None` [`EquipSlot`].
    pub equipped: Vec<(EquipSlot, ItemId)>,
}

impl Inventory {
    /// Add `qty` of an item, merging into an existing stack.
    pub fn add_item(&mut self, item: ItemId, qty: u16) {
        if let Some(slot) = self.slots.iter_mut().find(|(id, _)| *id == item) {
            slot.1 = slot.1.saturating_add(qty);
        } else {
            self.slots.push((item, qty));
        }
    }

    /// Remove up to `qty`; returns how many were actually removed.
    pub fn remove_item(&mut self, item: &ItemId, qty: u16) -> u16 {
        if let Some(idx) = self.slots.iter().position(|(id, _)| id == item) {
            let have = self.slots[idx].1;
            let take = have.min(qty);
            let left = have - take;
            if left == 0 {
                self.slots.remove(idx);
            } else {
                self.slots[idx].1 = left;
            }
            take
        } else {
            0
        }
    }

    /// Total quantity of `item` carried.
    pub fn count(&self, item: &ItemId) -> u16 {
        self.slots
            .iter()
            .find(|(id, _)| id == item)
            .map(|(_, q)| *q)
            .unwrap_or(0)
    }

    /// The item currently equipped in `slot`, if any.
    pub fn equipped_in(&self, slot: EquipSlot) -> Option<&ItemId> {
        self.equipped.iter().find(|(s, _)| *s == slot).map(|(_, i)| i)
    }

    /// Equip `item` (must be carried). Any item already in that slot is unequipped
    /// back into the bag. Returns false if the item is unknown or not carried.
    pub fn equip(&mut self, content: &ContentRegistry, item: &ItemId) -> bool {
        let Some(def) = content.item(item) else {
            return false;
        };
        if def.slot == EquipSlot::None {
            return false; // pure reagent, never equippable
        }
        if self.count(item) == 0 {
            return false;
        }
        // Move out anything already in the slot.
        if let Some(prev) = self.equipped_in(def.slot).cloned() {
            self.unequip(def.slot);
            let _ = prev; // returned to bag by unequip
        }
        // Consume one from the bag into the slot.
        self.remove_item(item, 1);
        self.equipped.push((def.slot, item.clone()));
        true
    }

    /// Unequip whatever is in `slot`, returning it to the bag.
    pub fn unequip(&mut self, slot: EquipSlot) {
        if let Some(idx) = self.equipped.iter().position(|(s, _)| *s == slot) {
            let (_, item) = self.equipped.remove(idx);
            self.add_item(item, 1);
        }
    }

    /// Sum the [`StatMods`] of every equipped item. Combined with tech `StatMult`
    /// by the caller into the character's effective stats.
    pub fn aggregate_mods(&self, content: &ContentRegistry) -> StatMods {
        let mut acc = StatMods::default();
        for (_, id) in &self.equipped {
            if let Some(def) = content.item(id) {
                acc = acc.combine(&def.stat_mods);
            }
        }
        acc
    }

    /// All spells granted by currently equipped gear.
    pub fn granted_spells(&self, content: &ContentRegistry) -> Vec<SpellId> {
        self.collect_equipped(content, |d| d.grants_spells.clone())
    }

    /// All abilities granted by currently equipped gear.
    pub fn granted_abilities(&self, content: &ContentRegistry) -> Vec<AbilityId> {
        self.collect_equipped(content, |d| d.grants_abilities.clone())
    }

    /// All movement modes granted by currently equipped gear.
    pub fn granted_movement(&self, content: &ContentRegistry) -> Vec<MovementModeId> {
        self.collect_equipped(content, |d| d.grants_movement.clone())
    }

    fn collect_equipped<T, F>(&self, content: &ContentRegistry, f: F) -> Vec<T>
    where
        F: Fn(&ItemDef) -> Vec<T>,
        T: PartialEq,
    {
        let mut out: Vec<T> = Vec::new();
        for (_, id) in &self.equipped {
            if let Some(def) = content.item(id) {
                for v in f(def) {
                    if !out.contains(&v) {
                        out.push(v);
                    }
                }
            }
        }
        out
    }

    /// Consume one of a consumable item, returning the spell its `on_use` fires (the
    /// caller casts it). Returns `None` if the item is missing, not consumable, or
    /// has no `on_use` spell.
    pub fn consume(&mut self, content: &ContentRegistry, item: &ItemId) -> Option<SpellId> {
        let def = content.item(item)?;
        let spell = def.on_use.clone()?;
        if self.remove_item(item, 1) == 0 {
            return None;
        }
        Some(spell)
    }

    /// Whether the bag holds every reagent a recipe needs (tech gate checked by the
    /// caller against the character's unlocked tech).
    pub fn can_craft(&self, recipe: &CraftRecipe) -> bool {
        recipe.inputs.iter().all(|(id, qty)| self.count(id) >= *qty)
    }

    /// Consume a recipe's reagents and produce `output`. Returns false (and changes
    /// nothing) if the inputs are not all present.
    pub fn craft(&mut self, recipe: &CraftRecipe, output: ItemId) -> bool {
        if !self.can_craft(recipe) {
            return false;
        }
        for (id, qty) in &recipe.inputs {
            self.remove_item(id, *qty);
        }
        self.add_item(output, 1);
        true
    }
}
