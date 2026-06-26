//! A [`Pose`]: the per-joint *local* transforms that override a skeleton's bind pose
//! for one instant. Everything that produces motion in this crate — clip sampling,
//! the procedural gait, IK — writes into a `Pose`; the renderer turns one into
//! skinning matrices.
//!
//! A pose is parallel to its skeleton: `locals[i]` is the local transform of joint
//! `i`. Starting from the bind pose and editing only the joints a system cares about
//! (a gait touches legs, a cast touches arms) keeps every generator composable.

use glam::Mat4;
use serde::{Deserialize, Serialize};

use crate::skeleton::Skeleton;
use crate::Transform;

/// A full set of local joint transforms for one frame.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Pose {
    /// Local transform per joint, parallel to [`Skeleton::joints`].
    pub locals: Vec<Transform>,
}

impl Pose {
    /// The rest pose: every joint at its bind-pose local transform. Generators start
    /// here and edit the joints they own.
    pub fn rest(skeleton: &Skeleton) -> Self {
        Self {
            locals: skeleton.joints.iter().map(|j| j.bind_local).collect(),
        }
    }

    /// An all-identity pose sized to a skeleton (rarely what you want directly; `rest`
    /// is the usual base, but handy for additive layers).
    pub fn identity(joint_count: usize) -> Self {
        Self { locals: vec![Transform::IDENTITY; joint_count] }
    }

    /// Number of joints this pose covers.
    pub fn len(&self) -> usize {
        self.locals.len()
    }

    /// True if the pose has no joints.
    pub fn is_empty(&self) -> bool {
        self.locals.is_empty()
    }

    /// Linearly blend two poses joint-by-joint (`t` in `0..=1`). The basis of every
    /// crossfade in [`crate::state::Animator`]. Length-mismatched poses blend over the
    /// shared prefix (defensive; both should match the same skeleton).
    pub fn blend(&self, other: &Pose, t: f32) -> Pose {
        let n = self.locals.len().min(other.locals.len());
        let mut locals = Vec::with_capacity(n);
        for i in 0..n {
            locals.push(self.locals[i].lerp(other.locals[i], t));
        }
        Pose { locals }
    }

    /// Blend `other` into `self` in place by weight `t` (cheaper than [`Pose::blend`]
    /// when you already own the buffer — the per-frame default in the animator).
    pub fn blend_into(&mut self, other: &Pose, t: f32) {
        let n = self.locals.len().min(other.locals.len());
        for i in 0..n {
            self.locals[i] = self.locals[i].lerp(other.locals[i], t);
        }
    }

    /// Apply an *additive* layer: add `layer`'s deviation-from-rest onto this pose by
    /// `weight`. `rest` is the skeleton's rest pose the layer was authored against.
    /// Used to stack a recoil / breathing twitch on top of a locomotion base without
    /// the layers fighting over absolute values.
    pub fn add_layer(&mut self, layer: &Pose, rest: &Pose, weight: f32) {
        let n = self
            .locals
            .len()
            .min(layer.locals.len())
            .min(rest.locals.len());
        for i in 0..n {
            // deviation = layer relative to rest, scaled, applied on top of current.
            let base = self.locals[i];
            let dev_t = layer.locals[i];
            let rest_t = rest.locals[i];
            self.locals[i] = Transform {
                translation: base.translation + (dev_t.translation - rest_t.translation) * weight,
                rotation: crate::nlerp(base.rotation, base.rotation * (rest_t.rotation.inverse() * dev_t.rotation), weight),
                scale: base.scale * Transform::IDENTITY.scale.lerp(dev_t.scale / rest_t.scale, weight),
            };
        }
    }

    /// Fold this pose down `skeleton`'s hierarchy into model-space **global**
    /// transforms (one forward pass; requires topological ordering).
    pub fn global_transforms(&self, skeleton: &Skeleton) -> Vec<Transform> {
        let mut out: Vec<Transform> = Vec::with_capacity(self.locals.len());
        for i in 0..self.locals.len() {
            let local = self.locals[i];
            let g = match skeleton.parent(i) {
                None => local,
                Some(p) => out[p].mul(local),
            };
            out.push(g);
        }
        out
    }

    /// The skinning matrices for this pose: `global(i) * inverse_bind(i)` per joint.
    /// This is exactly what a vertex shader multiplies its bone-weighted positions by;
    /// the renderer uploads these straight into a bone buffer. Pass the skeleton's
    /// cached `inverse_bind` (from [`Skeleton::inverse_bind_matrices`]) to avoid
    /// recomputing it every frame.
    pub fn skinning_matrices(&self, skeleton: &Skeleton, inverse_bind: &[Mat4]) -> Vec<Mat4> {
        let globals = self.global_transforms(skeleton);
        globals
            .iter()
            .zip(inverse_bind.iter())
            .map(|(g, ib)| g.to_mat4() * *ib)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::{Joint, JointRole};
    use glam::Vec3;

    fn two_joint() -> Skeleton {
        Skeleton {
            joints: vec![
                Joint { name: "r".into(), parent: -1, bind_local: Transform::IDENTITY, role: JointRole::Root },
                Joint { name: "h".into(), parent: 0, bind_local: Transform::from_translation(Vec3::Y), role: JointRole::Head },
            ],
        }
    }

    #[test]
    fn rest_pose_skins_to_identity() {
        let s = two_joint();
        let ib = s.inverse_bind_matrices();
        let pose = Pose::rest(&s);
        for m in pose.skinning_matrices(&s, &ib) {
            assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-4), "rest pose should skin to identity");
        }
    }

    #[test]
    fn blend_midpoint_is_halfway() {
        let s = two_joint();
        let a = Pose::rest(&s);
        let mut b = Pose::rest(&s);
        b.locals[1].translation = Vec3::new(0.0, 3.0, 0.0);
        let mid = a.blend(&b, 0.5);
        assert!((mid.locals[1].translation.y - 2.0).abs() < 1e-5); // bind 1.0 .. 3.0 midpoint 2.0
    }
}
