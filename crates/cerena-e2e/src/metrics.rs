//! Measurement: latency histograms, throughput counters, and a convergence
//! checker. Kept dependency-light (a plain sorted-sample quantile, not hdrhistogram)
//! so the harness has no heavy deps.

use std::collections::HashMap;
use std::sync::Mutex;

/// A cheap quantile sketch: just collects samples and sorts on demand. Fine for
//  e2e runs (tens of thousands of samples), and exact rather than approximate.
#[derive(Default)]
pub struct Latencies {
    samples_ms: Mutex<Vec<f64>>,
}

impl Latencies {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, ms: f64) {
        if ms.is_finite() && ms >= 0.0 {
            self.samples_ms.lock().unwrap().push(ms);
        }
    }

    pub fn count(&self) -> usize {
        self.samples_ms.lock().unwrap().len()
    }

    /// Quantile in [0,1]. Returns NaN if empty.
    pub fn quantile(&self, q: f64) -> f64 {
        let mut v = self.samples_ms.lock().unwrap().clone();
        if v.is_empty() {
            return f64::NAN;
        }
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((v.len() as f64 - 1.0) * q.clamp(0.0, 1.0)).round() as usize;
        v[idx]
    }

    pub fn mean(&self) -> f64 {
        let v = self.samples_ms.lock().unwrap();
        if v.is_empty() {
            return f64::NAN;
        }
        v.iter().sum::<f64>() / v.len() as f64
    }

    pub fn summary(&self) -> LatencySummary {
        LatencySummary {
            count: self.count(),
            mean_ms: self.mean(),
            p50_ms: self.quantile(0.50),
            p95_ms: self.quantile(0.95),
            p99_ms: self.quantile(0.99),
            max_ms: self.quantile(1.0),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LatencySummary {
    pub count: usize,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

/// Whole-run scale report, serialized to JSON at the end of a scenario.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScaleReport {
    pub nodes: usize,
    pub peak_players: usize,
    pub zones_active: usize,
    /// Snapshot round-trip latency (client send input -> see its effect).
    pub snapshot_rtt: LatencySummary,
    /// Measured authority tick rate, Hz, averaged across the fleet.
    pub tick_hz: f64,
    /// Mean downstream bandwidth per player, bytes/sec.
    pub bytes_per_player_s: f64,
    /// Inputs the server dropped as too-late / out-of-window, fraction.
    pub dropped_input_frac: f64,
    /// Did the fleet keep all authorities live for the whole hold window?
    pub all_authorities_live: bool,
}

/// Convergence checker: every authority periodically reports the `state_hash` of
/// each zone it owns at a given tick. A healthy distributed sim has, for any
/// (zone, tick), exactly one hash across all reporters (the owner's truth) and any
/// shadow validators agreeing. Disagreement = divergence (a bug) or a malicious
/// authority (which `arena-karma::CrossValidator` should already have flagged).
#[derive(Default)]
pub struct ConvergenceLog {
    /// (zone_token, tick) -> { hash_hex -> set of reporter node ids }.
    by_key: Mutex<HashMap<(String, u32), HashMap<String, Vec<String>>>>,
}

impl ConvergenceLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn report(&self, zone: &str, tick: u32, hash_hex: &str, node: &str) {
        let mut m = self.by_key.lock().unwrap();
        m.entry((zone.to_string(), tick))
            .or_default()
            .entry(hash_hex.to_string())
            .or_default()
            .push(node.to_string());
    }

    /// All (zone,tick) keys where reporters disagreed on the hash.
    pub fn divergences(&self) -> Vec<Divergence> {
        let m = self.by_key.lock().unwrap();
        let mut out = vec![];
        for ((zone, tick), hashes) in m.iter() {
            if hashes.len() > 1 {
                out.push(Divergence {
                    zone: zone.clone(),
                    tick: *tick,
                    hashes: hashes
                        .iter()
                        .map(|(h, nodes)| (h.clone(), nodes.clone()))
                        .collect(),
                });
            }
        }
        out
    }

    /// True if every reported (zone,tick) had a single agreed hash.
    pub fn fully_converged(&self) -> bool {
        self.divergences().is_empty()
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Divergence {
    pub zone: String,
    pub tick: u32,
    /// hash -> reporters that produced it.
    pub hashes: Vec<(String, Vec<String>)>,
}
