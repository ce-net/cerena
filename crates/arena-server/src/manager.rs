//! [`ZoneManager`]: decides which zones *this* node owns and runs their [`ZoneSim`]s.
//!
//! Ownership is computed, not assigned: every node runs the same stake-weighted
//! rendezvous hash ([`arena_mesh::assign_authority`]) over the discovered candidate set, so
//! they all agree on who owns each zone with no central directory. This manager:
//!
//! - refreshes its candidate set from discovery ([`update_candidates`](ZoneManager::update_candidates)),
//! - claims the zones it should own and retires the ones it should not
//!   ([`reconcile_ownership`](ZoneManager::reconcile_ownership)), broadcasting
//!   [`AuthorityClaim`](AuthorityMsg::AuthorityClaim)s with a monotonic epoch,
//! - steps every owned zone each tick and hands players off across zone boundaries
//!   ([`tick_all`](ZoneManager::tick_all)),
//! - adopts players handed to it by a neighbouring authority
//!   ([`adopt_player`](ZoneManager::adopt_player)).
//!
//! ## Content sharing
//!
//! [`arena_content::registry::ContentRegistry`] is not `Clone`, but [`ContentPack`] is. The
//! manager therefore holds the active pack + epoch and mints a fresh registry per zone when
//! it creates a [`ZoneSim`]. A hot-reload restages the pack into every owned zone and updates
//! the held copy so newly-created zones start on the new epoch.

use std::collections::{HashMap, HashSet};

use glam::Vec3;

use arena_mesh::{assign_authority, Candidate, Envelope, MeshTransport, ZoneRouter};

use arena_protocol::auth::SessionId;
use arena_protocol::entity::EntityState;
use arena_protocol::input::InputBatch;
use arena_protocol::message::{topic, AuthorityMsg};
use arena_protocol::world::{Team, ZoneId};
use arena_protocol::NodeId;

use arena_content::registry::ContentRegistry;
use arena_content::ContentPack;

use crate::anticheat::AntiCheat;
use crate::coordinator::{QUARANTINE_ZONE, SPAWN_ZONE};
use crate::zone::{build_zone_geometry, ZoneSim};

/// A pending cross-node player hand-off: as a player crosses into a zone owned by a
/// *different* node, the engine sends an [`AdoptPlayer`](AuthorityMsg::AdoptPlayer) to that
/// node and, on `Adopted`, redirects the client. This is the seamless boundary crossing.
#[derive(Debug, Clone)]
pub struct HandoffRequest {
    pub player: NodeId,
    pub from_zone: ZoneId,
    pub to_zone: ZoneId,
    pub to_authority: NodeId,
    /// The player's last authoritative state, carried so prediction stays seamless.
    pub state: EntityState,
    pub last_input_seq: u32,
}

/// Owns this node's zone authorities for one session.
pub struct ZoneManager {
    /// This node's id.
    me: NodeId,
    /// The session whose zones we (may) own.
    session: SessionId,
    /// Stake-weighted authority router over the current candidate set.
    router: ZoneRouter,
    /// Zones this node is currently authoritative for.
    owned: HashMap<ZoneId, ZoneSim>,
    /// Active content pack (source for new zones + restaging on hot-reload).
    pack: ContentPack,
    /// Active content epoch.
    epoch: u64,
    /// Monotonic authority-lease epoch, bumped on every claim so conflicts resolve by
    /// highest epoch (independent of the content epoch).
    claim_epoch: u64,
    /// The session's active zones (those worth reconciling): a seeded core region plus any
    /// zone a player has entered. Empty zones outside this set are not pre-created.
    active_zones: HashSet<ZoneId>,
    /// Highest authority-claim epoch we have seen per zone from peers, so a higher peer epoch
    /// makes us yield a zone we hold.
    peer_claims: HashMap<ZoneId, (NodeId, u64)>,
}

impl ZoneManager {
    /// Build a manager for `me` in `session`, starting on `pack` at `epoch`. The candidate
    /// set starts empty (a freshly-booted node owns everything until discovery runs).
    pub fn new(me: NodeId, session: SessionId, pack: ContentPack, epoch: u64) -> Self {
        let router = ZoneRouter::new(session.clone(), Vec::new());
        // Seed the active region with the spawn + quarantine cells and the spawn cell's ring,
        // so a multi-node fleet distributes the opening region immediately.
        let mut active_zones: HashSet<ZoneId> = SPAWN_ZONE.aoi(1).into_iter().collect();
        active_zones.insert(QUARANTINE_ZONE);
        Self {
            me,
            session,
            router,
            owned: HashMap::new(),
            pack,
            epoch,
            claim_epoch: 0,
            active_zones,
            peer_claims: HashMap::new(),
        }
    }

    /// This node's id.
    pub fn node_id(&self) -> &NodeId {
        &self.me
    }

    /// Number of zones currently owned (diagnostics / e2e status).
    pub fn owned_zone_count(&self) -> usize {
        self.owned.len()
    }

    /// Total players across all owned zones (diagnostics / e2e status).
    pub fn total_players(&self) -> usize {
        self.owned.values().map(|z| z.player_count()).sum()
    }

    /// Swap in a freshly-discovered candidate set (call on a timer from discovery).
    pub fn update_candidates(&mut self, candidates: Vec<Candidate>) {
        self.router.update_candidates(candidates);
    }

    /// Whether this node should own `zone` under the current candidate set.
    pub fn should_own(&self, zone: ZoneId) -> bool {
        self.router.authority_for(zone).as_ref() == Some(&self.me)
    }

    /// Get (creating if absent) the [`ZoneSim`] for a zone, marking it active. Used when a
    /// player joins/hands into a zone we own.
    pub fn ensure_zone(&mut self, zone: ZoneId) -> &mut ZoneSim {
        self.active_zones.insert(zone);
        let (pack, epoch, session) = (&self.pack, self.epoch, &self.session);
        self.owned.entry(zone).or_insert_with(|| {
            let content = ContentRegistry::new(epoch, pack.clone())
                .unwrap_or_else(|_| ContentRegistry::bootstrap());
            let geometry = build_zone_geometry(&pack.worldgen, zone);
            ZoneSim::new(zone, session.clone(), content, geometry)
        })
    }

    /// Mutable access to an owned zone, if we own it.
    pub fn zone_mut(&mut self, zone: ZoneId) -> Option<&mut ZoneSim> {
        self.owned.get_mut(&zone)
    }

    /// The node currently authoritative for `zone` under the live candidate set.
    pub fn authority_for(&self, zone: ZoneId) -> Option<NodeId> {
        self.router.authority_for(zone)
    }

    /// Every player node hosted across all owned zones (for the coordinator's karma pass).
    pub fn all_player_nodes(&self) -> Vec<NodeId> {
        self.owned.values().flat_map(|z| z.player_nodes()).collect()
    }

    /// Remove a player from whichever owned zone hosts them (graceful leave / kick). Returns
    /// the player's carried state if they were hosted.
    pub fn remove_player(&mut self, node: &NodeId) -> Option<(EntityState, u32)> {
        let zid = self.zone_of_player(node)?;
        self.owned.get_mut(&zid).and_then(|z| z.remove_player(node))
    }

    /// Find the zone hosting `node` (a player is in at most one owned zone at a time).
    fn zone_of_player(&self, node: &NodeId) -> Option<ZoneId> {
        self.owned
            .iter()
            .find(|(_, z)| z.has_player(node))
            .map(|(zid, _)| *zid)
    }

    /// Route a client input batch to the zone currently hosting that player.
    pub fn queue_input(&mut self, node: &NodeId, batch: InputBatch) {
        if let Some(zid) = self.zone_of_player(node) {
            if let Some(sim) = self.owned.get_mut(&zid) {
                sim.queue_input(node, batch);
            }
        }
    }

    /// Adopt a player handed to us by a neighbouring authority. Creates/locates the target
    /// zone and spawns the player on its team, replying [`Adopted`](AuthorityMsg::Adopted)
    /// with the new (per-zone) entity id.
    ///
    /// LIMITATION: `World` has no "place at carried state" entry point yet, so the adopted
    /// player respawns at a spawn point rather than at its exact carried position/health.
    /// Seamless carry-over is tracked for `arena_sim::seed_player(state)`; the carried `state`
    /// is accepted here so the wire contract is already correct.
    pub fn adopt_player(&mut self, msg: &AuthorityMsg) -> Option<AuthorityMsg> {
        let AuthorityMsg::AdoptPlayer { zone, player, state, .. } = msg else {
            return None;
        };
        let team = state.team;
        let sim = self.ensure_zone(*zone);
        let (entity, _spawn) = sim.add_player(player.clone(), team);
        Some(AuthorityMsg::Adopted { player: player.clone(), entity })
    }

    /// Record (or react to) a peer's authority claim. If a peer claims a zone we currently
    /// hold with a *higher* epoch, we yield it: retire the zone and return hand-off requests
    /// for its players so they migrate to the new owner.
    pub fn on_authority_claim(&mut self, msg: &AuthorityMsg) -> Vec<HandoffRequest> {
        let AuthorityMsg::AuthorityClaim { zone, claimant, epoch, .. } = msg else {
            return Vec::new();
        };
        if claimant == &self.me {
            return Vec::new();
        }
        let prev = self.peer_claims.get(zone).map(|(_, e)| *e).unwrap_or(0);
        if *epoch <= prev {
            return Vec::new();
        }
        self.peer_claims.insert(*zone, (claimant.clone(), *epoch));

        // If we hold this zone and the claim outranks our lease, hand it over.
        if self.owned.contains_key(zone) && *epoch > self.claim_epoch {
            tracing::info!(zone = %zone.token(), new_owner = %claimant, epoch, "yielding zone to higher-epoch claim");
            return self.retire_zone(*zone, claimant.clone());
        }
        Vec::new()
    }

    /// Reconcile ownership against the current candidate set: claim zones we should own and
    /// retire zones we no longer should. Returns hand-off requests for retired zones' players.
    /// Broadcasts an [`AuthorityClaim`](AuthorityMsg::AuthorityClaim) for each newly-claimed
    /// zone so peers converge.
    pub async fn reconcile_ownership(&mut self, transport: &MeshTransport) -> Vec<HandoffRequest> {
        // Consider every active zone plus everything we currently hold.
        let mut zones: HashSet<ZoneId> = self.active_zones.clone();
        zones.extend(self.owned.keys().copied());

        let mut handoffs = Vec::new();
        for zone in zones {
            let should = self.should_own(zone);
            let have = self.owned.contains_key(&zone);
            if should && !have {
                // Claim it: create the sim and announce the claim.
                self.ensure_zone(zone);
                self.claim_epoch += 1;
                self.broadcast_claim(transport, zone).await;
            } else if !should && have {
                // We no longer own it; hand its players to the new owner and drop the sim.
                let new_owner = self.router.authority_for(zone).unwrap_or_else(|| self.me.clone());
                handoffs.extend(self.retire_zone(zone, new_owner));
            }
        }
        handoffs
    }

    /// Tear down an owned zone, returning hand-off requests for each of its players toward
    /// `new_owner`.
    fn retire_zone(&mut self, zone: ZoneId, new_owner: NodeId) -> Vec<HandoffRequest> {
        let mut handoffs = Vec::new();
        let Some(mut sim) = self.owned.remove(&zone) else {
            return handoffs;
        };
        let players: Vec<NodeId> = sim.player_positions().into_iter().map(|(n, _)| n).collect();
        for player in players {
            if let Some((state, last_input_seq)) = sim.remove_player(&player) {
                handoffs.push(HandoffRequest {
                    player,
                    from_zone: zone,
                    to_zone: zone,
                    to_authority: new_owner.clone(),
                    state,
                    last_input_seq,
                });
            }
        }
        handoffs
    }

    /// Step every owned zone one tick, then detect players who have crossed into a zone owned
    /// by a **different** node and emit hand-off requests for them (removing them locally so
    /// they are not double-simulated).
    ///
    /// Same-node zone crossings are intentionally *not* handed off: a single node owning
    /// neighbouring cells keeps simulating the player in its current [`ZoneSim`] (the zone's
    /// border overlap tolerates this), which both avoids needless churn and side-steps the
    /// missing carried-state entry point. Only true cross-authority crossings migrate.
    pub async fn tick_all(
        &mut self,
        transport: &MeshTransport,
        anticheat: &mut AntiCheat,
        now_ms: u64,
    ) -> Vec<HandoffRequest> {
        let zone_ids: Vec<ZoneId> = self.owned.keys().copied().collect();

        // 1. Advance each zone (sends its AOI snapshots internally).
        for zid in &zone_ids {
            if let Some(sim) = self.owned.get_mut(zid) {
                sim.step(transport, anticheat, now_ms).await;
            }
        }

        // 2. Detect cross-node boundary crossings (immutable scan first to satisfy the borrow
        //    checker), then migrate.
        let mut crossings: Vec<(ZoneId, NodeId, ZoneId, NodeId)> = Vec::new();
        for zid in &zone_ids {
            let Some(sim) = self.owned.get(zid) else { continue };
            for (player, pos) in sim.player_positions() {
                let to_zone = ZoneId::from_world(pos);
                if to_zone == *zid {
                    continue; // still in their zone interior
                }
                match self.router.authority_for(to_zone) {
                    Some(owner) if owner != self.me => {
                        crossings.push((*zid, player, to_zone, owner));
                    }
                    // Owned by us (or nobody) → keep simulating here (see method doc).
                    _ => {}
                }
            }
        }

        let mut handoffs = Vec::new();
        for (from_zone, player, to_zone, to_authority) in crossings {
            if let Some(sim) = self.owned.get_mut(&from_zone) {
                if let Some((state, last_input_seq)) = sim.remove_player(&player) {
                    handoffs.push(HandoffRequest {
                        player,
                        from_zone,
                        to_zone,
                        to_authority,
                        state,
                        last_input_seq,
                    });
                }
            }
        }
        handoffs
    }

    /// A comparable state report per owned zone, for convergence / cross-validation logging.
    pub fn state_reports(&self) -> Vec<(String, arena_protocol::Tick, [u8; 32])> {
        self.owned.values().map(|z| z.state_report()).collect()
    }

    /// The inputs each owned zone applied this tick, for emitting [`VerifyTick`]s.
    pub fn verify_payloads(
        &self,
    ) -> Vec<(ZoneId, arena_protocol::Tick, [u8; 32], Vec<(NodeId, arena_protocol::input::InputFrame)>)> {
        self.owned
            .values()
            .map(|z| {
                let (_, tick, hash) = z.state_report();
                (z.zone, tick, hash, z.applied_inputs())
            })
            .collect()
    }

    /// Restage a new content pack into every owned zone and update the held copy so future
    /// zones start on the new epoch.
    pub fn stage_content(&mut self, epoch: u64, pack: ContentPack) {
        self.epoch = epoch;
        self.pack = pack.clone();
        for sim in self.owned.values_mut() {
            sim.stage_content(epoch, pack.clone());
        }
    }

    /// The stake `me` has bonded according to the current candidate set (0 if unknown).
    fn my_stake(&self) -> i128 {
        self.router
            .candidates()
            .iter()
            .find(|c| c.node == self.me)
            .map(|c| c.stake_base_units)
            .unwrap_or(0)
    }

    /// Broadcast an [`AuthorityClaim`](AuthorityMsg::AuthorityClaim) for a zone we just claimed.
    async fn broadcast_claim(&self, transport: &MeshTransport, zone: ZoneId) {
        let env = Envelope::Authority(AuthorityMsg::AuthorityClaim {
            session: self.session.clone(),
            zone,
            claimant: self.me.clone(),
            epoch: self.claim_epoch,
            stake_base_units: self.my_stake(),
        });
        let topic_name = topic::authority(&self.session);
        if let Err(e) = transport.publish_envelope(&topic_name, &env).await {
            tracing::warn!(zone = %zone.token(), error = %e, "authority claim broadcast failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_own_matches_assign_authority() {
        let session = SessionId("match-test".into());
        let me: NodeId = "node-a".into();
        let mut mgr = ZoneManager::new(me.clone(), session.clone(), arena_content::default_pack(), 1);

        let candidates = vec![
            Candidate::new("node-a", 0),
            Candidate::new("node-b", 0),
            Candidate::new("node-c", 0),
        ];
        mgr.update_candidates(candidates.clone());

        // The manager's ownership decision must agree with the pure HRW assignment for every
        // zone — deterministic, directory-free, identical on every node.
        for x in 0..8 {
            for z in 0..8 {
                let zone = ZoneId::new(x, z);
                let expected = assign_authority(&session, zone, &candidates).as_ref() == Some(&me);
                assert_eq!(mgr.should_own(zone), expected, "mismatch at {zone:?}");
            }
        }
    }

    #[test]
    fn adopt_player_creates_zone_and_assigns_entity() {
        let session = SessionId("s".into());
        let mut mgr = ZoneManager::new("me".into(), session, arena_content::default_pack(), 1);
        let state = EntityState {
            id: 0,
            kind: arena_protocol::entity::EntityKind::Player,
            pos: Vec3::ZERO,
            vel: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            flags: Default::default(),
            team: Team::Red,
            health: 100,
            armor: 0,
            weapon: 0,
            owner: "p1".into(),
        };
        let msg = AuthorityMsg::AdoptPlayer {
            session: SessionId("s".into()),
            zone: ZoneId::new(2, 3),
            player: "p1".into(),
            state,
            last_input_seq: 7,
            ammo_in_mag: 0,
            ammo_reserve: 0,
        };
        let reply = mgr.adopt_player(&msg).expect("adopt replies");
        assert!(matches!(reply, AuthorityMsg::Adopted { .. }));
        assert!(mgr.zone_mut(ZoneId::new(2, 3)).is_some(), "adopt creates the target zone");
    }
}
