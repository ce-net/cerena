//! The [`World`] aggregate: the one object an authority advances each tick.
//!
//! In Cerena the world is a first-person procedural mage RPG. `World` owns every
//! mutable piece of a zone: entities, per-character [`RpgState`]/[`Inventory`],
//! status effects, mana/stamina, the action bar, in-flight spell projectiles,
//! persistent fields, scheduled (delayed/repeated) effects, ground loot, and
//! anti-cheat telemetry. It also holds the hot-reloadable [`ContentRegistry`] and
//! swaps a staged pack at the top of [`World::tick`] so a live edit never tears a
//! half-simulated tick.
//!
//! The public surface (`new`/`spawn_player`/`remove_entity`/`set_input`/`tick`/
//! `entities`/`combat_view`/`state_hash`/`take_telemetry`, `World: Clone`) is the
//! contract `arena-net` and `arena-server` bind to. The one signature change from
//! the FPS prototype is [`World::new`], which now also takes a [`ContentRegistry`].

use std::collections::{HashMap, HashSet, VecDeque};

use glam::Vec3;
use sha2::{Digest, Sha256};

use arena_content::ids::{AbilityId, ElementId, ItemId, MobId, SpellId, StatusId};
use arena_content::item::StatMods;
use arena_content::movement::{MovementKind, MovementModeDef};
use arena_content::registry::ContentRegistry;
use arena_content::spell::{EffectOp, Faction, SpellDef, Target};
use arena_content::status::StatusKind;
use arena_content::tech::TechEffect;

use arena_protocol::entity::{EntityFlags, EntityKind, EntityState};
use arena_protocol::input::{Buttons, InputFrame};
use arena_protocol::snapshot::GameEvent;
use arena_protocol::world::Team;
use arena_protocol::{EntityId, NodeId, TICK_DT, TICK_HZ, Tick};

use crate::combat;
use crate::magic::{self, CastContext};
use crate::map::MapDef;
use crate::movement::{self, MoveParams, MovementRuntime};
use crate::rpg::RpgState;
use crate::inventory::Inventory;

/// Respawn delay after death (3 seconds).
pub const RESPAWN_TICKS: u32 = 3 * TICK_HZ;
/// How close a player must be to ground loot to collect it (metres).
pub const PICKUP_RADIUS: f32 = 1.8;
/// Look-angle change (radians) per tick beyond which we record an aim-snap prior.
pub const MAX_HUMAN_LOOK_RAD_PER_TICK: f32 = 0.9;
/// Horizontal speed clamp; exceeding it is a movement-exploit prior. Generous to
/// allow dashes/grapples.
pub const MAX_PLAUSIBLE_SPEED: f32 = 40.0;
/// XP dropped on death per character level.
pub const DROP_XP_PER_LEVEL: u64 = 20;
/// Fraction of dropped XP that stays bound to the victim (1/N).
pub const BOUND_XP_DIVISOR: u64 = 5;

/// Anti-cheat counters accumulated by the sim for one player, drained by
/// [`World::take_telemetry`]. `arena-karma` maps these onto the protocol's
/// `CheatTelemetry`. In the mage game, `shots_*` count casts, and
/// `firerate_violations` counts casts that beat the cooldown/mana gate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheatCounters {
    pub shots_fired: u32,
    pub shots_hit: u32,
    pub headshots: u32,
    pub aim_snap_events: u32,
    pub move_corrections: u32,
    pub firerate_violations: u32,
}

#[derive(Debug, Clone, Copy, Default)]
struct TelemetryAccumulator {
    shots_fired: u32,
    shots_hit: u32,
    headshots: u32,
    aim_snap_events: u32,
    move_corrections: u32,
    firerate_violations: u32,
}

/// One advance of the simulation.
#[derive(Debug, Clone, Default)]
pub struct TickReport {
    pub tick: Tick,
    pub events: Vec<GameEvent>,
    /// `(killer, victim)` kills this tick.
    pub deaths: Vec<(EntityId, EntityId)>,
}

/// A live status effect instance on an entity.
#[derive(Debug, Clone)]
struct StatusInstance {
    id: StatusId,
    /// Who applied it (for kill credit on DoT).
    source: EntityId,
    expire_tick: Tick,
    /// Next tick a periodic effect (DoT/regen/mana-burn) fires.
    next_tick: Tick,
    interval: u32,
    stacks: u8,
}

/// A travelling spell projectile carrying its on-impact continuation.
#[derive(Debug, Clone)]
struct ProjectileState {
    ctx: CastContext,
    on_hit: EffectOp,
    gravity: f32,
    homing: f32,
    radius: f32,
    expire_tick: Tick,
}

/// A persistent spell field that re-runs its `tick_op` on an interval.
#[derive(Debug, Clone)]
struct FieldState {
    ctx: CastContext,
    center: Vec3,
    radius: f32,
    faction: Faction,
    expire_tick: Tick,
    next_tick: Tick,
    interval: u32,
    tick_op: EffectOp,
}

/// A delayed / repeated effect waiting for its run tick.
#[derive(Debug, Clone)]
struct ScheduledEffect {
    run_tick: Tick,
    ctx: CastContext,
    op: EffectOp,
    target: Target,
}

/// Loot dropped by a fallen player, collectible by walking over it.
#[derive(Debug, Clone)]
struct LootPayload {
    items: Vec<(ItemId, u16)>,
    xp: u64,
    ability: Option<AbilityId>,
}

/// Runtime bookkeeping for a summoned creature.
#[derive(Debug, Clone)]
struct MobRuntime {
    expire_tick: Tick,
    xp_reward: u32,
}

/// One tick of position history (kept short; reserved for future lag-comp use).
#[derive(Debug, Clone)]
struct HistoryFrame {
    tick: Tick,
    positions: HashMap<EntityId, Vec3>,
}

/// The full mutable state of one zone simulation.
pub struct World {
    pub map: MapDef,
    content: ContentRegistry,
    entities: HashMap<EntityId, EntityState>,
    rpg: HashMap<EntityId, RpgState>,
    inventory: HashMap<EntityId, Inventory>,
    statuses: HashMap<EntityId, Vec<StatusInstance>>,
    /// Temporary shield HP that absorbs before health: `(amount, expire_tick)`.
    shields: HashMap<EntityId, (f32, Tick)>,
    /// Combo / synergy marks: tag -> expire tick.
    marks: HashMap<EntityId, HashMap<String, Tick>>,
    /// The player's ordered action bar of abilities (FIRE casts the selected slot).
    ability_bar: HashMap<EntityId, Vec<AbilityId>>,
    /// Per-ability cooldown ready-ticks.
    ability_cooldowns: HashMap<EntityId, HashMap<AbilityId, Tick>>,
    /// Movement-mode cooldown ready-ticks (keyed by mode id string).
    move_cooldowns: HashMap<EntityId, HashMap<String, Tick>>,
    move_runtime: HashMap<EntityId, MovementRuntime>,
    projectiles: HashMap<EntityId, ProjectileState>,
    fields: Vec<FieldState>,
    scheduled: Vec<ScheduledEffect>,
    loot: HashMap<EntityId, LootPayload>,
    mobs: HashMap<EntityId, MobRuntime>,
    /// Who last damaged each entity, for kill credit.
    last_attacker: HashMap<EntityId, EntityId>,
    inputs: HashMap<EntityId, InputFrame>,
    /// Buttons held last tick, for edge detection.
    prev_buttons: HashMap<EntityId, u16>,
    telemetry: HashMap<EntityId, TelemetryAccumulator>,
    respawn_at: HashMap<EntityId, Tick>,
    history: VecDeque<HistoryFrame>,
    tick: Tick,
    next_id: EntityId,
}

impl Clone for World {
    fn clone(&self) -> Self {
        // ContentRegistry is not `Clone`; rebuild it from the active pack (a staged-
        // but-unapplied swap is intentionally dropped on a snapshot clone).
        let content = ContentRegistry::new(self.content.epoch, self.content.pack().clone())
            .unwrap_or_else(|_| ContentRegistry::bootstrap());
        World {
            map: self.map.clone(),
            content,
            entities: self.entities.clone(),
            rpg: self.rpg.clone(),
            inventory: self.inventory.clone(),
            statuses: self.statuses.clone(),
            shields: self.shields.clone(),
            marks: self.marks.clone(),
            ability_bar: self.ability_bar.clone(),
            ability_cooldowns: self.ability_cooldowns.clone(),
            move_cooldowns: self.move_cooldowns.clone(),
            move_runtime: self.move_runtime.clone(),
            projectiles: self.projectiles.clone(),
            fields: self.fields.clone(),
            scheduled: self.scheduled.clone(),
            loot: self.loot.clone(),
            mobs: self.mobs.clone(),
            last_attacker: self.last_attacker.clone(),
            inputs: self.inputs.clone(),
            prev_buttons: self.prev_buttons.clone(),
            telemetry: self.telemetry.clone(),
            respawn_at: self.respawn_at.clone(),
            history: self.history.clone(),
            tick: self.tick,
            next_id: self.next_id,
        }
    }
}

impl World {
    /// Build an empty world on `map` driven by `content` (the active content pack).
    pub fn new(map: MapDef, content: ContentRegistry) -> World {
        World {
            map,
            content,
            entities: HashMap::new(),
            rpg: HashMap::new(),
            inventory: HashMap::new(),
            statuses: HashMap::new(),
            shields: HashMap::new(),
            marks: HashMap::new(),
            ability_bar: HashMap::new(),
            ability_cooldowns: HashMap::new(),
            move_cooldowns: HashMap::new(),
            move_runtime: HashMap::new(),
            projectiles: HashMap::new(),
            fields: Vec::new(),
            scheduled: Vec::new(),
            loot: HashMap::new(),
            mobs: HashMap::new(),
            last_attacker: HashMap::new(),
            inputs: HashMap::new(),
            prev_buttons: HashMap::new(),
            telemetry: HashMap::new(),
            respawn_at: HashMap::new(),
            history: VecDeque::with_capacity(8),
            tick: 0,
            next_id: 1,
        }
    }

    /// Read-only access to all entities, for the snapshot/replication layer.
    pub fn entities(&self) -> &HashMap<EntityId, EntityState> {
        &self.entities
    }

    pub fn current_tick(&self) -> Tick {
        self.tick
    }

    /// How a live content edit reaches the sim: stage a new pack for the next tick
    /// boundary. [`World::tick`] applies it before simulating, so the swap is atomic.
    pub fn stage_content(
        &mut self,
        epoch: u64,
        pack: arena_content::ContentPack,
    ) -> Result<(), arena_content::ContentError> {
        self.content.stage(epoch, pack)
    }

    /// Spawn a player for `owner` on `team`, with the starter loadout from content.
    pub fn spawn_player(&mut self, owner: NodeId, team: Team) -> EntityId {
        let enemies = self.enemy_positions(team);
        let seed = self.next_id as u64;
        let spawn = self.map.pick_spawn(team, &enemies, seed);

        let id = self.next_id;
        self.next_id += 1;

        // Starter loadout from the active pack (degrades gracefully if absent).
        let mut inv = Inventory::default();
        for item in ["item.novice_robe", "item.ember_staff", "item.swiftboots", "item.mana_crystal"] {
            let iid = ItemId::new(item);
            if self.content.item(&iid).is_some() {
                inv.add_item(iid.clone(), if item == "item.mana_crystal" { 3 } else { 1 });
                inv.equip(&self.content, &iid);
            }
        }
        let ability_bar = inv.granted_abilities(&self.content);

        let rpg = RpgState::default();
        let mods = inv.aggregate_mods(&self.content);
        let derived = rpg.derived(&mods);

        let mut flags = EntityFlags::default();
        flags.set(EntityFlags::ON_GROUND, true);
        let state = EntityState {
            id,
            kind: EntityKind::Player,
            pos: spawn.pos + Vec3::Y * movement::STAND_HALF_HEIGHT,
            vel: Vec3::ZERO,
            yaw: spawn.yaw,
            pitch: 0.0,
            flags,
            team,
            health: derived.max_health.round() as i16,
            armor: 0,
            weapon: 0,
            owner,
        };

        self.entities.insert(id, state);
        self.rpg.insert(id, rpg);
        self.inventory.insert(id, inv);
        self.ability_bar.insert(id, ability_bar);
        self.move_runtime.insert(id, MovementRuntime::default());
        self.telemetry.insert(id, TelemetryAccumulator::default());
        id
    }

    /// Remove an entity and all its bookkeeping.
    pub fn remove_entity(&mut self, id: EntityId) {
        self.entities.remove(&id);
        self.rpg.remove(&id);
        self.inventory.remove(&id);
        self.statuses.remove(&id);
        self.shields.remove(&id);
        self.marks.remove(&id);
        self.ability_bar.remove(&id);
        self.ability_cooldowns.remove(&id);
        self.move_cooldowns.remove(&id);
        self.move_runtime.remove(&id);
        self.projectiles.remove(&id);
        self.loot.remove(&id);
        self.mobs.remove(&id);
        self.last_attacker.remove(&id);
        self.inputs.remove(&id);
        self.prev_buttons.remove(&id);
        self.telemetry.remove(&id);
        self.respawn_at.remove(&id);
    }

    /// Accept a sanitised input frame; record an aim-snap prior on an inhuman flick.
    pub fn set_input(&mut self, id: EntityId, frame: InputFrame) {
        let frame = frame.sanitized();
        if let Some(prev) = self.inputs.get(&id) {
            if frame.look_delta(prev) > MAX_HUMAN_LOOK_RAD_PER_TICK {
                if let Some(t) = self.telemetry.get_mut(&id) {
                    t.aim_snap_events += 1;
                }
            }
        }
        self.inputs.insert(id, frame);
    }

    /// HUD view: `(mana, max_mana, respawn_at)`. Mana repurposes the old ammo fields
    /// so the wire snapshot is unchanged.
    pub fn combat_view(&self, id: EntityId) -> Option<(u16, u16, Tick)> {
        let rpg = self.rpg.get(&id)?;
        let respawn = self.respawn_at.get(&id).copied().unwrap_or(0);
        Some((
            rpg.mana.round().max(0.0) as u16,
            rpg.max_mana.round().max(0.0) as u16,
            respawn,
        ))
    }

    /// Drain and reset a player's anti-cheat counters.
    pub fn take_telemetry(&mut self, id: EntityId) -> CheatCounters {
        match self.telemetry.get_mut(&id) {
            Some(a) => {
                let out = CheatCounters {
                    shots_fired: a.shots_fired,
                    shots_hit: a.shots_hit,
                    headshots: a.headshots,
                    aim_snap_events: a.aim_snap_events,
                    move_corrections: a.move_corrections,
                    firerate_violations: a.firerate_violations,
                };
                *a = TelemetryAccumulator::default();
                out
            }
            None => CheatCounters::default(),
        }
    }

    // ======================================================================
    // The tick pipeline
    // ======================================================================

    pub fn tick(&mut self) -> TickReport {
        // 0. Hot-swap content at the tick boundary so a live edit is atomic.
        self.content.apply_pending();

        let prev = self.tick;
        self.tick = prev + 1;
        let now = self.tick;
        self.record_history(prev);

        let mut events: Vec<GameEvent> = Vec::new();
        let mut deaths: Vec<(EntityId, EntityId)> = Vec::new();

        let player_ids: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| e.kind == EntityKind::Player)
            .map(|(id, _)| *id)
            .collect();

        // 1. Regen pools + tick status effects (DoT/regen/mana-burn + expiry).
        self.regen_pools(&player_ids);
        self.tick_statuses(now, &player_ids, &mut events);

        // 2. Movement (base locomotion + status modifiers + parkour modes).
        self.movement_step(now, &player_ids, &mut events);

        // 3. Casting from inputs (FIRE = primary, ALT_FIRE = secondary, USE = item).
        self.cast_step(now, &player_ids, &mut events);

        // 4. Projectiles: integrate, run on-impact continuations.
        self.integrate_projectiles(now, &mut events);

        // 5. Persistent fields re-run their effect on interval.
        self.tick_fields(now, &mut events);

        // 6. Delayed / repeated effects whose time has come.
        self.run_scheduled(now, &mut events);

        // 7. Deaths -> kill credit + loot drop (and summon/mob cleanup).
        self.process_deaths(now, &mut events, &mut deaths);

        // 8. Loot pickup by proximity.
        self.pickup_loot(&mut events);

        // 9. Expire timed entities (summons/projectiles handled inline); respawn.
        self.expire_summons(now);
        self.process_respawns(now, &player_ids, &mut events);

        // 10. Remember this tick's buttons for next-tick edge detection.
        for id in &player_ids {
            let b = self.inputs.get(id).map(|f| f.buttons.0).unwrap_or(0);
            self.prev_buttons.insert(*id, b);
        }

        TickReport { tick: now, events, deaths }
    }

    /// Deterministic 32-byte fingerprint for cross-validation. Visits entities in
    /// sorted-id order and rounds floats to 1e-3 so platform jitter cannot break
    /// agreement.
    pub fn state_hash(&self) -> [u8; 32] {
        fn q(v: f32) -> i64 {
            (v * 1000.0).round() as i64
        }
        let mut ids: Vec<EntityId> = self.entities.keys().copied().collect();
        ids.sort_unstable();
        let mut h = Sha256::new();
        h.update(self.tick.to_le_bytes());
        for id in ids {
            let e = &self.entities[&id];
            h.update(id.to_le_bytes());
            h.update([e.kind as u8, e.team as u8, e.weapon]);
            for v in [e.pos.x, e.pos.y, e.pos.z, e.vel.x, e.vel.y, e.vel.z, e.yaw, e.pitch] {
                h.update(q(v).to_le_bytes());
            }
            h.update(e.health.to_le_bytes());
            h.update(e.armor.to_le_bytes());
            h.update(e.flags.0.to_le_bytes());
            if let Some(r) = self.rpg.get(&id) {
                h.update(r.level.to_le_bytes());
                h.update(q(r.mana).to_le_bytes());
                h.update(q(r.stamina).to_le_bytes());
            }
            let nstatus = self.statuses.get(&id).map(|s| s.len()).unwrap_or(0) as u32;
            h.update(nstatus.to_le_bytes());
            if let Some((amt, _)) = self.shields.get(&id) {
                h.update(q(*amt).to_le_bytes());
            }
        }
        let digest = h.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }

    // ======================================================================
    // Pipeline helpers
    // ======================================================================

    fn record_history(&mut self, tick: Tick) {
        let positions = self
            .entities
            .iter()
            .filter(|(_, e)| e.kind == EntityKind::Player)
            .map(|(id, e)| (*id, e.pos))
            .collect();
        self.history.push_back(HistoryFrame { tick, positions });
        while self.history.len() > 8 {
            self.history.pop_front();
        }
    }

    fn regen_pools(&mut self, player_ids: &[EntityId]) {
        for id in player_ids {
            // Summons have no RPG state.
            let Some(mods) = self.combined_mods(*id) else { continue };
            if let Some(rpg) = self.rpg.get_mut(id) {
                let derived = rpg.derived(&mods);
                rpg.regen(TICK_DT, &derived);
            }
        }
    }

    fn tick_statuses(&mut self, now: Tick, player_ids: &[EntityId], events: &mut Vec<GameEvent>) {
        for id in player_ids {
            // Take the list out so we can mutate the world while processing it.
            let mut list = match self.statuses.remove(id) {
                Some(l) => l,
                None => continue,
            };
            // Collected periodic actions to apply after the list is back in place.
            let mut dmg: Vec<(EntityId, f32, ElementId)> = Vec::new();
            let mut heal = 0.0f32;
            let mut mana_burn = 0.0f32;

            list.retain_mut(|inst| {
                if now >= inst.expire_tick {
                    return false;
                }
                if inst.interval > 0 {
                    let def_kind = self.content.status(&inst.id).map(|d| d.kind.clone());
                    while now >= inst.next_tick {
                        if let Some(kind) = &def_kind {
                            let secs = inst.interval as f32 * TICK_DT;
                            match kind {
                                StatusKind::DamageOverTime { element, dps } => {
                                    dmg.push((inst.source, dps * secs * inst.stacks as f32, element.clone()));
                                }
                                StatusKind::Burning { dps } => {
                                    dmg.push((inst.source, dps * secs * inst.stacks as f32, ElementId::new("fire")));
                                }
                                StatusKind::Regen { hps } => heal += hps * secs * inst.stacks as f32,
                                StatusKind::ManaBurn { mps } => mana_burn += mps * secs * inst.stacks as f32,
                                _ => {}
                            }
                        }
                        inst.next_tick += inst.interval;
                    }
                }
                true
            });

            self.statuses.insert(*id, list);

            // Apply the accumulated periodic effects.
            for (source, amount, element) in dmg {
                self.spell_damage(source, *id, amount, &element, events);
            }
            if heal > 0.0 {
                self.heal_entity(*id, heal);
            }
            if mana_burn > 0.0 {
                if let Some(rpg) = self.rpg.get_mut(id) {
                    rpg.mana = (rpg.mana - mana_burn).max(0.0);
                }
            }
        }
    }

    fn movement_step(&mut self, now: Tick, player_ids: &[EntityId], events: &mut Vec<GameEvent>) {
        let brushes = self.map.brushes.clone();
        for id in player_ids {
            let alive = self.entities.get(id).map(|e| e.is_alive()).unwrap_or(false);
            if !alive {
                continue;
            }
            let frame = self.input_for(*id);
            let prev = self.prev_buttons.get(id).copied().unwrap_or(0);
            let on_ground_prev = self
                .entities
                .get(id)
                .map(|e| e.flags.has(EntityFlags::ON_GROUND))
                .unwrap_or(false);

            // Status-driven movement modifiers.
            let sm = self.movement_status_mods(*id);
            // Stat bonuses.
            let speed_bonus = self
                .combined_mods(*id)
                .and_then(|m| self.rpg.get(id).map(|r| r.derived(&m)))
                .map(|d| d.move_speed_bonus)
                .unwrap_or(0.0);

            // Resolve unlocked movement modes for this player.
            let modes = self.movement_modes_of(*id);

            // Continuous modes shape MoveParams.
            let sprint_mult = modes
                .iter()
                .find_map(|m| match &m.kind {
                    MovementKind::Sprint { speed_mult } => Some(*speed_mult),
                    _ => None,
                })
                .unwrap_or(movement::SPRINT_MULT);

            let mut gravity_mult = if sm.levitate { 0.0 } else { 1.0 };
            let airborne = !on_ground_prev;
            let falling = self.entities.get(id).map(|e| e.vel.y < 0.0).unwrap_or(false);
            // Glide: airborne, falling, holding jump, mode unlocked.
            if airborne && falling && frame.buttons.has(Buttons::JUMP) {
                if let Some(fall_mult) = modes.iter().find_map(|m| match &m.kind {
                    MovementKind::Glide { fall_mult, .. } => Some(*fall_mult),
                    _ => None,
                }) {
                    gravity_mult = gravity_mult.min(fall_mult);
                }
            }
            // Wall-run: airborne, against a wall, sprinting, mode unlocked.
            let touching_wall = self
                .entities
                .get(id)
                .map(|e| movement::half_height_of(e))
                .map(|hh| {
                    crate::collision::wall_contact(self.entities[id].pos, hh, movement::PLAYER_RADIUS, &brushes)
                        .is_some()
                })
                .unwrap_or(false);
            if airborne && touching_wall && frame.buttons.has(Buttons::SPRINT) {
                if let Some(gm) = modes.iter().find_map(|m| match &m.kind {
                    MovementKind::WallRun { gravity_mult, .. } => Some(*gravity_mult),
                    _ => None,
                }) {
                    gravity_mult = gravity_mult.min(gm);
                }
            }

            let params = MoveParams {
                sprint_mult,
                speed_bonus,
                speed_scale: sm.speed_scale,
                gravity_mult,
                rooted: sm.rooted,
            };

            // --- Pre-move discrete activations (edge-triggered) ---------------
            let jump_edge = frame.buttons.has(Buttons::JUMP) && (prev & Buttons::JUMP == 0);
            let crouch_edge = frame.buttons.has(Buttons::CROUCH) && (prev & Buttons::CROUCH == 0);
            let melee_edge = frame.buttons.has(Buttons::MELEE) && (prev & Buttons::MELEE == 0);
            let aim = self
                .entities
                .get(id)
                .map(|e| movement::view_dir(e.yaw, e.pitch))
                .unwrap_or(Vec3::Z);

            // Double jump on a mid-air jump press.
            if jump_edge && airborne {
                if let Some(def) = modes.iter().find(|m| matches!(m.kind, MovementKind::DoubleJump { .. })) {
                    self.try_activate_mode(*id, def, aim, on_ground_prev, &brushes, now, events);
                }
            }
            // Ground slam on a mid-air crouch press.
            if crouch_edge && airborne {
                if let Some(def) = modes.iter().find(|m| matches!(m.kind, MovementKind::GroundSlam { .. })) {
                    self.try_activate_mode(*id, def, aim, on_ground_prev, &brushes, now, events);
                }
            }
            // Slide on a grounded crouch press.
            if crouch_edge && !airborne {
                if let Some(def) = modes.iter().find(|m| matches!(m.kind, MovementKind::Slide { .. })) {
                    self.try_activate_mode(*id, def, aim, on_ground_prev, &brushes, now, events);
                }
            }
            // MELEE casts the selected movement mode (dash/blink/grapple/climb): the
            // "movement abilities via the slot" hook. weapon_slot selects which mode.
            if melee_edge && !modes.is_empty() {
                let sel = (frame.weapon_slot as usize) % modes.len();
                let def = modes[sel].clone();
                self.try_activate_mode(*id, &def, aim, on_ground_prev, &brushes, now, events);
            }

            // --- Base locomotion sweep ----------------------------------------
            let status = {
                let e = self.entities.get_mut(id).unwrap();
                movement::move_player(e, &frame, TICK_DT, on_ground_prev, &brushes, &params)
            };

            // Reset air jumps and resolve a pending ground-slam on landing.
            if status.on_ground {
                let slam = {
                    let rt = self.move_runtime.entry(*id).or_default();
                    rt.extra_jumps_used = 0;
                    let s = rt.slam_pending;
                    rt.slam_pending = false;
                    s
                };
                if slam {
                    if let Some(def) = modes.iter().find(|m| matches!(m.kind, MovementKind::GroundSlam { .. })) {
                        if let MovementKind::GroundSlam { damage, radius, .. } = def.kind {
                            self.ground_slam_burst(*id, damage, radius, events);
                        }
                    }
                }
            }

            // Speed-hack guard.
            let clamped = {
                let e = self.entities.get_mut(id).unwrap();
                let horiz = (e.vel.x * e.vel.x + e.vel.z * e.vel.z).sqrt();
                if horiz > MAX_PLAUSIBLE_SPEED {
                    let scale = MAX_PLAUSIBLE_SPEED / horiz;
                    e.vel.x *= scale;
                    e.vel.z *= scale;
                    true
                } else {
                    false
                }
            };
            if clamped {
                if let Some(t) = self.telemetry.get_mut(id) {
                    t.move_corrections += 1;
                }
            }
        }
    }

    /// Charge a movement mode's resources/cooldown and apply its kinematics.
    fn try_activate_mode(
        &mut self,
        id: EntityId,
        def: &MovementModeDef,
        aim: Vec3,
        on_ground: bool,
        brushes: &[arena_protocol::world::Aabb],
        now: Tick,
        _events: &mut Vec<GameEvent>,
    ) {
        let key = def.id.0.clone();
        let ready = self
            .move_cooldowns
            .get(&id)
            .and_then(|m| m.get(&key))
            .map_or(true, |&t| now >= t);
        if !ready {
            return;
        }
        // Resource check.
        let ok = {
            let Some(rpg) = self.rpg.get_mut(&id) else { return };
            if rpg.mana + 1e-3 < def.mana_cost || rpg.stamina + 1e-3 < def.stamina_cost {
                false
            } else {
                rpg.mana -= def.mana_cost;
                rpg.stamina -= def.stamina_cost;
                true
            }
        };
        if !ok {
            return;
        }
        let act = {
            let rt = self.move_runtime.entry(id).or_default();
            let e = self.entities.get_mut(&id).unwrap();
            movement::apply_mode(e, &def.kind, aim, on_ground, brushes, rt)
        };
        if act.used {
            let cd = magic::secs_to_ticks(def.cooldown).max(1);
            self.move_cooldowns.entry(id).or_default().insert(key, now + cd);
        }
    }

    /// Resolve a landed ground-slam as area damage around the slammer's feet.
    fn ground_slam_burst(&mut self, slammer: EntityId, damage: f32, radius: f32, events: &mut Vec<GameEvent>) {
        let Some(center) = self.pos_of(slammer) else { return };
        let team = self.team_of(slammer).unwrap_or(Team::None);
        events.push(GameEvent::Explosion { center, radius });
        let targets = self.sphere_targets(center, radius, Faction::Enemies, slammer, team);
        for (victim, _dist) in targets {
            let element = ElementId::new("earth");
            self.spell_damage(slammer, victim, damage, &element, events);
        }
    }

    fn cast_step(&mut self, now: Tick, player_ids: &[EntityId], events: &mut Vec<GameEvent>) {
        for id in player_ids {
            // Summons have no action bar.
            if !self.is_real_player(*id) {
                continue;
            }
            if self.entities.get(id).map(|e| !e.is_alive()).unwrap_or(true) {
                continue;
            }
            // Frozen / silenced cannot cast.
            if self.movement_status_mods(*id).silenced {
                continue;
            }
            let frame = self.input_for(*id);
            let prev = self.prev_buttons.get(id).copied().unwrap_or(0);
            // Selected slot on the entity, for replication.
            if let Some(e) = self.entities.get_mut(id) {
                e.weapon = frame.weapon_slot;
            }

            // Primary: the selected action-bar slot.
            if frame.buttons.has(Buttons::FIRE) {
                let edge = prev & Buttons::FIRE == 0;
                if let Some(ability) = self.selected_ability(*id, frame.weapon_slot) {
                    self.try_cast_ability(*id, &ability, now, edge, events);
                }
            }
            // Secondary: the ability bound to CastInput::Secondary.
            if frame.buttons.has(Buttons::ALT_FIRE) {
                let edge = prev & Buttons::ALT_FIRE == 0;
                if let Some(ability) = self.secondary_ability(*id) {
                    self.try_cast_ability(*id, &ability, now, edge, events);
                }
            }
            // USE consumes a consumable (potions), casting its on_use spell.
            let use_edge = frame.buttons.has(Buttons::USE) && (prev & Buttons::USE == 0);
            if use_edge {
                self.use_consumable(*id, now, events);
            }
        }
    }

    /// Validate cooldown + mana, then cast. Records anti-cheat priors on a blocked
    /// fresh attempt.
    fn try_cast_ability(
        &mut self,
        id: EntityId,
        ability_id: &AbilityId,
        now: Tick,
        edge: bool,
        events: &mut Vec<GameEvent>,
    ) {
        let Some(ability) = self.content.ability(ability_id).cloned() else { return };
        let Some(spell) = self.content.spell(&ability.spell).cloned() else { return };

        // Cooldown gate.
        let ready = self
            .ability_cooldowns
            .get(&id)
            .and_then(|m| m.get(ability_id))
            .map_or(true, |&t| now >= t);
        if !ready {
            if edge {
                self.bump_violation(id);
            }
            return;
        }

        // Mana gate. Channeled spells pay per second; others pay once.
        let cost = if spell.channeled { spell.mana_cost * TICK_DT } else { spell.mana_cost };
        let paid = self.rpg.get_mut(&id).map(|r| r.try_spend_mana(cost)).unwrap_or(false);
        if !paid {
            if edge {
                self.bump_violation(id);
            }
            return;
        }

        let (origin, dir) = {
            let e = self.entities.get(&id).unwrap();
            (movement::eye_position(e), movement::view_dir(e.yaw, e.pitch))
        };
        let mut produced = magic::cast(self, id, &spell, origin, dir, now);
        let hit = produced
            .iter()
            .any(|e| matches!(e, GameEvent::Hit { attacker, .. } if *attacker == id));
        if let Some(t) = self.telemetry.get_mut(&id) {
            t.shots_fired += 1;
            if hit {
                t.shots_hit += 1;
            }
        }
        events.append(&mut produced);

        // Set cooldown (instant/charged spells only; channeled re-pays each tick).
        if !spell.channeled {
            let cd_secs = ability.cooldown_override.unwrap_or(spell.cooldown);
            let cdr = self
                .combined_mods(id)
                .and_then(|m| self.rpg.get(&id).map(|r| r.derived(&m)))
                .map(|d| d.cooldown_reduction)
                .unwrap_or(0.0);
            let cd = magic::secs_to_ticks(cd_secs * (1.0 - cdr)).max(1);
            self.ability_cooldowns
                .entry(id)
                .or_default()
                .insert(ability_id.clone(), now + cd);
        }
    }

    fn use_consumable(&mut self, id: EntityId, now: Tick, events: &mut Vec<GameEvent>) {
        // Find the first carried consumable with an on_use spell.
        let chosen = {
            let Some(inv) = self.inventory.get(&id) else { return };
            inv.slots
                .iter()
                .map(|(iid, _)| iid.clone())
                .find(|iid| self.content.item(iid).map(|d| d.on_use.is_some()).unwrap_or(false))
        };
        let Some(item) = chosen else { return };
        // Resolve the on-use spell before mutating the bag (keeps borrows disjoint).
        let Some(spell_id) = self.content.item(&item).and_then(|d| d.on_use.clone()) else { return };
        let removed = self
            .inventory
            .get_mut(&id)
            .map(|inv| inv.remove_item(&item, 1))
            .unwrap_or(0);
        if removed == 0 {
            return;
        }
        let Some(spell) = self.content.spell(&spell_id).cloned() else { return };
        let (origin, dir) = {
            let e = self.entities.get(&id).unwrap();
            (movement::eye_position(e), movement::view_dir(e.yaw, e.pitch))
        };
        let mut produced = magic::cast(self, id, &spell, origin, dir, now);
        events.append(&mut produced);
    }

    fn integrate_projectiles(&mut self, now: Tick, events: &mut Vec<GameEvent>) {
        let brushes = self.map.brushes.clone();
        let ids: Vec<EntityId> = self.projectiles.keys().copied().collect();
        let mut remove: Vec<EntityId> = Vec::new();
        // Continuations to run after we finish mutating the projectile set.
        let mut detonations: Vec<(CastContext, EffectOp, Target, Vec3, f32)> = Vec::new();

        for pid in ids {
            let proj = self.projectiles[&pid].clone();
            let (old, mut vel) = match self.entities.get(&pid) {
                Some(e) => (e.pos, e.vel),
                None => {
                    remove.push(pid);
                    continue;
                }
            };
            // Gravity (the field is a positive "pull" magnitude).
            vel.y -= proj.gravity * TICK_DT;
            // Homing: steer toward the nearest enemy, conserving speed.
            if proj.homing > 0.0 {
                if let Some(tp) = self.nearest_enemy_pos(old, proj.ctx.caster_team, proj.ctx.caster) {
                    let speed = vel.length();
                    let desired = (tp - old).normalize_or_zero() * speed;
                    vel = (vel + (desired - vel) * (proj.homing * TICK_DT).clamp(0.0, 1.0))
                        .normalize_or_zero()
                        * speed;
                }
            }
            let step = vel * TICK_DT;
            let len = step.length();
            let dir = if len > 1e-6 { step / len } else { vel.normalize_or_zero() };

            // Find the nearest contact: world geometry or an enemy capsule.
            let mut contact_t = crate::collision::raycast_aabbs(old, dir, len.max(1e-4), &brushes).map(|(t, _)| t);
            let mut victim: Option<EntityId> = None;
            for (cid, _e) in self.entities.iter() {
                if *cid == proj.ctx.caster || self.entities[cid].kind != EntityKind::Player {
                    continue;
                }
                let e = &self.entities[cid];
                if !e.is_alive() || !combat::can_damage(proj.ctx.caster_team, e.team) {
                    continue;
                }
                let hh = movement::half_height_of(e);
                let base = e.pos - Vec3::Y * hh;
                if let Some((t, _, _)) = crate::collision::ray_capsule(old, dir, base, hh, movement::PLAYER_RADIUS + proj.radius) {
                    if t <= len && contact_t.map_or(true, |c| t < c) {
                        contact_t = Some(t);
                        victim = Some(*cid);
                    }
                }
            }

            let timed_out = now >= proj.expire_tick;
            if let Some(t) = contact_t {
                let impact = old + dir * t;
                let target = victim.map(Target::Entity).unwrap_or(Target::Point(impact));
                detonations.push((proj.ctx.clone(), proj.on_hit.clone(), target, impact, proj.radius));
                remove.push(pid);
            } else if timed_out {
                let impact = old;
                detonations.push((proj.ctx.clone(), proj.on_hit.clone(), Target::Point(impact), impact, proj.radius));
                remove.push(pid);
            } else if let Some(e) = self.entities.get_mut(&pid) {
                e.pos = old + step;
                e.vel = vel;
            }
        }

        for pid in remove {
            self.entities.remove(&pid);
            self.projectiles.remove(&pid);
        }
        for (mut ctx, op, target, impact, radius) in detonations {
            // Re-centre the cast context on the impact for downstream shape ops.
            ctx.origin = impact;
            events.push(GameEvent::Explosion { center: impact, radius: radius.max(0.25) });
            magic::run_op(self, &ctx, &op, target, events);
        }
    }

    fn tick_fields(&mut self, now: Tick, events: &mut Vec<GameEvent>) {
        // Collect due field runs while updating their schedule; drop expired.
        let mut due: Vec<(CastContext, EffectOp, Vec3)> = Vec::new();
        let mut keep: Vec<FieldState> = Vec::with_capacity(self.fields.len());
        for mut f in std::mem::take(&mut self.fields) {
            if now >= f.expire_tick {
                continue;
            }
            while now >= f.next_tick {
                let mut ctx = f.ctx.clone();
                ctx.origin = f.center;
                due.push((ctx, f.tick_op.clone(), f.center));
                f.next_tick += f.interval.max(1);
            }
            keep.push(f);
        }
        self.fields = keep;
        for (ctx, op, center) in due {
            magic::run_op(self, &ctx, &op, Target::Point(center), events);
        }
    }

    fn run_scheduled(&mut self, now: Tick, events: &mut Vec<GameEvent>) {
        let mut due: Vec<ScheduledEffect> = Vec::new();
        let mut keep: Vec<ScheduledEffect> = Vec::with_capacity(self.scheduled.len());
        for s in std::mem::take(&mut self.scheduled) {
            if now >= s.run_tick {
                due.push(s);
            } else {
                keep.push(s);
            }
        }
        self.scheduled = keep;
        for s in due {
            magic::run_op(self, &s.ctx, &s.op, s.target, events);
        }
    }

    fn process_deaths(&mut self, now: Tick, events: &mut Vec<GameEvent>, deaths: &mut Vec<(EntityId, EntityId)>) {
        let ids: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| e.kind == EntityKind::Player)
            .map(|(id, _)| *id)
            .collect();
        let mut remove: Vec<EntityId> = Vec::new();
        let mut loot_spawns: Vec<(Vec3, Team, LootPayload)> = Vec::new();

        for id in ids {
            let dead_now = {
                let Some(e) = self.entities.get_mut(&id) else { continue };
                if e.flags.has(EntityFlags::DEAD) || e.health > 0 {
                    continue;
                }
                e.health = 0;
                e.flags.set(EntityFlags::DEAD, true);
                e.flags.set(EntityFlags::FIRING, false);
                e.flags.set(EntityFlags::SPRINTING, false);
                true
            };
            if !dead_now {
                continue;
            }

            let killer = self.last_attacker.get(&id).copied().unwrap_or(id);
            let victim_node = self.entities.get(&id).map(|e| e.owner.clone()).unwrap_or_default();
            let killer_node = self.entities.get(&killer).map(|e| e.owner.clone()).unwrap_or_default();
            events.push(GameEvent::Death {
                victim: id,
                killer,
                weapon: 0,
                victim_node,
                killer_node,
            });
            deaths.push((killer, id));

            if let Some(mob) = self.mobs.get(&id).cloned() {
                // A summoned/AI mob: award XP to its killer and remove it.
                if let Some(rpg) = self.rpg.get_mut(&killer) {
                    rpg.grant_xp(mob.xp_reward as u64);
                }
                remove.push(id);
            } else if self.is_real_player(id) {
                // A real player: drop loot and start the respawn timer.
                let (pos, team) = self
                    .entities
                    .get(&id)
                    .map(|e| (e.pos, e.team))
                    .unwrap_or((Vec3::ZERO, Team::None));
                let payload = self.build_loot(id);
                loot_spawns.push((pos, team, payload));
                self.respawn_at.insert(id, now + RESPAWN_TICKS);
            }
        }

        for id in remove {
            self.remove_entity(id);
        }
        for (pos, team, payload) in loot_spawns {
            self.spawn_loot(pos, team, payload);
        }
    }

    /// Build the loot a dying player drops: half their carried stacks, a bound-XP
    /// chunk, and a reference to one of their abilities.
    fn build_loot(&mut self, victim: EntityId) -> LootPayload {
        let mut items = Vec::new();
        if let Some(inv) = self.inventory.get_mut(&victim) {
            // Drop every other carried stack (keep the rest).
            let keep: Vec<(ItemId, u16)> = inv.slots.iter().cloned().collect();
            inv.slots.clear();
            for (i, (item, qty)) in keep.into_iter().enumerate() {
                if i % 2 == 0 {
                    items.push((item, qty));
                } else {
                    inv.slots.push((item, qty));
                }
            }
        }
        let mut xp = 0;
        if let Some(rpg) = self.rpg.get_mut(&victim) {
            let drop = (rpg.level as u64) * DROP_XP_PER_LEVEL;
            let bound = drop / BOUND_XP_DIVISOR; // stays with the victim
            let lost = drop.saturating_sub(bound);
            // Victim forfeits the unbound portion of dropped XP.
            rpg.xp = rpg.xp.saturating_sub(lost);
            xp = lost;
        }
        let ability = self.ability_bar.get(&victim).and_then(|b| b.first().cloned());
        LootPayload { items, xp, ability }
    }

    fn spawn_loot(&mut self, pos: Vec3, team: Team, payload: LootPayload) {
        let id = self.next_id;
        self.next_id += 1;
        let mut flags = EntityFlags::default();
        flags.set(EntityFlags::ON_GROUND, true);
        self.entities.insert(
            id,
            EntityState {
                id,
                kind: EntityKind::Pickup,
                pos,
                vel: Vec3::ZERO,
                yaw: 0.0,
                pitch: 0.0,
                flags,
                team,
                health: 1,
                armor: 0,
                weapon: 0,
                owner: String::new(),
            },
        );
        self.loot.insert(id, payload);
    }

    fn pickup_loot(&mut self, events: &mut Vec<GameEvent>) {
        let loot_ids: Vec<EntityId> = self.loot.keys().copied().collect();
        for lid in loot_ids {
            let Some(lpos) = self.entities.get(&lid).map(|e| e.pos) else {
                self.loot.remove(&lid);
                continue;
            };
            // Nearest alive real player within reach collects it.
            let collector = self
                .entities
                .iter()
                .filter(|(cid, e)| {
                    e.kind == EntityKind::Player
                        && e.is_alive()
                        && self.inventory.contains_key(cid)
                        && !self.mobs.contains_key(cid)
                        && e.pos.distance(lpos) <= PICKUP_RADIUS
                })
                .map(|(cid, e)| (*cid, e.pos.distance_squared(lpos)))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(cid, _)| cid);

            if let Some(cid) = collector {
                let payload = self.loot.remove(&lid).unwrap();
                if let Some(inv) = self.inventory.get_mut(&cid) {
                    for (item, qty) in payload.items {
                        inv.add_item(item, qty);
                    }
                }
                if payload.xp > 0 {
                    if let Some(rpg) = self.rpg.get_mut(&cid) {
                        rpg.grant_xp(payload.xp);
                    }
                }
                if let Some(ab) = payload.ability {
                    let bar = self.ability_bar.entry(cid).or_default();
                    if !bar.contains(&ab) {
                        bar.push(ab);
                    }
                }
                events.push(GameEvent::PickupTaken { pickup: lid, by: cid });
                self.entities.remove(&lid);
            }
        }
    }

    fn expire_summons(&mut self, now: Tick) {
        let expired: Vec<EntityId> = self
            .mobs
            .iter()
            .filter(|(_, m)| now >= m.expire_tick)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            self.remove_entity(id);
        }
    }

    fn process_respawns(&mut self, now: Tick, player_ids: &[EntityId], events: &mut Vec<GameEvent>) {
        for id in player_ids {
            let respawn_at = self.respawn_at.get(id).copied().unwrap_or(0);
            let dead = self
                .entities
                .get(id)
                .map(|e| e.flags.has(EntityFlags::DEAD))
                .unwrap_or(false);
            if !dead || respawn_at == 0 || now < respawn_at || !self.is_real_player(*id) {
                continue;
            }
            let team = self.entities[id].team;
            let enemies = self.enemy_positions(team);
            let spawn = self.map.pick_spawn(team, &enemies, (now as u64) ^ (*id as u64));
            let new_pos = spawn.pos + Vec3::Y * movement::STAND_HALF_HEIGHT;

            let max_health = self
                .combined_mods(*id)
                .and_then(|m| self.rpg.get(id).map(|r| r.derived(&m)))
                .map(|d| d.max_health)
                .unwrap_or(100.0);
            {
                let e = self.entities.get_mut(id).unwrap();
                e.pos = new_pos;
                e.vel = Vec3::ZERO;
                e.yaw = spawn.yaw;
                e.pitch = 0.0;
                e.health = max_health.round() as i16;
                e.armor = 0;
                e.flags = EntityFlags::default();
                e.flags.set(EntityFlags::ON_GROUND, true);
            }
            // Refill resources and clear transient combat state.
            if let Some(rpg) = self.rpg.get_mut(id) {
                rpg.mana = rpg.max_mana;
                rpg.stamina = rpg.max_stamina;
            }
            self.statuses.remove(id);
            self.shields.remove(id);
            self.respawn_at.remove(id);
            events.push(GameEvent::Spawn { entity: *id, pos: new_pos, team });
        }
    }

    // ======================================================================
    // World accessors / mutators used by the magic interpreter
    // ======================================================================

    pub fn team_of(&self, id: EntityId) -> Option<Team> {
        self.entities.get(&id).map(|e| e.team)
    }

    pub fn pos_of(&self, id: EntityId) -> Option<Vec3> {
        self.entities.get(&id).map(|e| e.pos)
    }

    /// The full magnitude multiplier for a spell cast by `caster`: its [`Scaling`]
    /// against the caster's attributes, times spell power.
    pub fn spell_damage_mult(&self, caster: EntityId, spell: &SpellDef) -> f32 {
        let Some(mods) = self.combined_mods(caster) else { return 1.0 };
        let Some(rpg) = self.rpg.get(&caster) else { return 1.0 };
        let d = rpg.derived(&mods);
        let s = &spell.scaling;
        let contrib = s.power * d.power * 0.01
            + s.focus * d.focus * 0.01
            + s.agility * d.agility * 0.01
            + s.level * (rpg.level as f32) * 0.02;
        d.spell_power * (1.0 + contrib)
    }

    /// Faction-filtered ray targets along the eye ray, up to `max_hits`, stopping at
    /// world geometry. Returns `(entity, hit_point)` nearest-first.
    pub fn ray_targets(
        &self,
        origin: Vec3,
        dir: Vec3,
        range: f32,
        max_hits: usize,
        faction: Faction,
        caster: EntityId,
        caster_team: Team,
    ) -> Vec<(EntityId, Vec3)> {
        let dir = dir.normalize_or_zero();
        let wall_t = crate::collision::raycast_aabbs(origin, dir, range, &self.map.brushes).map(|(t, _)| t);
        let mut hits: Vec<(f32, EntityId, Vec3)> = Vec::new();
        for (id, e) in &self.entities {
            if e.kind != EntityKind::Player || !e.is_alive() {
                continue;
            }
            if !faction_ok(faction, caster_team, caster, e.team, *id) {
                continue;
            }
            let hh = movement::half_height_of(e);
            let base = e.pos - Vec3::Y * hh;
            if let Some((t, point, _)) = crate::collision::ray_capsule(origin, dir, base, hh, movement::PLAYER_RADIUS) {
                if t <= range && wall_t.map_or(true, |w| t <= w) {
                    hits.push((t, *id, point));
                }
            }
        }
        hits.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        hits.into_iter().take(max_hits).map(|(_, id, p)| (id, p)).collect()
    }

    pub fn cone_targets(
        &self,
        origin: Vec3,
        dir: Vec3,
        range: f32,
        half_angle: f32,
        faction: Faction,
        caster: EntityId,
        caster_team: Team,
    ) -> Vec<EntityId> {
        let dir = dir.normalize_or_zero();
        let cos_lim = half_angle.cos();
        let mut out = Vec::new();
        for (id, e) in &self.entities {
            if e.kind != EntityKind::Player || !e.is_alive() {
                continue;
            }
            if !faction_ok(faction, caster_team, caster, e.team, *id) {
                continue;
            }
            let to = e.pos - origin;
            let dist = to.length();
            if dist > range || dist < 1e-4 {
                continue;
            }
            if to.normalize_or_zero().dot(dir) >= cos_lim {
                out.push(*id);
            }
        }
        out
    }

    pub fn sphere_targets(
        &self,
        center: Vec3,
        radius: f32,
        faction: Faction,
        caster: EntityId,
        caster_team: Team,
    ) -> Vec<(EntityId, f32)> {
        let mut out = Vec::new();
        for (id, e) in &self.entities {
            if e.kind != EntityKind::Player || !e.is_alive() {
                continue;
            }
            if !faction_ok(faction, caster_team, caster, e.team, *id) {
                continue;
            }
            let dist = e.pos.distance(center);
            if dist <= radius {
                out.push((*id, dist));
            }
        }
        out
    }

    pub fn spell_damage(
        &mut self,
        attacker: EntityId,
        victim: EntityId,
        amount: f32,
        element: &ElementId,
        events: &mut Vec<GameEvent>,
    ) {
        // Empower (attacker) and Vulnerable (victim) status multipliers.
        let dealt_mult = self.status_dealt_mult(attacker);
        let taken_mult = self.status_taken_mult(victim);
        let mut dmg = (amount * dealt_mult * taken_mult).max(0.0);
        let _ = element; // element drives resist math in a fuller build; reserved.

        // Temporary shield absorbs first.
        dmg = self.absorb_shield(victim, dmg);

        let point = self.entities.get(&victim).map(|e| e.pos).unwrap_or(Vec3::ZERO);
        if let Some(e) = self.entities.get_mut(&victim) {
            let before = e.health;
            combat::apply_armor_damage(dmg, &mut e.health, &mut e.armor);
            let real = (before - e.health).max(0) as f32;
            events.push(GameEvent::Hit {
                attacker,
                victim,
                damage: real,
                headshot: false,
                point,
            });
        }
        self.last_attacker.insert(victim, attacker);
    }

    pub fn heal_entity(&mut self, id: EntityId, amount: f32) {
        let max = self
            .combined_mods(id)
            .and_then(|m| self.rpg.get(&id).map(|r| r.derived(&m)))
            .map(|d| d.max_health)
            .unwrap_or(100.0);
        if let Some(e) = self.entities.get_mut(&id) {
            if e.is_alive() {
                e.health = ((e.health as f32 + amount).min(max)).round() as i16;
            }
        }
    }

    pub fn add_shield(&mut self, id: EntityId, amount: f32, expire: Tick) {
        let entry = self.shields.entry(id).or_insert((0.0, expire));
        entry.0 += amount;
        entry.1 = entry.1.max(expire);
    }

    /// Drain a target's shield to absorb `dmg`; returns the remaining damage.
    fn absorb_shield(&mut self, id: EntityId, dmg: f32) -> f32 {
        let mut remaining = dmg;
        let mut clear = false;
        if let Some((amount, _)) = self.shields.get_mut(&id) {
            if *amount > 0.0 {
                let take = dmg.min(*amount);
                *amount -= take;
                remaining = (dmg - take).max(0.0);
                clear = *amount <= 0.0;
            }
        }
        if clear {
            self.shields.remove(&id);
        }
        remaining
    }

    pub fn apply_status_to(
        &mut self,
        victim: EntityId,
        status_id: &StatusId,
        duration_s: f32,
        stacks: u8,
        tick: Tick,
        source: EntityId,
    ) {
        let Some(def) = self.content.status(status_id).cloned() else { return };
        let dur = if duration_s > 0.0 { duration_s } else { def.duration_default_s };
        let expire = tick + magic::secs_to_ticks(dur).max(1);
        let interval = magic::secs_to_ticks(def.tick_interval_s);

        // A Shielded status grants its shield amount immediately.
        if let StatusKind::Shielded { amount } = def.kind {
            self.add_shield(victim, amount, expire);
        }

        let list = self.statuses.entry(victim).or_default();
        // Refresh an existing instance of the same id (respecting max stacks).
        if let Some(inst) = list.iter_mut().find(|i| i.id == *status_id) {
            inst.expire_tick = expire;
            inst.stacks = (inst.stacks + stacks.max(1)).min(def.max_stacks.max(1));
        } else {
            list.push(StatusInstance {
                id: status_id.clone(),
                source,
                expire_tick: expire,
                next_tick: tick + interval.max(1),
                interval,
                stacks: stacks.max(1).min(def.max_stacks.max(1)),
            });
        }
    }

    pub fn apply_impulse(&mut self, victim: EntityId, ctx: &CastContext, force: f32, vertical_bias: f32) {
        let caster_pos = self.pos_of(ctx.caster).unwrap_or(ctx.origin);
        if let Some(e) = self.entities.get_mut(&victim) {
            // Positive force pushes along the cast direction; negative pulls toward
            // the caster (void grasp).
            let horiz = if force >= 0.0 {
                Vec3::new(ctx.dir.x, 0.0, ctx.dir.z).normalize_or_zero()
            } else {
                let to = caster_pos - e.pos;
                Vec3::new(to.x, 0.0, to.z).normalize_or_zero()
            };
            let mag = force.abs();
            e.vel += horiz * mag;
            e.vel.y += mag * vertical_bias;
        }
    }

    pub fn teleport_entity(&mut self, id: EntityId, dir: Vec3, max_distance: f32) {
        let brushes = self.map.brushes.clone();
        if let Some(e) = self.entities.get_mut(&id) {
            let hh = movement::half_height_of(e);
            e.pos = crate::collision::clamp_translation(
                e.pos,
                dir.normalize_or_zero() * max_distance,
                hh,
                movement::PLAYER_RADIUS,
                &brushes,
            );
        }
    }

    pub fn restore_mana(&mut self, id: EntityId, amount: f32) {
        if let Some(rpg) = self.rpg.get_mut(&id) {
            rpg.mana = (rpg.mana + amount).min(rpg.max_mana);
        }
    }

    pub fn set_mark(&mut self, id: EntityId, tag: String, expire: Tick) {
        self.marks.entry(id).or_default().insert(tag, expire);
    }

    pub fn has_mark(&self, id: EntityId, tag: &str, tick: Tick) -> bool {
        self.marks
            .get(&id)
            .and_then(|m| m.get(tag))
            .map_or(false, |&exp| tick < exp)
    }

    /// Spawn a summoned creature owned by `caster`'s team. Returns its entity id.
    pub fn spawn_summon(
        &mut self,
        _caster: EntityId,
        caster_team: Team,
        mob: &MobId,
        pos: Vec3,
        expire: Tick,
    ) -> Option<EntityId> {
        let def = self.content.mob(mob)?.clone();
        let id = self.next_id;
        self.next_id += 1;
        let mut flags = EntityFlags::default();
        flags.set(EntityFlags::ON_GROUND, true);
        self.entities.insert(
            id,
            EntityState {
                id,
                kind: EntityKind::Player, // summons share the player capsule/combat path
                pos,
                vel: Vec3::ZERO,
                yaw: 0.0,
                pitch: 0.0,
                flags,
                team: caster_team,
                health: def.max_health.round() as i16,
                armor: 0,
                weapon: 0,
                owner: String::new(),
            },
        );
        self.mobs.insert(id, MobRuntime { expire_tick: expire, xp_reward: def.xp_reward });
        Some(id)
    }

    pub fn spawn_spell_projectile(
        &mut self,
        ctx: CastContext,
        speed: f32,
        gravity: f32,
        radius: f32,
        lifetime_s: f32,
        homing: f32,
        on_hit: EffectOp,
    ) {
        let id = self.next_id;
        self.next_id += 1;
        let mut flags = EntityFlags::default();
        flags.set(EntityFlags::AIRBORNE, true);
        self.entities.insert(
            id,
            EntityState {
                id,
                kind: EntityKind::Projectile,
                pos: ctx.origin,
                vel: ctx.dir * speed,
                yaw: 0.0,
                pitch: 0.0,
                flags,
                team: ctx.caster_team,
                health: 1,
                armor: 0,
                weapon: 0,
                owner: String::new(),
            },
        );
        self.projectiles.insert(
            id,
            ProjectileState {
                expire_tick: ctx.tick + magic::secs_to_ticks(lifetime_s).max(1),
                ctx,
                on_hit,
                gravity,
                homing,
                radius,
            },
        );
    }

    pub fn spawn_field(
        &mut self,
        ctx: CastContext,
        center: Vec3,
        radius: f32,
        faction: Faction,
        duration_s: f32,
        interval_s: f32,
        tick_op: EffectOp,
    ) {
        let interval = magic::secs_to_ticks(interval_s).max(1);
        self.fields.push(FieldState {
            expire_tick: ctx.tick + magic::secs_to_ticks(duration_s).max(1),
            next_tick: ctx.tick + interval,
            ctx,
            center,
            radius,
            faction,
            interval,
            tick_op,
        });
    }

    pub fn schedule_effect(&mut self, run_tick: Tick, ctx: CastContext, op: EffectOp, target: Target) {
        self.scheduled.push(ScheduledEffect { run_tick, ctx, op, target });
    }

    // ======================================================================
    // Small internals
    // ======================================================================

    fn is_real_player(&self, id: EntityId) -> bool {
        self.inventory.contains_key(&id) && !self.mobs.contains_key(&id)
    }

    fn input_for(&self, id: EntityId) -> InputFrame {
        self.inputs.get(&id).copied().unwrap_or_else(|| {
            let (yaw, pitch, weapon) = self
                .entities
                .get(&id)
                .map(|e| (e.yaw, e.pitch, e.weapon))
                .unwrap_or((0.0, 0.0, 0));
            InputFrame {
                seq: 0,
                client_tick: 0,
                buttons: Buttons::default(),
                yaw,
                pitch,
                weapon_slot: weapon,
            }
        })
    }

    /// Equipped-item mods combined with unlocked-tech `StatMult` mods.
    fn combined_mods(&self, id: EntityId) -> Option<StatMods> {
        let inv = self.inventory.get(&id)?;
        let mut mods = inv.aggregate_mods(&self.content);
        if let Some(rpg) = self.rpg.get(&id) {
            for node in &self.content.pack().tech.nodes {
                if rpg.unlocked_tech.contains(&node.id) {
                    for e in &node.effects {
                        if let TechEffect::StatMult(m) = e {
                            mods = mods.combine(m);
                        }
                    }
                }
            }
        }
        Some(mods)
    }

    fn selected_ability(&self, id: EntityId, slot: u8) -> Option<AbilityId> {
        let bar = self.ability_bar.get(&id)?;
        if bar.is_empty() {
            return None;
        }
        Some(bar[(slot as usize) % bar.len()].clone())
    }

    fn secondary_ability(&self, id: EntityId) -> Option<AbilityId> {
        let bar = self.ability_bar.get(&id)?;
        bar.iter()
            .find(|aid| {
                self.content
                    .ability(aid)
                    .map(|a| matches!(a.binding, arena_content::ability::CastInput::Secondary))
                    .unwrap_or(false)
            })
            .cloned()
    }

    /// The movement-mode defs unlocked for a player (from equipped gear).
    fn movement_modes_of(&self, id: EntityId) -> Vec<MovementModeDef> {
        let Some(inv) = self.inventory.get(&id) else { return Vec::new() };
        let granted = inv.granted_movement(&self.content);
        let mut out = Vec::new();
        for mid in granted {
            if let Some(def) = self.content.pack().movement_modes.iter().find(|d| d.id == mid) {
                out.push(def.clone());
            }
        }
        out
    }

    fn nearest_enemy_pos(&self, from: Vec3, team: Team, caster: EntityId) -> Option<Vec3> {
        self.entities
            .iter()
            .filter(|(id, e)| {
                **id != caster
                    && e.kind == EntityKind::Player
                    && e.is_alive()
                    && faction_ok(Faction::Enemies, team, caster, e.team, **id)
            })
            .map(|(_, e)| (e.pos, e.pos.distance_squared(from)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(p, _)| p)
    }

    fn enemy_positions(&self, team: Team) -> Vec<Vec3> {
        self.entities
            .values()
            .filter(|e| e.kind == EntityKind::Player && e.is_alive() && e.team != team && combat::can_damage(team, e.team))
            .map(|e| e.pos)
            .collect()
    }

    fn bump_violation(&mut self, id: EntityId) {
        if let Some(t) = self.telemetry.get_mut(&id) {
            t.firerate_violations += 1;
        }
    }

    fn status_dealt_mult(&self, id: EntityId) -> f32 {
        let mut m = 1.0;
        if let Some(list) = self.statuses.get(&id) {
            for inst in list {
                if let Some(def) = self.content.status(&inst.id) {
                    if let StatusKind::Empower { frac } = def.kind {
                        m += frac * inst.stacks as f32;
                    }
                }
            }
        }
        m
    }

    fn status_taken_mult(&self, id: EntityId) -> f32 {
        let mut m = 1.0;
        if let Some(list) = self.statuses.get(&id) {
            for inst in list {
                if let Some(def) = self.content.status(&inst.id) {
                    if let StatusKind::Vulnerable { frac } = def.kind {
                        m += frac * inst.stacks as f32;
                    }
                }
            }
        }
        m
    }

    /// Status-derived movement modifiers for a player.
    fn movement_status_mods(&self, id: EntityId) -> MovementStatusMods {
        let mut out = MovementStatusMods {
            speed_scale: 1.0,
            rooted: false,
            silenced: false,
            levitate: false,
        };
        if let Some(list) = self.statuses.get(&id) {
            for inst in list {
                let Some(def) = self.content.status(&inst.id) else { continue };
                match def.kind {
                    StatusKind::Slow { frac } => out.speed_scale *= (1.0 - frac).max(0.0),
                    StatusKind::Haste { frac } => out.speed_scale *= 1.0 + frac,
                    StatusKind::Root => out.rooted = true,
                    StatusKind::Frozen => {
                        out.rooted = true;
                        out.silenced = true;
                    }
                    StatusKind::Silence => out.silenced = true,
                    StatusKind::Levitate => out.levitate = true,
                    _ => {}
                }
            }
        }
        out
    }
}

/// Status-derived movement modifiers.
struct MovementStatusMods {
    speed_scale: f32,
    rooted: bool,
    silenced: bool,
    levitate: bool,
}

/// True if a `faction` filter admits `target` for a caster on `caster_team`.
fn faction_ok(
    faction: Faction,
    caster_team: Team,
    caster: EntityId,
    target_team: Team,
    target: EntityId,
) -> bool {
    let allied = caster_team == target_team && caster_team != Team::None;
    match faction {
        Faction::Enemies => target != caster && combat::can_damage(caster_team, target_team),
        Faction::Allies => target == caster || allied,
        Faction::SelfOnly => target == caster,
        Faction::All => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arena_content::default_pack;
    use arena_content::ids::SpellId;
    use arena_content::spell::{EffectOp, Scaling, SpellDef};
    use crate::movement::STAND_HALF_HEIGHT;

    fn registry() -> ContentRegistry {
        ContentRegistry::new(1, default_pack()).expect("default pack is valid")
    }

    fn world() -> World {
        World::new(MapDef::test_arena(), registry())
    }

    fn place(w: &mut World, owner: &str, team: Team, pos: Vec3, yaw: f32) -> EntityId {
        let id = w.spawn_player(owner.to_string(), team);
        let e = w.entities.get_mut(&id).unwrap();
        e.pos = pos;
        e.vel = Vec3::ZERO;
        e.yaw = yaw;
        e.pitch = 0.0;
        e.flags = EntityFlags::default();
        e.flags.set(EntityFlags::ON_GROUND, true);
        id
    }

    /// A simple instant ray spell that deals a fixed large amount of damage.
    fn ray_nuke(damage: f32) -> SpellDef {
        SpellDef {
            id: SpellId::new("test.ray_nuke"),
            name: "Ray Nuke".into(),
            description: String::new(),
            element: arena_content::ids::ElementId::new("arcane"),
            mana_cost: 10.0,
            cast_time: 0.0,
            cooldown: 1.0,
            channeled: false,
            scaling: Scaling::default(),
            root: EffectOp::Ray {
                range: 50.0,
                pierce: 0,
                then: Box::new(EffectOp::Damage {
                    amount: damage,
                    element: arena_content::ids::ElementId::new("arcane"),
                }),
            },
            author_cost: 0,
        }
    }

    #[test]
    fn ray_damage_spell_kills_target() {
        let mut w = world();
        let caster = place(&mut w, "a", Team::Red, Vec3::new(0.0, STAND_HALF_HEIGHT, 0.0), 0.0);
        let victim = place(&mut w, "b", Team::Blue, Vec3::new(0.0, STAND_HALF_HEIGHT, -5.0), 0.0);
        let spell = ray_nuke(500.0);
        let dir = movement::view_dir(0.0, 0.0);
        let origin = movement::eye_position(&w.entities[&caster]);
        let events = magic::cast(&mut w, caster, &spell, origin, dir, 1);
        assert!(
            events.iter().any(|e| matches!(e, GameEvent::Hit { victim: v, .. } if *v == victim)),
            "expected a hit, got {:?}",
            events
        );
        assert!(w.entities[&victim].health <= 0, "victim should be dead");
    }

    #[test]
    fn mana_gate_rejects_empty_mana_cast() {
        let mut w = world();
        let caster = place(&mut w, "a", Team::Red, Vec3::new(0.0, STAND_HALF_HEIGHT, 0.0), 0.0);
        let victim = place(&mut w, "b", Team::Blue, Vec3::new(0.0, STAND_HALF_HEIGHT, -5.0), 0.0);
        // Drain mana to nothing.
        w.rpg.get_mut(&caster).unwrap().mana = 0.0;
        let hp_before = w.entities[&victim].health;
        // Hold FIRE (selected ability is fireball from the ember staff).
        let mut b = Buttons::default();
        b.set(Buttons::FIRE, true);
        w.set_input(
            caster,
            InputFrame { seq: 1, client_tick: 0, buttons: b, yaw: 0.0, pitch: 0.0, weapon_slot: 0 },
        );
        w.tick();
        assert_eq!(w.entities[&victim].health, hp_before, "no damage should occur with no mana");
        let tele = w.take_telemetry(caster);
        assert!(tele.firerate_violations >= 1, "a blocked cast should be flagged");
    }

    #[test]
    fn status_dot_reduces_health_over_ticks() {
        let mut w = world();
        let target = place(&mut w, "t", Team::Red, Vec3::new(0.0, STAND_HALF_HEIGHT, 0.0), 0.0);
        let attacker = place(&mut w, "a", Team::Blue, Vec3::new(0.0, STAND_HALF_HEIGHT, -3.0), 0.0);
        let hp0 = w.entities[&target].health;
        // Apply the default "burning" DoT directly.
        w.apply_status_to(target, &StatusId::new("status.burning"), 4.0, 1, w.current_tick(), attacker);
        for _ in 0..40 {
            w.tick();
        }
        assert!(w.entities[&target].health < hp0, "burning should have dealt damage");
    }

    #[test]
    fn loot_spawns_on_death_and_transfers_on_pickup() {
        let mut w = world();
        let victim = place(&mut w, "v", Team::Red, Vec3::new(0.0, STAND_HALF_HEIGHT, 0.0), 0.0);
        let looter = place(&mut w, "l", Team::Red, Vec3::new(0.5, STAND_HALF_HEIGHT, 0.0), 0.0);
        // Give the victim a distinctive item to drop.
        w.inventory.get_mut(&victim).unwrap().add_item(ItemId::new("item.crystal_shard"), 4);
        // Kill the victim outright.
        w.entities.get_mut(&victim).unwrap().health = 0;
        w.last_attacker.insert(victim, looter);

        // First tick: death + loot spawn.
        w.tick();
        let loot_exists = w.entities.values().any(|e| e.kind == EntityKind::Pickup);
        assert!(loot_exists, "a loot pickup should have spawned on death");

        // The looter is standing on it; subsequent ticks transfer it.
        let mut taken = false;
        for _ in 0..3 {
            let r = w.tick();
            if r.events.iter().any(|e| matches!(e, GameEvent::PickupTaken { .. })) {
                taken = true;
                break;
            }
        }
        assert!(taken, "the nearby player should collect the loot");
        assert!(
            w.inventory[&looter].count(&ItemId::new("item.crystal_shard")) > 0,
            "looter should have received dropped items"
        );
    }

    #[test]
    fn content_hot_swap_changes_a_spell() {
        let mut w = world();
        let caster = place(&mut w, "a", Team::Red, Vec3::new(0.0, STAND_HALF_HEIGHT, 0.0), 0.0);
        let victim = place(&mut w, "b", Team::Blue, Vec3::new(0.0, STAND_HALF_HEIGHT, -5.0), 0.0);

        // Cast arcane lance from the base pack and record damage.
        let lance = w.content.spell(&SpellId::new("spell.arcane_lance")).cloned().unwrap();
        let dir = movement::view_dir(0.0, 0.0);
        let origin = movement::eye_position(&w.entities[&caster]);
        let evs = magic::cast(&mut w, caster, &lance, origin, dir, 1);
        let base_dmg = evs
            .iter()
            .find_map(|e| match e {
                GameEvent::Hit { damage, .. } => Some(*damage),
                _ => None,
            })
            .expect("base cast should hit");

        // Stage a pack where arcane lance hits far harder, then apply at tick boundary.
        let mut pack = default_pack();
        for s in pack.spells.iter_mut() {
            if s.id == SpellId::new("spell.arcane_lance") {
                s.root = EffectOp::Ray {
                    range: 50.0,
                    pierce: 3,
                    then: Box::new(EffectOp::Damage {
                        amount: 999.0,
                        element: arena_content::ids::ElementId::new("arcane"),
                    }),
                };
            }
        }
        w.stage_content(2, pack).expect("stage ok");
        // Heal the victim back up and apply the swap.
        w.entities.get_mut(&victim).unwrap().health = 100;
        w.tick(); // applies pending content at the top

        let lance2 = w.content.spell(&SpellId::new("spell.arcane_lance")).cloned().unwrap();
        let origin2 = movement::eye_position(&w.entities[&caster]);
        let evs2 = magic::cast(&mut w, caster, &lance2, origin2, dir, w.current_tick());
        let new_dmg = evs2
            .iter()
            .find_map(|e| match e {
                GameEvent::Hit { damage, .. } => Some(*damage),
                _ => None,
            })
            .expect("swapped cast should hit");
        assert!(new_dmg > base_dmg, "hot-swapped spell should deal more ({new_dmg} vs {base_dmg})");
    }

    #[test]
    fn state_hash_is_stable_across_identical_sims() {
        fn run() -> [u8; 32] {
            let mut w = world();
            let _ = place(&mut w, "p", Team::Red, Vec3::new(0.0, STAND_HALF_HEIGHT, -10.0), 0.0);
            for _ in 0..20 {
                w.tick();
            }
            w.state_hash()
        }
        assert_eq!(run(), run());
    }
}
