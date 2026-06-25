//! # arena-sim
//!
//! The deterministic, fixed-timestep, **server-authoritative** simulation for CE
//! Arena. This crate is the single source of truth for "what actually happened":
//! movement, collision, ballistics, damage, deaths and respawns. The networking
//! layer (`arena-net`), the zone authority (`arena-server`) and the anti-cheat
//! aggregator (`arena-karma`) all drive *this* crate and trust its output.
//!
//! ## Hard constraints
//!
//! - **No networking, no tokio, no I/O.** This is pure simulation. The caller owns
//!   the clock and feeds inputs; we advance the world one [`TICK_DT`] step at a time.
//! - **Compiles on `wasm32`.** The browser client links this same crate to run
//!   client-side prediction, so the predicted world and the authoritative world are
//!   produced by *identical* code. Anything platform-specific is forbidden here.
//! - **Deterministic given the same inputs.** No wall-clock reads, no RNG. Where we
//!   need "randomness" (shotgun spread) we derive it from a hash of the tick and the
//!   shooter so client and server agree. (Note: glam f32 ops are not bit-identical
//!   across architectures — a single authority simulates a zone, and
//!   [`World::state_hash`] rounds before hashing so cross-validation tolerates jitter.)
//!
//! ## Module layout
//!
//! - [`map`]       — static collision geometry + spawn points ([`map::MapDef`]).
//! - [`collision`] — capsule-vs-AABB sliding, world raycasts, ray-vs-player tests.
//! - [`movement`]  — Quake/Source-style player locomotion from button intent.
//! - [`combat`]    — weapon firing, lag-compensated hit resolution, damage, splash.
//! - [`world`]     — the [`world::World`] aggregate that ties it all together and
//!   exposes [`world::World::tick`], the one entry point an authority calls per tick.
//!
//! [`TICK_DT`]: arena_protocol::TICK_DT

pub mod collision;
pub mod combat;
pub mod map;
pub mod movement;
pub mod world;

// The types other crates reach for most often, surfaced at the crate root.
pub use map::MapDef;
pub use world::{CheatCounters, TickReport, World};
