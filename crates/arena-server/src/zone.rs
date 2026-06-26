//! [`ZoneSim`]: one node's authoritative simulation of one zone.
//!
//! A [`ZoneSim`] owns the [`arena_sim::World`] for a single [`ZoneId`] plus the
//! per-client netcode state needed to ship it: a [`SnapshotEncoder`] (which itself owns
//! the per-client delta [`BaselineStore`]) and a roster of [`PlayerSlot`]s. The
//! [`ZoneManager`](crate::manager::ZoneManager) owns a map of these, one per zone this
//! node is authoritative for, and the fixed-tick loop is the *only* thing that touches
//! them — so all `World` access is single-threaded and lock-free.
//!
//! ## The per-tick flow ([`ZoneSim::step`])
//!
//! 1. Apply each player's newest queued input via [`World::set_input`].
//! 2. Advance the sim one tick ([`World::tick`]) and accumulate its events.
//! 3. Drain anti-cheat telemetry and, on a round boundary, feed it to [`AntiCheat`].
//! 4. Every [`TICKS_PER_SNAPSHOT`] ticks, compute each client's area-of-interest and
//!    unicast it a delta snapshot. **AOI culling is the bandwidth lever** that makes
//!    10,000 players affordable: a client only ever pays for entities near it.
//! 5. Evict players who have stopped sending input (timeout).

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use arena_mesh::{Envelope, MeshTransport};
use arena_net::SnapshotEncoder;
use arena_protocol::auth::SessionId;
use arena_protocol::entity::EntityState;
use arena_protocol::input::{InputBatch, InputFrame};
use arena_protocol::karma::CheatTelemetry;
use arena_protocol::message::{topic, ServerMsg};
use arena_protocol::snapshot::GameEvent;
use arena_protocol::world::{
    Aabb, MapId, SpawnPoint, Team, ZoneId, AOI_ZONE_RADIUS, WORLD_CEIL_M, WORLD_FLOOR_M,
    ZONE_SIZE_M,
};
use arena_protocol::{EntityId, NodeId, Tick};

use arena_content::registry::ContentRegistry;
use arena_content::worldgen::WorldGenParams;
use arena_content::ContentPack;

use arena_sim::map::MapDef;
use arena_sim::world::CheatCounters;
use arena_sim::World;

use crate::anticheat::AntiCheat;

/// AOI radius in metres: the player's own zone plus its ring of neighbours
/// ([`AOI_ZONE_RADIUS`] zones). Entities within this distance are replicated to a client.
pub const AOI_RADIUS_M: f32 = (AOI_ZONE_RADIUS as f32 + 1.0) * ZONE_SIZE_M;

/// A player is dropped from the sim after this many ticks with no fresh input — they have
/// disconnected or stalled, and we must not keep simulating (or billing) a ghost.
pub const PLAYER_TIMEOUT_TICKS: u32 = arena_protocol::TICK_HZ * 10; // 10 s

/// Ticks per anti-cheat "round". Telemetry is fused per round, not per tick, so the
/// detector's per-round statistics stay meaningful (a round ≈ 30 s here).
pub const ROUND_TICKS: u32 = arena_protocol::TICK_HZ * 30;

/// Everything the authority tracks about one connected client in this zone.
#[derive(Debug, Clone)]
pub struct PlayerSlot {
    /// The client's entity id within this zone's `World` (ids are per-zone).
    pub entity: EntityId,
    /// Highest input `seq` we have applied; echoed back so the client can reconcile.
    pub last_input_seq: u32,
    /// Last sim tick we received input on; drives the disconnect timeout.
    pub last_seen_tick: Tick,
    /// The client's last computed AOI entity set (diagnostics / churn tracking).
    pub aoi: HashSet<EntityId>,
    /// Queued input frames not yet applied, oldest first. The hot path keeps only the
    /// newest unapplied frame per tick; this buffer absorbs packet jitter/reorder.
    pub pending: Vec<InputFrame>,
    /// The most recent frame actually fed to the sim, retained so we can publish it in a
    /// [`VerifyTick`](arena_protocol::message::AuthorityMsg::VerifyTick) for cross-validation
    /// (the `World` keeps applied inputs private, so the slot mirrors it).
    last_applied: Option<InputFrame>,
    /// Per-round telemetry accumulator, flushed to [`AntiCheat`] every [`ROUND_TICKS`].
    round_telemetry: CheatCounters,
}

impl PlayerSlot {
    fn new(entity: EntityId, tick: Tick) -> Self {
        Self {
            entity,
            last_input_seq: 0,
            last_seen_tick: tick,
            aoi: HashSet::new(),
            pending: Vec::new(),
            last_applied: None,
            round_telemetry: CheatCounters::default(),
        }
    }
}

/// The authoritative simulation of one zone, plus its replication state.
pub struct ZoneSim {
    /// The zone this sim is authoritative for.
    pub zone: ZoneId,
    /// The mutable game world. Owned exclusively by the tick loop.
    pub world: World,
    /// Per-client AOI delta-snapshot encoder (owns the baseline ring internally).
    encoder: SnapshotEncoder,
    /// Connected clients, keyed by their authenticated CE node id.
    players: HashMap<NodeId, PlayerSlot>,
    /// Current sim tick (mirrors `world.current_tick()`).
    tick: Tick,
    /// Events accumulated since the last snapshot send, drained on each snapshot.
    pending_events: Vec<GameEvent>,
    /// The session, for building mesh topics.
    session: SessionId,
}

impl ZoneSim {
    /// Build a fresh authority for `zone` driven by `content`, simulating `geometry`.
    pub fn new(zone: ZoneId, session: SessionId, content: ContentRegistry, geometry: MapDef) -> Self {
        Self {
            zone,
            world: World::new(geometry, content),
            encoder: SnapshotEncoder::new(),
            players: HashMap::new(),
            tick: 0,
            pending_events: Vec::new(),
            session,
        }
    }

    /// Number of clients currently hosted in this zone.
    pub fn player_count(&self) -> usize {
        self.players.len()
    }

    /// Whether a given node is hosted here.
    pub fn has_player(&self, node: &NodeId) -> bool {
        self.players.contains_key(node)
    }

    /// Spawn a player for `node` on `team` and register a slot. Returns its entity id and
    /// the spawn point (read back from the freshly-spawned entity for the JoinAccept).
    pub fn add_player(&mut self, node: NodeId, team: Team) -> (EntityId, SpawnPoint) {
        let entity = self.world.spawn_player(node.clone(), team);
        let spawn = self
            .world
            .entities()
            .get(&entity)
            .map(|e| SpawnPoint { pos: e.pos, yaw: e.yaw, team: e.team })
            .unwrap_or(SpawnPoint { pos: self.zone.center(), yaw: 0.0, team });
        self.players.insert(node, PlayerSlot::new(entity, self.tick));
        (entity, spawn)
    }

    /// Remove a player (disconnect / hand-off), tearing down its sim entity and baselines.
    /// Returns the player's last authoritative state for hand-off carry-over, if present.
    pub fn remove_player(&mut self, node: &NodeId) -> Option<(EntityState, u32)> {
        let slot = self.players.remove(node)?;
        let state = self.world.entities().get(&slot.entity).cloned();
        self.world.remove_entity(slot.entity);
        self.encoder.forget(node);
        state.map(|s| (s, slot.last_input_seq))
    }

    /// Queue a client's input batch. Frames older than what we have already applied are
    /// dropped; the rest are buffered (sanitised lazily at apply time). The `ack_tick`
    /// advances the client's delta baseline.
    pub fn queue_input(&mut self, node: &NodeId, batch: InputBatch) {
        // Advance the delta baseline for the snapshots the client has confirmed.
        self.encoder.ack(node, batch.ack_tick);
        let Some(slot) = self.players.get_mut(node) else {
            // Input for a player we don't host (stale hand-off / race) — ignore.
            return;
        };
        for frame in batch.frames {
            if frame.seq > slot.last_input_seq {
                slot.pending.push(frame);
            }
        }
        slot.last_seen_tick = self.tick;
    }

    /// Stage a content pack for the next tick boundary (hot-reload).
    pub fn stage_content(&mut self, epoch: u64, pack: ContentPack) {
        if let Err(e) = self.world.stage_content(epoch, pack) {
            tracing::warn!(zone = %self.zone.token(), error = %e, "content stage rejected");
        }
    }

    /// A compact, comparable report of this zone's state for cross-validation /
    /// convergence checks: `(zone token, tick, post-tick state hash)`.
    pub fn state_report(&self) -> (String, Tick, [u8; 32]) {
        (self.zone.token(), self.tick, self.world.state_hash())
    }

    /// The inputs applied this tick, paired with their player node ids — the payload a
    /// [`VerifyTick`](arena_protocol::message::AuthorityMsg::VerifyTick) carries so peers can
    /// shadow-replay. (Best-effort: the newest applied frame per current player.)
    pub fn applied_inputs(&self) -> Vec<(NodeId, InputFrame)> {
        let mut out = Vec::with_capacity(self.players.len());
        for (node, slot) in &self.players {
            if let Some(frame) = slot.last_applied {
                out.push((node.clone(), frame));
            }
        }
        out
    }

    /// Advance this zone one tick and replicate.
    ///
    /// `anticheat` receives drained telemetry on round boundaries; `now_ms` is the server
    /// wall-clock estimate stamped into snapshots for client clock-sync.
    pub async fn step(&mut self, transport: &MeshTransport, anticheat: &mut AntiCheat, now_ms: u64) {
        // 1. Apply each player's newest queued input. The client batches several frames per
        //    packet for jitter resilience; we only need the freshest unapplied one to drive
        //    the authoritative sim this tick.
        for slot in self.players.values_mut() {
            if let Some(frame) = slot.pending.iter().copied().max_by_key(|f| f.seq) {
                self.world.set_input(slot.entity, frame);
                slot.last_input_seq = slot.last_input_seq.max(frame.seq);
                slot.last_applied = Some(frame);
            }
            slot.pending.clear();
        }

        // 2. Advance the simulation. This is the authoritative "what happened".
        let report = self.world.tick();
        self.tick = report.tick;
        self.pending_events.extend(report.events);

        // 3. Drain anti-cheat counters into per-round accumulators; flush on a round
        //    boundary so the detector sees per-round statistics, not per-tick noise.
        let round_boundary = self.tick % ROUND_TICKS == 0;
        for (node, slot) in self.players.iter_mut() {
            let c = self.world.take_telemetry(slot.entity);
            slot.round_telemetry = accumulate(slot.round_telemetry, c);
            if round_boundary {
                anticheat.observe_round(node, to_telemetry(node, slot.round_telemetry));
                slot.round_telemetry = CheatCounters::default();
            }
        }

        // 4. Evict players who stopped sending input.
        let timed_out: Vec<NodeId> = self
            .players
            .iter()
            .filter(|(_, s)| self.tick.saturating_sub(s.last_seen_tick) > PLAYER_TIMEOUT_TICKS)
            .map(|(n, _)| n.clone())
            .collect();
        for node in timed_out {
            tracing::debug!(zone = %self.zone.token(), player = %node, "evicting idle player");
            self.remove_player(&node);
        }

        // 5. On the snapshot cadence, unicast each client its AOI-scoped delta snapshot.
        if self.tick % arena_protocol::TICKS_PER_SNAPSHOT == 0 {
            self.send_snapshots(transport, now_ms).await;
            self.pending_events.clear();
        }
    }

    /// Encode and unicast one AOI-scoped delta snapshot per client. AOI culling here is
    /// what bounds bandwidth at 10k players: each client pays only for nearby entities.
    async fn send_snapshots(&mut self, transport: &MeshTransport, now_ms: u64) {
        let topic_name = topic::zone_state(&self.session, self.zone);
        // Snapshot the entity set once; every client diffs against the same authoritative map.
        let entities = self.world.entities().clone();

        // Collect per-client encode inputs first (immutable borrow of players), then encode
        // (mutable borrow of the encoder) — keeps the borrow checker happy without cloning slots.
        let clients: Vec<(NodeId, EntityId, u32)> = self
            .players
            .iter()
            .map(|(n, s)| (n.clone(), s.entity, s.last_input_seq))
            .collect();

        for (node, entity, last_seq) in clients {
            let center = entities.get(&entity).map(|e| e.pos).unwrap_or_else(|| self.zone.center());
            let aoi = compute_aoi(center, &entities, AOI_RADIUS_M, entity);

            // Mana repurposes the old ammo fields; respawn timer rides along.
            let (mana, max_mana, respawn_at) = self.world.combat_view(entity).unwrap_or((0, 0, 0));

            let snap = self.encoder.encode(
                &node,
                self.tick,
                now_ms,
                &entities,
                &aoi,
                entity,
                last_seq,
                (mana, max_mana),
                respawn_at,
                &self.pending_events,
            );

            if let Some(slot) = self.players.get_mut(&node) {
                slot.aoi = aoi;
            }

            // Fire-and-forget: a dropped snapshot is replaced by the next one, so we never
            // block the tick loop awaiting delivery confirmation per client.
            let env = Envelope::Server(ServerMsg::Snapshot(snap));
            if let Err(e) = transport.send_envelope(&node, &topic_name, &env).await {
                tracing::trace!(player = %node, error = %e, "snapshot send failed (will retry next tick)");
            }
        }
    }
}

/// Entities within `radius` metres of `center`, always including `self_entity`. This is
/// the AOI filter the snapshot encoder culls against — the core 10k-scale bandwidth lever.
pub fn compute_aoi(
    center: Vec3,
    entities: &HashMap<EntityId, EntityState>,
    radius: f32,
    self_entity: EntityId,
) -> HashSet<EntityId> {
    let r2 = radius * radius;
    let mut out: HashSet<EntityId> = entities
        .iter()
        .filter(|(_, e)| e.pos.distance_squared(center) <= r2)
        .map(|(id, _)| *id)
        .collect();
    out.insert(self_entity);
    out
}

/// Fold one tick's [`CheatCounters`] into a running per-round accumulator.
fn accumulate(mut a: CheatCounters, b: CheatCounters) -> CheatCounters {
    a.shots_fired += b.shots_fired;
    a.shots_hit += b.shots_hit;
    a.headshots += b.headshots;
    a.aim_snap_events += b.aim_snap_events;
    a.move_corrections += b.move_corrections;
    a.firerate_violations += b.firerate_violations;
    a
}

/// Map the sim's internal [`CheatCounters`] onto the protocol's wire [`CheatTelemetry`].
fn to_telemetry(player: &NodeId, c: CheatCounters) -> CheatTelemetry {
    CheatTelemetry {
        player: player.clone(),
        shots_fired: c.shots_fired,
        shots_hit: c.shots_hit,
        headshot_frac: if c.shots_hit == 0 {
            0.0
        } else {
            c.headshots as f32 / c.shots_hit as f32
        },
        aim_snap_events: c.aim_snap_events,
        move_corrections: c.move_corrections,
        firerate_violations: c.firerate_violations,
        // The sim does not yet measure reaction latency; 0 = unknown (the detector treats a
        // zero median as "no signal" rather than a sub-human reaction).
        median_reaction_ms: 0,
    }
}

/// Build a zone's collision geometry from the content worldgen recipe via `arena-procgen`,
/// falling back to the canonical [`MapDef::test_arena`] if procgen yields nothing.
///
/// Server-side we only need a coarse collider (box columns), not the full visible mesh —
/// see [`arena_procgen::world::generate_zone_collision`]. Spawn points are synthesised in a
/// ring around the zone centre, lifted just above the highest central terrain column.
pub fn build_zone_geometry(worldgen: &WorldGenParams, zone: ZoneId) -> MapDef {
    let brushes = arena_procgen::world::generate_zone_collision(worldgen, zone);
    if brushes.is_empty() {
        // Degenerate recipe (e.g. an empty/dev pack): fall back to the sealed test arena.
        return MapDef::test_arena();
    }

    let center = zone.center();
    // Approximate ground height near the centre from the tallest central column.
    let mut ground_y = WORLD_FLOOR_M;
    let quarter = ZONE_SIZE_M * 0.25;
    for b in &brushes {
        let c = b.center();
        if (c.x - center.x).abs() < quarter && (c.z - center.z).abs() < quarter {
            ground_y = ground_y.max(b.max.y);
        }
    }

    // A small ring of spawns, alternating teams, just above the surface.
    let offsets = [
        (-16.0, -16.0),
        (16.0, -16.0),
        (-16.0, 16.0),
        (16.0, 16.0),
        (0.0, -24.0),
        (0.0, 24.0),
        (-24.0, 0.0),
        (24.0, 0.0),
    ];
    let mut spawns = Vec::with_capacity(offsets.len());
    for (i, (dx, dz)) in offsets.into_iter().enumerate() {
        let team = if i % 2 == 0 { Team::Red } else { Team::Blue };
        spawns.push(SpawnPoint {
            pos: Vec3::new(center.x + dx, ground_y + 0.1, center.z + dz),
            yaw: 0.0,
            team,
        });
    }

    let bounds = Aabb::new(
        Vec3::new(zone.x as f32 * ZONE_SIZE_M, WORLD_FLOOR_M, zone.z as f32 * ZONE_SIZE_M),
        Vec3::new(
            (zone.x + 1) as f32 * ZONE_SIZE_M,
            WORLD_CEIL_M,
            (zone.z + 1) as f32 * ZONE_SIZE_M,
        ),
    );

    MapDef {
        id: MapId(format!("zone_{}", zone.token())),
        bounds,
        brushes,
        spawns,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::entity::{EntityFlags, EntityKind};

    fn ent(id: EntityId, pos: Vec3) -> EntityState {
        EntityState {
            id,
            kind: EntityKind::Player,
            pos,
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

    #[test]
    fn aoi_includes_near_and_excludes_far() {
        let mut entities = HashMap::new();
        entities.insert(1, ent(1, Vec3::new(0.0, 0.0, 0.0))); // the viewer
        entities.insert(2, ent(2, Vec3::new(10.0, 0.0, 0.0))); // near
        entities.insert(3, ent(3, Vec3::new(1000.0, 0.0, 0.0))); // far away

        let aoi = compute_aoi(Vec3::ZERO, &entities, AOI_RADIUS_M, 1);
        assert!(aoi.contains(&1), "the viewer itself is always in AOI");
        assert!(aoi.contains(&2), "a nearby entity must be in AOI");
        assert!(!aoi.contains(&3), "a far entity must be culled from AOI");
    }

    #[test]
    fn aoi_always_includes_self_even_if_absent() {
        // The local entity is always present in the snapshot even if it momentarily has no
        // position entry (mid-respawn), so the client keeps crisp truth for itself.
        let entities: HashMap<EntityId, EntityState> = HashMap::new();
        let aoi = compute_aoi(Vec3::ZERO, &entities, AOI_RADIUS_M, 7);
        assert!(aoi.contains(&7));
    }
}
