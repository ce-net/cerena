//! [`ServerConfig`]: every knob the orchestrator reads at startup.
//!
//! The binary (`arena-server-bin`) parses CLI flags into one of these; library
//! callers and tests build it directly. Defaults target a single node talking to a
//! locally-running CE node on the standard port, acting as a plain zone authority
//! (not the coordinator).

use arena_protocol::auth::SessionId;
use arena_protocol::world::MapId;
use arena_protocol::TICK_HZ;

/// All configuration for one [`crate::ArenaServer`].
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The match/session this node participates in. Drives every mesh topic
    /// ([`SessionId::topic_root`]) and the authority hash domain, so two nodes only
    /// cooperate when their sessions match.
    pub session: SessionId,
    /// Base URL of the **local CE node's HTTP API** this server drives over the mesh
    /// (e.g. `http://127.0.0.1:8844`). All peer-to-peer traffic is routed by that node;
    /// we never open a socket to a remote peer ourselves.
    pub api_url: String,
    /// API token for the local CE node, so mutating calls pass its auth middleware.
    /// `None` falls back to `ce_rs`'s own discovery (`$CE_API_TOKEN` / the node's
    /// `api.token`); read-only if neither is found.
    pub node_token: Option<String>,
    /// Whether this node is the session **coordinator** (admits players, assigns zones,
    /// publishes content versions, fuses karma). Exactly one node per session should set
    /// this; the rest are pure zone authorities.
    pub coordinator: bool,
    /// The map id reported to joining clients. Geometry itself is generated per-zone from
    /// the content worldgen params; this is the content-addressed label.
    pub map: MapId,
    /// TEST ONLY: relax session-ticket verification (accept any well-formed ticket without
    /// the issuer-signature gate). Lets local e2e fleets join without a real coordinator
    /// signing key. Never enable in a deployment that handles competitive stakes.
    pub e2e_insecure: bool,
    /// TEST ONLY: enable the test admin surface (e.g. the simulated-partition toggle the
    /// e2e harness drives). Inert unless the harness uses it.
    pub e2e_admin: bool,
    /// TEST ONLY: make this node behave as a **malicious authority** — it deliberately
    /// publishes a wrong post-tick state hash so the cross-validation tests can prove that
    /// honest verifiers catch a lying authority. Clearly gated; never enable in production.
    pub e2e_cheat: bool,
    /// Simulation tick rate in Hz. Defaults to [`arena_protocol::TICK_HZ`]; overridable so a
    /// test can run a slow, observable clock.
    pub tick_hz: u32,
    /// Proximity-replication factor K: how many nearby peers redundantly hold each player's
    /// full checkpoint, so a crashed zone authority loses nothing. Default 3.
    pub replication_factor: usize,
    /// How often (in sim ticks) the authority pushes fresh player checkpoints to its holders.
    /// Default 64 (~1 s at 64 Hz).
    pub replication_interval_ticks: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            session: SessionId("cerena-dev".to_string()),
            api_url: ce_rs::DEFAULT_BASE_URL.to_string(),
            node_token: None,
            coordinator: false,
            map: MapId("cerena-overworld".to_string()),
            e2e_insecure: false,
            e2e_admin: false,
            e2e_cheat: false,
            tick_hz: TICK_HZ,
            replication_factor: 3,
            replication_interval_ticks: 64,
        }
    }
}

impl ServerConfig {
    /// Start from defaults for `session`.
    pub fn new(session: impl Into<String>) -> Self {
        Self {
            session: SessionId(session.into()),
            ..Self::default()
        }
    }

    /// Point at a specific local CE node HTTP API.
    pub fn with_api_url(mut self, url: impl Into<String>) -> Self {
        self.api_url = url.into();
        self
    }

    /// Attach an explicit CE node API token.
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.node_token = token;
        self
    }

    /// Mark this node as the session coordinator.
    pub fn as_coordinator(mut self, yes: bool) -> Self {
        self.coordinator = yes;
        self
    }

    /// Override the simulation tick rate (Hz). A `0` is treated as the default.
    pub fn with_tick_hz(mut self, hz: u32) -> Self {
        self.tick_hz = if hz == 0 { TICK_HZ } else { hz };
        self
    }

    /// The effective tick period in seconds.
    pub fn tick_period_secs(&self) -> f64 {
        1.0 / self.tick_hz.max(1) as f64
    }
}
