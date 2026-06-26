//! Player locomotion (Quake/Source base) plus the parkour movement-mode kit.
//!
//! Base locomotion is the classic `PM_Friction` + `PM_Accelerate` pair: capped
//! ground accel with friction, and *un*capped air accel that adds only a sliver per
//! tick (air-strafing). On top of that, Cerena layers data-driven parkour modes
//! (dash, double-jump, wall-run, grapple, glide, blink, climb, ground-slam, slide,
//! sprint), each a [`arena_content::movement::MovementKind`] the world activates and
//! whose kinematics this module applies.
//!
//! Everything is derived purely from intent + state. No clocks, no RNG: identical
//! input yields identical motion, so the client predicts and the server stays
//! authoritative.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use arena_content::movement::MovementKind;
use arena_protocol::entity::{EntityFlags, EntityState};
use arena_protocol::input::{Buttons, InputFrame};
use arena_protocol::world::Aabb;

use crate::collision::{self, MoveResult};

// --- Locomotion tunables (metres, seconds, m/s) -----------------------------

/// Downward acceleration. Snappier than real gravity for a tighter jump arc.
pub const GRAVITY: f32 = -20.0;
/// Top speed under normal ground movement.
pub const MAX_GROUND_SPEED: f32 = 7.0;
/// Default sprint multiplier when no Sprint movement-mode is equipped.
pub const SPRINT_MULT: f32 = 1.4;
/// Crouch-walk speed.
pub const CROUCH_SPEED: f32 = 3.0;
/// Ground acceleration coefficient (how fast we reach `wishspeed`).
pub const GROUND_ACCEL: f32 = 80.0;
/// Air acceleration coefficient (with the cap below, enables air-strafing).
pub const AIR_ACCEL: f32 = 12.0;
/// Ground friction coefficient.
pub const FRICTION: f32 = 6.0;
/// Friction floor so a slow drift still halts crisply.
pub const STOP_SPEED: f32 = 1.0;
/// Upward velocity imparted by a jump.
pub const JUMP_IMPULSE: f32 = 6.5;
/// The slice of `wishspeed` air-accelerate may add per tick.
pub const AIR_CONTROL_CAP: f32 = 1.2;

/// Player capsule radius.
pub const PLAYER_RADIUS: f32 = 0.4;
/// Half the standing capsule height (=> ~1.8 m tall).
pub const STAND_HALF_HEIGHT: f32 = 0.9;
/// Half the crouched capsule height (=> ~1.2 m tall).
pub const CROUCH_HALF_HEIGHT: f32 = 0.6;
/// Eye height below the top of the capsule (muzzle / look origin).
pub const EYE_DROP: f32 = 0.15;

/// Per-entity transient parkour state the world keeps between ticks.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MovementRuntime {
    /// Mid-air jumps already spent this airtime (reset on landing).
    pub extra_jumps_used: u8,
    /// A ground-slam is descending and will burst on the next landing.
    pub slam_pending: bool,
}

/// Tuning knobs the world feeds into [`move_player`], folding in status effects
/// (slow/haste/levitate/root) and active movement modes (sprint/glide/wall-run).
#[derive(Debug, Clone, Copy)]
pub struct MoveParams {
    /// Multiplier applied to `wishspeed` when sprinting (a Sprint mode overrides
    /// the default [`SPRINT_MULT`]).
    pub sprint_mult: f32,
    /// Flat bonus to `wishspeed` from gear/attributes (m/s).
    pub speed_bonus: f32,
    /// Final scale on horizontal wishspeed from slow/haste statuses.
    pub speed_scale: f32,
    /// Scales gravity this tick (glide / wall-run < 1, levitate = 0).
    pub gravity_mult: f32,
    /// Immobilised (rooted / frozen): no horizontal control, no jump.
    pub rooted: bool,
}

impl Default for MoveParams {
    fn default() -> Self {
        Self {
            sprint_mult: SPRINT_MULT,
            speed_bonus: 0.0,
            speed_scale: 1.0,
            gravity_mult: 1.0,
            rooted: false,
        }
    }
}

/// What [`move_player`] reports back after sweeping the world.
#[derive(Debug, Clone, Copy)]
pub struct MoveStatus {
    pub on_ground: bool,
    /// A near-vertical wall the capsule ended the tick touching (for wall-run/climb).
    pub wall_normal: Option<Vec3>,
}

/// The half-height a player currently occupies, from its crouch flag.
pub fn half_height_of(state: &EntityState) -> f32 {
    if state.flags.has(EntityFlags::CROUCHING) {
        CROUCH_HALF_HEIGHT
    } else {
        STAND_HALF_HEIGHT
    }
}

/// World-space eye position (capsule centre raised to just under the crown).
pub fn eye_position(state: &EntityState) -> Vec3 {
    state.pos + Vec3::Y * (half_height_of(state) - EYE_DROP)
}

/// View direction from yaw/pitch, matching `EntityState::view_dir` exactly so the
/// muzzle ray the sim fires equals the ray the client predicted.
pub fn view_dir(yaw: f32, pitch: f32) -> Vec3 {
    let (sy, cy) = yaw.sin_cos();
    let (sp, cp) = pitch.sin_cos();
    Vec3::new(sy * cp, sp, -cy * cp).normalize_or_zero()
}

/// Horizontal (XZ) component of a vector, normalised.
fn horizontal(v: Vec3) -> Vec3 {
    Vec3::new(v.x, 0.0, v.z).normalize_or_zero()
}

/// Advance one player one tick from its input and the supplied [`MoveParams`].
pub fn move_player(
    state: &mut EntityState,
    frame: &InputFrame,
    dt: f32,
    on_ground_prev: bool,
    brushes: &[Aabb],
    params: &MoveParams,
) -> MoveStatus {
    state.yaw = frame.yaw;
    state.pitch = frame.pitch;

    // --- Crouch: resize the capsule, keeping the feet planted -----------------
    let crouching = frame.buttons.has(Buttons::CROUCH);
    let was_crouching = state.flags.has(EntityFlags::CROUCHING);
    if crouching != was_crouching {
        let old_hh = if was_crouching { CROUCH_HALF_HEIGHT } else { STAND_HALF_HEIGHT };
        let new_hh = if crouching { CROUCH_HALF_HEIGHT } else { STAND_HALF_HEIGHT };
        state.pos.y += new_hh - old_hh;
    }
    let half_height = if crouching { CROUCH_HALF_HEIGHT } else { STAND_HALF_HEIGHT };

    // --- Wish direction from buttons, in the horizontal plane -----------------
    let (sy, cy) = frame.yaw.sin_cos();
    let forward = Vec3::new(sy, 0.0, -cy);
    let right = Vec3::new(cy, 0.0, sy);
    let mut wish = Vec3::ZERO;
    if frame.buttons.has(Buttons::FORWARD) {
        wish += forward;
    }
    if frame.buttons.has(Buttons::BACK) {
        wish -= forward;
    }
    if frame.buttons.has(Buttons::RIGHT) {
        wish += right;
    }
    if frame.buttons.has(Buttons::LEFT) {
        wish -= right;
    }
    // Rooted / frozen: intent is ignored (gravity still applies below).
    let wishdir = if params.rooted { Vec3::ZERO } else { wish.normalize_or_zero() };

    let sprinting = frame.buttons.has(Buttons::SPRINT)
        && frame.buttons.has(Buttons::FORWARD)
        && !crouching;
    let mut wishspeed = if crouching { CROUCH_SPEED } else { MAX_GROUND_SPEED };
    wishspeed += params.speed_bonus;
    if sprinting {
        wishspeed *= params.sprint_mult;
    }
    wishspeed *= params.speed_scale;
    wishspeed = wishspeed.max(0.0);

    let mut vel = state.vel;

    if on_ground_prev {
        apply_friction(&mut vel, dt);
        accelerate(&mut vel, wishdir, wishspeed, GROUND_ACCEL, dt);
        if !params.rooted && frame.buttons.has(Buttons::JUMP) {
            vel.y = JUMP_IMPULSE;
        }
    } else {
        accelerate(&mut vel, wishdir, wishspeed.min(AIR_CONTROL_CAP), AIR_ACCEL, dt);
    }

    // Gravity (scaled for glide/wall-run/levitate).
    vel.y += GRAVITY * params.gravity_mult * dt;

    let MoveResult {
        pos: new_pos,
        vel: new_vel,
        on_ground: grounded,
        ..
    } = collision::resolve_move(state.pos, vel, dt, half_height, PLAYER_RADIUS, brushes);

    state.pos = new_pos;
    state.vel = new_vel;

    state.flags.set(EntityFlags::ON_GROUND, grounded);
    state.flags.set(EntityFlags::AIRBORNE, !grounded);
    state.flags.set(EntityFlags::CROUCHING, crouching);
    state.flags.set(EntityFlags::SPRINTING, sprinting && grounded);

    let wall_normal = if grounded {
        None
    } else {
        collision::wall_contact(new_pos, half_height, PLAYER_RADIUS, brushes)
    };

    MoveStatus { on_ground: grounded, wall_normal }
}

/// Classic ground friction on horizontal velocity (vertical is gravity/jumps).
pub fn apply_friction(vel: &mut Vec3, dt: f32) {
    let speed = (vel.x * vel.x + vel.z * vel.z).sqrt();
    if speed < 1e-4 {
        vel.x = 0.0;
        vel.z = 0.0;
        return;
    }
    let control = speed.max(STOP_SPEED);
    let drop = control * FRICTION * dt;
    let scale = (speed - drop).max(0.0) / speed;
    vel.x *= scale;
    vel.z *= scale;
}

/// Classic acceleration toward `wishdir` up to `wishspeed`, horizontal only.
pub fn accelerate(vel: &mut Vec3, wishdir: Vec3, wishspeed: f32, accel: f32, dt: f32) {
    let current = vel.x * wishdir.x + vel.z * wishdir.z;
    let add = wishspeed - current;
    if add <= 0.0 {
        return;
    }
    let accelspeed = (accel * dt * wishspeed).min(add);
    vel.x += wishdir.x * accelspeed;
    vel.z += wishdir.z * accelspeed;
}

// --- Movement-mode kinematics -----------------------------------------------
//
// These are the *kinematic effects* of activating a mode; the world decides *when*
// (button edges / selection) and charges mana+stamina+cooldown. A handful of modes
// (Sprint/Glide/WallRun) are continuous and instead shape `MoveParams`; the helpers
// below cover the discrete, impulse-style activations.

/// Result of activating a discrete movement mode.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModeActivation {
    /// True if the activation actually took effect (the world then charges costs).
    pub used: bool,
    /// A ground-slam landing burst the world should resolve as area damage:
    /// `(damage, radius)` applied at the player's feet on the landing tick.
    pub slam: Option<(f32, f32)>,
}

/// Apply a discrete movement mode's kinematics to `state`. `aim_dir` is the full
/// view direction; `brushes` is the static world for collision-clamped moves.
pub fn apply_mode(
    state: &mut EntityState,
    kind: &MovementKind,
    aim_dir: Vec3,
    on_ground: bool,
    brushes: &[Aabb],
    rt: &mut MovementRuntime,
) -> ModeActivation {
    let mut out = ModeActivation::default();
    let hh = half_height_of(state);
    match kind {
        MovementKind::Dash { distance: _, speed } => {
            // Horizontal burst along aim. Collision in the next sweep stops it at
            // walls; we set velocity rather than teleport so it feels kinetic.
            let d = horizontal(aim_dir);
            state.vel.x = d.x * speed;
            state.vel.z = d.z * speed;
            out.used = true;
        }
        MovementKind::DoubleJump { extra_jumps, impulse } => {
            if !on_ground && rt.extra_jumps_used < *extra_jumps {
                state.vel.y = *impulse;
                rt.extra_jumps_used += 1;
                out.used = true;
            }
        }
        MovementKind::Blink { distance } => {
            state.pos = collision::clamp_translation(
                state.pos,
                aim_dir.normalize_or_zero() * *distance,
                hh,
                PLAYER_RADIUS,
                brushes,
            );
            out.used = true;
        }
        MovementKind::Grapple { range, pull_speed } => {
            // Fire a grapple along aim; if it anchors on geometry, fling toward it.
            if let Some((t, _)) =
                collision::raycast_aabbs(eye_position(state), aim_dir.normalize_or_zero(), *range, brushes)
            {
                let anchor = eye_position(state) + aim_dir.normalize_or_zero() * t;
                let to = (anchor - state.pos).normalize_or_zero();
                state.vel = to * *pull_speed;
                out.used = true;
            }
        }
        MovementKind::GroundSlam { damage, radius, down_speed } => {
            if !on_ground {
                state.vel = Vec3::new(0.0, -*down_speed, 0.0);
                rt.slam_pending = true;
                out.used = true;
                // The landing burst is reported when we touch down (see world tick);
                // we stash the parameters via the activation so the caller knows them.
                out.slam = Some((*damage, *radius));
            }
        }
        MovementKind::Slide { speed, duration_s: _ } => {
            if on_ground {
                let d = horizontal(if horizontal(state.vel) == Vec3::ZERO { aim_dir } else { state.vel });
                state.vel.x = d.x * speed;
                state.vel.z = d.z * speed;
                out.used = true;
            }
        }
        // Continuous modes shape MoveParams instead; activating them here is a no-op
        // beyond acknowledging the input so the world can keep charging upkeep.
        MovementKind::WallRun { .. }
        | MovementKind::Glide { .. }
        | MovementKind::Sprint { .. }
        | MovementKind::Climb { .. } => {
            out.used = true;
        }
    }
    out
}
