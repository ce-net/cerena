//! # arena-sim
//!
//! The deterministic, fixed-timestep, **server-authoritative** simulation for
//! Cerena — a first-person procedural mage RPG built to host thousands of players
//! per zone. This crate is the single source of truth for "what actually happened":
//! movement and parkour, the spell VM, status effects, RPG progression, inventory
//! and loot, deaths and respawns.
//!
//! ## Hard constraints
//!
//! - **No networking, no tokio, no I/O.** Pure simulation. The caller owns the clock
//!   and feeds inputs; we advance one [`TICK_DT`] step at a time via [`World::tick`].
//! - **Compiles on `wasm32`.** The browser client links this same crate to run
//!   client-side prediction, so predicted and authoritative worlds run identical code.
//! - **Deterministic given the same inputs.** No wall-clock reads, no RNG. Where we
//!   need "randomness" (the `Chance` spell op, spread) it is hashed from the tick +
//!   caster + a per-cast salt so client and server agree.
//!
//! ## Data-driven by `arena-content`
//!
//! Spells, items, abilities, statuses, movement modes, mobs and the tech tree are
//! **hot-reloadable data** in `arena-content`. This crate is the fixed *interpreter*:
//! the spell VM ([`magic`]) walks an [`arena_content::spell::EffectOp`] graph; the
//! movement system reads [`arena_content::movement::MovementKind`]s; progression reads
//! [`arena_content::tech`]. A new spell or item is a content swap, not a code deploy.
//! [`World::stage_content`] queues a pack that is applied atomically at the next tick
//! boundary inside [`World::tick`].
//!
//! ## Modules
//!
//! - [`map`]       — static collision geometry + spawn points.
//! - [`collision`] — capsule-vs-AABB sliding, raycasts, ray-vs-player tests.
//! - [`movement`]  — base locomotion + the parkour movement-mode kit.
//! - [`combat`]    — shared damage/faction rules + deterministic hashing.
//! - [`magic`]     — the [`arena_content::spell::EffectOp`] interpreter (the spell VM).
//! - [`rpg`]       — attributes, level/XP, mana/stamina, tech-derived stats.
//! - [`inventory`] — items, equipment, grants, crafting, loot.
//! - [`world`]     — the [`World`] aggregate and the per-tick pipeline.
//!
//! [`TICK_DT`]: arena_protocol::TICK_DT

pub mod collision;
pub mod combat;
pub mod inventory;
pub mod magic;
pub mod map;
pub mod movement;
pub mod rpg;
pub mod world;

// The types other crates reach for most often, surfaced at the crate root.
pub use inventory::Inventory;
pub use magic::CastContext;
pub use map::MapDef;
pub use rpg::{Attributes, Derived, RpgState};
pub use world::{CheatCounters, PlayerCheckpoint, StatusInstance, TickReport, World};
