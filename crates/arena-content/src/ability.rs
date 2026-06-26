//! Abilities: an *equipped action* that binds a spell to an input.
//!
//! A spell is the raw effect graph; an [`AbilityDef`] is how a player actually fires
//! it — wired to a mouse button or an action slot, optionally overriding the spell's
//! cooldown (for gear/tech that re-tunes a shared spell). The action bar a player
//! sees is a list of [`crate::ids::AbilityId`]s.

use serde::{Deserialize, Serialize};

use crate::ids::{AbilityId, MaterialId, SpellId};

/// The input that triggers an ability. The client maps these to concrete controls;
/// the sim only cares about which ability fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastInput {
    /// Primary fire (left mouse / main trigger).
    Primary,
    /// Secondary fire (right mouse / alt trigger).
    Secondary,
    /// One of the numbered action-bar slots.
    Slot(u8),
}

/// An equipped action wrapping a spell with a binding and optional overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbilityDef {
    pub id: AbilityId,
    pub name: String,
    /// The spell this ability casts. Must resolve in the active pack
    /// (enforced by [`crate::pack::ContentPack::validate`]).
    pub spell: SpellId,
    /// Which input fires it.
    pub binding: CastInput,
    /// Replaces the spell's own cooldown when set (gear / tech tuning).
    pub cooldown_override: Option<f32>,
    /// Material for the action-bar icon (procedural icon synthesis).
    pub icon_material: Option<MaterialId>,
}
