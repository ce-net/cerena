//! CPU particle system for spell VFX — organic, additive, glowy.
//!
//! Spells in Cerena are content-defined, and so are their visual signatures. The
//! authoritative sim emits discrete [`GameEvent`]s (a shot left a muzzle, a bolt
//! hit a body, a fireball detonated); this module turns those events into bursts of
//! particles. Today it is a plain CPU buffer of [`Particle`] instances integrated
//! each frame and (eventually) uploaded for an additive, depth-read/no-write pass
//! so overlapping embers and motes bloom into soft glows rather than hard sprites.
//!
//! The emitter *shapes* (cone, sphere, trail; colour ramps; lifetimes) will come
//! from `arena-procgen`'s `EmitterDesc` so a designer tunes spell looks as data,
//! exactly like materials and shaders. For now the spawn shapes are hand-coded per
//! event kind and clearly marked as the seam.

use glam::Vec3;

use arena_content::worldgen::WorldGenParams;
use arena_protocol::snapshot::GameEvent;

/// Hard cap on live particles, so a frantic teamfight cannot unbound the buffer.
pub const MAX_PARTICLES: usize = 8192;

/// One live particle. Kept `Copy` and array-of-structs for a trivial later upload
/// into an instance buffer (position + colour + size per particle).
#[derive(Debug, Clone, Copy)]
pub struct Particle {
    pub pos: Vec3,
    pub vel: Vec3,
    /// Linear RGB; alpha is derived from remaining life for a clean fade-out.
    pub color: [f32; 3],
    /// World-space radius in metres.
    pub size: f32,
    /// Remaining life, seconds. Dead at <= 0.
    pub life: f32,
    /// Total life it spawned with, for normalised fade curves.
    pub max_life: f32,
    /// Constant downward acceleration (embers fall, sparks arc). 0 = drifting motes.
    pub gravity: f32,
}

impl Particle {
    /// Current alpha for additive blending: fades linearly with remaining life.
    pub fn alpha(&self) -> f32 {
        (self.life / self.max_life.max(1e-3)).clamp(0.0, 1.0)
    }
}

/// The live particle buffer plus spawn/integrate logic.
#[derive(Default)]
pub struct ParticleSystem {
    particles: Vec<Particle>,
    /// The world's terrain recipe, so particles collide with (settle on) the ground
    /// using the *same* `surface_height` the sim and render mesh use. `None` = no
    /// ground (particles just fall away, e.g. the flat native test arena).
    terrain: Option<WorldGenParams>,
}

impl ParticleSystem {
    pub fn new() -> Self {
        Self {
            particles: Vec::with_capacity(1024),
            terrain: None,
        }
    }

    /// Give the system the world's terrain so falling particles collide with the
    /// ground instead of sinking through it. Uses the shared procgen heightfield.
    pub fn set_terrain(&mut self, params: WorldGenParams) {
        self.terrain = Some(params);
    }

    /// Live particle count (HUD/diagnostics).
    pub fn len(&self) -> usize {
        self.particles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.particles.is_empty()
    }

    /// Advance every particle by `dt` seconds and reap the dead ones. Cheap Euler
    /// integration — particles are cosmetic, never simulated authoritatively.
    pub fn update(&mut self, dt: f32) {
        let terrain = self.terrain.as_ref();
        for p in &mut self.particles {
            p.vel.y -= p.gravity * dt;
            p.pos += p.vel * dt;
            p.life -= dt;

            // Collide with the ground: settle on the surface (the same heightfield the
            // sim collides against and the terrain mesh is built from) instead of
            // falling through it. Sparks/embers skid to a stop where they land.
            if let Some(params) = terrain {
                let ground = arena_procgen::world::surface_height(params, p.pos.x, p.pos.z);
                if p.pos.y < ground {
                    p.pos.y = ground;
                    p.vel.y = 0.0;
                    p.vel.x *= 0.4; // friction skid, then the fade-out finishes it
                    p.vel.z *= 0.4;
                }
            }
        }
        self.particles.retain(|p| p.life > 0.0);
    }

    /// Map a [`GameEvent`] to a particle burst. This is the spell-to-VFX seam: each
    /// event kind seeds a characteristic spawn. Deterministic spread is not required
    /// (these are cosmetic), so a simple hashed jitter keeps it allocation-free and
    /// reproducible enough without an RNG dependency.
    pub fn spawn_from_event(&mut self, event: &GameEvent) {
        match event {
            // A cast leaving the staff: a tight forward cone of bright motes.
            GameEvent::Shot { origin, dir, .. } => {
                self.burst_cone(*origin, *dir, 16, 6.0, [1.0, 0.85, 0.4], 0.05, 0.35);
            }
            // A confirmed hit: a small radial spark spray at the impact point.
            GameEvent::Hit { point, headshot, .. } => {
                let color = if *headshot {
                    [1.0, 0.3, 0.3]
                } else {
                    [1.0, 0.7, 0.5]
                };
                self.burst_sphere(*point, 24, 4.0, color, 0.04, 0.4);
            }
            // A detonation: a dense, fast, expanding shell scaled to the blast radius.
            GameEvent::Explosion { center, radius } => {
                let count = (32.0 * radius).min(256.0) as usize;
                self.burst_sphere(*center, count, radius * 3.0, [1.0, 0.55, 0.2], 0.12, 0.7);
            }
            // A (re)spawn flourish: a gentle upward bloom in the team-agnostic tint.
            GameEvent::Spawn { pos, .. } => {
                self.burst_cone(*pos, Vec3::Y, 20, 2.0, [0.5, 0.8, 1.0], 0.08, 0.6);
            }
            // A melee swing: a thin, fast arc of sparks thrown along the blade path.
            // (Melee is the one feedback event that carries a world origin; the
            // entity-targeted feedback events — Knockback / Heal / Buff — have no wire
            // position, so their *visual* read is handled by the camera-feel layer in
            // `crate::feedback` rather than spawned here at a bogus origin.)
            GameEvent::Melee { origin, dir, victim, .. } => {
                let n = if victim.is_some() { 22 } else { 12 };
                let color = if victim.is_some() { [1.0, 0.85, 0.6] } else { [0.85, 0.9, 1.0] };
                self.burst_cone(*origin + *dir, *dir, n, 9.0, color, 0.04, 0.18);
            }
            // Deaths/pickups/chat/shake + entity-targeted feedback carry no positional
            // VFX here (kill-feed, camera shake and flashes own those reads).
            _ => {}
        }
    }

    /// Spawn `count` particles in a cone about `dir` from `origin`.
    fn burst_cone(
        &mut self,
        origin: Vec3,
        dir: Vec3,
        count: usize,
        speed: f32,
        color: [f32; 3],
        size: f32,
        life: f32,
    ) {
        let dir = dir.normalize_or_zero();
        for i in 0..count {
            if self.particles.len() >= MAX_PARTICLES {
                break;
            }
            let j = jitter3(origin, i as u32) * 0.4;
            let vel = (dir + j).normalize_or_zero() * speed;
            self.push(origin, vel, color, size, life, 1.0);
        }
    }

    /// Spawn `count` particles radiating from `center`.
    fn burst_sphere(
        &mut self,
        center: Vec3,
        count: usize,
        speed: f32,
        color: [f32; 3],
        size: f32,
        life: f32,
    ) {
        for i in 0..count {
            if self.particles.len() >= MAX_PARTICLES {
                break;
            }
            let vel = jitter3(center, i as u32).normalize_or_zero() * speed;
            self.push(center, vel, color, size, life, 2.0);
        }
    }

    fn push(
        &mut self,
        pos: Vec3,
        vel: Vec3,
        color: [f32; 3],
        size: f32,
        life: f32,
        gravity: f32,
    ) {
        self.particles.push(Particle {
            pos,
            vel,
            color,
            size,
            life,
            max_life: life,
            gravity,
        });
    }

    /// Read-only view of live particles, for the (TODO) additive upload in the
    /// renderer's VFX pass.
    pub fn particles(&self) -> &[Particle] {
        &self.particles
    }
}

/// A cheap, deterministic pseudo-random unit-ish vector from a seed point + index.
/// Hash-based so it needs no RNG state and is reproducible per event; good enough
/// for cosmetic jitter.
fn jitter3(seed: Vec3, i: u32) -> Vec3 {
    fn h(mut x: u32) -> f32 {
        // A small integer hash -> [-1, 1].
        x ^= x >> 16;
        x = x.wrapping_mul(0x7feb352d);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846ca68b);
        x ^= x >> 16;
        (x as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
    let base = seed.x.to_bits() ^ seed.y.to_bits().rotate_left(11) ^ seed.z.to_bits().rotate_left(21);
    Vec3::new(
        h(base ^ i.wrapping_mul(2654435761)),
        h(base ^ i.wrapping_mul(40503)),
        h(base ^ i.wrapping_mul(73856093)),
    )
}
