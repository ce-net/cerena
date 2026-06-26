//! Gems and runes: the things you socket into gear to customise it.
//!
//! Two ARPG traditions, both data:
//! - **Gems** read *differently in weapons vs armor* (a Ruby is +fire-damage in a
//!   weapon, +max-health in a robe), giving sockets a real placement decision.
//! - **Runes** are gems with a fixed identity used to spell out [`crate::enchant::RunewordDef`]s.
//!
//! Gems tier up by **fusion** (`fuse_from`): three Chipped Rubies make one Flawed Ruby,
//! and so on, so a socketed build is itself an upgrade sink.

use serde::{Deserialize, Serialize};

use crate::ids::GemId;
use crate::item::{EquipSlot, ItemTrigger, StatMods};

/// A socketable gem or rune.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GemDef {
    pub id: GemId,
    pub name: String,
    /// 1 (chipped) .. 5 (perfect / great rune). Higher tiers grant more.
    pub tier: u8,
    /// Stats granted when socketed into a *weapon/staff*.
    #[serde(default)]
    pub weapon_mods: StatMods,
    /// Stats granted when socketed into *armor* (robe/helm/gloves/boots/belt/offhand).
    #[serde(default)]
    pub armor_mods: StatMods,
    /// Stats granted when socketed into a *jewellery/relic* slot (ring/amulet/relic/trinket).
    #[serde(default)]
    pub jewel_mods: StatMods,
    /// A proc granted regardless of slot (rare; usually higher-tier gems / runes).
    #[serde(default)]
    pub proc: Option<ItemTrigger>,
    /// If this is a *rune* (used in runewords) this is its rune letter/name for the
    /// recipe matcher; ordinary gems leave it `None`.
    #[serde(default)]
    pub rune_symbol: Option<String>,
    /// Fusion recipe: `Some((lower_gem, count))` means `count` of `lower_gem` fuse into
    /// one of this. `None` for a base (chipped) gem.
    #[serde(default)]
    pub fuse_from: Option<(GemId, u8)>,
}

impl GemDef {
    /// The mods this gem contributes in a given slot family.
    pub fn mods_for(&self, slot: EquipSlot) -> StatMods {
        if slot.is_weapon() {
            self.weapon_mods
        } else if slot.is_armor() {
            self.armor_mods
        } else {
            self.jewel_mods
        }
    }
}

/// Terse builder.
pub fn gem(id: &str, name: &str, tier: u8) -> GemDef {
    GemDef {
        id: GemId::new(id),
        name: name.to_string(),
        tier,
        weapon_mods: StatMods::default(),
        armor_mods: StatMods::default(),
        jewel_mods: StatMods::default(),
        proc: None,
        rune_symbol: None,
        fuse_from: None,
    }
}

impl GemDef {
    pub fn weapon(mut self, m: StatMods) -> Self {
        self.weapon_mods = m;
        self
    }
    pub fn armor(mut self, m: StatMods) -> Self {
        self.armor_mods = m;
        self
    }
    pub fn jewel(mut self, m: StatMods) -> Self {
        self.jewel_mods = m;
        self
    }
    pub fn rune(mut self, symbol: &str) -> Self {
        self.rune_symbol = Some(symbol.to_string());
        self
    }
    pub fn fuses_from(mut self, lower: &str, count: u8) -> Self {
        self.fuse_from = Some((GemId::new(lower), count));
        self
    }
    pub fn with_proc(mut self, p: ItemTrigger) -> Self {
        self.proc = Some(p);
        self
    }
}
