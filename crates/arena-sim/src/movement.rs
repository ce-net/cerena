//! Player locomotion, Quake/Source style.
//!
//! The feel of an arena shooter lives here: ground friction, capped ground
//! acceleration, and *un*capped air acceleration that only ever adds a sliver of
//! speed per tick (the basis of bunny-hopping / air-strafing). The model is the
//! classic `PM_Friction` + `PM_Accelerate` pair.
//!
//! Everything is derived purely from button intent + look angles + the previous
//! ground state. No clocks, no RNG: feed the same input and you get the same
//! motion, which is what lets the client predict and the server stay authoritative.

use glam::Vec3;

use arena_protocol::entity::{EntityFlags, EntityState};
use arena_protocol::input::{Buttons, InputFrame};
use arena_protocol::world::Aabb;

use crate::collision::{self, MoveResult};

// --- Locomotion tunables (metres, seconds, m/s) -----------------------------

/// Downward acceleration. Snappier than real gravity for a tighter jump arc.
pub const GRAVITY: f32 = -20.0;
/// Top speed under normal ground movement.
pub const MAX_GROUND_SPEED: f32 = 7.0;
/// Sprint multiplier (forward only).
pub const SPRINT_MULT: f32 = 1.4;
/// Crouch-walk speed.
pub const CROUCH_SPEED: f32 = 3.0;
/// Ground acceleration coefficient (how fast we reach `wishspeed`).
pub const GROUND_ACCEL: f32 = 80.0;
/// Air acceleration coefficient. Combined with the air speed cap below this is
/// what enables air-strafing.
pub const AIR_ACCEL: f32 = 12.0;
/// Ground friction coefficient.
pub const FRICTION: f32 = 6.0;
/// Friction never scales the effective speed below this, so a slow walk still
/// stops crisply rather than creeping forever.
pub const STOP_SPEED: f32 = 1.0;
/// Upward velocity imparted by a jump.
pub const JUMP_IMPULSE: f32 = 6.5;
/// The slice of `wishspeed` air-accelerate may add toward each tick. Small, so you
/// can redirect momentum in the air (strafe) without freely accelerating.
pub const AIR_CONTROL_CAP: f32 = 1.2;

/// Player capsule radius.
pub const PLAYER_RADIUS: f32 = 0.4;
/// Half the standing capsule height (=> ~1.8 m tall).
pub const STAND_HALF_HEIGHT: f32 = 0.9;
/// Half the crouched capsule height (=> ~1.2 m tall).
pub const CROUCH_HALF_HEIGHT: f32 = 0.6;

/// Eye height below the top of the capsule, used as the muzzle/look origin.
pub const EYE_DROP: f32 = 0.15;

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

/// Advance one player one tick from its input.
///
/// `on_ground_prev` is the ground state at the *start* of the tick — the friction
/// vs air-accel decision is made against the surface we were standing on, the
/// standard PM behaviour. Returns the new ground state (also written into flags).
pub fn move_player(
    state: &mut EntityState,
    frame: &InputFrame,
    dt: f32,
    on_ground_prev: bool,
    brushes: &[Aabb],
) -> bool {
    // Look angles are pure intent; adopt them verbatim (already sanitised upstream).
    state.yaw = frame.yaw;
    state.pitch = frame.pitch;

    // --- Crouch: resize the capsule, keeping the feet planted -----------------
    let crouching = frame.buttons.has(Buttons::CROUCH);
    let was_crouching = state.flags.has(EntityFlags::CROUCHING);
    if crouching != was_crouching {
        let old_hh = if was_crouching {
            CROUCH_HALF_HEIGHT
        } else {
            STAND_HALF_HEIGHT
        };
        let new_hh = if crouching {
            CROUCH_HALF_HEIGHT
        } else {
            STAND_HALF_HEIGHT
        };
        // The centre moves by the change in half-height so the feet stay put.
        // (We don't block standing up under a low ceiling here; the arena has
        // none, and collision would re-resolve any overlap next tick anyway.)
        state.pos.y += new_hh - old_hh;
    }
    let half_height = if crouching {
        CROUCH_HALF_HEIGHT
    } else {
        STAND_HALF_HEIGHT
    };

    // --- Wish direction from buttons, in the horizontal plane -----------------
    let (sy, cy) = frame.yaw.sin_cos();
    let forward = Vec3::new(sy, 0.0, -cy); // yaw 0 => -Z
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
    let wishdir = wish.normalize_or_zero();

    // Sprint only meaningfully helps a forward push, and never while crouched.
    let sprinting = frame.buttons.has(Buttons::SPRINT)
        && frame.buttons.has(Buttons::FORWARD)
        && !crouching;
    let mut wishspeed = if crouching {
        CROUCH_SPEED
    } else {
        MAX_GROUND_SPEED
    };
    if sprinting {
        wishspeed *= SPRINT_MULT;
    }

    let mut vel = state.vel;
    let mut on_ground = on_ground_prev;

    if on_ground {
        apply_friction(&mut vel, dt);
        accelerate(&mut vel, wishdir, wishspeed, GROUND_ACCEL, dt);
        // Jump leaves the ground; we re-derive grounding after the sweep.
        if frame.buttons.has(Buttons::JUMP) {
            vel.y = JUMP_IMPULSE;
            on_ground = false;
        }
    } else {
        // Air-strafe: full directional control but only a capped speed add.
        accelerate(&mut vel, wishdir, wishspeed.min(AIR_CONTROL_CAP), AIR_ACCEL, dt);
    }

    // Gravity every tick; when grounded, collision will zero the tiny downward
    // velocity each step so the player rests flush on the floor.
    vel.y += GRAVITY * dt;

    // --- Sweep through the world ----------------------------------------------
    let MoveResult {
        pos: new_pos,
        vel: new_vel,
        on_ground: grounded,
        ..
    } = collision::resolve_move(state.pos, vel, dt, half_height, PLAYER_RADIUS, brushes);

    state.pos = new_pos;
    state.vel = new_vel;

    // --- Pose flags -----------------------------------------------------------
    state.flags.set(EntityFlags::ON_GROUND, grounded);
    state.flags.set(EntityFlags::AIRBORNE, !grounded);
    state.flags.set(EntityFlags::CROUCHING, crouching);
    state.flags.set(EntityFlags::SPRINTING, sprinting && grounded);

    grounded
}

/// Classic ground friction applied to the horizontal velocity only (vertical is
/// owned by gravity / jumps).
pub fn apply_friction(vel: &mut Vec3, dt: f32) {
    let speed = (vel.x * vel.x + vel.z * vel.z).sqrt();
    if speed < 1e-4 {
        vel.x = 0.0;
        vel.z = 0.0;
        return;
    }
    // Friction acts on at least STOP_SPEED so a slow drift still halts promptly.
    let control = speed.max(STOP_SPEED);
    let drop = control * FRICTION * dt;
    let scale = (speed - drop).max(0.0) / speed;
    vel.x *= scale;
    vel.z *= scale;
}

/// Classic acceleration toward `wishdir` up to `wishspeed`, horizontal only. The
/// add is proportional to how far below `wishspeed` the current speed *along
/// wishdir* is — never overshooting it on the ground, and (with a capped
/// `wishspeed`) enabling air-strafe in the air.
pub fn accelerate(vel: &mut Vec3, wishdir: Vec3, wishspeed: f32, accel: f32, dt: f32) {
    // Current speed projected onto the wish direction (horizontal components).
    let current = vel.x * wishdir.x + vel.z * wishdir.z;
    let add = wishspeed - current;
    if add <= 0.0 {
        return;
    }
    let accelspeed = (accel * dt * wishspeed).min(add);
    vel.x += wishdir.x * accelspeed;
    vel.z += wishdir.z * accelspeed;
}
