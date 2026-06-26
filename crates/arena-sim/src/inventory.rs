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

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use arena_content::ids::{AbilityId, ItemId, MovementModeId, SpellId};
use arena_content::item::{CraftRecipe, EquipSlot, ItemDef, ItemTrigger, StatMods};
use arena_content::registry::ContentRegistry;

use crate::item_instance::{InstanceId, ItemInstance};

/// What a character carries and wears.
///
/// Two parallel stores, by design:
/// - **stacks** (`slots`): fungible items addressed only by [`ItemId`] — reagents,
///   essences, gems-in-bag, consumables. No per-copy state, so `(id, qty)` is enough.
/// - **instances** (`instances`): rolled gear with per-copy state (upgrade, affixes,
///   sockets, enchant). Equipping references an [`InstanceId`], so you can own two
///   different rolls of the same base and wear the better one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    /// Carried stacks: `(item, quantity)`. Non-stackable items appear as `qty 1`.
    pub slots: Vec<(ItemId, u16)>,
    /// Equipped *fungible* items (legacy / starter gear with no instance), at most one
    /// per non-`None` [`EquipSlot`]. New gear flows through `equipped_instances`.
    pub equipped: Vec<(EquipSlot, ItemId)>,
    /// Carried rolled instances (the "stash").
    #[serde(default)]
    pub instances: Vec<ItemInstance>,
    /// Equipped instances, by slot. Rings may appear more than once (see
    /// [`EquipSlot::is_multi`]).
    #[serde(default)]
    pub equipped_instances: Vec<(EquipSlot, InstanceId)>,
    /// Monotonic counter for minting fresh [`InstanceId`]s in this inventory.
    #[serde(default)]
    pub next_instance: u64,
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

    /// Sum the [`StatMods`] of everything equipped — legacy fungible gear *and* rolled
    /// instances (base + upgrade + affixes + gems + enchant + runeword) *and* every
    /// active set bonus. Combined with tech `StatMult` by the caller into the
    /// character's effective stats. Delegates to [`Inventory::effective_mods`] so every
    /// combat call site picks up the full build with no change.
    pub fn aggregate_mods(&self, content: &ContentRegistry) -> StatMods {
        self.effective_mods(content)
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

    // ================================================================================
    // Rolled instances: the modern gear path (upgrade/affixes/sockets/sets/procs).
    // ================================================================================

    /// Mint the next instance handle for this inventory.
    pub fn mint_instance_id(&mut self) -> InstanceId {
        let id = InstanceId(self.next_instance);
        self.next_instance += 1;
        id
    }

    /// Stash a rolled instance (a drop the player picked up).
    pub fn add_instance(&mut self, inst: ItemInstance) {
        self.instances.push(inst);
    }

    /// Borrow a carried-or-equipped instance by handle.
    pub fn instance(&self, id: InstanceId) -> Option<&ItemInstance> {
        self.instances.iter().find(|i| i.id == id)
    }
    /// Mutably borrow a carried-or-equipped instance (forge operations target this).
    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut ItemInstance> {
        self.instances.iter_mut().find(|i| i.id == id)
    }

    /// Equip a carried instance into its base item's slot. Anything already in a
    /// non-multi slot is unequipped (it stays in `instances`, just not worn). Rings use
    /// the first free ring "slot index" (two rings allowed). Returns false if the
    /// instance is missing, its base is unknown, or the slot is `None`.
    pub fn equip_instance(&mut self, content: &ContentRegistry, id: InstanceId) -> bool {
        let Some(inst) = self.instance(id) else { return false };
        let Some(def) = content.item(&inst.base) else { return false };
        let slot = def.slot;
        if slot == EquipSlot::None || slot == EquipSlot::Consumable {
            return false;
        }
        if slot.is_multi() {
            // Allow up to two rings; replace the oldest if both full.
            let count = self.equipped_instances.iter().filter(|(s, _)| *s == slot).count();
            if count >= 2 {
                if let Some(pos) = self.equipped_instances.iter().position(|(s, _)| *s == slot) {
                    self.equipped_instances.remove(pos);
                }
            }
        } else {
            self.equipped_instances.retain(|(s, _)| *s != slot);
        }
        self.equipped_instances.push((slot, id));
        true
    }

    /// Unequip the instance in `slot` (the first one for multi slots). It remains stashed.
    pub fn unequip_instance(&mut self, slot: EquipSlot) {
        if let Some(pos) = self.equipped_instances.iter().position(|(s, _)| *s == slot) {
            self.equipped_instances.remove(pos);
        }
    }

    /// Iterate the equipped instances (resolved), skipping any whose handle dangles.
    pub fn equipped_instance_refs(&self) -> impl Iterator<Item = &ItemInstance> {
        self.equipped_instances
            .iter()
            .filter_map(move |(_, id)| self.instances.iter().find(|i| i.id == *id))
    }

    /// Count how many distinct pieces of each set are equipped (for set bonuses).
    fn equipped_set_counts(&self, content: &ContentRegistry) -> HashMap<String, u8> {
        let mut counts: HashMap<String, u8> = HashMap::new();
        for inst in self.equipped_instance_refs() {
            if let Some(def) = content.item(&inst.base) {
                if let Some(set) = &def.set {
                    *counts.entry(set.0.clone()).or_insert(0) += 1;
                }
            }
        }
        counts
    }

    /// The full effective [`StatMods`] of everything worn: legacy fungible gear, every
    /// equipped instance (base + upgrade + affixes + gems + enchant + runeword), plus
    /// every active set bonus. This is what `arena_sim::rpg` folds into derived stats.
    pub fn effective_mods(&self, content: &ContentRegistry) -> StatMods {
        let mut acc = StatMods::default();

        // Legacy fungible equipped items (starter loadouts, simple drops).
        for (_, id) in &self.equipped {
            if let Some(def) = content.item(id) {
                acc = acc.combine(&def.stat_mods);
            }
        }
        // Instances.
        for inst in self.equipped_instance_refs() {
            acc = acc.combine(&inst.effective_mods(content));
        }
        // Set bonuses.
        for (set_id, count) in self.equipped_set_counts(content) {
            if let Some(set) = content.item_set(&arena_content::ids::SetId::new(&set_id)) {
                for bonus in set.active_bonuses(count) {
                    acc = acc.combine(&bonus.mods);
                }
            }
        }
        acc
    }

    /// Every proc currently granted by worn gear: instance triggers (base + affixes +
    /// gems + enchant + runeword) and active set-bonus triggers. The sim's
    /// `item_procs::evaluate` runs these against each event.
    pub fn equipped_triggers(&self, content: &ContentRegistry) -> Vec<ItemTrigger> {
        let mut out = Vec::new();
        for inst in self.equipped_instance_refs() {
            out.extend(inst.effective_triggers(content));
        }
        for (set_id, count) in self.equipped_set_counts(content) {
            if let Some(set) = content.item_set(&arena_content::ids::SetId::new(&set_id)) {
                for bonus in set.active_bonuses(count) {
                    out.extend(bonus.triggers.iter().cloned());
                }
            }
        }
        out
    }

    /// Spells granted by worn instances and active set bonuses (folded with the legacy
    /// [`Inventory::granted_spells`] path by the caller).
    pub fn instance_granted_spells(&self, content: &ContentRegistry) -> Vec<SpellId> {
        let mut out: Vec<SpellId> = Vec::new();
        for inst in self.equipped_instance_refs() {
            if let Some(def) = content.item(&inst.base) {
                for s in &def.grants_spells {
                    if !out.contains(s) {
                        out.push(s.clone());
                    }
                }
            }
            if let Some(rw_id) = inst.active_runeword(content) {
                if let Some(rw) = content.runeword(&rw_id) {
                    for s in &rw.grants_spells {
                        if !out.contains(s) {
                            out.push(s.clone());
                        }
                    }
                }
            }
        }
        for (set_id, count) in self.equipped_set_counts(content) {
            if let Some(set) = content.item_set(&arena_content::ids::SetId::new(&set_id)) {
                for bonus in set.active_bonuses(count) {
                    for s in &bonus.grants_spells {
                        if !out.contains(s) {
                            out.push(s.clone());
                        }
                    }
                }
            }
        }
        out
    }

    /// Credit a kill to a specific equipped instance (growing items like The Hungering
    /// Edge). The caller picks which weapon scored the kill.
    pub fn credit_kill(&mut self, id: InstanceId) {
        if let Some(inst) = self.instance_mut(id) {
            inst.kills = inst.kills.saturating_add(1);
        }
    }
}
