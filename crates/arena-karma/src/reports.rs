//! Player-report aggregation.
//!
//! Reports are a *social* signal, and social signals get gamed. A coordinated squad
//! can spam-report a player they lost to; a cheat can pre-emptively report the people
//! most likely to report it. So the [`ReportAggregator`] never converts reports
//! straight into punishment. It produces a single decayed, karma-weighted
//! [`pressure`](ReportAggregator::pressure) scalar that the ledger *fuses* with the
//! detector's statistical suspicion. A report-only case, with no telemetry backing,
//! barely moves karma.
//!
//! Two abuse defenses are built in:
//!
//! 1. **Reporter-karma weighting.** Each report counts proportionally to the
//!    reporter's own karma. A trusted, long-clean account's report is worth full
//!    weight; a quarantined account's report is worth ~0. This makes brigading with
//!    throwaway/low-karma accounts ineffective and makes a cheat's pre-emptive reports
//!    worthless once its own karma drops.
//! 2. **Brigade detection.** [`detect_brigading`](ReportAggregator::detect_brigading)
//!    flags reporters who pile onto high-karma, clean-profile players — itself a karma
//!    offense, so weaponizing the report system costs the abuser.
//!
//! Reports also decay across rounds, so stale grudges fade and only *sustained*
//! community pressure persists.

use std::collections::HashMap;
use std::collections::HashSet;

use arena_protocol::NodeId;
use arena_protocol::Tick;
use arena_protocol::karma::{Report, ReportReason};

use crate::detector::CheatDetector;

/// Severity multiplier per reason. Cheating accusations (aimbot/wallhack/speedhack)
/// weigh more than soft-social ones (toxicity/griefing), which are handled more by
/// conduct policy than anti-cheat.
fn reason_severity(reason: ReportReason) -> f32 {
    match reason {
        ReportReason::Aimbot => 1.0,
        ReportReason::Wallhack => 0.9,
        ReportReason::SpeedHack => 0.9,
        ReportReason::Teaming => 0.6,
        ReportReason::Griefing => 0.4,
        ReportReason::Toxicity => 0.3,
        ReportReason::Other => 0.25,
    }
}

/// Map a reporter's karma to a credibility weight in `0.0..=1.0`. A quarantined or
/// banned reporter (`karma <= KARMA_QUARANTINE`) counts for nothing; credibility ramps
/// up to full weight around the default-karma line and is capped there (a very high
/// karma reporter is trusted, not super-trusted, to avoid a single account dominating).
fn reporter_weight(reporter_karma: i32) -> f32 {
    use arena_protocol::karma::{KARMA_DEFAULT, KARMA_QUARANTINE};
    if reporter_karma <= KARMA_QUARANTINE {
        return 0.0;
    }
    let span = (KARMA_DEFAULT - KARMA_QUARANTINE).max(1) as f32;
    ((reporter_karma - KARMA_QUARANTINE) as f32 / span).clamp(0.0, 1.0)
}

/// Time-decay applied per round of age, so old reports fade. ~0.92/round ≈ half-life
/// of ~8 rounds.
const DECAY_PER_ROUND: f32 = 0.92;

/// Below this karma a player is *not* considered "high-karma clean" for the purposes of
/// brigade detection (we only protect the clearly-innocent from pile-ons).
const BRIGADE_PROTECT_KARMA: i32 = 90;

/// Above this aggregate weighted pressure on a single clean player, the reporters
/// involved are flagged as a probable brigade.
const BRIGADE_PRESSURE_THRESHOLD: f32 = 2.0;

/// One stored, weighted report. The reason is folded into `weight` at filing time via
/// [`reason_severity`], so it is not retained separately.
#[derive(Debug, Clone)]
struct StoredReport {
    reporter: NodeId,
    /// Combined credibility * severity weight at filing time.
    weight: f32,
    /// Round (tick) the report was filed at, for decay.
    tick: Tick,
}

/// Collects and de-duplicates reports, exposing decayed weighted pressure per accused.
#[derive(Debug, Default)]
pub struct ReportAggregator {
    /// accused -> its reports.
    by_accused: HashMap<NodeId, Vec<StoredReport>>,
    /// Dedup key set: (reporter, accused, round) — one report per reporter→accused per
    /// round. A second filing in the same round is ignored (no stacking).
    seen: HashSet<(NodeId, NodeId, Tick)>,
}

impl ReportAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// File a report, weighting it by the reporter's current karma and the reason's
    /// severity. Duplicate (reporter, accused, round) filings are dropped. Returns
    /// `true` if the report was accepted (not a duplicate, nonzero weight).
    pub fn file(&mut self, report: &Report, reporter_karma: i32) -> bool {
        let key = (report.reporter.clone(), report.accused.clone(), report.tick);
        if self.seen.contains(&key) {
            return false;
        }
        self.seen.insert(key);

        let cred = reporter_weight(reporter_karma);
        let weight = cred * reason_severity(report.reason);

        self.by_accused
            .entry(report.accused.clone())
            .or_default()
            .push(StoredReport {
                reporter: report.reporter.clone(),
                weight,
                tick: report.tick,
            });

        // A zero-weight report (quarantined reporter) is still recorded for audit but
        // signals nothing.
        weight > 0.0
    }

    /// Aggregate, time-decayed, karma-weighted report pressure on an accused player,
    /// evaluated as of `now_tick`. Returns `0.0` for an unreported player.
    ///
    /// This is intentionally *not* a probability and *not* an action — it is a prior the
    /// ledger fuses with detector suspicion. Distinct reporters matter more than one
    /// reporter shouting repeatedly (dedup already prevents same-round stacking, and
    /// cross-round reports from the same reporter add with diminishing effect here).
    pub fn pressure(&self, accused: &NodeId, now_tick: Tick) -> f32 {
        let Some(reports) = self.by_accused.get(accused) else {
            return 0.0;
        };

        // Diminish repeated reports from the same reporter so one persistent griefer
        // cannot manufacture pressure: collect each reporter's decayed report weights,
        // then count the strongest fully and the rest at 25%.
        let mut per_reporter: HashMap<&NodeId, Vec<f32>> = HashMap::new();
        for r in reports {
            let age = now_tick.saturating_sub(r.tick);
            // Decay smoothly by round-age (tick distance scaled to rounds).
            let rounds = age as f32 / TICKS_PER_ROUND;
            let decayed = r.weight * DECAY_PER_ROUND.powf(rounds);
            per_reporter.entry(&r.reporter).or_default().push(decayed);
        }

        let mut total = 0.0_f32;
        for mut weights in per_reporter.into_values() {
            // Strongest report from this reporter counts fully; extras at 25%.
            weights.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
            for (i, w) in weights.into_iter().enumerate() {
                total += if i == 0 { w } else { 0.25 * w };
            }
        }
        total
    }

    /// Flag reporters who appear to be brigading: piling reports onto players who are
    /// both high-karma *and* statistically clean (low detector suspicion). Filing such
    /// reports is itself a karma offense — this returns the offending reporters so the
    /// caller can penalize them. `detector` supplies the statistical innocence check;
    /// `karma_of` resolves the accused's current karma.
    pub fn detect_brigading(
        &self,
        now_tick: Tick,
        detector: &CheatDetector,
        karma_of: impl Fn(&NodeId) -> i32,
    ) -> Vec<NodeId> {
        let mut offenders: HashSet<NodeId> = HashSet::new();

        for (accused, reports) in &self.by_accused {
            // Only protect the clearly-innocent: high karma.
            if karma_of(accused) < BRIGADE_PROTECT_KARMA {
                continue;
            }
            // And statistically clean: detector either doesn't know them or scores low.
            let suspicious = detector
                .evaluate(accused)
                .map(|s| s.score >= 0.5)
                .unwrap_or(false);
            if suspicious {
                continue;
            }
            // If there is heavy aggregate pressure on this clean player, the distinct
            // reporters involved are probably brigading.
            if self.pressure(accused, now_tick) >= BRIGADE_PRESSURE_THRESHOLD {
                for r in reports {
                    // A report still carrying weight counts as participation.
                    if r.weight > 0.0 {
                        offenders.insert(r.reporter.clone());
                    }
                }
            }
        }

        let mut out: Vec<NodeId> = offenders.into_iter().collect();
        out.sort();
        out
    }

    /// Number of distinct reporters against an accused (for moderation display).
    pub fn distinct_reporters(&self, accused: &NodeId) -> usize {
        self.by_accused
            .get(accused)
            .map(|v| v.iter().map(|r| &r.reporter).collect::<HashSet<_>>().len())
            .unwrap_or(0)
    }

    /// Drop all stored reports older than `keep_rounds` rounds relative to `now_tick`,
    /// to bound memory. Fully-decayed reports contribute ~nothing anyway.
    pub fn prune(&mut self, now_tick: Tick, keep_rounds: u32) {
        let max_age = keep_rounds.saturating_mul(TICKS_PER_ROUND as u32);
        for reports in self.by_accused.values_mut() {
            reports.retain(|r| now_tick.saturating_sub(r.tick) <= max_age);
        }
        self.by_accused.retain(|_, v| !v.is_empty());
        // Note: `seen` keeps growing slowly; in a long-lived service the coordinator
        // resets aggregators per session, which clears it. Kept simple deliberately.
    }
}

/// Ticks we treat as one "round" for decay/pruning math. A round is far longer than a
/// snapshot; this is a smoothing constant, not a wire value, so it lives here.
const TICKS_PER_ROUND: f32 = (arena_protocol::TICK_HZ as f32) * 90.0; // ~90s rounds

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::karma::{KARMA_DEFAULT, KARMA_QUARANTINE, Report, ReportReason};
    use arena_protocol::world::MapId;

    fn report(reporter: &str, accused: &str, reason: ReportReason, tick: u32) -> Report {
        Report {
            reporter: reporter.to_string(),
            accused: accused.to_string(),
            reason,
            note: String::new(),
            tick,
            map: MapId("test-map".to_string()),
        }
    }

    #[test]
    fn quarantined_reporter_is_ignored() {
        let mut agg = ReportAggregator::new();
        // A quarantined reporter files an aimbot report.
        let accepted = agg.file(&report("quarantined", "victim", ReportReason::Aimbot, 0), KARMA_QUARANTINE);
        assert!(!accepted, "a quarantined reporter's report must carry no weight");
        assert_eq!(agg.pressure(&"victim".to_string(), 0), 0.0);

        // A trusted reporter, by contrast, produces real pressure.
        let accepted = agg.file(&report("trusted", "victim", ReportReason::Aimbot, 0), KARMA_DEFAULT);
        assert!(accepted);
        assert!(agg.pressure(&"victim".to_string(), 0) > 0.0);
    }

    #[test]
    fn same_round_duplicate_is_dropped() {
        let mut agg = ReportAggregator::new();
        assert!(agg.file(&report("a", "b", ReportReason::Wallhack, 5), KARMA_DEFAULT));
        // Same reporter, same accused, same round → dropped.
        assert!(!agg.file(&report("a", "b", ReportReason::Wallhack, 5), KARMA_DEFAULT));
        assert_eq!(agg.distinct_reporters(&"b".to_string()), 1);
    }

    #[test]
    fn pressure_decays_over_rounds() {
        let mut agg = ReportAggregator::new();
        agg.file(&report("t", "x", ReportReason::Aimbot, 0), KARMA_DEFAULT);
        let fresh = agg.pressure(&"x".to_string(), 0);
        let later = agg.pressure(&"x".to_string(), (TICKS_PER_ROUND as u32) * 10);
        assert!(later < fresh, "pressure should decay with age: {later} !< {fresh}");
    }

    #[test]
    fn brigading_against_clean_high_karma_player_is_flagged() {
        let mut agg = ReportAggregator::new();
        let detector = CheatDetector::new(); // knows nothing → treats victim as clean
        // Many trusted reporters pile onto a clean, high-karma victim.
        for i in 0..8 {
            agg.file(
                &report(&format!("brigadier{i}"), "innocent", ReportReason::Aimbot, 0),
                KARMA_DEFAULT,
            );
        }
        let offenders = agg.detect_brigading(0, &detector, |_| KARMA_DEFAULT);
        assert!(
            offenders.len() >= 5,
            "the brigade should be flagged, got {offenders:?}"
        );
    }
}
