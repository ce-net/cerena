//! Noise evaluation — turning [`NoiseLayer`] recipes into scalar fields.
//!
//! This module is the bridge between the `noise` crate's generators and Cerena's
//! data-driven [`arena_content::material::NoiseLayer`] descriptions. A material or
//! world param is a *stack* of layers; we evaluate the stack into a single scalar at
//! any point in space, which then drives a colour ramp, a displacement, or a terrain
//! height.
//!
//! All functions here are deterministic: the `noise` crate seeds (`Perlin::new(seed)`
//! etc.) make a given layer reproducible on every machine, and [`Rng`] is a
//! deterministic splitmix64 for placement decisions. Same seed in, same field out.
//!
//! Note on the `noise` 0.9 API: generators implement `NoiseFn<f64, 3>` with
//! `.get([f64; 3]) -> f64` returning roughly `[-1, 1]`. We build fbm / ridged / flow
//! on top of [`Perlin`] ourselves rather than using `noise::Fbm`, because that gives
//! us exact control over the per-layer `octaves` / `lacunarity` / `gain` from the
//! content definition.

use arena_content::material::{NoiseKind, NoiseLayer};
use glam::Vec3;
use noise::{NoiseFn, Perlin, Worley};

/// Fractal Brownian motion: sum of `octaves` Perlin octaves, each at higher
/// frequency (`lacunarity`) and lower amplitude (`gain`). Returns roughly `[-1, 1]`.
///
/// This is the workhorse used directly for soft cloudy detail and indirectly as the
/// building block for ridged, flow, and domain-warp layers.
pub fn fbm(p: Vec3, octaves: u8, lacunarity: f32, gain: f32, seed: u32) -> f32 {
    // One generator per call. `Perlin::new` builds a 256-entry permutation table, so
    // for very hot loops a caller could hoist this; for procgen the simplicity wins.
    let perlin = Perlin::new(seed);
    let mut freq = 1.0f32;
    let mut amp = 1.0f32;
    let mut sum = 0.0f32;
    let mut norm = 0.0f32;
    for _ in 0..octaves.max(1) {
        let v = perlin.get([
            (p.x * freq) as f64,
            (p.y * freq) as f64,
            (p.z * freq) as f64,
        ]) as f32;
        sum += v * amp;
        norm += amp;
        amp *= gain;
        freq *= lacunarity;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

/// Ridged multifractal: `1 - |noise|` per octave, squared to sharpen the ridges.
/// Returns roughly `[0, 1]` (peaks near 1). Used for mountain ranges and veins.
fn ridged(p: Vec3, octaves: u8, lacunarity: f32, gain: f32, seed: u32) -> f32 {
    let perlin = Perlin::new(seed);
    let mut freq = 1.0f32;
    let mut amp = 1.0f32;
    let mut sum = 0.0f32;
    let mut norm = 0.0f32;
    for _ in 0..octaves.max(1) {
        let n = perlin.get([
            (p.x * freq) as f64,
            (p.y * freq) as f64,
            (p.z * freq) as f64,
        ]) as f32;
        // The ridge: invert the absolute value so zero-crossings become sharp crests.
        let r = 1.0 - n.abs();
        sum += (r * r) * amp;
        norm += amp;
        amp *= gain;
        freq *= lacunarity;
    }
    if norm > 0.0 { sum / norm } else { 0.0 }
}

/// Worley / cellular noise via the `noise` crate. Returns roughly `[-1, 1]`. Used for
/// organic cells, scales, cracks, and (in 3D) the void pockets of cave systems.
fn worley(p: Vec3, seed: u32) -> f32 {
    let w = Worley::new(seed);
    w.get([p.x as f64, p.y as f64, p.z as f64]) as f32
}

/// Flow noise: directional, smeared streaks. We approximate true flow noise by
/// advecting the sample point along a fixed direction by an amount that itself varies
/// with noise — cheap, but gives the smeared look of lava / water / wind.
fn flow(p: Vec3, octaves: u8, lacunarity: f32, gain: f32, seed: u32) -> f32 {
    let dir = Vec3::new(1.0, 0.0, 0.35);
    let smear = fbm(p, 3, lacunarity, gain, seed ^ 0x55AA_55AA);
    fbm(p + dir * smear * 0.5, octaves, lacunarity, gain, seed)
}

/// Perturb a point by a noise-derived offset, scaled by `warp`. This is the heart of
/// the "melted / marbled / hand-grown" look: by displacing where we sample, straight
/// features bend and pool organically. `warp <= 0` is a no-op.
fn warp_point(p: Vec3, warp: f32, seed: u32) -> Vec3 {
    if warp <= 0.0 {
        return p;
    }
    // Three decorrelated fbm fields give an offset vector. The constant offsets keep
    // the three channels from sampling the same location.
    let wx = fbm(p + Vec3::splat(11.3), 3, 2.0, 0.5, seed ^ 0x00A1_00A1);
    let wy = fbm(p + Vec3::splat(31.7), 3, 2.0, 0.5, seed ^ 0x00B2_00B2);
    let wz = fbm(p + Vec3::splat(57.2), 3, 2.0, 0.5, seed ^ 0x00C3_00C3);
    p + Vec3::new(wx, wy, wz) * warp
}

/// Evaluate a single [`NoiseLayer`] at `p`, returning its (amplitude-weighted)
/// contribution. The layer's `frequency` scales the sample point, its `warp` domain-
/// warps it, and `kind` selects the generator.
pub fn sample_layer(layer: &NoiseLayer, p: Vec3) -> f32 {
    // Frequency first, then domain warp — so the warp acts in the layer's own scale.
    let sp = warp_point(p * layer.frequency, layer.warp, layer.seed);
    let raw = match layer.kind {
        NoiseKind::Fbm => fbm(sp, layer.octaves, layer.lacunarity, layer.gain, layer.seed),
        NoiseKind::Ridged => ridged(sp, layer.octaves, layer.lacunarity, layer.gain, layer.seed),
        NoiseKind::Worley => worley(sp, layer.seed),
        NoiseKind::Flow => flow(sp, layer.octaves, layer.lacunarity, layer.gain, layer.seed),
        NoiseKind::DomainWarp => {
            // A DomainWarp layer's whole purpose is to distort coordinates, so we warp
            // again (strongly) before sampling an fbm. This is what makes a material
            // look marbled rather than cloudy.
            let strength = layer.warp.max(0.5);
            let warped = warp_point(sp, strength, layer.seed ^ 0x9E37_79B9);
            fbm(warped, layer.octaves, layer.lacunarity, layer.gain, layer.seed)
        }
    };
    raw * layer.amplitude
}

/// Composite a stack of layers (in order) into a single scalar — the driving field
/// for a material or terrain. Layers simply sum; amplitudes balance their weights.
pub fn sample_stack(layers: &[NoiseLayer], p: Vec3) -> f32 {
    layers.iter().map(|l| sample_layer(l, p)).sum()
}

/// A tiny, fast, deterministic PRNG (splitmix64). Used for *placement* decisions
/// (structure scattering, creature part jitter) where we need reproducible randomness
/// keyed by a u64 seed. Not cryptographic — it just has to be identical everywhere.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seed the generator. Any u64 is fine; mix it yourself if combining several.
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next raw 64-bit value (splitmix64 step).
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform `f32` in `[0, 1)`. Uses the top 24 bits for a clean mantissa.
    pub fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / ((1u32 << 24) as f32)
    }

    /// Uniform `f32` in `[lo, hi)`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fbm_is_bounded_and_deterministic() {
        let p = Vec3::new(1.3, -2.1, 0.7);
        let a = fbm(p, 5, 2.0, 0.5, 42);
        let b = fbm(p, 5, 2.0, 0.5, 42);
        assert_eq!(a, b, "fbm must be deterministic for a fixed seed");
        assert!(a.abs() <= 1.5, "fbm should stay roughly in [-1, 1], got {a}");
    }

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(123);
        let mut b = Rng::new(123);
        for _ in 0..16 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }
}
