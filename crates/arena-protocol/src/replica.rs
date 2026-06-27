//! Replicated-authority wire types — the contract for the model where **every
//! player (and every donor node) near a zone runs the full deterministic sim for
//! it**, and the replicas reconcile by a periodic majority state-hash. This is the
//! same shape spacegame uses (`replica.rs` + `replication.rs`): playing *is*
//! hosting, with no trusted server.
//!
//! Two things cross the mesh in this model, both authored by the authenticated
//! sender (so neither carries a node field on the wire — the transport already
//! tells the receiver who sent it):
//!
//! - [`TaggedInput`] on a zone's `/in` topic: "apply `input` at simulation `tick`".
//!   Every replica schedules it at the SAME tick, so honest replicas converge
//!   regardless of packet arrival order (see `arena-replica::Replica`).
//! - [`StateProof`] on a zone's `/proof` topic: "at `tick` my deterministic world
//!   hashed to `hash`". Comparing replicas' proofs (`arena-replica::quorum::agree`)
//!   is how the mesh tells an honest sim from a desynced or cheating one; the odd
//!   one out merges to the agreed snapshot.
//!
//! `bincode`-clean and dependency-light like the rest of `arena-protocol`.

use serde::{Deserialize, Serialize};

use crate::input::InputFrame;
use crate::world::{Team, ZoneId};
use crate::Tick;

/// One participant's intent in the replicated model. Mirrors the meaningful
/// [`crate::message::ClientMsg`] variants, but as a tick-taggable unit every
/// replica applies deterministically. The author is the authenticated mesh
/// sender, so identity is never carried in the payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReplicaInput {
    /// Spawn / (re)admit this player into the zone sim. Idempotent on the author's
    /// node id: a second `Join` for an already-present player is ignored.
    Join {
        team_pref: Option<Team>,
        /// Cosmetic display name (identity is the node id). Length-capped by the sim.
        name: String,
    },
    /// One tick of player intent — the only authoritative client assertion.
    Input(InputFrame),
    /// Graceful departure; the sim despawns the player's body.
    Leave,
    /// The player crossed into `to`; the replicas hosting `to` adopt them.
    ZoneSwitch { to: ZoneId },
}

/// A tick-tagged input: apply `input` at simulation `tick` on **every** replica of
/// the zone. `seq` orders multiple inputs from the same author within one tick,
/// deterministically and identically everywhere. The author is the authenticated
/// mesh sender.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaggedInput {
    pub tick: Tick,
    pub seq: u32,
    pub input: ReplicaInput,
}

/// One replica's claim about the authoritative state of a zone at a tick: the
/// deterministic [`arena_sim::World::state_hash`](../../arena_sim/struct.World.html)
/// it computed. Replicas publish these periodically; the quorum compares them.
/// The author (the publishing node) is the authenticated mesh sender, so it is not
/// repeated in the payload — `arena-replica::quorum::agree` pairs each proof with
/// its sender at ingest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateProof {
    pub zone: ZoneId,
    pub tick: Tick,
    pub hash: [u8; 32],
}
