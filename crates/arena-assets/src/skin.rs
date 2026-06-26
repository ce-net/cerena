//! Skinning: bind a mesh's vertices to a skeleton's bones.
//!
//! Procedural meshes have no authored skin weights, so we derive them: each vertex is
//! weighted to the nearest few bones by inverse-square distance to their bind
//! positions. It is a heuristic, but because the rig ([`crate::rig`]) is built to sit
//! *inside* the mesh, "nearest bone" is the right bone — a vertex on a leg weights to
//! the leg bone — and linear-blend skinning then moves it with that limb.

use glam::Vec3;

use arena_anim::skeleton::Skeleton;

/// Up to this many bone influences per vertex (the GPU-standard 4).
pub const MAX_INFLUENCES: usize = 4;

/// Compute per-vertex bone indices + weights binding `positions` to `skeleton`.
/// Returns `(joints, weights)` parallel to `positions`; each row has 4 entries
/// (zero-weighted padding where a vertex has fewer than 4 nearby bones). Returns empty
/// vectors for an empty skeleton (the caller then treats the mesh as static/rigid).
pub fn skin_mesh(positions: &[[f32; 3]], skeleton: &Skeleton) -> (Vec<[u16; 4]>, Vec<[f32; 4]>) {
    if skeleton.is_empty() || positions.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let bones = skeleton.bind_positions();
    let mut joints = Vec::with_capacity(positions.len());
    let mut weights = Vec::with_capacity(positions.len());

    for p in positions {
        let v = Vec3::from_array(*p);
        // Score every bone by inverse-square distance, keep the best `MAX_INFLUENCES`.
        let mut best: [(u16, f32); MAX_INFLUENCES] = [(0, 0.0); MAX_INFLUENCES];
        for (bi, b) in bones.iter().enumerate() {
            let d2 = (v - *b).length_squared();
            let w = 1.0 / (d2 + 1e-3);
            // Insertion into the small fixed top-k (replace the current weakest).
            let mut weakest = 0;
            for k in 1..MAX_INFLUENCES {
                if best[k].1 < best[weakest].1 {
                    weakest = k;
                }
            }
            if w > best[weakest].1 {
                best[weakest] = (bi as u16, w);
            }
        }
        // Normalise the kept weights so they sum to 1 (LBS partition of unity).
        let sum: f32 = best.iter().map(|(_, w)| *w).sum();
        let inv = if sum > 0.0 { 1.0 / sum } else { 0.0 };
        joints.push([best[0].0, best[1].0, best[2].0, best[3].0]);
        weights.push([
            best[0].1 * inv,
            best[1].1 * inv,
            best[2].1 * inv,
            best[3].1 * inv,
        ]);
    }

    (joints, weights)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_anim::skeleton::{Joint, JointRole};
    use arena_anim::Transform;

    #[test]
    fn weights_sum_to_one_and_pick_nearest() {
        // Two bones: one at origin, one high up. A vertex near origin should weight
        // mostly to bone 0.
        let skel = Skeleton {
            joints: vec![
                Joint { name: "a".into(), parent: -1, bind_local: Transform::IDENTITY, role: JointRole::Root },
                Joint { name: "b".into(), parent: 0, bind_local: Transform::from_translation(Vec3::new(0.0, 5.0, 0.0)), role: JointRole::Head },
            ],
        };
        let (joints, weights) = skin_mesh(&[[0.0, 0.1, 0.0]], &skel);
        assert_eq!(joints.len(), 1);
        let total: f32 = weights[0].iter().sum();
        assert!((total - 1.0).abs() < 1e-4, "weights must sum to 1, got {total}");
        // Bone 0 (at origin) is nearest, so its weight dominates.
        let w0 = weights[0][joints[0].iter().position(|&j| j == 0).unwrap()];
        assert!(w0 > 0.8, "nearest bone should dominate, got {w0}");
    }

    #[test]
    fn empty_skeleton_yields_static() {
        let (j, w) = skin_mesh(&[[0.0, 0.0, 0.0]], &Skeleton::default());
        assert!(j.is_empty() && w.is_empty());
    }
}
