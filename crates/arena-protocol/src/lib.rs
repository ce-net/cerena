//! # arena-protocol
//!
//! The shared contract for Cerena — a thousands-of-players FPS that runs its
//! authoritative simulation *on the CE mesh itself*. Player nodes are trusted by
//! identity (each CE node id is an Ed25519 key), so any sufficiently-staked node
//! can host a slice of the world.
//!
//! This crate is intentionally dependency-light: it is the one crate every other
//! crate (sim, net, server, karma, mesh, client) links against, so it must compile
//! on `wasm32-unknown-unknown` as cleanly as on a server. No tokio, no ce_rs here.
//!
//! ## Layering
//!
//! - [`world`]   — coordinate system, zones, area-of-interest geometry.
//! - [`input`]   — client command frames (the only thing a client may assert).
//! - [`entity`]  — replicated entity state and the delta model.
//! - [`snapshot`]— server→client world state + events.
//! - [`message`] — the full wire envelope (client↔server, server↔server).
//! - [`weapon`]  — weapon definitions (shared by sim + client + anti-cheat).
//! - [`karma`]   — reports, telemetry, and karma deltas.
//! - [`auth`]    — session tickets binding a player to a CE identity.
//!
//! Wire format is `bincode` (deterministic, compact). Every top-level message is
//! versioned via [`PROTOCOL_VERSION`]; a mismatch is a hard disconnect.

pub mod auth;
pub mod entity;
pub mod input;
pub mod karma;
pub mod message;
pub mod snapshot;
pub mod weapon;
pub mod world;

pub use glam::{Quat, Vec2, Vec3};

/// Bumped on any breaking wire change. Client and server compare on handshake.
pub const PROTOCOL_VERSION: u32 = 1;

/// Simulation runs at a fixed timestep. 64 Hz is the Counter-Strike / Valorant
/// competitive standard: tight enough for crisp hit-reg, cheap enough that a
/// commodity node can simulate a full zone.
pub const TICK_HZ: u32 = 64;

/// Seconds per simulation tick. `1.0 / TICK_HZ`.
pub const TICK_DT: f32 = 1.0 / TICK_HZ as f32;

/// Authoritative snapshots are sent at a lower rate than the sim runs; clients
/// interpolate between them. 20 Hz keeps bandwidth sane for thousands of players
/// while interpolation hides the gap.
pub const SNAPSHOT_HZ: u32 = 20;

/// How many sim ticks between snapshot sends. `TICK_HZ / SNAPSHOT_HZ`.
pub const TICKS_PER_SNAPSHOT: u32 = TICK_HZ / SNAPSHOT_HZ;

/// Clients send their input frame every tick but we coalesce/ack in windows.
/// Inputs older than this many ticks behind the server clock are dropped (a
/// client that far behind is lagging or replaying).
pub const MAX_INPUT_LAG_TICKS: u32 = TICK_HZ; // 1 second

/// Lag-compensation rewind budget. The server may rewind hitscan targets up to
/// this far to honor a client's view of the world. Anything beyond this is
/// rejected — it is the upper bound on tolerated latency for fair hit-reg, and
/// also the anti-cheat ceiling for "impossible" backwards reconciliation.
pub const MAX_REWIND_MS: u32 = 220;

/// A monotonic simulation tick. Wraps after ~2 years at 64 Hz; sessions never
/// last that long, and zone handoff resets the baseline.
pub type Tick = u32;

/// An entity within a single zone simulation. Unique only per-zone; the globally
/// unique handle is [`entity::GlobalId`].
pub type EntityId = u32;

/// A CE node id (Ed25519 public key, hex-encoded). This *is* the player identity —
/// there is no separate account system. Trust, karma, and bans all key off this.
pub type NodeId = String;

/// Errors surfaced when decoding a wire message.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("protocol version mismatch: ours={ours} theirs={theirs}")]
    Version { ours: u32, theirs: u32 },
    #[error("bincode decode failed: {0}")]
    Decode(String),
    #[error("bincode encode failed: {0}")]
    Encode(String),
    #[error("message too large: {0} bytes (max {max})", max = MAX_MESSAGE_BYTES)]
    TooLarge(usize),
}

/// Hard cap on a single encoded message. Mesh transport will fragment above this;
/// the server rejects oversized client messages as a cheap DoS guard.
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Encode any serializable wire type with bincode.
pub fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    let bytes = bincode::serialize(value).map_err(|e| WireError::Encode(e.to_string()))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(WireError::TooLarge(bytes.len()));
    }
    Ok(bytes)
}

/// Decode any deserializable wire type with bincode.
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, WireError> {
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(WireError::TooLarge(bytes.len()));
    }
    bincode::deserialize(bytes).map_err(|e| WireError::Decode(e.to_string()))
}
