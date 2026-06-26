//! Cerena procedural generation — organic shapes and textures, grown from a seed.
//!
//! Everything you see in Cerena is *generated*, never authored by hand: terrain,
//! creatures, spell effects, and surface textures are all synthesised from layered
//! noise and signed-distance fields. This crate is the generator.
//!
//! ## The organic-SDF approach
//!
//! The art direction is "high detail and organic, non-sharp shapes — a procedural
//! open magical mystery world". We get that look from two techniques working
//! together:
//!
//! 1. **Signed-distance fields blended with smooth-min.** Shapes are built from
//!    SDF primitives (spheres, capsules, rounded boxes) combined with *smooth*
//!    boolean operators ([`sdf::smin`] / [`sdf::op_union_smooth`]). A hard union
//!    leaves a sharp crease where two shapes meet; a smooth-min melts them into one
//!    another, so a body grown from a handful of capsules reads as a single organic
//!    creature rather than a pile of geometry. Smooth-min is *why* nothing in
//!    Cerena has hard seams.
//!
//! 2. **Layered, domain-warped noise.** High-frequency detail comes from stacks of
//!    fractal noise ([`noise_eval`]). Domain warping (perturbing a sample's
//!    coordinates with another noise field) gives the melted, marbled, hand-grown
//!    surfaces that make the world feel alive rather than tiled.
//!
//! Surfaces are then extracted from those fields with [`mesh::surface_nets`] (a dual
//! method that yields smooth, watertight-ish meshes) and lit using normals taken
//! from the field gradient, which keeps curvature continuous.
//!
//! ## Deterministic by seed
//!
//! Cerena is a 10,000-player distributed game: many nodes and clients must agree on
//! the exact same world without shipping geometry between them. Every generator here
//! is a pure function of its inputs and seeds. The `noise` crate's generators are
//! seeded (`Perlin::new(seed)` etc.), and our placement RNG is a deterministic
//! splitmix64 ([`noise_eval::Rng`]). Feed two machines the same
//! [`arena_content::worldgen::WorldGenParams`] and zone and they extract identical
//! geometry — the server can compute collision while clients compute the visible
//! mesh, and they will line up.
//!
//! ## Data, not GPU calls
//!
//! This crate is pure and wasm-clean: no `wgpu`, no `tokio`, no I/O. It produces
//! plain data — [`mesh::Mesh`] vertex buffers, [`texture::TextureData`] pixel
//! buffers, and [`arena_protocol::world::Aabb`] collision volumes. `arena-client`
//! uploads the meshes and textures to the GPU; `arena-server` uses the collision
//! boxes. Because everything is data driven, materials and world params are
//! hot-reloadable: a content swap simply re-runs the relevant generator.

pub mod creature;
pub mod mesh;
pub mod noise_eval;
pub mod sdf;
pub mod texture;
pub mod vfx;
pub mod world;

// Convenience re-exports of the headline data types other crates consume.
pub use mesh::Mesh;
pub use noise_eval::Rng;
pub use texture::TextureData;
pub use vfx::EmitterDesc;
