//! End-to-end test entrypoints.
//!
//! Most tests are `#[ignore]` because they need infrastructure (a built
//! `arena-server` binary, or real VMs + Hetzner creds). Run them explicitly:
//!
//! ```text
//! # local subprocess fleet (needs target/release/arena-server)
//! cargo test -p cerena-e2e --test e2e local_ -- --ignored --nocapture
//!
//! # real VMs at scale + fault tolerance (needs HETZNER_API_TOKEN, CE_SSH_KEY_*)
//! cargo test -p cerena-e2e --test e2e hetzner_ -- --ignored --nocapture
//! ```
//!
//! The non-ignored tests are pure and exercise the harness's own logic.

use std::time::Duration;

use cerena_e2e::cluster::{Cluster, HetznerCluster, LocalCluster};
use cerena_e2e::{E2eConfig, fault, scenario};

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("info,cerena_e2e=debug")
        .try_init();
}

/// Path to the server binary the local cluster spawns. Override with
/// `CERENA_SERVER_BIN`; defaults to the workspace release build.
fn server_bin() -> String {
    std::env::var("CERENA_SERVER_BIN").unwrap_or_else(|_| {
        format!(
            "{}/../../target/release/arena-server",
            env!("CARGO_MANIFEST_DIR")
        )
    })
}

// ---------------------------------------------------------------------------
// Local subprocess fleet (no external infra, needs the built binary).
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "needs target/release/arena-server"]
async fn local_smoke_4_nodes_200_players() {
    init_tracing();
    let mut cfg = E2eConfig::default();
    cfg.nodes = 4;
    cfg.players = 200;
    cfg.hold = Duration::from_secs(20);
    cfg.max_p99_rtt_ms = 80.0;

    let mut cluster = LocalCluster::new(cfg.nodes, server_bin(), &cfg.session);
    let report = scenario::run_scale(&mut cluster, &cfg).await;
    cluster.teardown().await.ok();
    let report = report.expect("scale run");
    assert!(report.peak_players > 0, "no bots joined");
    assert!(report.all_authorities_live, "an authority died under load");
}

#[tokio::test]
#[ignore = "needs target/release/arena-server"]
async fn local_authority_failover() {
    init_tracing();
    let cfg = E2eConfig {
        nodes: 4,
        players: 150,
        hold: Duration::from_secs(15),
        ..Default::default()
    };
    let mut cluster = LocalCluster::new(cfg.nodes, server_bin(), &cfg.session);
    // Bring the fleet up under load, then crash node 2 and assert failover.
    scenario::run_scale(&mut cluster, &cfg).await.ok();
    let rep = fault::authority_failover(&mut cluster, 2, Duration::from_secs(10))
        .await
        .expect("failover scenario");
    cluster.teardown().await.ok();
    fault::assert_recovered(&rep, Duration::from_secs(10)).expect("recovered");
}

#[tokio::test]
#[ignore = "needs target/release/arena-server"]
async fn local_malicious_authority_is_flagged() {
    init_tracing();
    let cfg = E2eConfig {
        nodes: 5,
        players: 120,
        hold: Duration::from_secs(20),
        ..Default::default()
    };
    let mut cluster = LocalCluster::new(cfg.nodes, server_bin(), &cfg.session);
    scenario::run_scale(&mut cluster, &cfg).await.ok();
    // Node 3 is assumed started with --e2e-cheat by a variant of start_server in a
    // full harness; here we assert the detection plumbing wakes within budget.
    let rep = fault::malicious_authority(&mut cluster, 3, Duration::from_secs(30))
        .await
        .expect("malicious scenario");
    cluster.teardown().await.ok();
    assert!(
        rep.cheat_flagged,
        "cross-validation failed to flag the cheating authority"
    );
}

// ---------------------------------------------------------------------------
// Real VMs at scale + fault tolerance (Hetzner).
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "needs HETZNER_API_TOKEN + CE_SSH_KEY_* and provisions real VMs"]
async fn hetzner_scale_8_nodes_2000_players() {
    init_tracing();
    let cfg = E2eConfig {
        nodes: 8,
        players: 2_000,
        hold: Duration::from_secs(120),
        max_p99_rtt_ms: 150.0,
        min_tick_hz: 55.0,
        session: "hz-scale".to_string(),
    };
    let mut cluster = HetznerCluster::new(cfg.nodes, &cfg.session);
    cluster.provision().await.expect("provision VMs");
    cluster.deploy().await.expect("deploy arena-server over mesh");

    let report = scenario::run_scale(&mut cluster, &cfg).await;
    // Always tear down VMs, even on failure, so a failed run doesn't bill forever.
    cluster.teardown().await.ok();
    let report = report.expect("scale held");
    println!("{}", serde_json::to_string_pretty(&report).unwrap());
}

#[tokio::test]
#[ignore = "needs Hetzner; provisions real VMs and powers one off mid-match"]
async fn hetzner_failover_and_partition() {
    init_tracing();
    let cfg = E2eConfig {
        nodes: 6,
        players: 800,
        hold: Duration::from_secs(60),
        session: "hz-fault".to_string(),
        ..Default::default()
    };
    let mut cluster = HetznerCluster::new(cfg.nodes, &cfg.session);
    cluster.provision().await.expect("provision");
    cluster.deploy().await.expect("deploy");
    scenario::run_scale(&mut cluster, &cfg).await.ok();

    // Crash a VM, assert failover.
    let r1 = fault::authority_failover(&mut cluster, 1, Duration::from_secs(20)).await;
    // Netsplit another, hold, heal, assert no split-brain.
    let r2 = fault::partition_heal(
        &mut cluster,
        2,
        Duration::from_secs(15),
        Duration::from_secs(20),
    )
    .await;
    cluster.teardown().await.ok();

    fault::assert_recovered(&r1.unwrap(), Duration::from_secs(20)).unwrap();
    fault::assert_recovered(&r2.unwrap(), Duration::from_secs(35)).unwrap();
}

// ---------------------------------------------------------------------------
// Pure harness-logic tests (always run).
// ---------------------------------------------------------------------------

#[test]
fn latency_quantiles_are_exact() {
    let l = cerena_e2e::metrics::Latencies::new();
    for ms in 1..=100 {
        l.record(ms as f64);
    }
    let s = l.summary();
    assert_eq!(s.count, 100);
    assert!((s.p50_ms - 50.0).abs() <= 1.0);
    assert!((s.p99_ms - 99.0).abs() <= 1.0);
    assert_eq!(s.max_ms, 100.0);
}

#[test]
fn convergence_detects_divergent_authorities() {
    let log = cerena_e2e::metrics::ConvergenceLog::new();
    // Two honest reporters agree at (zone 0_0, tick 10).
    log.report("0_0", 10, "hashA", "node1");
    log.report("0_0", 10, "hashA", "node2");
    // A third disagrees -> divergence at that key.
    log.report("0_0", 10, "hashB", "node3");
    assert!(!log.fully_converged());
    let d = log.divergences();
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].zone, "0_0");
}
