//! The hot-reload protocol: how a content swap propagates to a live match.
//!
//! Only the session coordinator (a capability holder) may publish a new
//! [`ContentVersion`]. Authorities and clients react by fetching the pack blob and
//! staging it into their [`crate::registry::ContentRegistry`] for a tick-boundary
//! swap. This module defines the wire-shapes; `arena-server` carries them over the
//! mesh control plane and the ce-net blob store.

use serde::{Deserialize, Serialize};

/// Announced by the coordinator on the session control plane whenever content
/// changes. Monotonic `epoch` defeats stale/replayed announcements; `pack_hash`
/// is the content-addressed blob to fetch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentVersion {
    pub epoch: u64,
    /// Hex sha256 of the [`crate::pack::ContentPack`] bytes (its blob id).
    pub pack_hash: String,
    /// Human label for logs / the client's "content updated" toast.
    pub label: String,
    /// Coordinator node id that signed this announcement (verified by arena-mesh).
    pub issuer: String,
    /// If set, the swap must take effect no earlier than this sim tick, giving all
    /// peers time to fetch. 0 = at the next safe boundary after fetch completes.
    pub apply_at_tick: u32,
}

/// What a peer reports back so the coordinator can confirm the fleet converged
/// before relying on new content (e.g. before spawning items only the new pack
/// defines). Convergence is also a health signal: a node stuck on an old epoch is
/// fetched-failed or partitioned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentAck {
    pub node: String,
    pub epoch: u64,
    pub state: SwapState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SwapState {
    /// Saw the announcement, fetching the blob.
    Fetching,
    /// Pack fetched + validated + staged, awaiting the tick boundary.
    Staged,
    /// Swap applied; now serving the new epoch.
    Live,
    /// Fetch or validation failed; still on the previous epoch.
    Failed,
}

/// A targeted request a freshly-joined (or recovered) peer sends to ask the
/// coordinator which content version is current, so it can converge immediately
/// rather than waiting for the next announcement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentVersionQuery;

/// A development convenience: a diff summary the coordinator can log/show so the
/// designer sees exactly what changed between two packs. Not required for the swap
/// itself (the swap is whole-pack and atomic), but invaluable when tweaking live.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub changed: Vec<String>,
}

impl PackDiff {
    /// Compute a coarse id-level diff between two packs (by stable id presence and
    /// per-def hash). Intended for logging the designer's live edits.
    pub fn between(old: &crate::pack::ContentPack, new: &crate::pack::ContentPack) -> PackDiff {
        use std::collections::BTreeMap;
        // Index every def by a "kind:id" key -> its individual hash.
        fn index(p: &crate::pack::ContentPack) -> BTreeMap<String, u64> {
            let mut m = BTreeMap::new();
            for s in &p.spells {
                m.insert(format!("spell:{}", s.id), fnv(&s.id.0) ^ fnv(&s.name));
            }
            for i in &p.items {
                m.insert(format!("item:{}", i.id), fnv(&i.id.0) ^ fnv(&i.name));
            }
            for a in &p.abilities {
                m.insert(format!("ability:{}", a.id), fnv(&a.id.0) ^ fnv(&a.spell.0));
            }
            for s in &p.statuses {
                m.insert(format!("status:{}", s.id), fnv(&s.id.0));
            }
            for s in &p.shaders {
                m.insert(format!("shader:{}", s.id), fnv(&s.source));
            }
            m
        }
        let (a, b) = (index(old), index(new));
        let mut diff = PackDiff::default();
        for (k, hv) in &b {
            match a.get(k) {
                None => diff.added.push(k.clone()),
                Some(old_h) if old_h != hv => diff.changed.push(k.clone()),
                _ => {}
            }
        }
        for k in a.keys() {
            if !b.contains_key(k) {
                diff.removed.push(k.clone());
            }
        }
        diff
    }
}

/// Tiny FNV-1a over a string, only used for the human-facing [`PackDiff`] summary.
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
