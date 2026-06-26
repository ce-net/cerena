//! Items: equippable gear, consumables, and craftable materials.
//!
//! An [`ItemDef`] is pure data. Items are the main way a player gains *capabilities*
//! (spells, movement modes, abilities) and *stats* in the world: a staff grants a
//! fireball, boots grant a dash, a robe boosts mana. Live inventories store only
//! [`ItemId`]s, so re-tuning an item is a hot-reload, not a migration.

use serde::{Deserialize, Serialize};

use crate::ids::{AbilityId, ItemId, MaterialId, MovementModeId, SpellId, TechNodeId};

/// How rare / powerful an item is. Drives drop weighting, tint, and the VFX budget
/// the client spends on its glow. Purely a designer-facing tier; the sim does not
/// branch on it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
    Mythic,
}

/// The body slot an item occupies. A character may equip one item per non-`None`
/// slot (relics/trinkets/consumables are handled by their own inventory rules in
/// the sim). `None` means the item is never equipped (pure crafting reagent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EquipSlot {
    Staff,
    Robe,
    Ring,
    Amulet,
    Boots,
    Relic,
    Consumable,
    Trinket,
    None,
}

/// Flat, additive stat modifiers an item contributes while equipped. All fields are
/// `f32` and default to zero, so an item only states what it changes. The sim sums
/// every equipped item's mods (see [`StatMods::combine`]) into the character's
/// effective stats each tick.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct StatMods {
    /// Raw spell damage / effect magnitude.
    pub power: f32,
    /// Casting precision; scales spells via [`crate::spell::Scaling::focus`].
    pub focus: f32,
    /// Movement / parkour responsiveness.
    pub agility: f32,
    /// Survivability attribute.
    pub vitality: f32,
    /// Flat addition to the mana pool.
    pub max_mana: f32,
    /// Flat addition to the health pool.
    pub max_health: f32,
    /// Flat addition to base movement speed (m/s).
    pub move_speed: f32,
    /// Fraction (0..1) shaved off ability cooldowns.
    pub cooldown_reduction: f32,
    /// Mana regenerated per second.
    pub mana_regen: f32,
    /// Multiplicative spell-power bonus, expressed as a percentage fraction
    /// (e.g. `0.15` = +15% spell power). Summed additively across items, then
    /// applied once by the sim.
    pub spell_power_pct: f32,
}

impl StatMods {
    /// Sum this with another set of mods field-by-field. Used to fold all equipped
    /// items (and tech `StatMult` effects) into one effective modifier.
    pub fn combine(&self, other: &Self) -> Self {
        Self {
            power: self.power + other.power,
            focus: self.focus + other.focus,
            agility: self.agility + other.agility,
            vitality: self.vitality + other.vitality,
            max_mana: self.max_mana + other.max_mana,
            max_health: self.max_health + other.max_health,
            move_speed: self.move_speed + other.move_speed,
            cooldown_reduction: self.cooldown_reduction + other.cooldown_reduction,
            mana_regen: self.mana_regen + other.mana_regen,
            spell_power_pct: self.spell_power_pct + other.spell_power_pct,
        }
    }
}

/// A crafting recipe: a bag of input items (id + quantity) and an optional tech-tree
/// gate. The sim's crafting system consumes the inputs and produces the owning item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftRecipe {
    /// Required reagents as `(item, quantity)` pairs.
    pub inputs: Vec<(ItemId, u16)>,
    /// A tech node that must be unlocked before this recipe is craftable.
    pub tech_req: Option<TechNodeId>,
}

/// A complete item definition. The flagship way players gain power: equip it for its
/// [`StatMods`], or for the spells / movement modes / abilities it grants; or use a
/// consumable to fire its `on_use` spell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDef {
    pub id: ItemId,
    pub name: String,
    /// Tooltip flavour text; cosmetic.
    pub description: String,
    pub rarity: Rarity,
    pub slot: EquipSlot,
    /// Stats granted while equipped.
    pub stat_mods: StatMods,
    /// Spells this item adds to the wielder's spellbook while equipped.
    pub grants_spells: Vec<SpellId>,
    /// Movement / parkour modes unlocked while equipped.
    pub grants_movement: Vec<MovementModeId>,
    /// Equipped abilities (bound actions) this item provides.
    pub grants_abilities: Vec<AbilityId>,
    /// Procedural material used to texture the item mesh.
    pub material: Option<MaterialId>,
    /// Whether multiple copies stack in one inventory slot.
    pub stackable: bool,
    /// Maximum count per stack (1 if not stackable).
    pub max_stack: u16,
    /// A spell fired when the item is consumed/activated (potions, scrolls).
    pub on_use: Option<SpellId>,
    /// Minimum character level required to equip / use.
    pub level_req: u32,
    /// Optional crafting recipe that produces this item.
    pub craft: Option<CraftRecipe>,
}
