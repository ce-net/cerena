//! Proximity replication: redundancy that makes a crashed zone authority lossless.
//!
//! A zone is simulated by a single authority — a single point of failure for everyone in
//! it. To erase that risk we continuously replicate each player's *full* authoritative
//! checkpoint to the handful of peers physically **closest to them in the world**. If the
//! authority vanishes, the successor gathers those replicas and rebuilds the zone exactly,
//! instead of recovering only from the authority's last coarse broadcast. This is the
//! spacegame trick ("a standby adopts the replicated sector snapshot"), generalised to
//! per-player checkpoints held by whoever is standing next to you.
//!
//! ## Why *nearest* peers
//!
//! The K nearest other players are the cheapest, most robust holders:
//! - They are already in each other's area of interest, so they are exchanging packets
//!   anyway — pushing a checkpoint adds almost no new connectivity.
//! - They fail **independently of the authority**: the authority's crash (a process/host/
//!   network fault) does not take down the players standing in the zone, so the redundant
//!   copies survive exactly the event we are protecting against.
//!
//! ## Two roles, both owned by the tick loop
//!
//! - [`ReplicaStore`] — what *this* node holds *for others*. Every node runs one, even a
//!   light peer that owns no zones, so any participant can be a holder.
//! - [`ReplicationManager`] — the authority side: it picks holders, stamps monotonic
//!   checkpoint sequences, and tracks how many holders have acked (so the authority knows
//!   the replication factor is actually met before trusting it).
//!
//! Both live behind the single-consumer tick loop; mesh tasks only enqueue messages.

use std::collections::HashMap;

use glam::Vec3;

use arena_protocol::message::PlayerCheckpoint;
use arena_protocol::world::ZoneId;
use arena_protocol::{NodeId, Tick};

/// Default replication factor (K nearest holders per player).
pub const DEFAULT_FACTOR: usize = 3;

/// Default checkpoint cadence in ticks (~1 s at 64 Hz).
pub const DEFAULT_INTERVAL_TICKS: u32 = 64;

/// Max replicas a single node will hold for others before evicting the least-recently
/// touched. Bounds memory on a popular holder without a real cache library.
pub const MAX_HELD_REPLICAS: usize = 50_000;

/// A held replica ages out if it is not refreshed within this many ticks — its authority is
/// presumed gone or it moved out of proximity. Generous (~30 s) so a brief gap never drops a
/// valid backup.
pub const REPLICA_TTL_TICKS: Tick = 64 * 30;

/// One player checkpoint this node is holding on behalf of a (possibly dead) authority.
#[derive(Debug, Clone)]
pub struct StoredReplica {
    pub seq: u64,
    pub tick: Tick,
    /// The authority that issued this checkpoint (to ignore copies from a deposed one).
    pub authority: NodeId,
    /// The wire checkpoint, ready to hand to a successor verbatim.
    pub wire: PlayerCheckpoint,
    /// Last tick we (re)stored this replica, for LRU/TTL eviction.
    last_touch: Tick,
}

/// This node's replica holdings *for other authorities*, keyed by `(player, zone)`. Only the
/// newest sequence per key is kept; the set is bounded and TTL-evicted.
#[derive(Debug, Default)]
pub struct ReplicaStore {
    holdings: HashMap<(NodeId, ZoneId), StoredReplica>,
    cap: usize,
    ttl: Tick,
}

impl ReplicaStore {
    pub fn new() -> Self {
        Self {
            holdings: HashMap::new(),
            cap: MAX_HELD_REPLICAS,
            ttl: REPLICA_TTL_TICKS,
        }
    }

    /// Store a checkpoint received from `authority` for `zone`. Ignores it if we already hold
    /// a newer-or-equal sequence for that player+zone (a stale/deposed authority replaying old
    /// state cannot overwrite a fresher copy). Returns `true` if stored.
    pub fn store(
        &mut self,
        zone: ZoneId,
        checkpoint: PlayerCheckpoint,
        authority: NodeId,
        now_tick: Tick,
    ) -> bool {
        let key = (checkpoint.player.clone(), zone);
        if let Some(existing) = self.holdings.get(&key) {
            // Monotonic seq is the freshness oracle: a deposed authority can only present
            // an older (<=) seq, so seq-ordering subsumes "ignore stale authority".
            if existing.seq >= checkpoint.seq {
                return false;
            }
        }
        self.holdings.insert(
            key,
            StoredReplica {
                seq: checkpoint.seq,
                tick: checkpoint.tick,
                authority,
                wire: checkpoint,
                last_touch: now_tick,
            },
        );
        self.enforce_cap();
        true
    }

    /// Every checkpoint held for `zone` (what a successor gets in a [`ReplicaBundle`]).
    pub fn bundle_for(&self, zone: ZoneId) -> Vec<PlayerCheckpoint> {
        self.holdings
            .iter()
            .filter(|((_, z), _)| *z == zone)
            .map(|(_, r)| r.wire.clone())
            .collect()
    }

    /// `(player, highest seq held)` for `zone`, for a [`ReplicaStored`] ack back to the authority.
    pub fn acks_for(&self, zone: ZoneId) -> Vec<(NodeId, u64)> {
        self.holdings
            .iter()
            .filter(|((_, z), _)| *z == zone)
            .map(|((player, _), r)| (player.clone(), r.seq))
            .collect()
    }

    /// Drop replicas not refreshed within the TTL (their authority/proximity is gone).
    pub fn evict_expired(&mut self, now_tick: Tick) {
        let ttl = self.ttl;
        self.holdings
            .retain(|_, r| now_tick.saturating_sub(r.last_touch) <= ttl);
    }

    /// Number of replicas currently held (diagnostics).
    pub fn len(&self) -> usize {
        self.holdings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.holdings.is_empty()
    }

    /// Evict the least-recently-touched entries until under the cap.
    fn enforce_cap(&mut self) {
        while self.holdings.len() > self.cap {
            if let Some(oldest) = self
                .holdings
                .iter()
                .min_by_key(|(_, r)| r.last_touch)
                .map(|(k, _)| k.clone())
            {
                self.holdings.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

/// The raw material an authority needs to replicate one player: where they are (for holder
/// selection), their visible state, and the opaque sim checkpoint to serialise into the wire
/// `blob`. Produced by the zone sim; the engine stamps the seq and ships it.
pub struct PlayerExport {
    pub player: NodeId,
    pub entity: arena_protocol::EntityId,
    pub pos: Vec3,
    pub state: arena_protocol::entity::EntityState,
    /// The sim's full checkpoint (progression/inventory/status). Serialised to the wire blob.
    pub sim_ckpt: arena_sim::PlayerCheckpoint,
}

/// The authority side: holder selection, monotonic sequencing, and coverage tracking.
pub struct ReplicationManager {
    /// Replication factor K (number of nearest holders per player).
    pub factor: usize,
    /// Checkpoint cadence in ticks.
    pub interval_ticks: u32,
    /// Latest checkpoint seq issued per player (monotonic).
    seqs: HashMap<NodeId, u64>,
    /// player -> holder -> highest seq that holder has acked.
    coverage: HashMap<NodeId, HashMap<NodeId, u64>>,
}

impl ReplicationManager {
    pub fn new(factor: usize, interval_ticks: u32) -> Self {
        Self {
            factor: factor.max(1),
            interval_ticks: interval_ticks.max(1),
            seqs: HashMap::new(),
            coverage: HashMap::new(),
        }
    }

    /// True on the ticks a fresh checkpoint round is due.
    pub fn checkpoints_due(&self, tick: Tick) -> bool {
        tick % self.interval_ticks as Tick == 0
    }

    /// Pick the K nearest *other* players to `owner` as redundant holders (see the module
    /// docs for why proximity is the right choice). `owner` is excluded by id so a player is
    /// never its own backup; ties break by node id for determinism.
    pub fn select_holders(
        &self,
        owner: &NodeId,
        owner_pos: Vec3,
        all_players: &[(NodeId, Vec3)],
    ) -> Vec<NodeId> {
        let mut others: Vec<(&NodeId, f32)> = all_players
            .iter()
            .filter(|(node, _)| node != owner)
            .map(|(node, pos)| (node, pos.distance_squared(owner_pos)))
            .collect();
        others.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(b.0))
        });
        others
            .into_iter()
            .take(self.factor)
            .map(|(node, _)| node.clone())
            .collect()
    }

    /// Allocate the next monotonic checkpoint sequence for a player.
    pub fn next_seq(&mut self, player: &NodeId) -> u64 {
        let s = self.seqs.entry(player.clone()).or_insert(0);
        *s += 1;
        *s
    }

    /// The latest seq we issued for a player (0 if none yet).
    pub fn latest_seq(&self, player: &NodeId) -> u64 {
        self.seqs.get(player).copied().unwrap_or(0)
    }

    /// Record a holder's ack of the highest seq it durably holds for a player.
    pub fn record_ack(&mut self, player: &NodeId, holder: &NodeId, seq: u64) {
        let entry = self.coverage.entry(player.clone()).or_default();
        let slot = entry.entry(holder.clone()).or_insert(0);
        *slot = (*slot).max(seq);
    }

    /// How many holders have acked the player's *latest* issued checkpoint — the live
    /// replication factor. The authority can warn if this stays below `factor`.
    pub fn coverage(&self, player: &NodeId) -> usize {
        let latest = self.latest_seq(player);
        if latest == 0 {
            return 0;
        }
        self.coverage
            .get(player)
            .map(|holders| holders.values().filter(|&&s| s >= latest).count())
            .unwrap_or(0)
    }

    /// Forget a player who left the session, reclaiming their sequencing/coverage state.
    pub fn forget(&mut self, player: &NodeId) {
        self.seqs.remove(player);
        self.coverage.remove(player);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::entity::{EntityFlags, EntityKind, EntityState};
    use arena_protocol::world::Team;

    fn wire(player: &str, seq: u64, tick: Tick) -> PlayerCheckpoint {
        PlayerCheckpoint {
            player: player.to_string(),
            entity: 1,
            seq,
            tick,
            state: EntityState {
                id: 1,
                kind: EntityKind::Player,
                pos: Vec3::ZERO,
                vel: Vec3::ZERO,
                yaw: 0.0,
                pitch: 0.0,
                flags: EntityFlags::default(),
                team: Team::None,
                health: 100,
                armor: 0,
                weapon: 0,
                owner: player.to_string(),
            },
            blob: vec![1, 2, 3],
        }
    }

    #[test]
    fn store_keeps_newest_seq_and_ignores_stale() {
        let mut store = ReplicaStore::new();
        let zone = ZoneId::new(0, 0);

        assert!(store.store(zone, wire("p1", 5, 100), "authA".into(), 100));
        // A newer seq from the live authority replaces it.
        assert!(store.store(zone, wire("p1", 7, 110), "authA".into(), 110));
        // An older seq — e.g. a deposed authority replaying stale state — is rejected.
        assert!(!store.store(zone, wire("p1", 6, 120), "authB".into(), 120));
        // The same seq is also rejected (no churn).
        assert!(!store.store(zone, wire("p1", 7, 130), "authB".into(), 130));

        let acks = store.acks_for(zone);
        assert_eq!(acks, vec![("p1".to_string(), 7)]);
        assert_eq!(store.bundle_for(zone).len(), 1);
    }

    #[test]
    fn store_evicts_expired() {
        let mut store = ReplicaStore::new();
        let zone = ZoneId::new(1, 1);
        store.store(zone, wire("p1", 1, 0), "a".into(), 0);
        store.evict_expired(REPLICA_TTL_TICKS); // exactly at TTL: still kept
        assert_eq!(store.len(), 1);
        store.evict_expired(REPLICA_TTL_TICKS + 1); // past TTL: dropped
        assert!(store.is_empty());
    }

    #[test]
    fn select_holders_picks_k_nearest_others_excluding_self() {
        let mgr = ReplicationManager::new(2, 64);
        let owner: NodeId = "me".into();
        let owner_pos = Vec3::new(0.0, 0.0, 0.0);
        let players = vec![
            ("me".into(), Vec3::new(0.0, 0.0, 0.0)),       // self — must be excluded
            ("near1".into(), Vec3::new(1.0, 0.0, 0.0)),    // closest
            ("near2".into(), Vec3::new(3.0, 0.0, 0.0)),    // second
            ("far".into(), Vec3::new(100.0, 0.0, 0.0)),    // too far for k=2
        ];
        let holders = mgr.select_holders(&owner, owner_pos, &players);
        assert_eq!(holders, vec!["near1".to_string(), "near2".to_string()]);
        assert!(!holders.contains(&"me".to_string()), "self is never its own backup");
    }

    #[test]
    fn coverage_counts_holders_acking_latest() {
        let mut mgr = ReplicationManager::new(3, 64);
        let p: NodeId = "p1".into();
        let seq = mgr.next_seq(&p); // 1
        assert_eq!(seq, 1);
        assert_eq!(mgr.coverage(&p), 0);
        mgr.record_ack(&p, &"h1".into(), 1);
        mgr.record_ack(&p, &"h2".into(), 1);
        assert_eq!(mgr.coverage(&p), 2, "two holders acked the latest checkpoint");
        // A holder stuck on an old seq does not count toward current coverage.
        let _ = mgr.next_seq(&p); // 2
        assert_eq!(mgr.coverage(&p), 0, "holders are now behind the latest seq");
    }

    /// Round-trip a player through the wire `blob`: export from one world, bincode the sim
    /// checkpoint, rebuild the wire struct, then import into a fresh world. Exercises the real
    /// `arena_sim` checkpoint API (lands with this feature); it shares the same compile
    /// dependency as the production replication path.
    #[test]
    fn export_wire_import_roundtrip() {
        use crate::zone::build_zone_geometry;
        use arena_content::registry::ContentRegistry;
        use arena_sim::World;

        let pack = arena_content::default_pack();
        let zone = ZoneId::new(0, 0);

        let content = ContentRegistry::new(1, pack.clone()).unwrap();
        let mut world = World::new(build_zone_geometry(&pack.worldgen, zone), content);
        let owner: NodeId = "p1".into();
        let _e = world.spawn_player(owner.clone(), Team::None);

        // export -> bincode blob (this is exactly what the authority ships in the wire).
        let sim_ckpt = world.export_player(&owner).expect("player exists, export succeeds");
        let blob = bincode::serialize(&sim_ckpt).expect("serialize sim checkpoint");

        // Fresh world (the successor authority) imports the deserialised checkpoint.
        let content2 = ContentRegistry::new(1, pack.clone()).unwrap();
        let mut successor = World::new(build_zone_geometry(&pack.worldgen, zone), content2);
        let restored: arena_sim::PlayerCheckpoint =
            bincode::deserialize(&blob).expect("deserialize sim checkpoint");
        let imported = successor.import_player(restored);

        assert!(
            successor.entities().contains_key(&imported),
            "the imported player must exist in the successor world"
        );
    }
}
