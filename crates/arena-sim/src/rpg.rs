//! Per-character RPG progression: attributes, level/XP, mana and stamina pools,
//! and the tech a character has unlocked.
//!
//! [`RpgState`] is the mutable progression carried by every player entity. Item
//! [`StatMods`] (from equipped gear) and tech `StatMult` effects fold on top of the
//! raw attributes to produce the [`Derived`] stats the sim reads each tick (max
//! pools, regen, move-speed bonus, cooldown reduction, spell power). Everything is
//! deterministic and float-pure so the client can predict it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use arena_content::ids::TechNodeId;
use arena_content::item::{ElementMods, StatMods};

/// The four primary attributes. They rise on level-up and via gear/tech, and feed
/// the derived stats and spell scaling.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Attributes {
    pub power: f32,
    pub focus: f32,
    pub agility: f32,
    pub vitality: f32,
}

impl Default for Attributes {
    fn default() -> Self {
        // A fresh novice mage.
        Self {
            power: 10.0,
            focus: 10.0,
            agility: 10.0,
            vitality: 10.0,
        }
    }
}

/// Stats derived from attributes + equipped mods + tech, recomputed each tick. These
/// are what the rest of the sim actually consumes (pools, regen, multipliers). Every
/// gear stat surfaces here so combat/movement/the proc engine read *one* struct.
#[derive(Debug, Clone, Copy)]
pub struct Derived {
    pub max_mana: f32,
    pub max_health: f32,
    pub max_stamina: f32,
    pub mana_regen: f32,
    pub health_regen: f32,
    pub stamina_regen: f32,
    /// Additive bonus to base move speed (m/s).
    pub move_speed_bonus: f32,
    /// Fraction (0..0.8) shaved off cooldowns.
    pub cooldown_reduction: f32,
    /// Multiplicative spell-power factor (1.0 = baseline).
    pub spell_power: f32,
    pub power: f32,
    pub focus: f32,
    pub agility: f32,
    pub vitality: f32,

    // --- combat shaping (gear-driven; 1.0-based multipliers where noted) ---
    /// Total crit chance (0..1), base + gear.
    pub crit_chance: f32,
    /// Crit damage multiplier (1.5 base + gear bonus).
    pub crit_multiplier: f32,
    pub lifesteal: f32,
    pub mana_leech: f32,
    pub health_on_kill: f32,
    pub mana_on_kill: f32,
    /// Melee damage multiplier (1.0 = baseline).
    pub melee_power: f32,
    /// Added melee reach (m).
    pub melee_range_bonus: f32,
    /// Attack/swing/cast cadence multiplier (>1 = faster).
    pub attack_speed: f32,
    pub cast_speed: f32,
    /// Spell range / AoE / projectile-speed multipliers (1.0 = baseline).
    pub range_mult: f32,
    pub aoe_mult: f32,
    pub projectile_speed_mult: f32,
    /// Whole extra projectiles / pierce (rounded).
    pub extra_projectiles: u32,
    pub pierce: u32,
    /// Splash fraction of a hit dealt to nearby foes.
    pub area_damage: f32,
    pub knockback_mult: f32,

    // --- defence ---
    pub armor_flat: f32,
    /// Incoming-damage reduction fraction, capped at 0.85.
    pub damage_reduction: f32,
    pub block_chance: f32,
    pub thorns: f32,
    /// CC duration multiplier (<1 = shorter), from tenacity.
    pub cc_duration_mult: f32,
    pub fall_damage_mult: f32,

    // --- movement extras ---
    pub extra_jumps: u32,
    pub dash_charges: u32,

    // --- summons & economy ---
    pub summon_power: f32,
    pub extra_summons: u32,
    pub magic_find: f32,
    pub gold_find: f32,
    pub xp_gain: f32,

    // --- per-element ---
    pub elem_damage: ElementMods,
    pub elem_resist: ElementMods,
}

/// One character's progression and resource pools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpgState {
    pub level: u32,
    pub xp: u64,
    pub skill_points: u32,
    pub attributes: Attributes,
    pub max_mana: f32,
    pub mana: f32,
    pub mana_regen: f32,
    pub stamina: f32,
    pub max_stamina: f32,
    pub unlocked_tech: HashSet<TechNodeId>,
}

impl Default for RpgState {
    fn default() -> Self {
        let attributes = Attributes::default();
        let derived = derive(&attributes, 1, &StatMods::default());
        Self {
            level: 1,
            xp: 0,
            skill_points: 0,
            attributes,
            max_mana: derived.max_mana,
            mana: derived.max_mana,
            mana_regen: derived.mana_regen,
            stamina: derived.max_stamina,
            max_stamina: derived.max_stamina,
            unlocked_tech: HashSet::new(),
        }
    }
}

impl RpgState {
    /// XP required to advance *from* `level` to `level + 1`. A gentle quadratic-ish
    /// curve: early levels are quick, later ones grind.
    pub fn xp_for_level(level: u32) -> u64 {
        100 + (level as u64) * 50
    }

    /// Grant XP, applying as many level-ups as the amount affords. Each level grants
    /// a skill point and a small bump to every attribute. Returns true if at least
    /// one level was gained.
    pub fn grant_xp(&mut self, amount: u64) -> bool {
        self.xp += amount;
        let mut leveled = false;
        while self.xp >= Self::xp_for_level(self.level) {
            self.xp -= Self::xp_for_level(self.level);
            self.level += 1;
            self.skill_points += 1;
            self.attributes.power += 1.0;
            self.attributes.focus += 1.0;
            self.attributes.agility += 1.0;
            self.attributes.vitality += 1.0;
            leveled = true;
        }
        leveled
    }

    /// Compute the effective stats for this character given the summed [`StatMods`]
    /// of all equipped items and tech effects.
    pub fn derived(&self, mods: &StatMods) -> Derived {
        derive(&self.attributes, self.level, mods)
    }

    /// Spend mana if available; returns false (and spends nothing) if short. The
    /// authority calls this to gate a cast — a client can never cast for free.
    pub fn try_spend_mana(&mut self, cost: f32) -> bool {
        if self.mana + 1e-3 >= cost {
            self.mana = (self.mana - cost).max(0.0);
            true
        } else {
            false
        }
    }

    /// Spend stamina if available (movement modes). Returns false if short.
    pub fn try_spend_stamina(&mut self, cost: f32) -> bool {
        if self.stamina + 1e-3 >= cost {
            self.stamina = (self.stamina - cost).max(0.0);
            true
        } else {
            false
        }
    }

    /// Regenerate pools one tick. `dt` is the tick delta; `derived` carries the
    /// per-second regen rates and the (possibly mod-raised) pool caps.
    pub fn regen(&mut self, dt: f32, derived: &Derived) {
        self.max_mana = derived.max_mana;
        self.max_stamina = derived.max_stamina;
        self.mana_regen = derived.mana_regen;
        self.mana = (self.mana + derived.mana_regen * dt).min(self.max_mana);
        self.stamina = (self.stamina + derived.stamina_regen * dt).min(self.max_stamina);
    }
}

/// The shared derivation, used by both [`RpgState::derived`] and the `Default` impl.
fn derive(attrs: &Attributes, level: u32, mods: &StatMods) -> Derived {
    let _ = level;
    Derived {
        // Focus and vitality drive the resource pools; gear adds flat capacity.
        max_mana: 100.0 + attrs.focus * 5.0 + mods.max_mana,
        max_health: 100.0 + attrs.vitality * 8.0 + mods.max_health,
        max_stamina: 100.0 + attrs.agility * 2.0 + mods.max_stamina,
        mana_regen: 5.0 + attrs.focus * 0.2 + mods.mana_regen,
        health_regen: attrs.vitality * 0.1 + mods.health_regen,
        stamina_regen: 12.0 + attrs.agility * 0.3,
        move_speed_bonus: mods.move_speed + attrs.agility * 0.05,
        cooldown_reduction: mods.cooldown_reduction.clamp(0.0, 0.8),
        // Power raises raw magnitude; gear adds a flat percentage on top.
        spell_power: 1.0 + mods.spell_power_pct + attrs.power * 0.01,
        power: attrs.power + mods.power,
        focus: attrs.focus + mods.focus,
        agility: attrs.agility + mods.agility,
        vitality: attrs.vitality + mods.vitality,

        // Combat shaping. Base crit 5% / x1.5, raised by gear and a little by focus.
        crit_chance: (0.05 + mods.crit_chance + attrs.focus * 0.001).clamp(0.0, 1.0),
        crit_multiplier: 1.5 + mods.crit_damage,
        lifesteal: mods.lifesteal,
        mana_leech: mods.mana_leech,
        health_on_kill: mods.health_on_kill,
        mana_on_kill: mods.mana_on_kill,
        melee_power: 1.0 + mods.melee_power_pct + attrs.power * 0.008,
        melee_range_bonus: mods.melee_range,
        attack_speed: 1.0 + mods.attack_speed_pct,
        cast_speed: 1.0 + mods.cast_speed_pct,
        range_mult: 1.0 + mods.range_pct,
        aoe_mult: 1.0 + mods.aoe_radius_pct,
        projectile_speed_mult: 1.0 + mods.projectile_speed_pct,
        extra_projectiles: mods.extra_projectiles.round().max(0.0) as u32,
        pierce: mods.pierce.round().max(0.0) as u32,
        area_damage: mods.area_damage,
        knockback_mult: 1.0 + mods.knockback_pct,

        // Defence. Flat armor + a percentage pool, capped so you can't hit immortality.
        armor_flat: mods.armor,
        damage_reduction: mods.armor_pct.clamp(0.0, 0.85),
        block_chance: mods.block_chance.clamp(0.0, 0.75),
        thorns: mods.thorns,
        cc_duration_mult: (1.0 - mods.tenacity).clamp(0.1, 1.0),
        fall_damage_mult: (1.0 - mods.fall_damage_pct).clamp(0.0, 1.0),

        extra_jumps: mods.jump_count.round().max(0.0) as u32,
        dash_charges: mods.dash_charges.round().max(0.0) as u32,

        summon_power: 1.0 + mods.summon_power_pct,
        extra_summons: mods.summon_count.round().max(0.0) as u32,
        magic_find: mods.magic_find,
        gold_find: mods.gold_find,
        xp_gain: mods.xp_gain_pct,

        elem_damage: mods.elem_damage,
        elem_resist: mods.elem_resist,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xp_grant_levels_up() {
        let mut rpg = RpgState::default();
        assert_eq!(rpg.level, 1);
        // Level 1 needs 150 XP to reach level 2; grant more to also reach 3.
        let leveled = rpg.grant_xp(400);
        assert!(leveled, "should have leveled up");
        assert!(rpg.level >= 2, "level was {}", rpg.level);
        assert!(rpg.skill_points >= 1);
        assert!(rpg.attributes.power > 10.0);
    }
}
