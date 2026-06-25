//! Server→client world snapshots and the discrete events that ride alongside them.
//!
//! A snapshot is the authoritative truth for one tick, scoped to a single client's
//! area of interest. It is delta-encoded against a baseline the client has already
//! acknowledged, so steady-state bandwidth is proportional to *motion*, not to the
//! number of players in the world.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::{
    EntityId, NodeId, Tick,
    entity::{EntityDelta, EntityState},
    world::Team,
};

/// A discrete, one-shot thing that happened this tick: a shot, a hit, a death.
/// Events are not delta-encoded; they are reliable-ish (re-sent until acked for
/// the few that matter, like kills) and drive client VFX/SFX and the kill feed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GameEvent {
    /// A weapon was fired from `origin` along `dir` by `shooter`. Clients play
    /// the muzzle flash / tracer even for shots that hit nothing.
    Shot {
        shooter: EntityId,
        weapon: u8,
        origin: Vec3,
        dir: Vec3,
    },
    /// A confirmed hit. `damage` is post-armor, post-falloff. Drives hitmarkers.
    Hit {
        attacker: EntityId,
        victim: EntityId,
        damage: f32,
        headshot: bool,
        point: Vec3,
    },
    /// A player died. Drives the kill feed and respawn timer.
    Death {
        victim: EntityId,
        killer: EntityId,
        weapon: u8,
        victim_node: NodeId,
        killer_node: NodeId,
    },
    /// A player (re)spawned at `pos`.
    Spawn { entity: EntityId, pos: Vec3, team: Team },
    /// An explosion at `center` (rockets, grenades) for client VFX.
    Explosion { center: Vec3, radius: f32 },
    /// A pickup was taken.
    PickupTaken { pickup: EntityId, by: EntityId },
    /// Free-form chat / system line scoped to the AOI.
    Chat { from: NodeId, text: String },
}

/// The authoritative state for the local player, sent every snapshot so the
/// client can reconcile its prediction. Kept separate from the entity list so
/// the client always has its own crisp truth even at the AOI edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPlayerState {
    pub entity: EntityId,
    pub state: EntityState,
    /// The highest input `seq` the server has applied for this player. The client
    /// discards acked inputs and replays the rest on top of `state`.
    pub last_input_seq: u32,
    pub ammo_in_mag: u16,
    pub ammo_reserve: u16,
    /// Server tick when the player may next respawn (0 = alive / now).
    pub respawn_at_tick: Tick,
}

/// A delta-encoded world snapshot for one client's AOI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    /// The tick this snapshot represents (the authoritative "now").
    pub tick: Tick,
    /// The tick this snapshot is delta-encoded against. If the client does not
    /// hold that baseline it requests a full snapshot (baseline == 0).
    pub baseline_tick: Tick,
    /// Server wall-clock estimate in milliseconds, for clock sync / interpolation.
    pub server_time_ms: u64,
    /// The local player's authoritative state + reconciliation cursor.
    pub local: LocalPlayerState,
    /// Per-entity deltas for everything in the client's AOI.
    pub entities: Vec<EntityDelta>,
    /// Entities that left the AOI / were destroyed since the baseline.
    pub despawns: Vec<EntityId>,
    /// Events that occurred since the baseline.
    pub events: Vec<GameEvent>,
}

impl Snapshot {
    /// A full (keyframe) snapshot has `baseline_tick == 0`; the client can
    /// reconstruct world state with no prior knowledge.
    pub fn is_keyframe(&self) -> bool {
        self.baseline_tick == 0
    }
}

/// Client-side decoded world: the materialized result of applying a chain of
/// snapshots. Lives in `arena-net`, but the storage type is shared so the client
/// renderer and the prediction layer agree on shape.
#[derive(Debug, Clone, Default)]
pub struct WorldView {
    pub tick: Tick,
    pub entities: std::collections::HashMap<EntityId, EntityState>,
}

impl WorldView {
    /// Apply a snapshot's deltas onto this view, mutating it to the snapshot tick.
    /// Returns the list of events for the caller to drain into VFX/SFX.
    pub fn apply(&mut self, snap: &Snapshot) -> Vec<GameEvent> {
        for d in &snap.entities {
            let base = self.entities.get(&d.id);
            if let Some(next) = d.apply(base) {
                self.entities.insert(d.id, next);
            }
        }
        for id in &snap.despawns {
            self.entities.remove(id);
        }
        self.entities.insert(snap.local.entity, snap.local.state.clone());
        self.tick = snap.tick;
        snap.events.clone()
    }
}
