//! Keyframed animation clips.
//!
//! Cerena's default creatures animate procedurally ([`crate::procedural`]), but a clip
//! is the right tool for anything with *authored* motion: a boss's scripted slam, a
//! spell-effect mesh that pulses on a fixed cadence, an emote. A [`AnimationClip`] is
//! a set of sparse per-joint [`JointTrack`]s; sampling at a time `t` produces a
//! [`Pose`] by interpolating each track and leaving untouched joints at rest.
//!
//! Like everything here it is pure data and deterministic: sampling the same clip at
//! the same `t` always yields the same pose.

use serde::{Deserialize, Serialize};

use crate::pose::Pose;
use crate::skeleton::Skeleton;
use crate::Transform;

/// One keyframe on a joint track: a local [`Transform`] at a time (seconds).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    pub time: f32,
    pub transform: Transform,
}

/// A track of keyframes for a single joint. Keys must be sorted ascending by `time`
/// (the sampler assumes this and binary-searches them).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JointTrack {
    /// Index of the joint this track drives, in the target skeleton's array.
    pub joint: u16,
    pub keys: Vec<Keyframe>,
}

impl JointTrack {
    /// Sample this track at `time`, clamping to the first/last key outside the range
    /// and linearly interpolating between the bracketing keys inside it.
    pub fn sample(&self, time: f32) -> Transform {
        match self.keys.as_slice() {
            [] => Transform::IDENTITY,
            [only] => only.transform,
            keys => {
                if time <= keys[0].time {
                    return keys[0].transform;
                }
                let last = &keys[keys.len() - 1];
                if time >= last.time {
                    return last.transform;
                }
                // Binary search for the first key strictly after `time`.
                let hi = keys.partition_point(|k| k.time <= time).max(1);
                let a = &keys[hi - 1];
                let b = &keys[hi];
                let span = (b.time - a.time).max(f32::EPSILON);
                let f = (time - a.time) / span;
                a.transform.lerp(b.transform, f)
            }
        }
    }
}

/// A complete clip: a duration, a loop flag, and the joint tracks it animates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimationClip {
    /// Human / debug name (e.g. `"slam"`).
    pub name: String,
    /// Total length in seconds.
    pub duration: f32,
    /// Whether time wraps (cyclic gaits) or holds on the last frame (one-shots).
    pub looping: bool,
    /// Sparse per-joint tracks. Joints with no track stay at their rest pose.
    pub tracks: Vec<JointTrack>,
}

impl AnimationClip {
    /// Map a raw playback time onto the clip's local time, honouring `looping`.
    pub fn local_time(&self, t: f32) -> f32 {
        if self.duration <= 0.0 {
            return 0.0;
        }
        if self.looping {
            t.rem_euclid(self.duration)
        } else {
            t.clamp(0.0, self.duration)
        }
    }

    /// Sample the clip into a fresh pose over `skeleton`'s rest pose: every tracked
    /// joint is interpolated; every other joint keeps its bind transform.
    pub fn sample(&self, skeleton: &Skeleton, time: f32) -> Pose {
        let mut pose = Pose::rest(skeleton);
        let lt = self.local_time(time);
        for track in &self.tracks {
            let j = track.joint as usize;
            if j < pose.locals.len() {
                pose.locals[j] = track.sample(lt);
            }
        }
        pose
    }

    /// True once a non-looping clip has played past its end (for one-shot teardown).
    pub fn finished(&self, t: f32) -> bool {
        !self.looping && t >= self.duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::{Joint, JointRole};
    use glam::Vec3;

    #[test]
    fn track_interpolates_between_keys() {
        let track = JointTrack {
            joint: 0,
            keys: vec![
                Keyframe { time: 0.0, transform: Transform::from_translation(Vec3::ZERO) },
                Keyframe { time: 1.0, transform: Transform::from_translation(Vec3::new(0.0, 10.0, 0.0)) },
            ],
        };
        assert!((track.sample(0.5).translation.y - 5.0).abs() < 1e-5);
        assert!((track.sample(-1.0).translation.y).abs() < 1e-5); // clamped to first
        assert!((track.sample(2.0).translation.y - 10.0).abs() < 1e-5); // clamped to last
    }

    #[test]
    fn looping_wraps_time() {
        let clip = AnimationClip {
            name: "c".into(),
            duration: 2.0,
            looping: true,
            tracks: vec![],
        };
        assert!((clip.local_time(2.5) - 0.5).abs() < 1e-5);
        let sk = Skeleton { joints: vec![Joint { name: "r".into(), parent: -1, bind_local: Transform::IDENTITY, role: JointRole::Root }] };
        // Sampling an empty-track clip leaves the rest pose intact.
        assert_eq!(clip.sample(&sk, 9.0).locals.len(), 1);
    }
}
