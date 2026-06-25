//! Authority cross-validation — the defense against a *malicious zone authority*.
//!
//! A staked node can win an [`AuthorityClaim`] and become authoritative for a zone.
//! Nothing stops it from then *lying*: fabricating hits for a confederate, denying a
//! rival's shots, teleporting an ally. Client telemetry is useless here, because the
//! authority produces the telemetry.
//!
//! The countermeasure is redundant re-simulation. For sampled ticks, the authority
//! publishes a [`VerifyTick`] carrying the exact inputs it claims it applied and the
//! hash of its post-tick entity set. A sample of *other* authorities run a shadow
//! simulation of that same tick from those same inputs and reply with a
//! [`VerifyResult`]: do they agree, and what is their own result hash? Because a zone is
//! simulated by a single node and glam f32 is not bit-identical across machines, honest
//! verifiers may differ in the last ulp — so a verifier reports `agree` after comparing
//! within the sim's tolerance epsilon (that comparison happens in `arena-sim`; this
//! module only tallies the boolean verdicts and logs hashes for disputes).
//!
//! If a quorum of verifiers *disagree* with the authority's claimed hash, the tick is
//! `DISPUTED`. A single disputed tick triggers an immediate zone reassignment
//! recommendation (get a suspect authority off the zone fast, cheaply reversible if it
//! was a transient glitch). *Sustained* disputes — several across the session — escalate
//! to a slash recommendation.
//!
//! ## Ties to ce-gov and bonded stake
//!
//! Winning an authority lease requires bonding on-chain stake (see
//! [`AuthorityClaim::stake_base_units`]). A [`Verdict`] with `recommend_slash` feeds the
//! ce-gov slashing path: the bonded stake is burned/redistributed, the authority's
//! karma takes a hit through the same [`KarmaLedger`](crate::ledger::KarmaLedger) honest
//! clients use, and the zone is reassigned to the next rendezvous-hash candidate. The
//! economic cost (lost stake) plus the reputational cost (lost karma on a scarce
//! identity) is what makes running a malicious authority irrational.
//!
//! [`AuthorityClaim`]: arena_protocol::message::AuthorityMsg::AuthorityClaim
//! [`AuthorityClaim::stake_base_units`]: arena_protocol::message::AuthorityMsg::AuthorityClaim
//! [`VerifyTick`]: arena_protocol::message::AuthorityMsg::VerifyTick
//! [`VerifyResult`]: arena_protocol::message::AuthorityMsg::VerifyResult

use std::collections::BTreeSet;
use std::collections::HashMap;

use arena_protocol::message::AuthorityMsg;
use arena_protocol::world::ZoneId;
use arena_protocol::{NodeId, Tick};

/// Number of disputed ticks (across the session) at or above which we recommend
/// reassigning the zone away from the authority. One is enough — a suspect authority
/// should not keep simulating while under dispute, and reassignment is cheap/reversible.
pub const REASSIGN_DISPUTE_THRESHOLD: usize = 1;

/// Number of disputed ticks at or above which we recommend *slashing* the authority's
/// bonded stake. Higher than reassignment: slashing is destructive, so we require a
/// sustained pattern rather than a single epsilon disagreement.
pub const SLASH_DISPUTE_THRESHOLD: usize = 3;

/// The smallest verifier sample we will ever decide on, regardless of configured count.
/// Below this, one malicious verifier could swing a quorum.
pub const MIN_VERIFIERS: usize = 3;

/// Quorum size for a verifier sample of `n`: `ceil(2/3 * n)`. A 2/3 supermajority must
/// disagree to dispute a tick, tolerating up to ~1/3 faulty/malicious verifiers.
pub fn quorum(n: usize) -> usize {
    // ceil(2n/3) without floats.
    (2 * n + 2) / 3
}

/// The cross-validation verdict for one authority. Distributed to the session
/// coordinator and, on `recommend_slash`, to the ce-gov slashing path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// The authority under judgement.
    pub authority: NodeId,
    /// Ticks of this authority that a verifier quorum disputed, sorted ascending.
    pub disputed_ticks: Vec<Tick>,
    /// Sustained disputes → recommend burning the authority's bonded stake (ce-gov).
    pub recommend_slash: bool,
    /// Any dispute → recommend reassigning the zone to another candidate.
    pub recommend_reassign: bool,
}

/// One authority's claimed result for a (zone, tick).
#[derive(Debug, Clone)]
struct Claim {
    authority: NodeId,
    result_hash: [u8; 32],
}

/// Tally of verifier votes for a (zone, tick), deduplicated per verifier.
#[derive(Debug, Default)]
struct VoteTally {
    /// verifier -> agreed with the claimed hash.
    votes: HashMap<NodeId, bool>,
}

impl VoteTally {
    fn total(&self) -> usize {
        self.votes.len()
    }
    fn disagreements(&self) -> usize {
        self.votes.values().filter(|agreed| !**agreed).count()
    }
}

/// Collects authority claims and verifier votes, resolving disputes into [`Verdict`]s.
#[derive(Debug)]
pub struct CrossValidator {
    /// Expected number of verifiers sampled per tick. Drives the quorum threshold.
    verifier_sample: usize,
    /// (zone, tick) -> the authority's claimed result.
    claims: HashMap<(ZoneId, Tick), Claim>,
    /// (zone, tick) -> verifier vote tally.
    tallies: HashMap<(ZoneId, Tick), VoteTally>,
    /// authority -> the set of ticks of theirs that ended up disputed (session-cumulative).
    disputed_by_authority: HashMap<NodeId, BTreeSet<Tick>>,
    /// (zone, tick) keys already resolved, so a late vote doesn't double-count a dispute.
    resolved: BTreeSet<(ZoneId, Tick)>,
}

impl CrossValidator {
    /// Create a validator expecting `verifier_sample` shadow simulators per tick. The
    /// sample is floored at [`MIN_VERIFIERS`] for quorum-safety.
    pub fn new(verifier_sample: usize) -> Self {
        Self {
            verifier_sample: verifier_sample.max(MIN_VERIFIERS),
            claims: HashMap::new(),
            tallies: HashMap::new(),
            disputed_by_authority: HashMap::new(),
            resolved: BTreeSet::new(),
        }
    }

    /// The quorum (number of disagreeing verifiers) needed to dispute a tick.
    pub fn quorum_size(&self) -> usize {
        quorum(self.verifier_sample)
    }

    /// Record the authority's own claim for a (zone, tick): the post-tick result hash it
    /// published in its [`VerifyTick`]. Idempotent per key (last write wins; an authority
    /// should not re-publish a different hash for the same tick — if it does, the latest
    /// is what verifiers were asked to check).
    pub fn record_claim(&mut self, zone: ZoneId, tick: Tick, authority: NodeId, result_hash: [u8; 32]) {
        self.claims.insert((zone, tick), Claim { authority, result_hash });
    }

    /// Convenience: ingest a [`VerifyTick`] message directly as the authority's claim.
    /// `authority` is the message sender (the mesh layer authenticates it).
    pub fn record_claim_msg(&mut self, msg: &AuthorityMsg, authority: NodeId) {
        if let AuthorityMsg::VerifyTick { zone, tick, result_hash, .. } = msg {
            self.record_claim(*zone, *tick, authority, *result_hash);
        }
    }

    /// Record a verifier's [`VerifyResult`] vote. One vote per verifier per (zone, tick);
    /// a repeat from the same verifier overwrites its previous vote.
    pub fn record_vote(&mut self, result: &AuthorityMsg) {
        let AuthorityMsg::VerifyResult { zone, tick, verifier, agree, their_hash } = result else {
            return;
        };
        // We also treat a mismatching `their_hash` against a known claim as disagreement,
        // even if the verifier optimistically set `agree` — the hashes are the ground truth.
        let agreed = match self.claims.get(&(*zone, *tick)) {
            Some(claim) => *agree && claim.result_hash == *their_hash,
            None => *agree,
        };
        self.tallies
            .entry((*zone, *tick))
            .or_default()
            .votes
            .insert(verifier.clone(), agreed);
    }

    /// Resolve a (zone, tick) once enough verifier votes are in.
    ///
    /// Returns `None` while votes are still below quorum (the decision isn't safe yet) or
    /// if there is no claim to judge. Once at least a quorum of votes exists, returns a
    /// [`Verdict`] for the claiming authority reflecting its session-cumulative disputed
    /// ticks. A quorum of *disagreements* marks this tick disputed (recorded once).
    pub fn resolve(&mut self, zone: ZoneId, tick: Tick) -> Option<Verdict> {
        let claim = self.claims.get(&(zone, tick))?.clone();
        let tally = self.tallies.get(&(zone, tick))?;

        let q = self.quorum_size();
        // Need at least a quorum of votes before any decision is trustworthy.
        if tally.total() < q {
            return None;
        }

        // Record the dispute exactly once per (zone, tick).
        if tally.disagreements() >= q && !self.resolved.contains(&(zone, tick)) {
            self.disputed_by_authority
                .entry(claim.authority.clone())
                .or_default()
                .insert(tick);
        }
        self.resolved.insert((zone, tick));

        Some(self.verdict_for(&claim.authority))
    }

    /// Build the current verdict for an authority from its cumulative disputed ticks.
    pub fn verdict_for(&self, authority: &NodeId) -> Verdict {
        let ticks: Vec<Tick> = self
            .disputed_by_authority
            .get(authority)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default();
        let n = ticks.len();
        Verdict {
            authority: authority.clone(),
            disputed_ticks: ticks,
            recommend_slash: n >= SLASH_DISPUTE_THRESHOLD,
            recommend_reassign: n >= REASSIGN_DISPUTE_THRESHOLD,
        }
    }

    /// How many ticks of this authority are currently disputed.
    pub fn dispute_count(&self, authority: &NodeId) -> usize {
        self.disputed_by_authority.get(authority).map(|s| s.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::world::ZoneId;

    fn vote(zone: ZoneId, tick: Tick, verifier: &str, agree: bool, their_hash: [u8; 32]) -> AuthorityMsg {
        AuthorityMsg::VerifyResult {
            zone,
            tick,
            verifier: verifier.to_string(),
            agree,
            their_hash,
        }
    }

    #[test]
    fn quorum_is_two_thirds_ceiling() {
        assert_eq!(quorum(3), 2);
        assert_eq!(quorum(4), 3);
        assert_eq!(quorum(6), 4);
        assert_eq!(quorum(9), 6);
    }

    #[test]
    fn authority_flagged_when_two_of_three_disagree() {
        let zone = ZoneId::new(1, 2);
        let tick: Tick = 500;
        let claimed = [0xAAu8; 32];
        let honest = [0xBBu8; 32]; // verifiers' shadow result differs from the claim

        let mut cv = CrossValidator::new(3);
        cv.record_claim(zone, tick, "evil_authority".to_string(), claimed);

        // Two of three verifiers disagree (their hash differs) → quorum (2) reached.
        cv.record_vote(&vote(zone, tick, "v1", false, honest));
        cv.record_vote(&vote(zone, tick, "v2", false, honest));
        cv.record_vote(&vote(zone, tick, "v3", true, claimed));

        let verdict = cv.resolve(zone, tick).expect("enough votes to decide");
        assert_eq!(verdict.authority, "evil_authority");
        assert_eq!(verdict.disputed_ticks, vec![tick]);
        assert!(verdict.recommend_reassign, "a disputed tick should recommend reassignment");
        assert!(!verdict.recommend_slash, "a single dispute is not yet a slash");
    }

    #[test]
    fn honest_authority_is_not_disputed() {
        let zone = ZoneId::new(0, 0);
        let tick: Tick = 10;
        let h = [0x11u8; 32];

        let mut cv = CrossValidator::new(3);
        cv.record_claim(zone, tick, "good".to_string(), h);
        // All verifiers agree and report the same hash.
        cv.record_vote(&vote(zone, tick, "v1", true, h));
        cv.record_vote(&vote(zone, tick, "v2", true, h));
        cv.record_vote(&vote(zone, tick, "v3", true, h));

        let verdict = cv.resolve(zone, tick).expect("quorum of votes present");
        assert!(verdict.disputed_ticks.is_empty());
        assert!(!verdict.recommend_reassign);
        assert!(!verdict.recommend_slash);
    }

    #[test]
    fn sustained_disputes_escalate_to_slash() {
        let zone = ZoneId::new(5, 5);
        let claimed = [0x01u8; 32];
        let honest = [0x02u8; 32];
        let mut cv = CrossValidator::new(3);

        // Dispute SLASH_DISPUTE_THRESHOLD distinct ticks.
        for t in 0..SLASH_DISPUTE_THRESHOLD as u32 {
            cv.record_claim(zone, t, "repeat_offender".to_string(), claimed);
            cv.record_vote(&vote(zone, t, "v1", false, honest));
            cv.record_vote(&vote(zone, t, "v2", false, honest));
            cv.record_vote(&vote(zone, t, "v3", true, claimed));
            cv.resolve(zone, t);
        }

        let verdict = cv.verdict_for(&"repeat_offender".to_string());
        assert_eq!(verdict.disputed_ticks.len(), SLASH_DISPUTE_THRESHOLD);
        assert!(verdict.recommend_slash, "sustained disputes must recommend a slash");
        assert!(verdict.recommend_reassign);
    }

    #[test]
    fn no_decision_before_quorum_of_votes() {
        let zone = ZoneId::new(3, 3);
        let tick: Tick = 7;
        let claimed = [0u8; 32];
        let mut cv = CrossValidator::new(6); // quorum = 4
        cv.record_claim(zone, tick, "auth".to_string(), claimed);
        // Only two votes — well below quorum.
        cv.record_vote(&vote(zone, tick, "v1", false, [9u8; 32]));
        cv.record_vote(&vote(zone, tick, "v2", false, [9u8; 32]));
        assert!(cv.resolve(zone, tick).is_none(), "must not decide below quorum");
    }
}
