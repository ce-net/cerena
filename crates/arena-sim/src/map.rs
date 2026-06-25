//! Static map geometry and spawn placement.
//!
//! A [`MapDef`] is the immutable collision world for one arena: an axis-aligned
//! bound, a list of solid brushes (the floor, walls and cover are all just
//! [`Aabb`]s — cheap to raycast and to sweep a capsule against), and the spawn
//! points. Maps are content-addressed (`MapId` is the hash of the compiled map),
//! so every node that loads the same id agrees byte-for-byte on the geometry.

use glam::Vec3;

use arena_protocol::world::{Aabb, MapId, SpawnPoint, Team, WORLD_CEIL_M, WORLD_FLOOR_M};

/// The compiled, immutable definition of one arena.
#[derive(Debug, Clone)]
pub struct MapDef {
    /// Content-addressed id; identical geometry => identical id everywhere.
    pub id: MapId,
    /// The outer playable bound. Anything outside is a hard kill / clamp region.
    pub bounds: Aabb,
    /// Solid static geometry: floor, walls, crates, ramps. The collision and
    /// raycast routines treat every entry uniformly.
    pub brushes: Vec<Aabb>,
    /// Baked spawn points, tagged by team.
    pub spawns: Vec<SpawnPoint>,
}

impl MapDef {
    /// Build the canonical test arena: a sealed box with a flat floor at `y = 0`,
    /// four perimeter walls, a handful of cover crates and ramps, and 16 spawn
    /// points split 8/8 across the two teams at opposite ends.
    ///
    /// The arena spans roughly `[-32, 32]` on X and Z so it lives inside a single
    /// zone cell, which keeps the unit tests free of zone hand-off concerns.
    pub fn test_arena() -> MapDef {
        let mut brushes = Vec::new();

        // Flat floor: a large, thin slab whose top face sits exactly at y = 0 so a
        // player standing on it has feet at 0. We give it real thickness so a
        // downward raycast / capsule sweep always finds a surface to rest on.
        brushes.push(Aabb::new(
            Vec3::new(-32.0, -1.0, -32.0),
            Vec3::new(32.0, 0.0, 32.0),
        ));

        // Four perimeter walls, 8 m tall, 1 m thick, hugging the floor edges.
        let wall_h = 8.0;
        // North (+Z) and South (-Z).
        brushes.push(Aabb::new(
            Vec3::new(-32.0, 0.0, 31.0),
            Vec3::new(32.0, wall_h, 32.0),
        ));
        brushes.push(Aabb::new(
            Vec3::new(-32.0, 0.0, -32.0),
            Vec3::new(32.0, wall_h, -31.0),
        ));
        // East (+X) and West (-X).
        brushes.push(Aabb::new(
            Vec3::new(31.0, 0.0, -32.0),
            Vec3::new(32.0, wall_h, 32.0),
        ));
        brushes.push(Aabb::new(
            Vec3::new(-32.0, 0.0, -32.0),
            Vec3::new(-31.0, wall_h, 32.0),
        ));

        // Cover crates: 2 m cubes scattered near the centre so firefights have
        // line-of-sight breaks (essential for a believable hitscan test bed).
        for (cx, cz) in [(-6.0, 0.0), (6.0, 0.0), (0.0, -8.0), (0.0, 8.0)] {
            brushes.push(Aabb::from_center_half(
                Vec3::new(cx, 1.0, cz),
                Vec3::new(1.0, 1.0, 1.0),
            ));
        }

        // Two ramps approximated as a short stack of stepped blocks. A swept
        // capsule walks up these as a staircase; good enough for an arena and far
        // simpler (and more deterministic) than arbitrary triangle slopes.
        for i in 0..4 {
            let y = i as f32 * 0.5;
            // East ramp climbing toward +X.
            brushes.push(Aabb::new(
                Vec3::new(12.0 + i as f32, 0.0, -3.0),
                Vec3::new(13.0 + i as f32, y + 0.5, 3.0),
            ));
            // West ramp climbing toward -X.
            brushes.push(Aabb::new(
                Vec3::new(-13.0 - i as f32, 0.0, -3.0),
                Vec3::new(-12.0 - i as f32, y + 0.5, 3.0),
            ));
        }

        // 16 spawns: red along the -Z edge, blue along the +Z edge, facing centre.
        let mut spawns = Vec::with_capacity(16);
        let xs = [-21.0, -15.0, -9.0, -3.0, 3.0, 9.0, 15.0, 21.0];
        for &x in &xs {
            spawns.push(SpawnPoint {
                pos: Vec3::new(x, 0.0, -24.0),
                yaw: 0.0, // yaw 0 looks toward -Z; red faces away from its wall, into the map.
                team: Team::Red,
            });
            spawns.push(SpawnPoint {
                pos: Vec3::new(x, 0.0, 24.0),
                yaw: std::f32::consts::PI, // blue faces -... toward -Z is PI from +Z; faces into the map.
                team: Team::Blue,
            });
        }

        MapDef {
            id: MapId("test_arena_v1".to_string()),
            bounds: Aabb::new(
                Vec3::new(-32.0, WORLD_FLOOR_M, -32.0),
                Vec3::new(32.0, WORLD_CEIL_M, 32.0),
            ),
            brushes,
            spawns,
        }
    }

    /// Pick the spawn for `team` that is *furthest from the nearest enemy*, to
    /// avoid dropping a player into an enemy's sights (the classic spawn-kill).
    ///
    /// `enemy_positions` are the current world positions of hostile players. With
    /// no enemies we fall back to a stable `rng_seed`-indexed pick so two players
    /// spawning the same tick don't stack. The choice is fully deterministic.
    pub fn pick_spawn(&self, team: Team, enemy_positions: &[Vec3], rng_seed: u64) -> SpawnPoint {
        // Candidate spawns: matching team, plus neutral spawns, plus everything in
        // free-for-all (Team::None). Falls back to all spawns if a team has none.
        let candidates: Vec<&SpawnPoint> = self
            .spawns
            .iter()
            .filter(|s| team == Team::None || s.team == team || s.team == Team::None)
            .collect();
        let pool: &[&SpawnPoint] = if candidates.is_empty() {
            // Degenerate map with no team spawns: use everything.
            return self.spawns[(rng_seed as usize) % self.spawns.len().max(1)];
        } else {
            &candidates
        };

        if enemy_positions.is_empty() {
            // No threats: round-robin by seed so simultaneous spawns spread out.
            return *pool[(rng_seed as usize) % pool.len()];
        }

        // Score each candidate by squared distance to its nearest enemy; bigger is
        // safer. Squared distance avoids a sqrt and preserves ordering.
        let mut best = pool[0];
        let mut best_score = f32::NEG_INFINITY;
        for &s in pool {
            let nearest = enemy_positions
                .iter()
                .map(|e| s.pos.distance_squared(*e))
                .fold(f32::INFINITY, f32::min);
            // Tie-break deterministically with the seed so identical scores don't
            // always favour the first spawn (would funnel everyone to one corner).
            let jitter = ((rng_seed.wrapping_mul(2654435761) ^ s.pos.x.to_bits() as u64) & 0xff)
                as f32
                * 1e-3;
            let score = nearest + jitter;
            if score > best_score {
                best_score = score;
                best = s;
            }
        }
        *best
    }
}
