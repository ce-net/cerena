//! The spell VM: the interpreter for the closed [`EffectOp`] set.
//!
//! A spell is a tree of [`EffectOp`]s (defined as hot-reloadable data in
//! `arena-content`). This module walks that tree against the live [`World`], reusing
//! the sim's existing primitives: raycasts and capsule queries for shape ops,
//! projectile spawning for travelling ops, the armour/health damage split, statuses,
//! impulses and teleports for effect ops. Creativity lives in *composition* of these
//! fixed primitives, so a brand-new spell needs no code change.
//!
//! Everything is deterministic: the one source of "randomness" (`Chance`) is a hash
//! of the cast's tick + caster + a per-cast salt, so client prediction and the
//! authority agree.
//!
//! Safety: the interpreter is bounded by an op budget and a max depth so a
//! pathological (or maliciously authored) graph can never stall a tick.

use glam::Vec3;

use arena_content::ids::MobId;
use arena_content::spell::{EffectOp, Faction, SpellDef, Target};
use arena_protocol::snapshot::GameEvent;
use arena_protocol::world::Team;
use arena_protocol::{EntityId, TICK_HZ, Tick};

use crate::combat;
use crate::world::World;

/// Hard ceiling on ops evaluated in a single cast (or continuation run). Generous
/// for legitimate spells, fatal to a graph built to grief the tick loop.
pub const MAX_OPS_PER_RUN: u32 = 512;
/// Hard ceiling on nesting depth.
pub const MAX_DEPTH: u32 = 24;

/// The immutable context threaded through one cast: who cast it, from where, and the
/// precomputed damage multiplier (scaling vs caster attributes + spell power). Plain
/// data so it can be stored on a projectile / field / scheduled effect and replayed
/// later when the continuation fires.
#[derive(Debug, Clone)]
pub struct CastContext {
    pub caster: EntityId,
    pub caster_team: Team,
    pub origin: Vec3,
    pub dir: Vec3,
    /// Final magnitude multiplier applied to Damage/Heal amounts.
    pub damage_mult: f32,
    pub tick: Tick,
    /// Per-cast deterministic salt (folds in the spell id), so `Chance` rolls and
    /// other hashed decisions are stable and unique per cast.
    pub salt: u64,
}

/// Tracks the remaining op/recursion budget for one run.
struct Budget {
    ops_left: u32,
    depth: u32,
}

impl Budget {
    fn new() -> Self {
        Budget {
            ops_left: MAX_OPS_PER_RUN,
            depth: 0,
        }
    }
    /// Charge one op and one depth level; false if the budget is exhausted.
    fn enter(&mut self) -> bool {
        if self.ops_left == 0 || self.depth >= MAX_DEPTH {
            return false;
        }
        self.ops_left -= 1;
        self.depth += 1;
        true
    }
    fn exit(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

/// Cast `spell` for `caster`, aimed from `aim_origin` along `aim_dir`. Returns the
/// VFX/feedback events produced (Shot/Hit/Death/Explosion/...). The caller (the
/// world tick) has already validated mana / cooldown / cast-time.
pub fn cast(
    world: &mut World,
    caster: EntityId,
    spell: &SpellDef,
    aim_origin: Vec3,
    aim_dir: Vec3,
    tick: Tick,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let caster_team = world.team_of(caster).unwrap_or(Team::None);
    let damage_mult = world.spell_damage_mult(caster, spell);
    let salt = combat::str_salt(spell.id.as_str()) ^ ((tick as u64) << 1);
    let ctx = CastContext {
        caster,
        caster_team,
        origin: aim_origin,
        dir: aim_dir.normalize_or_zero(),
        damage_mult,
        tick,
        salt,
    };

    // A muzzle/cast event so the client always shows the cast, even on a whiff.
    events.push(GameEvent::Shot {
        shooter: caster,
        weapon: spell_event_weapon(spell),
        origin: aim_origin,
        dir: ctx.dir,
    });

    let mut budget = Budget::new();
    eval(world, &ctx, &spell.root, Target::SelfCaster, &mut budget, &mut events);
    events
}

/// Run a stored continuation op (projectile impact, field tick, delayed/repeated
/// effect) with a fresh budget. Used by the world tick.
pub fn run_op(
    world: &mut World,
    ctx: &CastContext,
    op: &EffectOp,
    target: Target,
    events: &mut Vec<GameEvent>,
) {
    let mut budget = Budget::new();
    eval(world, ctx, op, target, &mut budget, events);
}

/// Evaluate one op against `target`, bounded by `budget`.
fn eval(
    world: &mut World,
    ctx: &CastContext,
    op: &EffectOp,
    target: Target,
    budget: &mut Budget,
    events: &mut Vec<GameEvent>,
) {
    if !budget.enter() {
        return;
    }
    eval_inner(world, ctx, op, target, budget, events);
    budget.exit();
}

fn eval_inner(
    world: &mut World,
    ctx: &CastContext,
    op: &EffectOp,
    target: Target,
    budget: &mut Budget,
    events: &mut Vec<GameEvent>,
) {
    match op {
        // ---- shape ops ----
        EffectOp::Ray { range, pierce, then } => {
            // Gather up to `pierce + 1` faction-matching entities along the eye ray,
            // stopping at world geometry. Default faction for a bare ray is Enemies.
            let hits = world.ray_targets(
                ctx.origin,
                ctx.dir,
                *range,
                *pierce as usize + 1,
                Faction::Enemies,
                ctx.caster,
                ctx.caster_team,
            );
            for (id, _point) in hits {
                eval(world, ctx, then, Target::Entity(id), budget, events);
            }
        }
        EffectOp::Cone { range, half_angle_rad, faction, then } => {
            let ids = world.cone_targets(
                ctx.origin,
                ctx.dir,
                *range,
                *half_angle_rad,
                *faction,
                ctx.caster,
                ctx.caster_team,
            );
            for id in ids {
                eval(world, ctx, then, Target::Entity(id), budget, events);
            }
        }
        EffectOp::Area { radius, faction, falloff, then } => {
            let center = target_point(world, ctx, target);
            let hits = world.sphere_targets(center, *radius, *faction, ctx.caster, ctx.caster_team);
            for (id, dist) in hits {
                // Falloff scales the magnitude from full at the centre to `falloff`
                // at the edge.
                let scale = 1.0 - (1.0 - *falloff) * (dist / *radius).clamp(0.0, 1.0);
                let sub = CastContext {
                    damage_mult: ctx.damage_mult * scale,
                    ..ctx.clone()
                };
                eval(world, &sub, then, Target::Entity(id), budget, events);
            }
        }
        EffectOp::Field { radius, duration_s, interval_s, faction, tick } => {
            let center = target_point(world, ctx, target);
            world.spawn_field(
                ctx.clone(),
                center,
                *radius,
                *faction,
                *duration_s,
                *interval_s,
                (**tick).clone(),
            );
        }
        EffectOp::Projectile { speed, gravity, radius, lifetime_s, homing, on_hit } => {
            world.spawn_spell_projectile(
                ctx.clone(),
                *speed,
                *gravity,
                *radius,
                *lifetime_s,
                *homing,
                (**on_hit).clone(),
            );
        }

        // ---- effect ops ----
        EffectOp::Damage { amount, element } => {
            if let Some(victim) = target_entity(ctx, target) {
                world.spell_damage(ctx.caster, victim, amount * ctx.damage_mult, element, events);
            }
        }
        EffectOp::Heal { amount } => {
            let who = target_entity(ctx, target).unwrap_or(ctx.caster);
            let healed = amount * ctx.damage_mult;
            world.heal_entity(who, healed);
            // Feedback: floating restore motes + a soft green flash on the healed one.
            events.push(GameEvent::Heal { target: who, amount: healed });
        }
        EffectOp::Shield { amount, duration_s } => {
            let who = target_entity(ctx, target).unwrap_or(ctx.caster);
            world.add_shield(who, amount * ctx.damage_mult, ctx.tick + secs_to_ticks(*duration_s));
        }
        EffectOp::ApplyStatus { status, duration_s, stacks } => {
            if let Some(victim) = target_entity(ctx, target) {
                world.apply_status_to(victim, status, *duration_s, *stacks, ctx.tick, ctx.caster);
                // Feedback: buff bloom (gold) vs debuff pulse (sickly), by polarity.
                let beneficial = world.status_beneficial(status);
                events.push(GameEvent::Buff { entity: victim, beneficial });
            }
        }
        EffectOp::Impulse { force, vertical_bias } => {
            if let Some(victim) = target_entity(ctx, target) {
                world.apply_impulse(victim, ctx, *force, *vertical_bias, events);
            }
        }
        EffectOp::Vortex { strength, vertical_bias } => {
            // Radial force relative to the *current cast centre* (ctx.origin): the
            // impact point of a projectile, a field's centre, or the caster's aim.
            // Positive pulls inward (gravity well), negative shoves outward (blast).
            if let Some(victim) = target_entity(ctx, target) {
                world.apply_vortex(victim, ctx.origin, *strength, *vertical_bias, events);
            }
        }
        EffectOp::Teleport { max_distance, to_target } => {
            let dir = if *to_target {
                let p = target_point(world, ctx, target);
                (p - ctx.origin).normalize_or_zero()
            } else {
                ctx.dir
            };
            world.teleport_entity(ctx.caster, dir, *max_distance);
        }
        EffectOp::Summon { mob, count, duration_s } => {
            summon(world, ctx, mob, *count, *duration_s, events);
        }
        EffectOp::RestoreMana { amount } => {
            let who = target_entity(ctx, target).unwrap_or(ctx.caster);
            world.restore_mana(who, *amount);
        }
        EffectOp::Mark { tag, duration_s } => {
            let who = target_entity(ctx, target).unwrap_or(ctx.caster);
            world.set_mark(who, tag.clone(), ctx.tick + secs_to_ticks(*duration_s));
        }

        // ---- control ops ----
        EffectOp::Sequence(children) | EffectOp::Parallel(children) => {
            // Parallel has no ordering guarantees; same-tick sequential is a valid
            // (and deterministic) realisation of "simultaneous".
            for child in children {
                eval(world, ctx, child, target, budget, events);
            }
        }
        EffectOp::Delay { secs, then } => {
            let run_tick = ctx.tick + secs_to_ticks(*secs).max(1);
            world.schedule_effect(run_tick, ctx.clone(), (**then).clone(), target);
        }
        EffectOp::Repeat { count, interval_s, op } => {
            let step = secs_to_ticks(*interval_s);
            for k in 0..*count {
                if k == 0 {
                    eval(world, ctx, op, target, budget, events);
                } else {
                    let run_tick = ctx.tick + step.max(1) * k as u32;
                    world.schedule_effect(run_tick, ctx.clone(), (**op).clone(), target);
                }
            }
        }
        EffectOp::Chance { chance, then } => {
            // Fold the remaining op budget into the seed so multiple Chance ops in
            // one cast roll independently yet deterministically.
            let seed = combat::hash_seed(ctx.tick, ctx.caster, ctx.salt ^ budget.ops_left as u64);
            if combat::unit_f32(seed) < *chance {
                eval(world, ctx, then, target, budget, events);
            }
        }
        EffectOp::IfMarked { tag, then, otherwise } => {
            let who = target_entity(ctx, target).unwrap_or(ctx.caster);
            if world.has_mark(who, tag, ctx.tick) {
                eval(world, ctx, then, target, budget, events);
            } else {
                eval(world, ctx, otherwise, target, budget, events);
            }
        }
        EffectOp::Noop => {}
    }
}

/// Spawn `count` summoned mobs near the caster, owned by the caster's team.
fn summon(
    world: &mut World,
    ctx: &CastContext,
    mob: &MobId,
    count: u8,
    duration_s: f32,
    events: &mut Vec<GameEvent>,
) {
    let expire = ctx.tick + secs_to_ticks(duration_s);
    for i in 0..count {
        // Fan the spawns out a little so they don't stack exactly.
        let seed = combat::hash_seed(ctx.tick, ctx.caster, ctx.salt ^ i as u64);
        let angle = combat::unit_f32(seed) * std::f32::consts::TAU;
        let offset = Vec3::new(angle.cos(), 0.0, angle.sin()) * 1.5;
        if let Some(id) = world.spawn_summon(ctx.caster, ctx.caster_team, mob, ctx.origin + offset, expire) {
            if let Some(pos) = world.pos_of(id) {
                events.push(GameEvent::Spawn {
                    entity: id,
                    pos,
                    team: ctx.caster_team,
                });
            }
        }
    }
}

/// Resolve a [`Target`] to an entity id where one is implied.
fn target_entity(ctx: &CastContext, target: Target) -> Option<EntityId> {
    match target {
        Target::SelfCaster => Some(ctx.caster),
        Target::Entity(id) => Some(id),
        Target::Point(_) => None,
    }
}

/// Resolve a [`Target`] to a world point.
fn target_point(world: &World, ctx: &CastContext, target: Target) -> Vec3 {
    match target {
        Target::SelfCaster => world.pos_of(ctx.caster).unwrap_or(ctx.origin),
        Target::Point(p) => p,
        Target::Entity(id) => world.pos_of(id).unwrap_or(ctx.origin),
    }
}

/// Convert seconds to whole sim ticks (rounded).
pub fn secs_to_ticks(secs: f32) -> u32 {
    (secs * TICK_HZ as f32).round().max(0.0) as u32
}

/// A stable pseudo "weapon" id for the Shot event, derived from the spell element so
/// the client can colour the tracer. The protocol field is a `u8`; we hash into it.
fn spell_event_weapon(spell: &SpellDef) -> u8 {
    (combat::str_salt(spell.element.as_str()) & 0xff) as u8
}
