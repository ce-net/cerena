//! Combat: firing, lag-compensated hit resolution, damage, and splash.
//!
//! This module holds the *pure* combat building blocks — fire-rate/reload state,
//! deterministic spread, hitscan resolution against a set of target capsules, and
//! the damage/armour maths. [`crate::world::World`] owns the orchestration (who
//! fired, which targets to assemble for lag-compensation, applying results) and
//! calls into here. Splitting it this way keeps the firing logic free of the
//! borrow gymnastics that the mutable `World` aggregate forces, and makes each
//! piece unit-testable in isolation.
//!
//! Determinism note: where a real game would roll dice (shotgun spread) we instead
//! hash the tick, shooter and pellet index. The client predicting a shot and the
//! server resolving it derive the *same* pattern, so they agree without shared RNG.

use glam::Vec3;

use arena_protocol::weapon::WeaponDef;
use arena_protocol::world::{Aabb, Team};
use arena_protocol::{EntityId, Tick};

use crate::collision;

/// Per-player combat bookkeeping, stored in the world alongside entity state.
#[derive(Debug, Clone)]
pub struct CombatState {
    pub ammo_in_mag: u16,
    pub ammo_reserve: u16,
    /// Server tick of this player's last shot; gates the fire-rate.
    pub last_shot_tick: Option<Tick>,
    /// Tick at which an in-progress reload completes (`None` = not reloading).
    pub reload_end_tick: Option<Tick>,
    /// Tick at/after which a dead player may respawn (`0` = alive).
    pub respawn_at: Tick,
    /// Whether FIRE was held last tick — used only to distinguish a fresh trigger
    /// pull from a held auto-fire when counting fire-rate violations.
    pub prev_fire: bool,
}

impl CombatState {
    /// Fresh combat state with a full magazine and a stock of reserve ammo.
    pub fn fresh(weapon: &WeaponDef) -> CombatState {
        CombatState {
            ammo_in_mag: weapon.mag_size,
            // Five spare magazines (melee/0-mag weapons get none).
            ammo_reserve: weapon.mag_size.saturating_mul(5),
            last_shot_tick: None,
            reload_end_tick: None,
            respawn_at: 0,
            prev_fire: false,
        }
    }

    pub fn is_reloading(&self) -> bool {
        self.reload_end_tick.is_some()
    }
}

/// A flying projectile (rocket/grenade) tracked outside of [`CombatState`].
#[derive(Debug, Clone, Copy)]
pub struct ProjectileState {
    /// The player entity that fired it (credited with kills, immune to its blast).
    pub owner: EntityId,
    /// Weapon id, for damage/splash lookup and the kill feed.
    pub weapon: u8,
    /// Tick at which it self-destructs if it has hit nothing.
    pub expire_tick: Tick,
}

/// A target capsule presented to hitscan, already resolved to the tick the shooter
/// is being lag-compensated to. `base` is the **feet** position.
#[derive(Debug, Clone, Copy)]
pub struct FireTarget {
    pub id: EntityId,
    pub base: Vec3,
    pub half_height: f32,
    pub radius: f32,
}

/// One resolved hit: the victim, where it landed, the distance travelled, and
/// whether it was a headshot. Damage is computed by the caller from the weapon.
#[derive(Debug, Clone, Copy)]
pub struct HitResult {
    pub victim: EntityId,
    pub point: Vec3,
    pub dist: f32,
    pub headshot: bool,
}

/// A pending damage event, queued during resolution and applied after all fires
/// for the tick are computed (so reads of the world stay consistent).
#[derive(Debug, Clone, Copy)]
pub struct DamageApply {
    pub attacker: EntityId,
    pub victim: EntityId,
    pub amount: f32,
    pub headshot: bool,
    pub point: Vec3,
    pub weapon: u8,
}

/// Resolve a single hitscan ray against the world and the candidate targets,
/// returning the nearest target hit that is not occluded by static geometry.
///
/// `dir` must be normalised. `targets` should already exclude the shooter and
/// friendlies, and carry lag-compensated positions.
pub fn hitscan_ray(
    origin: Vec3,
    dir: Vec3,
    max_range: f32,
    targets: &[FireTarget],
    brushes: &[Aabb],
) -> Option<HitResult> {
    // A wall between shooter and target stops the bullet.
    let wall_t = collision::raycast_aabbs(origin, dir, max_range, brushes).map(|(t, _)| t);

    let mut best: Option<HitResult> = None;
    for tgt in targets {
        if let Some((t, point, head)) =
            collision::ray_capsule(origin, dir, tgt.base, tgt.half_height, tgt.radius)
        {
            if t > max_range {
                continue;
            }
            // Occluded by geometry that is closer than the player.
            if let Some(wt) = wall_t {
                if wt < t {
                    continue;
                }
            }
            if best.map_or(true, |b| t < b.dist) {
                best = Some(HitResult {
                    victim: tgt.id,
                    point,
                    dist: t,
                    headshot: head,
                });
            }
        }
    }
    best
}

/// Final damage for a hit at `dist` with the given weapon, applying distance
/// falloff and the headshot multiplier.
pub fn damage_for(weapon: &WeaponDef, dist: f32, headshot: bool) -> f32 {
    let mut dmg = weapon.damage_at(dist);
    if headshot {
        dmg *= weapon.headshot_mult;
    }
    dmg
}

/// Split incoming `damage` across armour then health. Armour soaks half of each
/// hit until depleted; the rest (and everything once armour is gone) bites health.
/// Values are rounded to the nearest integer to match the `i16` wire fields.
pub fn apply_armor_damage(damage: f32, health: &mut i16, armor: &mut i16) {
    let to_armor = (damage * 0.5).min(*armor as f32).max(0.0);
    let to_health = damage - to_armor;
    *armor = (*armor as f32 - to_armor).round().max(0.0) as i16;
    *health = (*health as f32 - to_health).round() as i16;
}

/// Deterministically perturb `base_dir` within a cone of half-angle `spread_rad`.
/// The pattern is a function of `seed` only (no RNG state), so a client and server
/// fed the same `seed` produce the identical pellet direction.
pub fn deterministic_spread(base_dir: Vec3, spread_rad: f32, seed: u64) -> Vec3 {
    if spread_rad <= 0.0 {
        return base_dir;
    }
    // Two independent uniforms from the seed.
    let u1 = unit_f32(splitmix64(seed));
    let u2 = unit_f32(splitmix64(seed ^ 0x9E37_79B9_7F4A_7C15));
    // Uniform sample over the disk: angle + sqrt-distributed radius keeps density
    // even (so pellets aren't bunched at the centre).
    let theta = u1 * std::f32::consts::TAU;
    let rho = spread_rad * u2.sqrt();

    // Build a basis perpendicular to the shot direction.
    let up = if base_dir.y.abs() > 0.99 {
        Vec3::X
    } else {
        Vec3::Y
    };
    let t1 = base_dir.cross(up).normalize_or_zero();
    let t2 = base_dir.cross(t1).normalize_or_zero();

    let offset = t1 * (rho * theta.cos()) + t2 * (rho * theta.sin());
    (base_dir + offset).normalize_or_zero()
}

/// Combine the tick, shooter id and pellet index into a spread seed. Distinct
/// pellets of one shot get distinct seeds; the same shot replays identically.
pub fn spread_seed(tick: Tick, shooter: EntityId, pellet: u8) -> u64 {
    let base = ((tick as u64) << 32) ^ (shooter as u64);
    splitmix64(base).wrapping_add(pellet as u64)
}

// --- deterministic hashing --------------------------------------------------

/// SplitMix64: a tiny, well-distributed integer hash. Pure, branch-free, and
/// identical on every platform — exactly what deterministic spread needs.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Map a 64-bit hash to an `f32` in `[0, 1)` using its top 24 bits (f32 mantissa).
fn unit_f32(h: u64) -> f32 {
    ((h >> 40) as f32) / (1u64 << 24) as f32
}

/// True if a shooter on `shooter_team` may damage a target on `target_team`.
/// Free-for-all (`Team::None` on either side) means everyone is fair game.
pub fn can_damage(shooter_team: Team, target_team: Team) -> bool {
    shooter_team == Team::None || target_team == Team::None || shooter_team != target_team
}
