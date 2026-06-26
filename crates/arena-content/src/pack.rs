//! A [`ContentPack`]: the versioned, content-addressed bundle of every definition.
//!
//! A pack is the unit of hot-reload. The designer edits definitions, builds a pack,
//! and publishes its bytes as a ce-net blob; its [`ContentPack::hash`] is its
//! identity. Authorities and clients fetch by hash, validate, and swap.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    ContentError,
    ability::AbilityDef,
    affix::AffixDef,
    enchant::{EnchantDef, RunewordDef},
    forge::ForgeConfig,
    gamemode::GameModeDef,
    gem::GemDef,
    item::ItemDef,
    itemset::SetDef,
    loot::LootTableDef,
    material::{MaterialDef, ShaderDef},
    mission::MissionDef,
    mob::MobDef,
    movement::MovementModeDef,
    spawn::SpawnRuleDef,
    spell::SpellDef,
    status::StatusEffectDef,
    tech::TechTree,
    triggers::{GameTrigger, RuleAction, TriggerCondition, TriggerDef},
    tuning::TuningConfig,
    worldgen::WorldGenParams,
};

/// Every piece of designer-authored data for one game build. Collections are flat
/// vectors keyed by each def's stable `id`; [`crate::registry::ContentRegistry`]
/// indexes them for O(1) lookup at swap time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPack {
    /// Human label for the pack (e.g. "0.4.2 — frost rework").
    pub label: String,
    pub spells: Vec<SpellDef>,
    pub items: Vec<ItemDef>,
    pub abilities: Vec<AbilityDef>,
    pub tech: TechTree,
    pub statuses: Vec<StatusEffectDef>,
    pub movement_modes: Vec<MovementModeDef>,
    pub materials: Vec<MaterialDef>,
    pub shaders: Vec<ShaderDef>,
    pub worldgen: WorldGenParams,
    pub mobs: Vec<MobDef>,
    pub missions: Vec<MissionDef>,
    /// Global balance numbers (gravity, speeds, regen, XP curve, loot fractions...).
    /// All gameplay magic-numbers live here so they hot-reload (see [`TuningConfig`]).
    pub tuning: TuningConfig,
    /// Match rule-sets (win/scoring/teams/loadout). Switching the active mode is live.
    pub game_modes: Vec<GameModeDef>,
    /// Named weighted drop pools referenced by mobs, missions, and death-drops.
    pub loot_tables: Vec<LootTableDef>,
    /// Data-driven rules for populating the open world with creatures.
    pub spawn_rules: Vec<SpawnRuleDef>,
    /// Data-driven event->action rules: the designer's scripting layer.
    pub triggers: Vec<TriggerDef>,

    // --- the gear / build system (all hot-reloadable like everything else) ---
    /// Rollable magic properties that drop instances roll from.
    #[serde(default)]
    pub affixes: Vec<AffixDef>,
    /// Socketable gems and runes.
    #[serde(default)]
    pub gems: Vec<GemDef>,
    /// Gear sets and their bonus ladders.
    #[serde(default)]
    pub item_sets: Vec<SetDef>,
    /// Permanent applied enchants.
    #[serde(default)]
    pub enchants: Vec<EnchantDef>,
    /// Runeword recipes.
    #[serde(default)]
    pub runewords: Vec<RunewordDef>,
    /// House rules for the forge (upgrade/reforge/socket/enchant economy).
    #[serde(default)]
    pub forge: ForgeConfig,
}

impl ContentPack {
    /// Deterministic content hash (hex sha256 over the bincode bytes). This is the
    /// pack's identity on the mesh; clients verify what they fetched matches the
    /// hash a coordinator announced.
    pub fn hash(&self) -> String {
        let bytes = bincode::serialize(self).unwrap_or_default();
        let mut h = Sha256::new();
        h.update(&bytes);
        hex_lower(&h.finalize())
    }

    /// Serialize for transport / blob storage.
    pub fn encode(&self) -> Result<Vec<u8>, ContentError> {
        bincode::serialize(self).map_err(|e| ContentError::Decode(e.to_string()))
    }

    /// Decode and verify against an expected hash (the one the coordinator
    /// announced). Rejects a pack whose bytes do not hash to `expected`.
    pub fn decode_verified(bytes: &[u8], expected: &str) -> Result<Self, ContentError> {
        let pack: ContentPack =
            bincode::deserialize(bytes).map_err(|e| ContentError::Decode(e.to_string()))?;
        let got = pack.hash();
        if got != expected {
            return Err(ContentError::HashMismatch {
                expected: expected.to_string(),
                got,
            });
        }
        Ok(pack)
    }

    /// Structural validation run before a pack is accepted for swap. Catches
    /// dangling references (an ability pointing at a missing spell, a tech node
    /// granting a missing item) that would otherwise surface mid-match.
    pub fn validate(&self) -> Result<(), ContentError> {
        use std::collections::HashSet;
        let spell_ids: HashSet<_> = self.spells.iter().map(|s| s.id.clone()).collect();
        let item_ids: HashSet<_> = self.items.iter().map(|i| i.id.clone()).collect();
        let status_ids: HashSet<_> = self.statuses.iter().map(|s| s.id.clone()).collect();
        let ability_ids: HashSet<_> = self.abilities.iter().map(|a| a.id.clone()).collect();
        let mob_ids: HashSet<_> = self.mobs.iter().map(|m| m.id.clone()).collect();
        let tech_ids: HashSet<_> = self.tech.nodes.iter().map(|n| n.id.clone()).collect();
        let loot_table_ids: HashSet<_> = self.loot_tables.iter().map(|t| t.id.clone()).collect();

        for a in &self.abilities {
            if !spell_ids.contains(&a.spell) {
                return Err(ContentError::Invalid(format!(
                    "ability {} references missing spell {}",
                    a.id, a.spell
                )));
            }
        }
        for node in &self.tech.nodes {
            for it in &node.unlock_items {
                if !item_ids.contains(it) {
                    return Err(ContentError::Invalid(format!(
                        "tech node {} unlocks missing item {}",
                        node.id, it
                    )));
                }
            }
        }
        // Spell ops that reference a status must resolve.
        for s in &self.spells {
            check_status_refs(&s.root, &status_ids, &s.id.0)?;
        }

        // Game modes: starting loadouts and items must resolve.
        for m in &self.game_modes {
            for ab in &m.starting_loadout {
                if !ability_ids.contains(ab) {
                    return Err(ContentError::Invalid(format!(
                        "game mode {} starting_loadout references missing ability {ab}",
                        m.id
                    )));
                }
            }
            for (it, _) in &m.starting_items {
                if !item_ids.contains(it) {
                    return Err(ContentError::Invalid(format!(
                        "game mode {} starting_items references missing item {it}",
                        m.id
                    )));
                }
            }
        }

        // Loot tables: every entry's item must exist.
        for t in &self.loot_tables {
            for e in &t.entries {
                if !item_ids.contains(&e.item) {
                    return Err(ContentError::Invalid(format!(
                        "loot table {} references missing item {}",
                        t.id, e.item
                    )));
                }
            }
        }

        // Spawn rules: the mob and any loot-table override must resolve.
        for r in &self.spawn_rules {
            if !mob_ids.contains(&r.mob) {
                return Err(ContentError::Invalid(format!(
                    "spawn rule {} references missing mob {}",
                    r.id, r.mob
                )));
            }
            if let Some(lt) = &r.loot_table {
                if !loot_table_ids.contains(lt) {
                    return Err(ContentError::Invalid(format!(
                        "spawn rule {} references missing loot table {lt}",
                        r.id
                    )));
                }
            }
        }

        // Triggers: every id named by an event, condition, or action must resolve.
        for tr in &self.triggers {
            match &tr.on {
                GameTrigger::OnPickup { item } => {
                    if !item_ids.contains(item) {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} fires on pickup of missing item {item}",
                            tr.id
                        )));
                    }
                }
                GameTrigger::OnSpellCast { spell } => {
                    if !spell_ids.contains(spell) {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} fires on cast of missing spell {spell}",
                            tr.id
                        )));
                    }
                }
                _ => {}
            }
            for c in &tr.conditions {
                match c {
                    TriggerCondition::HasItem { item } if !item_ids.contains(item) => {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} condition references missing item {item}",
                            tr.id
                        )));
                    }
                    TriggerCondition::HasTech { node } if !tech_ids.contains(node) => {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} condition references missing tech node {node}",
                            tr.id
                        )));
                    }
                    _ => {}
                }
            }
            for a in &tr.actions {
                match a {
                    RuleAction::GrantItem { item, .. } if !item_ids.contains(item) => {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} action grants missing item {item}",
                            tr.id
                        )));
                    }
                    RuleAction::ApplyStatus { status, .. } if !status_ids.contains(status) => {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} action applies missing status {status}",
                            tr.id
                        )));
                    }
                    RuleAction::SpawnMob { mob, .. } if !mob_ids.contains(mob) => {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} action spawns missing mob {mob}",
                            tr.id
                        )));
                    }
                    RuleAction::SpawnLoot { table } if !loot_table_ids.contains(table) => {
                        return Err(ContentError::Invalid(format!(
                            "trigger {} action rolls missing loot table {table}",
                            tr.id
                        )));
                    }
                    _ => {}
                }
            }
        }

        // Gear: set rosters and runeword sequences must resolve against real content.
        let gem_symbols: std::collections::HashSet<_> = self
            .gems
            .iter()
            .filter_map(|g| g.rune_symbol.clone())
            .collect();
        for s in &self.item_sets {
            for it in &s.pieces {
                if !item_ids.contains(it) {
                    return Err(ContentError::Invalid(format!(
                        "set {} lists missing item {it}",
                        s.id
                    )));
                }
            }
        }
        for rw in &self.runewords {
            for sym in &rw.sequence {
                if !gem_symbols.contains(sym) {
                    return Err(ContentError::Invalid(format!(
                        "runeword {} needs missing rune symbol {sym}",
                        rw.id
                    )));
                }
            }
        }
        Ok(())
    }

    /// An empty pack (used as a safe default before the first real pack loads).
    pub fn empty() -> Self {
        Self {
            label: "empty".into(),
            spells: vec![],
            items: vec![],
            abilities: vec![],
            tech: TechTree::default(),
            statuses: vec![],
            movement_modes: vec![],
            materials: vec![],
            shaders: vec![],
            worldgen: WorldGenParams::default(),
            mobs: vec![],
            missions: vec![],
            tuning: TuningConfig::default(),
            game_modes: vec![],
            loot_tables: vec![],
            spawn_rules: vec![],
            triggers: vec![],
            affixes: vec![],
            gems: vec![],
            item_sets: vec![],
            enchants: vec![],
            runewords: vec![],
            forge: ForgeConfig::default(),
        }
    }
}

fn check_status_refs(
    op: &crate::spell::EffectOp,
    statuses: &std::collections::HashSet<crate::ids::StatusId>,
    spell: &str,
) -> Result<(), ContentError> {
    if let crate::spell::EffectOp::ApplyStatus { status, .. } = op {
        if !statuses.contains(status) {
            return Err(ContentError::Invalid(format!(
                "spell {spell} applies missing status {status}"
            )));
        }
    }
    for c in op.children() {
        check_status_refs(c, statuses, spell)?;
    }
    Ok(())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
