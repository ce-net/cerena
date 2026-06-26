//! Per-character RPG progression: attributes, level/XP, mana and stamina pools,
//! and the tech a character has unlocked.
//!
//! [`RpgState`] is the mutable progression carried by every player entity. Item
//! [`StatMods`] (from equipped gear) and tech `StatMult` effects fold on top of the
//! raw attributes to produce the [`Derived`] stats the sim reads each tick (max
//! pools, regen, move-speed bonus, cooldown reduction, spell power). Everything is
//! deterministic and float-pure so the client can predict it.

use std::collections::HashSet;

use arena_content::ids::TechNodeId;
use arena_content::item::StatMods;

/// The four primary attributes. They rise on level-up and via gear/tech, and feed
/// the derived stats and spell scaling.
#[derive(Debug, Clone, Copy, PartialEq)]
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
/// are what the rest of the sim actually consumes (pools, regen, multipliers).
#[derive(Debug, Clone, Copy)]
pub struct Derived {
    pub max_mana: f32,
    pub max_health: f32,
    pub max_stamina: f32,
    pub mana_regen: f32,
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
}

/// One character's progression and resource pools.
#[derive(Debug, Clone)]
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
    Derived {
        // Focus and vitality drive the resource pools; gear adds flat capacity.
        max_mana: 100.0 + attrs.focus * 5.0 + mods.max_mana,
        max_health: 100.0 + attrs.vitality * 8.0 + mods.max_health,
        max_stamina: 100.0 + attrs.agility * 2.0,
        mana_regen: 5.0 + attrs.focus * 0.2 + mods.mana_regen,
        stamina_regen: 12.0 + attrs.agility * 0.3,
        move_speed_bonus: mods.move_speed + attrs.agility * 0.05,
        cooldown_reduction: mods.cooldown_reduction.clamp(0.0, 0.8),
        // Power raises raw magnitude; gear adds a flat percentage on top.
        spell_power: 1.0 + mods.spell_power_pct + attrs.power * 0.01,
        power: attrs.power + mods.power,
        focus: attrs.focus + mods.focus,
        agility: attrs.agility + mods.agility,
        vitality: attrs.vitality + mods.vitality,
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
