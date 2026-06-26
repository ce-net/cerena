//! Critically-damped springs and spring-bone chains for **secondary motion**.
//!
//! A spring is the cheapest way to make rigid procedural motion feel alive: a head
//! that lags then catches up, a tail that trails a turn, a camera that eases to its
//! target. These springs are *semi-implicit* and stable for any `dt` (no overshoot
//! blow-up), and — like everything in this crate — deterministic given the same `dt`.
//!
//! The same [`Spring`] / [`SpringV3`] are reused well beyond animation: `arena-sim`'s
//! physics layer uses them for camera smoothing and network reconciliation easing, so
//! the easing feel is identical everywhere.

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// A scalar critically-damped spring toward a moving target.
///
/// `stiffness` (omega^2-ish) sets how fast it chases; the damping is derived to be
/// critical (no oscillation) from `half_life`, the time to close half the remaining
/// gap. Tune with `half_life` — it is intuitive ("catch up in ~0.1 s").
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Spring {
    pub value: f32,
    pub velocity: f32,
    /// Time (seconds) to halve the remaining distance. Smaller = snappier.
    pub half_life: f32,
}

impl Spring {
    pub fn new(value: f32, half_life: f32) -> Self {
        Self { value, velocity: 0.0, half_life: half_life.max(1e-4) }
    }

    /// Advance toward `target` over `dt`. Uses the exact critically-damped solution so
    /// it never overshoots and is stable at large `dt`.
    pub fn step(&mut self, target: f32, dt: f32) -> f32 {
        // y = 2 * ln(2) / half_life is the damping ratio for a critical spring whose
        // envelope halves every `half_life`.
        let y = 2.0 * std::f32::consts::LN_2 / self.half_life;
        let j0 = self.value - target;
        let j1 = self.velocity + j0 * y;
        let e = (-y * dt).exp();
        self.value = target + (j0 + j1 * dt) * e;
        self.velocity = (self.velocity - j1 * y * dt) * e;
        self.value
    }
}

/// A 3-component critically-damped spring (three independent [`Spring`]s). For
/// positions, look directions, colours — anything that should ease, not snap.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpringV3 {
    pub value: Vec3,
    pub velocity: Vec3,
    pub half_life: f32,
}

impl SpringV3 {
    pub fn new(value: Vec3, half_life: f32) -> Self {
        Self { value, velocity: Vec3::ZERO, half_life: half_life.max(1e-4) }
    }

    /// Advance every axis toward `target` over `dt`.
    pub fn step(&mut self, target: Vec3, dt: f32) -> Vec3 {
        let y = 2.0 * std::f32::consts::LN_2 / self.half_life;
        let j0 = self.value - target;
        let j1 = self.velocity + j0 * y;
        let e = (-y * dt).exp();
        self.value = target + (j0 + j1 * dt) * e;
        self.velocity = (self.velocity - j1 * y * dt) * e;
        self.value
    }
}

/// A chain of point masses that trail a root — a **spring bone** for tails, fronds,
/// tentacles, hair. Each link springs toward its rest offset from the previous link,
/// so when the root moves the chain whips and settles. The result is fed back into a
/// pose by aiming each joint at the next link.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SpringChain {
    /// World-space position of each link (index 0 is the anchored root).
    pub points: Vec<Vec3>,
    /// Per-link velocity.
    pub velocities: Vec<Vec3>,
    /// Rest length between consecutive links.
    pub rest_length: f32,
    /// Chase rate; higher = stiffer chain.
    pub stiffness: f32,
    /// Velocity damping per second (0..1-ish after dt scaling).
    pub damping: f32,
}

impl SpringChain {
    /// A straight chain of `n` links hanging along `dir` from `anchor`.
    pub fn new(anchor: Vec3, dir: Vec3, n: usize, rest_length: f32) -> Self {
        let d = dir.normalize_or_zero();
        let points = (0..n).map(|i| anchor + d * (rest_length * i as f32)).collect();
        Self {
            points,
            velocities: vec![Vec3::ZERO; n],
            rest_length,
            stiffness: 40.0,
            damping: 6.0,
        }
    }

    /// Pin link 0 to `anchor`, then integrate the rest toward their rest offset from
    /// the previous link under an optional `gravity` (e.g. a drooping tail). Stable
    /// semi-implicit Euler; deterministic in `dt`.
    pub fn step(&mut self, anchor: Vec3, gravity: Vec3, dt: f32) {
        if self.points.is_empty() {
            return;
        }
        self.points[0] = anchor;
        self.velocities[0] = Vec3::ZERO;
        for i in 1..self.points.len() {
            let prev = self.points[i - 1];
            let dir = (self.points[i] - prev).normalize_or_zero();
            let rest_target = prev + dir * self.rest_length;
            let accel = (rest_target - self.points[i]) * self.stiffness + gravity;
            let mut v = self.velocities[i] + accel * dt;
            v *= 1.0 / (1.0 + self.damping * dt); // implicit-ish damping, always stable
            self.velocities[i] = v;
            self.points[i] += v * dt;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_converges_to_target() {
        let mut s = Spring::new(0.0, 0.1);
        for _ in 0..200 {
            s.step(10.0, 1.0 / 64.0);
        }
        assert!((s.value - 10.0).abs() < 1e-2, "spring should settle on target, got {}", s.value);
        assert!(s.velocity.abs() < 1e-2);
    }

    #[test]
    fn spring_does_not_overshoot_wildly_at_big_dt() {
        let mut s = Spring::new(0.0, 0.05);
        // A huge dt must not explode (stability of the closed-form integrator).
        let v = s.step(1.0, 5.0);
        assert!(v.is_finite() && v <= 1.001, "no overshoot/explosion, got {v}");
    }

    #[test]
    fn chain_settles_to_rest_length() {
        let mut c = SpringChain::new(Vec3::ZERO, Vec3::X, 4, 0.5);
        for _ in 0..500 {
            c.step(Vec3::ZERO, Vec3::ZERO, 1.0 / 64.0);
        }
        let seg = (c.points[1] - c.points[0]).length();
        assert!((seg - 0.5).abs() < 0.1, "links should rest at rest_length, got {seg}");
    }
}
