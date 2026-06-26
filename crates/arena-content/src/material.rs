//! Procedural materials and shaders — the look of an organic, hand-grown world.
//!
//! Cerena avoids hard-edged authored textures: surfaces are synthesised from layered
//! noise and coloured by ramps, so everything reads as natural and "grown". A
//! [`MaterialDef`] is the recipe `arena-procgen` evaluates to bake textures (and that
//! `arena-client` feeds to its GPU material), and a [`ShaderDef`] carries hot-
//! recompilable WGSL. All of it is data, so the art direction is tunable live.

use serde::{Deserialize, Serialize};

use crate::ids::{MaterialId, ShaderId};

/// The family of a noise layer. Each maps to a generator in `arena-procgen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoiseKind {
    /// Fractal Brownian motion — soft cloudy detail (default workhorse).
    Fbm,
    /// Worley / cellular — organic cells, scales, cracks.
    Worley,
    /// Ridged multifractal — sharp veins, mountain ridges, lightning.
    Ridged,
    /// Flow noise — directional, smeared streaks (lava, water, wind).
    Flow,
    /// Domain-warp — distorts sample coordinates of the layers above it for a
    /// melted, marbled, hand-grown look.
    DomainWarp,
}

/// One octave-stack of noise. Layers are composited (in order) by the procedural
/// material evaluator into a single scalar field that drives the colour ramp,
/// roughness, and displacement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseLayer {
    pub kind: NoiseKind,
    /// Base spatial frequency (features per world unit).
    pub frequency: f32,
    /// Output amplitude (contribution weight).
    pub amplitude: f32,
    /// Number of fractal octaves summed.
    pub octaves: u8,
    /// Per-octave frequency multiplier (>1; typically ~2.0).
    pub lacunarity: f32,
    /// Per-octave amplitude multiplier (<1; typically ~0.5).
    pub gain: f32,
    /// Domain-warp strength applied before sampling (0 = none).
    pub warp: f32,
    /// Seed so two layers of the same kind differ.
    pub seed: u32,
}

impl Default for NoiseLayer {
    fn default() -> Self {
        // A neutral fbm layer — sensible base for hand-tuning.
        Self {
            kind: NoiseKind::Fbm,
            frequency: 1.0,
            amplitude: 1.0,
            octaves: 4,
            lacunarity: 2.0,
            gain: 0.5,
            warp: 0.0,
            seed: 0,
        }
    }
}

/// A colour gradient sampled by the composited noise value. Stops are `(t, rgba)`
/// with `t` in 0..1; [`ColorRamp::sample`] linearly interpolates between them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ColorRamp {
    /// Sorted-by-`t` colour stops. Each colour is linear RGBA in 0..1.
    pub stops: Vec<(f32, [f32; 4])>,
}

impl ColorRamp {
    /// Sample the ramp at `t` (clamped to 0..1), lerping between the surrounding
    /// stops. Returns transparent black if the ramp is empty, and the nearest stop
    /// past the ends.
    pub fn sample(&self, t: f32) -> [f32; 4] {
        if self.stops.is_empty() {
            return [0.0, 0.0, 0.0, 0.0];
        }
        let t = t.clamp(0.0, 1.0);
        // Before the first stop.
        if t <= self.stops[0].0 {
            return self.stops[0].1;
        }
        // Find the bracketing pair.
        for pair in self.stops.windows(2) {
            let (t0, c0) = pair[0];
            let (t1, c1) = pair[1];
            if t >= t0 && t <= t1 {
                let span = (t1 - t0).max(f32::EPSILON);
                let f = (t - t0) / span;
                return [
                    c0[0] + (c1[0] - c0[0]) * f,
                    c0[1] + (c1[1] - c0[1]) * f,
                    c0[2] + (c1[2] - c0[2]) * f,
                    c0[3] + (c1[3] - c0[3]) * f,
                ];
            }
        }
        // Past the last stop.
        self.stops[self.stops.len() - 1].1
    }
}

/// A procedural material: layered noise + a colour ramp + PBR surface parameters.
/// `arena-procgen` bakes this into texture maps; `arena-client` binds them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialDef {
    pub id: MaterialId,
    pub name: String,
    /// Noise layers composited (in order) to form the driving scalar field.
    pub layers: Vec<NoiseLayer>,
    /// Maps the composited field to base colour.
    pub ramp: ColorRamp,
    /// PBR roughness (0 = mirror, 1 = matte).
    pub roughness: f32,
    /// PBR metallic (0 = dielectric, 1 = metal).
    pub metallic: f32,
    /// Emissive strength (0 = none); multiplies `emissive_color`.
    pub emissive: f32,
    /// Emissive tint (linear RGB).
    pub emissive_color: [f32; 3],
    /// World-space scale for triplanar projection (avoids UV seams on organic mesh).
    pub triplanar_scale: f32,
    /// Vertex / parallax displacement amount driven by the noise field.
    pub displacement: f32,
    /// Optional custom surface shader; falls back to the standard shader if `None`.
    pub shader: Option<ShaderId>,
}

/// Where in the render pipeline a shader runs. Lets the client bind the right inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShaderStage {
    /// Per-surface fragment shading of world geometry.
    Surface,
    /// Full-screen post-process pass.
    Fullscreen,
    /// Sky / atmosphere dome.
    Sky,
    /// GPU particle shading.
    Particle,
    /// Animated water surface.
    Water,
}

/// A WGSL shader program. `source` is raw WGSL the client compiles; because it is
/// data in a [`crate::pack::ContentPack`], shaders are hot-recompiled on a swap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShaderDef {
    pub id: ShaderId,
    pub name: String,
    pub stage: ShaderStage,
    /// The WGSL source. Hot-recompiled by `arena-client` on a content swap.
    pub source: String,
    /// Named scalar uniforms exposed to the designer as `(name, default)`.
    pub params: Vec<(String, f32)>,
}
