//! The proc engine: it turns the equipped gear's [`ItemTrigger`]s into actual things
//! that happen in the world. This is the seam that makes "every item has a noticeable
//! effect" true — a sword's chain-lightning, boots' fire trail, a robe's frost nova all
//! flow through here.
//!
//! Flow: the sim raises a [`ProcEvent`] at the natural moment (a hit lands, a kill is
//! confirmed, a tick elapses), builds a [`ProcContext`], and calls [`evaluate`] with the
//! wearer's equipped triggers and their per-entity [`ProcRuntime`] (which holds the
//! internal-cooldown timers and aura/interval bookkeeping). [`evaluate`] returns a list
//! of [`ProcOutcome`]s — pure descriptions of what to do — that the caller applies using
//! the existing sim primitives (the magic VM for `CastSpell`, the status system for
//! buffs/debuffs, `combat` for novas/bolts). Keeping outcomes as data means the engine
//! is deterministic and testable without a live `World`.
//!
//! Determinism: proc *chance* is rolled from a per-event deterministic seed
//! (`ForgeRng`), never an ambient RNG, so an authority and its shadow validators fire
//! exactly the same procs.

use std::collections::HashMap;

use glam::Vec3;
use serde::{Deserialize, Serialize};

use arena_content::ids::{ElementId, MobId, SpellId, StatusId};
use arena_content::item::{ItemTrigger, ProcEffect, ProcWhen, StatMods};

use crate::forge::ForgeRng;

/// The sim event that may set off procs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcEvent {
    Hit,
    Crit,
    Melee,
    Kill,
    Cast,
    TookDamage,
    Blocked,
    Dashed,
    /// A periodic heartbeat (the caller ticks this; `Aura`/`Interval` procs read it).
    Tick,
}

/// Everything a proc needs to know about the moment it fires.
#[derive(Debug, Clone)]
pub struct ProcContext {
    pub event: ProcEvent,
    /// The entity wearing the gear (the proc's "self").
    pub wearer: u64,
    /// The other party (the thing hit, the attacker) if any.
    pub other: Option<u64>,
    /// World point the effect originates from (impact point, wearer position).
    pub origin: Vec3,
    /// Damage involved in this event (for context; not all procs use it).
    pub damage: f32,
    /// Wearer health fraction *after* this event (drives `OnLowHealth`).
    pub health_frac: f32,
    /// Seconds elapsed since the previous `Tick` (for `Interval`/`Aura` accounting).
    pub dt: f32,
    /// A deterministic per-event seed (e.g. tick * something ^ wearer). The engine
    /// derives all chance rolls from this, never an ambient RNG.
    pub seed: u64,
}

/// A resolved thing to do, in the sim's own vocabulary. The caller maps each to an
/// existing primitive; nothing here touches the world directly.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcOutcome {
    CastSpell { caster: u64, spell: SpellId, at: Vec3 },
    ApplyStatus { target: u64, status: StatusId, duration_s: f32, stacks: u8 },
    Nova { source: u64, at: Vec3, element: ElementId, radius: f32, damage: f32 },
    ChainBolt { source: u64, from: Vec3, element: ElementId, jumps: u8, damage: f32 },
    Heal { target: u64, amount: f32 },
    Shield { target: u64, amount: f32, duration_s: f32 },
    /// A timed flat stat buff to apply to the wearer (the sim folds it into derived
    /// stats for `duration_s`). The boxed mods mirror the content shape.
    TempStats { target: u64, mods: Box<StatMods>, duration_s: f32 },
    Summon { owner: u64, mob: MobId, count: u8, duration_s: f32 },
}

/// Per-entity proc bookkeeping: internal-cooldown timers (keyed by trigger label) and
/// interval accumulators. Lives on the entity, persisted with it across hand-off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcRuntime {
    /// Remaining ICD seconds per trigger label (0 / absent = ready).
    icd: HashMap<String, f32>,
    /// Seconds accumulated toward each `Interval` trigger's period.
    interval_accum: HashMap<String, f32>,
}

impl ProcRuntime {
    /// Advance all timers by `dt`. Call once per tick before evaluating `Tick` events.
    pub fn tick(&mut self, dt: f32) {
        for v in self.icd.values_mut() {
            *v = (*v - dt).max(0.0);
        }
        self.icd.retain(|_, v| *v > 0.0);
    }

    fn ready(&self, label: &str) -> bool {
        self.icd.get(label).copied().unwrap_or(0.0) <= 0.0
    }

    fn arm(&mut self, label: &str, icd_s: f32) {
        if icd_s > 0.0 {
            self.icd.insert(label.to_string(), icd_s);
        }
    }
}

/// Whether a trigger's `when` is satisfied by `event`. `Interval`/`Aura` only fire on
/// `Tick`; the interval gating (has enough time accumulated?) is handled in [`evaluate`].
fn fires_on(when: &ProcWhen, ctx: &ProcContext) -> bool {
    match (when, ctx.event) {
        (ProcWhen::OnHit, ProcEvent::Hit) => true,
        (ProcWhen::OnCrit, ProcEvent::Crit) => true,
        (ProcWhen::OnMelee, ProcEvent::Melee) => true,
        (ProcWhen::OnKill, ProcEvent::Kill) => true,
        (ProcWhen::OnCast, ProcEvent::Cast) => true,
        (ProcWhen::OnTakeDamage, ProcEvent::TookDamage) => true,
        (ProcWhen::OnBlock, ProcEvent::Blocked) => true,
        (ProcWhen::OnDash, ProcEvent::Dashed) => true,
        (ProcWhen::OnLowHealth { frac }, ProcEvent::TookDamage) => ctx.health_frac <= *frac,
        (ProcWhen::Aura, ProcEvent::Tick) => true,
        (ProcWhen::Interval { .. }, ProcEvent::Tick) => true,
        _ => false,
    }
}

/// Evaluate all `triggers` against one event, returning the outcomes that fire. Mutates
/// `rt` to record internal cooldowns and interval progress. Deterministic in `ctx.seed`.
pub fn evaluate(
    triggers: &[ItemTrigger],
    ctx: &ProcContext,
    rt: &mut ProcRuntime,
) -> Vec<ProcOutcome> {
    let mut out = Vec::new();
    let mut rng = ForgeRng::seed(&[ctx.seed, ctx.wearer, ctx.event as u64]);

    for (i, t) in triggers.iter().enumerate() {
        if !fires_on(&t.when, ctx) {
            continue;
        }

        // Interval gating: accumulate dt; only fire once a full period elapses.
        if let ProcWhen::Interval { secs } = t.when {
            let acc = rt.interval_accum.entry(t.label.clone()).or_insert(0.0);
            *acc += ctx.dt;
            if *acc < secs {
                continue;
            }
            *acc -= secs;
        }

        // Internal cooldown (auras/intervals self-gate via their period, so skip ICD
        // for them unless explicitly set).
        if !rt.ready(&t.label) {
            continue;
        }

        // Chance roll (auras always apply).
        let always = matches!(t.when, ProcWhen::Aura | ProcWhen::Interval { .. });
        if !always && t.chance < 1.0 {
            // Mix the trigger index in so co-located procs roll independently.
            let mut r = ForgeRng::seed(&[rng.next_u64(), i as u64]);
            if r.unit() > t.chance {
                continue;
            }
        }

        if let Some(o) = resolve(&t.effect, ctx) {
            out.push(o);
            rt.arm(&t.label, t.icd_s);
        }
    }
    out
}

/// Translate a content [`ProcEffect`] into a sim [`ProcOutcome`] at the event's natural
/// origin/target.
fn resolve(effect: &ProcEffect, ctx: &ProcContext) -> Option<ProcOutcome> {
    let target_other = ctx.other.unwrap_or(ctx.wearer);
    Some(match effect {
        ProcEffect::CastSpell { spell } => ProcOutcome::CastSpell {
            caster: ctx.wearer,
            spell: spell.clone(),
            at: ctx.origin,
        },
        ProcEffect::BuffSelf { status, duration_s, stacks } => ProcOutcome::ApplyStatus {
            target: ctx.wearer,
            status: status.clone(),
            duration_s: *duration_s,
            stacks: *stacks,
        },
        ProcEffect::DebuffTarget { status, duration_s, stacks } => ProcOutcome::ApplyStatus {
            target: target_other,
            status: status.clone(),
            duration_s: *duration_s,
            stacks: *stacks,
        },
        ProcEffect::Nova { element, radius, damage } => ProcOutcome::Nova {
            source: ctx.wearer,
            at: ctx.origin,
            element: element.clone(),
            radius: *radius,
            damage: *damage,
        },
        ProcEffect::ChainBolt { element, jumps, damage } => ProcOutcome::ChainBolt {
            source: ctx.wearer,
            from: ctx.origin,
            element: element.clone(),
            jumps: *jumps,
            damage: *damage,
        },
        ProcEffect::Heal { amount } => ProcOutcome::Heal { target: ctx.wearer, amount: *amount },
        ProcEffect::Shield { amount, duration_s } => ProcOutcome::Shield {
            target: ctx.wearer,
            amount: *amount,
            duration_s: *duration_s,
        },
        ProcEffect::TempStats { mods, duration_s } => ProcOutcome::TempStats {
            target: ctx.wearer,
            mods: mods.clone(),
            duration_s: *duration_s,
        },
        ProcEffect::Summon { mob, count, duration_s } => ProcOutcome::Summon {
            owner: ctx.wearer,
            mob: mob.clone(),
            count: *count,
            duration_s: *duration_s,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::item::ProcWhen;

    fn ctx(event: ProcEvent, hp: f32) -> ProcContext {
        ProcContext {
            event,
            wearer: 1,
            other: Some(2),
            origin: Vec3::ZERO,
            damage: 50.0,
            health_frac: hp,
            dt: 0.1,
            seed: 42,
        }
    }

    #[test]
    fn icd_blocks_repeat_fire() {
        let trig = ItemTrigger {
            label: "nova".into(),
            when: ProcWhen::OnHit,
            chance: 1.0,
            icd_s: 2.0,
            effect: ProcEffect::Nova { element: "fire".into(), radius: 3.0, damage: 40.0 },
        };
        let mut rt = ProcRuntime::default();
        let first = evaluate(std::slice::from_ref(&trig), &ctx(ProcEvent::Hit, 1.0), &mut rt);
        assert_eq!(first.len(), 1, "first hit should fire");
        let second = evaluate(std::slice::from_ref(&trig), &ctx(ProcEvent::Hit, 1.0), &mut rt);
        assert!(second.is_empty(), "ICD should suppress the immediate refire");
        rt.tick(2.0);
        let third = evaluate(std::slice::from_ref(&trig), &ctx(ProcEvent::Hit, 1.0), &mut rt);
        assert_eq!(third.len(), 1, "after ICD elapses it should fire again");
    }

    #[test]
    fn low_health_gate_respects_threshold() {
        let trig = ItemTrigger {
            label: "panic".into(),
            when: ProcWhen::OnLowHealth { frac: 0.3 },
            chance: 1.0,
            icd_s: 0.0,
            effect: ProcEffect::Shield { amount: 100.0, duration_s: 4.0 },
        };
        let mut rt = ProcRuntime::default();
        assert!(evaluate(std::slice::from_ref(&trig), &ctx(ProcEvent::TookDamage, 0.8), &mut rt).is_empty());
        assert_eq!(evaluate(std::slice::from_ref(&trig), &ctx(ProcEvent::TookDamage, 0.25), &mut rt).len(), 1);
    }
}
