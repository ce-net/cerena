//! Replicated entity state and the delta model.
//!
//! An [`EntityState`] is the full per-tick description of one networked thing
//! (player, projectile, pickup). Snapshots delta-encode these against a baseline
//! to keep bandwidth flat as player counts climb.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::{EntityId, NodeId, world::Team};

/// What kind of thing an entity is. Drives client rendering and sim handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum EntityKind {
    Player = 0,
    Projectile = 1,
    Pickup = 2,
    /// A read-only mirror of an entity owned by a neighbouring zone authority.
    /// Clients render it; only the owning authority simulates it.
    ZoneMirror = 3,
}

/// Movement / pose flags, packed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityFlags(pub u16);

impl EntityFlags {
    pub const CROUCHING: u16 = 1 << 0;
    pub const AIRBORNE: u16 = 1 << 1;
    pub const SPRINTING: u16 = 1 << 2;
    pub const FIRING: u16 = 1 << 3;
    pub const RELOADING: u16 = 1 << 4;
    pub const DEAD: u16 = 1 << 5;
    pub const ON_GROUND: u16 = 1 << 6;
    /// Flying under a flight movement mode (drives the client's pose + trail VFX).
    pub const FLYING: u16 = 1 << 7;
    /// Clinging to / climbing a sheer surface.
    pub const CLIMBING: u16 = 1 << 8;
    /// Running along a wall (reduced-gravity parkour state).
    pub const WALLRUNNING: u16 = 1 << 9;
    /// Mid melee swing (drives the first-person weapon arc + remote swing pose).
    pub const MELEEING: u16 = 1 << 10;

    pub fn has(self, f: u16) -> bool {
        self.0 & f != 0
    }
    pub fn set(&mut self, f: u16, on: bool) {
        if on {
            self.0 |= f;
        } else {
            self.0 &= !f;
        }
    }
}

/// A globally-unique entity handle: the owning zone authority's node id plus a
/// per-authority counter. Survives zone hand-off so clients keep a stable id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GlobalId {
    pub authority: NodeId,
    pub local: u64,
}

/// The full replicated state of one entity at one tick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityState {
    pub id: EntityId,
    pub kind: EntityKind,
    pub pos: Vec3,
    pub vel: Vec3,
    /// View yaw (radians). Players only; others use 0.
    pub yaw: f32,
    /// View pitch (radians). Players only.
    pub pitch: f32,
    pub flags: EntityFlags,
    pub team: Team,
    /// 0..=100 typically; armor is separate so clients can show both.
    pub health: i16,
    pub armor: i16,
    /// Active weapon slot (players); projectile owner-weapon id otherwise.
    pub weapon: u8,
    /// For players: the CE node id of the controlling client (rendered as name
    /// tag, used by reporting). Empty for non-player entities.
    #[serde(default)]
    pub owner: NodeId,
}

impl EntityState {
    /// Full orientation as a quaternion (yaw then pitch), for the renderer.
    pub fn orientation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
    }

    /// Forward view ray direction in world space.
    pub fn view_dir(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        // y-up, -z forward at yaw 0
        Vec3::new(sy * cp, sp, -cy * cp).normalize_or_zero()
    }

    pub fn is_alive(&self) -> bool {
        !self.flags.has(EntityFlags::DEAD) && self.health > 0
    }
}

/// A field-level delta of one entity against a baseline. Only changed fields are
/// transmitted; a missing field means "unchanged since baseline". This is the
/// core bandwidth lever for thousands of players.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EntityDelta {
    pub id: EntityId,
    /// Present only when the entity is new in this snapshot (full state needed).
    pub spawn: Option<EntityState>,
    pub pos: Option<Vec3>,
    pub vel: Option<Vec3>,
    pub yaw: Option<f32>,
    pub pitch: Option<f32>,
    pub flags: Option<EntityFlags>,
    pub health: Option<i16>,
    pub armor: Option<i16>,
    pub weapon: Option<u8>,
}

impl EntityDelta {
    /// Compute the delta of `new` relative to `old`. If `old` is `None` the
    /// entity is new and the delta carries a full `spawn`.
    pub fn diff(old: Option<&EntityState>, new: &EntityState) -> EntityDelta {
        let mut d = EntityDelta {
            id: new.id,
            ..Default::default()
        };
        match old {
            None => {
                d.spawn = Some(new.clone());
            }
            Some(o) => {
                if o.pos != new.pos {
                    d.pos = Some(new.pos);
                }
                if o.vel != new.vel {
                    d.vel = Some(new.vel);
                }
                if o.yaw != new.yaw {
                    d.yaw = Some(new.yaw);
                }
                if o.pitch != new.pitch {
                    d.pitch = Some(new.pitch);
                }
                if o.flags != new.flags {
                    d.flags = Some(new.flags);
                }
                if o.health != new.health {
                    d.health = Some(new.health);
                }
                if o.armor != new.armor {
                    d.armor = Some(new.armor);
                }
                if o.weapon != new.weapon {
                    d.weapon = Some(new.weapon);
                }
            }
        }
        d
    }

    /// Apply this delta on top of a baseline state to reconstruct the new state.
    /// Returns `None` only if the delta lacks a spawn for a previously-unknown id.
    pub fn apply(&self, base: Option<&EntityState>) -> Option<EntityState> {
        if let Some(spawn) = &self.spawn {
            return Some(spawn.clone());
        }
        let mut s = base?.clone();
        s.id = self.id;
        if let Some(v) = self.pos {
            s.pos = v;
        }
        if let Some(v) = self.vel {
            s.vel = v;
        }
        if let Some(v) = self.yaw {
            s.yaw = v;
        }
        if let Some(v) = self.pitch {
            s.pitch = v;
        }
        if let Some(v) = self.flags {
            s.flags = v;
        }
        if let Some(v) = self.health {
            s.health = v;
        }
        if let Some(v) = self.armor {
            s.armor = v;
        }
        if let Some(v) = self.weapon {
            s.weapon = v;
        }
        Some(s)
    }

    /// True if the delta carries no changes (used to skip empty entries).
    pub fn is_empty(&self) -> bool {
        self.spawn.is_none()
            && self.pos.is_none()
            && self.vel.is_none()
            && self.yaw.is_none()
            && self.pitch.is_none()
            && self.flags.is_none()
            && self.health.is_none()
            && self.armor.is_none()
            && self.weapon.is_none()
    }
}
