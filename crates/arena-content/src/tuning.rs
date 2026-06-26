//! Global balance tuning — every gameplay magic-number, as hot-reloadable data.
//!
//! The single most important rule for a "tweak gameplay while 10,000 people play"
//! game is: **no balance constant may live in code**. If gravity, move speed, mana
//! regen, the XP curve, or the loot-drop fractions were `const`s in `arena-sim`,
//! changing them would mean a recompile + a fleet redeploy + a match restart. By
//! parking them all in one [`TuningConfig`] that travels inside the
//! [`crate::pack::ContentPack`], the designer edits a value, publishes a new pack,
//! and the authority swaps it at the next tick boundary — the match never stops.
//!
//! **`arena-sim` must read these from the active [`crate::registry::ContentRegistry`]
//! instead of hardcoding constants.** Each field documents the system that consults
//! it. Everything here is plain `f32`/`u32`/`bool` so it serializes, hashes, and
//! diffs trivially.

use serde::{Deserialize, Serialize};

/// All global, match-wide balance numbers. Read by `arena-sim`'s movement, health,
/// mana, stamina, combat, progression, and loot systems every tick (or on demand)
/// rather than baked into code, so any of them can change live.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TuningConfig {
    // ---- movement / physics (arena-sim character controller) ----
    /// Downward acceleration in m/s^2 applied to grounded-and-airborne actors.
    pub gravity: f32,
    /// Base walk speed in m/s before item/status multipliers.
    pub base_move_speed: f32,
    /// Multiplier applied to `base_move_speed` while sprinting.
    pub sprint_mult: f32,
    /// Multiplier applied to `base_move_speed` while crouched.
    pub crouch_mult: f32,
    /// Upward velocity (m/s) imparted by a jump.
    pub jump_impulse: f32,
    /// Horizontal acceleration (m/s^2) available while airborne (air control).
    pub air_accel: f32,
    /// Horizontal acceleration (m/s^2) available while grounded.
    pub ground_accel: f32,
    /// Velocity damping per second applied on the ground (higher = stickier stops).
    pub friction: f32,

    // ---- vitals (health / mana / stamina pools the sim initializes per actor) ----
    /// Default maximum health before `StatMods` / tech contributions.
    pub base_max_health: f32,
    /// Default maximum mana before contributions.
    pub base_max_mana: f32,
    /// Mana regenerated per second at rest before contributions.
    pub base_mana_regen: f32,
    /// Default maximum stamina (the pool movement modes drain).
    pub base_max_stamina: f32,
    /// Stamina regenerated per second.
    pub stamina_regen: f32,

    // ---- respawn / lifecycle ----
    /// Seconds a dead player waits before respawning.
    pub respawn_seconds: f32,

    // ---- loot economy (the "kill -> drop" mechanic; see crate::loot) ----
    /// Fraction (0..1) of the victim's banked XP that drops as a free pickup.
    pub loot_xp_drop_frac: f32,
    /// Fraction (0..1) of the victim's XP that is permanently bound to the killer
    /// (cannot be stolen by a third party picking up the world drop).
    pub loot_xp_bound_frac: f32,
    /// Fraction (0..1) of the victim's carried items that scatter as world drops.
    pub loot_item_drop_frac: f32,

    // ---- netcode ----
    /// Maximum milliseconds the authority will rewind for lag-compensated hit
    /// validation. Caps how far a high-ping client can "see into the past".
    pub lag_comp_max_ms: f32,

    // ---- combat pacing ----
    /// A floor (seconds) between any two spell casts by one actor, regardless of a
    /// spell's own cooldown — the global cooldown that keeps cast spam in check.
    pub spell_global_cooldown: f32,
    /// Global multiplier applied to damage from a headshot/critical hit location.
    pub headshot_mult_global: f32,

    // ---- progression (the XP curve and per-level point grants) ----
    /// Base of the level XP curve: `xp_to(level) = xp_curve_base * level^xp_curve_exp`.
    pub xp_curve_base: f32,
    /// Exponent of the level XP curve (super-linear difficulty ramp).
    pub xp_curve_exp: f32,
    /// Tech/skill points granted per level-up (spent in the tech tree).
    pub skill_points_per_level: u32,
    /// Attribute points granted per level-up (spent on power/focus/agility/vitality).
    pub attribute_points_per_level: u32,

    // ---- world population ----
    /// Multiplier on every [`crate::spawn::SpawnRuleDef`]'s `max_alive` budget, so the
    /// designer can thin or flood the whole world from one knob.
    pub mob_spawn_density: f32,
    /// Whether terrain/world edits persist across the match (mage-shaped world) or
    /// heal back over time.
    pub world_edit_persistence: bool,
    /// Whether allies can damage each other globally (game modes may still override).
    pub friendly_fire: bool,
}

impl Default for TuningConfig {
    fn default() -> Self {
        // Sensible, playable defaults for a fast first-person mage shooter. These are
        // the values arena-sim falls back to before any custom pack is loaded.
        Self {
            gravity: 24.0,
            base_move_speed: 7.0,
            sprint_mult: 1.6,
            crouch_mult: 0.5,
            jump_impulse: 8.5,
            air_accel: 18.0,
            ground_accel: 80.0,
            friction: 10.0,

            base_max_health: 100.0,
            base_max_mana: 100.0,
            base_mana_regen: 5.0,
            base_max_stamina: 100.0,
            stamina_regen: 18.0,

            respawn_seconds: 5.0,

            loot_xp_drop_frac: 0.25,
            loot_xp_bound_frac: 0.10,
            loot_item_drop_frac: 0.5,

            lag_comp_max_ms: 250.0,

            spell_global_cooldown: 0.25,
            headshot_mult_global: 2.0,

            xp_curve_base: 100.0,
            xp_curve_exp: 1.5,
            skill_points_per_level: 1,
            attribute_points_per_level: 3,

            mob_spawn_density: 1.0,
            world_edit_persistence: true,
            friendly_fire: false,
        }
    }
}

impl TuningConfig {
    /// XP required to advance *from* `level` to the next, per the curve params. A
    /// convenience the sim's progression system can call so the formula itself is
    /// data-driven (only the shape is fixed). `level` is 1-based.
    pub fn xp_to_next(&self, level: u32) -> u64 {
        let l = level.max(1) as f32;
        (self.xp_curve_base * l.powf(self.xp_curve_exp)).round() as u64
    }
}
