//! [`ContentRegistry`]: the live, indexed, hot-swappable view of a [`ContentPack`].
//!
//! The sim and client hold a registry and resolve content by id every time they
//! need a definition. A new pack is **staged**, then **applied at a tick boundary**
//! so a swap never tears a half-simulated tick. Because lookups go through the
//! registry, the swap is atomic from the simulation's point of view.

use std::collections::HashMap;

use crate::{
    ContentError,
    ability::AbilityDef,
    gamemode::GameModeDef,
    ids::{
        AbilityId, GameModeId, ItemId, LootTableId, MaterialId, MobId, ShaderId, SpawnRuleId,
        SpellId, StatusId, TriggerId,
    },
    item::ItemDef,
    loot::LootTableDef,
    material::{MaterialDef, ShaderDef},
    mob::MobDef,
    pack::ContentPack,
    spawn::SpawnRuleDef,
    spell::SpellDef,
    status::StatusEffectDef,
    triggers::{GameTriggerKind, TriggerDef},
};

/// Indexed, read-optimized content. Rebuilt from a [`ContentPack`] on every swap.
pub struct ContentRegistry {
    /// Monotonic version epoch this registry currently serves.
    pub epoch: u64,
    /// Hash of the active pack (for diagnostics / client-server agreement checks).
    pub active_hash: String,
    pack: ContentPack,
    spells: HashMap<SpellId, usize>,
    items: HashMap<ItemId, usize>,
    abilities: HashMap<AbilityId, usize>,
    statuses: HashMap<StatusId, usize>,
    mobs: HashMap<MobId, usize>,
    materials: HashMap<MaterialId, usize>,
    shaders: HashMap<ShaderId, usize>,
    game_modes: HashMap<GameModeId, usize>,
    loot_tables: HashMap<LootTableId, usize>,
    spawn_rules: HashMap<SpawnRuleId, usize>,
    triggers: HashMap<TriggerId, usize>,
    /// A pack staged for the next tick-boundary swap, with its target epoch.
    pending: Option<(u64, ContentPack)>,
}

impl ContentRegistry {
    /// Build a registry from a pack at `epoch`. Validates and indexes.
    pub fn new(epoch: u64, pack: ContentPack) -> Result<Self, ContentError> {
        pack.validate()?;
        let active_hash = pack.hash();
        let mut r = ContentRegistry {
            epoch,
            active_hash,
            pack: ContentPack::empty(),
            spells: HashMap::new(),
            items: HashMap::new(),
            abilities: HashMap::new(),
            statuses: HashMap::new(),
            mobs: HashMap::new(),
            materials: HashMap::new(),
            shaders: HashMap::new(),
            game_modes: HashMap::new(),
            loot_tables: HashMap::new(),
            spawn_rules: HashMap::new(),
            triggers: HashMap::new(),
            pending: None,
        };
        r.reindex(pack);
        Ok(r)
    }

    /// A registry holding nothing, epoch 0. Safe to query (all lookups miss).
    pub fn bootstrap() -> Self {
        // empty() validates trivially.
        Self::new(0, ContentPack::empty()).expect("empty pack always valid")
    }

    fn reindex(&mut self, pack: ContentPack) {
        self.spells = pack.spells.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.items = pack.items.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.abilities = pack.abilities.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.statuses = pack.statuses.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.mobs = pack.mobs.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.materials = pack.materials.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.shaders = pack.shaders.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.game_modes = pack.game_modes.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.loot_tables = pack.loot_tables.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.spawn_rules = pack.spawn_rules.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.triggers = pack.triggers.iter().enumerate().map(|(i, d)| (d.id.clone(), i)).collect();
        self.active_hash = pack.hash();
        self.pack = pack;
    }

    /// Stage a new pack to be applied at the next tick boundary. Validated now so a
    /// bad pack is rejected before it can affect the live sim. The actual swap
    /// happens in [`ContentRegistry::apply_pending`].
    pub fn stage(&mut self, epoch: u64, pack: ContentPack) -> Result<(), ContentError> {
        if epoch <= self.epoch {
            return Err(ContentError::Invalid(format!(
                "stale content epoch {epoch} <= active {}",
                self.epoch
            )));
        }
        pack.validate()?;
        self.pending = Some((epoch, pack));
        Ok(())
    }

    /// True if a swap is queued. The tick loop calls this and, at a safe boundary,
    /// calls [`ContentRegistry::apply_pending`].
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Apply any staged pack. Returns the new epoch if a swap occurred. Called by
    /// the authority/client at a tick boundary so the swap is atomic.
    pub fn apply_pending(&mut self) -> Option<u64> {
        if let Some((epoch, pack)) = self.pending.take() {
            self.epoch = epoch;
            self.reindex(pack);
            Some(epoch)
        } else {
            None
        }
    }

    // ---- id resolution (the hot path) ----

    pub fn spell(&self, id: &SpellId) -> Option<&SpellDef> {
        self.spells.get(id).map(|&i| &self.pack.spells[i])
    }
    pub fn item(&self, id: &ItemId) -> Option<&ItemDef> {
        self.items.get(id).map(|&i| &self.pack.items[i])
    }
    pub fn ability(&self, id: &AbilityId) -> Option<&AbilityDef> {
        self.abilities.get(id).map(|&i| &self.pack.abilities[i])
    }
    pub fn status(&self, id: &StatusId) -> Option<&StatusEffectDef> {
        self.statuses.get(id).map(|&i| &self.pack.statuses[i])
    }
    pub fn mob(&self, id: &MobId) -> Option<&MobDef> {
        self.mobs.get(id).map(|&i| &self.pack.mobs[i])
    }
    pub fn material(&self, id: &MaterialId) -> Option<&MaterialDef> {
        self.materials.get(id).map(|&i| &self.pack.materials[i])
    }
    pub fn shader(&self, id: &ShaderId) -> Option<&ShaderDef> {
        self.shaders.get(id).map(|&i| &self.pack.shaders[i])
    }
    pub fn game_mode(&self, id: &GameModeId) -> Option<&GameModeDef> {
        self.game_modes.get(id).map(|&i| &self.pack.game_modes[i])
    }
    pub fn loot_table(&self, id: &LootTableId) -> Option<&LootTableDef> {
        self.loot_tables.get(id).map(|&i| &self.pack.loot_tables[i])
    }
    pub fn spawn_rule(&self, id: &SpawnRuleId) -> Option<&SpawnRuleDef> {
        self.spawn_rules.get(id).map(|&i| &self.pack.spawn_rules[i])
    }
    pub fn trigger(&self, id: &TriggerId) -> Option<&TriggerDef> {
        self.triggers.get(id).map(|&i| &self.pack.triggers[i])
    }

    /// The global balance numbers for this epoch. The sim should read movement,
    /// vitals, XP-curve, and loot constants from here rather than hardcoding them.
    pub fn tuning(&self) -> &crate::tuning::TuningConfig {
        &self.pack.tuning
    }

    /// Every trigger whose event matches `kind`. The sim's tick loop calls this when
    /// an event fires (e.g. `triggers_for(GameTriggerKind::Kill)` on a kill) and then
    /// checks each returned trigger's conditions before applying its actions. Returns
    /// them in pack order so firing is deterministic across nodes.
    pub fn triggers_for(&self, kind: GameTriggerKind) -> Vec<&TriggerDef> {
        self.pack
            .triggers
            .iter()
            .filter(|t| t.on.kind() == kind)
            .collect()
    }

    /// All spawn rules (the authority iterates these to populate the world).
    pub fn spawn_rules(&self) -> &[SpawnRuleDef] {
        &self.pack.spawn_rules
    }

    /// All game modes (the lobby/coordinator picks the active one by id).
    pub fn game_modes(&self) -> &[GameModeDef] {
        &self.pack.game_modes
    }

    /// The whole active pack (for the client to (re)generate procedural assets and
    /// recompile shaders after a swap).
    pub fn pack(&self) -> &ContentPack {
        &self.pack
    }
}
