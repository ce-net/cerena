//! Server-side snapshot encoding.
//!
//! [`SnapshotEncoder`] turns the authority's full entity set into a per-client,
//! AOI-scoped, delta-encoded [`Snapshot`]. It owns a [`BaselineStore`] so each
//! diff is computed against exactly what the target client has acknowledged. The
//! authority calls [`SnapshotEncoder::encode`] once per client per snapshot tick
//! and [`SnapshotEncoder::ack`] whenever a client's `InputBatch.ack_tick` arrives.

use std::collections::{HashMap, HashSet};

use arena_protocol::{
    EntityId, NodeId, Tick,
    entity::{EntityDelta, EntityState},
    snapshot::{GameEvent, LocalPlayerState, Snapshot},
};
use glam::Vec3;

use crate::baseline::BaselineStore;

/// An event is forwarded to a client if any entity it references is in that
/// client's AOI, or (for purely positional events) if it happens within this many
/// metres of one of the client's AOI entities. Keeps VFX/SFX local without leaking
/// out-of-view activity.
pub const EVENT_AOI_RADIUS_M: f32 = 96.0;

/// Encodes authoritative world state into per-client delta snapshots.
#[derive(Debug, Default)]
pub struct SnapshotEncoder {
    baselines: BaselineStore,
}

impl SnapshotEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `client` acknowledged receiving the snapshot for `tick`. Drives
    /// baseline selection on the next [`SnapshotEncoder::encode`].
    pub fn ack(&mut self, client: &NodeId, tick: Tick) {
        self.baselines.ack(client, tick);
    }

    /// Forget a client that has disconnected or been handed off.
    pub fn forget(&mut self, client: &NodeId) {
        self.baselines.forget(client);
    }

    /// Read-only access to the baseline store (diagnostics / tests).
    pub fn baselines(&self) -> &BaselineStore {
        &self.baselines
    }

    /// Build the snapshot for one client at `tick`.
    ///
    /// - `all_entities` is the authority's full simulated entity set this tick.
    /// - `aoi_ids` is the set of entity ids visible to this client (computed by the
    ///   caller from zone geometry; see `arena_protocol::world`).
    /// - `local_entity` is the client's own entity id.
    /// - `last_input_seq` is the highest input seq the sim has applied for them.
    /// - `ammo` is `(in_mag, reserve)`; `respawn_at` is the next respawn tick.
    /// - `events` are this interval's events, already coarsely filtered by the
    ///   caller; we additionally drop any that fall outside the client's AOI.
    ///
    /// The client's own entity is carried in [`Snapshot::local`], never in the
    /// entity delta list, so the client always has crisp authoritative truth for
    /// itself even at the AOI edge.
    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &mut self,
        client: &NodeId,
        tick: Tick,
        server_time_ms: u64,
        all_entities: &HashMap<EntityId, EntityState>,
        aoi_ids: &HashSet<EntityId>,
        local_entity: EntityId,
        last_input_seq: u32,
        ammo: (u16, u16),
        respawn_at: Tick,
        events: &[GameEvent],
    ) -> Snapshot {
        // The full AOI entity set we are about to send. Stored as the next baseline
        // so the following snapshot can diff against it. Includes the local entity
        // so it matches the client's reconstructed `WorldView` exactly.
        let mut sent: HashMap<EntityId, EntityState> = HashMap::with_capacity(aoi_ids.len());
        for id in aoi_ids {
            if let Some(state) = all_entities.get(id) {
                sent.insert(*id, state.clone());
            }
        }
        // Ensure the local entity is present even if the caller's AOI set omits it.
        if let Some(local_state) = all_entities.get(&local_entity) {
            sent.insert(local_entity, local_state.clone());
        }

        let mut entities: Vec<EntityDelta> = Vec::new();
        let mut despawns: Vec<EntityId> = Vec::new();
        let baseline_tick;

        // Borrow the baseline immutably only inside this block; it must be released
        // before we `record` the new one (mutable borrow of the same store).
        {
            let empty: HashMap<EntityId, EntityState> = HashMap::new();
            // Diff against the acked snapshot if we still hold it; else keyframe.
            let (btick, baseline): (Tick, &HashMap<EntityId, EntityState>) =
                match self.baselines.latest_acked(client) {
                    Some(acked) => match self.baselines.get(client, acked) {
                        Some(set) => (acked, set),
                        None => (0, &empty), // acked snapshot aged out of the ring
                    },
                    None => (0, &empty), // never acked anything → keyframe
                };
            baseline_tick = btick;

            // Per-AOI-entity deltas. The local entity is excluded; it rides in
            // `local`. Empty deltas (no change since baseline) are skipped.
            for id in aoi_ids {
                if *id == local_entity {
                    continue;
                }
                let Some(new) = all_entities.get(id) else {
                    continue; // in AOI set but not actually simulated → handled as despawn below
                };
                let delta = EntityDelta::diff(baseline.get(id), new);
                if !delta.is_empty() {
                    entities.push(delta);
                }
            }

            // Despawns: anything in the baseline (so the client still renders it)
            // that has since left the AOI or stopped existing.
            for id in baseline.keys() {
                if *id == local_entity {
                    continue;
                }
                let gone = !aoi_ids.contains(id) || !all_entities.contains_key(id);
                if gone {
                    despawns.push(*id);
                }
            }
        }

        // Record the new baseline now that the immutable borrow is gone.
        self.baselines.record(client, tick, sent);

        // The local player's authoritative state + reconciliation cursor.
        let local_state = all_entities
            .get(&local_entity)
            .cloned()
            .unwrap_or_else(|| placeholder_local(local_entity));
        let local = LocalPlayerState {
            entity: local_entity,
            state: local_state,
            last_input_seq,
            ammo_in_mag: ammo.0,
            ammo_reserve: ammo.1,
            respawn_at_tick: respawn_at,
        };

        // AOI-filter events: keep those that touch a visible entity or land near
        // one. `all_entities` gives us positions for the proximity test.
        let filtered_events: Vec<GameEvent> = events
            .iter()
            .filter(|e| event_in_aoi(e, aoi_ids, all_entities))
            .cloned()
            .collect();

        Snapshot {
            tick,
            baseline_tick,
            server_time_ms,
            local,
            entities,
            despawns,
            events: filtered_events,
        }
    }
}

/// A safe stand-in `LocalPlayerState.state` for the rare tick where the local
/// entity is momentarily absent from the sim (e.g. mid-respawn). Marked dead at
/// the origin so the client renders nothing controversial until the next spawn.
fn placeholder_local(id: EntityId) -> EntityState {
    use arena_protocol::{
        entity::{EntityFlags, EntityKind},
        world::Team,
    };
    let mut flags = EntityFlags::default();
    flags.set(EntityFlags::DEAD, true);
    EntityState {
        id,
        kind: EntityKind::Player,
        pos: Vec3::ZERO,
        vel: Vec3::ZERO,
        yaw: 0.0,
        pitch: 0.0,
        flags,
        team: Team::None,
        health: 0,
        armor: 0,
        weapon: 0,
        owner: String::new(),
    }
}

/// True if an event is relevant to a client's AOI: it references a visible entity,
/// or it carries a world position close to one of the client's visible entities.
fn event_in_aoi(
    event: &GameEvent,
    aoi_ids: &HashSet<EntityId>,
    all_entities: &HashMap<EntityId, EntityState>,
) -> bool {
    let touches = |id: &EntityId| aoi_ids.contains(id);
    let near = |p: Vec3| {
        let r2 = EVENT_AOI_RADIUS_M * EVENT_AOI_RADIUS_M;
        aoi_ids.iter().any(|id| {
            all_entities
                .get(id)
                .map(|e| e.pos.distance_squared(p) <= r2)
                .unwrap_or(false)
        })
    };
    match event {
        GameEvent::Shot { shooter, origin, .. } => touches(shooter) || near(*origin),
        GameEvent::Hit { attacker, victim, point, .. } => {
            touches(attacker) || touches(victim) || near(*point)
        }
        GameEvent::Death { victim, killer, .. } => touches(victim) || touches(killer),
        GameEvent::Spawn { entity, pos, .. } => touches(entity) || near(*pos),
        GameEvent::Explosion { center, .. } => near(*center),
        GameEvent::PickupTaken { pickup, by } => touches(pickup) || touches(by),
        // Chat is scoped by the caller (zone/team); always forward what we are given.
        GameEvent::Chat { .. } => true,
        // Feedback events: relevant when they touch a visible entity or land near one.
        GameEvent::Melee { attacker, victim, origin, .. } => {
            touches(attacker) || victim.map(|v| touches(&v)).unwrap_or(false) || near(*origin)
        }
        GameEvent::Knockback { entity, .. } => touches(entity),
        GameEvent::Buff { entity, .. } => touches(entity),
        GameEvent::Heal { target, .. } => touches(target),
        GameEvent::Shake { center, .. } => near(*center),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::{
        entity::{EntityFlags, EntityKind},
        snapshot::WorldView,
        world::Team,
    };

    fn player(id: EntityId, x: f32) -> EntityState {
        EntityState {
            id,
            kind: EntityKind::Player,
            pos: Vec3::new(x, 0.0, 0.0),
            vel: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            flags: EntityFlags::default(),
            team: Team::None,
            health: 100,
            armor: 0,
            weapon: 0,
            owner: format!("node{id}"),
        }
    }

    /// Encode a keyframe, apply it to a fresh client `WorldView`, then encode a
    /// delta after motion and apply that — the view must equal the authority.
    #[test]
    fn delta_roundtrip_reproduces_entities() {
        let mut enc = SnapshotEncoder::new();
        let client: NodeId = "viewer".into();
        let local: EntityId = 1;

        let mut world: HashMap<EntityId, EntityState> = HashMap::new();
        world.insert(1, player(1, 0.0));
        world.insert(2, player(2, 5.0));
        world.insert(3, player(3, 9.0));
        let aoi: HashSet<EntityId> = [1, 2, 3].into_iter().collect();

        // First snapshot is a keyframe (no ack yet).
        let snap1 = enc.encode(&client, 100, 1000, &world, &aoi, local, 0, (30, 90), 0, &[]);
        assert!(snap1.is_keyframe());
        let mut view = WorldView::default();
        view.apply(&snap1);
        for id in [1, 2, 3] {
            assert_eq!(view.entities.get(&id), world.get(&id));
        }

        // Client acks tick 100; next encode diffs against it.
        enc.ack(&client, 100);

        // Move entity 2; everything else unchanged.
        world.get_mut(&2).unwrap().pos = Vec3::new(7.5, 0.0, 0.0);
        let snap2 = enc.encode(&client, 110, 1050, &world, &aoi, local, 4, (29, 90), 0, &[]);
        assert!(!snap2.is_keyframe());
        assert_eq!(snap2.baseline_tick, 100);
        // Only the moved remote entity should carry a delta (local rides in `local`).
        assert_eq!(snap2.entities.len(), 1);
        assert_eq!(snap2.entities[0].id, 2);

        view.apply(&snap2);
        for id in [1, 2, 3] {
            assert_eq!(view.entities.get(&id), world.get(&id));
        }
    }

    /// An entity leaving the AOI becomes a despawn against the acked baseline.
    #[test]
    fn leaving_aoi_despawns() {
        let mut enc = SnapshotEncoder::new();
        let client: NodeId = "viewer".into();
        let local: EntityId = 1;

        let mut world: HashMap<EntityId, EntityState> = HashMap::new();
        world.insert(1, player(1, 0.0));
        world.insert(2, player(2, 5.0));
        let aoi_full: HashSet<EntityId> = [1, 2].into_iter().collect();

        let snap1 = enc.encode(&client, 1, 0, &world, &aoi_full, local, 0, (0, 0), 0, &[]);
        let mut view = WorldView::default();
        view.apply(&snap1);
        enc.ack(&client, 1);

        // Entity 2 leaves the AOI.
        let aoi_small: HashSet<EntityId> = [1].into_iter().collect();
        let snap2 = enc.encode(&client, 2, 0, &world, &aoi_small, local, 0, (0, 0), 0, &[]);
        assert!(snap2.despawns.contains(&2));
        view.apply(&snap2);
        assert!(!view.entities.contains_key(&2));
    }
}
