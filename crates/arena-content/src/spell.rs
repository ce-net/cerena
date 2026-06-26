//! The spell VM — the data model behind "make your own spells".
//!
//! A [`SpellDef`] is a small tree of [`EffectOp`]s. The interpreter lives in
//! `arena-sim::magic`; this crate only defines the *shape* of a spell so that new
//! spells (and player-authored ones) need **no code deploy** — they are just new
//! trees over the same fixed primitive ops.
//!
//! Design intent:
//! - Every op reuses a sim primitive (raycast, projectile, sphere query, damage,
//!   status, impulse...). The op set is closed; creativity comes from composition.
//! - A [`SpellDef`] is pure data: it serializes, hashes, and hot-reloads.
//! - Casting validates `mana_cost`, `cast_time`, and `cooldown` server-side, so a
//!   custom spell can never be cheaper or faster than its definition allows.
//! - Player-authored spells are the same `SpellDef` type, gated by a [`Budget`] the
//!   tech tree raises as a player advances novice -> expert.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::ids::{ElementId, MobId, SpellId, StatusId};

/// A complete spell: metadata plus the root effect graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpellDef {
    pub id: SpellId,
    pub name: String,
    /// Short flavour / tooltip; cosmetic.
    pub description: String,
    /// Primary element, used for resist/affinity math and default VFX colouring.
    pub element: ElementId,
    /// Mana required to begin the cast. Validated by the authority.
    pub mana_cost: f32,
    /// Seconds of channel before the effect fires. 0 = instant.
    pub cast_time: f32,
    /// Seconds before this spell can be cast again.
    pub cooldown: f32,
    /// If true the spell can be held to keep channeling (beams, auras), spending
    /// `mana_cost` per second instead of once.
    pub channeled: bool,
    /// How spell power scales with caster stats (see [`Scaling`]).
    pub scaling: Scaling,
    /// The root of the effect graph, evaluated when the cast fires.
    pub root: EffectOp,
    /// Authoring cost, used to gate player-made spells against their unlocked
    /// [`Budget`]. Recomputed from `root` by [`SpellDef::complexity`].
    #[serde(default)]
    pub author_cost: u32,
}

impl SpellDef {
    /// Total VM complexity: the count of effect ops weighted by op cost. Used to
    /// validate a player-authored spell against their [`Budget`].
    pub fn complexity(&self) -> u32 {
        self.root.complexity()
    }

    /// True if this spell fits within `budget` (for player-authored spells).
    pub fn fits(&self, budget: &Budget) -> bool {
        self.complexity() <= budget.max_complexity
            && self.root.max_depth() <= budget.max_depth
            && self.mana_cost >= budget.min_mana_floor
    }
}

/// How a caster's attributes amplify a spell. Final magnitude = base * (1 + sum of
/// attribute contributions). Kept as plain coefficients so it is data-tweakable.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Scaling {
    pub power: f32,
    pub focus: f32,
    pub agility: f32,
    pub level: f32,
}

/// A target selected by a shape op and fed to downstream effect ops.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Target {
    /// The caster themselves.
    SelfCaster,
    /// A point in the world (impact location, aim point).
    Point(Vec3),
    /// An entity (by sim entity id). Stored as u32 to avoid a protocol dep cycle.
    Entity(u32),
}

/// Who a spell op is allowed to affect, evaluated against caster/target teams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Faction {
    Enemies,
    Allies,
    SelfOnly,
    All,
}

/// The closed set of primitive operations a spell is built from. The interpreter
/// in `arena-sim` matches on these. Adding a *new* op is the rare case that needs a
/// code+binary change; everything else is composition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EffectOp {
    // ---- shape ops: select targets, then run `then` against each ----
    /// Instant ray from the caster's eye; first capsule/world hit becomes the
    /// target. Rifles/beams of the original FPS framing are just this.
    Ray {
        range: f32,
        /// Pierce N entities (0 = stop at first).
        pierce: u8,
        then: Box<EffectOp>,
    },
    /// A travelling projectile carrying `on_hit` as its continuation. Reuses the
    /// sim projectile system; on impact (or timeout) `on_hit` runs at the hit point.
    Projectile {
        speed: f32,
        gravity: f32,
        radius: f32,
        lifetime_s: f32,
        /// Homing strength toward the aim target (0 = dumb-fire).
        homing: f32,
        on_hit: Box<EffectOp>,
    },
    /// Sphere query around a target; runs `then` on each entity matching `faction`.
    Area {
        radius: f32,
        faction: Faction,
        /// Damage/effect falloff to the edge (1 = no falloff, 0 = none at edge).
        falloff: f32,
        then: Box<EffectOp>,
    },
    /// A cone in front of the caster (shotgun-like, breath weapons).
    Cone {
        range: f32,
        half_angle_rad: f32,
        faction: Faction,
        then: Box<EffectOp>,
    },
    /// Spawn a persistent field entity (fire patch, healing zone) that runs `tick`
    /// every `interval_s` for `duration_s` on entities inside `radius`.
    Field {
        radius: f32,
        duration_s: f32,
        interval_s: f32,
        faction: Faction,
        tick: Box<EffectOp>,
    },

    // ---- effect ops: mutate the world at the current target ----
    /// Deal damage. Element drives resist math; scales with caster + spell scaling.
    Damage { amount: f32, element: ElementId },
    /// Restore health.
    Heal { amount: f32 },
    /// Grant temporary shield HP that absorbs before health.
    Shield { amount: f32, duration_s: f32 },
    /// Apply a status effect instance (DoT, slow, burn, root, levitate...).
    ApplyStatus { status: StatusId, duration_s: f32, stacks: u8 },
    /// Push (+) or pull (-) the target relative to a point with `force`.
    Impulse { force: f32, vertical_bias: f32 },
    /// Teleport / blink the caster (or target) by `distance` along aim, or to a point.
    Teleport { max_distance: f32, to_target: bool },
    /// Summon a mob under the caster's ownership for `duration_s`.
    Summon { mob: MobId, count: u8, duration_s: f32 },
    /// Restore mana (siphon builds, mana potions expressed as spells).
    RestoreMana { amount: f32 },
    /// Mark the target (for combos / tech synergies); pure data flag.
    Mark { tag: String, duration_s: f32 },

    // ---- control ops: compose other ops ----
    /// Run each child in order, same tick.
    Sequence(Vec<EffectOp>),
    /// Run all children "simultaneously" (no ordering semantics).
    Parallel(Vec<EffectOp>),
    /// Wait `secs` (in sim ticks) before running `then`.
    Delay { secs: f32, then: Box<EffectOp> },
    /// Run `op` `count` times spaced `interval_s` apart.
    Repeat { count: u16, interval_s: f32, op: Box<EffectOp> },
    /// Run `then` with probability `chance` (deterministic per-cast seed in sim).
    Chance { chance: f32, then: Box<EffectOp> },
    /// Branch on whether the current target carries `tag` (set by `Mark`).
    IfMarked {
        tag: String,
        then: Box<EffectOp>,
        otherwise: Box<EffectOp>,
    },
    /// Does nothing (graph leaf / placeholder).
    Noop,
}

impl EffectOp {
    /// Weighted op count, used to price player-authored spells.
    pub fn complexity(&self) -> u32 {
        let here = match self {
            EffectOp::Summon { .. } | EffectOp::Field { .. } => 5,
            EffectOp::Teleport { .. } | EffectOp::Shield { .. } => 3,
            EffectOp::Projectile { .. } | EffectOp::Area { .. } | EffectOp::Cone { .. } => 2,
            EffectOp::Noop => 0,
            _ => 1,
        };
        here + self.children().iter().map(|c| c.complexity()).sum::<u32>()
    }

    /// Maximum nesting depth (guards against pathological recursion / griefing).
    pub fn max_depth(&self) -> u32 {
        1 + self
            .children()
            .iter()
            .map(|c| c.max_depth())
            .max()
            .unwrap_or(0)
    }

    /// Borrow all child ops, so traversal (complexity, validation, the interpreter)
    /// can be written once.
    pub fn children(&self) -> Vec<&EffectOp> {
        match self {
            EffectOp::Ray { then, .. }
            | EffectOp::Area { then, .. }
            | EffectOp::Cone { then, .. }
            | EffectOp::Delay { then, .. }
            | EffectOp::Chance { then, .. } => vec![then],
            EffectOp::Projectile { on_hit, .. } => vec![on_hit],
            EffectOp::Field { tick, .. } => vec![tick],
            EffectOp::Repeat { op, .. } => vec![op],
            EffectOp::IfMarked { then, otherwise, .. } => vec![then, otherwise],
            EffectOp::Sequence(v) | EffectOp::Parallel(v) => v.iter().collect(),
            _ => vec![],
        }
    }
}

/// The authoring budget a player may spend on a custom spell. The tech tree raises
/// these as the player progresses, so novices craft simple spells and experts craft
/// elaborate multi-stage ones.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    pub max_complexity: u32,
    pub max_depth: u32,
    /// A floor on mana cost relative to complexity, so a player can't author a
    /// free nuke. The authority also recomputes a fair mana cost from complexity.
    pub min_mana_floor: f32,
}

impl Default for Budget {
    fn default() -> Self {
        // Novice budget: a couple of ops, shallow.
        Self {
            max_complexity: 3,
            max_depth: 3,
            min_mana_floor: 5.0,
        }
    }
}
