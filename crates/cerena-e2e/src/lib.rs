//! # cerena-e2e
//!
//! End-to-end verification that Cerena actually holds up *deployed*, *at scale*,
//! and *under failure* — not just in unit tests. Three layers:
//!
//! 1. [`cluster`] — bring up a fleet of authority nodes. Two backends behind one
//!    trait: [`cluster::LocalCluster`] (subprocess `arena-server` instances on one
//!    box, for CI) and [`cluster::HetznerCluster`] (real VMs provisioned via the
//!    Hetzner API + the CE node, for true at-scale runs).
//! 2. [`bots`] — a headless load generator: thousands of simulated players that
//!    join, move, cast, and die, exercising the real netcode path
//!    (`arena-net::ClientWorld` + the wire protocol).
//! 3. [`fault`] — fault injection (kill a zone authority, partition a node, run a
//!    malicious authority, mass-disconnect) with assertions on recovery: failover
//!    time, state convergence, and that cross-validation flags cheating authorities.
//!
//! [`metrics`] ties it together with latency histograms, throughput, and a
//! convergence checker built on `arena_sim::World::state_hash`.
//!
//! ## Running
//!
//! - CI / no infra: `cargo test -p cerena-e2e` runs the local-cluster + load tests.
//! - At scale on real VMs: `cargo test -p cerena-e2e -- --ignored --nocapture`
//!   with `HETZNER_API_TOKEN`, `CE_SSH_KEY_NAME`, `CE_SSH_KEY_PATH` set (mirrors the
//!   `ce-deploy` E2E convention). These provision, deploy, run, and tear down.
//!
//! Nothing here is built or run by the harness author (CPU constraint); it is
//! eyeballed test code meant to be exercised when the cluster is available.

pub mod bots;
pub mod cluster;
pub mod fault;
pub mod metrics;
pub mod scenario;

/// Shared result alias.
pub type Result<T> = anyhow::Result<T>;

/// Defaults that scenarios start from; overridable per test.
#[derive(Debug, Clone)]
pub struct E2eConfig {
    /// Number of authority nodes in the fleet.
    pub nodes: usize,
    /// Target concurrent players to ramp to.
    pub players: usize,
    /// How long to hold peak load before tearing down.
    pub hold: std::time::Duration,
    /// Session/map identifier under test.
    pub session: String,
    /// Acceptable p99 snapshot round-trip before a scale test fails.
    pub max_p99_rtt_ms: f64,
    /// Minimum sustained authority tick rate (Hz) before a test fails.
    pub min_tick_hz: f64,
}

impl Default for E2eConfig {
    fn default() -> Self {
        Self {
            nodes: 4,
            players: 1_000,
            hold: std::time::Duration::from_secs(60),
            session: "e2e".to_string(),
            max_p99_rtt_ms: 120.0,
            min_tick_hz: 60.0,
        }
    }
}
