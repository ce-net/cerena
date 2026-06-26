//! Triggers — a data-driven event/scripting system, so new game logic ships as data.
//!
//! This is the extensible "systems" layer. Instead of hardcoding "on level-up, grant a
//! skill point and announce it" or "killing in the hollows sometimes drops loot" in
//! `arena-sim`, the designer writes [`TriggerDef`]s: *when* something happens
//! ([`GameTrigger`]), *if* some conditions hold ([`TriggerCondition`]), *do* a list of
//! actions ([`RuleAction`]). New behaviour therefore needs **no code deploy** — it is
//! authored as data and hot-reloaded into the live match like any other content.
//!
//! ## How the sim uses this
//!
//! `arena-sim`'s tick loop, when an event occurs, asks the registry for the triggers
//! whose `on` matches that event's kind (see
//! [`crate::registry::ContentRegistry::triggers_for`]), checks each trigger's
//! conditions against the actor/world, and applies the actions. Evaluation is
//! deterministic: a [`TriggerCondition::Chance`] consumes the same per-tick
//! deterministic seed the rest of the sim uses (NO wall-clock, NO ambient rng), so
//! every authority replaying the tick produces identical results.

use serde::{Deserialize, Serialize};

use crate::ids::{
    ItemId, LootTableId, MobId, SpellId, StatusId, TechNodeId, TriggerId,
};

/// The event that may fire a trigger. The sim raises one of these per game event and
/// matches it (by [`GameTriggerKind`]) against authored triggers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GameTrigger {
    /// An actor scored a kill.
    OnKill,
    /// An actor died.
    OnDeath,
    /// A player entered a tagged zone.
    OnZoneEnter { zone_tag: String },
    /// A player reached `level`.
    OnLevelUp { level: u32 },
    /// A player picked up `item`.
    OnPickup { item: ItemId },
    /// A periodic tick every `interval_s` seconds (ambient world logic).
    OnTimer { interval_s: f32 },
    /// An objective was completed.
    OnObjectiveComplete,
    /// A specific spell was cast (combo/synergy hooks).
    OnSpellCast { spell: SpellId },
}

/// A coarse discriminant for [`GameTrigger`], independent of its payload. Used to
/// bucket triggers so the sim can fetch "all OnKill triggers" without matching on the
/// specific zone tag / spell / level inside the variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GameTriggerKind {
    Kill,
    Death,
    ZoneEnter,
    LevelUp,
    Pickup,
    Timer,
    ObjectiveComplete,
    SpellCast,
}

impl GameTrigger {
    /// The payload-free discriminant of this trigger, for bucketed lookup.
    pub fn kind(&self) -> GameTriggerKind {
        match self {
            GameTrigger::OnKill => GameTriggerKind::Kill,
            GameTrigger::OnDeath => GameTriggerKind::Death,
            GameTrigger::OnZoneEnter { .. } => GameTriggerKind::ZoneEnter,
            GameTrigger::OnLevelUp { .. } => GameTriggerKind::LevelUp,
            GameTrigger::OnPickup { .. } => GameTriggerKind::Pickup,
            GameTrigger::OnTimer { .. } => GameTriggerKind::Timer,
            GameTrigger::OnObjectiveComplete => GameTriggerKind::ObjectiveComplete,
            GameTrigger::OnSpellCast { .. } => GameTriggerKind::SpellCast,
        }
    }
}

/// A gate that must hold for a matched trigger to fire its actions. All conditions on
/// a trigger must pass (logical AND); compose richer logic with multiple triggers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TriggerCondition {
    /// Always passes.
    Always,
    /// The actor is at least `level`.
    MinLevel { level: u32 },
    /// The actor holds `item`.
    HasItem { item: ItemId },
    /// The actor has unlocked tech `node`.
    HasTech { node: TechNodeId },
    /// Passes with probability `p` (0..1), drawn from the deterministic per-tick seed.
    Chance { p: f32 },
    /// The event occurred in biome `name`.
    InBiome { name: String },
}

/// An effect a trigger applies when it fires. The sim interprets each against the
/// triggering actor / location. This closed set is the "verbs" a designer can script;
/// new verbs are the rare case needing a code change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RuleAction {
    /// Grant XP to the actor.
    GrantXp { amount: u64 },
    /// Give the actor `count` of an item.
    GrantItem { item: ItemId, count: u16 },
    /// Apply a status to the actor for `duration_s` seconds.
    ApplyStatus { status: StatusId, duration_s: f32 },
    /// Spawn `count` of a mob near the event location.
    SpawnMob { mob: MobId, count: u8 },
    /// Roll a loot table and scatter the drops at the event location.
    SpawnLoot { table: LootTableId },
    /// Broadcast a message to players (toast / kill feed / world event banner).
    Broadcast { message: String },
    /// Grant the actor `n` skill points to spend in the tech tree.
    GrantSkillPoints { n: u32 },
    /// Adjust the actor's / team's score (modes that use scoring).
    ModifyScore { points: i32 },
}

/// A complete event->action rule: the designer's unit of scripting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDef {
    pub id: TriggerId,
    pub name: String,
    /// The event that may fire this trigger.
    pub on: GameTrigger,
    /// All must pass for the actions to run.
    pub conditions: Vec<TriggerCondition>,
    /// What happens when it fires, in order.
    pub actions: Vec<RuleAction>,
    /// If true, fire at most once per player (the sim tracks per-player firing).
    pub once_per_player: bool,
}
