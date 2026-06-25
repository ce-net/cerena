//! Client command frames.
//!
//! The single most important security rule in a server-authoritative FPS: the
//! client asserts *intent*, never *outcome*. A client may say "I am holding W and
//! looking here and pressing fire on tick N". It may never say "I moved to X" or
//! "I hit player P". The server derives all outcomes from intent + its own world.
//!
//! Every field here is bounded and validated server-side (see `arena-sim` apply
//! and `arena-karma` plausibility checks).

use serde::{Deserialize, Serialize};

use crate::Tick;

/// Button bitflags packed into a single byte for cheap transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Buttons(pub u16);

impl Buttons {
    pub const FORWARD: u16 = 1 << 0;
    pub const BACK: u16 = 1 << 1;
    pub const LEFT: u16 = 1 << 2;
    pub const RIGHT: u16 = 1 << 3;
    pub const JUMP: u16 = 1 << 4;
    pub const CROUCH: u16 = 1 << 5;
    pub const SPRINT: u16 = 1 << 6;
    pub const FIRE: u16 = 1 << 7;
    pub const ALT_FIRE: u16 = 1 << 8;
    pub const RELOAD: u16 = 1 << 9;
    pub const USE: u16 = 1 << 10;
    pub const MELEE: u16 = 1 << 11;

    pub fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }

    pub fn set(&mut self, flag: u16, on: bool) {
        if on {
            self.0 |= flag;
        } else {
            self.0 &= !flag;
        }
    }
}

/// One tick of player intent. This is the *only* authoritative client input.
///
/// The look direction is sent as yaw/pitch (radians) rather than a full quat:
/// it is half the bytes and the server reconstructs the view ray identically.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct InputFrame {
    /// The client's local sim tick this frame was produced for. Used for
    /// prediction/reconciliation: the server echoes the last-applied seq so the
    /// client can replay unacked inputs on top of the authoritative state.
    pub seq: u32,
    /// The server tick the client *believes* is current (its clock estimate).
    /// The server clamps this; large skew is a desync or a time-cheat.
    pub client_tick: Tick,
    pub buttons: Buttons,
    /// Yaw in radians, wrapped to [-pi, pi]. Horizontal aim.
    pub yaw: f32,
    /// Pitch in radians, clamped to [-pi/2, pi/2]. Vertical aim.
    pub pitch: f32,
    /// Selected weapon slot (validated against the player's loadout).
    pub weapon_slot: u8,
}

impl InputFrame {
    /// Sanitize a freshly-received frame: NaN-scrub and clamp look angles. Returns
    /// a frame that is always safe to feed into the simulation. The server treats
    /// out-of-range values as an anti-cheat signal *and* repairs them so a bad
    /// packet can never crash the sim.
    pub fn sanitized(mut self) -> Self {
        use std::f32::consts::{FRAC_PI_2, PI};
        if !self.yaw.is_finite() {
            self.yaw = 0.0;
        }
        if !self.pitch.is_finite() {
            self.pitch = 0.0;
        }
        // wrap yaw to [-pi, pi]
        self.yaw = (self.yaw + PI).rem_euclid(2.0 * PI) - PI;
        self.pitch = self.pitch.clamp(-FRAC_PI_2, FRAC_PI_2);
        self
    }

    /// Did the look direction change implausibly fast relative to `prev`? Returns
    /// the angular delta in radians; `arena-karma` compares it to a human ceiling.
    pub fn look_delta(&self, prev: &InputFrame) -> f32 {
        let dy = (self.yaw - prev.yaw).abs().min(2.0 * std::f32::consts::PI - (self.yaw - prev.yaw).abs());
        let dp = (self.pitch - prev.pitch).abs();
        (dy * dy + dp * dp).sqrt()
    }
}

/// A client batches several input frames per packet so a single dropped packet
/// does not stall reconciliation — the next packet re-includes recent history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputBatch {
    /// Highest server tick the client has fully received a snapshot for. The
    /// server uses this to choose a delta baseline.
    pub ack_tick: Tick,
    /// Recent input frames, oldest first. The server applies only those newer
    /// than what it has already consumed for this player.
    pub frames: Vec<InputFrame>,
}
