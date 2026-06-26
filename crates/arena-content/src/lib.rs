//! # arena-content
//!
//! The entire **game-design surface** of Cerena, expressed as **data** so it can
//! be **hot-reloaded into a live 10,000-player match**. Code (in `arena-sim` and
//! `arena-client`) is a fixed *interpreter*; everything a designer tweaks day-to-day
//! — spells, items, the tech tree, abilities, movement modes, materials, shaders,
//! world-gen, mobs, missions — lives here as serializable definitions.
//!
//! ## The hot-reload contract (see [`hotreload`] and [`registry`])
//!
//! 1. A [`pack::ContentPack`] is a versioned, content-addressed bundle of every
//!    definition. Its identity is the hash of its bytes ([`pack::ContentPack::hash`]),
//!    exactly like every other artifact in ce-net.
//! 2. The session coordinator publishes a [`hotreload::ContentVersion`] (a monotonic
//!    epoch + pack hash) on the control plane.
//! 3. Authorities and clients fetch the pack and **swap it at a tick boundary**
//!    ([`registry::ContentRegistry::stage`] + [`registry::ContentRegistry::apply_pending`]),
//!    so the simulation never tears.
//!
//! Live *state* references content **by stable id** (an inventory stores [`ids::ItemId`]s,
//! a cast names a [`ids::SpellId`]). Swapping a definition therefore changes behavior
//! without disturbing identity or progress — that is what makes "applied instantly
//! while people play" safe.
//!
//! This crate is pure data + pure logic: no tokio, no wgpu, no ce_rs. It compiles on
//! `wasm32` so the client shares the exact same definitions as the authority.

pub mod ability;
pub mod affix;
pub mod default_pack;
pub mod enchant;
pub mod expansion;
pub mod forge;
pub mod gamemode;
pub mod gear;
pub mod gem;
pub mod ids;
pub mod item;
pub mod itemset;
pub mod loot;
pub mod material;
pub mod mission;
pub mod mob;
pub mod movement;
pub mod pack;
pub mod registry;
pub mod hotreload;
pub mod spawn;
pub mod spell;
pub mod status;
pub mod tech;
pub mod triggers;
pub mod tuning;
pub mod worldgen;

pub use affix::{AffixDef, AffixKind};
pub use default_pack::{default_pack, starter_pack};
pub use enchant::{EnchantDef, RunewordDef};
pub use forge::ForgeConfig;
pub use gamemode::{GameModeDef, ScoringRule, TeamConfig, WinCondition};
pub use gem::GemDef;
pub use ids::*;
pub use item::{
    ElementMods, EquipSlot, ItemDef, ItemTrigger, ProcEffect, ProcWhen, Rarity, StatMods,
    UpgradeProfile,
};
pub use itemset::{SetBonus, SetDef};
pub use loot::{LootEntry, LootTableDef};
pub use pack::ContentPack;
pub use registry::ContentRegistry;
pub use spawn::{SpawnRuleDef, SpawnTrigger};
pub use triggers::{
    GameTrigger, GameTriggerKind, RuleAction, TriggerCondition, TriggerDef,
};
pub use tuning::TuningConfig;

/// Errors raised while loading or swapping content.
#[derive(Debug, thiserror::Error)]
pub enum ContentError {
    #[error("content decode failed: {0}")]
    Decode(String),
    #[error("content hash mismatch: expected {expected}, got {got}")]
    HashMismatch { expected: String, got: String },
    #[error("unknown {kind} id: {id}")]
    UnknownId { kind: &'static str, id: String },
    #[error("validation failed: {0}")]
    Invalid(String),
}
