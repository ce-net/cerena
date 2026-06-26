//! # arena-server — the Cerena authority orchestrator
//!
//! This crate is the **backend centerpiece** of Cerena: the process a player's node
//! runs to become part of the distributed, authoritative simulation of a
//! 10,000-player first-person procedural mage RPG. There is no central game server —
//! the players' own CE nodes *are* the servers, and this crate is what turns a node
//! into one.
//!
//! ## What a node runs
//!
//! Every node runs exactly one [`ArenaServer`] for a session. That server:
//!
//! - **Owns N zone authorities.** The world is a regular grid of zones
//!   ([`arena_protocol::world::ZoneId`]); each zone is simulated by exactly one node at
//!   a time. Which node owns which zone is decided with **stake-weighted rendezvous
//!   (HRW) hashing** ([`arena_mesh::assign_authority`]) — a directory-free, deterministic
//!   function every node computes identically. A node claims the zones it should own and
//!   retires the ones it should not, broadcasting [`AuthorityClaim`](arena_protocol::message::AuthorityMsg::AuthorityClaim)s
//!   with a monotonic epoch so conflicts resolve cleanly. See [`manager`].
//!
//! - **May also be the coordinator.** One node per session runs [`coordinator::Coordinator`]:
//!   it admits players (verifying their [`SessionTicket`](arena_protocol::auth::SessionTicket)
//!   and karma band), assigns them a spawn zone + its authority, and publishes
//!   hot-reloadable content versions. See [`coordinator`].
//!
//! ## How 10,000 players stay cheap
//!
//! The scale levers are all here:
//!
//! 1. **Per-zone authority** spreads the simulation cost across the whole fleet — no node
//!    simulates more than its share of zones.
//! 2. **Area-of-interest (AOI) snapshots** ([`zone::ZoneSim`]) mean a client receives state
//!    only for entities near it, delta-encoded against an acked baseline
//!    ([`arena_net::SnapshotEncoder`]). Steady-state bandwidth tracks *motion in view*, not
//!    the global player count.
//! 3. **Seamless hand-off** migrates a player (and its authoritative state) to the
//!    neighbouring authority as it crosses a zone boundary, so the world is continuous even
//!    though it is sharded across many nodes.
//!
//! ## How a sharded sim stays honest
//!
//! A staked node can win a zone and then *lie*. The defense is redundant re-simulation:
//! authorities periodically publish [`VerifyTick`](arena_protocol::message::AuthorityMsg::VerifyTick)s
//! and a sample of peers shadow-replay the same inputs and vote
//! ([`arena_karma::CrossValidator`]). A disputed authority is reassigned and, on sustained
//! disputes, slashed. Client cheating is caught separately by statistical telemetry +
//! karma-weighted reports ([`anticheat`]).
//!
//! ## The actor architecture (read this before editing)
//!
//! [`arena_sim::World`] is the mutable heart of a zone and is decidedly **not** something we
//! want to wrap in a lock on the hot path. So the design is a single-consumer actor:
//!
//! - **Mesh tasks are producers.** The [`handler::ArenaHandler`] (reliable RPC) and the
//!   fire-and-forget envelope/content/discovery loops do no game logic. They decode, then
//!   enqueue an [`handler::Inbound`] message onto an `mpsc` channel (RPCs carry a `oneshot`
//!   reply channel).
//! - **The fixed-tick loop is the one consumer.** It exclusively owns every [`arena_sim::World`]
//!   (inside [`manager::ZoneManager`]), the [`coordinator::Coordinator`], and the
//!   [`anticheat::AntiCheat`]. Each tick it drains the channel, applies inputs, steps every
//!   owned zone, and emits snapshots / verify-ticks / karma. Long async operations
//!   (cross-node hand-off, remote join forwarding) are handed to detached tasks so they never
//!   stall the clock.
//!
//! This keeps all `World` access single-threaded and lock-free while the mesh I/O runs
//! concurrently around it.
//!
//! See [`server::ArenaServer`] for the wiring and [`config::ServerConfig`] for the knobs.

pub mod anticheat;
pub mod config;
pub mod coordinator;
pub mod handler;
pub mod manager;
pub mod server;
pub mod zone;

pub use config::ServerConfig;
pub use server::ArenaServer;
