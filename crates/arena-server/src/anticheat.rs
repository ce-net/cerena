//! [`AntiCheat`]: the node-local anti-cheat surface, defending *both* halves of the
//! threat model (see `arena-karma`):
//!
//! - **Cheating clients** — statistical telemetry ([`CheatDetector`]) plus karma-weighted
//!   player [`Report`]s ([`ReportAggregator`]). Neither bans on its own; the coordinator's
//!   [`KarmaLedger`](arena_karma::KarmaLedger) fuses them.
//! - **Cheating authorities** — the nasty one. A staked node can win a zone and then
//!   *simulate it dishonestly*. Client telemetry is useless there because the authority
//!   produces it. The defense is redundant re-simulation: when this node receives a
//!   [`VerifyTick`](AuthorityMsg::VerifyTick) it **shadow-replays** the claimed inputs against
//!   a fresh [`World`] built from the same content + geometry and compares the post-tick state
//!   hash. A disagreement is a vote against the authority ([`CrossValidator`]).
//!
//! This struct only does the *local* computation (observe, replay, tally). Turning a
//! [`Verdict`] into a karma penalty + a slash/reassign recommendation is the coordinator's
//! job (it holds the ledger and the broadcast topics).

use std::collections::HashSet;

use arena_karma::{CheatDetector, CrossValidator, ReportAggregator, SuspicionScore, Verdict};

use arena_protocol::input::InputFrame;
use arena_protocol::karma::{CheatTelemetry, Report};
use arena_protocol::message::AuthorityMsg;
use arena_protocol::world::{Team, ZoneId};
use arena_protocol::{NodeId, Tick};

use arena_content::registry::ContentRegistry;
use arena_content::ContentPack;

use arena_sim::World;

use crate::zone::build_zone_geometry;

/// Verifier sample size we assume per cross-validated tick. Drives the dispute quorum
/// (a 2/3 supermajority must disagree); floored at [`arena_karma::crossval::MIN_VERIFIERS`].
pub const VERIFIER_SAMPLE: usize = 3;

/// The per-node anti-cheat state.
pub struct AntiCheat {
    /// This node's id — stamped into the [`VerifyResult`](AuthorityMsg::VerifyResult) votes it
    /// casts as a shadow verifier.
    node_id: NodeId,
    /// Statistical client anti-cheat over per-round telemetry.
    detector: CheatDetector,
    /// Karma-weighted, decaying player-report aggregator.
    reports: ReportAggregator,
    /// Authority cross-validation tally (claims + verifier votes → verdicts).
    crossval: CrossValidator,
    /// Authorities we have seen claims from, so we can poll their verdicts in
    /// [`resolve_disputes`](Self::resolve_disputes) (the validator does not enumerate them).
    authorities: HashSet<NodeId>,
    /// The active content pack, used to build shadow simulations for verification. Kept in
    /// sync with the live sim by [`stage_content`](Self::stage_content).
    pack: ContentPack,
    /// Active content epoch (matches `pack`).
    epoch: u64,
}

impl AntiCheat {
    /// Build the anti-cheat state for `node_id` against `pack` at `epoch`.
    pub fn new(node_id: NodeId, pack: ContentPack, epoch: u64) -> Self {
        Self {
            node_id,
            detector: CheatDetector::new(),
            reports: ReportAggregator::new(),
            crossval: CrossValidator::new(VERIFIER_SAMPLE),
            authorities: HashSet::new(),
            pack,
            epoch,
        }
    }

    /// Ingest one round of a client's anti-cheat telemetry (the sim's counters, already
    /// converted to the wire [`CheatTelemetry`] by the zone). `player` is redundant with
    /// `telemetry.player` and kept only for log clarity.
    pub fn observe_round(&mut self, player: &NodeId, telemetry: CheatTelemetry) {
        tracing::trace!(player = %player, acc = telemetry.accuracy(), "anti-cheat round observed");
        self.detector.observe(&telemetry);
    }

    /// The detector's current suspicion score for a player (for the coordinator's fusion).
    pub fn suspicion(&self, player: &NodeId) -> Option<SuspicionScore> {
        self.detector.evaluate(player)
    }

    /// Decayed, karma-weighted report pressure on a player as of `now_tick`.
    pub fn report_pressure(&self, accused: &NodeId, now_tick: Tick) -> f32 {
        self.reports.pressure(accused, now_tick)
    }

    /// File a player report, weighted by the reporter's current karma. Returns `true` if it
    /// carried any weight (a quarantined reporter's report is recorded but counts for ~0).
    pub fn on_report(&mut self, report: &Report, reporter_karma: i32) -> bool {
        self.reports.file(report, reporter_karma)
    }

    /// Shadow-replay a claimed tick and vote on whether the authority's result matches.
    ///
    /// This is the catch for a malicious *authority*. We rebuild a fresh world from the same
    /// content + procedural geometry, spawn the players named in the claimed inputs, apply
    /// those inputs, advance one tick, and compare our post-tick [`World::state_hash`] to the
    /// authority's claimed hash. (The sim quantises floats in its hash so honest cross-machine
    /// results agree; a lying authority's fabricated hash will not.)
    ///
    /// NOTE: a *faithful* shadow needs the authority's pre-tick baseline state, which the
    /// `VerifyTick` message does not carry — a true verifier runs its own continuous shadow of
    /// the zone. This single-tick replay is the structural mechanism; wiring the baseline is a
    /// refinement tracked for `arena-sim::seed_player`.
    pub fn on_verify_tick(
        &mut self,
        zone: ZoneId,
        tick: Tick,
        claimed_hash: [u8; 32],
        inputs: Vec<(NodeId, InputFrame)>,
        authority: NodeId,
    ) -> AuthorityMsg {
        // Record the authority's claim so this node (if it is also the tally point) can later
        // resolve the (zone, tick) once a quorum of votes arrives.
        self.authorities.insert(authority.clone());
        self.crossval
            .record_claim(zone, tick, authority, claimed_hash);

        let our_hash = self.shadow_hash(zone, &inputs);
        let agree = our_hash == claimed_hash;

        AuthorityMsg::VerifyResult {
            zone,
            tick,
            verifier: self.node_id.clone(),
            agree,
            their_hash: our_hash,
        }
    }

    /// Record *our own* authority claim for a (zone, tick) so this node, acting as the tally
    /// point, counts the verifier votes that arrive for it. Called when we publish a
    /// [`VerifyTick`](AuthorityMsg::VerifyTick) for a zone we own.
    pub fn record_own_claim(&mut self, zone: ZoneId, tick: Tick, hash: [u8; 32]) {
        self.authorities.insert(self.node_id.clone());
        self.crossval.record_claim(zone, tick, self.node_id.clone(), hash);
    }

    /// Compute the post-tick state hash of a shadow replay of `inputs` in `zone`. Exposed for
    /// tests and reused by [`on_verify_tick`](Self::on_verify_tick).
    pub fn shadow_hash(&self, zone: ZoneId, inputs: &[(NodeId, InputFrame)]) -> [u8; 32] {
        let geometry = build_zone_geometry(&self.pack.worldgen, zone);
        let content = ContentRegistry::new(self.epoch, self.pack.clone())
            .unwrap_or_else(|_| ContentRegistry::bootstrap());
        let mut world = World::new(geometry, content);
        // Spawn each input's player deterministically (order follows the slice) and feed its
        // frame, so the same inputs build the same world on every honest verifier.
        for (node, frame) in inputs {
            let entity = world.spawn_player(node.clone(), Team::None);
            world.set_input(entity, *frame);
        }
        world.tick();
        world.state_hash()
    }

    /// Record a verifier's vote ([`VerifyResult`](AuthorityMsg::VerifyResult)) and, if the
    /// (zone, tick) now has a quorum of votes, resolve it. Returns the resulting [`Verdict`]
    /// when a decision was reached (it may recommend reassigning or slashing the authority).
    pub fn on_verify_result(&mut self, msg: &AuthorityMsg) -> Option<Verdict> {
        let AuthorityMsg::VerifyResult { zone, tick, .. } = msg else {
            return None;
        };
        let (zone, tick) = (*zone, *tick);
        self.crossval.record_vote(msg);
        self.crossval.resolve(zone, tick)
    }

    /// Current verdicts for every authority we are tracking that has at least one disputed
    /// tick. The coordinator turns `recommend_reassign` / `recommend_slash` into action.
    pub fn resolve_disputes(&self) -> Vec<Verdict> {
        self.authorities
            .iter()
            .map(|a| self.crossval.verdict_for(a))
            .filter(|v| !v.disputed_ticks.is_empty())
            .collect()
    }

    /// Keep the shadow-replay content in lock-step with the live sim after a hot-reload.
    pub fn stage_content(&mut self, epoch: u64, pack: ContentPack) {
        self.epoch = epoch;
        self.pack = pack;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::input::{Buttons, InputFrame};

    fn frame(seq: u32) -> InputFrame {
        InputFrame {
            seq,
            client_tick: seq,
            buttons: Buttons::default(),
            yaw: 0.1,
            pitch: 0.0,
            weapon_slot: 0,
        }
    }

    /// An honest replay agrees with the authority's hash; a tampered hash (what an
    /// `e2e_cheat` malicious authority would publish) is caught as a disagreement.
    #[test]
    fn shadow_replay_agrees_then_disagrees_on_tamper() {
        let pack = arena_content::default_pack();
        let mut ac = AntiCheat::new("verifier".into(), pack.clone(), 1);
        let zone = ZoneId::new(0, 0);
        let inputs = vec![("p1".to_string(), frame(1)), ("p2".to_string(), frame(1))];

        // The honest authority would publish exactly the hash an identical replay produces.
        let honest = ac.shadow_hash(zone, &inputs);
        let res = ac.on_verify_tick(zone, 10, honest, inputs.clone(), "authority".into());
        match res {
            AuthorityMsg::VerifyResult { agree, .. } => {
                assert!(agree, "an honest tick must verify as agreeing");
            }
            _ => panic!("expected a VerifyResult"),
        }

        // A malicious authority flips a byte of the hash; verifiers must disagree.
        let mut tampered = honest;
        tampered[0] ^= 0xFF;
        let res = ac.on_verify_tick(zone, 11, tampered, inputs, "authority".into());
        match res {
            AuthorityMsg::VerifyResult { agree, .. } => {
                assert!(!agree, "a tampered (cheating-authority) hash must be caught");
            }
            _ => panic!("expected a VerifyResult"),
        }
    }
}
