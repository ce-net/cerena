//! Status effects: the buffs, debuffs, damage-over-time, and control states that
//! ride on entities.
//!
//! Spells apply statuses by id (`EffectOp::ApplyStatus`); the sim's status system
//! ticks them. A [`StatusEffectDef`] is pure data, so re-balancing a burn or adding
//! a new buff is a hot-reload. Live entities carry status *instances* that reference
//! these defs by [`StatusId`].

use serde::{Deserialize, Serialize};

use crate::ids::{ElementId, MaterialId, StatusId};

/// What a status actually does each tick / while active. The sim matches on this to
/// apply the mechanical effect; the variants cover the common RPG vocabulary and are
/// deliberately rich so designers compose interesting combat without new code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StatusKind {
    /// Generic elemental damage over time at `dps` per second.
    DamageOverTime { element: ElementId, dps: f32 },
    /// Reduce movement (and optionally cast) speed by `frac` (0..1).
    Slow { frac: f32 },
    /// Increase movement speed by `frac` (0..1+).
    Haste { frac: f32 },
    /// Immobilize (cannot move) but can still act.
    Root,
    /// Prevent casting.
    Silence,
    /// Suppress gravity; the target floats (set-up for combos, crowd control).
    Levitate,
    /// Fire-flavoured DoT at `dps`; distinct from generic DoT for VFX + synergy.
    Burning { dps: f32 },
    /// Frozen solid: immobilized and unable to act.
    Frozen,
    /// Heal over time at `hps` per second.
    Regen { hps: f32 },
    /// Drain mana over time at `mps` per second.
    ManaBurn { mps: f32 },
    /// Take `frac` extra damage (0..1+).
    Vulnerable { frac: f32 },
    /// Deal `frac` extra damage (0..1+).
    Empower { frac: f32 },
    /// Hidden from enemy targeting / reduced detection.
    Invisible,
    /// Absorb up to `amount` incoming damage before health is touched.
    Shielded { amount: f32 },
}

/// A status definition: its mechanic plus stacking / timing / presentation metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusEffectDef {
    pub id: StatusId,
    pub name: String,
    /// The mechanical effect.
    pub kind: StatusKind,
    /// Maximum simultaneous stacks (1 = non-stacking).
    pub max_stacks: u8,
    /// Seconds between ticks for periodic kinds (DoT/Regen/ManaBurn).
    pub tick_interval_s: f32,
    /// Default duration when an applier does not specify one.
    pub duration_default_s: f32,
    /// True for buffs (helps the client tint the icon, and lets cleanse logic tell
    /// beneficial from harmful effects).
    pub beneficial: bool,
    /// Material for the status icon / on-body VFX.
    pub material: Option<MaterialId>,
}
