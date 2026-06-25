//! The durable karma ledger.
//!
//! [`KarmaLedger`] is the system of record: a per-identity karma scalar plus an
//! append-only audit log of every [`KarmaUpdate`] ever applied. Conceptually this is
//! anchored on-chain — karma rides with a scarce CE node id — but here it is a local
//! JSON store the coordinator owns and republishes on the karma verdict topic. The
//! append-only log is what makes a ban *auditable*: any moderator (or a ce-gov
//! anti-proof vote) can replay exactly why a player ended up where they are.
//!
//! ## The fusion policy
//!
//! [`fuse`](KarmaLedger::fuse) is where the detector's statistical suspicion and the
//! aggregator's social report pressure meet. The deliberate stance:
//!
//! - **Telemetry leads, reports support.** A high detector score moves karma on its
//!   own; report pressure alone moves it only a little. This is what stops a brigade
//!   from banning an innocent — the statistical evidence has to be there too.
//! - **Graduated, banded enforcement.** The combined evidence becomes a karma *delta*;
//!   the resulting absolute karma decides the [`KarmaAction`] band. We only emit a
//!   [`KarmaUpdate`] when the action band actually changes (or a penalty is non-trivial),
//!   to keep the audit log meaningful and the verdict topic quiet.
//! - **A recovery path.** [`record_outcome`](KarmaLedger::record_outcome) grants small,
//!   capped karma for completing clean matches, so honest players climb back from a bad
//!   patch and the system is not purely punitive.
//!
//! Time is never read here: every mutating method takes `now_unix` from the caller, so
//! the ledger is deterministic and testable.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use arena_protocol::NodeId;
use arena_protocol::karma::{
    KARMA_DEFAULT, KARMA_PERMBAN, KarmaAction, KarmaUpdate, MatchOutcome, action_for,
};

use crate::detector::{SuspicionScore, penalty_for};

/// Hard floor on stored karma. We never let karma run away below the perm-ban line by
/// more than a margin — once perm-banned, deeper is meaningless, and a bounded value
/// keeps recovery math sane if an anti-proof vote later restores the account.
pub const KARMA_FLOOR: i32 = KARMA_PERMBAN - 50;
/// Hard ceiling on stored karma. Trusted is trusted; we cap so a veteran cannot bank
/// infinite goodwill and then cheat with impunity.
pub const KARMA_CEILING: i32 = 300;

/// Per-clean-match karma reward, before the K/D bonus.
const OUTCOME_BASE_REWARD: i32 = 1;
/// Extra karma for a positive, plausibly-clean K/D, capped so farming bots can't grind
/// karma by padding kills.
const OUTCOME_KD_BONUS_CAP: i32 = 2;

/// Weight applied to report pressure when fusing it into a karma penalty. Small on
/// purpose: reports support, telemetry leads.
const REPORT_PRESSURE_WEIGHT: f32 = 4.0;
/// Report pressure below this is treated as noise and ignored in fusion.
const REPORT_PRESSURE_FLOOR: f32 = 0.75;

/// The persisted shape. Kept separate-but-identical to the live struct so the on-disk
/// format is explicit and stable; `KarmaLedger` serializes via this directly.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct LedgerData {
    /// player -> absolute karma. Missing = [`KARMA_DEFAULT`].
    karma: HashMap<NodeId, i32>,
    /// Append-only audit log of every applied update, oldest first.
    audit: Vec<KarmaUpdate>,
}

/// The durable karma store: balances + audit log.
#[derive(Debug, Clone, Default)]
pub struct KarmaLedger {
    data: LedgerData,
}

impl KarmaLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current karma for a player, defaulting to [`KARMA_DEFAULT`] for an unseen id.
    pub fn karma(&self, player: &NodeId) -> i32 {
        self.data.karma.get(player).copied().unwrap_or(KARMA_DEFAULT)
    }

    /// The enforcement action a player's current karma lands them in.
    pub fn action(&self, player: &NodeId) -> KarmaAction {
        action_for(self.karma(player))
    }

    /// Append-only audit log (read-only view), oldest first.
    pub fn audit(&self) -> &[KarmaUpdate] {
        &self.data.audit
    }

    /// Apply a raw karma delta, clamp into the sane range, push an audit record, and
    /// return the resulting [`KarmaUpdate`]. `now_unix` is supplied by the caller; this
    /// method never reads the clock. `reason` is the human-readable justification.
    pub fn apply(&mut self, player: &NodeId, delta: i32, reason: impl Into<String>, now_unix: u64) -> KarmaUpdate {
        let current = self.karma(player);
        let new_karma = (current + delta).clamp(KARMA_FLOOR, KARMA_CEILING);
        // The effective delta after clamping (so audit + balance stay consistent).
        let effective_delta = new_karma - current;
        self.data.karma.insert(player.clone(), new_karma);

        let update = KarmaUpdate {
            player: player.clone(),
            karma: new_karma,
            delta: effective_delta,
            action: action_for(new_karma),
            reason: reason.into(),
            at_unix: now_unix,
        };
        self.data.audit.push(update.clone());
        update
    }

    /// Fuse detector suspicion and report pressure into a karma decision.
    ///
    /// Returns an emitted [`KarmaUpdate`] only when the combined evidence is strong
    /// enough to change the player's enforcement band, or to record a non-trivial
    /// penalty; otherwise returns `None` and leaves karma untouched. This keeps the
    /// audit log and the verdict topic signal-rich rather than chatty.
    pub fn fuse(
        &mut self,
        player: &NodeId,
        suspicion: Option<&SuspicionScore>,
        report_pressure: f32,
        now_unix: u64,
    ) -> Option<KarmaUpdate> {
        // Telemetry-driven penalty (already graduated and sample-confidence tempered).
        let detector_penalty = suspicion.map(|s| penalty_for(s.score)).unwrap_or(0);

        // Report-driven penalty: only meaningful pressure counts, scaled small.
        let report_penalty = if report_pressure >= REPORT_PRESSURE_FLOOR {
            -((report_pressure - REPORT_PRESSURE_FLOOR) * REPORT_PRESSURE_WEIGHT).round() as i32
        } else {
            0
        };

        // Reports may *amplify* a real detector signal but cannot, alone, exceed a mild
        // nudge: if the detector is silent, cap the report-only penalty hard so a brigade
        // cannot ban anyone without statistical backing.
        let report_penalty = if detector_penalty == 0 {
            report_penalty.max(-8)
        } else {
            report_penalty
        };

        let total = detector_penalty + report_penalty;
        if total == 0 {
            return None;
        }

        let before_action = self.action(player);

        // Build a justification from whatever fired.
        let mut why = Vec::new();
        if detector_penalty != 0 {
            if let Some(s) = suspicion {
                why.push(format!("anti-cheat suspicion {:.2}", s.score));
                why.extend(s.signals.iter().cloned());
            }
        }
        if report_penalty != 0 {
            why.push(format!("weighted report pressure {report_pressure:.2}"));
        }
        let reason = why.join("; ");

        let update = self.apply(player, total, reason, now_unix);
        let after_action = update.action;

        // Emit if the band changed, or if the penalty was non-trivial even within a band
        // (so escalating evidence is still recorded once it crosses ~one quarantine step).
        if after_action != before_action || total <= -15 {
            Some(update)
        } else {
            // The penalty applied (karma did move) but it was minor and didn't cross a
            // band; we already pushed it to the audit log via `apply`, so don't broadcast.
            None
        }
    }

    /// Reward clean play: a small, capped positive karma bump for completing a match,
    /// with a tiny K/D bonus. This is the recovery path. Returns the update if any karma
    /// was granted (none for a rage-quit / incomplete match).
    pub fn record_outcome(&mut self, outcome: &MatchOutcome, now_unix: u64) -> Option<KarmaUpdate> {
        // No reward for leaving early — completing the match is the anti-rage-quit gate.
        if !outcome.completed {
            return None;
        }

        let mut reward = OUTCOME_BASE_REWARD;
        // Positive contribution earns a small, capped bonus. Deaths in the denominator
        // keep a farming bot from grinding karma off lopsided stat-padding.
        if outcome.kills > outcome.deaths {
            let kd_edge = (outcome.kills - outcome.deaths) as i32;
            reward += kd_edge.min(OUTCOME_KD_BONUS_CAP);
        }
        // Assists are mild positive signal too, capped tightly.
        if outcome.assists > 0 {
            reward += 1;
        }

        // Don't bother once a player is already at the ceiling.
        if self.karma(&outcome.player) >= KARMA_CEILING {
            return None;
        }

        Some(self.apply(
            &outcome.player,
            reward,
            format!(
                "clean match: {}K/{}D/{}A completed",
                outcome.kills, outcome.deaths, outcome.assists
            ),
            now_unix,
        ))
    }

    /// Persist the ledger to JSON at `path` (pretty-printed for human auditability).
    pub fn save_json(&self, path: impl AsRef<Path>) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(&self.data)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }

    /// Load a ledger from JSON. A missing file yields an empty ledger (first run).
    pub fn load_json(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::new());
        }
        let bytes = std::fs::read(path)?;
        let data: LedgerData = serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        Ok(Self { data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::karma::{KARMA_QUARANTINE, KarmaAction};

    fn susp(player: &str, score: f32) -> SuspicionScore {
        SuspicionScore {
            player: player.to_string(),
            score,
            signals: vec!["test signal".to_string()],
        }
    }

    #[test]
    fn unseen_player_is_default_karma_and_no_action() {
        let l = KarmaLedger::new();
        let p = "newbie".to_string();
        assert_eq!(l.karma(&p), KARMA_DEFAULT);
        assert_eq!(l.action(&p), KarmaAction::None);
    }

    #[test]
    fn fuse_escalates_suspect_through_quarantine_into_tempban() {
        let mut l = KarmaLedger::new();
        let p = "suspect".to_string();

        // First strong-but-not-certain detection: drops from 100 toward quarantine.
        let u1 = l.fuse(&p, Some(&susp("suspect", 0.9)), 0.0, 1000).expect("update emitted");
        assert!(u1.delta < 0);
        // 100 - 70 = 30 → Quarantine band (<= 40).
        assert_eq!(u1.action, KarmaAction::Quarantine, "got {:?} at karma {}", u1.action, u1.karma);

        // Sustained near-certain detection: another big hit pushes into TempBan.
        let u2 = l.fuse(&p, Some(&susp("suspect", 0.97)), 0.0, 2000).expect("update emitted");
        // 30 - 160 clamps but lands well below the temp-ban line (<= 10).
        assert_eq!(u2.action, KarmaAction::TempBan, "got {:?} at karma {}", u2.action, u2.karma);

        // Audit log recorded both.
        assert_eq!(l.audit().len(), 2);
    }

    #[test]
    fn reports_alone_cannot_ban() {
        let mut l = KarmaLedger::new();
        let p = "reported".to_string();
        // Heavy report pressure but NO detector suspicion: capped to a mild nudge.
        let before = l.karma(&p);
        let _ = l.fuse(&p, None, 10.0, 1000);
        let after = l.karma(&p);
        assert!(before - after <= 8, "report-only penalty must be tightly capped: {before}->{after}");
        assert_eq!(l.action(&p), KarmaAction::None, "reports alone must not change the action band");
    }

    #[test]
    fn clean_match_grants_capped_recovery() {
        let mut l = KarmaLedger::new();
        let p = "good".to_string();
        // Knock them down first.
        l.apply(&p, -70, "test setup", 100);
        assert_eq!(l.action(&p), KarmaAction::Quarantine);

        let before = l.karma(&p);
        let outcome = MatchOutcome {
            player: p.clone(),
            entity: 1,
            kills: 20,
            deaths: 2,
            assists: 5,
            completed: true,
        };
        let u = l.record_outcome(&outcome, 200).expect("reward emitted");
        assert!(u.delta > 0 && u.delta <= OUTCOME_BASE_REWARD + OUTCOME_KD_BONUS_CAP + 1);
        assert!(l.karma(&p) > before);
    }

    #[test]
    fn rage_quit_earns_nothing() {
        let mut l = KarmaLedger::new();
        let outcome = MatchOutcome {
            player: "quitter".to_string(),
            entity: 1,
            kills: 30,
            deaths: 0,
            assists: 0,
            completed: false,
        };
        assert!(l.record_outcome(&outcome, 200).is_none());
    }

    #[test]
    fn json_roundtrip_preserves_karma_and_audit() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("arena-karma-test-{}.json", std::process::id()));

        let mut l = KarmaLedger::new();
        let p = "persisted".to_string();
        l.apply(&p, -65, "manual penalty", 42);
        assert_eq!(l.action(&p), KarmaAction::Quarantine);

        l.save_json(&path).expect("save");
        let reloaded = KarmaLedger::load_json(&path).expect("load");
        assert_eq!(reloaded.karma(&p), l.karma(&p));
        assert_eq!(reloaded.audit().len(), 1);
        assert!(reloaded.karma(&p) <= KARMA_QUARANTINE);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_is_empty_ledger() {
        let path = std::env::temp_dir().join("arena-karma-does-not-exist-xyz.json");
        let _ = std::fs::remove_file(&path);
        let l = KarmaLedger::load_json(&path).expect("missing file → empty ledger");
        assert_eq!(l.audit().len(), 0);
    }
}
