//! Game modes — the *rules of the match*, as hot-reloadable data.
//!
//! A mage RPG world can be played many ways: a persistent open-world PvPvE sandbox,
//! a fast arena deathmatch, a team conquest. Rather than fork the simulation per
//! mode, every mode is a [`GameModeDef`]: who is on whose team, how you win, how
//! points are scored, what you spawn with, whether loot drops. The sim reads the
//! active mode and applies it — so the designer can switch the entire match's rules
//! (or just retune a scoring value) live, mid-session, by publishing a new pack.
//!
//! This is "hot-reloadable gameplay": the game *loop* is fixed code; the game *rules*
//! are data.

use serde::{Deserialize, Serialize};

use crate::ids::{AbilityId, GameModeId, ItemId};

/// How a match ends. The sim's match controller polls the active condition each tick
/// and ends the round (declaring a winner by score) when it trips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WinCondition {
    /// First team/player to reach `points` wins.
    ScoreLimit { points: u32 },
    /// Highest score when `seconds` elapse wins.
    TimeLimit { seconds: f32 },
    /// Last team with a living member standing wins (battle-royale flavour).
    LastTeamStanding,
    /// Win by capturing `count` objectives (king-of-the-hill / control points).
    ObjectiveCapture { count: u32 },
    /// Never ends on its own — the persistent open world.
    Endless,
}

/// One scoring rule. A mode carries a list of them; the sim awards points when the
/// matching event fires. Points are `i32` so a rule can also *penalize*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScoringRule {
    /// Points for a kill.
    KillPoints { points: i32 },
    /// Points for an assist (damage/CC contribution to a kill).
    AssistPoints { points: i32 },
    /// Points for capturing/holding an objective tick.
    ObjectivePoints { points: i32 },
    /// One-time bonus for the first kill of the match.
    FirstBloodBonus { points: i32 },
}

/// Team layout for the mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TeamConfig {
    /// Everyone for themselves.
    FreeForAll,
    /// `count` teams; `friendly_fire` overrides the global tuning value for this mode.
    Teams { count: u8, friendly_fire: bool },
}

/// A complete game mode definition: the rules the match controller enforces.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameModeDef {
    pub id: GameModeId,
    pub name: String,
    /// Designer-facing description / lobby blurb.
    pub description: String,
    /// Team layout (FFA vs N teams).
    pub teams: TeamConfig,
    /// How the match is won.
    pub win: WinCondition,
    /// Scoring rules evaluated as events fire.
    pub scoring: Vec<ScoringRule>,
    /// Respawn delay for this mode (overrides [`crate::tuning::TuningConfig::respawn_seconds`]
    /// so e.g. a battle-royale mode can disable respawns with a huge value).
    pub respawn_seconds: f32,
    /// Whether kills drop loot in this mode (off for tournament/arena play).
    pub allow_loot_drops: bool,
    /// Multiplier on rolled loot quantities/rates (festival weekends, double-drop).
    pub loot_multiplier: f32,
    /// Abilities every player starts the match already equipped with.
    pub starting_loadout: Vec<AbilityId>,
    /// Items every player starts with, as `(item, count)` pairs.
    pub starting_items: Vec<(ItemId, u16)>,
    /// Whether players can damage each other at all (PvE-only modes set this false).
    pub pvp_enabled: bool,
    /// Whether the world spawns hostile mob waves (PvPvE / horde flavour).
    pub mob_waves: bool,
}
