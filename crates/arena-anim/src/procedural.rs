//! Procedural animation generators — motion synthesised from [`JointRole`] alone, so a
//! creature with *any* generated body (two legs or five) animates with no authored
//! clips. Each generator returns a fresh [`Pose`] over the skeleton's rest pose,
//! editing only the joints it owns; the [`crate::state::Animator`] blends them.
//!
//! Conventions: a limb's rest forward is taken to be its bind direction from its
//! parent. Legs "step" by swinging fore/aft about the model's right axis (`+X`); arms
//! counter-swing; the spine breathes with a small scale + lean; tails are left to the
//! spring-bone layer.

use glam::{Quat, Vec3};

use crate::pose::Pose;
use crate::skeleton::{JointRole, LimbSide, Skeleton};

/// Tunables for the synthesised gaits. All have sensible defaults; a rigger can scale
/// them per creature (a heavy golem strides slow and wide, a wisp bobs).
#[derive(Debug, Clone, Copy)]
pub struct GaitParams {
    /// Peak leg swing angle (radians) at full speed.
    pub stride: f32,
    /// Peak arm counter-swing (radians).
    pub arm_swing: f32,
    /// Vertical body bob amplitude (metres) per step.
    pub bob: f32,
    /// Breathing depth (fractional spine scale, e.g. 0.03 = +/-3%).
    pub breath: f32,
}

impl Default for GaitParams {
    fn default() -> Self {
        Self { stride: 0.7, arm_swing: 0.5, bob: 0.06, breath: 0.03 }
    }
}

/// Phase offset (radians) for a limb so left/right and successive limbs alternate
/// rather than moving in lockstep. Center limbs share the root phase.
fn limb_phase_offset(side: LimbSide, index: u8) -> f32 {
    let side_off = match side {
        LimbSide::Left => 0.0,
        LimbSide::Right => std::f32::consts::PI,
        LimbSide::Center => std::f32::consts::FRAC_PI_2,
    };
    // Stagger extra limbs (e.g. a quadruped's second pair) by a quarter cycle each.
    side_off + index as f32 * std::f32::consts::FRAC_PI_2
}

/// An idle pose: gentle breathing on the spine, a slow head bob. `phase` should be a
/// slowly-advancing value (e.g. time in seconds). Used when the creature is still.
pub fn idle(skeleton: &Skeleton, phase: f32, params: &GaitParams) -> Pose {
    let mut pose = Pose::rest(skeleton);
    let breath = (phase * 1.2).sin() * params.breath;
    for i in skeleton.joints_where(|r| matches!(r, JointRole::Spine)) {
        // Subtle chest swell + lean.
        pose.locals[i].scale *= Vec3::new(1.0 + breath * 0.5, 1.0 + breath, 1.0 + breath * 0.5);
        pose.locals[i].rotation =
            (pose.locals[i].rotation * Quat::from_rotation_x((phase * 1.2).sin() * 0.02)).normalize();
    }
    if let Some(h) = skeleton.find(|r| matches!(r, JointRole::Head)) {
        pose.locals[h].translation += Vec3::Y * (phase * 1.2).cos() * 0.01;
    }
    pose
}

/// A walk/run pose: swing legs fore/aft and arms in counter-phase by `cycle`
/// (radians; advance it by speed). `intensity` (0..1) scales the swing so the same
/// generator covers a slow creep and a full sprint.
pub fn walk(skeleton: &Skeleton, cycle: f32, intensity: f32, params: &GaitParams) -> Pose {
    let mut pose = Pose::rest(skeleton);
    let k = intensity.clamp(0.0, 1.0);

    for i in skeleton.joints_where(|r| r.is_leg()) {
        if let JointRole::Leg { side, index } = skeleton.joints[i].role {
            let swing = (cycle + limb_phase_offset(side, index)).sin() * params.stride * k;
            pose.locals[i].rotation =
                (pose.locals[i].rotation * Quat::from_rotation_x(swing)).normalize();
        }
    }
    for i in skeleton.joints_where(|r| r.is_arm()) {
        if let JointRole::Arm { side, index } = skeleton.joints[i].role {
            // Arms swing opposite the same-side leg (+PI), for a natural contralateral gait.
            let swing = (cycle + limb_phase_offset(side, index) + std::f32::consts::PI).sin()
                * params.arm_swing
                * k;
            pose.locals[i].rotation =
                (pose.locals[i].rotation * Quat::from_rotation_x(swing)).normalize();
        }
    }
    // Whole-body vertical bob at twice the step frequency.
    let bob = (cycle * 2.0).sin().abs() * params.bob * k;
    if let Some(root) = skeleton.joints.iter().position(|j| matches!(j.role, JointRole::Root)) {
        pose.locals[root].translation += Vec3::Y * bob;
    }
    pose
}

/// An airborne pose: tuck legs up and spread arms slightly (a falling/leaping shape).
pub fn airborne(skeleton: &Skeleton, rising: bool) -> Pose {
    let mut pose = Pose::rest(skeleton);
    let tuck = if rising { 0.5 } else { 0.3 };
    for i in skeleton.joints_where(|r| r.is_leg()) {
        pose.locals[i].rotation =
            (pose.locals[i].rotation * Quat::from_rotation_x(tuck)).normalize();
    }
    for i in skeleton.joints_where(|r| r.is_arm()) {
        pose.locals[i].rotation =
            (pose.locals[i].rotation * Quat::from_rotation_z(0.35)).normalize();
    }
    pose
}

/// A casting flourish: raise the arms and tilt the spine back as a spell winds up.
/// `t01` is the cast progress 0..1 (peaks mid-cast, eases at the ends).
pub fn cast(skeleton: &Skeleton, t01: f32) -> Pose {
    let mut pose = Pose::rest(skeleton);
    // A smooth bump that rises then falls across the cast.
    let amt = (t01.clamp(0.0, 1.0) * std::f32::consts::PI).sin();
    for i in skeleton.joints_where(|r| r.is_arm()) {
        pose.locals[i].rotation =
            (pose.locals[i].rotation * Quat::from_rotation_x(-1.1 * amt)).normalize();
    }
    if let Some(s) = skeleton.find(|r| matches!(r, JointRole::Spine)) {
        pose.locals[s].rotation =
            (pose.locals[s].rotation * Quat::from_rotation_x(-0.2 * amt)).normalize();
    }
    pose
}

/// A hit recoil: a sharp jerk away from `dir_local` (the local-space incoming hit
/// direction) that decays over the one-shot. `t01` is recoil progress 0..1.
pub fn hit_recoil(skeleton: &Skeleton, dir_local: Vec3, t01: f32) -> Pose {
    let mut pose = Pose::rest(skeleton);
    // Fast attack, exponential-ish decay (sharp at t=0, gone by t=1).
    let amt = (1.0 - t01.clamp(0.0, 1.0)).powi(2);
    let d = dir_local.normalize_or_zero();
    if let Some(s) = skeleton.find(|r| matches!(r, JointRole::Spine)) {
        // Lean along the hit direction in the XZ plane.
        pose.locals[s].rotation = (pose.locals[s].rotation
            * Quat::from_rotation_x(d.z * 0.5 * amt)
            * Quat::from_rotation_z(-d.x * 0.5 * amt))
        .normalize();
    }
    if let Some(h) = skeleton.find(|r| matches!(r, JointRole::Head)) {
        pose.locals[h].rotation =
            (pose.locals[h].rotation * Quat::from_rotation_x(d.z * 0.4 * amt)).normalize();
    }
    pose
}

/// A death slump: collapse forward and sink as `t01` goes 0..1, holding at the end.
pub fn death(skeleton: &Skeleton, t01: f32) -> Pose {
    let mut pose = Pose::rest(skeleton);
    let t = t01.clamp(0.0, 1.0);
    // Ease-out so it lands softly rather than snapping flat.
    let e = 1.0 - (1.0 - t) * (1.0 - t);
    if let Some(root) = skeleton.joints.iter().position(|j| matches!(j.role, JointRole::Root)) {
        pose.locals[root].translation -= Vec3::Y * 0.6 * e;
        pose.locals[root].rotation =
            (pose.locals[root].rotation * Quat::from_rotation_x(1.4 * e)).normalize();
    }
    for i in skeleton.joints_where(|r| r.is_leg() || r.is_arm()) {
        pose.locals[i].rotation =
            (pose.locals[i].rotation * Quat::from_rotation_x(0.6 * e)).normalize();
    }
    pose
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::Joint;
    use crate::Transform;

    fn biped() -> Skeleton {
        Skeleton {
            joints: vec![
                Joint { name: "root".into(), parent: -1, bind_local: Transform::IDENTITY, role: JointRole::Root },
                Joint { name: "spine".into(), parent: 0, bind_local: Transform::from_translation(Vec3::Y), role: JointRole::Spine },
                Joint { name: "head".into(), parent: 1, bind_local: Transform::from_translation(Vec3::Y), role: JointRole::Head },
                Joint { name: "leg.l".into(), parent: 0, bind_local: Transform::from_translation(Vec3::new(-0.2, -0.5, 0.0)), role: JointRole::Leg { side: LimbSide::Left, index: 0 } },
                Joint { name: "leg.r".into(), parent: 0, bind_local: Transform::from_translation(Vec3::new(0.2, -0.5, 0.0)), role: JointRole::Leg { side: LimbSide::Right, index: 0 } },
            ],
        }
    }

    #[test]
    fn legs_swing_in_opposite_phase() {
        let s = biped();
        let p = walk(&s, 0.0, 1.0, &GaitParams::default());
        // At cycle 0: left = sin(0) = 0, right = sin(PI) ~ 0; advance to PI/2 to separate.
        let p2 = walk(&s, std::f32::consts::FRAC_PI_2, 1.0, &GaitParams::default());
        let lrot = p2.locals[3].rotation;
        let rrot = p2.locals[4].rotation;
        assert!(lrot.angle_between(rrot) > 0.1, "left/right legs should be out of phase");
        // Rest reference shouldn't have moved the head.
        assert_eq!(p.locals[2].translation, s.joints[2].bind_local.translation);
    }

    #[test]
    fn generators_produce_full_length_poses() {
        let s = biped();
        for p in [
            idle(&s, 1.0, &GaitParams::default()),
            airborne(&s, true),
            cast(&s, 0.5),
            hit_recoil(&s, Vec3::Z, 0.0),
            death(&s, 1.0),
        ] {
            assert_eq!(p.locals.len(), s.len());
        }
    }
}
