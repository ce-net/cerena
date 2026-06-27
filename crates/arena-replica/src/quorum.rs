//! State-hash consensus — how the mesh tells an honest sim from a desynced or
//! cheating one with no trusted server. Every replica of a zone periodically
//! publishes its deterministic [`arena_sim::World::state_hash`] as a
//! [`arena_protocol::replica::StateProof`]; comparing them, the most common hash
//! wins, and every replica that computed a different hash is a **dissenter** to be
//! re-synced (and, on sustained disagreement, karma-slashed by `arena-karma`).
//!
//! A strict majority is required, so a single honest node can never be overruled by
//! one liar and a 1-vs-1 split is reported as no-quorum rather than a false
//! accusation. This mirrors spacegame's `replication::agree`, over Cerena's 32-byte
//! hash and pairing each proof with its authenticated sender.

use std::collections::BTreeMap;

use arena_protocol::NodeId;

/// The outcome of comparing replicas' state hashes for one tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agreement {
    /// The hash the plurality of replicas computed (the accepted truth), if any.
    pub quorum_hash: Option<[u8; 32]>,
    /// Nodes whose hash matched the winner.
    pub agree: Vec<NodeId>,
    /// Nodes whose hash disagreed — faulty or cheating, to be re-synced or excluded.
    pub dissent: Vec<NodeId>,
    /// True only if the winning hash has a STRICT majority (the result is trustworthy).
    pub has_quorum: bool,
}

/// What a replica should DO this checkpoint, derived from an [`Agreement`] for a node.
/// This turns "the replicas disagree" into a convergent action — the heart of the
/// merge: an out-voted replica (a cheater, or one that merely desynced) adopts the
/// agreed state so the zone stays single-valued without a trusted central server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Fewer than two replicas, or no strict majority — cannot safely act this round.
    Inconclusive,
    /// This node is part of the quorum (or all agree) — nothing to do.
    Agreed,
    /// This node is out-voted by a quorum that agreed on this hash — it must MERGE:
    /// re-sync its world to the snapshot whose hash equals this value.
    ResyncTo([u8; 32]),
    /// This node IS in the quorum; these other node(s) diverged — the cheat/fault
    /// suspects (fed to `arena-karma` for sustained-disagreement slashing).
    PeersDiverged(Vec<NodeId>),
}

impl Agreement {
    /// The action `this_node` should take given this agreement. See [`Verdict`].
    pub fn verdict(&self, this_node: &str) -> Verdict {
        if !self.has_quorum {
            return Verdict::Inconclusive;
        }
        if self.dissent.is_empty() {
            return Verdict::Agreed;
        }
        if self.dissent.iter().any(|d| d == this_node) {
            match self.quorum_hash {
                Some(h) => Verdict::ResyncTo(h),
                None => Verdict::Inconclusive,
            }
        } else {
            Verdict::PeersDiverged(self.dissent.clone())
        }
    }
}

/// Compare the replicas' `(node, hash)` proofs for a single tick and decide the agreed
/// truth. The most common hash wins (ties broken by the lexicographically smallest
/// hash, deterministically). `has_quorum` is true only if the winner has a strict
/// majority, so a single liar cannot frame a single honest node, and a 1-1 split is
/// reported as no-quorum. Each proof is paired with its authenticated sender by the
/// caller (the mesh layer), so a node cannot vote as someone else.
pub fn agree(proofs: &[(NodeId, [u8; 32])]) -> Agreement {
    if proofs.is_empty() {
        return Agreement { quorum_hash: None, agree: vec![], dissent: vec![], has_quorum: false };
    }
    // Count votes per hash. BTreeMap iterates hashes ascending, so picking the first
    // hash that beats the running best is a deterministic lowest-hash tiebreak.
    let mut counts: BTreeMap<[u8; 32], usize> = BTreeMap::new();
    for (_node, hash) in proofs {
        *counts.entry(*hash).or_insert(0) += 1;
    }
    let mut winner = [0u8; 32];
    let mut top = 0usize;
    for (&hash, &c) in &counts {
        if c > top {
            winner = hash;
            top = c;
        }
    }
    let total = proofs.len();
    let has_quorum = top * 2 > total;

    let mut agree_nodes = Vec::new();
    let mut dissent_nodes = Vec::new();
    for (node, hash) in proofs {
        if *hash == winner {
            agree_nodes.push(node.clone());
        } else {
            dissent_nodes.push(node.clone());
        }
    }
    agree_nodes.sort();
    dissent_nodes.sort();
    Agreement { quorum_hash: Some(winner), agree: agree_nodes, dissent: dissent_nodes, has_quorum }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn p(node: &str, byte: u8) -> (NodeId, [u8; 32]) {
        (node.to_string(), h(byte))
    }

    #[test]
    fn honest_majority_outvotes_a_cheater() {
        let a = agree(&[p("a", 7), p("b", 7), p("c", 7), p("cheat", 9)]);
        assert_eq!(a.quorum_hash, Some(h(7)));
        assert!(a.has_quorum);
        assert_eq!(a.agree, vec!["a", "b", "c"]);
        assert_eq!(a.dissent, vec!["cheat"]);
    }

    #[test]
    fn a_one_to_one_split_is_no_quorum_not_a_false_accusation() {
        let a = agree(&[p("a", 1), p("b", 2)]);
        assert!(!a.has_quorum, "a 1-1 split has no quorum");
    }

    #[test]
    fn verdict_tells_an_outvoted_node_to_merge_to_the_quorum() {
        let a = agree(&[p("a", 7), p("b", 7), p("c", 7), p("cheat", 9)]);
        assert_eq!(a.verdict("cheat"), Verdict::ResyncTo(h(7)), "the out-voted node merges to truth");
        assert_eq!(a.verdict("a"), Verdict::PeersDiverged(vec!["cheat".into()]), "a quorum node flags the suspect");
        let unanimous = agree(&[p("a", 5), p("b", 5)]);
        assert_eq!(unanimous.verdict("a"), Verdict::Agreed);
        let split = agree(&[p("a", 1), p("b", 2)]);
        assert_eq!(split.verdict("a"), Verdict::Inconclusive);
    }

    #[test]
    fn empty_proofs_is_inconclusive() {
        let a = agree(&[]);
        assert!(!a.has_quorum && a.quorum_hash.is_none());
    }
}
