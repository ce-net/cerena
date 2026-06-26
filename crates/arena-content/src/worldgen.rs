//! World generation parameters — the seed-driven recipe for the procedural world.
//!
//! Cerena's world is generated, not authored: a deterministic seed plus layered
//! noise yields continents, mountains, caves, and biomes. [`WorldGenParams`] is the
//! full recipe `arena-procgen` consumes. Because it is data, the world's character
//! (more islands, deeper caves, new biomes) is a hot-reloadable tweak — and the same
//! params on authority and client guarantee identical terrain.

use serde::{Deserialize, Serialize};

use crate::ids::{MaterialId, MobId};
use crate::material::{ColorRamp, NoiseKind, NoiseLayer};

/// A biome: a height band with its surface look, fog, and creature spawns. The
/// generator assigns a biome per region from the biome-selection noise, then bands by
/// terrain height.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BiomeDef {
    pub name: String,
    /// Lowest normalised terrain height (0..1) this biome covers.
    pub height_min: f32,
    /// Highest normalised terrain height (0..1) this biome covers.
    pub height_max: f32,
    /// Material painted on the ground surface.
    pub surface_material: Option<MaterialId>,
    /// Distance-fog tint (linear RGB) that sets the biome's mood.
    pub fog_color: [f32; 3],
    /// Creatures that spawn here as `(mob, relative weight)`.
    pub mob_spawns: Vec<(MobId, f32)>,
}

/// A scattered structure / point-of-interest (ruins, monoliths, nests). The
/// generator places these by Poisson-ish sampling at `frequency`, seeding each
/// instance's procedural mesh with `mesh_seed`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructureDef {
    pub name: String,
    /// Placement density (instances per unit area, roughly).
    pub frequency: f32,
    /// Optional creature bound to the structure (a guardian, a nest spawner).
    pub mob_id: Option<MobId>,
    /// World-space scale of the structure.
    pub scale: f32,
    /// Seed for the structure's procedural organic mesh.
    pub mesh_seed: u32,
}

/// The complete world-generation recipe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldGenParams {
    /// Master seed; everything derives deterministically from this.
    pub seed: u64,
    /// World units per noise unit — bigger = larger continents.
    pub world_scale: f32,
    /// Low-frequency layer carving continents from ocean.
    pub continent: NoiseLayer,
    /// Mid/high-frequency ridged layer raising mountain ranges.
    pub mountains: NoiseLayer,
    /// 3D layer carving cave systems; cells above `cave_threshold` become air.
    pub caves: NoiseLayer,
    /// Threshold (0..1) above which the cave field hollows out rock.
    pub cave_threshold: f32,
    /// Frequency of the biome-selection field.
    pub biome_freq: f32,
    /// Normalised sea level (0..1); terrain below is underwater.
    pub sea_level: f32,
    /// Biomes the world can express.
    pub biomes: Vec<BiomeDef>,
    /// Scattered structures / points of interest.
    pub structures: Vec<StructureDef>,
}

impl Default for WorldGenParams {
    /// An organic mystery world: rolling continents, ridged peaks, winding caves, a
    /// verdant lowland biome and a high-crystal highland biome, dotted with ruins.
    fn default() -> Self {
        Self {
            seed: 0xCE5E_4A00_1234_5678,
            world_scale: 2048.0,
            continent: NoiseLayer {
                kind: NoiseKind::Fbm,
                frequency: 0.6,
                amplitude: 1.0,
                octaves: 5,
                lacunarity: 2.0,
                gain: 0.5,
                warp: 0.35,
                seed: 1,
            },
            mountains: NoiseLayer {
                kind: NoiseKind::Ridged,
                frequency: 1.8,
                amplitude: 0.7,
                octaves: 6,
                lacunarity: 2.1,
                gain: 0.55,
                warp: 0.2,
                seed: 2,
            },
            caves: NoiseLayer {
                kind: NoiseKind::Worley,
                frequency: 3.2,
                amplitude: 1.0,
                octaves: 3,
                lacunarity: 2.0,
                gain: 0.5,
                warp: 0.6,
                seed: 3,
            },
            cave_threshold: 0.62,
            biome_freq: 0.4,
            sea_level: 0.42,
            biomes: vec![
                BiomeDef {
                    name: "Verdant Lowlands".into(),
                    height_min: 0.42,
                    height_max: 0.7,
                    surface_material: Some(MaterialId::new("material.mystic_grass")),
                    fog_color: [0.55, 0.72, 0.6],
                    mob_spawns: vec![
                        (MobId::new("mob.forest_guardian"), 1.0),
                        (MobId::new("mob.wisp"), 2.0),
                    ],
                },
                BiomeDef {
                    name: "Crystal Highlands".into(),
                    height_min: 0.7,
                    height_max: 1.0,
                    surface_material: Some(MaterialId::new("material.crystal")),
                    fog_color: [0.6, 0.62, 0.85],
                    mob_spawns: vec![(MobId::new("mob.crystal_golem"), 1.0)],
                },
                BiomeDef {
                    name: "Void Hollows".into(),
                    height_min: 0.0,
                    height_max: 0.42,
                    surface_material: Some(MaterialId::new("material.void_fog")),
                    fog_color: [0.12, 0.08, 0.18],
                    mob_spawns: vec![(MobId::new("mob.void_wraith"), 1.5)],
                },
            ],
            structures: vec![
                StructureDef {
                    name: "Sunken Monolith".into(),
                    frequency: 0.015,
                    mob_id: Some(MobId::new("mob.crystal_golem")),
                    scale: 6.0,
                    mesh_seed: 0x5701,
                },
                StructureDef {
                    name: "Whispering Ruins".into(),
                    frequency: 0.03,
                    mob_id: Some(MobId::new("mob.void_wraith")),
                    scale: 3.5,
                    mesh_seed: 0x5702,
                },
            ],
        }
    }
}
