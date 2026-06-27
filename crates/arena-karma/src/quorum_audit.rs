//! Anti-cheat backstop for the **replicated-authority** model — the counterpart to
//! [`crossval`](crate::crossval) for the world where every player near a zone runs the
//! full sim and the replicas reconcile by a majority state-hash.
//!
//! When the quorum ([`arena_replica::agree`]) finds a replica out-voted, that node's
//! [`arena_sim::World::state_hash`] disagrees with the supermajority — it is desynced
//! or cheating. A *single* disagreement is not actionable: glam f32 is not bit-identical
//! across architectures (the workspace `Cargo.toml` says so), so an honest replica can
//! drift a hair and lose one round. The correct, honest-drift-tolerant signal is
//! **sustained** dissent: an honest replica, out-voted, fetches the agreed snapshot and
//! `reseed`s to it, so it agrees again the very next round and its streak resets. A
//! tampered `World` keeps producing a divergent hash round after round. Only once a node
//! crosses [`SLASH_DISPUTE_THRESHOLD`](crate::crossval::SLASH_DISPUTE_THRESHOLD)
//! *consecutive* dissenting rounds do we recommend a slash — fed to the same ce-gov
//! stake-burn + [`KarmaLedger`](crate::ledger::KarmaLedger) path that
//! [`crossval`](crate::crossval) uses.

use std::collections::HashMap;

use arena_protocol::world::ZoneId;
use arena_protocol::{NodeId, Tick};

use crate::crossval::SLASH_DISPUTE_THRESHOLD;

/// A slash recommendation for a replica whose state hash has been out-voted by the
/// quorum for a sustained run of rounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuorumVerdict {
    /// The replica under judgement.
    pub node: NodeId,
    /// The zone whose quorum it kept losing.
    pub zone: ZoneId,
    /// The ticks (quorum rounds) at which it dissented, ascending.
    pub disputed_ticks: Vec<Tick>,
    /// Always true when emitted — it crossed the sustained-dissent threshold.
    pub recommend_slash: bool,
}

/// Tracks per-`(zone, node)` consecutive dissent against the state-hash quorum.
#[derive(Debug, Default)]
pub struct QuorumAuditor {
    /// Consecutive dissenting rounds, reset to 0 on any agreeing round.
    streak: HashMap<(ZoneId, NodeId), usize>,
    /// The dissenting ticks accumulated for the current streak (for the verdict).
    ticks: HashMap<(ZoneId, NodeId), Vec<Tick>>,
}

impl QuorumAuditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current consecutive-dissent streak for a `(zone, node)`.
    pub fn streak(&self, zone: ZoneId, node: &NodeId) -> usize {
        self.streak.get(&(zone, node.clone())).copied().unwrap_or(0)
    }

    /// Record one quorum round for `zone` at `tick`: `agreeing` matched the quorum
    /// hash, `dissenting` did not. Agreement clears a node's streak (an honest replica
    /// that reseeded is now back with the quorum); dissent extends it. Returns a slash
    /// recommendation for every node that just crossed the sustained-dissent threshold.
    pub fn record_round(
        &mut self,
        zone: ZoneId,
        tick: Tick,
        agreeing: &[NodeId],
        dissenting: &[NodeId],
    ) -> Vec<QuorumVerdict> {
        // Agreement resets — this is the honest-drift / post-reseed escape hatch.
        for node in agreeing {
            self.streak.remove(&(zone, node.clone()));
            self.ticks.remove(&(zone, node.clone()));
        }

        let mut verdicts = Vec::new();
        for node in dissenting {
            let key = (zone, node.clone());
            let s = self.streak.entry(key.clone()).or_insert(0);
            *s += 1;
            let ts = self.ticks.entry(key.clone()).or_default();
            ts.push(tick);
            if *s >= SLASH_DISPUTE_THRESHOLD {
                verdicts.push(QuorumVerdict {
                    node: node.clone(),
                    zone,
                    disputed_ticks: ts.clone(),
                    recommend_slash: true,
                });
            }
        }
        verdicts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> NodeId {
        s.to_string()
    }

    #[test]
    fn a_single_dissent_is_not_slashed() {
        let mut a = QuorumAuditor::new();
        let zone = ZoneId::new(0, 0);
        let v = a.record_round(zone, 10, &[n("a"), n("b"), n("c")], &[n("x")]);
        assert!(v.is_empty(), "one off round (likely f32 drift) is tolerated");
        assert_eq!(a.streak(zone, &n("x")), 1);
    }

    #[test]
    fn sustained_dissent_recommends_a_slash() {
        let mut a = QuorumAuditor::new();
        let zone = ZoneId::new(0, 0);
        let honest = vec![n("a"), n("b"), n("c")];
        let mut last = Vec::new();
        for tick in [10u32, 11, 12] {
            last = a.record_round(zone, tick, &honest, &[n("x")]);
        }
        assert_eq!(last.len(), 1, "three consecutive losses crosses the slash threshold");
        assert_eq!(last[0].node, n("x"));
        assert!(last[0].recommend_slash);
        assert_eq!(last[0].disputed_ticks, vec![10, 11, 12]);
    }

    #[test]
    fn reseeding_back_to_the_quorum_clears_the_streak() {
        let mut a = QuorumAuditor::new();
        let zone = ZoneId::new(0, 0);
        let honest = vec![n("a"), n("b"), n("c")];
        a.record_round(zone, 10, &honest, &[n("x")]);
        a.record_round(zone, 11, &honest, &[n("x")]);
        // x reseeds and agrees this round → streak resets.
        let mut who = honest.clone();
        who.push(n("x"));
        a.record_round(zone, 12, &who, &[]);
        assert_eq!(a.streak(zone, &n("x")), 0);
        // A later isolated dissent does not immediately slash.
        let v = a.record_round(zone, 13, &honest, &[n("x")]);
        assert!(v.is_empty());
    }
}
