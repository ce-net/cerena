//! [`Coordinator`]: the session's matchmaker, content publisher, and karma authority.
//!
//! Exactly one node per session runs a coordinator (the rest are pure zone authorities).
//! It owns the three session-wide policy decisions that must *not* be made per-zone:
//!
//! 1. **Admission** — verify a joining player's [`SessionTicket`], check their karma band,
//!    and assign them a spawn zone + the node currently authoritative for it.
//! 2. **Content** — publish a hot-reloadable [`ContentVersion`] (monotonic epoch + pack
//!    hash) so the whole fleet swaps to new game data at a tick boundary.
//! 3. **Karma** — fuse anti-cheat suspicion with report pressure into a durable
//!    [`KarmaLedger`] and turn authority cross-validation [`Verdict`]s into penalties.
//!
//! The coordinator holds no `World` and does no async I/O itself — it returns decisions
//! and records the engine then carries over the mesh. That keeps it deterministic and unit
//! testable.

use arena_karma::{KarmaLedger, SuspicionScore, Verdict};

use arena_protocol::auth::{SessionId, SessionTicket};
use arena_protocol::karma::{KarmaAction, KarmaUpdate};
use arena_protocol::world::{MapId, Team, ZoneId};
use arena_protocol::{NodeId, Tick};

use arena_content::hotreload::ContentVersion;
use arena_content::ContentPack;

use arena_mesh::verify_ticket;

/// The zone new (non-quarantined) players spawn into — the session's central arena cell.
pub const SPAWN_ZONE: ZoneId = ZoneId { x: 0, z: 0 };

/// A distant zone reserved for quarantined (low-karma) players, so suspected cheats are
/// matched with each other rather than dropped into honest games.
pub const QUARANTINE_ZONE: ZoneId = ZoneId { x: 10_000, z: 10_000 };

/// The outcome of an admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinDecision {
    /// Refuse the join with a human-readable reason (bad ticket / banned).
    Reject(String),
    /// Admit the player: spawn them in `zone`, currently owned by `authority`, on `team`.
    /// The engine compares `authority` to its own id to decide local-add vs cross-node
    /// forward.
    Accept {
        zone: ZoneId,
        authority: NodeId,
        team: Team,
    },
}

/// Session matchmaking + content + karma authority.
pub struct Coordinator {
    session: SessionId,
    /// Content-addressed map label reported to clients.
    map: MapId,
    /// TEST ONLY: skip the ticket signature gate (see [`crate::ServerConfig::e2e_insecure`]).
    e2e_insecure: bool,
    /// The durable karma store: balances + an append-only audit log.
    ledger: KarmaLedger,
    /// The content version currently announced to the fleet.
    version: ContentVersion,
}

impl Coordinator {
    /// Build a coordinator for `session`, starting at content `epoch` with `pack_hash`.
    pub fn new(
        session: SessionId,
        map: MapId,
        e2e_insecure: bool,
        node_id: NodeId,
        epoch: u64,
        pack_hash: String,
    ) -> Self {
        let version = ContentVersion {
            epoch,
            pack_hash,
            label: "cerena-initial".to_string(),
            issuer: node_id,
            apply_at_tick: 0,
        };
        Self {
            session,
            map,
            e2e_insecure,
            ledger: KarmaLedger::new(),
            version,
        }
    }

    /// Read-only access to the karma ledger (for the engine's fusion + broadcasts).
    pub fn ledger(&self) -> &KarmaLedger {
        &self.ledger
    }

    /// The map label admitted players are told to load.
    pub fn map(&self) -> &MapId {
        &self.map
    }

    /// Decide whether to admit a joining player and where to put them.
    ///
    /// `from` is the **authenticated** sender node id; `authority_for` answers which node
    /// currently owns a given zone (the engine supplies the manager's HRW router without
    /// exposing it); `me` is this node's id (the authority fallback when discovery has not yet
    /// populated any candidates).
    pub fn handle_join<F>(
        &mut self,
        ticket: &SessionTicket,
        team_pref: Option<Team>,
        from: &NodeId,
        me: &NodeId,
        now_unix: u64,
        authority_for: F,
    ) -> JoinDecision
    where
        F: Fn(ZoneId) -> Option<NodeId>,
    {
        // 1. Ticket: verified unless explicitly relaxed for local e2e fleets.
        if !self.e2e_insecure {
            if let Err(e) = verify_ticket(ticket, now_unix) {
                return JoinDecision::Reject(format!("invalid session ticket: {e}"));
            }
            // The ticket must admit *this* session and the player it was issued to.
            if ticket.session != self.session {
                return JoinDecision::Reject("ticket is for a different session".to_string());
            }
            if &ticket.player != from {
                return JoinDecision::Reject("ticket player does not match sender".to_string());
            }
        }

        // 2. Karma band gates admission and the spawn pool.
        let action = self.ledger.action(from);
        let zone = match action {
            KarmaAction::PermBan => {
                return JoinDecision::Reject("permanently banned".to_string());
            }
            KarmaAction::TempBan => {
                return JoinDecision::Reject("temporarily banned (low karma)".to_string());
            }
            // Suspected cheats play each other in the quarantine pool, never honest games.
            KarmaAction::Quarantine => QUARANTINE_ZONE,
            KarmaAction::None => SPAWN_ZONE,
        };

        // 3. Authority for the spawn zone (HRW). With no discovered candidates yet, we host
        //    it ourselves — a freshly-booted coordinator is also the bootstrap authority.
        let authority = authority_for(zone).unwrap_or_else(|| me.clone());
        let team = team_pref.unwrap_or(Team::None);

        JoinDecision::Accept { zone, authority, team }
    }

    /// Publish a new content pack: bump the epoch, recompute the announced version, and return
    /// it. The engine stores the pack as a blob and broadcasts this [`ContentVersion`]; peers
    /// fetch by `pack_hash` and stage at a tick boundary.
    pub fn publish_content(&mut self, pack: &ContentPack, label: impl Into<String>) -> ContentVersion {
        let epoch = self.version.epoch + 1;
        self.version = ContentVersion {
            epoch,
            pack_hash: pack.hash(),
            label: label.into(),
            issuer: self.version.issuer.clone(),
            apply_at_tick: 0,
        };
        self.version.clone()
    }

    /// The content version currently in force, e.g. to answer a freshly-joined peer's query.
    pub fn current_version(&self) -> ContentVersion {
        self.version.clone()
    }

    /// Fuse a player's detector suspicion and report pressure into a karma decision. Returns
    /// a [`KarmaUpdate`] worth broadcasting only when the band changed or the penalty was
    /// non-trivial (see [`KarmaLedger::fuse`]).
    pub fn fuse_player(
        &mut self,
        player: &NodeId,
        suspicion: Option<&SuspicionScore>,
        report_pressure: f32,
        now_unix: u64,
    ) -> Option<KarmaUpdate> {
        self.ledger.fuse(player, suspicion, report_pressure, now_unix)
    }

    /// Apply a cross-validation [`Verdict`] against a misbehaving authority: dock karma in
    /// proportion to the number of disputed ticks. Slashing the authority's bonded stake +
    /// reassigning the zone is handled by the engine (it owns the mesh + ce-gov path); this
    /// records the reputational half on the same scarce identity an honest cheat burns.
    pub fn apply_verdict(&mut self, verdict: &Verdict, now_unix: u64) -> Option<KarmaUpdate> {
        if verdict.disputed_ticks.is_empty() {
            return None;
        }
        // -20 karma per disputed tick; sustained dispute → straight into a ban band.
        let penalty = -(20 * verdict.disputed_ticks.len() as i32);
        let reason = format!(
            "authority cross-validation: {} disputed tick(s){}",
            verdict.disputed_ticks.len(),
            if verdict.recommend_slash { " (slash recommended)" } else { "" }
        );
        Some(self.ledger.apply(&verdict.authority, penalty, reason, now_unix))
    }

    /// Karma for a node (for weighting that node's reports).
    pub fn karma(&self, node: &NodeId) -> i32 {
        self.ledger.karma(node)
    }

    /// The enforcement band a node's karma lands it in (so a mid-session quarantine can be
    /// pushed to the client).
    pub fn action(&self, node: &NodeId) -> KarmaAction {
        self.ledger.action(node)
    }

    /// Ticks-since-epoch helper is the engine's concern; the coordinator never reads time.
    /// (Kept here so the field is documented as the policy clock the engine supplies.)
    pub fn now_marker(_tick: Tick) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_mesh::{assign_authority, Candidate};

    fn ticket(player: &str, session: &SessionId, expires_at: u64) -> SessionTicket {
        SessionTicket {
            player: player.to_string(),
            session: session.clone(),
            map: MapId("m".into()),
            expires_at,
            issuer: "11".repeat(32),
            sig: vec![0u8; 64],
            karma: 100,
        }
    }

    #[test]
    fn insecure_join_admits_to_spawn_zone() {
        let session = SessionId("s".into());
        let mut c = Coordinator::new(session.clone(), MapId("m".into()), true, "me".into(), 1, "hash".into());
        let router = ZoneRouter::new(session.clone(), vec![Candidate::new("me", 0)]);
        let t = ticket("player-a", &session, 0); // expired ticket, but e2e_insecure ignores it
        let decision = c.handle_join(&t, Some(Team::Red), &"player-a".into(), &router, &"me".into(), 1000);
        match decision {
            JoinDecision::Accept { zone, authority, team } => {
                assert_eq!(zone, SPAWN_ZONE);
                assert_eq!(authority, "me");
                assert_eq!(team, Team::Red);
            }
            other => panic!("expected Accept, got {other:?}"),
        }
    }

    #[test]
    fn banned_player_is_rejected() {
        let session = SessionId("s".into());
        let mut c = Coordinator::new(session.clone(), MapId("m".into()), true, "me".into(), 1, "hash".into());
        let router = ZoneRouter::new(session.clone(), vec![Candidate::new("me", 0)]);
        // Drive the player below the temp-ban line.
        c.ledger.apply(&"cheat".into(), -200, "test", 1);
        let t = ticket("cheat", &session, 0);
        let decision = c.handle_join(&t, None, &"cheat".into(), &router, &"me".into(), 1000);
        assert!(matches!(decision, JoinDecision::Reject(_)));
    }

    #[test]
    fn publish_content_bumps_epoch_monotonically() {
        let session = SessionId("s".into());
        let mut c = Coordinator::new(session, MapId("m".into()), false, "me".into(), 1, "h0".into());
        let pack = arena_content::default_pack();
        let v = c.publish_content(&pack, "next");
        assert_eq!(v.epoch, 2);
        assert_eq!(v.pack_hash, pack.hash());
        assert_eq!(c.current_version().epoch, 2);
    }
}
