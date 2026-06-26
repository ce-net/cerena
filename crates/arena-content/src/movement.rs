//! Movement / parkour modes — the kinetic half of the game feel.
//!
//! Cerena is first-person and movement-forward: dashing, wall-running, grappling,
//! gliding, blinking. Each mode is pure data here; the simulation
//! (`arena-sim::movement`) is the fixed interpreter that reads a [`MovementKind`] and
//! applies the corresponding kinematics. Adding tunable parkour is therefore a
//! hot-reload, and items / tech grant modes by [`MovementModeId`].

use serde::{Deserialize, Serialize};

use crate::ids::MovementModeId;

/// The kind of movement and its tuning parameters. `arena-sim::movement` matches on
/// this to drive the character controller; the variants are the closed set of
/// kinematic primitives the engine understands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MovementKind {
    /// Burst horizontally `distance` metres at `speed`.
    Dash { distance: f32, speed: f32 },
    /// Grant `extra_jumps` mid-air jumps, each with vertical `impulse`.
    DoubleJump { extra_jumps: u8, impulse: f32 },
    /// Run along walls for up to `max_time_s`, at `speed`, with reduced gravity.
    WallRun {
        max_time_s: f32,
        speed: f32,
        gravity_mult: f32,
    },
    /// Fire a grapple up to `range`; reel the player in at `pull_speed`.
    Grapple { range: f32, pull_speed: f32 },
    /// Glide: scale fall speed by `fall_mult` (<1 = slow fall) with `forward_boost`.
    Glide { fall_mult: f32, forward_boost: f32 },
    /// Short instantaneous teleport of `distance` along aim.
    Blink { distance: f32 },
    /// Climb sheer surfaces at `speed`.
    Climb { speed: f32 },
    /// Slam down: deal `damage` in `radius` on landing, descending at `down_speed`.
    GroundSlam {
        damage: f32,
        radius: f32,
        down_speed: f32,
    },
    /// Slide along the ground at `speed` for `duration_s` (low profile, momentum).
    Slide { speed: f32, duration_s: f32 },
    /// Sustained sprint multiplying base move speed by `speed_mult`.
    Sprint { speed_mult: f32 },
    /// Free 3D flight while the fly intent is held: move toward the full view ray at
    /// `speed` (reached with `accel`), with jump/crouch overriding vertical at
    /// `ascend_speed`. Gravity is suppressed. A continuous mode (shapes `MoveParams`),
    /// draining stamina/mana as upkeep.
    Fly {
        speed: f32,
        accel: f32,
        ascend_speed: f32,
    },
    /// Convert and amplify existing momentum: scale current horizontal velocity by
    /// `boost_mult` and add a flat `impulse` burst along the aim, but only when
    /// already moving faster than `min_speed` (so it rewards flow — chaining off a
    /// slide, wall-run, or grapple — rather than starting from a standstill).
    MomentumBoost {
        boost_mult: f32,
        min_speed: f32,
        impulse: f32,
    },
}

/// A movement mode definition: a kinematic primitive plus its resource costs. The
/// sim charges mana/stamina and enforces the cooldown when the mode activates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MovementModeDef {
    pub id: MovementModeId,
    pub name: String,
    /// The kinematic behaviour (interpreted by `arena-sim::movement`).
    pub kind: MovementKind,
    /// Mana spent to activate.
    pub mana_cost: f32,
    /// Seconds before it can be used again.
    pub cooldown: f32,
    /// Stamina spent to activate.
    pub stamina_cost: f32,
}
