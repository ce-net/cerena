//! The [`World`] aggregate: the one object an authority advances each tick.
//!
//! `World` owns every mutable piece of a zone's simulation — entities, their
//! combat bookkeeping, a short ring buffer of past positions (for lag-compensated
//! hit registration), pending inputs, respawn timers, and per-player anti-cheat
//! telemetry counters. [`World::tick`] runs the whole fixed-step pipeline:
//!
//! 1. snapshot positions into the history ring (for next tick's lag-comp),
//! 2. move every alive player from its pending input,
//! 3. integrate in-flight projectiles and detonate on contact,
//! 4. resolve weapon fires (reload/fire-rate gating + lag-compensated hitscan),
//! 5. apply all damage, deaths and kill credit,
//! 6. tick respawn timers,
//! 7. garbage-collect spent projectiles.
//!
//! It is deterministic and free of I/O so the client can run the identical code
//! for prediction. The only "randomness" — shotgun spread — is hashed from the
//! tick + shooter so both sides agree (see [`crate::combat`]).

use std::collections::{HashMap, HashSet, VecDeque};

use glam::Vec3;
use sha2::{Digest, Sha256};

use arena_protocol::entity::{EntityFlags, EntityKind, EntityState};
use arena_protocol::input::{Buttons, InputFrame};
use arena_protocol::snapshot::GameEvent;
use arena_protocol::weapon::{DamageKind, WeaponDef, default_loadout};
use arena_protocol::world::Team;
use arena_protocol::{EntityId, MAX_REWIND_MS, NodeId, TICK_DT, TICK_HZ, Tick};

use crate::collision::CapsuleSample;
use crate::combat::{self, CombatState, DamageApply, FireTarget, ProjectileState};
use crate::map::MapDef;
use crate::movement;

/// How many ticks of position history we keep, derived from the lag-comp rewind
/// budget plus a little slack. At 64 Hz, 220 ms is ~15 ticks.
pub const MAX_REWIND_TICKS: u32 = (MAX_REWIND_MS * TICK_HZ + 999) / 1000;

/// Ticks of history retained in the ring buffer.
const HISTORY_LEN: usize = MAX_REWIND_TICKS as usize + 2;

/// Respawn delay after death (3 seconds).
pub const RESPAWN_TICKS: u32 = 3 * TICK_HZ;

/// A projectile self-destructs after this long if it has hit nothing (5 seconds).
pub const PROJECTILE_LIFETIME_TICKS: u32 = 5 * TICK_HZ;

/// The largest look-angle change (radians) one tick that we treat as humanly
/// possible. Beyond this we clamp nothing (look is intent) but we *count* it as an
/// aim-snap event — a flick-aimbot prior for `arena-karma`. A blistering human
/// flick is well under ~50 deg/tick at 64 Hz.
pub const MAX_HUMAN_LOOK_RAD_PER_TICK: f32 = 0.9;

/// Hard cap on a player's resulting horizontal speed. Normal movement (including
/// air-strafe accumulation and ramps) stays well below this; exceeding it means a
/// movement exploit, so we clamp and count it as a speed-hack prior.
pub const MAX_PLAUSIBLE_SPEED: f32 = 15.0;

/// Anti-cheat counters accumulated by the sim for one player, drained by
/// [`World::take_telemetry`]. `arena-karma` maps these onto the protocol's
/// `CheatTelemetry`; here they are plain, conclusive-of-nothing tallies.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CheatCounters {
    pub shots_fired: u32,
    pub shots_hit: u32,
    pub headshots: u32,
    /// Look-delta exceeded the human ceiling this many times.
    pub aim_snap_events: u32,
    /// Movement had to be clamped to a plausible speed this many times.
    pub move_corrections: u32,
    /// Fire inputs rejected for beating the weapon fire-rate gate.
    pub firerate_violations: u32,
}

/// The mutable accumulator behind [`CheatCounters`] (identical shape; kept as its
/// own type so the public counters stay `Copy` and obviously read-only).
#[derive(Debug, Clone, Copy, Default)]
struct TelemetryAccumulator {
    shots_fired: u32,
    shots_hit: u32,
    headshots: u32,
    aim_snap_events: u32,
    move_corrections: u32,
    firerate_violations: u32,
}

/// One tick of history: the positions/capsules of all players at that tick.
struct HistoryFrame {
    tick: Tick,
    samples: HashMap<EntityId, CapsuleSample>,
}

/// The result of advancing one tick.
#[derive(Debug, Clone, Default)]
pub struct TickReport {
    /// The tick that was just produced (the new authoritative "now").
    pub tick: Tick,
    /// Discrete events that occurred this tick (shots, hits, deaths, spawns, ...).
    pub events: Vec<GameEvent>,
    /// `(killer, victim)` pairs for kills this tick, for scoring/kill credit.
    pub deaths: Vec<(EntityId, EntityId)>,
}

/// A decided fire for this tick, queued in pass A and resolved in pass B.
struct FireJob {
    shooter: EntityId,
    weapon_slot: u8,
    /// Muzzle/eye origin in world space.
    origin: Vec3,
    /// Normalised aim direction.
    dir: Vec3,
    /// The shooter's reported client tick, for choosing the lag-comp rewind.
    client_tick: Tick,
}

/// The full mutable state of one zone simulation.
pub struct World {
    pub map: MapDef,
    /// The weapon table; indices are stable wire weapon slots.
    pub loadout: Vec<WeaponDef>,
    /// All live entities (players, projectiles).
    entities: HashMap<EntityId, EntityState>,
    /// Per-player combat bookkeeping (ammo, reload, fire-rate, respawn).
    combat: HashMap<EntityId, CombatState>,
    /// Per-projectile data (owner, weapon, expiry).
    projectiles: HashMap<EntityId, ProjectileState>,
    /// The latest input we hold for each player. Persists across ticks: if no new
    /// input arrives we keep applying the last one (a stalled client keeps moving
    /// the way it last asked, which the client also predicts).
    inputs: HashMap<EntityId, InputFrame>,
    /// Position history ring for lag-compensated hit registration.
    history: VecDeque<HistoryFrame>,
    /// Anti-cheat counters per player.
    telemetry: HashMap<EntityId, TelemetryAccumulator>,
    /// Monotonic tick of the latest completed state.
    tick: Tick,
    /// Counter for minting fresh entity ids.
    next_id: EntityId,
}

impl World {
    /// Build an empty world on `map` with the default loadout.
    pub fn new(map: MapDef) -> World {
        World {
            map,
            loadout: default_loadout(),
            entities: HashMap::new(),
            combat: HashMap::new(),
            projectiles: HashMap::new(),
            inputs: HashMap::new(),
            history: VecDeque::with_capacity(HISTORY_LEN),
            telemetry: HashMap::new(),
            tick: 0,
            next_id: 1,
        }
    }

    /// Read-only access to all entities, for the snapshot/replication layer.
    pub fn entities(&self) -> &HashMap<EntityId, EntityState> {
        &self.entities
    }

    /// The current authoritative tick.
    pub fn current_tick(&self) -> Tick {
        self.tick
    }

    /// Spawn a player for `owner` on `team`, at the safest available spawn point
    /// (furthest from enemies). Returns the new entity id. Full health/armor and a
    /// full magazine of the default weapon.
    pub fn spawn_player(&mut self, owner: NodeId, team: Team) -> EntityId {
        let enemies = self.enemy_positions(team);
        // A stable, deterministic seed so simultaneous spawns spread out.
        let seed = self.next_id as u64;
        let spawn = self.map.pick_spawn(team, &enemies, seed);

        let id = self.next_id;
        self.next_id += 1;

        let mut flags = EntityFlags::default();
        flags.set(EntityFlags::ON_GROUND, true);

        let state = EntityState {
            id,
            kind: EntityKind::Player,
            // Capsule centre sits a standing half-height above the spawn floor.
            pos: spawn.pos + Vec3::Y * movement::STAND_HALF_HEIGHT,
            vel: Vec3::ZERO,
            yaw: spawn.yaw,
            pitch: 0.0,
            flags,
            team,
            health: 100,
            armor: 50,
            weapon: 0,
            owner,
        };
        self.entities.insert(id, state);
        self.combat.insert(id, CombatState::fresh(&self.loadout[0]));
        self.telemetry.insert(id, TelemetryAccumulator::default());
        id
    }

    /// Remove an entity (player disconnect, projectile cleanup) and all its
    /// associated bookkeeping.
    pub fn remove_entity(&mut self, id: EntityId) {
        self.entities.remove(&id);
        self.combat.remove(&id);
        self.projectiles.remove(&id);
        self.inputs.remove(&id);
        self.telemetry.remove(&id);
    }

    /// Accept a new input frame for a player. The frame is sanitised (NaN-scrubbed,
    /// look angles clamped) and the weapon slot bounded to the loadout. If the look
    /// direction moved faster than a human possibly could relative to the previous
    /// frame, we record an aim-snap event for anti-cheat (we still apply it — look
    /// is intent and clamping it would harm honest high-sensitivity players).
    pub fn set_input(&mut self, id: EntityId, frame: InputFrame) {
        let mut frame = frame.sanitized();
        // Bound the weapon slot so a bad packet can never index out of the loadout.
        let max_slot = self.loadout.len().saturating_sub(1) as u8;
        if frame.weapon_slot > max_slot {
            frame.weapon_slot = max_slot;
        }
        // Aim-snap detection against the previously held frame.
        if let Some(prev) = self.inputs.get(&id) {
            if frame.look_delta(prev) > MAX_HUMAN_LOOK_RAD_PER_TICK {
                if let Some(t) = self.telemetry.get_mut(&id) {
                    t.aim_snap_events += 1;
                }
            }
        }
        self.inputs.insert(id, frame);
    }

    /// Ammo / respawn view for the snapshot layer: `(in_mag, reserve, respawn_at)`.
    pub fn combat_view(&self, id: EntityId) -> Option<(u16, u16, Tick)> {
        self.combat
            .get(&id)
            .map(|c| (c.ammo_in_mag, c.ammo_reserve, c.respawn_at))
    }

    /// Drain and reset a player's anti-cheat counters. `arena-karma` calls this
    /// periodically and folds the result into its cross-round aggregation.
    pub fn take_telemetry(&mut self, id: EntityId) -> CheatCounters {
        match self.telemetry.get_mut(&id) {
            Some(acc) => {
                let out = CheatCounters {
                    shots_fired: acc.shots_fired,
                    shots_hit: acc.shots_hit,
                    headshots: acc.headshots,
                    aim_snap_events: acc.aim_snap_events,
                    move_corrections: acc.move_corrections,
                    firerate_violations: acc.firerate_violations,
                };
                *acc = TelemetryAccumulator::default();
                out
            }
            None => CheatCounters::default(),
        }
    }

    /// Advance the simulation one fixed step and return what happened.
    pub fn tick(&mut self) -> TickReport {
        // Record the state we are leaving so lag-comp can rewind into it.
        let prev_tick = self.tick;
        self.record_history(prev_tick);
        self.tick = prev_tick + 1;
        let now = self.tick;

        let mut events: Vec<GameEvent> = Vec::new();
        let mut deaths: Vec<(EntityId, EntityId)> = Vec::new();
        // All damage decided this tick, applied in one pass at the end.
        let mut damages: Vec<DamageApply> = Vec::new();

        // The brush list is read constantly while we mutate entities; clone it once
        // (the arena has a handful of AABBs) to sidestep the borrow checker.
        let brushes = self.map.brushes.clone();

        let player_ids: Vec<EntityId> = self
            .entities
            .iter()
            .filter(|(_, e)| e.kind == EntityKind::Player)
            .map(|(id, _)| *id)
            .collect();

        // --- 2. Movement ------------------------------------------------------
        for id in &player_ids {
            let pre = {
                let e = match self.entities.get(id) {
                    Some(e) if e.is_alive() => e,
                    _ => continue,
                };
                let frame = self.input_for(*id, e);
                (e.flags.has(EntityFlags::ON_GROUND), frame)
            };
            let (on_ground_prev, frame) = pre;

            let clamped = {
                let e = self.entities.get_mut(id).unwrap();
                e.weapon = frame.weapon_slot;
                movement::move_player(e, &frame, TICK_DT, on_ground_prev, &brushes);
                // Speed-hack guard: clamp absurd horizontal speed and flag it.
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

        // --- 3. Projectiles ---------------------------------------------------
        self.integrate_projectiles(now, &brushes, &player_ids, &mut events, &mut damages);

        // --- 4a. Fire decisions (reload + fire-rate gating) -------------------
        let mut fire_jobs: Vec<FireJob> = Vec::new();
        let mut fired_ids: HashSet<EntityId> = HashSet::new();
        for id in &player_ids {
            let (frame, eye, aim, _team) = {
                let e = match self.entities.get(id) {
                    Some(e) if e.is_alive() => e,
                    _ => continue,
                };
                let frame = self.input_for(*id, e);
                (frame, movement::eye_position(e), movement::view_dir(frame.yaw, frame.pitch), e.team)
            };
            let slot = (frame.weapon_slot as usize).min(self.loadout.len().saturating_sub(1));
            let (min_interval, mag_size, reload_ticks) = {
                let w = &self.loadout[slot];
                (
                    w.min_shot_interval(),
                    w.mag_size,
                    (w.reload_s * TICK_HZ as f32).ceil() as u32,
                )
            };

            let mut do_fire = false;
            let mut violation = false;
            {
                let cs = self.combat.get_mut(id).unwrap();
                // Complete an in-progress reload.
                if let Some(end) = cs.reload_end_tick {
                    if now >= end {
                        let need = mag_size.saturating_sub(cs.ammo_in_mag);
                        let take = need.min(cs.ammo_reserve);
                        cs.ammo_in_mag += take;
                        cs.ammo_reserve -= take;
                        cs.reload_end_tick = None;
                    }
                }
                let fire_pressed = frame.buttons.has(Buttons::FIRE);
                let reload_pressed = frame.buttons.has(Buttons::RELOAD);
                // Begin a reload if asked and it would help.
                if reload_pressed
                    && !cs.is_reloading()
                    && cs.ammo_in_mag < mag_size
                    && cs.ammo_reserve > 0
                {
                    cs.reload_end_tick = Some(now + reload_ticks);
                }
                let reloading = cs.is_reloading();
                let elapsed = match cs.last_shot_tick {
                    None => true,
                    Some(t) => (now - t) as f32 * TICK_DT + 1e-6 >= min_interval,
                };
                let edge = fire_pressed && !cs.prev_fire;
                cs.prev_fire = fire_pressed;

                if fire_pressed && !reloading && cs.ammo_in_mag > 0 && elapsed {
                    cs.ammo_in_mag -= 1;
                    cs.last_shot_tick = Some(now);
                    do_fire = true;
                } else if edge && fire_pressed && !reloading && cs.ammo_in_mag > 0 && !elapsed {
                    // A fresh trigger pull that beats the fire-rate gate: prior for
                    // a rapid-fire / macro cheat.
                    violation = true;
                }
            }

            if do_fire {
                fire_jobs.push(FireJob {
                    shooter: *id,
                    weapon_slot: slot as u8,
                    origin: eye,
                    dir: aim,
                    client_tick: frame.client_tick,
                });
                fired_ids.insert(*id);
                if let Some(t) = self.telemetry.get_mut(id) {
                    t.shots_fired += 1;
                }
            } else if violation {
                if let Some(t) = self.telemetry.get_mut(id) {
                    t.firerate_violations += 1;
                }
            }
        }

        // Sync the transient FIRING / RELOADING pose flags.
        for id in &player_ids {
            let reloading = self.combat.get(id).map(|c| c.is_reloading()).unwrap_or(false);
            if let Some(e) = self.entities.get_mut(id) {
                e.flags.set(EntityFlags::FIRING, fired_ids.contains(id));
                e.flags.set(EntityFlags::RELOADING, reloading);
            }
        }

        // --- 4b. Resolve fires (hitscan with lag compensation) ----------------
        let mut spawned: Vec<(EntityState, ProjectileState)> = Vec::new();
        for job in &fire_jobs {
            // Copy the scalars we need so the loadout borrow ends before we mutate
            // `self.next_id` for projectile weapons.
            let (kind, weapon_id, spread, max_range, proj_speed) = {
                let w = &self.loadout[job.weapon_slot as usize];
                (w.kind, w.id, w.spread_rad, w.max_range_m, w.projectile_speed)
            };

            // Every fire shows a tracer/flash, even a clean miss.
            events.push(GameEvent::Shot {
                shooter: job.shooter,
                weapon: weapon_id,
                origin: job.origin,
                dir: job.dir,
            });

            match kind {
                DamageKind::Hitscan => {
                    let sample_tick = self.rewind_sample_tick(now, job.client_tick);
                    let targets = self.build_targets(job.shooter, sample_tick);
                    let dir = combat::deterministic_spread(
                        job.dir,
                        spread,
                        combat::spread_seed(now, job.shooter, 0),
                    );
                    if let Some(hit) =
                        combat::hitscan_ray(job.origin, dir, max_range, &targets, &brushes)
                    {
                        let w = &self.loadout[job.weapon_slot as usize];
                        let dmg = combat::damage_for(w, hit.dist, hit.headshot);
                        damages.push(DamageApply {
                            attacker: job.shooter,
                            victim: hit.victim,
                            amount: dmg,
                            headshot: hit.headshot,
                            point: hit.point,
                            weapon: weapon_id,
                        });
                        if let Some(t) = self.telemetry.get_mut(&job.shooter) {
                            t.shots_hit += 1;
                            if hit.headshot {
                                t.headshots += 1;
                            }
                        }
                    }
                }
                DamageKind::Melee => {
                    // Shotgun / melee cone: `pellets` independent rays, each with a
                    // deterministic spread seed so client and server agree.
                    let sample_tick = self.rewind_sample_tick(now, job.client_tick);
                    let targets = self.build_targets(job.shooter, sample_tick);
                    let w = self.loadout[job.weapon_slot as usize].clone();
                    let pellets = w.pellets.max(1);
                    let mut any_hit = false;
                    let mut any_head = false;
                    for p in 0..pellets {
                        let dir = combat::deterministic_spread(
                            job.dir,
                            spread,
                            combat::spread_seed(now, job.shooter, p),
                        );
                        if let Some(hit) =
                            combat::hitscan_ray(job.origin, dir, max_range, &targets, &brushes)
                        {
                            let dmg = combat::damage_for(&w, hit.dist, hit.headshot);
                            damages.push(DamageApply {
                                attacker: job.shooter,
                                victim: hit.victim,
                                amount: dmg,
                                headshot: hit.headshot,
                                point: hit.point,
                                weapon: weapon_id,
                            });
                            any_hit = true;
                            any_head |= hit.headshot;
                        }
                    }
                    if let Some(t) = self.telemetry.get_mut(&job.shooter) {
                        if any_hit {
                            t.shots_hit += 1;
                        }
                        if any_head {
                            t.headshots += 1;
                        }
                    }
                }
                DamageKind::Projectile => {
                    // Spawn a travelling projectile entity; it resolves over the
                    // coming ticks in `integrate_projectiles`.
                    let team = self
                        .entities
                        .get(&job.shooter)
                        .map(|e| e.team)
                        .unwrap_or(Team::None);
                    let pid = self.next_id;
                    self.next_id += 1;
                    let state = EntityState {
                        id: pid,
                        kind: EntityKind::Projectile,
                        pos: job.origin,
                        vel: job.dir * proj_speed,
                        yaw: 0.0,
                        pitch: 0.0,
                        flags: EntityFlags::default(),
                        team,
                        health: 1,
                        armor: 0,
                        weapon: weapon_id,
                        owner: String::new(),
                    };
                    spawned.push((
                        state,
                        ProjectileState {
                            owner: job.shooter,
                            weapon: weapon_id,
                            expire_tick: now + PROJECTILE_LIFETIME_TICKS,
                        },
                    ));
                }
            }
        }
        for (state, ps) in spawned {
            let id = state.id;
            self.entities.insert(id, state);
            self.projectiles.insert(id, ps);
        }

        // --- 5. Apply damage, deaths and kill credit --------------------------
        self.apply_damages(now, &damages, &mut events, &mut deaths);

        // --- 6. Respawns ------------------------------------------------------
        self.process_respawns(now, &player_ids, &mut events);

        TickReport {
            tick: now,
            events,
            deaths,
        }
    }

    /// Deterministic 32-byte fingerprint of the world state, for cross-validation
    /// between authorities. Entities are visited in sorted-id order and floats are
    /// rounded to 1e-3 before hashing so platform float jitter does not break
    /// agreement (a small epsilon is acceptable; bit-exactness is not required).
    pub fn state_hash(&self) -> [u8; 32] {
        // Quantise a float to milli-units to absorb cross-platform jitter.
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
            h.update([e.kind as u8]);
            for v in [
                e.pos.x, e.pos.y, e.pos.z, e.vel.x, e.vel.y, e.vel.z, e.yaw, e.pitch,
            ] {
                h.update(q(v).to_le_bytes());
            }
            h.update(e.health.to_le_bytes());
            h.update(e.armor.to_le_bytes());
            h.update(e.flags.0.to_le_bytes());
            h.update([e.weapon, e.team as u8]);
        }
        h.finalize().into()
    }

    // --- internals ----------------------------------------------------------

    /// The input to apply for `id`: the last frame received, or a neutral frame
    /// (no buttons, current look) if the client has never sent one.
    fn input_for(&self, id: EntityId, e: &EntityState) -> InputFrame {
        self.inputs.get(&id).copied().unwrap_or(InputFrame {
            seq: 0,
            client_tick: 0,
            buttons: Buttons::default(),
            yaw: e.yaw,
            pitch: e.pitch,
            weapon_slot: e.weapon,
        })
    }

    /// Snapshot all player capsules at `tick` into the history ring, pruning old
    /// frames beyond the rewind budget.
    fn record_history(&mut self, tick: Tick) {
        let mut samples = HashMap::new();
        for (id, e) in &self.entities {
            if e.kind == EntityKind::Player {
                samples.insert(
                    *id,
                    CapsuleSample {
                        pos: e.pos,
                        half_height: movement::half_height_of(e),
                        radius: movement::PLAYER_RADIUS,
                        team: e.team,
                        alive: e.is_alive(),
                    },
                );
            }
        }
        self.history.push_back(HistoryFrame { tick, samples });
        while self.history.len() > HISTORY_LEN {
            self.history.pop_front();
        }
    }

    /// Which historical tick to resolve a shot against, from the shooter's reported
    /// clock. The further behind the client's clock, the further we rewind, capped
    /// at the rewind budget so no one can reconcile arbitrarily far into the past.
    fn rewind_sample_tick(&self, now: Tick, client_tick: Tick) -> Tick {
        let rewind = now.saturating_sub(client_tick).min(MAX_REWIND_TICKS);
        now.saturating_sub(rewind)
    }

    /// The capsule of `id` at `sample_tick` from history, falling back to the
    /// current state if that tick isn't retained or the player wasn't present.
    fn sample_capsule(
        &self,
        id: EntityId,
        current: &EntityState,
        sample_tick: Tick,
    ) -> (Vec3, f32, f32) {
        if let Some(frame) = self.history.iter().find(|f| f.tick == sample_tick) {
            if let Some(s) = frame.samples.get(&id) {
                return (s.pos, s.half_height, s.radius);
            }
        }
        (
            current.pos,
            movement::half_height_of(current),
            movement::PLAYER_RADIUS,
        )
    }

    /// Assemble the candidate hitscan targets for `shooter`: every alive, hostile
    /// player, positioned where it was at `sample_tick`.
    fn build_targets(&self, shooter: EntityId, sample_tick: Tick) -> Vec<FireTarget> {
        let shooter_team = self
            .entities
            .get(&shooter)
            .map(|e| e.team)
            .unwrap_or(Team::None);
        let mut out = Vec::new();
        for (id, e) in &self.entities {
            if *id == shooter || e.kind != EntityKind::Player || !e.is_alive() {
                continue;
            }
            if !combat::can_damage(shooter_team, e.team) {
                continue;
            }
            let (pos, hh, r) = self.sample_capsule(*id, e, sample_tick);
            out.push(FireTarget {
                id: *id,
                base: pos - Vec3::Y * hh, // capsule feet
                half_height: hh,
                radius: r,
            });
        }
        out
    }

    /// World positions of all hostile players relative to `team` (for spawn safety).
    fn enemy_positions(&self, team: Team) -> Vec<Vec3> {
        self.entities
            .values()
            .filter(|e| {
                e.kind == EntityKind::Player
                    && e.is_alive()
                    && e.team != team
                    && combat::can_damage(team, e.team)
            })
            .map(|e| e.pos)
            .collect()
    }

    /// Move every in-flight projectile one tick, detonating on the first contact
    /// (geometry or player) along its path, or silently expiring after its
    /// lifetime. Splash damage is queued into `damages`.
    fn integrate_projectiles(
        &mut self,
        now: Tick,
        brushes: &[arena_protocol::world::Aabb],
        player_ids: &[EntityId],
        events: &mut Vec<GameEvent>,
        damages: &mut Vec<DamageApply>,
    ) {
        let proj_ids: Vec<EntityId> = self.projectiles.keys().copied().collect();
        let mut remove: Vec<EntityId> = Vec::new();

        for pid in proj_ids {
            let (owner, weapon_id) = match self.projectiles.get(&pid) {
                Some(p) => (p.owner, p.weapon),
                None => continue,
            };
            let expire = self.projectiles[&pid].expire_tick;
            let (old, vel) = match self.entities.get(&pid) {
                Some(e) => (e.pos, e.vel),
                None => {
                    remove.push(pid);
                    continue;
                }
            };

            let step = vel * TICK_DT;
            let len = step.length();
            // Degenerate (no speed): just check for expiry.
            if len < 1e-6 {
                if now >= expire {
                    remove.push(pid);
                }
                continue;
            }
            let dir = step / len;

            // Nearest contact: static geometry first, then any non-owner player.
            let mut contact_t = crate::collision::raycast_aabbs(old, dir, len, brushes)
                .map(|(t, _)| t);
            for id in player_ids {
                if *id == owner {
                    continue;
                }
                let e = match self.entities.get(id) {
                    Some(e) if e.is_alive() => e,
                    _ => continue,
                };
                let hh = movement::half_height_of(e);
                let base = e.pos - Vec3::Y * hh;
                if let Some((t, _, _)) =
                    crate::collision::ray_capsule(old, dir, base, hh, movement::PLAYER_RADIUS)
                {
                    if t <= len && contact_t.map_or(true, |c| t < c) {
                        contact_t = Some(t);
                    }
                }
            }

            match contact_t {
                Some(t) => {
                    let center = old + dir * t;
                    self.detonate(center, owner, weapon_id, player_ids, events, damages);
                    remove.push(pid);
                }
                None => {
                    if now >= expire {
                        // Flew its life out without hitting anything: vanish.
                        remove.push(pid);
                    } else if let Some(e) = self.entities.get_mut(&pid) {
                        e.pos = old + step;
                    }
                }
            }
        }

        for pid in remove {
            self.entities.remove(&pid);
            self.projectiles.remove(&pid);
        }
    }

    /// Apply a projectile blast at `center`: an Explosion event plus radius-falloff
    /// splash damage to every hostile player in range (the owner is immune).
    fn detonate(
        &mut self,
        center: Vec3,
        owner: EntityId,
        weapon_id: u8,
        player_ids: &[EntityId],
        events: &mut Vec<GameEvent>,
        damages: &mut Vec<DamageApply>,
    ) {
        let (radius, base_damage, owner_team) = {
            let w = &self.loadout[weapon_id as usize];
            let owner_team = self
                .entities
                .get(&owner)
                .map(|e| e.team)
                .unwrap_or(Team::None);
            (w.splash_radius_m.max(0.01), w.base_damage, owner_team)
        };

        events.push(GameEvent::Explosion { center, radius });

        for id in player_ids {
            if *id == owner {
                continue;
            }
            let e = match self.entities.get(id) {
                Some(e) if e.is_alive() => e,
                _ => continue,
            };
            if !combat::can_damage(owner_team, e.team) {
                continue;
            }
            let dist = e.pos.distance(center);
            if dist >= radius {
                continue;
            }
            // Linear falloff from full at the centre to zero at the radius.
            let amount = base_damage * (1.0 - dist / radius);
            damages.push(DamageApply {
                attacker: owner,
                victim: *id,
                amount,
                headshot: false,
                point: center,
                weapon: weapon_id,
            });
        }
    }

    /// Apply all queued damage, emitting Hit and Death events and recording kills.
    /// A victim already dead this tick is skipped, so a multi-pellet shot or a
    /// blast can't kill the same player twice.
    fn apply_damages(
        &mut self,
        now: Tick,
        damages: &[DamageApply],
        events: &mut Vec<GameEvent>,
        deaths: &mut Vec<(EntityId, EntityId)>,
    ) {
        for d in damages {
            let mut died_node: Option<NodeId> = None;
            {
                let v = match self.entities.get_mut(&d.victim) {
                    Some(v) if v.is_alive() => v,
                    _ => continue,
                };
                let before = v.health;
                combat::apply_armor_damage(d.amount, &mut v.health, &mut v.armor);
                let dealt = (before - v.health).max(0) as f32;
                events.push(GameEvent::Hit {
                    attacker: d.attacker,
                    victim: d.victim,
                    damage: dealt,
                    headshot: d.headshot,
                    point: d.point,
                });
                if v.health <= 0 {
                    v.health = 0;
                    v.flags.set(EntityFlags::DEAD, true);
                    v.flags.set(EntityFlags::FIRING, false);
                    v.flags.set(EntityFlags::SPRINTING, false);
                    died_node = Some(v.owner.clone());
                }
            }
            if let Some(victim_node) = died_node {
                let killer_node = self
                    .entities
                    .get(&d.attacker)
                    .map(|a| a.owner.clone())
                    .unwrap_or_default();
                if let Some(cs) = self.combat.get_mut(&d.victim) {
                    cs.respawn_at = now + RESPAWN_TICKS;
                }
                events.push(GameEvent::Death {
                    victim: d.victim,
                    killer: d.attacker,
                    weapon: d.weapon,
                    victim_node,
                    killer_node,
                });
                deaths.push((d.attacker, d.victim));
            }
        }
    }

    /// Respawn any dead player whose timer has elapsed, at a fresh safe spawn.
    fn process_respawns(
        &mut self,
        now: Tick,
        player_ids: &[EntityId],
        events: &mut Vec<GameEvent>,
    ) {
        for id in player_ids {
            let respawn_at = self.combat.get(id).map(|c| c.respawn_at).unwrap_or(0);
            let dead = self
                .entities
                .get(id)
                .map(|e| e.flags.has(EntityFlags::DEAD))
                .unwrap_or(false);
            if !dead || respawn_at == 0 || now < respawn_at {
                continue;
            }

            let team = self.entities[id].team;
            let enemies = self.enemy_positions(team);
            // Seed off tick + id so two simultaneous respawns don't collide.
            let spawn = self
                .map
                .pick_spawn(team, &enemies, (now as u64) ^ (*id as u64));
            let new_pos = spawn.pos + Vec3::Y * movement::STAND_HALF_HEIGHT;

            {
                let e = self.entities.get_mut(id).unwrap();
                e.pos = new_pos;
                e.vel = Vec3::ZERO;
                e.yaw = spawn.yaw;
                e.pitch = 0.0;
                e.health = 100;
                e.armor = 50;
                e.weapon = 0;
                e.flags = EntityFlags::default();
                e.flags.set(EntityFlags::ON_GROUND, true);
            }
            if let Some(cs) = self.combat.get_mut(id) {
                *cs = CombatState::fresh(&self.loadout[0]);
            }
            events.push(GameEvent::Spawn {
                entity: *id,
                pos: new_pos,
                team,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::movement::{MAX_GROUND_SPEED, STAND_HALF_HEIGHT};

    /// Spawn a player then place it precisely, grounded, for controlled tests.
    fn grounded_player(
        w: &mut World,
        owner: &str,
        team: Team,
        pos: Vec3,
        yaw: f32,
    ) -> EntityId {
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

    fn buttons(flags: &[u16]) -> Buttons {
        let mut b = Buttons::default();
        for f in flags {
            b.set(*f, true);
        }
        b
    }

    #[test]
    fn ground_movement_reaches_max_speed() {
        let mut w = World::new(MapDef::test_arena());
        let id = grounded_player(
            &mut w,
            "p1",
            Team::Red,
            Vec3::new(0.0, STAND_HALF_HEIGHT, -10.0),
            0.0,
        );
        // Hold forward for two seconds.
        for t in 0..128 {
            w.set_input(
                id,
                InputFrame {
                    seq: t,
                    client_tick: 0,
                    buttons: buttons(&[Buttons::FORWARD]),
                    yaw: 0.0,
                    pitch: 0.0,
                    weapon_slot: 0,
                },
            );
            w.tick();
        }
        let e = &w.entities()[&id];
        let speed = (e.vel.x * e.vel.x + e.vel.z * e.vel.z).sqrt();
        // Friction caps ground speed at MAX_GROUND_SPEED (~7 m/s).
        assert!(
            speed > MAX_GROUND_SPEED - 1.0 && speed < MAX_GROUND_SPEED + 0.6,
            "expected ~max ground speed, got {speed}"
        );
    }

    #[test]
    fn gravity_pulls_a_falling_player_down() {
        let mut w = World::new(MapDef::test_arena());
        let id = w.spawn_player("p".to_string(), Team::Red);
        {
            let e = w.entities.get_mut(&id).unwrap();
            e.pos = Vec3::new(0.0, 50.0, 0.0);
            e.vel = Vec3::ZERO;
            e.flags = EntityFlags::default();
            e.flags.set(EntityFlags::AIRBORNE, true);
        }
        let y0 = 50.0;
        for _ in 0..10 {
            w.tick();
        }
        let e = &w.entities()[&id];
        assert!(e.vel.y < 0.0, "falling player should have downward velocity");
        assert!(e.pos.y < y0, "falling player should have descended");
    }

    #[test]
    fn hitscan_straight_line_registers_a_hit() {
        let mut w = World::new(MapDef::test_arena());
        let shooter = grounded_player(
            &mut w,
            "s",
            Team::Red,
            Vec3::new(0.0, STAND_HALF_HEIGHT, 0.0),
            0.0, // yaw 0 looks toward -Z
        );
        let target = grounded_player(
            &mut w,
            "t",
            Team::Blue,
            Vec3::new(0.0, STAND_HALF_HEIGHT, -5.0),
            0.0,
        );
        // Fire with client_tick in the future so lag-comp uses current positions.
        w.set_input(
            shooter,
            InputFrame {
                seq: 1,
                client_tick: u32::MAX,
                buttons: buttons(&[Buttons::FIRE]),
                yaw: 0.0,
                pitch: 0.0,
                weapon_slot: 0,
            },
        );
        let report = w.tick();
        let hit = report.events.iter().any(|ev| {
            matches!(ev, GameEvent::Hit { victim, .. } if *victim == target)
        });
        assert!(hit, "expected a Hit on the target, events: {:?}", report.events);
        assert!(
            w.entities()[&target].health < 100,
            "target should have taken damage"
        );
    }

    #[test]
    fn fire_rate_gate_rejects_a_too_soon_second_shot() {
        let mut w = World::new(MapDef::test_arena());
        // Sniper (slot 4): ~1.1 rps, so a second shot one tick later must be denied.
        let id = grounded_player(
            &mut w,
            "g",
            Team::None,
            Vec3::new(0.0, STAND_HALF_HEIGHT, 0.0),
            0.0,
        );
        let fire = InputFrame {
            seq: 1,
            client_tick: u32::MAX,
            buttons: buttons(&[Buttons::FIRE]),
            yaw: 0.0,
            pitch: 0.0,
            weapon_slot: 4,
        };
        w.set_input(id, fire);
        let r1 = w.tick();
        w.set_input(id, InputFrame { seq: 2, ..fire });
        let r2 = w.tick();

        let count_shots = |r: &TickReport| {
            r.events
                .iter()
                .filter(|e| matches!(e, GameEvent::Shot { shooter, .. } if *shooter == id))
                .count()
        };
        assert_eq!(
            count_shots(&r1) + count_shots(&r2),
            1,
            "fire-rate gate should allow exactly one shot across two adjacent ticks"
        );
    }

    #[test]
    fn state_hash_is_stable_across_identical_sims() {
        fn run() -> [u8; 32] {
            let mut w = World::new(MapDef::test_arena());
            let id = grounded_player(
                &mut w,
                "p",
                Team::Red,
                Vec3::new(0.0, STAND_HALF_HEIGHT, -10.0),
                0.0,
            );
            for t in 0..30 {
                w.set_input(
                    id,
                    InputFrame {
                        seq: t,
                        client_tick: 0,
                        buttons: buttons(&[Buttons::FORWARD]),
                        yaw: 0.0,
                        pitch: 0.0,
                        weapon_slot: 0,
                    },
                );
                w.tick();
            }
            w.state_hash()
        }
        assert_eq!(run(), run(), "identical inputs must yield identical state");
    }
}
