//! Auto-rigging: derive an [`arena_anim::Skeleton`] (and a tuned animator config) for a
//! procedurally-generated creature, **aligned to the body `arena-procgen` grows from
//! the same seed**.
//!
//! `arena_procgen::creature` builds a mob's body from `mesh_seed`: a torso capsule, a
//! head sphere, and 2–5 limbs at seeded angles. We reproduce that exact draw sequence
//! here (same [`arena_procgen::Rng`], same order) so the skeleton's bones land inside
//! the corresponding mesh parts — torso bone in the torso, a leg bone down each leg —
//! and the skinner ([`crate::skin`]) can bind vertices to the nearest bone sensibly.
//!
//! The result is role-tagged ([`JointRole`]) so the procedural animator drives it
//! without caring how many limbs a given creature happens to have.
//!
//! > Note: this mirrors `creature::build_body`'s RNG sequence by construction. If that
//! > generator's draw order changes, update this in lockstep (a shared body-spec would
//! > remove the duplication; deferred to keep `arena-procgen` untouched for now).

use std::f32::consts::TAU;

use glam::Vec3;

use arena_anim::skeleton::{Joint, JointRole, LimbSide, Skeleton};
use arena_anim::state::AnimatorConfig;
use arena_anim::Transform;
use arena_content::mob::MobDef;
use arena_procgen::Rng;

/// Build a skeleton for `mob`, scaled to its visual size so it shares the mesh's space.
pub fn build_skeleton(mob: &MobDef) -> Skeleton {
    // Same seed mix as `creature::build_body` so we walk the identical RNG stream.
    let mut rng = Rng::new(mob.mesh_seed as u64 ^ 0xC0FF_EE17_BEEF_0001);
    let scale = if mob.scale > 0.0 { mob.scale } else { 1.0 };

    // --- reproduce the body's primitive draws, in order ---
    let torso_r = rng.range(0.28, 0.42);
    let torso_h = rng.range(0.35, 0.55);
    let head_r = rng.range(0.22, 0.34);
    let head_dx = rng.range(-0.06, 0.06);

    let mut joints: Vec<Joint> = Vec::new();
    // 0: root at the model origin (torso centre).
    joints.push(Joint {
        name: "root".into(),
        parent: -1,
        bind_local: Transform::IDENTITY,
        role: JointRole::Root,
    });
    // 1: spine, halfway up the torso.
    joints.push(Joint {
        name: "spine".into(),
        parent: 0,
        bind_local: Transform::from_translation(Vec3::new(0.0, torso_h * 0.5 * scale, 0.0)),
        role: JointRole::Spine,
    });
    // 2: head, perched above the torso (matches creature head centre).
    joints.push(Joint {
        name: "head".into(),
        parent: 1,
        bind_local: Transform::from_translation(Vec3::new(
            head_dx * scale,
            (torso_h * 0.5 + head_r * 0.7) * scale,
            0.0,
        )),
        role: JointRole::Head,
    });

    // Limbs: identical seeded sequence to the body generator.
    let limb_count = 2 + (rng.next_u64() % 4) as usize;
    let mut leg_index: u8 = 0;
    let mut arm_index: u8 = 0;
    for _ in 0..limb_count {
        let angle = rng.range(0.0, TAU);
        let pitch = rng.range(-0.6, 0.4);
        let len = rng.range(0.4, 0.8);
        let attach_y = rng.range(-torso_h * 0.5, torso_h * 0.5);
        let _limb_r = rng.range(0.08, 0.16); // consumed to stay in sync with build_body

        let dir = Vec3::new(angle.cos() * pitch.cos(), pitch.sin(), angle.sin() * pitch.cos());
        let attach = Vec3::new(0.0, attach_y, 0.0);
        // Place the bone mid-limb so nearby vertices weight to it.
        let bone_pos = (attach + dir * len * 0.5) * scale;

        // Downward-pointing limbs bear weight (legs); the rest gesture (arms).
        let side = if dir.x < -0.05 {
            LimbSide::Left
        } else if dir.x > 0.05 {
            LimbSide::Right
        } else {
            LimbSide::Center
        };
        let role = if dir.y < -0.1 {
            let r = JointRole::Leg { side, index: leg_index };
            leg_index += 1;
            r
        } else {
            let r = JointRole::Arm { side, index: arm_index };
            arm_index += 1;
            r
        };

        joints.push(Joint {
            name: format!("limb.{}", joints.len()),
            parent: 0,
            bind_local: Transform::from_translation(bone_pos),
            role,
        });
    }

    Skeleton { joints }
}

/// Tune an [`AnimatorConfig`] to a creature's size and speed, so a lumbering golem
/// strides slowly and a darting wisp bobs quickly — all from its [`MobDef`].
pub fn build_config(mob: &MobDef) -> AnimatorConfig {
    let scale = if mob.scale > 0.0 { mob.scale } else { 1.0 };
    let mut cfg = AnimatorConfig::default();
    // Reach full gait intensity at the creature's own top speed.
    cfg.run_speed = mob.move_speed.max(1.0);
    // Larger creatures take slower, longer strides.
    cfg.cadence = (2.4 / scale.clamp(0.4, 4.0)).max(0.6);
    cfg.gait.stride = 0.6 + 0.1 * scale.clamp(0.0, 3.0);
    cfg.gait.bob = 0.05 * scale.clamp(0.5, 3.0);
    cfg
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::ids::MobId;

    fn mob(seed: u32, scale: f32) -> MobDef {
        MobDef {
            id: MobId::new("mob.t"),
            name: "t".into(),
            max_health: 100.0,
            move_speed: 4.0,
            abilities: vec![],
            xp_reward: 0,
            loot_table: vec![],
            material: None,
            scale,
            aggressive: false,
            mesh_seed: seed,
        }
    }

    #[test]
    fn rig_is_deterministic_and_valid() {
        let a = build_skeleton(&mob(0x7001, 1.0));
        let b = build_skeleton(&mob(0x7001, 1.0));
        assert_eq!(a.len(), b.len());
        assert!(a.len() >= 5, "root+spine+head plus >=2 limbs");
        a.validate().expect("rig must be topologically ordered");
        // Root, spine, head are always present.
        assert!(matches!(a.joints[0].role, JointRole::Root));
        assert!(matches!(a.joints[1].role, JointRole::Spine));
        assert!(matches!(a.joints[2].role, JointRole::Head));
    }

    #[test]
    fn scale_grows_the_rig() {
        let small = build_skeleton(&mob(0x7004, 1.0));
        let big = build_skeleton(&mob(0x7004, 3.0));
        // Same seed, same joint count; big one's head sits higher.
        assert_eq!(small.len(), big.len());
        assert!(big.global_bind()[2].translation.y > small.global_bind()[2].translation.y);
    }
}
