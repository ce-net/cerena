//! Statistical anti-cheat for *clients*.
//!
//! The [`CheatDetector`] ingests one [`CheatTelemetry`] record per player per round
//! and maintains a rolling profile (the last [`PROFILE_WINDOW`] rounds plus a few
//! EWMA-smoothed rates). From that profile it produces a probabilistic
//! [`SuspicionScore`] in `0.0..=1.0`.
//!
//! ## Design stance: no single signal bans
//!
//! Every signal here is a *prior*, not a proof. A world-class player legitimately
//! posts high accuracy and a high headshot fraction; a streamer on a good day flicks
//! fast. What separates a human from a cheat is (1) the *combination* of signals and
//! (2) *sustained* extremity over a large sample. So:
//!
//! - Each contribution is weighted by sample size (`shots_fired`, rounds observed),
//!   so three lucky shots cannot move the needle.
//! - Scores are continuous and additive-then-saturated, so one borderline signal
//!   yields mild suspicion while several stacked signals approach certainty.
//! - The detector never bans. It emits a score; [`penalty_for`] maps that score to a
//!   karma *delta*, and the ledger's banding decides the enforcement action. Penalties
//!   escalate with suspicion rather than tripping a binary switch.
//!
//! The one near-binary signal is `firerate_violations`: the authority only counts a
//! violation when a fire input beats the weapon's hard fire-rate gate, which an
//! unmodified client physically cannot do. A nonzero sustained count is close to proof
//! of a modified client, so it carries the heaviest weight — but even it is fused, not
//! absolute, to survive the occasional false positive from clock skew or a dropped tick.

use std::collections::HashMap;
use std::collections::VecDeque;

use arena_protocol::NodeId;
use arena_protocol::karma::CheatTelemetry;

/// How many recent rounds we retain per player. Long enough that sustained cheating
/// dominates noise, short enough that a reformed account (new key notwithstanding)
/// can age out an early bad streak.
pub const PROFILE_WINDOW: usize = 12;

/// Accuracy a strong human tops out around across many shots. Pro hitscan players
/// sit well under this over a full session; sustained accuracy above it, on a large
/// sample, is the canonical aimbot tell.
pub const HUMAN_ACCURACY_CEILING: f32 = 0.45;

/// Sustained headshot fraction above this (snipers aside) is suspicious — aimbots
/// snap to the head hitbox.
pub const HUMAN_HEADSHOT_CEILING: f32 = 0.70;

/// Human visual reaction floor in milliseconds. Elite players land first shots around
/// 150-200 ms; the literature puts raw simple-reaction near 120 ms. Sustained medians
/// below this are not reflexes.
pub const HUMAN_REACTION_FLOOR_MS: u32 = 120;

/// Sustained median reaction below this is triggerbot territory: the client is firing
/// on visibility, not on a human decision.
pub const TRIGGERBOT_REACTION_MS: u32 = 60;

/// Below this many shots in a round, accuracy/headshot stats are treated as noise and
/// contribute little — this is the lucky-streak guard.
pub const MIN_SHOTS_FOR_ACCURACY: u32 = 40;

/// Smoothing factor for the exponentially-weighted moving averages. Higher = more
/// weight on the latest round.
const EWMA_ALPHA: f32 = 0.35;

/// Per-signal weights. They need not sum to 1; the raw weighted sum is squashed into
/// `0..1` by [`squash`]. The relative magnitudes encode how damning each signal is.
const W_ACCURACY: f32 = 1.6;
const W_HEADSHOT: f32 = 1.0;
const W_AIMSNAP: f32 = 1.2;
const W_MOVE: f32 = 1.1;
const W_FIRERATE: f32 = 3.0; // near-proof of a modified client
const W_REACTION: f32 = 1.8;

/// A rolling per-player anti-cheat profile. Cheap to keep in memory for every active
/// player; serialization is the ledger's job, not the detector's.
#[derive(Debug, Clone)]
struct Profile {
    /// Most recent telemetry records, newest at the back, capped at [`PROFILE_WINDOW`].
    rounds: VecDeque<CheatTelemetry>,
    /// EWMA of per-round accuracy (only updated on rounds with enough shots).
    ewma_accuracy: Option<f32>,
    /// EWMA of headshot fraction.
    ewma_headshot: Option<f32>,
    /// EWMA of median reaction time (ms).
    ewma_reaction: Option<f32>,
    /// Total shots observed across the retained window — the sample-size weight.
    total_shots: u64,
    /// Accumulated anomaly counters across the window.
    total_aim_snaps: u64,
    total_move_corrections: u64,
    total_firerate_violations: u64,
}

impl Profile {
    fn new() -> Self {
        Self {
            rounds: VecDeque::with_capacity(PROFILE_WINDOW),
            ewma_accuracy: None,
            ewma_headshot: None,
            ewma_reaction: None,
            total_shots: 0,
            total_aim_snaps: 0,
            total_move_corrections: 0,
            total_firerate_violations: 0,
        }
    }

    fn observe(&mut self, t: &CheatTelemetry) {
        // Evict the oldest round once the window is full, rolling its counters out.
        if self.rounds.len() == PROFILE_WINDOW {
            if let Some(old) = self.rounds.pop_front() {
                self.total_shots = self.total_shots.saturating_sub(old.shots_fired as u64);
                self.total_aim_snaps = self.total_aim_snaps.saturating_sub(old.aim_snap_events as u64);
                self.total_move_corrections =
                    self.total_move_corrections.saturating_sub(old.move_corrections as u64);
                self.total_firerate_violations = self
                    .total_firerate_violations
                    .saturating_sub(old.firerate_violations as u64);
            }
        }

        self.total_shots += t.shots_fired as u64;
        self.total_aim_snaps += t.aim_snap_events as u64;
        self.total_move_corrections += t.move_corrections as u64;
        self.total_firerate_violations += t.firerate_violations as u64;

        // Only let well-sampled rounds steer the accuracy/headshot EWMAs; otherwise a
        // 1-for-1 round would read as 100% accuracy and poison the average.
        if t.shots_fired >= MIN_SHOTS_FOR_ACCURACY {
            self.ewma_accuracy = Some(ewma(self.ewma_accuracy, t.accuracy()));
            self.ewma_headshot = Some(ewma(self.ewma_headshot, t.headshot_frac));
        }
        // Reaction time is reported as a median already; trust it when nonzero.
        if t.median_reaction_ms > 0 {
            self.ewma_reaction = Some(ewma(self.ewma_reaction, t.median_reaction_ms as f32));
        }

        self.rounds.push_back(t.clone());
    }

    fn rounds_observed(&self) -> usize {
        self.rounds.len()
    }
}

/// Update an EWMA with a new sample, seeding it on first observation.
fn ewma(prev: Option<f32>, sample: f32) -> f32 {
    match prev {
        Some(p) => EWMA_ALPHA * sample + (1.0 - EWMA_ALPHA) * p,
        None => sample,
    }
}

/// Logistic squash mapping an unbounded non-negative weighted sum into `0..1`. A sum of
/// 0 maps to 0; growing evidence saturates toward 1 without ever quite reaching it.
fn squash(weighted_sum: f32) -> f32 {
    if weighted_sum <= 0.0 {
        return 0.0;
    }
    // 1 - e^-x: smooth, monotonic, hits ~0.63 at x=1 and ~0.95 at x=3.
    1.0 - (-weighted_sum).exp()
}

/// Confidence multiplier from sample size: a profile with very few total shots cannot
/// be very suspicious no matter how extreme the rates look. Ramps from ~0 to 1 as the
/// player accrues shots, full confidence by a few hundred shots.
fn sample_confidence(total_shots: u64) -> f32 {
    let n = total_shots as f32;
    // Saturating ramp; 200 shots ≈ 0.63 confidence, 600 ≈ 0.95.
    1.0 - (-(n / 200.0)).exp()
}

/// The detector's probabilistic verdict for one player. `score` in `0.0..=1.0`;
/// `signals` lists the human-readable reasons that contributed, for moderation logs.
#[derive(Debug, Clone, PartialEq)]
pub struct SuspicionScore {
    pub player: NodeId,
    /// Fused suspicion in `0.0..=1.0`. Higher = more likely cheating.
    pub score: f32,
    /// Why — one entry per signal that fired, newest analysis.
    pub signals: Vec<String>,
}

/// Rolling statistical anti-cheat over per-round telemetry.
#[derive(Debug, Default)]
pub struct CheatDetector {
    profiles: HashMap<NodeId, Profile>,
}

impl CheatDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest one round of telemetry for a player, updating its rolling profile.
    pub fn observe(&mut self, t: &CheatTelemetry) {
        self.profiles
            .entry(t.player.clone())
            .or_insert_with(Profile::new)
            .observe(t);
    }

    /// Evaluate a player's current profile into a [`SuspicionScore`].
    ///
    /// Returns `None` if the player is unknown. Returns a score (possibly `0.0`) once a
    /// profile exists; callers typically ignore scores below a policy threshold.
    pub fn evaluate(&self, player: &NodeId) -> Option<SuspicionScore> {
        let p = self.profiles.get(player)?;

        let mut weighted = 0.0_f32;
        let mut signals = Vec::new();

        // --- Accuracy: how far above the human ceiling, scaled by sample size. ---
        if let Some(acc) = p.ewma_accuracy {
            if acc > HUMAN_ACCURACY_CEILING {
                // Normalize the overshoot by the room between ceiling and perfect.
                let over = (acc - HUMAN_ACCURACY_CEILING) / (1.0 - HUMAN_ACCURACY_CEILING);
                let contrib = W_ACCURACY * over;
                weighted += contrib;
                signals.push(format!(
                    "sustained accuracy {:.0}% over {} shots (human ceiling {:.0}%)",
                    acc * 100.0,
                    p.total_shots,
                    HUMAN_ACCURACY_CEILING * 100.0
                ));
            }
        }

        // --- Headshot fraction: aimbots favor the head hitbox. ---
        if let Some(hs) = p.ewma_headshot {
            if hs > HUMAN_HEADSHOT_CEILING {
                let over = (hs - HUMAN_HEADSHOT_CEILING) / (1.0 - HUMAN_HEADSHOT_CEILING);
                let contrib = W_HEADSHOT * over;
                weighted += contrib;
                signals.push(format!(
                    "sustained headshot fraction {:.0}% (suspicious above {:.0}%)",
                    hs * 100.0,
                    HUMAN_HEADSHOT_CEILING * 100.0
                ));
            }
        }

        // --- Aim-snap rate: flick-aimbot prior. Rate per round, lightly compressed. ---
        let rounds = p.rounds_observed().max(1) as f32;
        let aimsnap_rate = p.total_aim_snaps as f32 / rounds;
        if aimsnap_rate > 0.5 {
            // log1p compresses huge counts; ~1 contribution at a few snaps/round.
            let contrib = W_AIMSNAP * (aimsnap_rate).ln_1p() / 3.0;
            weighted += contrib;
            signals.push(format!("{:.1} aim-snap clamps/round", aimsnap_rate));
        }

        // --- Movement corrections: speed/teleport prior. ---
        let move_rate = p.total_move_corrections as f32 / rounds;
        if move_rate > 0.5 {
            let contrib = W_MOVE * (move_rate).ln_1p() / 3.0;
            weighted += contrib;
            signals.push(format!("{:.1} movement corrections/round (speed/teleport)", move_rate));
        }

        // --- Fire-rate violations: near-proof of a modified client. ---
        if p.total_firerate_violations > 0 {
            let viol_rate = p.total_firerate_violations as f32 / rounds;
            // Strong, fast-saturating: even ~1/round is a heavy contribution.
            let contrib = W_FIRERATE * (viol_rate).ln_1p() / 1.5;
            weighted += contrib;
            signals.push(format!(
                "{} fire-rate violations (modified client; {:.1}/round)",
                p.total_firerate_violations, viol_rate
            ));
        }

        // --- Reaction time: sub-human = triggerbot. ---
        if let Some(rt) = p.ewma_reaction {
            if rt < HUMAN_REACTION_FLOOR_MS as f32 {
                // Deeper below the floor = stronger; below the triggerbot line, max it.
                let span = HUMAN_REACTION_FLOOR_MS.saturating_sub(TRIGGERBOT_REACTION_MS) as f32;
                let depth = ((HUMAN_REACTION_FLOOR_MS as f32 - rt) / span).clamp(0.0, 1.0);
                let contrib = W_REACTION * depth;
                weighted += contrib;
                signals.push(format!(
                    "median reaction {:.0} ms (human floor {} ms)",
                    rt, HUMAN_REACTION_FLOOR_MS
                ));
            }
        }

        // Squash to 0..1, then temper by overall sample confidence so a thin profile
        // cannot reach a damning score off a couple of rounds.
        let raw = squash(weighted);
        let score = (raw * sample_confidence(p.total_shots)).clamp(0.0, 1.0);

        Some(SuspicionScore {
            player: player.clone(),
            score,
            signals,
        })
    }

    /// Whether we hold any profile for this player yet.
    pub fn knows(&self, player: &NodeId) -> bool {
        self.profiles.contains_key(player)
    }

    /// Drop a player's profile (e.g. after a perm-ban is finalized, to reclaim memory).
    pub fn forget(&mut self, player: &NodeId) {
        self.profiles.remove(player);
    }
}

/// Map a suspicion score to a karma delta (always `<= 0`; the detector only penalizes).
///
/// Graduated, not binary: a borderline score nudges karma down a little, a near-certain
/// score takes a big bite. The ledger's banding then decides whether the resulting karma
/// crosses into `Quarantine`, `TempBan`, or `PermBan`. Below a floor the detector is
/// silent — it is not confident enough to touch the player at all.
pub fn penalty_for(score: f32) -> i32 {
    let s = score.clamp(0.0, 1.0);
    if s < 0.5 {
        // Not confident enough; leave karma alone and let reports/time decide.
        0
    } else if s < 0.7 {
        -10
    } else if s < 0.85 {
        -30
    } else if s < 0.95 {
        -70
    } else {
        // Near-certain (e.g. stacked fire-rate + triggerbot signals): straight to a ban band.
        -160
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::karma::CheatTelemetry;

    fn tel(player: &str, fired: u32, hit: u32, hs: f32, snaps: u32, moves: u32, fr: u32, rt: u32) -> CheatTelemetry {
        CheatTelemetry {
            player: player.to_string(),
            shots_fired: fired,
            shots_hit: hit,
            headshot_frac: hs,
            aim_snap_events: snaps,
            move_corrections: moves,
            firerate_violations: fr,
            median_reaction_ms: rt,
        }
    }

    #[test]
    fn obvious_aimbot_is_flagged() {
        let mut d = CheatDetector::new();
        let player = "aimbot".to_string();
        // 90% accuracy over 200 shots/round, mostly headshots, fast reactions, for
        // several rounds — every prior screams cheat and the sample is huge.
        for _ in 0..6 {
            d.observe(&tel("aimbot", 200, 180, 0.85, 6, 0, 0, 70));
        }
        let s = d.evaluate(&player).expect("profile exists");
        assert!(
            s.score > 0.9,
            "aimbot should score very high, got {} ({:?})",
            s.score,
            s.signals
        );
        assert!(penalty_for(s.score) <= -70, "should incur a heavy penalty");
        assert!(!s.signals.is_empty());
    }

    #[test]
    fn lucky_small_sample_is_not_flagged() {
        let mut d = CheatDetector::new();
        let player = "lucky".to_string();
        // 3 shots, 3 hits, all headshots, one round. 100% accuracy but tiny sample.
        d.observe(&tel("lucky", 3, 3, 1.0, 0, 0, 0, 200));
        let s = d.evaluate(&player).expect("profile exists");
        assert!(
            s.score < 0.3,
            "a 3-shot lucky streak must not be damning, got {} ({:?})",
            s.score,
            s.signals
        );
        assert_eq!(penalty_for(s.score), 0, "low score → no penalty");
    }

    #[test]
    fn skilled_human_stays_below_ban_band() {
        let mut d = CheatDetector::new();
        let player = "pro".to_string();
        // A very good but human player: ~40% accuracy, normal headshots, normal reaction,
        // zero modified-client signals, over a big sample.
        for _ in 0..8 {
            d.observe(&tel("pro", 150, 60, 0.5, 0, 0, 0, 180));
        }
        let s = d.evaluate(&player).expect("profile exists");
        assert!(
            s.score < 0.5,
            "a skilled but clean human should not trip the penalty floor, got {} ({:?})",
            s.score,
            s.signals
        );
        assert_eq!(penalty_for(s.score), 0);
    }

    #[test]
    fn firerate_violation_is_a_strong_signal() {
        let mut d = CheatDetector::new();
        let player = "modded".to_string();
        // Otherwise unremarkable, but the client physically beat the fire-rate gate.
        for _ in 0..4 {
            d.observe(&tel("modded", 100, 35, 0.4, 0, 0, 5, 160));
        }
        let s = d.evaluate(&player).expect("profile exists");
        assert!(
            s.score > 0.5,
            "fire-rate violations should push past the penalty floor, got {}",
            s.score
        );
    }
}
