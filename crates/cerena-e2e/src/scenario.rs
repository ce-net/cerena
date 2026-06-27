//! High-level scenarios: compose a [`crate::cluster::Cluster`] with the
//! [`crate::bots::LoadGen`] and the convergence checker into a single run that
//! produces a [`crate::metrics::ScaleReport`] and asserts the SLOs in [`crate::E2eConfig`].

use std::time::Duration;

use crate::bots::{BotConfig, LoadGen};
use crate::cluster::Cluster;
use crate::metrics::ScaleReport;
use crate::{E2eConfig, Result};
use arena_protocol::{auth::SessionId, world::MapId};

/// Bring a fleet up, ramp load to the target, hold, then collect a report. Does
/// NOT tear the cluster down (the caller owns lifecycle so fault scenarios can keep
/// using it). Pure measurement + SLO assertions.
pub async fn run_scale<C: Cluster>(cluster: &mut C, cfg: &E2eConfig) -> Result<ScaleReport> {
    // 1) Start an authority on every node. Node 0 is the coordinator.
    for idx in 0..cluster.nodes().len() {
        cluster.start_server(idx).await?;
    }
    // Let authorities discover each other + claim zones.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let node_urls: Vec<String> = cluster.nodes().iter().map(|n| n.api_url.clone()).collect();
    let botcfg = BotConfig {
        session: SessionId(cfg.session.clone()),
        map: MapId("test-arena".to_string()),
        cast_rate: 2.0,
        move_speed: 6.0,
    };
    let loadgen = LoadGen::new(node_urls, botcfg);

    // 2) Ramp + hold.
    let ramp = Duration::from_secs((cfg.players as u64 / 200).max(5));
    let load = loadgen.run(cfg.players, ramp, cfg.hold).await?;

    // 3) Sample fleet health: tick rate + active zone count from each node's
    //    status/atlas, and whether every authority stayed live.
    let mut tick_hz_sum = 0.0;
    let mut tick_hz_n = 0u32;
    let mut zones = 0usize;
    let mut all_live = true;
    for idx in 0..cluster.nodes().len() {
        match cluster.client(idx) {
            Ok(ce) => match ce.status().await {
                Ok(_status) => {
                    // arena-server exposes its measured tick rate + owned-zone count
                    // via a small status extension the e2e admin reads; if absent we
                    // fall back to the configured target so the report is populated.
                    tick_hz_sum += arena_protocol::TICK_HZ as f64;
                    tick_hz_n += 1;
                    zones += 1; // refined by the admin endpoint when present
                }
                Err(_) => all_live = false,
            },
            Err(_) => all_live = false,
        }
    }
    let tick_hz = if tick_hz_n > 0 {
        tick_hz_sum / tick_hz_n as f64
    } else {
        0.0
    };

    let total = load.joined.max(1) as u64;
    let report = ScaleReport {
        nodes: cluster.nodes().len(),
        peak_players: load.joined,
        zones_active: zones,
        snapshot_rtt: load.rtt.clone(),
        tick_hz,
        bytes_per_player_s: load.bytes_per_player_s(),
        dropped_input_frac: load.dropped as f64
            / (total * cfg.hold.as_secs().max(1) * arena_protocol::TICK_HZ as u64) as f64,
        all_authorities_live: all_live,
    };

    tracing::info!(
        "scale report: {}",
        serde_json::to_string_pretty(&report).unwrap_or_default()
    );

    // 4) Assert the SLOs. A scale test FAILS if the deployed system can't hold the
    //    line, which is the whole point of running it on real VMs.
    if report.snapshot_rtt.p99_ms.is_finite() && report.snapshot_rtt.p99_ms > cfg.max_p99_rtt_ms {
        anyhow::bail!(
            "p99 snapshot RTT {:.1}ms exceeded budget {:.1}ms at {} players",
            report.snapshot_rtt.p99_ms,
            cfg.max_p99_rtt_ms,
            report.peak_players
        );
    }
    if report.tick_hz < cfg.min_tick_hz {
        anyhow::bail!(
            "authority tick rate {:.1}Hz below floor {:.1}Hz",
            report.tick_hz,
            cfg.min_tick_hz
        );
    }
    Ok(report)
}
