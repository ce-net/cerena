//! Weapon definitions, shared by the simulation (damage/ballistics), the client
//! (HUD/anim/prediction), and anti-cheat (fire-rate and accuracy ceilings).
//!
//! Definitions are data, not code, so a map/mode can ship its own balance table.

use serde::{Deserialize, Serialize};

/// How a weapon delivers damage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageKind {
    /// Instant ray, resolved with lag compensation (rifles, pistols, snipers).
    Hitscan,
    /// Travelling projectile simulated as an entity (rockets, grenades).
    Projectile,
    /// Short-range instant cone (melee, shotgun pellets share this with spread).
    Melee,
}

/// A single weapon's tunables. `id` indexes the loadout and the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeaponDef {
    pub id: u8,
    pub name: String,
    pub kind: DamageKind,
    /// Damage at the muzzle / point blank.
    pub base_damage: f32,
    /// Headshot multiplier applied on top of falloff.
    pub headshot_mult: f32,
    /// Rounds per second. The server enforces this as a hard fire-rate gate;
    /// `arena-karma` flags any client that beats it.
    pub fire_rate: f32,
    /// Magazine size; 0 = no reload (melee).
    pub mag_size: u16,
    /// Reload time, seconds.
    pub reload_s: f32,
    /// Muzzle speed for projectiles, m/s (ignored for hitscan/melee).
    pub projectile_speed: f32,
    /// Cone half-angle in radians at rest (recoil/movement widen it).
    pub spread_rad: f32,
    /// Pellets per shot (1 for rifles, N for shotguns).
    pub pellets: u8,
    /// Max effective range in metres; damage falls off to `falloff_min_frac`.
    pub max_range_m: f32,
    /// Damage fraction remaining at max range.
    pub falloff_min_frac: f32,
    /// Splash radius for projectiles (0 = no splash).
    pub splash_radius_m: f32,
}

impl WeaponDef {
    /// Minimum seconds between shots permitted by this weapon. The server rejects
    /// any fire input that arrives sooner than this since the player's last shot.
    pub fn min_shot_interval(&self) -> f32 {
        if self.fire_rate <= 0.0 {
            f32::INFINITY
        } else {
            1.0 / self.fire_rate
        }
    }

    /// Damage after distance falloff for a hit at `dist` metres.
    pub fn damage_at(&self, dist: f32) -> f32 {
        if dist <= 0.0 {
            return self.base_damage;
        }
        let t = (dist / self.max_range_m).clamp(0.0, 1.0);
        let frac = 1.0 + t * (self.falloff_min_frac - 1.0);
        self.base_damage * frac
    }
}

/// The default arena loadout. Indices are stable wire identifiers.
pub fn default_loadout() -> Vec<WeaponDef> {
    vec![
        WeaponDef {
            id: 0,
            name: "Rifle".into(),
            kind: DamageKind::Hitscan,
            base_damage: 24.0,
            headshot_mult: 2.0,
            fire_rate: 9.0,
            mag_size: 30,
            reload_s: 2.1,
            projectile_speed: 0.0,
            spread_rad: 0.006,
            pellets: 1,
            max_range_m: 90.0,
            falloff_min_frac: 0.6,
            splash_radius_m: 0.0,
        },
        WeaponDef {
            id: 1,
            name: "Pistol".into(),
            kind: DamageKind::Hitscan,
            base_damage: 18.0,
            headshot_mult: 2.4,
            fire_rate: 5.0,
            mag_size: 12,
            reload_s: 1.3,
            projectile_speed: 0.0,
            spread_rad: 0.004,
            pellets: 1,
            max_range_m: 60.0,
            falloff_min_frac: 0.5,
            splash_radius_m: 0.0,
        },
        WeaponDef {
            id: 2,
            name: "Shotgun".into(),
            kind: DamageKind::Melee, // instant cone
            base_damage: 11.0,
            headshot_mult: 1.5,
            fire_rate: 1.4,
            mag_size: 6,
            reload_s: 2.8,
            projectile_speed: 0.0,
            spread_rad: 0.10,
            pellets: 9,
            max_range_m: 22.0,
            falloff_min_frac: 0.15,
            splash_radius_m: 0.0,
        },
        WeaponDef {
            id: 3,
            name: "Rocket".into(),
            kind: DamageKind::Projectile,
            base_damage: 90.0,
            headshot_mult: 1.0,
            fire_rate: 0.9,
            mag_size: 4,
            reload_s: 3.2,
            projectile_speed: 38.0,
            spread_rad: 0.0,
            pellets: 1,
            max_range_m: 200.0,
            falloff_min_frac: 1.0,
            splash_radius_m: 4.5,
        },
        WeaponDef {
            id: 4,
            name: "Sniper".into(),
            kind: DamageKind::Hitscan,
            base_damage: 95.0,
            headshot_mult: 1.6,
            fire_rate: 1.1,
            mag_size: 5,
            reload_s: 3.0,
            projectile_speed: 0.0,
            spread_rad: 0.0008,
            pellets: 1,
            max_range_m: 300.0,
            falloff_min_frac: 1.0,
            splash_radius_m: 0.0,
        },
    ]
}
