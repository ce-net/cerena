//! Stake-weighted rendezvous (HRW) hashing for zone → authority assignment.
//!
//! Every node in a session can compute, with no directory and no coordination, which
//! node owns a given [`ZoneId`]. The scheme is [Highest-Random-Weight hashing][hrw]:
//! for each candidate node we derive a deterministic per-zone score and the highest
//! score wins the zone. Because the score is a pure function of `(session, zone, node)`,
//! all nodes agree, and adding/removing a candidate only reshuffles the zones that
//! candidate would have touched (minimal disruption) rather than the whole map.
//!
//! ## Stake weighting (Sybil cost without monopoly)
//!
//! Plain HRW gives every node an equal expected share. We want more-bonded nodes to
//! host more zones — bonding on-chain stake is the cost an attacker pays to seize
//! authority — but we must **not** let the single richest node own the entire world
//! (that would re-centralise the sim and make it one bribe/seizure away from total
//! control). So we fold stake in with the standard *weighted* rendezvous formula
//! `score = u ^ (1 / weight)` where `u ∈ (0,1)` is the uniform hash draw, and the
//! weight is **log-scaled** in stake: doubling your stake does not double your zones,
//! it adds a slowly-growing constant. A zero-stake node still wins every zone where
//! its raw hash draw happens to be high, so the tail of small nodes keeps a real,
//! non-trivial share. This is the anti-monopoly intent, made explicit.
//!
//! [hrw]: https://en.wikipedia.org/wiki/Rendezvous_hashing

use arena_protocol::{auth::SessionId, world::ZoneId, NodeId};
use glam::Vec3;
use sha2::{Digest, Sha256};

/// Base units per credit (wei-style fixed point; see CE money model).
const CREDIT: f64 = 1_000_000_000_000_000_000.0;

/// Convert on-chain base units to a (possibly fractional) credit count as `f64`. Used only
/// to feed the log-scaled stake weight; precision loss here is harmless (the weight is itself
/// a coarse, slowly-growing bias).
fn amount_credits(base_units: i128) -> f64 {
    base_units as f64 / CREDIT
}

/// One node eligible to own zones, plus the on-chain stake it has bonded. The stake
/// only biases the assignment; it never excludes a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub node: NodeId,
    /// Bonded stake in base units (10^18 per credit). May be `0` for an unbonded node.
    pub stake_base_units: i128,
}

impl Candidate {
    pub fn new(node: impl Into<NodeId>, stake_base_units: i128) -> Self {
        Self { node: node.into(), stake_base_units }
    }
}

/// Deterministic stake-weighted rendezvous score for `(session, zone, node)`.
///
/// The hash domain is `sha256(node_id || session || zone.token())`, so the draw is
/// independent per zone and unpredictable, yet identical on every node. We take the
/// first 8 bytes as a `u64`, normalise to a uniform `u ∈ (0,1)`, then apply the
/// weighted-HRW transform `u^(1/weight)` with a log-scaled stake weight. The result
/// is mapped back onto the full `u64` range so callers can compare scores as plain
/// integers; **higher wins**.
///
/// Note: the abstract signature in the design doc omitted `session`, but the hash
/// domain it specifies includes it (so the same node+zone maps differently across
/// concurrent sessions). We therefore thread `session` through explicitly.
pub fn hrw_score(
    session: &SessionId,
    zone: ZoneId,
    node: &NodeId,
    stake_base_units: i128,
) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(node.as_bytes());
    hasher.update(session.as_str().as_bytes());
    hasher.update(zone.token().as_bytes());
    let digest = hasher.finalize();

    // First 8 bytes → u64. The remaining 24 bytes are unused but keep the domain wide.
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&digest[..8]);
    let base = u64::from_be_bytes(raw);

    // Uniform draw in the open interval (0, 1). We shift off zero (an exact 0 would make
    // any weight win) by mapping onto (0, 1] via (base + 1) / (2^64).
    let u = (base as f64 + 1.0) / (u64::MAX as f64 + 1.0);

    // Log-scaled stake weight, always >= 1. `ln(1 + credits)` grows slowly, so a whale
    // gets a meaningful-but-bounded edge and a zero-stake node still has weight 1.0.
    let credits = amount_credits(stake_base_units).max(0.0);
    let weight = 1.0 + (1.0 + credits).ln();

    // Weighted rendezvous transform: larger weight pushes the score toward 1.0, but the
    // per-zone randomness in `u` still lets low-weight nodes win their share of zones.
    let score = u.powf(1.0 / weight);

    // Map the (0,1] score back onto u64 so callers compare integers. Saturating cast.
    (score * (u64::MAX as f64)) as u64
}

/// The authoritative owner of `zone`: the candidate with the highest [`hrw_score`].
/// `None` only when there are no candidates. Ties (astronomically unlikely with a
/// 64-bit score) break by node id for determinism.
pub fn assign_authority(
    session: &SessionId,
    zone: ZoneId,
    candidates: &[Candidate],
) -> Option<NodeId> {
    candidates
        .iter()
        .max_by(|a, b| {
            let sa = hrw_score(session, zone, &a.node, a.stake_base_units);
            let sb = hrw_score(session, zone, &b.node, b.stake_base_units);
            // Higher score wins; on an exact tie fall back to node id for a stable order.
            sa.cmp(&sb).then_with(|| a.node.cmp(&b.node))
        })
        .map(|c| c.node.clone())
}

/// The full ownership ranking for `zone`, primary first. This *is* the failover order:
/// if the primary authority stops producing valid ticks, the next node adopts the zone.
/// The live handoff is sequenced by monotonically increasing `AuthorityClaim` epochs in
/// `arena-server`; this function only decides *who is next*, not *when* they take over.
pub fn authority_ranking(
    session: &SessionId,
    zone: ZoneId,
    candidates: &[Candidate],
) -> Vec<NodeId> {
    let mut ranked: Vec<&Candidate> = candidates.iter().collect();
    ranked.sort_by(|a, b| {
        let sa = hrw_score(session, zone, &a.node, a.stake_base_units);
        let sb = hrw_score(session, zone, &b.node, b.stake_base_units);
        // Descending by score, then ascending by node id to make the order total.
        sb.cmp(&sa).then_with(|| a.node.cmp(&b.node))
    });
    ranked.into_iter().map(|c| c.node.clone()).collect()
}

/// A cheap, cloneable view that answers authority/routing queries for one session over a
/// snapshot of candidates. Hold one per node; refresh `candidates` from [`crate::Discovery`]
/// on a timer (atlas entries expire, nodes join/leave) via [`ZoneRouter::update_candidates`].
#[derive(Debug, Clone)]
pub struct ZoneRouter {
    session: SessionId,
    candidates: Vec<Candidate>,
}

impl ZoneRouter {
    pub fn new(session: SessionId, candidates: Vec<Candidate>) -> Self {
        Self { session, candidates }
    }

    /// The session this router routes for.
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    /// Current candidate set.
    pub fn candidates(&self) -> &[Candidate] {
        &self.candidates
    }

    /// Swap in a freshly-discovered candidate set (call periodically; see [`crate::Discovery`]).
    /// Authority assignment changes only for the zones the changed candidates would touch.
    pub fn update_candidates(&mut self, candidates: Vec<Candidate>) {
        self.candidates = candidates;
    }

    /// The authoritative node for `zone`, or `None` if there are no candidates.
    pub fn authority_for(&self, zone: ZoneId) -> Option<NodeId> {
        assign_authority(&self.session, zone, &self.candidates)
    }

    /// The full primary-then-failover ranking for `zone`.
    pub fn ranking_for(&self, zone: ZoneId) -> Vec<NodeId> {
        authority_ranking(&self.session, zone, &self.candidates)
    }

    /// Which zone a world position falls into (delegates to [`ZoneId::from_world`]). Combine
    /// with [`authority_for`](Self::authority_for) to route a player's traffic to its owner.
    pub fn zone_of(&self, pos: Vec3) -> ZoneId {
        ZoneId::from_world(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId("match-test".to_string())
    }

    fn candidate(id: &str, stake_credits: i128) -> Candidate {
        // stake in whole credits → base units (10^18). i128 holds this comfortably.
        Candidate::new(id, stake_credits * 1_000_000_000_000_000_000)
    }

    #[test]
    fn assignment_is_deterministic_and_stable() {
        let s = session();
        let cands = vec![candidate("aaaa", 1), candidate("bbbb", 1), candidate("cccc", 1)];
        let zone = ZoneId::new(3, -7);
        let first = assign_authority(&s, zone, &cands);
        // Re-running with the same inputs (and a reordered slice) yields the same owner.
        let mut reordered = cands.clone();
        reordered.reverse();
        let second = assign_authority(&s, zone, &reordered);
        assert!(first.is_some());
        assert_eq!(first, second, "assignment must be order-independent and stable");
    }

    #[test]
    fn higher_stake_wins_more_zones() {
        let s = session();
        // One whale, one minnow. Across many zones the whale should host clearly more,
        // but the minnow must still win a non-trivial slice (anti-monopoly).
        let cands = vec![candidate("whale", 1_000_000), candidate("minno", 0)];
        let mut whale_wins = 0u32;
        let mut minnow_wins = 0u32;
        for x in 0..64 {
            for z in 0..64 {
                match assign_authority(&s, ZoneId::new(x, z), &cands).as_deref() {
                    Some("whale") => whale_wins += 1,
                    Some("minno") => minnow_wins += 1,
                    _ => unreachable!(),
                }
            }
        }
        assert!(whale_wins > minnow_wins, "more stake should win more zones ({whale_wins} vs {minnow_wins})");
        assert!(minnow_wins > 0, "a zero-stake node must still win some zones (no monopoly)");
    }

    #[test]
    fn ranking_is_a_permutation_of_candidates() {
        let s = session();
        let cands = vec![candidate("aaaa", 5), candidate("bbbb", 2), candidate("cccc", 9)];
        let zone = ZoneId::new(1, 1);
        let ranking = authority_ranking(&s, zone, &cands);
        assert_eq!(ranking.len(), cands.len());
        // Every candidate appears exactly once, and the primary matches assign_authority.
        for c in &cands {
            assert!(ranking.contains(&c.node), "{} missing from ranking", c.node);
        }
        assert_eq!(Some(ranking[0].clone()), assign_authority(&s, zone, &cands));
    }

    #[test]
    fn empty_candidates_have_no_authority() {
        assert_eq!(assign_authority(&session(), ZoneId::new(0, 0), &[]), None);
    }
}
