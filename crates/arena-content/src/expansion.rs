//! # Expansion: *Tempest, Verdance & the Hollow Dead*
//!
//! The first content expansion folded into [`crate::default_pack::default_pack`]. The
//! starter pack establishes four schools (Pyromancy, Cryomancy, Arcana, Mobility); this
//! module triples the game on top of it with **four more schools** —
//!
//! - **Stormcalling** — chain lightning, tempests, static charge, wind kinetics.
//! - **Verdancy** — nature/poison, vines, thorns, healing groves, summoned wildlife.
//! - **Necromancy** — shadow, blood, curses, drain, the hollow dead.
//! - **Chronomancy** — time dilation, haste/slow, rewind-flavoured combos.
//!
//! Everything here is **pure additive data** built from the exact same closed
//! primitive sets as the starter pack — no new [`crate::spell::EffectOp`], no new
//! [`crate::status::StatusKind`], no new [`crate::movement::MovementKind`]. So it ships
//! with **zero engine changes**: the fixed interpreters in `arena-sim`/`arena-client`
//! already understand every op these definitions compose.
//!
//! ## How it merges
//!
//! [`apply`] takes the freshly-built starter [`ContentPack`] and pushes the new
//! definitions onto each collection (and grafts two biomes + their structures onto the
//! world). Every cross-reference is intentional and resolves, so the combined pack
//! still passes [`ContentPack::validate`]. Ids are new and namespaced by school
//! (`spell.storm.*`, `item.verdance.*`, `mob.dead.*`) so nothing collides with the
//! starter ids.

use crate::ability::{AbilityDef, CastInput};
use crate::gamemode::{GameModeDef, ScoringRule, TeamConfig, WinCondition};
use crate::ids::*;
use crate::item::{CraftRecipe, EquipSlot, ItemDef, Rarity, StatMods};
use crate::loot::{LootEntry, LootTableDef};
use crate::material::{ColorRamp, MaterialDef, NoiseKind, NoiseLayer, ShaderDef, ShaderStage};
use crate::mission::{MissionDef, Objective};
use crate::mob::MobDef;
use crate::movement::{MovementKind, MovementModeDef};
use crate::pack::ContentPack;
use crate::spawn::{SpawnRuleDef, SpawnTrigger};
use crate::spell::{EffectOp, Faction, Scaling, SpellDef};
use crate::status::{StatusEffectDef, StatusKind};
use crate::tech::{TechEffect, TechNode};
use crate::triggers::{GameTrigger, RuleAction, TriggerCondition, TriggerDef};
use crate::worldgen::{BiomeDef, StructureDef};

/// Box an effect-op continuation (mirrors `default_pack::b`).
fn b(op: EffectOp) -> Box<EffectOp> {
    Box::new(op)
}

/// Build a spell, deriving `author_cost` from graph complexity exactly like the
/// starter pack so authored and expansion spells share one pricing scale.
#[allow(clippy::too_many_arguments)]
fn spell(
    id: &str,
    name: &str,
    description: &str,
    element: &str,
    mana_cost: f32,
    cast_time: f32,
    cooldown: f32,
    channeled: bool,
    scaling: Scaling,
    root: EffectOp,
) -> SpellDef {
    let author_cost = root.complexity();
    SpellDef {
        id: SpellId::new(id),
        name: name.to_string(),
        description: description.to_string(),
        element: ElementId::new(element),
        mana_cost,
        cast_time,
        cooldown,
        channeled,
        scaling,
        root,
        author_cost,
    }
}

/// Graft every expansion definition onto an already-built starter pack. Pure append +
/// world-graft; the result still satisfies [`ContentPack::validate`].
pub fn apply(pack: &mut ContentPack) {
    pack.label = "cerena-default-0.3 + tempest-verdance-hollow".to_string();

    pack.statuses.extend(statuses());
    pack.spells.extend(spells());
    pack.movement_modes.extend(movement_modes());
    pack.materials.extend(materials());
    pack.shaders.extend(shaders());
    pack.mobs.extend(mobs());
    pack.items.extend(items());
    pack.abilities.extend(abilities());
    pack.tech.nodes.extend(tech_nodes());
    pack.missions.extend(missions());
    pack.loot_tables.extend(loot_tables());
    pack.spawn_rules.extend(spawn_rules());
    pack.triggers.extend(triggers());
    pack.game_modes.extend(game_modes());
    graft_world(&mut pack.worldgen);
}

// ===========================================================================
// Status effects — the new mechanical vocabulary the expansion's spells lean on.
// Uses only existing StatusKind variants (incl. the previously-unused
// DamageOverTime / Silence / Levitate / ManaBurn).
// ===========================================================================

fn statuses() -> Vec<StatusEffectDef> {
    // Terse constructor for the common shape.
    fn st(
        id: &str,
        name: &str,
        kind: StatusKind,
        max_stacks: u8,
        tick: f32,
        dur: f32,
        beneficial: bool,
        material: Option<&str>,
    ) -> StatusEffectDef {
        StatusEffectDef {
            id: StatusId::new(id),
            name: name.into(),
            kind,
            max_stacks,
            tick_interval_s: tick,
            duration_default_s: dur,
            beneficial,
            material: material.map(MaterialId::new),
        }
    }
    vec![
        // --- Verdancy ---
        st(
            "status.poisoned",
            "Poisoned",
            StatusKind::DamageOverTime { element: ElementId::new("nature"), dps: 9.0 },
            5, 0.5, 6.0, false, Some("material.moss"),
        ),
        st(
            "status.entangled",
            "Entangled",
            StatusKind::Root,
            1, 0.0, 2.5, false, Some("material.moss"),
        ),
        st(
            "status.thornmail",
            "Thornmail",
            // Mechanically a damage buff in the closed set; flavoured as retaliatory bark.
            StatusKind::Empower { frac: 0.15 },
            1, 0.0, 8.0, true, Some("material.moss"),
        ),
        st(
            "status.overgrown",
            "Overgrown",
            StatusKind::Regen { hps: 10.0 },
            3, 1.0, 6.0, true, Some("material.moss"),
        ),
        // --- Stormcalling ---
        st(
            "status.static_charge",
            "Static Charge",
            StatusKind::DamageOverTime { element: ElementId::new("storm"), dps: 7.0 },
            6, 0.4, 5.0, false, Some("material.storm_cloud"),
        ),
        st(
            "status.conductive",
            "Conductive",
            // Marked targets take more damage — chains hit harder. Vulnerable models it.
            StatusKind::Vulnerable { frac: 0.25 },
            3, 0.0, 4.0, false, Some("material.storm_cloud"),
        ),
        st(
            "status.windswept",
            "Windswept",
            StatusKind::Haste { frac: 0.35 },
            1, 0.0, 5.0, true, None,
        ),
        st(
            "status.levitating",
            "Levitating",
            StatusKind::Levitate,
            1, 0.0, 3.0, false, Some("material.storm_cloud"),
        ),
        // --- Necromancy ---
        st(
            "status.bleeding",
            "Bleeding",
            StatusKind::DamageOverTime { element: ElementId::new("blood"), dps: 11.0 },
            4, 0.5, 5.0, false, Some("material.blood"),
        ),
        st(
            "status.soul_burn",
            "Soul Burn",
            StatusKind::DamageOverTime { element: ElementId::new("shadow"), dps: 14.0 },
            3, 0.5, 6.0, false, Some("material.obsidian"),
        ),
        st(
            "status.cursed",
            "Cursed",
            StatusKind::ManaBurn { mps: 8.0 },
            1, 1.0, 6.0, false, Some("material.obsidian"),
        ),
        st(
            "status.silenced",
            "Silenced",
            StatusKind::Silence,
            1, 0.0, 2.5, false, Some("material.obsidian"),
        ),
        st(
            "status.withered",
            "Withered",
            StatusKind::Vulnerable { frac: 0.3 },
            1, 0.0, 6.0, false, Some("material.obsidian"),
        ),
        st(
            "status.petrified",
            "Petrified",
            StatusKind::Frozen,
            1, 0.0, 2.0, false, Some("material.bone"),
        ),
        // --- Chronomancy & Radiance ---
        st(
            "status.timewarp",
            "Time-Warped",
            StatusKind::Haste { frac: 0.6 },
            1, 0.0, 5.0, true, Some("material.gilded"),
        ),
        st(
            "status.timelock",
            "Time-Locked",
            StatusKind::Slow { frac: 0.7 },
            1, 0.0, 3.0, false, Some("material.gilded"),
        ),
        st(
            "status.sanctified",
            "Sanctified",
            StatusKind::Empower { frac: 0.3 },
            1, 0.0, 8.0, true, Some("material.gilded"),
        ),
        st(
            "status.radiant_ward",
            "Radiant Ward",
            StatusKind::Shielded { amount: 120.0 },
            1, 0.0, 7.0, true, Some("material.gilded"),
        ),
    ]
}

// ===========================================================================
// Spells — composed entirely from the closed EffectOp set.
// ===========================================================================

fn spells() -> Vec<SpellDef> {
    vec![
        // ---------------- Stormcalling ----------------
        spell(
            "spell.storm.spark",
            "Spark",
            "A snap of lightning that leaves the target crackling with charge.",
            "storm", 10.0, 0.15, 0.6, false,
            Scaling { power: 0.5, focus: 0.4, agility: 0.0, level: 0.4 },
            EffectOp::Ray {
                range: 30.0,
                pierce: 0,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 28.0, element: ElementId::new("storm") },
                    EffectOp::ApplyStatus { status: StatusId::new("status.static_charge"), duration_s: 5.0, stacks: 1 },
                ])),
            },
        ),
        spell(
            "spell.storm.thunderclap",
            "Thunderclap",
            "Detonate the air around you, deafening and flinging back nearby foes.",
            "storm", 24.0, 0.3, 4.0, false,
            Scaling { power: 0.7, focus: 0.3, agility: 0.0, level: 0.5 },
            EffectOp::Area {
                radius: 6.0, faction: Faction::Enemies, falloff: 0.4,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 45.0, element: ElementId::new("storm") },
                    EffectOp::Impulse { force: 16.0, vertical_bias: 0.3 },
                    EffectOp::ApplyStatus { status: StatusId::new("status.silenced"), duration_s: 1.5, stacks: 1 },
                ])),
            },
        ),
        spell(
            "spell.storm.gale_step",
            "Gale Step",
            "Ride a gust forward, leaving allies behind you hastened by the draft.",
            "wind", 18.0, 0.0, 6.0, false,
            Scaling { power: 0.0, focus: 0.4, agility: 0.6, level: 0.4 },
            EffectOp::Sequence(vec![
                EffectOp::Teleport { max_distance: 10.0, to_target: false },
                EffectOp::Area {
                    radius: 5.0, faction: Faction::Allies, falloff: 1.0,
                    then: b(EffectOp::ApplyStatus { status: StatusId::new("status.windswept"), duration_s: 5.0, stacks: 1 }),
                },
            ]),
        ),
        spell(
            "spell.storm.tempest",
            "Tempest",
            "Anchor a screaming storm cell that shocks, charges, and lifts the unwary.",
            "storm", 60.0, 1.0, 14.0, false,
            Scaling { power: 0.8, focus: 0.6, agility: 0.0, level: 0.8 },
            EffectOp::Field {
                radius: 8.0, duration_s: 8.0, interval_s: 0.5, faction: Faction::Enemies,
                tick: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 12.0, element: ElementId::new("storm") },
                    EffectOp::Chance {
                        chance: 0.25,
                        then: b(EffectOp::ApplyStatus { status: StatusId::new("status.levitating"), duration_s: 1.5, stacks: 1 }),
                    },
                    EffectOp::ApplyStatus { status: StatusId::new("status.static_charge"), duration_s: 3.0, stacks: 1 },
                ])),
            },
        ),
        spell(
            "spell.storm.ball_lightning",
            "Ball Lightning",
            "Loose a slow, homing orb of lightning that forks into nearby foes on contact.",
            "storm", 34.0, 0.5, 7.0, false,
            Scaling { power: 0.9, focus: 0.4, agility: 0.0, level: 0.6 },
            EffectOp::Projectile {
                speed: 18.0, gravity: 0.0, radius: 0.6, lifetime_s: 5.0, homing: 0.7,
                on_hit: b(EffectOp::Area {
                    radius: 5.0, faction: Faction::Enemies, falloff: 0.3,
                    then: b(EffectOp::Repeat {
                        count: 3, interval_s: 0.1,
                        op: b(EffectOp::Damage { amount: 22.0, element: ElementId::new("storm") }),
                    }),
                }),
            },
        ),
        // ---------------- Verdancy ----------------
        spell(
            "spell.verdance.thornlash",
            "Thornlash",
            "A whip of barbed vine that rakes a foe and leaves them festering with poison.",
            "nature", 14.0, 0.25, 1.2, false,
            Scaling { power: 0.6, focus: 0.3, agility: 0.1, level: 0.4 },
            EffectOp::Ray {
                range: 18.0, pierce: 1,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 32.0, element: ElementId::new("nature") },
                    EffectOp::ApplyStatus { status: StatusId::new("status.poisoned"), duration_s: 6.0, stacks: 2 },
                ])),
            },
        ),
        spell(
            "spell.verdance.entangle",
            "Entangling Roots",
            "Burst roots from the soil that seize and hold everything in a patch of ground.",
            "nature", 26.0, 0.4, 6.0, false,
            Scaling { power: 0.3, focus: 0.6, agility: 0.0, level: 0.5 },
            EffectOp::Area {
                radius: 5.0, faction: Faction::Enemies, falloff: 1.0,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::ApplyStatus { status: StatusId::new("status.entangled"), duration_s: 3.0, stacks: 1 },
                    EffectOp::Damage { amount: 18.0, element: ElementId::new("nature") },
                ])),
            },
        ),
        spell(
            "spell.verdance.thornmail",
            "Barkskin",
            "Sheathe yourself in living bark, hardening your strikes for a time.",
            "nature", 20.0, 0.3, 12.0, false,
            Scaling::default(),
            EffectOp::Sequence(vec![
                EffectOp::Shield { amount: 60.0, duration_s: 8.0 },
                EffectOp::ApplyStatus { status: StatusId::new("status.thornmail"), duration_s: 8.0, stacks: 1 },
            ]),
        ),
        spell(
            "spell.verdance.grove",
            "Living Grove",
            "Coax a grove of light from the earth that knits the wounds of all who shelter in it.",
            "life", 40.0, 0.8, 10.0, false,
            Scaling { power: 0.0, focus: 1.0, agility: 0.0, level: 0.7 },
            EffectOp::Field {
                radius: 6.0, duration_s: 8.0, interval_s: 1.0, faction: Faction::Allies,
                tick: b(EffectOp::Sequence(vec![
                    EffectOp::Heal { amount: 18.0 },
                    EffectOp::ApplyStatus { status: StatusId::new("status.overgrown"), duration_s: 2.0, stacks: 1 },
                ])),
            },
        ),
        spell(
            "spell.verdance.spore_burst",
            "Spore Burst",
            "Fling a fat spore pod that erupts into a choking cloud of toxins.",
            "nature", 28.0, 0.5, 5.0, false,
            Scaling { power: 0.7, focus: 0.4, agility: 0.0, level: 0.5 },
            EffectOp::Projectile {
                speed: 24.0, gravity: 4.0, radius: 0.5, lifetime_s: 4.0, homing: 0.0,
                on_hit: b(EffectOp::Field {
                    radius: 4.0, duration_s: 5.0, interval_s: 0.75, faction: Faction::Enemies,
                    tick: b(EffectOp::ApplyStatus { status: StatusId::new("status.poisoned"), duration_s: 3.0, stacks: 1 }),
                }),
            },
        ),
        spell(
            "spell.verdance.summon_treant",
            "Summon Treant",
            "Wake an ancient treant from a fallen log to lumber to your defence.",
            "nature", 48.0, 1.2, 20.0, false,
            Scaling { power: 0.0, focus: 0.9, agility: 0.0, level: 0.8 },
            EffectOp::Summon { mob: MobId::new("mob.verdance.treant"), count: 1, duration_s: 40.0 },
        ),
        // ---------------- Necromancy ----------------
        spell(
            "spell.dead.shadow_bolt",
            "Shadow Bolt",
            "A clot of unlight that sears the soul long after it strikes.",
            "shadow", 18.0, 0.35, 1.4, false,
            Scaling { power: 0.8, focus: 0.3, agility: 0.0, level: 0.5 },
            EffectOp::Projectile {
                speed: 38.0, gravity: 0.0, radius: 0.35, lifetime_s: 3.0, homing: 0.2,
                on_hit: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 42.0, element: ElementId::new("shadow") },
                    EffectOp::ApplyStatus { status: StatusId::new("status.soul_burn"), duration_s: 6.0, stacks: 1 },
                ])),
            },
        ),
        spell(
            "spell.dead.hemorrhage",
            "Hemorrhage",
            "Open a wound that will not close, bleeding the target with every motion.",
            "blood", 16.0, 0.25, 2.0, false,
            Scaling { power: 0.7, focus: 0.2, agility: 0.1, level: 0.4 },
            EffectOp::Ray {
                range: 22.0, pierce: 0,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 30.0, element: ElementId::new("blood") },
                    EffectOp::ApplyStatus { status: StatusId::new("status.bleeding"), duration_s: 5.0, stacks: 2 },
                ])),
            },
        ),
        spell(
            "spell.dead.curse_of_silence",
            "Curse of Silence",
            "Lay a hex that gnaws the mana from a foe and stills their tongue.",
            "shadow", 30.0, 0.5, 8.0, false,
            Scaling { power: 0.3, focus: 0.7, agility: 0.0, level: 0.5 },
            EffectOp::Ray {
                range: 25.0, pierce: 0,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::ApplyStatus { status: StatusId::new("status.cursed"), duration_s: 6.0, stacks: 1 },
                    EffectOp::ApplyStatus { status: StatusId::new("status.silenced"), duration_s: 2.5, stacks: 1 },
                    EffectOp::ApplyStatus { status: StatusId::new("status.withered"), duration_s: 6.0, stacks: 1 },
                ])),
            },
        ),
        spell(
            "spell.dead.harvest",
            "Blood Harvest",
            "A channelled scythe of red mist that reaps health from all it touches into you.",
            "blood", 10.0, 0.0, 1.0, true,
            Scaling { power: 0.5, focus: 0.5, agility: 0.0, level: 0.5 },
            EffectOp::Cone {
                range: 8.0, half_angle_rad: 0.7, faction: Faction::Enemies,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 16.0, element: ElementId::new("blood") },
                    EffectOp::Heal { amount: 9.0 },
                ])),
            },
        ),
        spell(
            "spell.dead.raise_dead",
            "Raise the Hollow Dead",
            "Tear two risen husks from the ground to shamble after your enemies.",
            "shadow", 50.0, 1.3, 22.0, false,
            Scaling { power: 0.0, focus: 0.8, agility: 0.0, level: 0.8 },
            EffectOp::Summon { mob: MobId::new("mob.dead.risen_husk"), count: 2, duration_s: 35.0 },
        ),
        spell(
            "spell.dead.doom",
            "Mark of Doom",
            "Brand a foe; if the mark is still set when it expires, doom erupts upon them.",
            "shadow", 36.0, 0.6, 10.0, false,
            Scaling { power: 1.0, focus: 0.5, agility: 0.0, level: 0.7 },
            EffectOp::Ray {
                range: 30.0, pierce: 0,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Mark { tag: "doom".into(), duration_s: 4.0 },
                    EffectOp::Delay {
                        secs: 4.0,
                        then: b(EffectOp::IfMarked {
                            tag: "doom".into(),
                            then: b(EffectOp::Area {
                                radius: 5.0, faction: Faction::Enemies, falloff: 0.5,
                                then: b(EffectOp::Damage { amount: 140.0, element: ElementId::new("shadow") }),
                            }),
                            otherwise: b(EffectOp::Noop),
                        }),
                    },
                ])),
            },
        ),
        // ---------------- Chronomancy ----------------
        spell(
            "spell.chrono.haste",
            "Quicken",
            "Spin your own thread of time faster, blurring into hastened motion.",
            "chrono", 28.0, 0.0, 14.0, false,
            Scaling::default(),
            EffectOp::ApplyStatus { status: StatusId::new("status.timewarp"), duration_s: 5.0, stacks: 1 },
        ),
        spell(
            "spell.chrono.stasis",
            "Stasis Field",
            "Thicken time in a patch of ground until those caught within crawl as if mired.",
            "chrono", 38.0, 0.6, 12.0, false,
            Scaling { power: 0.0, focus: 0.7, agility: 0.0, level: 0.6 },
            EffectOp::Area {
                radius: 5.0, faction: Faction::Enemies, falloff: 1.0,
                then: b(EffectOp::ApplyStatus { status: StatusId::new("status.timelock"), duration_s: 3.0, stacks: 1 }),
            },
        ),
        spell(
            "spell.chrono.echo_strike",
            "Echo Strike",
            "Strike once now and twice from echoes of the next instant.",
            "chrono", 32.0, 0.3, 6.0, false,
            Scaling { power: 0.8, focus: 0.5, agility: 0.2, level: 0.6 },
            EffectOp::Ray {
                range: 28.0, pierce: 0,
                then: b(EffectOp::Repeat {
                    count: 3, interval_s: 0.25,
                    op: b(EffectOp::Damage { amount: 34.0, element: ElementId::new("chrono") }),
                }),
            },
        ),
        // ---------------- Radiance ----------------
        spell(
            "spell.radiant.smite",
            "Smite",
            "Call a pillar of cleansing light down upon a single foe.",
            "radiant", 22.0, 0.4, 2.0, false,
            Scaling { power: 0.9, focus: 0.4, agility: 0.0, level: 0.5 },
            EffectOp::Projectile {
                speed: 60.0, gravity: 0.0, radius: 0.5, lifetime_s: 2.0, homing: 0.3,
                on_hit: b(EffectOp::Damage { amount: 58.0, element: ElementId::new("radiant") }),
            },
        ),
        spell(
            "spell.radiant.consecrate",
            "Consecrate",
            "Sanctify the ground, blessing allies and searing the undead who tread it.",
            "radiant", 44.0, 0.8, 12.0, false,
            Scaling { power: 0.5, focus: 0.7, agility: 0.0, level: 0.7 },
            EffectOp::Parallel(vec![
                EffectOp::Field {
                    radius: 6.0, duration_s: 8.0, interval_s: 1.0, faction: Faction::Allies,
                    tick: b(EffectOp::ApplyStatus { status: StatusId::new("status.sanctified"), duration_s: 2.0, stacks: 1 }),
                },
                EffectOp::Field {
                    radius: 6.0, duration_s: 8.0, interval_s: 0.5, faction: Faction::Enemies,
                    tick: b(EffectOp::Damage { amount: 14.0, element: ElementId::new("radiant") }),
                },
            ]),
        ),
        spell(
            "spell.radiant.aegis",
            "Aegis of Light",
            "Wrap an ally in a radiant ward that drinks the next blows aimed at them.",
            "radiant", 30.0, 0.3, 9.0, false,
            Scaling::default(),
            EffectOp::Sequence(vec![
                EffectOp::Shield { amount: 100.0, duration_s: 7.0 },
                EffectOp::ApplyStatus { status: StatusId::new("status.radiant_ward"), duration_s: 7.0, stacks: 1 },
            ]),
        ),
        // ---------------- Reagent-driven consumable spells ----------------
        spell(
            "spell.consume.elixir_vigor",
            "Elixir of Vigor",
            "A draught that floods the body with healing warmth.",
            "life", 0.0, 0.0, 0.0, false,
            Scaling::default(),
            EffectOp::Sequence(vec![
                EffectOp::Heal { amount: 120.0 },
                EffectOp::ApplyStatus { status: StatusId::new("status.overgrown"), duration_s: 4.0, stacks: 1 },
            ]),
        ),
        spell(
            "spell.consume.smoke_bomb",
            "Smoke Bomb",
            "Shatter a phial of shadow-smoke and vanish from sight.",
            "shadow", 0.0, 0.0, 0.0, false,
            Scaling::default(),
            EffectOp::ApplyStatus { status: StatusId::new("status.invisible"), duration_s: 5.0, stacks: 1 },
        ),
    ]
}

// ===========================================================================
// Movement modes — new kinetics from the closed MovementKind set.
// ===========================================================================

fn movement_modes() -> Vec<MovementModeDef> {
    fn mv(id: &str, name: &str, kind: MovementKind, mana: f32, cd: f32, stam: f32) -> MovementModeDef {
        MovementModeDef { id: MovementModeId::new(id), name: name.into(), kind, mana_cost: mana, cooldown: cd, stamina_cost: stam }
    }
    vec![
        mv("movement.updraft", "Updraft", MovementKind::DoubleJump { extra_jumps: 2, impulse: 9.0 }, 8.0, 0.5, 8.0),
        mv("movement.shadowstep", "Shadowstep", MovementKind::Blink { distance: 12.0 }, 18.0, 3.0, 0.0),
        mv("movement.vine_swing", "Vine Swing", MovementKind::Grapple { range: 36.0, pull_speed: 26.0 }, 4.0, 1.5, 0.0),
        mv("movement.tide_slide", "Tide Slide", MovementKind::Slide { speed: 18.0, duration_s: 1.4 }, 0.0, 0.8, 7.0),
        mv("movement.gale_sprint", "Gale Sprint", MovementKind::Sprint { speed_mult: 1.9 }, 0.0, 0.0, 5.0),
        mv("movement.feather_fall", "Feather Fall", MovementKind::Glide { fall_mult: 0.2, forward_boost: 8.0 }, 0.0, 0.0, 4.0),
        mv("movement.comet_dive", "Comet Dive", MovementKind::GroundSlam { damage: 70.0, radius: 6.0, down_speed: 55.0 }, 10.0, 4.0, 22.0),
        mv("movement.spirit_run", "Spirit Wall-Run", MovementKind::WallRun { max_time_s: 4.0, speed: 12.0, gravity_mult: 0.15 }, 0.0, 0.0, 10.0),
    ]
}

// ===========================================================================
// Materials & shaders for the new schools and biomes.
// ===========================================================================

fn materials() -> Vec<MaterialDef> {
    fn fbm(frequency: f32, octaves: u8, warp: f32, seed: u32) -> NoiseLayer {
        NoiseLayer { kind: NoiseKind::Fbm, frequency, amplitude: 1.0, octaves, lacunarity: 2.0, gain: 0.5, warp, seed }
    }
    fn mat(
        id: &str, name: &str, layers: Vec<NoiseLayer>, stops: Vec<(f32, [f32; 4])>,
        roughness: f32, metallic: f32, emissive: f32, emissive_color: [f32; 3],
        triplanar_scale: f32, displacement: f32, shader: &str,
    ) -> MaterialDef {
        MaterialDef {
            id: MaterialId::new(id), name: name.into(), layers,
            ramp: ColorRamp { stops }, roughness, metallic, emissive, emissive_color,
            triplanar_scale, displacement, shader: Some(ShaderId::new(shader)),
        }
    }
    vec![
        mat("material.moss", "Living Moss",
            vec![fbm(9.0, 4, 0.7, 71), NoiseLayer { kind: NoiseKind::Worley, frequency: 14.0, amplitude: 0.3, octaves: 2, lacunarity: 2.0, gain: 0.5, warp: 0.4, seed: 72 }],
            vec![(0.0, [0.04, 0.14, 0.05, 1.0]), (0.6, [0.12, 0.34, 0.12, 1.0]), (1.0, [0.35, 0.6, 0.28, 1.0])],
            0.85, 0.0, 0.2, [0.2, 0.55, 0.25], 1.0, 0.2, "shader.foliage"),
        mat("material.storm_cloud", "Storm Cloud",
            vec![NoiseLayer { kind: NoiseKind::DomainWarp, frequency: 1.2, amplitude: 1.0, octaves: 5, lacunarity: 2.1, gain: 0.55, warp: 1.0, seed: 81 }],
            vec![(0.0, [0.06, 0.07, 0.1, 1.0]), (0.6, [0.2, 0.22, 0.3, 1.0]), (1.0, [0.55, 0.6, 0.75, 1.0])],
            0.9, 0.0, 0.5, [0.5, 0.6, 1.0], 1.4, 0.1, "shader.lightning"),
        mat("material.blood", "Clotted Blood",
            vec![NoiseLayer { kind: NoiseKind::Flow, frequency: 3.0, amplitude: 1.0, octaves: 4, lacunarity: 2.0, gain: 0.55, warp: 0.6, seed: 91 }],
            vec![(0.0, [0.12, 0.0, 0.0, 1.0]), (0.6, [0.45, 0.03, 0.04, 1.0]), (1.0, [0.75, 0.1, 0.12, 1.0])],
            0.5, 0.0, 0.3, [0.6, 0.05, 0.05], 0.7, 0.2, "shader.surface_triplanar"),
        mat("material.obsidian", "Hollow Obsidian",
            vec![NoiseLayer { kind: NoiseKind::Ridged, frequency: 6.0, amplitude: 1.0, octaves: 5, lacunarity: 2.2, gain: 0.5, warp: 0.2, seed: 101 }],
            vec![(0.0, [0.02, 0.02, 0.04, 1.0]), (0.6, [0.08, 0.06, 0.12, 1.0]), (1.0, [0.22, 0.16, 0.3, 1.0])],
            0.25, 0.2, 0.4, [0.4, 0.1, 0.5], 0.8, 0.4, "shader.dissolve"),
        mat("material.bone", "Bleached Bone",
            vec![fbm(5.0, 4, 0.3, 111), NoiseLayer { kind: NoiseKind::Worley, frequency: 10.0, amplitude: 0.3, octaves: 2, lacunarity: 2.0, gain: 0.5, warp: 0.1, seed: 112 }],
            vec![(0.0, [0.5, 0.48, 0.42, 1.0]), (0.6, [0.78, 0.74, 0.64, 1.0]), (1.0, [0.92, 0.9, 0.82, 1.0])],
            0.8, 0.0, 0.0, [0.0, 0.0, 0.0], 0.6, 0.3, "shader.surface_triplanar"),
        mat("material.gilded", "Gilded Radiance",
            vec![NoiseLayer { kind: NoiseKind::Ridged, frequency: 4.0, amplitude: 1.0, octaves: 4, lacunarity: 2.0, gain: 0.5, warp: 0.15, seed: 121 }],
            vec![(0.0, [0.3, 0.22, 0.05, 1.0]), (0.6, [0.85, 0.65, 0.2, 1.0]), (1.0, [1.0, 0.95, 0.6, 1.0])],
            0.2, 0.8, 1.2, [1.0, 0.85, 0.4], 0.7, 0.2, "shader.surface_triplanar"),
        mat("material.bog_water", "Bog Water",
            vec![NoiseLayer { kind: NoiseKind::Flow, frequency: 2.0, amplitude: 1.0, octaves: 3, lacunarity: 2.0, gain: 0.5, warp: 0.5, seed: 131 }],
            vec![(0.0, [0.04, 0.08, 0.05, 1.0]), (0.6, [0.1, 0.18, 0.12, 1.0]), (1.0, [0.2, 0.3, 0.22, 1.0])],
            0.3, 0.0, 0.1, [0.1, 0.25, 0.15], 1.2, 0.05, "shader.water"),
    ]
}

fn shaders() -> Vec<ShaderDef> {
    vec![
        ShaderDef {
            id: ShaderId::new("shader.foliage"),
            name: "Wind-Swayed Foliage".into(),
            stage: ShaderStage::Surface,
            params: vec![("sway_amount".into(), 0.15), ("sway_speed".into(), 1.2)],
            source: r#"
// Foliage surface: a vertical sway driven by world position + time, with a
// subsurface-tinted wrapped-Lambert pass so leaves glow when back-lit.
fn sway(world_pos: vec3<f32>, time: f32, amount: f32, speed: f32) -> vec3<f32> {
    let phase = world_pos.x * 0.3 + world_pos.z * 0.27 + time * speed;
    return vec3<f32>(sin(phase) * amount, 0.0, cos(phase * 0.8) * amount);
}

fn shade_leaf(normal: vec3<f32>, light_dir: vec3<f32>, base: vec3<f32>) -> vec3<f32> {
    let n = normalize(normal);
    let ndl = clamp(dot(n, normalize(light_dir)), 0.0, 1.0);
    let back = clamp(dot(-n, normalize(light_dir)), 0.0, 1.0);
    let subsurface = base * vec3<f32>(0.3, 0.6, 0.25) * back;
    return base * (0.3 + 0.7 * ndl) + subsurface * 0.5;
}
"#.into(),
        },
        ShaderDef {
            id: ShaderId::new("shader.lightning"),
            name: "Arc Lightning".into(),
            stage: ShaderStage::Surface,
            params: vec![("arc_freq".into(), 8.0), ("flicker".into(), 0.7)],
            source: r#"
// Arc lightning surface: ridged noise threshold carves bright filaments that
// flicker over time; emissive output spikes along the arcs.
fn hashv(p: vec2<f32>) -> f32 {
    return fract(sin(dot(p, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

fn arc(uv: vec2<f32>, time: f32, freq: f32, flicker: f32) -> f32 {
    let v = abs(sin((uv.x + hashv(floor(uv * freq))) * freq + time * 6.0));
    let mask = smoothstep(0.85, 1.0, 1.0 - v);
    let f = mix(1.0 - flicker, 1.0, hashv(vec2<f32>(floor(time * 20.0), 0.0)));
    return mask * f;
}
"#.into(),
        },
        ShaderDef {
            id: ShaderId::new("shader.dissolve"),
            name: "Void Dissolve".into(),
            stage: ShaderStage::Surface,
            params: vec![("edge_width".into(), 0.06), ("threshold".into(), 0.5)],
            source: r#"
// Dissolve surface: a noise field clips fragments below `threshold`, with a hot
// emissive rim along the dissolving edge — used for summons fading in/out and the
// hollow obsidian of the dead.
fn dissolve(noise: f32, threshold: f32, edge: f32) -> vec2<f32> {
    let alpha = step(threshold, noise);
    let rim = smoothstep(threshold, threshold + edge, noise) * (1.0 - alpha + edge);
    return vec2<f32>(alpha, clamp(rim, 0.0, 1.0));
}
"#.into(),
        },
        ShaderDef {
            id: ShaderId::new("shader.bloom_post"),
            name: "Radiant Bloom".into(),
            stage: ShaderStage::Fullscreen,
            params: vec![("threshold".into(), 0.8), ("intensity".into(), 0.6)],
            source: r#"
// Fullscreen bloom post: extract bright pixels above `threshold` and add a cheap
// boxed blur of them back, weighted by `intensity`. Gives emissive spells and
// gilded radiance their glow without per-light cost.
fn extract(color: vec3<f32>, threshold: f32) -> vec3<f32> {
    let luma = dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
    let k = max(luma - threshold, 0.0) / max(luma, 1e-4);
    return color * k;
}

fn composite(scene: vec3<f32>, blurred: vec3<f32>, intensity: f32) -> vec3<f32> {
    return scene + blurred * intensity;
}
"#.into(),
        },
    ]
}

// ===========================================================================
// Mobs — new creatures, summons, and two bosses.
// ===========================================================================

fn mobs() -> Vec<MobDef> {
    fn mob(
        id: &str, name: &str, hp: f32, speed: f32, abilities: &[&str], xp: u32,
        loot: &[(&str, f32)], material: &str, scale: f32, aggressive: bool, seed: u32,
    ) -> MobDef {
        MobDef {
            id: MobId::new(id), name: name.into(), max_health: hp, move_speed: speed,
            abilities: abilities.iter().map(|a| AbilityId::new(*a)).collect(),
            xp_reward: xp,
            loot_table: loot.iter().map(|(i, c)| (ItemId::new(*i), *c)).collect(),
            material: Some(MaterialId::new(material)), scale, aggressive, mesh_seed: seed,
        }
    }
    vec![
        // --- Verdancy ---
        mob("mob.verdance.thornling", "Thornling", 60.0, 4.5,
            &["ability.verdance.thornlash"], 25, &[("item.verdance.seed_pod", 0.5)],
            "material.moss", 0.8, true, 0x8101),
        mob("mob.verdance.treant", "Ancient Treant", 320.0, 2.6,
            &["ability.verdance.entangle", "ability.verdance.thornlash"], 130,
            &[("item.verdance.heartwood", 0.4), ("item.crystal_shard", 0.5)],
            "material.moss", 3.2, false, 0x8102),
        mob("mob.verdance.grove_warden", "Grove Warden", 480.0, 3.0,
            &["ability.verdance.entangle", "ability.verdance.grove", "ability.verdance.spore_burst"], 320,
            &[("item.verdance.heartwood", 0.9), ("item.verdance.staff_thorn", 0.06)],
            "material.moss", 3.8, true, 0x8103),
        // --- Stormcalling ---
        mob("mob.storm.elemental", "Storm Elemental", 150.0, 5.0,
            &["ability.storm.spark", "ability.storm.ball_lightning"], 95,
            &[("item.storm.charged_core", 0.5)], "material.storm_cloud", 1.8, true, 0x8201),
        mob("mob.storm.djinn", "Tempest Djinn", 620.0, 6.0,
            &["ability.storm.tempest", "ability.storm.thunderclap", "ability.storm.ball_lightning"], 420,
            &[("item.storm.charged_core", 1.0), ("item.storm.ring_tempest", 0.07)],
            "material.storm_cloud", 3.5, true, 0x8202),
        // --- Necromancy / the Hollow Dead ---
        mob("mob.dead.risen_husk", "Risen Husk", 70.0, 3.2,
            &["ability.dead.hemorrhage"], 20, &[("item.dead.bone_dust", 0.6)],
            "material.bone", 1.0, true, 0x8301),
        mob("mob.dead.shade", "Wailing Shade", 110.0, 5.5,
            &["ability.dead.shadow_bolt", "ability.dead.curse_of_silence"], 80,
            &[("item.dead.shadow_silk", 0.5)], "material.obsidian", 1.4, true, 0x8302),
        mob("mob.dead.blood_acolyte", "Blood Acolyte", 180.0, 4.0,
            &["ability.dead.hemorrhage", "ability.dead.harvest"], 120,
            &[("item.dead.shadow_silk", 0.4), ("item.void_essence", 0.3)],
            "material.blood", 1.6, true, 0x8303),
        mob("mob.dead.bone_colossus", "Bone Colossus", 1400.0, 2.4,
            &["ability.dead.doom", "ability.dead.raise_dead", "ability.dead.shadow_bolt"], 900,
            &[("item.dead.bone_dust", 1.0), ("item.dead.amulet_lich", 0.05), ("item.void_relic", 0.08)],
            "material.bone", 5.0, true, 0x8304),
        // --- Radiance (neutral guardians) ---
        mob("mob.radiant.seraph", "Lesser Seraph", 260.0, 5.5,
            &["ability.radiant.smite", "ability.radiant.aegis"], 200,
            &[("item.radiant.gilded_feather", 0.7)], "material.gilded", 2.0, false, 0x8401),
    ]
}

// ===========================================================================
// Items — gear, reagents, consumables for the new schools, with craft recipes.
// ===========================================================================

fn items() -> Vec<ItemDef> {
    #[allow(clippy::too_many_arguments)]
    fn item(
        id: &str, name: &str, desc: &str, rarity: Rarity, slot: EquipSlot, mods: StatMods,
        spells: &[&str], movement: &[&str], abilities: &[&str], material: &str,
        stackable: bool, max_stack: u16, on_use: Option<&str>, level_req: u32, craft: Option<CraftRecipe>,
    ) -> ItemDef {
        ItemDef {
            id: ItemId::new(id), name: name.into(), description: desc.into(), rarity, slot,
            stat_mods: mods,
            grants_spells: spells.iter().map(|s| SpellId::new(*s)).collect(),
            grants_movement: movement.iter().map(|m| MovementModeId::new(*m)).collect(),
            grants_abilities: abilities.iter().map(|a| AbilityId::new(*a)).collect(),
            material: Some(MaterialId::new(material)), stackable, max_stack,
            on_use: on_use.map(SpellId::new), level_req, craft,
            ..Default::default()
        }
    }
    vec![
        // ---- Reagents ----
        item("item.verdance.seed_pod", "Seed Pod", "A swollen pod of volatile spores. A reagent.",
            Rarity::Common, EquipSlot::None, StatMods::default(), &[], &[], &[], "material.moss",
            true, 99, None, 0, None),
        item("item.verdance.heartwood", "Heartwood", "A knot of living wood that will not rot. A reagent.",
            Rarity::Uncommon, EquipSlot::None, StatMods::default(), &[], &[], &[], "material.moss",
            true, 99, None, 0, None),
        item("item.storm.charged_core", "Charged Core", "A core that hums and stings to the touch. A reagent.",
            Rarity::Uncommon, EquipSlot::None, StatMods::default(), &[], &[], &[], "material.storm_cloud",
            true, 99, None, 0, None),
        item("item.dead.bone_dust", "Bone Dust", "Ground remnants of the restless dead. A reagent.",
            Rarity::Common, EquipSlot::None, StatMods::default(), &[], &[], &[], "material.bone",
            true, 99, None, 0, None),
        item("item.dead.shadow_silk", "Shadow Silk", "Thread spun from a shade's wail. A reagent.",
            Rarity::Rare, EquipSlot::None, StatMods::default(), &[], &[], &[], "material.obsidian",
            true, 99, None, 0, None),
        item("item.radiant.gilded_feather", "Gilded Feather", "A feather that holds its own light. A reagent.",
            Rarity::Rare, EquipSlot::None, StatMods::default(), &[], &[], &[], "material.gilded",
            true, 99, None, 0, None),
        // ---- Staves / focuses (spell sources) ----
        item("item.storm.staff_tempest", "Tempest Staff", "A rod of fulgurite that drinks the sky's anger.",
            Rarity::Epic, EquipSlot::Staff, StatMods { power: 14.0, focus: 8.0, spell_power_pct: 0.1, ..Default::default() },
            &["spell.storm.spark", "spell.storm.ball_lightning"], &[], &["ability.storm.spark"], "material.storm_cloud",
            false, 1, None, 6, None),
        item("item.verdance.staff_thorn", "Thornwood Staff", "A staff still budding, alive in your grip.",
            Rarity::Epic, EquipSlot::Staff, StatMods { focus: 12.0, max_mana: 40.0, mana_regen: 2.0, ..Default::default() },
            &["spell.verdance.thornlash", "spell.verdance.entangle"], &[], &["ability.verdance.thornlash"], "material.moss",
            false, 1, None, 6, Some(CraftRecipe { inputs: vec![(ItemId::new("item.verdance.heartwood"), 4), (ItemId::new("item.crystal_shard"), 2)], tech_req: Some(TechNodeId::new("tech.verdance_2")) })),
        item("item.dead.scepter_grave", "Grave Scepter", "Bone fused to black iron; it remembers every death it dealt.",
            Rarity::Legendary, EquipSlot::Staff, StatMods { power: 18.0, focus: 10.0, spell_power_pct: 0.14, ..Default::default() },
            &["spell.dead.shadow_bolt", "spell.dead.hemorrhage", "spell.dead.harvest"], &[], &["ability.dead.shadow_bolt"], "material.bone",
            false, 1, None, 14, None),
        // ---- Relics / orbs ----
        item("item.chrono.hourglass", "Sand-Bound Hourglass", "Time pools in it like honey; tilt it and the world lurches.",
            Rarity::Legendary, EquipSlot::Relic, StatMods { focus: 16.0, cooldown_reduction: 0.12, max_mana: 60.0, ..Default::default() },
            &["spell.chrono.haste", "spell.chrono.stasis", "spell.chrono.echo_strike"], &[], &["ability.chrono.haste"], "material.gilded",
            false, 1, None, 16, Some(CraftRecipe { inputs: vec![(ItemId::new("item.radiant.gilded_feather"), 3), (ItemId::new("item.crystal_shard"), 6)], tech_req: Some(TechNodeId::new("tech.chrono_2")) })),
        item("item.dead.amulet_lich", "Lich's Phylactery", "A cold locket that knots your soul to the world. It is not warm to wear.",
            Rarity::Mythic, EquipSlot::Amulet, StatMods { power: 22.0, focus: 14.0, max_health: 60.0, spell_power_pct: 0.18, ..Default::default() },
            &["spell.dead.raise_dead", "spell.dead.doom"], &[], &["ability.dead.raise_dead"], "material.obsidian",
            false, 1, None, 20, None),
        // ---- Rings / radiant focus ----
        item("item.storm.ring_tempest", "Ring of the Tempest", "A band that thrums before the thunder comes.",
            Rarity::Epic, EquipSlot::Ring, StatMods { power: 12.0, focus: 6.0, cooldown_reduction: 0.08, ..Default::default() },
            &["spell.storm.tempest"], &[], &["ability.storm.tempest"], "material.storm_cloud",
            false, 1, None, 10, None),
        item("item.radiant.crown_dawn", "Crown of Dawn", "A circlet of fixed sunlight worn by the wardens of the heights.",
            Rarity::Legendary, EquipSlot::Amulet, StatMods { focus: 18.0, vitality: 12.0, max_health: 70.0, spell_power_pct: 0.12, ..Default::default() },
            &["spell.radiant.smite", "spell.radiant.consecrate", "spell.radiant.aegis"], &[], &["ability.radiant.smite"], "material.gilded",
            false, 1, None, 15, None),
        // ---- Mobility gear ----
        item("item.storm.windsoles", "Windsoles", "Boots woven from gale and ghost-thread; you barely touch the ground.",
            Rarity::Rare, EquipSlot::Boots, StatMods { agility: 10.0, move_speed: 1.2, ..Default::default() },
            &[], &["movement.gale_sprint", "movement.feather_fall"], &[], "material.storm_cloud",
            false, 1, None, 5, None),
        item("item.dead.shadow_cloak", "Cloak of the Hollow", "Shadow stitched into cloth; step into it and step out elsewhere.",
            Rarity::Epic, EquipSlot::Trinket, StatMods { agility: 8.0, focus: 6.0, ..Default::default() },
            &[], &["movement.shadowstep"], &[], "material.obsidian",
            false, 1, None, 9, Some(CraftRecipe { inputs: vec![(ItemId::new("item.dead.shadow_silk"), 5), (ItemId::new("item.void_essence"), 2)], tech_req: Some(TechNodeId::new("tech.necro_3")) })),
        item("item.verdance.vinecaster", "Vinecaster Gauntlet", "A glove that grows a living vine to swing you through the canopy.",
            Rarity::Rare, EquipSlot::Trinket, StatMods { agility: 7.0, ..Default::default() },
            &[], &["movement.vine_swing"], &[], "material.moss",
            false, 1, None, 5, None),
        // ---- Consumables ----
        item("item.consume.vigor_elixir", "Elixir of Vigor", "A thick green draught that mends flesh in a rush of warmth.",
            Rarity::Uncommon, EquipSlot::Consumable, StatMods::default(), &[], &[], &[], "material.moss",
            true, 10, Some("spell.consume.elixir_vigor"), 1,
            Some(CraftRecipe { inputs: vec![(ItemId::new("item.verdance.seed_pod"), 3), (ItemId::new("item.crystal_shard"), 1)], tech_req: None })),
        item("item.consume.smoke_phial", "Shadow Phial", "Break it and the world loses sight of you.",
            Rarity::Rare, EquipSlot::Consumable, StatMods::default(), &[], &[], &[], "material.obsidian",
            true, 10, Some("spell.consume.smoke_bomb"), 1,
            Some(CraftRecipe { inputs: vec![(ItemId::new("item.dead.shadow_silk"), 2)], tech_req: Some(TechNodeId::new("tech.necro_1")) })),
    ]
}

// ===========================================================================
// Abilities — wire the new spells to inputs / slots.
// ===========================================================================

fn abilities() -> Vec<AbilityDef> {
    fn ab(id: &str, name: &str, spell: &str, binding: CastInput) -> AbilityDef {
        AbilityDef { id: AbilityId::new(id), name: name.into(), spell: SpellId::new(spell), binding, cooldown_override: None, icon_material: None }
    }
    vec![
        // Stormcalling
        ab("ability.storm.spark", "Spark", "spell.storm.spark", CastInput::Primary),
        ab("ability.storm.thunderclap", "Thunderclap", "spell.storm.thunderclap", CastInput::Slot(1)),
        ab("ability.storm.gale_step", "Gale Step", "spell.storm.gale_step", CastInput::Slot(2)),
        ab("ability.storm.tempest", "Tempest", "spell.storm.tempest", CastInput::Slot(3)),
        ab("ability.storm.ball_lightning", "Ball Lightning", "spell.storm.ball_lightning", CastInput::Secondary),
        // Verdancy
        ab("ability.verdance.thornlash", "Thornlash", "spell.verdance.thornlash", CastInput::Primary),
        ab("ability.verdance.entangle", "Entangling Roots", "spell.verdance.entangle", CastInput::Slot(4)),
        ab("ability.verdance.thornmail", "Barkskin", "spell.verdance.thornmail", CastInput::Slot(5)),
        ab("ability.verdance.grove", "Living Grove", "spell.verdance.grove", CastInput::Slot(6)),
        ab("ability.verdance.spore_burst", "Spore Burst", "spell.verdance.spore_burst", CastInput::Secondary),
        ab("ability.verdance.summon_treant", "Summon Treant", "spell.verdance.summon_treant", CastInput::Slot(7)),
        // Necromancy
        ab("ability.dead.shadow_bolt", "Shadow Bolt", "spell.dead.shadow_bolt", CastInput::Primary),
        ab("ability.dead.hemorrhage", "Hemorrhage", "spell.dead.hemorrhage", CastInput::Secondary),
        ab("ability.dead.curse_of_silence", "Curse of Silence", "spell.dead.curse_of_silence", CastInput::Slot(8)),
        ab("ability.dead.harvest", "Blood Harvest", "spell.dead.harvest", CastInput::Slot(1)),
        ab("ability.dead.raise_dead", "Raise the Hollow Dead", "spell.dead.raise_dead", CastInput::Slot(2)),
        ab("ability.dead.doom", "Mark of Doom", "spell.dead.doom", CastInput::Slot(3)),
        // Chronomancy
        ab("ability.chrono.haste", "Quicken", "spell.chrono.haste", CastInput::Slot(4)),
        ab("ability.chrono.stasis", "Stasis Field", "spell.chrono.stasis", CastInput::Slot(5)),
        ab("ability.chrono.echo_strike", "Echo Strike", "spell.chrono.echo_strike", CastInput::Secondary),
        // Radiance
        ab("ability.radiant.smite", "Smite", "spell.radiant.smite", CastInput::Primary),
        ab("ability.radiant.consecrate", "Consecrate", "spell.radiant.consecrate", CastInput::Slot(6)),
        ab("ability.radiant.aegis", "Aegis of Light", "spell.radiant.aegis", CastInput::Slot(7)),
    ]
}

// ===========================================================================
// Tech tree — four new branches grafted onto the existing tree.
// ===========================================================================

fn tech_nodes() -> Vec<TechNode> {
    fn node(
        id: &str, name: &str, description: &str, branch: &str, tier: u32, cost: u32,
        prereqs: &[&str], effects: Vec<TechEffect>, unlock_items: &[&str],
    ) -> TechNode {
        TechNode {
            id: TechNodeId::new(id), name: name.into(), description: description.into(),
            branch: branch.into(), tier, cost_skill_points: cost,
            prereqs: prereqs.iter().map(|p| TechNodeId::new(*p)).collect(),
            effects, unlock_items: unlock_items.iter().map(|i| ItemId::new(*i)).collect(),
            icon_material: None,
        }
    }
    vec![
        // --- Stormcalling ---
        node("tech.storm_1", "Static", "Feel the charge gather at your fingertips.",
            "Stormcalling", 0, 1, &[],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.storm.spark"))], &["item.storm.windsoles"]),
        node("tech.storm_2", "Conductor", "Lightning leaps where you will it; your sparks bite deeper.",
            "Stormcalling", 1, 2, &["tech.storm_1"],
            vec![TechEffect::StatMult(StatMods { power: 8.0, focus: 4.0, ..Default::default() }), TechEffect::UnlockAbility(AbilityId::new("ability.storm.ball_lightning"))],
            &["item.storm.staff_tempest"]),
        node("tech.storm_3", "Galecaller", "Bend the wind to carry you and your allies.",
            "Stormcalling", 2, 3, &["tech.storm_2"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.storm.gale_step")), TechEffect::UnlockMovement(MovementModeId::new("movement.gale_sprint"))],
            &[]),
        node("tech.storm_4", "Stormlord", "The tempest answers to your name.",
            "Stormcalling", 3, 4, &["tech.storm_3"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.storm.tempest")), TechEffect::RaiseSpellBudget { complexity: 4, depth: 2 }],
            &["item.storm.ring_tempest"]),
        // --- Verdancy ---
        node("tech.verdance_1", "First Bloom", "Awaken the green; lash with thorn and vine.",
            "Verdancy", 0, 1, &[],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.verdance.thornlash"))], &["item.verdance.vinecaster"]),
        node("tech.verdance_2", "Rootbinder", "Coax the soil to seize your foes and craft a living staff.",
            "Verdancy", 1, 2, &["tech.verdance_1"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.verdance.entangle")), TechEffect::UnlockRecipe(ItemId::new("item.verdance.staff_thorn")), TechEffect::UnlockMovement(MovementModeId::new("movement.vine_swing"))],
            &["item.verdance.staff_thorn"]),
        node("tech.verdance_3", "Lifeweaver", "Grow groves that mend, and harden your skin to bark.",
            "Verdancy", 2, 3, &["tech.verdance_2"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.verdance.grove")), TechEffect::GrantSpell(SpellId::new("spell.verdance.thornmail"))],
            &[]),
        node("tech.verdance_4", "Wildshaper", "Call the old forest's children to your side.",
            "Verdancy", 3, 4, &["tech.verdance_3"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.verdance.summon_treant")), TechEffect::StatMult(StatMods { focus: 12.0, max_health: 40.0, ..Default::default() })],
            &[]),
        // --- Necromancy ---
        node("tech.necro_1", "Touch of Death", "Reach across the veil; loose a bolt of unlight.",
            "Necromancy", 0, 1, &[],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.dead.shadow_bolt"))], &["item.consume.smoke_phial"]),
        node("tech.necro_2", "Bloodletter", "Make wounds that will not close; reap what bleeds.",
            "Necromancy", 1, 2, &["tech.necro_1"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.dead.hemorrhage")), TechEffect::UnlockAbility(AbilityId::new("ability.dead.harvest"))],
            &["item.dead.scepter_grave"]),
        node("tech.necro_3", "Hexweaver", "Bind tongues and burn souls; walk through shadow.",
            "Necromancy", 2, 3, &["tech.necro_2"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.dead.curse_of_silence")), TechEffect::UnlockMovement(MovementModeId::new("movement.shadowstep")), TechEffect::UnlockRecipe(ItemId::new("item.dead.shadow_cloak"))],
            &["item.dead.shadow_cloak"]),
        node("tech.necro_4", "Lichdom", "Raise the hollow dead and mark the living for doom.",
            "Necromancy", 3, 4, &["tech.necro_3"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.dead.raise_dead")), TechEffect::UnlockAbility(AbilityId::new("ability.dead.doom")), TechEffect::RaiseSpellBudget { complexity: 5, depth: 2 }],
            &["item.dead.amulet_lich"]),
        // --- Chronomancy ---
        node("tech.chrono_1", "Quickening", "Pull your own thread of time taut and fast.",
            "Chronomancy", 0, 2, &[],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.chrono.haste"))], &[]),
        node("tech.chrono_2", "Mire of Ages", "Thicken time around your foes until they wade through it.",
            "Chronomancy", 1, 3, &["tech.chrono_1"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.chrono.stasis")), TechEffect::UnlockRecipe(ItemId::new("item.chrono.hourglass"))],
            &["item.chrono.hourglass"]),
        node("tech.chrono_3", "Echoes", "Strike from the next instant as well as this one.",
            "Chronomancy", 2, 4, &["tech.chrono_2"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.chrono.echo_strike")), TechEffect::StatMult(StatMods { cooldown_reduction: 0.1, focus: 10.0, ..Default::default() })],
            &[]),
        // --- Radiance (a short prestige branch) ---
        node("tech.radiant_1", "Kindled Light", "Call down a mote of cleansing sun.",
            "Radiance", 0, 2, &[],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.radiant.smite"))], &[]),
        node("tech.radiant_2", "Warden of Dawn", "Sanctify the ground and ward your allies in light.",
            "Radiance", 1, 3, &["tech.radiant_1"],
            vec![TechEffect::UnlockAbility(AbilityId::new("ability.radiant.consecrate")), TechEffect::UnlockAbility(AbilityId::new("ability.radiant.aegis"))],
            &["item.radiant.crown_dawn"]),
    ]
}

// ===========================================================================
// Missions — new objectives across the schools, including a boss hunt.
// ===========================================================================

fn missions() -> Vec<MissionDef> {
    vec![
        MissionDef {
            id: MissionId::new("mission.green_tide"),
            name: "The Green Tide".into(),
            description: "The mire is rising. Cull the thornlings before they overrun the lowland paths.".into(),
            objectives: vec![
                Objective::Kill { mob: Some(MobId::new("mob.verdance.thornling")), count: 8 },
                Objective::Collect { item: ItemId::new("item.verdance.seed_pod"), count: 5 },
            ],
            xp_reward: 180, item_rewards: vec![(ItemId::new("item.verdance.vinecaster"), 1)],
            tech_points: 2, level_req: 3, repeatable: true,
        },
        MissionDef {
            id: MissionId::new("mission.storm_chaser"),
            name: "Storm Chaser".into(),
            description: "Lightning has woken something in the peaks. Hunt the storm elementals and learn their charge.".into(),
            objectives: vec![
                Objective::Kill { mob: Some(MobId::new("mob.storm.elemental")), count: 6 },
                Objective::CastSpell { spell: Some(SpellId::new("spell.storm.spark")), count: 15 },
            ],
            xp_reward: 240, item_rewards: vec![(ItemId::new("item.storm.charged_core"), 4)],
            tech_points: 3, level_req: 6, repeatable: true,
        },
        MissionDef {
            id: MissionId::new("mission.the_hollow_dead"),
            name: "The Hollow Dead".into(),
            description: "Something old stirs the bones of the deep barrows. Descend, endure, and find what raises them.".into(),
            objectives: vec![
                Objective::Survive { seconds: 240.0 },
                Objective::Kill { mob: Some(MobId::new("mob.dead.shade")), count: 5 },
                Objective::Explore { zone_count: 3 },
            ],
            xp_reward: 400, item_rewards: vec![(ItemId::new("item.dead.shadow_silk"), 6)],
            tech_points: 4, level_req: 12, repeatable: false,
        },
        MissionDef {
            id: MissionId::new("mission.slay_colossus"),
            name: "Bane of the Barrow-King".into(),
            description: "The Bone Colossus walks. End it, and the dead of the barrows may finally rest.".into(),
            objectives: vec![
                Objective::Kill { mob: Some(MobId::new("mob.dead.bone_colossus")), count: 1 },
            ],
            xp_reward: 1200, item_rewards: vec![(ItemId::new("item.dead.amulet_lich"), 1)],
            tech_points: 6, level_req: 20, repeatable: false,
        },
        MissionDef {
            id: MissionId::new("mission.djinn_of_the_peaks"),
            name: "The Djinn of the Peaks".into(),
            description: "A Tempest Djinn has claimed the high stormpeaks. Climb to it and break the storm it rides.".into(),
            objectives: vec![
                Objective::ReachPoint { point: [-640.0, 180.0, 700.0], radius: 25.0 },
                Objective::Kill { mob: Some(MobId::new("mob.storm.djinn")), count: 1 },
            ],
            xp_reward: 900, item_rewards: vec![(ItemId::new("item.storm.ring_tempest"), 1)],
            tech_points: 5, level_req: 18, repeatable: false,
        },
    ]
}

// ===========================================================================
// Loot tables — new pools for the new schools and bosses.
// ===========================================================================

fn loot_tables() -> Vec<LootTableDef> {
    fn e(item: &str, weight: f32, min: u16, max: u16, rarity_bonus: f32) -> LootEntry {
        LootEntry { item: ItemId::new(item), weight, min, max, rarity_bonus }
    }
    vec![
        LootTableDef {
            id: LootTableId::new("loot.verdance"),
            name: "Verdant Drops".into(),
            entries: vec![
                e("item.verdance.seed_pod", 6.0, 1, 3, 0.0),
                e("item.verdance.heartwood", 3.0, 1, 2, 0.1),
                e("item.consume.vigor_elixir", 1.5, 1, 1, 0.1),
                e("item.verdance.staff_thorn", 0.2, 1, 1, 0.4),
            ],
            rolls: 2, level_scaling: 0.15,
        },
        LootTableDef {
            id: LootTableId::new("loot.storm"),
            name: "Storm Drops".into(),
            entries: vec![
                e("item.storm.charged_core", 6.0, 1, 3, 0.0),
                e("item.storm.windsoles", 0.5, 1, 1, 0.3),
                e("item.storm.staff_tempest", 0.3, 1, 1, 0.4),
            ],
            rolls: 2, level_scaling: 0.2,
        },
        LootTableDef {
            id: LootTableId::new("loot.dead"),
            name: "Grave Drops".into(),
            entries: vec![
                e("item.dead.bone_dust", 7.0, 1, 4, 0.0),
                e("item.dead.shadow_silk", 3.0, 1, 2, 0.2),
                e("item.consume.smoke_phial", 1.0, 1, 1, 0.1),
                e("item.dead.scepter_grave", 0.15, 1, 1, 0.5),
            ],
            rolls: 2, level_scaling: 0.25,
        },
        LootTableDef {
            id: LootTableId::new("loot.boss_colossus"),
            name: "Barrow-King's Hoard".into(),
            entries: vec![
                e("item.dead.bone_dust", 8.0, 6, 12, 0.0),
                e("item.dead.shadow_silk", 5.0, 3, 6, 0.0),
                e("item.void_essence", 4.0, 2, 4, 0.0),
                e("item.dead.scepter_grave", 1.0, 1, 1, 0.5),
                e("item.dead.amulet_lich", 0.2, 1, 1, 0.7),
                e("item.void_relic", 0.25, 1, 1, 0.6),
            ],
            rolls: 4, level_scaling: 0.4,
        },
        LootTableDef {
            id: LootTableId::new("loot.boss_djinn"),
            name: "Djinn's Spoils".into(),
            entries: vec![
                e("item.storm.charged_core", 8.0, 4, 8, 0.0),
                e("item.crystal_shard", 5.0, 3, 6, 0.0),
                e("item.storm.staff_tempest", 1.2, 1, 1, 0.4),
                e("item.storm.ring_tempest", 0.8, 1, 1, 0.5),
            ],
            rolls: 3, level_scaling: 0.35,
        },
    ]
}

// ===========================================================================
// Spawn rules — populate the two new biomes and seed the bosses.
// ===========================================================================

fn spawn_rules() -> Vec<SpawnRuleDef> {
    vec![
        SpawnRuleDef {
            id: SpawnRuleId::new("spawn.mire_thornlings"),
            name: "Mire Thornlings".into(),
            mob: MobId::new("mob.verdance.thornling"),
            trigger: SpawnTrigger::Continuous { interval_s: 7.0 },
            max_alive: 30, biome_filter: vec!["mire".into(), "forest".into()],
            level_scaling: 0.12, loot_table: Some(LootTableId::new("loot.verdance")),
        },
        SpawnRuleDef {
            id: SpawnRuleId::new("spawn.mire_warden"),
            name: "Grove Warden".into(),
            mob: MobId::new("mob.verdance.grove_warden"),
            trigger: SpawnTrigger::OnObjective,
            max_alive: 1, biome_filter: vec!["mire".into()],
            level_scaling: 0.3, loot_table: Some(LootTableId::new("loot.boss")),
        },
        SpawnRuleDef {
            id: SpawnRuleId::new("spawn.stormpeak_elementals"),
            name: "Stormpeak Elementals".into(),
            mob: MobId::new("mob.storm.elemental"),
            trigger: SpawnTrigger::Continuous { interval_s: 20.0 },
            max_alive: 12, biome_filter: vec!["stormpeaks".into(), "highlands".into()],
            level_scaling: 0.25, loot_table: Some(LootTableId::new("loot.storm")),
        },
        SpawnRuleDef {
            id: SpawnRuleId::new("spawn.stormpeak_djinn"),
            name: "Tempest Djinn".into(),
            mob: MobId::new("mob.storm.djinn"),
            trigger: SpawnTrigger::OnObjective,
            max_alive: 1, biome_filter: vec!["stormpeaks".into()],
            level_scaling: 0.4, loot_table: Some(LootTableId::new("loot.boss_djinn")),
        },
        SpawnRuleDef {
            id: SpawnRuleId::new("spawn.barrow_dead"),
            name: "Barrow Dead".into(),
            mob: MobId::new("mob.dead.risen_husk"),
            trigger: SpawnTrigger::Wave { wave_size: 8, interval_s: 30.0 },
            max_alive: 40, biome_filter: vec!["hollows".into(), "mire".into()],
            level_scaling: 0.2, loot_table: Some(LootTableId::new("loot.dead")),
        },
        SpawnRuleDef {
            id: SpawnRuleId::new("spawn.barrow_shades"),
            name: "Wailing Shades".into(),
            mob: MobId::new("mob.dead.shade"),
            trigger: SpawnTrigger::Continuous { interval_s: 25.0 },
            max_alive: 14, biome_filter: vec!["hollows".into()],
            level_scaling: 0.28, loot_table: Some(LootTableId::new("loot.dead")),
        },
        SpawnRuleDef {
            id: SpawnRuleId::new("spawn.barrow_colossus"),
            name: "The Bone Colossus".into(),
            mob: MobId::new("mob.dead.bone_colossus"),
            trigger: SpawnTrigger::OnObjective,
            max_alive: 1, biome_filter: vec!["hollows".into()],
            level_scaling: 0.5, loot_table: Some(LootTableId::new("loot.boss_colossus")),
        },
    ]
}

// ===========================================================================
// Triggers — combo synergies, weather events, and boss-arrival drama.
// ===========================================================================

fn triggers() -> Vec<TriggerDef> {
    vec![
        // Conductive synergy: casting Spark sometimes primes a target as conductive,
        // making the next storm spell land harder. Pure combo-reward.
        TriggerDef {
            id: TriggerId::new("trigger.storm_conduct"),
            name: "Conduction".into(),
            on: GameTrigger::OnSpellCast { spell: SpellId::new("spell.storm.spark") },
            conditions: vec![TriggerCondition::Chance { p: 0.4 }],
            actions: vec![RuleAction::ApplyStatus { status: StatusId::new("status.conductive"), duration_s: 4.0 }],
            once_per_player: false,
        },
        // Verdance synergy: thornlash kills sometimes scatter seed pods at the corpse.
        TriggerDef {
            id: TriggerId::new("trigger.verdant_bloom"),
            name: "Verdant Bloom".into(),
            on: GameTrigger::OnKill,
            conditions: vec![TriggerCondition::InBiome { name: "mire".into() }, TriggerCondition::Chance { p: 0.35 }],
            actions: vec![RuleAction::GrantItem { item: ItemId::new("item.verdance.seed_pod"), count: 1 }],
            once_per_player: false,
        },
        // Necromancy reward: deaths in the hollows sometimes raise a husk to fight on.
        TriggerDef {
            id: TriggerId::new("trigger.restless_dead"),
            name: "Restless Dead".into(),
            on: GameTrigger::OnDeath,
            conditions: vec![TriggerCondition::InBiome { name: "hollows".into() }, TriggerCondition::Chance { p: 0.5 }],
            actions: vec![
                RuleAction::SpawnMob { mob: MobId::new("mob.dead.risen_husk"), count: 1 },
                RuleAction::Broadcast { message: "The barrows do not forgive the fallen...".into() },
            ],
            once_per_player: false,
        },
        // Ambient weather: a periodic storm sweeps the stormpeaks, charging those caught out.
        TriggerDef {
            id: TriggerId::new("trigger.rolling_storm"),
            name: "Rolling Storm".into(),
            on: GameTrigger::OnTimer { interval_s: 120.0 },
            conditions: vec![TriggerCondition::InBiome { name: "stormpeaks".into() }],
            actions: vec![
                RuleAction::ApplyStatus { status: StatusId::new("status.static_charge"), duration_s: 6.0 },
                RuleAction::Broadcast { message: "Thunder rolls across the peaks.".into() },
            ],
            once_per_player: false,
        },
        // Boss drama: zone-entering the barrow announces the colossus and seeds its hoard table.
        TriggerDef {
            id: TriggerId::new("trigger.colossus_wakes"),
            name: "The Colossus Wakes".into(),
            on: GameTrigger::OnZoneEnter { zone_tag: "barrow".into() },
            conditions: vec![TriggerCondition::MinLevel { level: 18 }],
            actions: vec![
                RuleAction::SpawnMob { mob: MobId::new("mob.dead.bone_colossus"), count: 1 },
                RuleAction::Broadcast { message: "The ground splits. The Barrow-King rises.".into() },
            ],
            once_per_player: true,
        },
        // Radiance reward: the first Seraph kill blesses the slayer and grants a gilded feather.
        TriggerDef {
            id: TriggerId::new("trigger.seraph_blessing"),
            name: "Seraph's Blessing".into(),
            on: GameTrigger::OnKill,
            conditions: vec![TriggerCondition::HasTech { node: TechNodeId::new("tech.radiant_1") }, TriggerCondition::Chance { p: 0.2 }],
            actions: vec![
                RuleAction::ApplyStatus { status: StatusId::new("status.sanctified"), duration_s: 8.0 },
                RuleAction::GrantItem { item: ItemId::new("item.radiant.gilded_feather"), count: 1 },
            ],
            once_per_player: false,
        },
    ]
}

// ===========================================================================
// Game modes — two new ways to play.
// ===========================================================================

fn game_modes() -> Vec<GameModeDef> {
    vec![
        // Horde survival: PvE waves, you win by simply lasting.
        GameModeDef {
            id: GameModeId::new("gamemode.survival_horde"),
            name: "Survival Horde".into(),
            description: "Stand against escalating waves of the hollow dead. Last until the dawn bell tolls.".into(),
            teams: TeamConfig::Teams { count: 1, friendly_fire: false },
            win: WinCondition::TimeLimit { seconds: 900.0 },
            scoring: vec![ScoringRule::KillPoints { points: 1 }, ScoringRule::AssistPoints { points: 1 }],
            respawn_seconds: 12.0,
            allow_loot_drops: true, loot_multiplier: 1.5,
            starting_loadout: vec![AbilityId::new("ability.fireball"), AbilityId::new("ability.radiant.smite")],
            starting_items: vec![(ItemId::new("item.mana_crystal"), 5), (ItemId::new("item.consume.vigor_elixir"), 2)],
            pvp_enabled: false, mob_waves: true,
        },
        // Relic royale: free-for-all, last mage standing, loot stays on.
        GameModeDef {
            id: GameModeId::new("gamemode.relic_royale"),
            name: "Relic Royale".into(),
            description: "A shrinking arena, full loot, no respawns. The last mage holding a relic wins.".into(),
            teams: TeamConfig::FreeForAll,
            win: WinCondition::LastTeamStanding,
            scoring: vec![ScoringRule::KillPoints { points: 2 }, ScoringRule::FirstBloodBonus { points: 3 }],
            respawn_seconds: 9999.0,
            allow_loot_drops: true, loot_multiplier: 2.0,
            starting_loadout: vec![AbilityId::new("ability.fireball")],
            starting_items: vec![(ItemId::new("item.mana_crystal"), 2)],
            pvp_enabled: true, mob_waves: false,
        },
    ]
}

// ===========================================================================
// World graft — two new biomes (the Mire and the Stormpeaks) plus structures.
// ===========================================================================

fn graft_world(world: &mut crate::worldgen::WorldGenParams) {
    world.biomes.push(BiomeDef {
        name: "The Mire".into(),
        height_min: 0.42, height_max: 0.55,
        surface_material: Some(MaterialId::new("material.moss")),
        fog_color: [0.2, 0.28, 0.18],
        mob_spawns: vec![
            (MobId::new("mob.verdance.thornling"), 2.0),
            (MobId::new("mob.verdance.treant"), 0.5),
            (MobId::new("mob.dead.risen_husk"), 1.0),
        ],
    });
    world.biomes.push(BiomeDef {
        name: "The Stormpeaks".into(),
        height_min: 0.85, height_max: 1.0,
        surface_material: Some(MaterialId::new("material.storm_cloud")),
        fog_color: [0.3, 0.32, 0.42],
        mob_spawns: vec![
            (MobId::new("mob.storm.elemental"), 2.0),
            (MobId::new("mob.radiant.seraph"), 0.4),
        ],
    });
    world.structures.push(StructureDef {
        name: "The Barrow".into(),
        frequency: 0.012,
        mob_id: Some(MobId::new("mob.dead.bone_colossus")),
        scale: 7.0,
        mesh_seed: 0x5801,
    });
    world.structures.push(StructureDef {
        name: "Lightning Spire".into(),
        frequency: 0.02,
        mob_id: Some(MobId::new("mob.storm.djinn")),
        scale: 5.0,
        mesh_seed: 0x5802,
    });
    world.structures.push(StructureDef {
        name: "Overgrown Shrine".into(),
        frequency: 0.035,
        mob_id: Some(MobId::new("mob.verdance.grove_warden")),
        scale: 3.0,
        mesh_seed: 0x5803,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_pack::default_pack;

    /// The expanded default pack must still pass structural validation — every new
    /// ability resolves a spell, every spell's statuses exist, loot/spawn/trigger
    /// references are intact, and game-mode loadouts resolve.
    #[test]
    fn expanded_pack_validates() {
        let pack = default_pack();
        pack.validate().expect("expanded default pack must validate");
    }

    /// The expansion meaningfully grows the game (guards against an accidental no-op
    /// merge): the combined pack should carry far more than the starter alone.
    #[test]
    fn expansion_adds_content() {
        let pack = default_pack();
        assert!(pack.spells.len() >= 35, "expected many spells, got {}", pack.spells.len());
        assert!(pack.tech.nodes.len() >= 30, "expected a deep tech tree, got {}", pack.tech.nodes.len());
        assert!(pack.mobs.len() >= 12, "expected a full bestiary, got {}", pack.mobs.len());
        assert!(pack.game_modes.len() >= 5, "expected new game modes, got {}", pack.game_modes.len());
    }

    /// Every ability a mob casts must resolve to a real ability id — the validator
    /// doesn't check this, but a mob casting a missing ability would be a silent bug.
    #[test]
    fn mob_abilities_resolve() {
        let pack = default_pack();
        let ability_ids: std::collections::HashSet<_> =
            pack.abilities.iter().map(|a| a.id.clone()).collect();
        for m in &pack.mobs {
            for ab in &m.abilities {
                assert!(ability_ids.contains(ab), "mob {} casts missing ability {}", m.id, ab);
            }
        }
    }
}
