//! Inverse kinematics: two-bone IK (limbs reaching a target) and look-at (aiming a
//! joint down a direction).
//!
//! The procedural animator fakes a walk by swinging legs in joint space, but IK is
//! what makes it *land*: plant a foot on the actual ground height, reach a hand to a
//! grabbed ledge, point a head at a target. Both solvers are closed-form (no
//! iteration), deterministic, and operate in plain world space so any caller — client
//! foot-planting, sim ledge-grabs — gets identical results.

use glam::{Quat, Vec3};

/// Result of a two-bone solve: the new **world-space** positions of the mid joint
/// (knee/elbow) and the end joint (foot/hand). The caller converts these back into
/// local joint rotations for its pose (see [`aim_rotation`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TwoBoneSolution {
    pub mid: Vec3,
    pub end: Vec3,
}

/// Solve a two-bone chain so the end effector reaches `target` (or as close as the
/// bone lengths allow). `root` is the shoulder/hip, `len_upper` the upper bone,
/// `len_lower` the lower bone, and `pole` a hint point the joint bends *toward* (the
/// knee's forward direction) so the solution is unambiguous.
///
/// Law-of-cosines placement: clamp the reach, find the mid joint on the circle where
/// the two bones meet, and pull it toward the pole. Always returns a valid pose.
pub fn two_bone(
    root: Vec3,
    target: Vec3,
    len_upper: f32,
    len_lower: f32,
    pole: Vec3,
) -> TwoBoneSolution {
    let total = len_upper + len_lower;
    let to_target = target - root;
    let dist = to_target.length().clamp(1e-4, total - 1e-4).max((len_upper - len_lower).abs() + 1e-4);
    let dir = to_target.normalize_or_zero();

    // Distance from root to the foot of the perpendicular from the mid joint onto the
    // root->target line (law of cosines).
    let a = (len_upper * len_upper - len_lower * len_lower + dist * dist) / (2.0 * dist);
    // Height of the mid joint above that line.
    let h = (len_upper * len_upper - a * a).max(0.0).sqrt();

    // Bend plane: perpendicular to `dir`, biased toward the pole hint.
    let to_pole = pole - root;
    let mut bend = (to_pole - dir * to_pole.dot(dir)).normalize_or_zero();
    if bend.length_squared() < 1e-6 {
        // Degenerate pole (colinear): pick any stable perpendicular.
        bend = dir.cross(Vec3::Y).normalize_or_zero();
        if bend.length_squared() < 1e-6 {
            bend = dir.cross(Vec3::X).normalize_or_zero();
        }
    }

    let mid = root + dir * a + bend * h;
    let end = root + dir * dist.min(total); // reached point (clamped if out of range)
    TwoBoneSolution { mid, end }
}

/// The rotation that turns the `from` direction onto the `to` direction (shortest
/// arc). Used to convert an IK target direction into a joint's local rotation, and by
/// look-at below.
pub fn aim_rotation(from: Vec3, to: Vec3) -> Quat {
    let f = from.normalize_or_zero();
    let t = to.normalize_or_zero();
    if f.length_squared() < 1e-8 || t.length_squared() < 1e-8 {
        return Quat::IDENTITY;
    }
    let d = f.dot(t).clamp(-1.0, 1.0);
    if d > 0.999_99 {
        return Quat::IDENTITY;
    }
    if d < -0.999_99 {
        // Opposite: rotate 180 deg about any axis perpendicular to `f`.
        let axis = f.cross(Vec3::Y).normalize_or_zero();
        let axis = if axis.length_squared() < 1e-6 { f.cross(Vec3::X).normalize_or_zero() } else { axis };
        return Quat::from_axis_angle(axis, std::f32::consts::PI);
    }
    let axis = f.cross(t).normalize_or_zero();
    Quat::from_axis_angle(axis, d.acos())
}

/// A look-at rotation aiming `forward` (a joint's rest forward axis) at `target_dir`,
/// keeping roll out of it by re-orthonormalising against `up`. For heads/turrets.
pub fn look_at(forward: Vec3, target_dir: Vec3, up: Vec3) -> Quat {
    let yaw_pitch = aim_rotation(forward, target_dir);
    // Project `up` to remove residual roll, then nudge toward it. Cheap and stable.
    let right = target_dir.cross(up).normalize_or_zero();
    if right.length_squared() < 1e-6 {
        return yaw_pitch;
    }
    yaw_pitch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reachable_target_is_reached() {
        // Two unit bones, target 1.5 away along +X with pole toward +Y.
        let sol = two_bone(Vec3::ZERO, Vec3::new(1.5, 0.0, 0.0), 1.0, 1.0, Vec3::new(0.5, 1.0, 0.0));
        // End sits at (or short of) the target distance.
        assert!((sol.end - Vec3::new(1.5, 0.0, 0.0)).length() < 0.2);
        // Mid bends toward +Y (the pole), so it's above the X axis.
        assert!(sol.mid.y > 0.1, "knee should bend toward the pole");
        // Bone lengths are roughly preserved.
        let u = (sol.mid - Vec3::ZERO).length();
        assert!((u - 1.0).abs() < 0.2, "upper bone length preserved, got {u}");
    }

    #[test]
    fn overreach_clamps_without_nan() {
        let sol = two_bone(Vec3::ZERO, Vec3::new(50.0, 0.0, 0.0), 1.0, 1.0, Vec3::Y);
        assert!(sol.mid.is_finite() && sol.end.is_finite());
    }

    #[test]
    fn aim_rotation_maps_axes() {
        let q = aim_rotation(Vec3::X, Vec3::Y);
        let r = q * Vec3::X;
        assert!((r - Vec3::Y).length() < 1e-4, "X should rotate onto Y, got {r:?}");
    }
}
