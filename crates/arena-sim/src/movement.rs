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
    /// Current melee combo step (0 = opening Slash). Advances on each chained swing,
    /// resets to 0 when the combo window lapses.
    pub melee_combo: u8,
    /// Sim tick of the last melee swing, for the combo-window timer.
    pub melee_last_tick: u32,
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
    /// Flight is engaged this tick (full 3D control, gravity suppressed). The world
    /// sets this when a [`MovementKind::Fly`] mode is unlocked and the fly intent is
    /// held *and* its per-tick upkeep was paid.
    pub fly: bool,
    /// Target flight speed (m/s) when `fly` is set.
    pub fly_speed: f32,
    /// How quickly flight velocity converges on its target (per second).
    pub fly_accel: f32,
    /// Vertical climb/ascend speed (m/s). >0 means cling-and-climb a wall this tick
    /// (set by the world when a [`MovementKind::Climb`] mode is active against a
    /// wall); overrides gravity and drives the capsule straight up.
    pub climb_speed: f32,
}

impl Default for MoveParams {
    fn default() -> Self {
        Self {
            sprint_mult: SPRINT_MULT,
            speed_bonus: 0.0,
            speed_scale: 1.0,
            gravity_mult: 1.0,
            rooted: false,
            fly: false,
            fly_speed: 0.0,
            fly_accel: 0.0,
            climb_speed: 0.0,
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

    // --- Flight: full 3D control, gravity suppressed --------------------------
    // (The MELEEING flag is a one-tick pulse the world's melee path sets *before*
    // this sweep runs; the base move leaves it untouched so it clears next tick.)
    if params.fly {
        return fly_move(state, frame, dt, brushes, params);
    }

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

    // Wall-climb overrides gravity: stick to the surface and ascend at climb speed,
    // damping any outward drift so the capsule hugs the wall instead of peeling off.
    let climbing = params.climb_speed > 0.0;
    if climbing {
        vel.y = params.climb_speed;
        vel.x *= 0.6;
        vel.z *= 0.6;
    } else {
        // Gravity (scaled for glide/wall-run/levitate).
        vel.y += GRAVITY * params.gravity_mult * dt;
    }

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
    state.flags.set(EntityFlags::FLYING, false);
    state.flags.set(EntityFlags::CLIMBING, climbing);

    let wall_normal = if grounded {
        None
    } else {
        collision::wall_contact(new_pos, half_height, PLAYER_RADIUS, brushes)
    };

    MoveStatus { on_ground: grounded, wall_normal }
}

/// Flight kinematics: steer the capsule along the full view ray in three
/// dimensions, with jump/crouch overriding vertical, and smoothly converge velocity
/// on the target so flight feels weighty rather than instant. Gravity is suppressed
/// entirely; collision is still resolved so you cannot fly through geometry.
fn fly_move(
    state: &mut EntityState,
    frame: &InputFrame,
    dt: f32,
    brushes: &[Aabb],
    params: &MoveParams,
) -> MoveStatus {
    let look = view_dir(frame.yaw, frame.pitch);
    let (sy, cy) = frame.yaw.sin_cos();
    let right = Vec3::new(cy, 0.0, sy);

    let mut wish = Vec3::ZERO;
    if frame.buttons.has(Buttons::FORWARD) {
        wish += look;
    }
    if frame.buttons.has(Buttons::BACK) {
        wish -= look;
    }
    if frame.buttons.has(Buttons::RIGHT) {
        wish += right;
    }
    if frame.buttons.has(Buttons::LEFT) {
        wish -= right;
    }
    // Jump ascends, crouch descends — explicit vertical control on top of the look ray.
    if frame.buttons.has(Buttons::JUMP) {
        wish += Vec3::Y;
    }
    if frame.buttons.has(Buttons::CROUCH) {
        wish -= Vec3::Y;
    }

    let wishdir = if params.rooted { Vec3::ZERO } else { wish.normalize_or_zero() };
    let speed = (params.fly_speed * params.speed_scale).max(0.0);
    let target = wishdir * speed;
    // Critically-damped-ish convergence: lerp toward the target velocity. With no
    // input the target is zero, so the flyer eases to a hover.
    let t = (params.fly_accel * dt).clamp(0.0, 1.0);
    let mut vel = state.vel + (target - state.vel) * t;

    let half_height = half_height_of(state);
    let MoveResult { pos: new_pos, vel: new_vel, on_ground, .. } =
        collision::resolve_move(state.pos, vel, dt, half_height, PLAYER_RADIUS, brushes);
    vel = new_vel;
    state.pos = new_pos;
    state.vel = vel;

    state.flags.set(EntityFlags::ON_GROUND, on_ground);
    state.flags.set(EntityFlags::AIRBORNE, !on_ground);
    state.flags.set(EntityFlags::FLYING, true);
    state.flags.set(EntityFlags::CLIMBING, false);
    state.flags.set(EntityFlags::CROUCHING, false);

    MoveStatus { on_ground, wall_normal: None }
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
        MovementKind::MomentumBoost { boost_mult, min_speed, impulse } => {
            // Reward flow: only fires when already moving, amplifying the *existing*
            // horizontal velocity and adding a flat burst along it (or along aim from
            // a near-stop). Chains beautifully off a slide, wall-run, or grapple.
            let horiz = Vec3::new(state.vel.x, 0.0, state.vel.z);
            let speed = horiz.length();
            if speed >= *min_speed {
                let dir = if speed > 1e-3 { horiz / speed } else { horizontal(aim_dir) };
                let new_speed = speed * *boost_mult + *impulse;
                state.vel.x = dir.x * new_speed;
                state.vel.z = dir.z * new_speed;
                // A sliver of lift so a ground boost can carry over a lip.
                if on_ground {
                    state.vel.y = state.vel.y.max(2.0);
                }
                out.used = true;
            }
        }
        // Continuous modes shape MoveParams instead; activating them here is a no-op
        // beyond acknowledging the input so the world can keep charging upkeep.
        MovementKind::WallRun { .. }
        | MovementKind::Glide { .. }
        | MovementKind::Sprint { .. }
        | MovementKind::Climb { .. }
        | MovementKind::Fly { .. } => {
            out.used = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_protocol::entity::EntityKind;
    use arena_protocol::world::Team;

    fn body(pos: Vec3, vel: Vec3, on_ground: bool) -> EntityState {
        let mut flags = EntityFlags::default();
        flags.set(EntityFlags::ON_GROUND, on_ground);
        flags.set(EntityFlags::AIRBORNE, !on_ground);
        EntityState {
            id: 1,
            kind: EntityKind::Player,
            pos,
            vel,
            yaw: 0.0,
            pitch: 0.0,
            flags,
            team: Team::None,
            health: 100,
            armor: 0,
            weapon: 0,
            owner: String::new(),
        }
    }

    fn frame(buttons: u16, yaw: f32, pitch: f32) -> InputFrame {
        InputFrame {
            seq: 1,
            client_tick: 0,
            buttons: Buttons(buttons),
            yaw,
            pitch,
            weapon_slot: 0,
        }
    }

    #[test]
    fn flight_suppresses_gravity_and_steers_along_view() {
        // Flying forward at yaw 0 (forward = -Z) over empty space.
        let mut s = body(Vec3::new(0.0, 50.0, 0.0), Vec3::ZERO, false);
        let params = MoveParams { fly: true, fly_speed: 16.0, fly_accel: 22.0, ..Default::default() };
        let f = frame(Buttons::FORWARD, 0.0, 0.0);
        for _ in 0..30 {
            move_player(&mut s, &f, 1.0 / 64.0, false, &[], &params);
        }
        assert!(s.vel.z < -5.0, "flight should carry the wizard forward (-Z), got {:?}", s.vel);
        assert!(s.vel.y.abs() < 0.5, "flight must suppress gravity, vel.y={}", s.vel.y);
        assert!(s.flags.has(EntityFlags::FLYING), "the flying flag should be set");
    }

    #[test]
    fn flight_hovers_when_no_input() {
        let mut s = body(Vec3::new(0.0, 50.0, 0.0), Vec3::new(0.0, -8.0, 0.0), false);
        let params = MoveParams { fly: true, fly_speed: 16.0, fly_accel: 22.0, ..Default::default() };
        let f = frame(0, 0.0, 0.0);
        for _ in 0..60 {
            move_player(&mut s, &f, 1.0 / 64.0, false, &[], &params);
        }
        assert!(s.vel.length() < 1.0, "with no input flight eases to a hover, got {:?}", s.vel);
    }

    #[test]
    fn momentum_boost_amplifies_existing_speed() {
        // Moving at 8 m/s along +X, grounded.
        let mut s = body(Vec3::ZERO, Vec3::new(8.0, 0.0, 0.0), true);
        let mut rt = MovementRuntime::default();
        let kind = MovementKind::MomentumBoost { boost_mult: 1.5, min_speed: 6.0, impulse: 10.0 };
        let out = apply_mode(&mut s, &kind, Vec3::X, true, &[], &mut rt);
        assert!(out.used, "boost should fire above min_speed");
        // 8 * 1.5 + 10 = 22 along +X.
        assert!((s.vel.x - 22.0).abs() < 0.01, "boosted speed, got {}", s.vel.x);
    }

    #[test]
    fn momentum_boost_requires_flow() {
        // Below min_speed: the boost refuses (rewards keeping momentum, not standing).
        let mut s = body(Vec3::ZERO, Vec3::new(3.0, 0.0, 0.0), true);
        let mut rt = MovementRuntime::default();
        let kind = MovementKind::MomentumBoost { boost_mult: 1.5, min_speed: 6.0, impulse: 10.0 };
        let out = apply_mode(&mut s, &kind, Vec3::X, true, &[], &mut rt);
        assert!(!out.used, "boost should not fire from a near-standstill");
    }

    #[test]
    fn climb_drives_the_capsule_upward() {
        let mut s = body(Vec3::new(0.0, 1.0, 0.0), Vec3::ZERO, false);
        let params = MoveParams { climb_speed: 4.0, ..Default::default() };
        let f = frame(Buttons::FORWARD, 0.0, 0.0);
        let before = s.pos.y;
        move_player(&mut s, &f, 1.0 / 64.0, false, &[], &params);
        assert!(s.pos.y > before, "climbing should raise the capsule");
        assert!(s.flags.has(EntityFlags::CLIMBING), "the climbing flag should be set");
    }
}
