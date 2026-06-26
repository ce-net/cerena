//! The [`Animator`]: a deterministic, self-contained animation driver. Feed it an
//! entity's motion each tick and it produces the [`Pose`] (and, on request, the
//! skinning matrices) to draw that entity — no authored clips required.
//!
//! It is the single entry point the rest of the engine uses. The client builds one
//! per visible creature from its baked rig and calls [`Animator::update`] +
//! [`Animator::skinning_matrices`] every frame; because it is deterministic in its
//! inputs, two clients (or a client and a replay) animating the same creature with the
//! same motion stream agree frame-for-frame.
//!
//! ## What it blends
//!
//! A base locomotion layer (idle ↔ walk/run, by speed) crossfades into an airborne
//! pose when off the ground, and one-shot overlays (cast, hit, death) are layered on
//! top with their own timers. Tails/fronds get spring-bone secondary motion. All of
//! it is procedural ([`crate::procedural`]) and role-driven, so any generated body
//! shape just works.

use glam::{Mat4, Vec3};

use crate::pose::Pose;
use crate::procedural::{self, GaitParams};
use crate::skeleton::Skeleton;

/// The per-tick motion summary the host feeds the animator. Everything the animator
/// needs to choose and shape its motion, derived from the entity's sim state.
#[derive(Debug, Clone, Copy)]
pub struct LocomotionInput {
    /// Horizontal speed (m/s); selects idle↔walk↔run and sets gait intensity.
    pub planar_speed: f32,
    /// Vertical speed (m/s); sign chooses the rising vs falling airborne pose.
    pub vertical_speed: f32,
    /// On the ground (locomotion) vs airborne (jump/fall pose).
    pub grounded: bool,
    /// Dead: drive the death overlay to completion and hold.
    pub dead: bool,
}

impl Default for LocomotionInput {
    fn default() -> Self {
        Self { planar_speed: 0.0, vertical_speed: 0.0, grounded: true, dead: false }
    }
}

/// A discrete, one-shot motion the host triggers (edge-triggered, not held).
#[derive(Debug, Clone, Copy)]
pub enum MotionEvent {
    /// Begin a cast flourish lasting `duration` seconds.
    Cast { duration: f32 },
    /// A hit from local-space direction `dir`, recoiling over a short fixed window.
    Hit { dir: Vec3 },
}

/// Tuning for how motion maps to animation (speed thresholds, cadence). Defaults suit
/// a roughly human-scaled biped; a rigger can scale per creature.
#[derive(Debug, Clone, Copy)]
pub struct AnimatorConfig {
    /// Speed (m/s) at which the gait reaches full intensity (a run).
    pub run_speed: f32,
    /// Step cadence: radians of gait cycle per metre travelled. Higher = quicker steps.
    pub cadence: f32,
    /// Idle breathing/sway rate (radians/sec of idle phase).
    pub idle_rate: f32,
    /// Crossfade half-life (seconds) between locomotion states.
    pub blend_half_life: f32,
    /// Hit-recoil duration (seconds).
    pub hit_duration: f32,
    /// Death-collapse duration (seconds).
    pub death_duration: f32,
    pub gait: GaitParams,
}

impl Default for AnimatorConfig {
    fn default() -> Self {
        Self {
            run_speed: 7.0,
            cadence: 2.4,
            idle_rate: 1.0,
            blend_half_life: 0.12,
            hit_duration: 0.35,
            death_duration: 1.2,
            gait: GaitParams::default(),
        }
    }
}

/// The animation driver for one creature.
#[derive(Debug, Clone)]
pub struct Animator {
    skeleton: Skeleton,
    inverse_bind: Vec<Mat4>,
    config: AnimatorConfig,

    // --- live state (all deterministic in fed dt/inputs) ---
    /// Accumulated gait cycle (radians), advanced by distance travelled.
    gait_phase: f32,
    /// Accumulated idle phase (radians), advanced by time.
    idle_phase: f32,
    /// Smoothed locomotion intensity 0..1 (idle..run), eased so it never pops.
    intensity: f32,
    /// Remaining cast time (s); >0 while a cast flourish plays.
    cast_remaining: f32,
    cast_total: f32,
    /// Remaining hit recoil (s) and its local direction.
    hit_remaining: f32,
    hit_dir: Vec3,
    /// Death progress 0..1.
    death_t: f32,

    /// The pose produced by the last [`Animator::update`].
    current: Pose,
}

impl Animator {
    /// Build an animator from a baked rig. `inverse_bind` is the skeleton's cached
    /// inverse-bind matrices (so we don't recompute them every frame); pass
    /// `skeleton.inverse_bind_matrices()` if you don't already have them.
    pub fn new(skeleton: Skeleton, inverse_bind: Vec<Mat4>, config: AnimatorConfig) -> Self {
        let rest = Pose::rest(&skeleton);
        Self {
            skeleton,
            inverse_bind,
            config,
            gait_phase: 0.0,
            idle_phase: 0.0,
            intensity: 0.0,
            cast_remaining: 0.0,
            cast_total: 0.0,
            hit_remaining: 0.0,
            hit_dir: Vec3::Z,
            death_t: 0.0,
            current: rest,
        }
    }

    /// Convenience constructor that computes the inverse-bind cache for you.
    pub fn from_skeleton(skeleton: Skeleton, config: AnimatorConfig) -> Self {
        let ib = skeleton.inverse_bind_matrices();
        Self::new(skeleton, ib, config)
    }

    /// The skeleton this animator drives.
    pub fn skeleton(&self) -> &Skeleton {
        &self.skeleton
    }

    /// Trigger a one-shot motion (cast / hit). Idempotent per call; the host fires it
    /// on the sim event edge.
    pub fn trigger(&mut self, event: MotionEvent) {
        match event {
            MotionEvent::Cast { duration } => {
                self.cast_total = duration.max(0.05);
                self.cast_remaining = self.cast_total;
            }
            MotionEvent::Hit { dir } => {
                self.hit_remaining = self.config.hit_duration;
                self.hit_dir = if dir.length_squared() > 1e-6 { dir } else { Vec3::Z };
            }
        }
    }

    /// Advance the animation by `dt` seconds under `input`, producing a new pose.
    /// Deterministic: same `dt` + inputs ⇒ same pose, on every machine.
    pub fn update(&mut self, input: LocomotionInput, dt: f32) {
        let cfg = self.config;

        // --- advance phases & timers ---
        self.idle_phase += dt * cfg.idle_rate;
        // Gait advances with distance covered, so feet don't slide at any speed.
        self.gait_phase += input.planar_speed * cfg.cadence * dt;
        let target_intensity = (input.planar_speed / cfg.run_speed.max(1e-3)).clamp(0.0, 1.0);
        // Ease intensity with the same half-life used for crossfades.
        let a = 1.0 - (-dt * std::f32::consts::LN_2 / cfg.blend_half_life.max(1e-3)).exp();
        self.intensity += (target_intensity - self.intensity) * a;

        if self.cast_remaining > 0.0 {
            self.cast_remaining = (self.cast_remaining - dt).max(0.0);
        }
        if self.hit_remaining > 0.0 {
            self.hit_remaining = (self.hit_remaining - dt).max(0.0);
        }
        if input.dead {
            self.death_t = (self.death_t + dt / cfg.death_duration.max(1e-3)).min(1.0);
        } else if self.death_t > 0.0 {
            // Revive (respawn): ease back out of the slump.
            self.death_t = (self.death_t - dt / cfg.death_duration.max(1e-3)).max(0.0);
        }

        // --- base locomotion layer: idle <-> walk/run by intensity ---
        let idle = procedural::idle(&self.skeleton, self.idle_phase, &cfg.gait);
        let mut base = if self.intensity > 1e-3 {
            let walk = procedural::walk(&self.skeleton, self.gait_phase, self.intensity, &cfg.gait);
            idle.blend(&walk, self.intensity)
        } else {
            idle
        };

        // --- airborne overlay ---
        if !input.grounded {
            let air = procedural::airborne(&self.skeleton, input.vertical_speed > 0.0);
            base = base.blend(&air, 0.85);
        }

        // --- one-shot overlays (cast, hit), then death takes over fully ---
        if self.cast_remaining > 0.0 && self.cast_total > 0.0 {
            let t01 = 1.0 - self.cast_remaining / self.cast_total;
            let cast = procedural::cast(&self.skeleton, t01);
            base = base.blend(&cast, 0.7);
        }
        if self.hit_remaining > 0.0 {
            let t01 = 1.0 - self.hit_remaining / self.config.hit_duration.max(1e-3);
            let hit = procedural::hit_recoil(&self.skeleton, self.hit_dir, t01);
            base = base.blend(&hit, 0.6);
        }
        if self.death_t > 0.0 {
            let dead = procedural::death(&self.skeleton, self.death_t);
            base = base.blend(&dead, self.death_t);
        }

        self.current = base;
    }

    /// The pose produced by the last [`Animator::update`].
    pub fn pose(&self) -> &Pose {
        &self.current
    }

    /// Skinning matrices (`global * inverse_bind` per joint) for the current pose —
    /// upload straight into the renderer's bone buffer. This is the one call the
    /// renderer needs per creature per frame.
    pub fn skinning_matrices(&self) -> Vec<Mat4> {
        self.current.skinning_matrices(&self.skeleton, &self.inverse_bind)
    }

    /// Model-space global joint transforms (for attaching effects/weapons to bones,
    /// or pose-accurate hitboxes in the sim).
    pub fn global_joint_transforms(&self) -> Vec<crate::Transform> {
        self.current.global_transforms(&self.skeleton)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::{Joint, JointRole, LimbSide};
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
    fn idle_then_run_changes_intensity_and_pose() {
        let mut anim = Animator::from_skeleton(biped(), AnimatorConfig::default());
        // Idle for a bit.
        for _ in 0..16 {
            anim.update(LocomotionInput::default(), 1.0 / 64.0);
        }
        let idle_legs = anim.pose().locals[3].rotation;
        // Now run for a second.
        let run = LocomotionInput { planar_speed: 7.0, grounded: true, ..Default::default() };
        for _ in 0..64 {
            anim.update(run, 1.0 / 64.0);
        }
        assert!(anim.intensity > 0.8, "running should drive intensity high");
        // Legs should be doing something different than idle.
        let run_legs = anim.pose().locals[3].rotation;
        assert!(idle_legs.angle_between(run_legs) > 0.05);
    }

    #[test]
    fn skinning_matrices_match_joint_count() {
        let mut anim = Animator::from_skeleton(biped(), AnimatorConfig::default());
        anim.update(LocomotionInput::default(), 1.0 / 64.0);
        assert_eq!(anim.skinning_matrices().len(), 5);
    }

    #[test]
    fn cast_and_hit_are_one_shots() {
        let mut anim = Animator::from_skeleton(biped(), AnimatorConfig::default());
        anim.trigger(MotionEvent::Cast { duration: 0.2 });
        anim.trigger(MotionEvent::Hit { dir: Vec3::Z });
        // Run them out; they must decay to zero (no lingering overlay).
        for _ in 0..64 {
            anim.update(LocomotionInput::default(), 1.0 / 64.0);
        }
        assert_eq!(anim.cast_remaining, 0.0);
        assert_eq!(anim.hit_remaining, 0.0);
    }

    #[test]
    fn determinism_same_inputs_same_pose() {
        let mut a = Animator::from_skeleton(biped(), AnimatorConfig::default());
        let mut b = Animator::from_skeleton(biped(), AnimatorConfig::default());
        let input = LocomotionInput { planar_speed: 3.0, grounded: true, ..Default::default() };
        for _ in 0..40 {
            a.update(input, 1.0 / 64.0);
            b.update(input, 1.0 / 64.0);
        }
        assert_eq!(a.pose().locals[3].rotation, b.pose().locals[3].rotation);
    }
}
