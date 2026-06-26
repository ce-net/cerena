//! [`ArenaServer`]: the top-level orchestrator and the fixed-tick actor loop.
//!
//! `new` wires every subsystem to the local CE node; `run` spawns the concurrent mesh
//! producer tasks and then *becomes* the single tick-loop consumer that owns all
//! simulation state. See the crate docs for the actor architecture this implements.

use std::collections::HashMap;
use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot, watch};

use arena_mesh::{Discovery, Envelope, MeshTransport};

use arena_protocol::auth::SessionId;
use arena_protocol::entity::{EntityFlags, EntityKind, EntityState};
use arena_protocol::message::{topic, AuthorityMsg, ClientMsg, PlayerCheckpoint, ServerMsg};
use arena_protocol::weapon::default_loadout;
use arena_protocol::world::{MapId, SpawnPoint, Team, ZoneId};
use arena_protocol::{decode, NodeId, Tick, PROTOCOL_VERSION};

use arena_content::default_pack;
use arena_content::hotreload::ContentVersion;
use arena_content::ContentPack;

use crate::anticheat::AntiCheat;
use crate::config::ServerConfig;
use crate::coordinator::{Coordinator, JoinDecision};
use crate::handler::{ArenaHandler, Inbound};
use crate::manager::{HandoffRequest, ZoneManager};
use crate::replication::{ReplicaStore, ReplicationManager};

/// How many inbound messages the producer→consumer channel buffers before producers block.
/// Generous so a burst of input never stalls the mesh tasks; the tick loop drains it fast.
const INBOUND_BUFFER: usize = 16_384;

/// How often (in ticks) authorities publish a [`VerifyTick`](AuthorityMsg::VerifyTick) for
/// cross-validation. Once per second at 64 Hz.
const CROSSVAL_INTERVAL_TICKS: u64 = arena_protocol::TICK_HZ as u64;

/// How often (in ticks) the coordinator runs its karma pass.
const KARMA_INTERVAL_TICKS: u64 = arena_protocol::TICK_HZ as u64 * 5;

/// How often the discovery task refreshes the arena candidate set.
const DISCOVERY_INTERVAL: Duration = Duration::from_secs(5);

/// Round-trip timeout for authority-to-authority RPC (hand-off / adopt).
const AUTHORITY_RPC_TIMEOUT_MS: u64 = 4_000;

/// How long (in ticks) a successor authority collects [`ReplicaBundle`](AuthorityMsg::ReplicaBundle)
/// replies before rebuilding the zone from whatever arrived. Short — the holders are nearby and
/// answer fast — so failover is near-instant.
const RECOVERY_WINDOW_TICKS: u64 = arena_protocol::TICK_HZ as u64; // ~1 s

/// The top-level server. Build with [`ArenaServer::new`], then [`ArenaServer::run`].
pub struct ArenaServer {
    config: ServerConfig,
    transport: MeshTransport,
    discovery: Discovery,
    node_id: NodeId,
    manager: ZoneManager,
    coordinator: Option<Coordinator>,
    anticheat: AntiCheat,
}

impl ArenaServer {
    /// Connect to the local CE node and build every subsystem. Fails with a clear error if
    /// the CE node is unreachable (the orchestrator cannot run without it).
    pub async fn new(config: ServerConfig) -> Result<Self> {
        // Resolve the CE node API token: explicit config, else ce_rs discovery.
        let token = config
            .node_token
            .clone()
            .or_else(ce_rs::discover_api_token);
        let ce = ce_rs::CeClient::with_token(config.api_url.clone(), token);
        let transport = MeshTransport::new(ce.clone());
        let discovery = Discovery::new(ce);

        // This is where a missing/unreachable CE node surfaces — fail loudly and helpfully.
        let node_id = transport.node_id().await.with_context(|| {
            format!(
                "could not reach the local CE node at {} — is `ce start` running on this machine?",
                config.api_url
            )
        })?;

        // Everyone boots on the same default content at epoch 1; future packs hot-reload.
        let pack = default_pack();
        let epoch: u64 = 1;

        let manager = ZoneManager::new(node_id.clone(), config.session.clone(), pack.clone(), epoch);
        let anticheat = AntiCheat::new(node_id.clone(), pack.clone(), epoch);
        let coordinator = if config.coordinator {
            Some(Coordinator::new(
                config.session.clone(),
                config.map.clone(),
                config.e2e_insecure,
                node_id.clone(),
                epoch,
                pack.hash(),
            ))
        } else {
            None
        };

        Ok(Self {
            config,
            transport,
            discovery,
            node_id,
            manager,
            coordinator,
            anticheat,
        })
    }

    /// This node's CE id (the authority identity).
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Run the server until ctrl_c. Spawns the mesh producer tasks (RPC serve loop, inbound
    /// envelope fan-in, discovery, content fetch) and then runs the fixed-tick consumer loop
    /// that exclusively owns the simulation. Returns when shutdown completes.
    pub async fn run(self) -> Result<()> {
        let ArenaServer {
            config,
            transport,
            discovery,
            node_id,
            manager,
            coordinator,
            anticheat,
        } = self;

        let session = config.session.clone();
        let content_topic = format!("{}/content", session.topic_root());

        // Subscribe to the pub/sub control planes (directed RPC/inputs arrive regardless).
        transport
            .subscribe(&topic::authority(&session))
            .await
            .context("subscribe authority topic")?;
        transport
            .subscribe(&content_topic)
            .await
            .context("subscribe content topic")?;

        // The single producer→consumer channel and the shutdown broadcast.
        let (tx, mut rx) = mpsc::channel::<Inbound>(INBOUND_BUFFER);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        // ctrl_c → shutdown.
        {
            let shutdown_tx = shutdown_tx.clone();
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                tracing::info!("ctrl_c received; shutting down arena-server");
                let _ = shutdown_tx.send(true);
            });
        }

        // (a) Reliable-RPC serve loop. The handler enqueues Inbound::Request with a oneshot
        //     reply; the tick loop answers. serve_where owns reconnect/dedup/reply.
        {
            let serve_transport = transport.clone();
            let handler = ArenaHandler::new(tx.clone());
            let mut srx = shutdown_rx.clone();
            let session_root = session.topic_root();
            tokio::spawn(async move {
                let shutdown = async move {
                    let _ = srx.changed().await;
                };
                if let Err(e) = serve_transport
                    .serve_session(&session_root, &handler, shutdown)
                    .await
                {
                    tracing::error!(error = %e, "RPC serve loop exited with error");
                }
            });
        }

        // (b) Fire-and-forget inbound fan-in. envelopes() decodes arena Envelopes; we forward
        //     only non-request messages (requests carry a reply_token and are served by (a)).
        {
            let env_transport = transport.clone();
            let env_tx = tx.clone();
            let mut erx = shutdown_rx.clone();
            tokio::spawn(async move {
                let stream = match env_transport.envelopes().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(error = %e, "could not open inbound envelope stream");
                        return;
                    }
                };
                tokio::pin!(stream);
                loop {
                    tokio::select! {
                        _ = erx.changed() => break,
                        item = stream.next() => match item {
                            Some((from, topic, reply_token, env)) => {
                                if reply_token.is_none() {
                                    let _ = env_tx.send(Inbound::Message { from, topic, env }).await;
                                }
                            }
                            None => break,
                        }
                    }
                }
            });
        }

        // (c) Discovery: refresh the arena candidate set on a timer.
        {
            let disc = discovery.clone();
            let dtx = tx.clone();
            let mut drx = shutdown_rx.clone();
            tokio::spawn(async move {
                let mut ticker = tokio::time::interval(DISCOVERY_INTERVAL);
                loop {
                    tokio::select! {
                        _ = drx.changed() => break,
                        _ = ticker.tick() => match disc.arena_nodes().await {
                            Ok(candidates) => { let _ = dtx.send(Inbound::Candidates(candidates)).await; }
                            Err(e) => tracing::debug!(error = %e, "candidate discovery failed (atlas unavailable?)"),
                        }
                    }
                }
            });
        }

        // (d) Content-version watcher: fetch + decode announced packs and queue them to stage.
        {
            let ce = transport.client().clone();
            let ctx = tx.clone();
            let mut crx = shutdown_rx.clone();
            let content_topic = content_topic.clone();
            tokio::spawn(async move {
                let stream = match ce.messages_stream().await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!(error = %e, "content watcher: stream open failed");
                        return;
                    }
                };
                tokio::pin!(stream);
                loop {
                    tokio::select! {
                        _ = crx.changed() => break,
                        item = stream.next() => match item {
                            Some(Ok(m)) if m.topic == content_topic => {
                                if let Some((epoch, pack)) = fetch_announced_pack(&ce, &m).await {
                                    let _ = ctx.send(Inbound::StageContent { epoch, pack }).await;
                                }
                            }
                            Some(_) => {}
                            None => break,
                        }
                    }
                }
            });
        }

        // The single consumer: the fixed-tick loop that owns all simulation state.
        let mut engine = Engine {
            replica_store: ReplicaStore::new(),
            replication: ReplicationManager::new(
                config.replication_factor,
                config.replication_interval_ticks,
            ),
            recoveries: HashMap::new(),
            replication_subs: HashSet::new(),
            config: config.clone(),
            node_id,
            transport,
            manager,
            coordinator,
            anticheat,
            global_tick: 0,
        };

        tracing::info!(
            session = %session.as_str(),
            tick_hz = config.tick_hz,
            coordinator = config.coordinator,
            "arena-server tick loop starting"
        );

        let period = Duration::from_secs_f64(config.tick_period_secs());
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut main_shutdown = shutdown_rx.clone();

        loop {
            tokio::select! {
                _ = main_shutdown.changed() => {
                    tracing::info!("tick loop shutting down");
                    break;
                }
                _ = ticker.tick() => {
                    // Drain everything the producers enqueued, then advance the sim once.
                    while let Ok(msg) = rx.try_recv() {
                        engine.dispatch(msg).await;
                    }
                    engine.step(now_ms()).await;
                }
            }
        }

        Ok(())
    }
}

/// In-flight failover recovery for one zone this node just claimed: it gathers replica
/// bundles from surviving holders until `deadline_tick`, keeping the newest checkpoint per
/// player, then rebuilds the zone from them.
struct Recovery {
    deadline_tick: u64,
    /// Newest checkpoint seen per player across all bundles received so far.
    best: HashMap<NodeId, PlayerCheckpoint>,
}

/// The tick-loop consumer's owned state. Lives on one task; never shared, never locked.
struct Engine {
    config: ServerConfig,
    node_id: NodeId,
    transport: MeshTransport,
    manager: ZoneManager,
    coordinator: Option<Coordinator>,
    anticheat: AntiCheat,
    /// Replicas this node holds *for others* — its role as a redundant backup. Every node runs
    /// one, even if it owns no zones.
    replica_store: ReplicaStore,
    /// Authority-side proximity replication: holder selection, sequencing, coverage.
    replication: ReplicationManager,
    /// Zones currently being recovered after a failover claim, keyed by zone.
    recoveries: HashMap<ZoneId, Recovery>,
    /// Replication topics we have subscribed to (as a holder) so we hear failover gathers.
    replication_subs: HashSet<ZoneId>,
    /// Monotonic wall-clock tick counter driving the periodic crossval / karma / replication passes.
    global_tick: u64,
}

impl Engine {
    /// Route one inbound message from a producer task.
    async fn dispatch(&mut self, msg: Inbound) {
        match msg {
            Inbound::Request { from, topic, env, reply } => {
                self.handle_request(from, topic, env, reply).await;
            }
            Inbound::Message { from, topic, env } => {
                self.handle_message(from, topic, env).await;
            }
            Inbound::Candidates(candidates) => {
                self.manager.update_candidates(candidates);
                // Ownership may have changed (e.g. a dead authority dropped out): reconcile,
                // hand off any lost zones' players, and start failover recovery for zones we
                // just took over.
                let (handoffs, claimed) = self.manager.reconcile_ownership(&self.transport).await;
                self.spawn_handoffs(handoffs);
                for zone in claimed {
                    self.start_recovery(zone).await;
                }
            }
            Inbound::StageContent { epoch, pack } => {
                tracing::info!(epoch, "staging hot-reloaded content");
                self.manager.stage_content(epoch, pack.clone());
                self.anticheat.stage_content(epoch, pack);
            }
        }
    }

    /// Answer a reliable RPC. Most replies are immediate; cross-node joins move the reply
    /// channel into a detached forwarding task so the tick loop never blocks on a peer.
    async fn handle_request(
        &mut self,
        from: NodeId,
        _topic: String,
        env: Envelope,
        reply: oneshot::Sender<Envelope>,
    ) {
        let now_ms = now_ms();
        match env {
            Envelope::Client(ClientMsg::Join { ticket, team_pref, .. }) => {
                let Some(coord) = self.coordinator.as_mut() else {
                    // Only the coordinator admits players; a non-coordinator was asked by mistake.
                    let _ = reply.send(server(ServerMsg::JoinReject {
                        reason: "this node is not the session coordinator".into(),
                    }));
                    return;
                };
                // Disjoint field borrows: the coordinator (mut) decides admission while the
                // closure queries the manager's (shared) HRW router for the spawn-zone owner.
                let decision = coord.handle_join(
                    &ticket,
                    team_pref,
                    &from,
                    &self.node_id,
                    now_unix(),
                    |zone| self.manager.authority_for(zone),
                );
                match decision {
                    JoinDecision::Reject(reason) => {
                        let _ = reply.send(server(ServerMsg::JoinReject { reason }));
                    }
                    JoinDecision::Accept { zone, authority, team } if authority == self.node_id => {
                        // We own the spawn zone: add the player locally and reply immediately.
                        let sim = self.manager.ensure_zone(zone);
                        let (entity, spawn) = sim.add_player(from.clone(), team);
                        let tick = sim.current_tick();
                        let _ = reply.send(server(ServerMsg::JoinAccept {
                            protocol: PROTOCOL_VERSION,
                            entity,
                            zone,
                            authority: self.node_id.clone(),
                            tick,
                            server_time_ms: now_ms,
                            loadout: default_loadout(),
                            spawn,
                            map: self.config.map.clone(),
                        }));
                    }
                    JoinDecision::Accept { zone, authority, team } => {
                        // The spawn zone is owned by another node: forward the adoption and reply
                        // asynchronously so the tick loop is not blocked on the round trip.
                        let transport = self.transport.clone();
                        let session = self.config.session.clone();
                        let map = self.config.map.clone();
                        let player = from.clone();
                        tokio::spawn(async move {
                            forward_join(transport, session, map, authority, zone, team, player, reply, now_ms).await;
                        });
                    }
                }
            }

            Envelope::Client(ClientMsg::Ping { client_time_ms }) => {
                let _ = reply.send(server(ServerMsg::Pong { client_time_ms, server_time_ms: now_ms }));
            }

            Envelope::Client(ClientMsg::Report(report)) => {
                // Weight the report by the reporter's karma (default if no coordinator/ledger).
                let reporter_karma = self
                    .coordinator
                    .as_ref()
                    .map(|c| c.karma(&from))
                    .unwrap_or(arena_protocol::karma::KARMA_DEFAULT);
                self.anticheat.on_report(&report, reporter_karma);
                // No dedicated ack message; a Pong confirms receipt.
                let _ = reply.send(server(ServerMsg::Pong { client_time_ms: 0, server_time_ms: now_ms }));
            }

            Envelope::Client(ClientMsg::RequestZoneSwitch { to }) => {
                // Tell the client which node owns the requested zone so it re-homes its input
                // stream there. The actual entity migration happens via boundary hand-off.
                match self.manager.authority_for(to) {
                    Some(authority) => {
                        let _ = reply.send(server(ServerMsg::Redirect {
                            zone: to,
                            authority,
                            entity: 0, // assigned by the new authority on adopt
                            tick: 0,
                        }));
                    }
                    None => {
                        let _ = reply.send(server(ServerMsg::Pong { client_time_ms: 0, server_time_ms: now_ms }));
                    }
                }
            }

            Envelope::Client(ClientMsg::Leave) => {
                self.manager.remove_player(&from);
                let _ = reply.send(server(ServerMsg::Pong { client_time_ms: 0, server_time_ms: now_ms }));
            }

            Envelope::Authority(AuthorityMsg::AdoptPlayer { .. }) => {
                let msg = match &env {
                    Envelope::Authority(m) => m,
                    _ => unreachable!(),
                };
                let reply_env = self
                    .manager
                    .adopt_player(msg)
                    .map(Envelope::Authority)
                    .unwrap_or_else(|| server(ServerMsg::Kick { reason: "adopt failed".into() }));
                let _ = reply.send(reply_env);
            }

            Envelope::Authority(AuthorityMsg::VerifyTick { zone, tick, result_hash, inputs, .. }) => {
                // Acting as a shadow verifier on request: replay and vote.
                let result = self.anticheat.on_verify_tick(zone, tick, result_hash, inputs, from);
                let _ = reply.send(Envelope::Authority(result));
            }

            Envelope::Authority(AuthorityMsg::AuthorityClaim { .. }) => {
                let msg = match &env {
                    Envelope::Authority(m) => m,
                    _ => unreachable!(),
                };
                let handoffs = self.manager.on_authority_claim(msg);
                self.spawn_handoffs(handoffs);
                let _ = reply.send(server(ServerMsg::Pong { client_time_ms: 0, server_time_ms: now_ms }));
            }

            // Anything else over RPC gets a benign ack so the requester never times out.
            _ => {
                let _ = reply.send(server(ServerMsg::Pong { client_time_ms: 0, server_time_ms: now_ms }));
            }
        }
    }

    /// Handle one fire-and-forget message.
    async fn handle_message(&mut self, from: NodeId, _topic: String, env: Envelope) {
        match env {
            // The high-rate path: route the player's input to the zone hosting them.
            Envelope::Client(ClientMsg::Input(batch)) => {
                self.manager.queue_input(&from, batch);
            }

            // A peer authority asks us to shadow-verify a tick: replay and publish our vote.
            Envelope::Authority(AuthorityMsg::VerifyTick { zone, tick, result_hash, inputs, .. }) => {
                let result = self.anticheat.on_verify_tick(zone, tick, result_hash, inputs, from);
                let env = Envelope::Authority(result);
                let topic_name = topic::authority(&self.config.session);
                let _ = self.transport.publish_envelope(&topic_name, &env).await;
            }

            // A verifier's vote: tally it and, on a quorum, act on the verdict.
            Envelope::Authority(AuthorityMsg::VerifyResult { .. }) => {
                let msg = match &env {
                    Envelope::Authority(m) => m,
                    _ => unreachable!(),
                };
                if let Some(verdict) = self.anticheat.on_verify_result(msg) {
                    if let Some(coord) = self.coordinator.as_mut() {
                        if let Some(update) = coord.apply_verdict(&verdict, now_unix()) {
                            broadcast_karma(&self.transport, &update).await;
                        }
                    }
                    if verdict.recommend_reassign {
                        tracing::warn!(
                            authority = %verdict.authority,
                            disputes = verdict.disputed_ticks.len(),
                            slash = verdict.recommend_slash,
                            "authority disputed by cross-validation; zone will reassign via HRW/claims (slash routes to ce-gov)"
                        );
                    }
                }
            }

            // A peer claims/reclaims a zone: record it and yield if it outranks our lease.
            Envelope::Authority(AuthorityMsg::AuthorityClaim { .. }) => {
                let msg = match &env {
                    Envelope::Authority(m) => m,
                    _ => unreachable!(),
                };
                let handoffs = self.manager.on_authority_claim(msg);
                self.spawn_handoffs(handoffs);
            }

            // Holder role: an authority asks us to redundantly hold player checkpoints. Store
            // them (newest seq wins), subscribe to this zone's replication plane so we hear a
            // future failover gather, and ack the highest seq held back to the authority.
            Envelope::Authority(AuthorityMsg::ReplicateCheckpoint {
                session,
                zone,
                authority,
                checkpoints,
                ..
            }) => {
                let now_tick = self.global_tick as Tick;
                for ckpt in checkpoints {
                    self.replica_store.store(zone, ckpt, authority.clone(), now_tick);
                }
                // Subscribe once per zone so the successor's RequestReplicas reaches us.
                if self.replication_subs.insert(zone) {
                    let topic_name = topic::replication(&session, zone);
                    let _ = self.transport.subscribe(&topic_name).await;
                }
                // Ack coverage back to the issuing authority.
                let ack = Envelope::Authority(AuthorityMsg::ReplicaStored {
                    holder: self.node_id.clone(),
                    zone,
                    acked: self.replica_store.acks_for(zone),
                });
                let topic_name = topic::replication(&session, zone);
                let _ = self.transport.send_envelope(&authority, &topic_name, &ack).await;
            }

            // Successor authority on failover wants every checkpoint we hold for a zone. Reply
            // directly with our bundle (no reply token — RequestReplicas is a broadcast).
            Envelope::Authority(AuthorityMsg::RequestReplicas { session, zone, requester }) => {
                if requester == self.node_id {
                    return; // our own broadcast echoed back
                }
                let checkpoints = self.replica_store.bundle_for(zone);
                if checkpoints.is_empty() {
                    return;
                }
                let bundle = Envelope::Authority(AuthorityMsg::ReplicaBundle {
                    zone,
                    holder: self.node_id.clone(),
                    checkpoints,
                });
                let topic_name = topic::replication(&session, zone);
                let _ = self.transport.send_envelope(&requester, &topic_name, &bundle).await;
            }

            // Authority role: a holder confirms how much of each player it durably holds —
            // update coverage so we know the replication factor is met.
            Envelope::Authority(AuthorityMsg::ReplicaStored { holder, acked, .. }) => {
                for (player, seq) in acked {
                    self.replication.record_ack(&player, &holder, seq);
                }
            }

            // Recovery: a holder's bundle in answer to our gather — fold it into the open
            // recovery for that zone (newest checkpoint per player).
            Envelope::Authority(AuthorityMsg::ReplicaBundle { zone, checkpoints, .. }) => {
                self.collect_bundle(zone, checkpoints);
            }

            // Border mirrors are read-only neighbour state for cross-zone hitscan/rendering.
            // Applying them as ZoneMirror entities is a refinement; accepted + ignored for now.
            Envelope::Authority(AuthorityMsg::BorderMirror { .. }) => {}

            // Adopted arrives as the RPC reply inside the hand-off task, not here; ignore stray ones.
            Envelope::Authority(AuthorityMsg::Adopted { .. }) => {}

            // Client RPCs delivered fire-and-forget (no reply token) are ignored — those paths
            // require the reliable RPC channel.
            _ => {}
        }
    }

    /// Advance every owned zone one tick and run the periodic cross-validation / karma passes.
    async fn step(&mut self, now_ms: u64) {
        self.global_tick += 1;

        // Step all zones; migrate players that crossed into another node's zone.
        let handoffs = self
            .manager
            .tick_all(&self.transport, &mut self.anticheat, now_ms)
            .await;
        self.spawn_handoffs(handoffs);

        // Proximity replication: periodically push each player's checkpoint to its nearest
        // peers so a crash is lossless.
        if self.replication.checkpoints_due(self.global_tick as Tick) {
            self.emit_checkpoints().await;
        }

        // Finalize any failover recovery whose collection window has elapsed.
        self.finalize_recoveries().await;

        // Age out stale replicas we hold for others.
        self.replica_store.evict_expired(self.global_tick as Tick);

        // Publish verify-ticks for our zones so peers can shadow-replay them.
        if self.global_tick % CROSSVAL_INTERVAL_TICKS == 0 {
            self.emit_verifications().await;
        }

        // Coordinator-only: fuse client suspicion + report pressure and resolve disputes.
        if self.coordinator.is_some() && self.global_tick % KARMA_INTERVAL_TICKS == 0 {
            self.karma_pass(now_unix()).await;
        }
    }

    /// Producer side of proximity replication: for every player in every owned zone, ship a
    /// fresh checkpoint to its K nearest peers. Directed sends (the holders are specific nodes),
    /// fire-and-forget — a dropped checkpoint is replaced by the next round.
    async fn emit_checkpoints(&mut self) {
        let batches = self.manager.replication_batch();
        for batch in batches {
            let topic_name = topic::replication(&self.config.session, batch.zone);
            for export in &batch.exports {
                // Serialise the sim checkpoint into the opaque wire blob.
                let blob = match bincode::serialize(&export.sim_ckpt) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(player = %export.player, error = %e, "checkpoint serialize failed");
                        continue;
                    }
                };
                let seq = self.replication.next_seq(&export.player);
                let wire = PlayerCheckpoint {
                    player: export.player.clone(),
                    entity: export.entity,
                    seq,
                    tick: batch.tick,
                    state: export.state.clone(),
                    blob,
                };
                // The K nearest *other* players are this player's redundant backups.
                let holders =
                    self.replication
                        .select_holders(&export.player, export.pos, &batch.players);
                let env = Envelope::Authority(AuthorityMsg::ReplicateCheckpoint {
                    session: self.config.session.clone(),
                    zone: batch.zone,
                    tick: batch.tick,
                    authority: self.node_id.clone(),
                    checkpoints: vec![wire.clone()],
                });
                for holder in holders {
                    if let Err(e) = self.transport.send_envelope(&holder, &topic_name, &env).await {
                        tracing::trace!(holder = %holder, error = %e, "checkpoint replicate send failed");
                    }
                }
            }
        }
    }

    /// Begin failover recovery for a freshly-claimed zone: broadcast a replica-gather request
    /// to the fleet and open a short collection window. Surviving holders answer with the
    /// checkpoints they hold; [`finalize_recoveries`](Self::finalize_recoveries) rebuilds the
    /// zone from the newest per player. This is the redundancy headline: no central standby —
    /// the players who were standing next to the crashed authority's players ARE the backup.
    async fn start_recovery(&mut self, zone: ZoneId) {
        let topic_name = topic::replication(&self.config.session, zone);
        // Subscribe so directed bundle replies and any cross-talk on this plane reach us.
        if self.replication_subs.insert(zone) {
            let _ = self.transport.subscribe(&topic_name).await;
        }
        let env = Envelope::Authority(AuthorityMsg::RequestReplicas {
            session: self.config.session.clone(),
            zone,
            requester: self.node_id.clone(),
        });
        if let Err(e) = self.transport.publish_envelope(&topic_name, &env).await {
            tracing::warn!(zone = %zone.token(), error = %e, "replica-gather broadcast failed");
        }
        self.recoveries.insert(
            zone,
            Recovery {
                deadline_tick: self.global_tick + RECOVERY_WINDOW_TICKS,
                best: HashMap::new(),
            },
        );
        tracing::info!(zone = %zone.token(), "failover recovery started; gathering proximity replicas");
    }

    /// Fold an incoming replica bundle into any open recovery for its zone (newest per player).
    fn collect_bundle(&mut self, zone: ZoneId, checkpoints: Vec<PlayerCheckpoint>) {
        let Some(recovery) = self.recoveries.get_mut(&zone) else {
            return; // not recovering this zone (late/duplicate bundle) — ignore
        };
        for ckpt in checkpoints {
            let keep = recovery
                .best
                .get(&ckpt.player)
                .map(|existing| ckpt.seq > existing.seq)
                .unwrap_or(true);
            if keep {
                recovery.best.insert(ckpt.player.clone(), ckpt);
            }
        }
    }

    /// Complete any recovery whose window elapsed: import the newest checkpoint per player into
    /// the rebuilt zone and redirect those players to this node. If no replicas arrived the zone
    /// simply starts empty and players re-home via their own redirect/rejoin (the coarse fallback).
    async fn finalize_recoveries(&mut self) {
        let due: Vec<ZoneId> = self
            .recoveries
            .iter()
            .filter(|(_, r)| self.global_tick >= r.deadline_tick)
            .map(|(z, _)| *z)
            .collect();

        for zone in due {
            let Some(recovery) = self.recoveries.remove(&zone) else { continue };
            let count = recovery.best.len();
            for (player, ckpt) in recovery.best {
                // Decode the opaque sim checkpoint and import it, losslessly restoring the player.
                let sim_ckpt: arena_sim::PlayerCheckpoint = match bincode::deserialize(&ckpt.blob) {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(player = %player, error = %e, "replica blob decode failed; skipping");
                        continue;
                    }
                };
                let entity = self.manager.import_recovered(zone, player.clone(), sim_ckpt);
                // Re-home the recovered player's client to us as the new authority.
                let redirect = Envelope::Server(ServerMsg::Redirect {
                    zone,
                    authority: self.node_id.clone(),
                    entity,
                    tick: 0,
                });
                let client_topic = topic::zone_state(&self.config.session, zone);
                let _ = self.transport.send_envelope(&player, &client_topic, &redirect).await;
            }
            tracing::info!(zone = %zone.token(), restored = count, "failover recovery complete");
        }
    }

    /// Publish one [`VerifyTick`](AuthorityMsg::VerifyTick) per owned zone that applied input
    /// this tick, recording our own claim so we can tally the votes that come back.
    async fn emit_verifications(&mut self) {
        let payloads = self.manager.verify_payloads();
        let topic_name = topic::authority(&self.config.session);
        for (zone, tick, hash, inputs) in payloads {
            if inputs.is_empty() {
                continue; // nothing meaningful to cross-validate this tick
            }
            let mut result_hash = hash;
            if self.config.e2e_cheat {
                // TEST ONLY: a malicious authority publishes a wrong hash. Honest verifiers
                // shadow-replay the same inputs, disagree, and the quorum disputes this tick.
                result_hash[0] ^= 0xFF;
            }
            // Record the (honest or lying) claim we are asking peers to check.
            self.anticheat.record_own_claim(zone, tick, result_hash);
            let env = Envelope::Authority(AuthorityMsg::VerifyTick {
                session: self.config.session.clone(),
                zone,
                tick,
                result_hash,
                inputs,
            });
            if let Err(e) = self.transport.publish_envelope(&topic_name, &env).await {
                tracing::trace!(error = %e, "verify-tick publish failed");
            }
        }
    }

    /// The coordinator's periodic karma pass: fuse per-player suspicion + report pressure into
    /// the ledger, and dock karma for any authority that cross-validation has disputed.
    async fn karma_pass(&mut self, now_unix: u64) {
        let roster = self.manager.all_player_nodes();
        let mut to_broadcast = Vec::new();
        {
            let Some(coord) = self.coordinator.as_mut() else { return };
            for node in &roster {
                // Disjoint field borrows: coordinator (mut) vs anticheat (shared).
                let suspicion = self.anticheat.suspicion(node);
                let pressure = self.anticheat.report_pressure(node, 0);
                if let Some(update) = coord.fuse_player(node, suspicion.as_ref(), pressure, now_unix) {
                    to_broadcast.push(update);
                }
            }
            for verdict in self.anticheat.resolve_disputes() {
                if let Some(update) = coord.apply_verdict(&verdict, now_unix) {
                    to_broadcast.push(update);
                }
            }
        }
        for update in to_broadcast {
            broadcast_karma(&self.transport, &update).await;
        }
    }

    /// Detach each cross-node hand-off so the AdoptPlayer round trip never stalls the tick loop.
    fn spawn_handoffs(&self, handoffs: Vec<HandoffRequest>) {
        for req in handoffs {
            let transport = self.transport.clone();
            let session = self.config.session.clone();
            tokio::spawn(async move {
                run_handoff(transport, session, req).await;
            });
        }
    }
}

/// Build a `Server`-tagged envelope.
fn server(msg: ServerMsg) -> Envelope {
    Envelope::Server(msg)
}

/// Unix milliseconds (server clock estimate stamped into snapshots).
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Unix seconds (for the karma ledger, which never reads the clock itself).
fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Broadcast a karma verdict to the fleet on the shared verdict topic.
async fn broadcast_karma(transport: &MeshTransport, update: &arena_protocol::karma::KarmaUpdate) {
    let env = Envelope::Server(ServerMsg::Karma(update.clone()));
    if let Err(e) = transport.publish_envelope(topic::KARMA_VERDICT, &env).await {
        tracing::trace!(error = %e, "karma verdict broadcast failed");
    }
}

/// Drive one cross-node hand-off: AdoptPlayer → Adopted → Redirect the client. Runs detached.
async fn run_handoff(transport: MeshTransport, session: SessionId, req: HandoffRequest) {
    let adopt = Envelope::Authority(AuthorityMsg::AdoptPlayer {
        session: session.clone(),
        zone: req.to_zone,
        player: req.player.clone(),
        state: req.state.clone(),
        last_input_seq: req.last_input_seq,
        ammo_in_mag: 0,
        ammo_reserve: 0,
    });
    let topic_name = topic::authority(&session);
    match transport
        .request_envelope(&req.to_authority, &topic_name, &adopt, AUTHORITY_RPC_TIMEOUT_MS)
        .await
    {
        Ok(Envelope::Authority(AuthorityMsg::Adopted { entity, .. })) => {
            // Tell the client to re-home its input stream to the new authority. Seamless: the
            // client keeps predicting locally while it switches authorities under the hood.
            let redirect = Envelope::Server(ServerMsg::Redirect {
                zone: req.to_zone,
                authority: req.to_authority.clone(),
                entity,
                tick: 0,
            });
            let client_topic = topic::zone_state(&session, req.to_zone);
            if let Err(e) = transport.send_envelope(&req.player, &client_topic, &redirect).await {
                tracing::debug!(player = %req.player, error = %e, "redirect send failed");
            }
        }
        Ok(_) => tracing::warn!(player = %req.player, "hand-off got an unexpected reply"),
        Err(e) => tracing::warn!(player = %req.player, to = %req.to_authority, error = %e, "hand-off adopt failed"),
    }
}

/// Forward a join to the remote authority that owns the spawn zone, then reply JoinAccept.
/// Runs detached so the tick loop is never blocked awaiting a peer.
#[allow(clippy::too_many_arguments)]
async fn forward_join(
    transport: MeshTransport,
    session: SessionId,
    map: MapId,
    authority: NodeId,
    zone: ZoneId,
    team: Team,
    player: NodeId,
    reply: oneshot::Sender<Envelope>,
    now_ms: u64,
) {
    // A minimal carried state at the zone centre; the remote authority spawns the player on
    // its own spawn point (carried-position seeding is the tracked seed_player refinement).
    let mut flags = EntityFlags::default();
    flags.set(EntityFlags::ON_GROUND, true);
    let state = EntityState {
        id: 0,
        kind: EntityKind::Player,
        pos: zone.center(),
        vel: glam::Vec3::ZERO,
        yaw: 0.0,
        pitch: 0.0,
        flags,
        team,
        health: 100,
        armor: 0,
        weapon: 0,
        owner: player.clone(),
    };
    let adopt = Envelope::Authority(AuthorityMsg::AdoptPlayer {
        session: session.clone(),
        zone,
        player: player.clone(),
        state,
        last_input_seq: 0,
        ammo_in_mag: 0,
        ammo_reserve: 0,
    });
    let topic_name = topic::authority(&session);

    let reply_env = match transport
        .request_envelope(&authority, &topic_name, &adopt, AUTHORITY_RPC_TIMEOUT_MS)
        .await
    {
        Ok(Envelope::Authority(AuthorityMsg::Adopted { entity, .. })) => Envelope::Server(ServerMsg::JoinAccept {
            protocol: PROTOCOL_VERSION,
            entity,
            zone,
            authority,
            tick: 0,
            server_time_ms: now_ms,
            loadout: default_loadout(),
            spawn: SpawnPoint { pos: zone.center(), yaw: 0.0, team },
            map,
        }),
        _ => Envelope::Server(ServerMsg::JoinReject {
            reason: "spawn-zone authority unreachable".into(),
        }),
    };
    let _ = reply.send(reply_env);
}

/// Decode a [`ContentVersion`] announcement and fetch+verify its pack blob. Returns
/// `(epoch, pack)` ready to stage, or `None` on any failure (logged).
async fn fetch_announced_pack(ce: &ce_rs::CeClient, m: &ce_rs::AppMessage) -> Option<(u64, ContentPack)> {
    let bytes = m.payload().ok()?;
    let version: ContentVersion = decode(&bytes).ok()?;
    match ce.get_blob(&version.pack_hash).await {
        Ok(blob) => match ContentPack::decode_verified(&blob, &version.pack_hash) {
            Ok(pack) => Some((version.epoch, pack)),
            Err(e) => {
                tracing::warn!(error = %e, "announced content pack failed verification");
                None
            }
        },
        Err(e) => {
            tracing::warn!(hash = %version.pack_hash, error = %e, "could not fetch announced content pack");
            None
        }
    }
}
