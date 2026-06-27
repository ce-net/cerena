//! The headless replica host — a donor/relay node hosting zones the **same way a
//! browser does**: it runs the shared [`arena_replica::Replica`] for each zone in
//! its set, driven by tick-tagged inputs from the zone's `/in` topic, and it
//! reconciles with the other replicas (browsers and nodes alike) by the periodic
//! state-hash quorum on `/proof`. There is no privileged authority here — this node
//! is just another replica that happens to have no screen.
//!
//! This is deliberately a *separate* path from the legacy single-authority
//! [`crate::server::ArenaServer`]: it is the forward model (everyone hosts), and
//! running it headless lets the exact mesh-driven replica loop the browser will run
//! be exercised and load-tested natively first. It shares 100% of its simulation and
//! consensus logic with the browser via `arena-replica` — only the transport differs
//! (CE node HTTP here, the `window.__ceNode` bridge in the tab).
//!
//! Per zone, each tick the host:
//! 1. drains tick-tagged inputs received on `/in` into the replica,
//! 2. advances the replica to the shared wall-clock tick ([`arena_replica::tick_at`]),
//! 3. on a cadence, publishes its own [`StateProof`] on `/proof`, tallies peers'
//!    proofs ([`arena_replica::agree`]), and — if out-voted — fetches the agreed
//!    snapshot and merges to it (`reseed`),
//! 4. if it is a top-K snapshot custodian for the zone (stake-weighted HRW), exports
//!    the whole-zone snapshot to the CE object store and advertises the CID on
//!    `/state` so joining/recovering replicas can converge.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use futures_util::StreamExt;

use arena_content::registry::ContentRegistry;
use arena_content::ContentPack;
use arena_karma::QuorumAuditor;
use arena_mesh::authority::{authority_ranking, Candidate};
use arena_mesh::MeshTransport;
use arena_protocol::auth::SessionId;
use arena_protocol::message::{topic, Envelope};
use arena_protocol::replica::{ReplicaMsg, SnapshotAd, StateProof};
use arena_protocol::world::ZoneId;
use arena_protocol::{NodeId, Tick, TICK_HZ};
use arena_replica::{agree, tick_at, Replica, Verdict};
use arena_sim::{World, ZoneSnapshot};

use crate::zone::build_zone_geometry;

/// Wall-clock milliseconds since the Unix epoch, for the shared tick clock.
fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

/// Per-zone host state.
struct ZoneReplica {
    replica: Replica,
    /// Latest state proofs gathered this interval: node -> hash.
    proofs: HashMap<NodeId, [u8; 32]>,
    /// The newest snapshot advert we have seen for this zone `(tick, cid)`.
    latest_snapshot: Option<(Tick, String)>,
}

/// A headless node hosting a set of zones as replicas. Build with [`ReplicaHost::new`],
/// then drive with [`ReplicaHost::run`].
pub struct ReplicaHost {
    transport: MeshTransport,
    session: SessionId,
    me: NodeId,
    pack: ContentPack,
    epoch: u64,
    zones: HashMap<ZoneId, ZoneReplica>,
    /// Reverse map from a subscribed `/in` topic to its zone (a `TaggedInput` carries
    /// no zone; the topic identifies it).
    input_topic_zone: HashMap<String, ZoneId>,
    /// Candidate set for stake-weighted custodian selection. Defaults to just this
    /// node (always custodian) until refreshed from discovery.
    candidates: Vec<Candidate>,
    /// How many top-ranked nodes are snapshot custodians per zone.
    replication_factor: usize,
    /// Ticks between state-proof publishes / quorum rounds.
    proof_interval: Tick,
    /// Ticks between snapshot-custody exports.
    snapshot_interval: Tick,
    /// Sustained-dissent tracker → slash recommendations (the anti-cheat backstop).
    auditor: QuorumAuditor,
}

impl ReplicaHost {
    /// Build a host for `session` on the local node, driving the `pack` content at
    /// `epoch`. `me` is this node's id (used as the proof author + custodian key).
    pub fn new(
        transport: MeshTransport,
        session: SessionId,
        me: NodeId,
        pack: ContentPack,
        epoch: u64,
        replication_factor: usize,
    ) -> Self {
        let candidates = vec![Candidate::new(me.clone(), 0)];
        Self {
            transport,
            session,
            me,
            pack,
            epoch,
            zones: HashMap::new(),
            input_topic_zone: HashMap::new(),
            candidates,
            replication_factor: replication_factor.max(1),
            proof_interval: 32,
            snapshot_interval: TICK_HZ, // ~1s
            auditor: QuorumAuditor::new(),
        }
    }

    /// Replace the custodian candidate set (refresh periodically from discovery; the
    /// CE capacity atlas of `"arena"`-capable nodes). Until called, this node is the
    /// sole candidate and therefore always a custodian.
    pub fn set_candidates(&mut self, candidates: Vec<Candidate>) {
        if !candidates.is_empty() {
            self.candidates = candidates;
        }
    }

    fn content(&self) -> ContentRegistry {
        ContentRegistry::new(self.epoch, self.pack.clone())
            .unwrap_or_else(|_| ContentRegistry::bootstrap())
    }

    /// Begin hosting `zone`: build a fresh replica positioned at the current shared
    /// tick (a later quorum round reseeds it from a peer snapshot if it turns out a
    /// populated zone already exists), and subscribe to its input/proof/state topics.
    pub async fn ensure_zone(&mut self, zone: ZoneId) -> Result<()> {
        if self.zones.contains_key(&zone) {
            return Ok(());
        }
        let geometry = build_zone_geometry(&self.pack.worldgen, zone);
        let mut world = World::new(geometry, self.content());
        world.set_tick(tick_at(now_ms()));
        let replica = Replica::new(world);

        let in_topic = topic::zone_input(&self.session, zone);
        let proof_topic = topic::zone_proof(&self.session, zone);
        let state_topic = topic::zone_state(&self.session, zone);
        self.transport.subscribe(&in_topic).await?;
        self.transport.subscribe(&proof_topic).await?;
        self.transport.subscribe(&state_topic).await?;
        self.input_topic_zone.insert(in_topic, zone);

        self.zones.insert(
            zone,
            ZoneReplica { replica, proofs: HashMap::new(), latest_snapshot: None },
        );
        tracing::info!(zone = %zone.token(), "replica-host now hosting zone");
        Ok(())
    }

    /// Route one inbound envelope (the `from` is the authenticated mesh sender).
    fn handle_envelope(&mut self, from: NodeId, in_topic: &str, env: Envelope) {
        let Envelope::Replica(msg) = env else { return };
        match msg {
            ReplicaMsg::Input(ti) => {
                if let Some(&zone) = self.input_topic_zone.get(in_topic) {
                    if let Some(zr) = self.zones.get_mut(&zone) {
                        zr.replica.schedule(from, ti);
                    }
                }
            }
            ReplicaMsg::Proof(StateProof { zone, hash, .. }) => {
                if let Some(zr) = self.zones.get_mut(&zone) {
                    zr.proofs.insert(from, hash);
                }
            }
            ReplicaMsg::Snapshot(SnapshotAd { zone, tick, cid }) => {
                if let Some(zr) = self.zones.get_mut(&zone) {
                    let newer = zr.latest_snapshot.as_ref().map_or(true, |(t, _)| tick >= *t);
                    if newer {
                        zr.latest_snapshot = Some((tick, cid));
                    }
                }
            }
        }
    }

    /// Is this node a top-K snapshot custodian for `zone`?
    fn is_custodian(&self, zone: ZoneId) -> bool {
        let ranking = authority_ranking(&self.session, zone, &self.candidates);
        ranking.iter().take(self.replication_factor).any(|n| n == &self.me)
    }

    /// One host tick across every hosted zone: advance, then on cadence publish a
    /// proof + reconcile, and export a custody snapshot.
    async fn step(&mut self) {
        let target = tick_at(now_ms());
        let zones: Vec<ZoneId> = self.zones.keys().copied().collect();
        for zone in zones {
            let tick = {
                let zr = self.zones.get_mut(&zone).expect("zone present");
                zr.replica.advance_to(target);
                zr.replica.tick()
            };

            if tick % self.proof_interval == 0 {
                self.publish_proof(zone, tick).await;
                self.reconcile(zone, tick).await;
            }
            if tick % self.snapshot_interval == 0 && self.is_custodian(zone) {
                self.publish_snapshot(zone, tick).await;
            }
        }
    }

    /// Publish this node's state-hash proof and record it among the votes.
    async fn publish_proof(&mut self, zone: ZoneId, tick: Tick) {
        let hash = self.zones[&zone].replica.state_hash();
        // Our own vote counts in the tally.
        self.zones.get_mut(&zone).unwrap().proofs.insert(self.me.clone(), hash);
        let env = Envelope::Replica(ReplicaMsg::Proof(StateProof { zone, tick, hash }));
        let topic_name = topic::zone_proof(&self.session, zone);
        if let Err(e) = self.transport.publish_envelope(&topic_name, &env).await {
            tracing::trace!(error = %e, "state-proof publish failed");
        }
    }

    /// Tally the gathered proofs; feed sustained dissent to the anti-cheat auditor; and
    /// if this node is itself out-voted by a quorum, fetch the advertised snapshot and
    /// merge to it so the zone stays single-valued.
    async fn reconcile(&mut self, zone: ZoneId, tick: Tick) {
        let (agreement, snapshot) = {
            let zr = self.zones.get(&zone).unwrap();
            let proofs: Vec<(NodeId, [u8; 32])> =
                zr.proofs.iter().map(|(n, h)| (n.clone(), *h)).collect();
            (agree(&proofs), zr.latest_snapshot.clone())
        };
        // A fresh tally each interval.
        self.zones.get_mut(&zone).unwrap().proofs.clear();

        // Only a trustworthy supermajority feeds the anti-cheat ledger; a 1-1 split or a
        // lone proof is inconclusive and must not punish anyone.
        if agreement.has_quorum {
            let slashes = self.auditor.record_round(zone, tick, &agreement.agree, &agreement.dissent);
            for v in slashes {
                tracing::warn!(
                    zone = %zone.token(),
                    node = %v.node,
                    rounds = v.disputed_ticks.len(),
                    "quorum: sustained state-hash dissent — recommend slash"
                );
            }
        }

        if let Verdict::ResyncTo(_agreed_hash) = agreement.verdict(&self.me) {
            // Merge to the quorum by adopting the latest advertised snapshot. (The
            // custody cadence keeps a recent one available; a future refinement is to
            // demand the snapshot whose hash equals `_agreed_hash` exactly.)
            if let Some((_t, cid)) = snapshot {
                if let Err(e) = self.merge_from_snapshot(zone, &cid).await {
                    tracing::warn!(zone = %zone.token(), error = %e, "merge-to-quorum failed");
                } else {
                    tracing::info!(zone = %zone.token(), "out-voted: merged to the quorum snapshot");
                }
            }
        }
    }

    /// Fetch a snapshot blob by CID, import it, and reseed the zone's replica at the
    /// current shared tick.
    async fn merge_from_snapshot(&mut self, zone: ZoneId, cid: &str) -> Result<()> {
        // Single-blob (not chunked object) so the CID is interoperable with the
        // browser host, which fetches the same `/blobs/:hash` over the mesh bridge.
        let bytes = self.transport.client().get_blob(cid).await?;
        let snap: ZoneSnapshot = bincode::deserialize(&bytes)?;
        let geometry = build_zone_geometry(&self.pack.worldgen, zone);
        let world = World::import_snapshot(geometry, self.content(), snap);
        let zr = self.zones.get_mut(&zone).unwrap();
        zr.replica.reseed(world);
        zr.replica.set_tick(tick_at(now_ms()));
        Ok(())
    }

    /// Export the whole-zone snapshot, store it content-addressed, and advertise the
    /// CID so joining/recovering replicas can converge.
    async fn publish_snapshot(&mut self, zone: ZoneId, tick: Tick) {
        let snap = self.zones[&zone].replica.world().export_snapshot();
        let bytes = match bincode::serialize(&snap) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, "snapshot serialize failed");
                return;
            }
        };
        let cid = match self.transport.client().put_blob(bytes).await {
            Ok(c) => c,
            Err(e) => {
                tracing::trace!(error = %e, "snapshot put_blob failed");
                return;
            }
        };
        let env = Envelope::Replica(ReplicaMsg::Snapshot(SnapshotAd { zone, tick, cid }));
        let topic_name = topic::zone_state(&self.session, zone);
        if let Err(e) = self.transport.publish_envelope(&topic_name, &env).await {
            tracing::trace!(error = %e, "snapshot advert publish failed");
        }
    }

    /// Host `initial_zones` until `shutdown` resolves, advancing every zone at the sim
    /// rate and servicing inbound mesh traffic.
    pub async fn run(
        mut self,
        initial_zones: Vec<ZoneId>,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<()> {
        for zone in initial_zones {
            self.ensure_zone(zone).await?;
        }
        // The inbound stream borrows the transport; clone the (cheap) handle into a local
        // so the stream doesn't pin `&self`, leaving `self` free for handle/step + advance.
        let transport = self.transport.clone();
        let mut stream = Box::pin(transport.envelopes().await?);
        let mut ticker = tokio::time::interval(Duration::from_millis((1000 / TICK_HZ.max(1)) as u64));
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                maybe = stream.next() => {
                    match maybe {
                        Some((from, in_topic, _reply, env)) => self.handle_envelope(from, &in_topic, env),
                        None => {
                            // Stream ended (node restart); rebuild it.
                            stream = Box::pin(transport.envelopes().await?);
                        }
                    }
                }
                _ = ticker.tick() => self.step().await,
            }
        }
        Ok(())
    }
}
