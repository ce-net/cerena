//! World geometry, the zone grid, and area-of-interest math.
//!
//! Scaling to thousands of players means *nobody simulates the whole world*. The
//! map is partitioned into a regular grid of [`ZoneId`] cells. Each cell is owned
//! by exactly one authoritative node at a time (see `arena-mesh` for assignment).
//! A player only ever receives state for entities inside their area of interest
//! (AOI), which usually spans their own zone plus the eight neighbours.

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Edge length of a zone cell, in metres. 128 m comfortably holds a dense
/// firefight; an authority simulates one cell plus a thin border overlap.
pub const ZONE_SIZE_M: f32 = 128.0;

/// Border overlap (metres) shared with neighbouring zones. Entities within the
/// border are mirrored read-only into the neighbour so cross-zone hitscan and
/// hand-off are seamless.
pub const ZONE_BORDER_M: f32 = 16.0;

/// World vertical bounds (metres). Maps are wide and shallow.
pub const WORLD_FLOOR_M: f32 = -64.0;
pub const WORLD_CEIL_M: f32 = 256.0;

/// Radius (in zones) a player can see/hear. 1 = own zone + 8 neighbours.
pub const AOI_ZONE_RADIUS: i32 = 1;

/// A zone cell coordinate on the horizontal grid. Vertical is not partitioned —
/// FPS maps are effectively 2.5D for partitioning purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ZoneId {
    pub x: i32,
    pub z: i32,
}

impl ZoneId {
    pub const fn new(x: i32, z: i32) -> Self {
        Self { x, z }
    }

    /// Which zone a world position falls into.
    pub fn from_world(pos: Vec3) -> Self {
        Self {
            x: (pos.x / ZONE_SIZE_M).floor() as i32,
            z: (pos.z / ZONE_SIZE_M).floor() as i32,
        }
    }

    /// Centre of this zone in world space (y = 0 plane).
    pub fn center(self) -> Vec3 {
        Vec3::new(
            (self.x as f32 + 0.5) * ZONE_SIZE_M,
            0.0,
            (self.z as f32 + 0.5) * ZONE_SIZE_M,
        )
    }

    /// Chebyshev distance in zone cells.
    pub fn cheb_distance(self, other: ZoneId) -> i32 {
        (self.x - other.x).abs().max((self.z - other.z).abs())
    }

    /// The 3x3 (by default) block of zones in this player's AOI, this cell first.
    pub fn aoi(self, radius: i32) -> Vec<ZoneId> {
        let mut out = Vec::with_capacity(((2 * radius + 1) * (2 * radius + 1)) as usize);
        out.push(self);
        for dz in -radius..=radius {
            for dx in -radius..=radius {
                if dx == 0 && dz == 0 {
                    continue;
                }
                out.push(ZoneId::new(self.x + dx, self.z + dz));
            }
        }
        out
    }

    /// Stable token used in mesh topics, e.g. `ce-game/arena/<map>/12_-3/state`.
    pub fn token(self) -> String {
        format!("{}_{}", self.x, self.z)
    }

    /// Parse a token produced by [`ZoneId::token`].
    pub fn parse_token(s: &str) -> Option<Self> {
        let (x, z) = s.split_once('_')?;
        Some(Self {
            x: x.parse().ok()?,
            z: z.parse().ok()?,
        })
    }

    /// True if `pos` lies within this zone *including* the shared border overlap.
    /// The authority simulates everything here; the inner region is exclusively
    /// owned, the border is mirrored from/to neighbours.
    pub fn contains_with_border(self, pos: Vec3) -> bool {
        let min_x = self.x as f32 * ZONE_SIZE_M - ZONE_BORDER_M;
        let max_x = (self.x + 1) as f32 * ZONE_SIZE_M + ZONE_BORDER_M;
        let min_z = self.z as f32 * ZONE_SIZE_M - ZONE_BORDER_M;
        let max_z = (self.z + 1) as f32 * ZONE_SIZE_M + ZONE_BORDER_M;
        pos.x >= min_x && pos.x < max_x && pos.z >= min_z && pos.z < max_z
    }

    /// True if `pos` is in the exclusively-owned interior (no border).
    pub fn contains_interior(self, pos: Vec3) -> bool {
        Self::from_world(pos) == self
    }
}

/// Identifies a map/arena. Maps are content-addressed: the id is the hex of the
/// blob hash of the compiled map, so every node agrees on identical geometry.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MapId(pub String);

impl MapId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An axis-aligned bounding box, the workhorse collider for static geometry and
/// player capsules' broad phase.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    pub fn from_center_half(center: Vec3, half: Vec3) -> Self {
        Self {
            min: center - half,
            max: center + half,
        }
    }

    pub fn center(&self) -> Vec3 {
        (self.min + self.max) * 0.5
    }

    pub fn half_extents(&self) -> Vec3 {
        (self.max - self.min) * 0.5
    }

    pub fn intersects(&self, other: &Aabb) -> bool {
        self.min.x <= other.max.x
            && self.max.x >= other.min.x
            && self.min.y <= other.max.y
            && self.max.y >= other.min.y
            && self.min.z <= other.max.z
            && self.max.z >= other.min.z
    }

    pub fn contains_point(&self, p: Vec3) -> bool {
        p.x >= self.min.x
            && p.x <= self.max.x
            && p.y >= self.min.y
            && p.y <= self.max.y
            && p.z >= self.min.z
            && p.z <= self.max.z
    }

    /// Expand to include another box.
    pub fn union(&self, other: &Aabb) -> Aabb {
        Aabb {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

/// A team affiliation. Free-for-all uses [`Team::None`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Team {
    None = 0,
    Red = 1,
    Blue = 2,
}

/// A spawn point baked into the map.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SpawnPoint {
    pub pos: Vec3,
    pub yaw: f32,
    pub team: Team,
}
