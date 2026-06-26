//! The client-side world: glue that ties decoding, interpolation, prediction, and
//! clock sync into one object a renderer can drive.
//!
//! [`ClientWorld`] is what `arena-client` holds. Each authoritative snapshot goes
//! in via [`ClientWorld::apply_snapshot`]; each rendered frame pulls a list of
//! entity states out via [`ClientWorld::render_entities`]; each input goes in via
//! [`ClientWorld::push_input`] and out (to the mesh) via
//! [`ClientWorld::make_input_batch`]. Remote players are interpolated in the past;
//! the local player is predicted in the present. The renderer never has to know
//! the difference — it just gets a coherent set of [`EntityState`]s for `now`.

use std::collections::HashMap;

use arena_protocol::{
    EntityId, Tick,
    entity::EntityState,
    input::{InputBatch, InputFrame},
    snapshot::{GameEvent, Snapshot, WorldView},
};

use crate::{
    clock::ClockSync,
    interp::{INTERP_DELAY_TICKS, InterpolationBuffer},
    predict::{LocalSim, Predictor},
};

/// Everything the client needs to turn a snapshot stream into a renderable,
/// responsive world. Generic over the local replay sim (`arena-client` plugs in
/// [`crate::predict::SimReplay`] over `arena_sim::World`; tests use a stub).
pub struct ClientWorld<S: LocalSim> {
    local_id: EntityId,
    /// Authoritative materialized world (remote + local), latest snapshot tick.
    view: WorldView,
    /// Past-time buffer of remote entities for smooth interpolation.
    interp: InterpolationBuffer,
    /// Local-player prediction + reconciliation.
    predictor: Predictor<S>,
    /// Server-tick / RTT estimation.
    clock: ClockSync,
    /// Highest snapshot tick fully received — sent as the input-batch ack so the
    /// server can choose a delta baseline.
    ack_tick: Tick,
}

impl<S: LocalSim> ClientWorld<S> {
    /// Build a client world for `local_id`, driven by replay sim `sim`.
    pub fn new(local_id: EntityId, sim: S) -> Self {
        Self {
            local_id,
            view: WorldView::default(),
            interp: InterpolationBuffer::new(),
            predictor: Predictor::new(local_id, sim),
            clock: ClockSync::new(),
            ack_tick: 0,
        }
    }

    /// Apply an authoritative snapshot received at local time `now_ms`:
    ///
    /// 1. update clock sync from the snapshot timestamp,
    /// 2. materialize the snapshot into the authoritative [`WorldView`],
    /// 3. feed the *remote* entities into the interpolation buffer,
    /// 4. reconcile the local-player prediction against `snap.local`,
    /// 5. advance the ack high-water mark.
    ///
    /// Returns the snapshot's events (drained from the view) for the caller to turn
    /// into VFX/SFX and kill-feed entries.
    pub fn apply_snapshot(&mut self, snap: Snapshot, now_ms: u64) -> Vec<GameEvent> {
        self.clock.on_snapshot(snap.server_time_ms, now_ms);

        // Reconcile prediction first, while we still hold the snapshot, so the
        // local sim is corrected before anything reads it this frame.
        self.predictor.reconcile(&snap.local);

        // Materialize into the authoritative view (this also inserts the local
        // player's authoritative state) and drain events.
        let events = self.view.apply(&snap);

        // Buffer remote entities only — the local player is predicted, never
        // interpolated, so feeding it here would fight the predictor.
        let remote: HashMap<EntityId, EntityState> = self
            .view
            .entities
            .iter()
            .filter(|(id, _)| **id != self.local_id)
            .map(|(id, s)| (*id, s.clone()))
            .collect();
        self.interp.push(snap.tick, remote);

        if snap.tick > self.ack_tick {
            self.ack_tick = snap.tick;
        }
        events
    }

    /// Feed a `Pong` round-trip sample into clock sync (RTT estimation).
    pub fn on_pong(&mut self, sent_ms: u64, now_ms: u64) {
        self.clock.on_pong(sent_ms, now_ms);
    }

    /// Record and immediately apply a local input. The caller stamps `seq` and
    /// `client_tick` (the latter from [`ClientWorld::estimated_server_tick`]).
    pub fn push_input(&mut self, frame: InputFrame) {
        self.predictor.push_input(frame);
    }

    /// The set of entity states to render at local time `now_ms`: interpolated
    /// remote players (rendered ~[`INTERP_DELAY_TICKS`] in the past) plus the
    /// predicted local player (rendered in the present so input feels instant).
    pub fn render_entities(&mut self, now_ms: u64) -> Vec<EntityState> {
        // Remote: sample the interpolation buffer at the delayed render tick.
        let render_tick_f = self.clock.estimated_server_tick_f(now_ms) - INTERP_DELAY_TICKS;
        let remote = self.interp.sample(render_tick_f);

        // Local: decay the visual correction one frame, then take the prediction.
        self.predictor.relax_error();

        let mut out: Vec<EntityState> = Vec::with_capacity(remote.len() + 1);
        out.extend(remote.into_values());
        if let Some(local) = self.predictor.predicted_local_state() {
            out.push(local);
        }
        out
    }

    /// Build an outgoing input batch carrying the latest ack and all unacked input
    /// frames (re-sending them makes a single dropped packet harmless). `ack_tick`
    /// is supplied by the caller, normally [`ClientWorld::ack_tick`].
    pub fn make_input_batch(&self, ack_tick: Tick) -> InputBatch {
        InputBatch {
            ack_tick,
            frames: self.predictor.unacked_frames(),
        }
    }

    /// The highest snapshot tick fully received — pass to [`ClientWorld::make_input_batch`].
    pub fn ack_tick(&self) -> Tick {
        self.ack_tick
    }

    /// Estimated current server tick, for stamping `InputFrame.client_tick`.
    pub fn estimated_server_tick(&self, now_ms: u64) -> Tick {
        self.clock.estimated_server_tick(now_ms)
    }

    /// The smoothed round-trip time in milliseconds (HUD / netgraph).
    pub fn rtt_ms(&self) -> f32 {
        self.clock.rtt_ms()
    }

    pub fn local_id(&self) -> EntityId {
        self.local_id
    }

    /// Read-only access to the authoritative materialized world (HUD, scoreboard).
    pub fn view(&self) -> &WorldView {
        &self.view
    }

    /// Read-only access to the predictor (e.g. camera aim from the true predicted
    /// position via [`Predictor::authoritative_local_state`]).
    pub fn predictor(&self) -> &Predictor<S> {
        &self.predictor
    }

    /// Read-only access to the clock estimator.
    pub fn clock(&self) -> &ClockSync {
        &self.clock
    }

    /// Read-only access to the interpolation buffer (diagnostics / tests).
    pub fn interp(&self) -> &InterpolationBuffer {
        &self.interp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predict::tests::StubSim;
    use crate::server::SnapshotEncoder;
    use arena_protocol::{
        NodeId,
        entity::{EntityFlags, EntityKind, EntityState},
        input::{Buttons, InputFrame},
        world::Team,
    };
    use glam::Vec3;
    use std::collections::{HashMap, HashSet};

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
            owner: String::new(),
        }
    }

    fn fwd_input(seq: u32) -> InputFrame {
        let mut buttons = Buttons::default();
        buttons.set(Buttons::FORWARD, true);
        InputFrame {
            seq,
            client_tick: seq,
            buttons,
            yaw: 0.0,
            pitch: 0.0,
            weapon_slot: 0,
        }
    }

    /// A snapshot encoded by the server, applied by the client, reproduces the
    /// authoritative remote entity set in the client's `WorldView`.
    #[test]
    fn snapshot_roundtrip_into_client_view() {
        let mut enc = SnapshotEncoder::new();
        let client: NodeId = "me".into();
        let local: EntityId = 1;

        let mut world: HashMap<EntityId, EntityState> = HashMap::new();
        world.insert(1, player(1, 0.0));
        world.insert(2, player(2, 5.0));
        let aoi: HashSet<EntityId> = [1, 2].into_iter().collect();

        let snap = enc.encode(&client, 100, 5_000, &world, &aoi, local, 0, (30, 90), 0, &[]);

        let mut cw = ClientWorld::new(local, StubSim::new());
        cw.apply_snapshot(snap, 5_000);

        // Authoritative view reproduces both entities exactly.
        assert_eq!(cw.view().entities.get(&1), world.get(&1));
        assert_eq!(cw.view().entities.get(&2), world.get(&2));
        assert!(cw.clock().is_synced());
    }

    /// Pushing inputs then applying a snapshot whose `last_input_seq` acks some of
    /// them drops the acked inputs from the outgoing batch.
    #[test]
    fn applying_snapshot_drops_acked_inputs() {
        let local: EntityId = 1;
        let mut cw = ClientWorld::new(local, StubSim::new());
        cw.push_input(fwd_input(1));
        cw.push_input(fwd_input(2));
        cw.push_input(fwd_input(3));
        assert_eq!(cw.make_input_batch(0).frames.len(), 3);

        // Server confirms through seq 2.
        let snap = Snapshot {
            tick: 50,
            baseline_tick: 0,
            server_time_ms: 1_000,
            local: arena_protocol::snapshot::LocalPlayerState {
                entity: local,
                state: player(local, 2.0),
                last_input_seq: 2,
                ammo_in_mag: 30,
                ammo_reserve: 90,
                respawn_at_tick: 0,
            },
            entities: vec![],
            despawns: vec![],
            events: vec![],
        };
        cw.apply_snapshot(snap, 1_000);

        let batch = cw.make_input_batch(cw.ack_tick());
        assert_eq!(batch.ack_tick, 50);
        assert_eq!(batch.frames.len(), 1);
        assert_eq!(batch.frames[0].seq, 3);
    }

    /// Two remote snapshots straddling a render time interpolate to the midpoint.
    #[test]
    fn remote_interpolation_midpoint() {
        let local: EntityId = 1;
        let mut cw = ClientWorld::new(local, StubSim::new());

        // Snapshot A at tick 0, remote entity 2 at x = 0.
        let snap_a = Snapshot {
            tick: 0,
            baseline_tick: 0,
            server_time_ms: 0,
            local: arena_protocol::snapshot::LocalPlayerState {
                entity: local,
                state: player(local, 0.0),
                last_input_seq: 0,
                ammo_in_mag: 0,
                ammo_reserve: 0,
                respawn_at_tick: 0,
            },
            entities: vec![arena_protocol::entity::EntityDelta {
                id: 2,
                spawn: Some(player(2, 0.0)),
                ..Default::default()
            }],
            despawns: vec![],
            events: vec![],
        };
        cw.apply_snapshot(snap_a, 0);

        // Snapshot B at tick 10, remote entity 2 at x = 10.
        let snap_b = Snapshot {
            tick: 10,
            baseline_tick: 0,
            server_time_ms: 0,
            local: arena_protocol::snapshot::LocalPlayerState {
                entity: local,
                state: player(local, 0.0),
                last_input_seq: 0,
                ammo_in_mag: 0,
                ammo_reserve: 0,
                respawn_at_tick: 0,
            },
            entities: vec![arena_protocol::entity::EntityDelta {
                id: 2,
                spawn: Some(player(2, 10.0)),
                ..Default::default()
            }],
            despawns: vec![],
            events: vec![],
        };
        cw.apply_snapshot(snap_b, 0);

        // Sample the buffer directly at the midpoint render tick.
        let mid = cw.interp().sample(5.0);
        assert!((mid.get(&2).unwrap().pos.x - 5.0).abs() < 1e-4);
    }
}
