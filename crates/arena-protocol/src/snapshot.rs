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

/// The flavour of a melee swing, so the client picks the right weapon arc, sound,
/// and screen kick. The combo escalates Slash -> Thrust -> Spin as a player chains
/// strikes inside the combo window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum MeleeKind {
    /// Horizontal sweep (combo step 0).
    Slash = 0,
    /// Forward lunge (combo step 1).
    Thrust = 1,
    /// Spinning finisher with wide knockback (combo step 2+).
    Spin = 2,
}

/// A discrete, one-shot thing that happened this tick: a shot, a hit, a death.
/// Events are not delta-encoded; they are reliable-ish (re-sent until acked for
/// the few that matter, like kills) and drive client VFX/SFX and the kill feed.
///
/// The later variants ([`GameEvent::Melee`] onward) are the **feedback channel**:
/// they carry the semantic detail the client needs to make combat *feel* — a melee
/// arc, a knockback shove, a buff bloom, a heal tick, an explicit camera-shake hint —
/// without the client having to re-derive intent from raw state deltas.
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

    // ---- feedback channel ----
    /// A melee weapon swing. `victim` is `Some` only when the arc connected; `hit`
    /// mirrors that for callers that do not care who. Drives the first-person weapon
    /// arc, the swing whoosh, the connect "thunk", and a directional screen kick.
    Melee {
        attacker: EntityId,
        victim: Option<EntityId>,
        origin: Vec3,
        dir: Vec3,
        kind: MeleeKind,
        /// Post-mitigation damage dealt (0 on a whiff).
        damage: f32,
    },
    /// A force shoved `entity` by `impulse` (m/s applied this tick). Knockback,
    /// gravity pulls, explosion shoves. The client turns this into a camera lurch on
    /// the local player and a stagger lean / dust kick on remotes.
    Knockback { entity: EntityId, impulse: Vec3 },
    /// A status effect landed on `entity`. `beneficial` tints the bloom (gold buff vs
    /// sickly debuff) and decides whether the local player sees a buff flare or a
    /// damage-y vignette pulse.
    Buff { entity: EntityId, beneficial: bool },
    /// `target` was healed for `amount`. Drives floating green motes + a soft restore
    /// flash when it is the local player.
    Heal { target: EntityId, amount: f32 },
    /// An explicit camera-shake hint centred at `center` with normalised `trauma`
    /// (0..1). Big set-pieces (meteor, singularity collapse) emit this so the shake is
    /// authored rather than purely derived; the client scales it by proximity.
    Shake { center: Vec3, trauma: f32 },
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
