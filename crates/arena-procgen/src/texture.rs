//! Procedural texture synthesis from a [`MaterialDef`].
//!
//! Cerena bakes no authored textures. A material is a stack of noise layers plus a
//! colour ramp and PBR parameters; here we evaluate that recipe over UV space into a
//! pixel buffer the client uploads as a GPU texture. Because materials are pure data
//! in a content pack, they are hot-reloadable: swapping a material's definition simply
//! re-runs these functions and re-uploads the result.
//!
//! We produce two maps:
//! * [`synth_material_texture`] — base albedo (RGBA), the noise field coloured by the
//!   ramp, with emissive folded in.
//! * [`synth_normal_map`] — a tangent-space normal map from the height (noise)
//!   gradient, for surface detail.
//!
//! Roughness and metallic are single scalars on the material, bound by the client as
//! uniforms, so we do not bake per-texel roughness here. Triplanar projection also
//! happens in the shader (the material carries `triplanar_scale`); these maps are the
//! base inputs to it.

use arena_content::material::MaterialDef;
use glam::Vec3;

use crate::noise_eval::sample_stack;

/// A baked texture: tightly-packed RGBA8 pixels, row-major, `width * height * 4` bytes.
#[derive(Debug, Clone)]
pub struct TextureData {
    pub width: u32,
    pub height: u32,
    /// Linear RGBA8, length `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// Convert a UV pixel index plus a seed into the sample point fed to the noise stack.
/// The `seed` simply offsets along a third axis so two instances of the same material
/// look different without changing the recipe.
fn sample_point(u: f32, v: f32, seed: u32) -> Vec3 {
    // UV is in [0,1]; each layer's own `frequency` controls how many features appear.
    Vec3::new(u, v, (seed as f32) * 0.137)
}

#[inline]
fn to_u8(x: f32) -> u8 {
    (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Bake the base albedo texture for `mat` at `size x size`. Each texel evaluates the
/// material's composited noise field, remaps it to `0..1`, samples the colour ramp,
/// and folds in the emissive contribution.
pub fn synth_material_texture(mat: &MaterialDef, size: u32, seed: u32) -> TextureData {
    let size = size.max(1);
    let mut rgba = vec![0u8; (size as usize) * (size as usize) * 4];

    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / size as f32;
            let v = y as f32 / size as f32;
            let p = sample_point(u, v, seed);

            // The composited field is roughly [-1, 1]; remap to the ramp's [0, 1].
            let field = sample_stack(&mat.layers, p);
            let t = (field * 0.5 + 0.5).clamp(0.0, 1.0);
            let base = mat.ramp.sample(t);

            // Emissive adds light-emitting tint on top of the albedo (clamped on write).
            let er = base[0] + mat.emissive_color[0] * mat.emissive;
            let eg = base[1] + mat.emissive_color[1] * mat.emissive;
            let eb = base[2] + mat.emissive_color[2] * mat.emissive;

            let o = ((y * size + x) * 4) as usize;
            rgba[o] = to_u8(er);
            rgba[o + 1] = to_u8(eg);
            rgba[o + 2] = to_u8(eb);
            rgba[o + 3] = to_u8(base[3]);
        }
    }

    TextureData { width: size, height: size, rgba }
}

/// Bake a tangent-space normal map for `mat` from the height field's gradient. Height
/// is the same composited noise used for albedo; its slope in UV becomes the surface
/// normal. `mat.displacement` scales how pronounced the bumps are.
pub fn synth_normal_map(mat: &MaterialDef, size: u32, seed: u32) -> TextureData {
    let size = size.max(1);
    let mut rgba = vec![0u8; (size as usize) * (size as usize) * 4];

    // Finite-difference step of one texel in UV space.
    let step = 1.0 / size as f32;
    // Avoid a zero-height flat map when displacement is unset.
    let scale = if mat.displacement > 0.0 { mat.displacement } else { 1.0 };

    let height = |u: f32, v: f32| -> f32 { sample_stack(&mat.layers, sample_point(u, v, seed)) };

    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / size as f32;
            let v = y as f32 / size as f32;

            // Central differences give the slope of the height field.
            let dhdu = (height(u + step, v) - height(u - step, v)) * 0.5;
            let dhdv = (height(u, v + step) - height(u, v - step)) * 0.5;

            // Surface normal of a height field h(u,v): (-dh/du, -dh/dv, 1), scaled.
            let n = Vec3::new(-dhdu * scale, -dhdv * scale, 1.0).normalize_or_zero();

            // Encode [-1,1] -> [0,1] -> bytes (the conventional normal-map packing).
            let o = ((y * size + x) * 4) as usize;
            rgba[o] = to_u8(n.x * 0.5 + 0.5);
            rgba[o + 1] = to_u8(n.y * 0.5 + 0.5);
            rgba[o + 2] = to_u8(n.z * 0.5 + 0.5);
            rgba[o + 3] = 255;
        }
    }

    TextureData { width: size, height: size, rgba }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::ids::MaterialId;
    use arena_content::material::{ColorRamp, NoiseLayer};

    fn test_material() -> MaterialDef {
        MaterialDef {
            id: MaterialId::new("material.test"),
            name: "test".into(),
            layers: vec![NoiseLayer::default()],
            ramp: ColorRamp {
                stops: vec![(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])],
            },
            roughness: 0.5,
            metallic: 0.0,
            emissive: 0.0,
            emissive_color: [0.0, 0.0, 0.0],
            triplanar_scale: 1.0,
            displacement: 0.1,
            shader: None,
        }
    }

    #[test]
    fn albedo_has_expected_byte_count() {
        let tex = synth_material_texture(&test_material(), 16, 7);
        assert_eq!(tex.width, 16);
        assert_eq!(tex.rgba.len(), 16 * 16 * 4);
    }

    #[test]
    fn normal_map_has_expected_byte_count() {
        let tex = synth_normal_map(&test_material(), 16, 7);
        assert_eq!(tex.rgba.len(), 16 * 16 * 4);
    }
}
