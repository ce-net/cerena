//! The joint hierarchy: a [`Skeleton`] is a flat, topologically-ordered list of
//! [`Joint`]s, each naming its parent and carrying a bind-pose [`Transform`].
//!
//! Two design choices make this fit Cerena:
//!
//! 1. **Flat + ordered.** Joints are stored in an array with `parent < child` always
//!    true, so computing global transforms is a single forward pass with no recursion
//!    and no allocation — important when thousands of creatures animate per frame.
//! 2. **Role-tagged.** Because creatures are *procedurally* rigged with a variable
//!    number of limbs, animation can't hard-code "joint 4 is the left leg". Instead
//!    every joint carries a [`JointRole`], and the procedural animator drives motion
//!    by role. A two-limbed wisp and a five-limbed wraith share the exact same code.

use glam::Mat4;
use serde::{Deserialize, Serialize};

use crate::Transform;

/// Which side of the body a limb is on (for counter-swing and gait phase offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LimbSide {
    Left,
    Right,
    Center,
}

/// The functional role of a joint, assigned by the rigger. The procedural animator
/// ([`crate::procedural`]) reads these instead of fixed indices, so it animates any
/// generated body shape without per-creature code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum JointRole {
    /// The grounded root (pelvis / core); the whole rig hangs off it.
    Root,
    /// A spine segment between root and head (breathing, lean).
    Spine,
    /// The head (look-at, bob).
    Head,
    /// A weight-bearing limb that drives the walk gait. `index` distinguishes
    /// multiple legs; `side` sets its phase offset.
    Leg { side: LimbSide, index: u8 },
    /// A non-weight-bearing limb that counter-swings / gestures while casting.
    Arm { side: LimbSide, index: u8 },
    /// A trailing appendage (tail, tentacle, frond) animated as a spring bone.
    Tail { index: u8 },
    /// Anything unclassified — animated only by inherited parent motion.
    Other,
}

impl JointRole {
    /// True for the limbs the gait generator swings to fake walking.
    pub fn is_leg(self) -> bool {
        matches!(self, JointRole::Leg { .. })
    }
    /// True for limbs that counter-swing / gesture (arms, and tails lightly).
    pub fn is_arm(self) -> bool {
        matches!(self, JointRole::Arm { .. })
    }
}

/// One joint: a name, its parent index (`-1` for a root), its bind-pose local
/// transform, and its [`JointRole`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Joint {
    /// Human / debug name (e.g. `"leg.l.0"`). Not used for lookup on the hot path.
    pub name: String,
    /// Index of the parent joint in the skeleton's array, or `-1` if this is a root.
    /// Invariant: `parent < self_index` (skeletons are topologically ordered).
    pub parent: i32,
    /// Rest-pose transform relative to the parent.
    pub bind_local: Transform,
    pub role: JointRole,
}

/// A complete skeleton: an ordered joint array plus cached inverse-bind matrices for
/// skinning. Pure data; serializes into a content-addressed rig asset.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Skeleton {
    pub joints: Vec<Joint>,
}

impl Skeleton {
    /// Number of joints.
    pub fn len(&self) -> usize {
        self.joints.len()
    }

    /// True if the skeleton has no joints.
    pub fn is_empty(&self) -> bool {
        self.joints.is_empty()
    }

    /// The parent index of joint `i`, or `None` for a root.
    pub fn parent(&self, i: usize) -> Option<usize> {
        let p = self.joints[i].parent;
        if p < 0 { None } else { Some(p as usize) }
    }

    /// Global (model-space) **bind** transforms: fold each joint's `bind_local` down
    /// the hierarchy. One forward pass thanks to topological ordering.
    pub fn global_bind(&self) -> Vec<Transform> {
        let mut out: Vec<Transform> = Vec::with_capacity(self.joints.len());
        for (i, j) in self.joints.iter().enumerate() {
            let g = match self.parent(i) {
                None => j.bind_local,
                Some(p) => out[p].mul(j.bind_local),
            };
            out.push(g);
        }
        out
    }

    /// Inverse of each joint's global bind matrix — the matrix that takes a vertex from
    /// model space into a joint's local bind space, the first half of linear-blend
    /// skinning. Cached by `arena-assets` next to the mesh it skins.
    pub fn inverse_bind_matrices(&self) -> Vec<Mat4> {
        self.global_bind()
            .into_iter()
            .map(|t| t.to_mat4().inverse())
            .collect()
    }

    /// Model-space bind **position** of every joint (the translation of its global
    /// bind transform). Used by the skinner to weight vertices to nearby bones.
    pub fn bind_positions(&self) -> Vec<glam::Vec3> {
        self.global_bind().into_iter().map(|t| t.translation).collect()
    }

    /// Indices of joints matching a role predicate, in skeleton order. Convenience for
    /// the procedural animator ("all legs", "all tails").
    pub fn joints_where(&self, pred: impl Fn(JointRole) -> bool) -> Vec<usize> {
        self.joints
            .iter()
            .enumerate()
            .filter(|(_, j)| pred(j.role))
            .map(|(i, _)| i)
            .collect()
    }

    /// The first joint with the given role match, if any (e.g. the head).
    pub fn find(&self, pred: impl Fn(JointRole) -> bool) -> Option<usize> {
        self.joints.iter().position(|j| pred(j.role))
    }

    /// Debug/authoring guard: every parent index is a real, earlier joint. Returns the
    /// offending index on failure. (Cheap; call it in tests / asset bake validation.)
    pub fn validate(&self) -> Result<(), usize> {
        for (i, j) in self.joints.iter().enumerate() {
            if j.parent >= 0 && (j.parent as usize >= i) {
                return Err(i);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec3;

    fn chain() -> Skeleton {
        // root -> spine -> head, stacked +Y by 1 each.
        Skeleton {
            joints: vec![
                Joint { name: "root".into(), parent: -1, bind_local: Transform::IDENTITY, role: JointRole::Root },
                Joint { name: "spine".into(), parent: 0, bind_local: Transform::from_translation(Vec3::Y), role: JointRole::Spine },
                Joint { name: "head".into(), parent: 1, bind_local: Transform::from_translation(Vec3::Y), role: JointRole::Head },
            ],
        }
    }

    #[test]
    fn global_bind_accumulates_down_the_chain() {
        let s = chain();
        let g = s.global_bind();
        assert!((g[2].translation - Vec3::new(0.0, 2.0, 0.0)).length() < 1e-5);
        s.validate().expect("ordered skeleton validates");
    }

    #[test]
    fn inverse_bind_undoes_bind() {
        let s = chain();
        let g = s.global_bind();
        let inv = s.inverse_bind_matrices();
        // global * inverse_bind == identity at bind pose.
        let m = g[2].to_mat4() * inv[2];
        assert!(m.abs_diff_eq(Mat4::IDENTITY, 1e-4), "global*inverse_bind should be identity, got {m:?}");
    }
}
