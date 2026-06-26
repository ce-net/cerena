//! # arena-anim
//!
//! Cerena's **animation system** — a small, deterministic, dependency-light library
//! (only `glam` + `serde`) shared by every other crate that needs to *pose* a thing:
//! the client to render it, `arena-assets` to skin a procedural mesh to it, and the
//! sim if it ever wants pose-accurate hitboxes. It is the kinetic counterpart to
//! `arena-procgen`: procgen grows the *body*, arena-anim makes it *move*.
//!
//! ## Why it looks the way it does
//!
//! Cerena's creatures are **procedurally generated** with a *variable* layout — a
//! wisp has two stubby limbs, a wraith has five. There are no hand-authored animation
//! clips because there are no hand-authored models. So this crate is built around
//! **procedural, role-driven animation**: every joint carries a [`JointRole`]
//! (spine, head, leg, arm, tail...) and the [`state::Animator`] synthesises motion —
//! a walk gait, idle breathing, a cast flourish, a hit recoil, a death slump — from
//! those roles plus the entity's velocity and flags. Keyframed [`clip::AnimationClip`]s
//! are *also* supported (for anything that does ship authored motion), but the default
//! path needs none.
//!
//! ## Hard constraints (shared with `arena-sim` / `arena-procgen`)
//!
//! - **Deterministic.** No wall-clock, no RNG. The caller owns `dt`; identical inputs
//!   produce identical poses, so an authority and a client agree on where a limb is.
//! - **`wasm32`-clean.** No `wgpu`, no `tokio`, no I/O. Pure math over plain data.
//! - **Pure data.** A [`Skeleton`], a [`Pose`], an [`AnimationClip`] all serialize, so
//!   a rig can be baked into a content-addressed asset and shipped over the mesh.
//!
//! ## Module map
//!
//! - [`skeleton`]   — the joint hierarchy + bind pose + [`JointRole`] tagging.
//! - [`pose`]       — a per-joint local-transform set; blending; skinning matrices.
//! - [`clip`]       — keyframed animation tracks and time-sampling.
//! - [`ik`]         — two-bone inverse kinematics + look-at (feet, hands, heads).
//! - [`spring`]     — critically-damped springs + spring-bone secondary motion.
//! - [`procedural`] — gait / breathing / recoil generators driven by [`JointRole`].
//! - [`state`]      — the [`state::Animator`]: a deterministic blend state-machine.

pub mod clip;
pub mod ik;
pub mod pose;
pub mod procedural;
pub mod skeleton;
pub mod spring;
pub mod state;

pub use clip::{AnimationClip, JointTrack, Keyframe};
pub use pose::Pose;
pub use skeleton::{Joint, JointRole, LimbSide, Skeleton};
pub use spring::{Spring, SpringV3};
pub use state::{Animator, LocomotionInput, MotionEvent};

use glam::{Mat4, Quat, Vec3};
use serde::{Deserialize, Serialize};

/// A TRS transform: the local pose of one joint relative to its parent. We carry an
/// explicit scale so a generated creature can be non-uniformly squashed/stretched
/// (a "squash and stretch" jump, a swelling cast) without leaving the TRS model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub translation: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
}

impl Default for Transform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Transform {
    /// The identity transform (no translation/rotation, unit scale).
    pub const IDENTITY: Self = Self {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// A pure translation.
    pub fn from_translation(t: Vec3) -> Self {
        Self { translation: t, ..Self::IDENTITY }
    }

    /// A pure rotation.
    pub fn from_rotation(r: Quat) -> Self {
        Self { rotation: r, ..Self::IDENTITY }
    }

    /// Build from all three components.
    pub fn new(translation: Vec3, rotation: Quat, scale: Vec3) -> Self {
        Self { translation, rotation, scale }
    }

    /// This transform as a 4x4 matrix (for skinning / rendering).
    pub fn to_mat4(self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    /// Compose `self * child`: apply `child` then `self` (parent-space chaining). Used
    /// to fold a joint's local transform into its parent's global transform.
    pub fn mul(self, child: Transform) -> Transform {
        Transform {
            translation: self.translation + self.rotation * (self.scale * child.translation),
            rotation: (self.rotation * child.rotation).normalize(),
            scale: self.scale * child.scale,
        }
    }

    /// Transform a point from this space into the parent space.
    pub fn transform_point(self, p: Vec3) -> Vec3 {
        self.translation + self.rotation * (self.scale * p)
    }

    /// Component-wise interpolation between two transforms (nlerp on rotation). `t` is
    /// clamped to `0..=1`. The workhorse of pose blending and clip sampling.
    pub fn lerp(self, other: Transform, t: f32) -> Transform {
        let t = t.clamp(0.0, 1.0);
        Transform {
            translation: self.translation.lerp(other.translation, t),
            // nlerp: cheaper than slerp and stable for the small per-frame deltas here;
            // flip to the near hemisphere first so we take the short way round.
            rotation: nlerp(self.rotation, other.rotation, t),
            scale: self.scale.lerp(other.scale, t),
        }
    }
}

/// Normalised lerp between quaternions, taking the shortest arc. Deterministic and
/// branch-stable — preferred over slerp for the tiny per-tick deltas in this crate.
pub fn nlerp(a: Quat, b: Quat, t: f32) -> Quat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    Quat::from_xyzw(
        a.x + (b.x - a.x) * t,
        a.y + (b.y - a.y) * t,
        a.z + (b.z - a.z) * t,
        a.w + (b.w - a.w) * t,
    )
    .normalize()
}
