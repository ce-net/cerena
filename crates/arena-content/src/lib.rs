//! # arena-content
//!
//! The entire **game-design surface** of CE Arena, expressed as **data** so it can
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
pub mod ids;
pub mod item;
pub mod material;
pub mod mission;
pub mod mob;
pub mod movement;
pub mod pack;
pub mod registry;
pub mod hotreload;
pub mod spell;
pub mod status;
pub mod tech;
pub mod worldgen;

pub use ids::*;
pub use pack::ContentPack;
pub use registry::ContentRegistry;

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
