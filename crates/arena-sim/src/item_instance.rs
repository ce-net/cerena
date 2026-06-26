//! Item *instances*: the per-character, per-item state that turns a shared
//! [`arena_content::item::ItemDef`] base into *your* +9 Flaming Ember Staff of Storms
//! with a Perfect Ruby in the socket.
//!
//! The content registry holds the *base* (stats, signature procs, upgrade profile). An
//! [`ItemInstance`] layers on everything that is rolled or earned per copy: upgrade
//! level, quality, rolled affixes, socketed gems, an applied enchant, an active
//! runeword, and live progression (kill count for growing items). All of it folds back
//! into one effective [`StatMods`] + one effective trigger list that the rest of the
//! sim consumes exactly like a plain equipped item — so combat, movement, and the proc
//! engine never need to know an item was upgraded.
//!
//! Instances reference content by **stable id**, so a hot-reload that re-tunes a base,
//! an affix, or a gem changes every instance's effective stats live, without disturbing
//! identity, sockets, or upgrade level. That is the whole point of the id indirection.

use serde::{Deserialize, Serialize};

use arena_content::ids::{AffixId, EnchantId, GemId, ItemId, RunewordId};
use arena_content::item::{EquipSlot, ItemTrigger, Rarity, StatMods};
use arena_content::registry::ContentRegistry;

/// A per-inventory unique handle for an instance (equip slots reference this, not the
/// base id, so you can own two distinct rolls of the same base).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct InstanceId(pub u64);

/// A rolled affix frozen onto an instance: which affix, and the concrete stats it
/// rolled (the `t in 0..1` is baked in at drop time so the roll is stable forever,
/// until a reforge replaces it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RolledAffix {
    pub affix: AffixId,
    /// The frozen roll position (0..1), kept so the UI can show "87% roll" and so a
    /// hot-reload that widens an affix range re-derives the value at the same quality.
    pub roll_t: f32,
    /// Whether the player has imprinted (locked) this affix against the next reforge.
    pub imprinted: bool,
}

/// What occupies a socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SocketFill {
    Empty,
    Gem(GemId),
}

/// One owned copy of an item, with all of its earned/rolled state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemInstance {
    pub id: InstanceId,
    /// The base definition id (resolved against the live registry).
    pub base: ItemId,
    /// Effective rarity (a base may be promoted by magic-find at drop time).
    pub rarity: Rarity,
    /// Forge upgrade level (+N).
    pub upgrade_level: u8,
    /// Quality 0..100; a flat multiplier on base stats, raised by polishing.
    pub quality: u8,
    /// Rolled random affixes (empty for unique/mythic signature items).
    pub affixes: Vec<RolledAffix>,
    /// Sockets, in order. Length is the instance's socket count.
    pub sockets: Vec<SocketFill>,
    /// A permanently applied enchant, if any.
    pub enchant: Option<EnchantId>,
    /// Live progression for "growing" items: kills credited to this item. Signature
    /// procs may read it (e.g. The Hungering Edge). Persisted with the instance.
    pub kills: u32,
    /// The node id this instance is soulbound to (None = freely lootable).
    pub bound_to: Option<[u8; 32]>,
}

impl ItemInstance {
    /// A plain, unrolled instance of a base — what a vendor or a starter loadout hands
    /// out. Drops go through `forge::roll_drop` instead.
    pub fn plain(id: InstanceId, base: &ItemId, content: &ContentRegistry) -> Self {
        let (rarity, sockets) = content
            .item(base)
            .map(|d| (d.rarity, d.base_sockets as usize))
            .unwrap_or((Rarity::Common, 0));
        Self {
            id,
            base: base.clone(),
            rarity,
            upgrade_level: 0,
            quality: 0,
            affixes: Vec::new(),
            sockets: vec![SocketFill::Empty; sockets],
            enchant: None,
            kills: 0,
            bound_to: None,
        }
    }

    /// The slot this instance occupies (`None` if its base is gone after a bad reload).
    pub fn slot(&self, content: &ContentRegistry) -> Option<EquipSlot> {
        content.item(&self.base).map(|d| d.slot)
    }

    /// The ordered rune symbols currently socketed (for runeword matching).
    pub fn socketed_runes(&self, content: &ContentRegistry) -> Vec<String> {
        self.sockets
            .iter()
            .filter_map(|s| match s {
                SocketFill::Gem(g) => content.gem(g).and_then(|gd| gd.rune_symbol.clone()),
                SocketFill::Empty => None,
            })
            .collect()
    }

    /// The runeword active on this instance, if its sockets spell one out. When active,
    /// the individual gem stats are *replaced* by the runeword's combined bonus.
    pub fn active_runeword(&self, content: &ContentRegistry) -> Option<RunewordId> {
        let slot = self.slot(content)?;
        let runes = self.socketed_runes(content);
        if runes.is_empty() {
            return None;
        }
        content.match_runeword(slot, &runes).map(|rw| rw.id.clone())
    }

    /// Fold every layer into one effective [`StatMods`]. This is what the inventory
    /// aggregates across equipped gear. Layers, in order:
    /// 1. base stats, scaled by upgrade level and quality,
    /// 2. each rolled affix's frozen roll,
    /// 3. socketed gems (slot-aware) — *unless* a runeword is active, in which case the
    ///    runeword's combined mods replace all gem contributions,
    /// 4. the applied enchant,
    /// 5. signature "growing" bonuses (kills).
    pub fn effective_mods(&self, content: &ContentRegistry) -> StatMods {
        let Some(def) = content.item(&self.base) else {
            return StatMods::default();
        };
        let forge = content.forge_config();

        // 1. base, grown by upgrade + quality.
        let upgrade_mult = 1.0
            + (self.upgrade_level as f32)
                * def.upgrade.as_ref().map(|u| u.stat_growth).unwrap_or(0.0);
        let quality_mult = 1.0 + (self.quality as f32) * forge.quality_stat_per_point;
        let mut acc = def.stat_mods.scaled(upgrade_mult * quality_mult);

        // 2. affixes (frozen rolls).
        for ra in &self.affixes {
            if let Some(ad) = content.affix(&ra.affix) {
                acc = acc.combine(&ad.roll(ra.roll_t));
            }
        }

        // 3. sockets OR runeword.
        if let Some(rw_id) = self.active_runeword(content) {
            if let Some(rw) = content.runeword(&rw_id) {
                acc = acc.combine(&rw.mods);
            }
        } else {
            let slot = def.slot;
            for s in &self.sockets {
                if let SocketFill::Gem(g) = s {
                    if let Some(gd) = content.gem(g) {
                        acc = acc.combine(&gd.mods_for(slot));
                    }
                }
            }
        }

        // 4. enchant.
        if let Some(eid) = &self.enchant {
            if let Some(ed) = content.enchant(eid) {
                acc = acc.combine(&ed.mods);
            }
        }

        // 5. growing items: The Hungering Edge and friends read `kills` here. Capped so
        //    a 10k-kill veteran is strong, not infinite. (+0.5% melee per kill, cap +50%.)
        if self.base.as_str().contains("hungering_edge") {
            let bonus = (self.kills as f32 * 0.005).min(0.5);
            acc = acc.combine(&StatMods { melee_power_pct: bonus, ..StatMods::default() });
        }

        acc
    }

    /// Every proc this instance contributes: base signature triggers (scaled in
    /// magnitude by upgrade level via the proc-growth curve), affix procs, gem procs,
    /// the enchant proc, and — if active — the runeword's triggers (which then suppress
    /// individual gem procs, mirroring the stat replacement).
    pub fn effective_triggers(&self, content: &ContentRegistry) -> Vec<ItemTrigger> {
        let mut out = Vec::new();
        let Some(def) = content.item(&self.base) else {
            return out;
        };

        // Base signature procs, magnitude grown by upgrade level.
        let proc_growth = def.upgrade.as_ref().map(|u| u.proc_growth).unwrap_or(0.0);
        let proc_mult = 1.0 + (self.upgrade_level as f32) * proc_growth;
        for t in &def.triggers {
            out.push(scale_trigger(t, proc_mult));
        }

        // Affix procs.
        for ra in &self.affixes {
            if let Some(ad) = content.affix(&ra.affix) {
                if let Some(p) = &ad.proc {
                    out.push(p.clone());
                }
            }
        }

        // Runeword procs replace gem procs; otherwise gem procs apply.
        if let Some(rw_id) = self.active_runeword(content) {
            if let Some(rw) = content.runeword(&rw_id) {
                out.extend(rw.triggers.iter().cloned());
            }
        } else {
            for s in &self.sockets {
                if let SocketFill::Gem(g) = s {
                    if let Some(gd) = content.gem(g) {
                        if let Some(p) = &gd.proc {
                            out.push(p.clone());
                        }
                    }
                }
            }
        }

        // Enchant proc.
        if let Some(eid) = &self.enchant {
            if let Some(ed) = content.enchant(eid) {
                if let Some(p) = &ed.proc {
                    out.push(p.clone());
                }
            }
        }

        out
    }

    /// Build the display name: `[enchant word] [prefix word] <base> [suffix word] (+N)`.
    /// Unique/mythic items keep their authored name (plus +N). Active runewords prepend
    /// the runeword name. This is what the client renders, tinted by `rarity`.
    pub fn display_name(&self, content: &ContentRegistry) -> String {
        let base_name = content
            .item(&self.base)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| self.base.0.clone());

        if let Some(rw_id) = self.active_runeword(content) {
            if let Some(rw) = content.runeword(&rw_id) {
                return self.with_plus(format!("{} '{}'", base_name, rw.name));
            }
        }

        let is_unique = content.item(&self.base).map(|d| d.unique).unwrap_or(false);
        if is_unique {
            return self.with_plus(base_name);
        }

        // Pick the highest-tier naming prefix and suffix among rolled affixes.
        let mut prefix: Option<(u8, String)> = None;
        let mut suffix: Option<(u8, String)> = None;
        for ra in &self.affixes {
            let Some(ad) = content.affix(&ra.affix) else { continue };
            if ad.word.is_empty() {
                continue;
            }
            let slot = match ad.kind {
                arena_content::affix::AffixKind::Prefix => &mut prefix,
                arena_content::affix::AffixKind::Suffix => &mut suffix,
            };
            if slot.as_ref().map(|(t, _)| ad.tier > *t).unwrap_or(true) {
                *slot = Some((ad.tier, ad.word.clone()));
            }
        }

        let enchant_word = self
            .enchant
            .as_ref()
            .and_then(|e| content.enchant(e))
            .and_then(|e| e.name_word.clone());

        let mut name = String::new();
        if let Some(w) = enchant_word {
            name.push_str(&w);
            name.push(' ');
        }
        if let Some((_, w)) = &prefix {
            name.push_str(w);
            name.push(' ');
        }
        name.push_str(&base_name);
        if let Some((_, w)) = &suffix {
            name.push(' ');
            name.push_str(w);
        }
        self.with_plus(name)
    }

    fn with_plus(&self, name: String) -> String {
        if self.upgrade_level > 0 {
            format!("{name} (+{})", self.upgrade_level)
        } else {
            name
        }
    }

    /// A one-line power score for sorting/vendoring: the sum of the magnitudes of the
    /// effective mods, weighted lightly. Cosmetic — the sim never gates on it.
    pub fn item_score(&self, content: &ContentRegistry) -> f32 {
        let m = self.effective_mods(content);
        m.power + m.focus + m.vitality + m.agility
            + (m.max_health + m.max_mana) * 0.1
            + (m.spell_power_pct + m.melee_power_pct + m.crit_chance + m.crit_damage) * 100.0
            + self.upgrade_level as f32 * 5.0
            + self.affixes.len() as f32 * 8.0
            + self.rarity as u8 as f32 * 12.0
    }
}

/// Scale a trigger's *effect magnitude* (not its chance/ICD) by `mult`. Used to grow a
/// signature proc with the item's upgrade level so a +12 legendary hits like a legend.
fn scale_trigger(t: &ItemTrigger, mult: f32) -> ItemTrigger {
    use arena_content::item::ProcEffect as PE;
    let effect = match &t.effect {
        PE::Nova { element, radius, damage } => PE::Nova {
            element: element.clone(),
            radius: *radius,
            damage: damage * mult,
        },
        PE::ChainBolt { element, jumps, damage } => PE::ChainBolt {
            element: element.clone(),
            jumps: *jumps,
            damage: damage * mult,
        },
        PE::Heal { amount } => PE::Heal { amount: amount * mult },
        PE::Shield { amount, duration_s } => PE::Shield {
            amount: amount * mult,
            duration_s: *duration_s,
        },
        PE::TempStats { mods, duration_s } => PE::TempStats {
            mods: Box::new(mods.scaled(mult)),
            duration_s: *duration_s,
        },
        // Status/cast/summon procs scale by their own definitions, not by magnitude.
        other => other.clone(),
    };
    ItemTrigger {
        label: t.label.clone(),
        when: t.when.clone(),
        chance: t.chance,
        icd_s: t.icd_s,
        effect,
    }
}
