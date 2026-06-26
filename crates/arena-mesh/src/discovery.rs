//! Candidate-node discovery via the CE capacity atlas.
//!
//! The authority assignment ([`crate::authority`]) needs a set of [`Candidate`] nodes to
//! hash zones over. We get that set for free from the CE node's `/atlas`: every peer that
//! advertises capacity shows up there with its self-declared capability tags. A node that
//! wants to host arena zones advertises the `"arena"` tag (see `ce_rs::TagAdvertiser`);
//! discovery keeps exactly those.
//!
//! The candidate set is *not* static — atlas entries expire and nodes join/leave — so the
//! caller refreshes it on a timer and feeds it into [`ZoneRouter::update_candidates`].

use anyhow::Result;
use arena_protocol::NodeId;
use ce_rs::CeClient;

use crate::authority::Candidate;
use crate::ZoneRouter;

/// The capability self-tag a node advertises to volunteer as an arena zone host.
pub const ARENA_TAG: &str = "arena";

/// Reads the local CE node's atlas to discover arena-capable hosts.
#[derive(Clone)]
pub struct Discovery {
    ce: CeClient,
}

impl Discovery {
    /// Wrap a CE client (typically the local node).
    pub fn new(ce: CeClient) -> Self {
        Self { ce }
    }

    /// The local node's own id (`GET /status`). A node uses this to tell whether *it* is the
    /// authority for a given zone.
    pub async fn this_node(&self) -> Result<NodeId> {
        Ok(self.ce.status().await?.node_id)
    }

    /// All arena-capable candidate hosts known to the local node right now.
    ///
    /// Filters the atlas to entries tagged [`ARENA_TAG`] and maps each to a [`Candidate`].
    ///
    /// STAKE PLACEHOLDER: on-chain stake is not yet surfaced per-atlas-entry, so until that
    /// wiring lands we derive a *stand-in* weight from advertised capacity (`cpu_cores * mem_mb`,
    /// in base units). This biases ownership toward beefier hosts, which is a reasonable interim
    /// proxy, but it is **not** Sybil-resistant — a node can lie about its capacity for free.
    /// Replacing this with the real bonded stake (from the node's `bond` / a chain query) is the
    /// step that turns authority assignment into an actual Sybil cost. Tracked for `arena-server`.
    pub async fn arena_nodes(&self) -> Result<Vec<Candidate>> {
        let atlas = self.ce.atlas().await?;
        let candidates = atlas
            .into_iter()
            .filter(|e| e.has_tag(ARENA_TAG))
            .map(|e| {
                // Capacity-derived placeholder weight (see method doc). Cast through i128 so the
                // product cannot overflow for any realistic cpu/mem advertisement.
                let stake = e.cpu_cores as i128 * e.mem_mb as i128;
                Candidate { node: e.node_id, stake_base_units: stake }
            })
            .collect();
        Ok(candidates)
    }

    /// Convenience: discover candidates and build a [`ZoneRouter`] for `session` in one call.
    pub async fn router_for(
        &self,
        session: arena_protocol::auth::SessionId,
    ) -> Result<ZoneRouter> {
        let candidates = self.arena_nodes().await?;
        Ok(ZoneRouter::new(session, candidates))
    }
}
