//! Reporting, anti-cheat telemetry, and karma deltas.
//!
//! Karma is a per-identity reputation scalar that gates play. Because identity is
//! a scarce CE node id, karma is durable: it rides with the player across sessions
//! and is anchored on-chain (the karma service issues `KarmaUpdate` records that
//! reference the node id). This module is the shared *shape*; the policy lives in
//! `arena-karma`.

use serde::{Deserialize, Serialize};

use crate::{EntityId, NodeId, Tick, world::MapId};

/// Why a player is reporting another. Coarse buckets keep the UI simple and the
/// aggregation meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ReportReason {
    Aimbot = 0,
    Wallhack = 1,
    SpeedHack = 2,
    Griefing = 3,
    Toxicity = 4,
    Teaming = 5,
    Other = 6,
}

/// A player-filed report against another player. Filed through the session
/// authority, which attaches the round context the reporter cannot forge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub reporter: NodeId,
    pub accused: NodeId,
    pub reason: ReportReason,
    /// Free-text, length-capped server side.
    pub note: String,
    /// Server tick at which the report was filed (round context).
    pub tick: Tick,
    pub map: MapId,
}

/// A statistical observation the authority emits about a player every round.
/// `arena-karma` aggregates these across rounds/authorities to flag cheats that
/// no single round proves. None of these are individually conclusive — they are
/// priors that combine with reports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheatTelemetry {
    pub player: NodeId,
    /// Shots fired vs shots that hit (accuracy). Inhuman sustained accuracy is a
    /// strong aimbot prior.
    pub shots_fired: u32,
    pub shots_hit: u32,
    /// Fraction of hits that were headshots. Snipers aside, sustained >0.7 is
    /// suspicious.
    pub headshot_frac: f32,
    /// Number of times the server clamped this player's look-delta to the human
    /// ceiling (aim-snap events). High counts = flick-aimbot prior.
    pub aim_snap_events: u32,
    /// Number of times movement integration had to be corrected because the
    /// client asserted an impossible position/velocity (speed/teleport prior).
    pub move_corrections: u32,
    /// Number of fire inputs rejected for beating the weapon fire-rate gate.
    pub firerate_violations: u32,
    /// Median time (ms) between an enemy becoming visible and the player's first
    /// shot landing on them. Sub-human reaction (e.g. <60 ms sustained) is a
    /// triggerbot prior.
    pub median_reaction_ms: u32,
}

impl CheatTelemetry {
    pub fn accuracy(&self) -> f32 {
        if self.shots_fired == 0 {
            0.0
        } else {
            self.shots_hit as f32 / self.shots_fired as f32
        }
    }
}

/// The karma service's verdict for one player after aggregation. Distributed to
/// session coordinators so matchmaking and in-match privileges can react.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KarmaUpdate {
    pub player: NodeId,
    /// New absolute karma after applying this update.
    pub karma: i32,
    /// Signed change applied (negative = penalty).
    pub delta: i32,
    pub action: KarmaAction,
    /// Human-readable justification (shown in moderation logs).
    pub reason: String,
    pub at_unix: u64,
}

/// The enforcement bucket a karma score lands a player in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum KarmaAction {
    /// No restriction.
    None = 0,
    /// Matched only with other low-karma players (quarantine pool). Keeps suspected
    /// cheats playing each other rather than ruining honest games.
    Quarantine = 1,
    /// Temporarily banned from ranked/main sessions.
    TempBan = 2,
    /// Permanently barred; only an expert anti-proof vote (ce-gov) can reverse it.
    PermBan = 3,
}

/// Default karma for a never-seen identity. Mid-scale so new players are neither
/// trusted nor quarantined.
pub const KARMA_DEFAULT: i32 = 100;
/// Below this, quarantine.
pub const KARMA_QUARANTINE: i32 = 40;
/// Below this, temp ban.
pub const KARMA_TEMPBAN: i32 = 10;
/// At/below this, perm ban.
pub const KARMA_PERMBAN: i32 = -50;

/// Map a karma score to its enforcement action.
pub fn action_for(karma: i32) -> KarmaAction {
    if karma <= KARMA_PERMBAN {
        KarmaAction::PermBan
    } else if karma <= KARMA_TEMPBAN {
        KarmaAction::TempBan
    } else if karma <= KARMA_QUARANTINE {
        KarmaAction::Quarantine
    } else {
        KarmaAction::None
    }
}

/// A kill/assist/objective record the authority emits for honest-play karma
/// gains, so good actors slowly recover karma and the system is not purely
/// punitive. Also feeds anti-cheat (a confirmed clean MVP is a positive prior).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchOutcome {
    pub player: NodeId,
    pub entity: EntityId,
    pub kills: u32,
    pub deaths: u32,
    pub assists: u32,
    /// True if the player completed the match without disconnecting (anti-rage-quit).
    pub completed: bool,
}
