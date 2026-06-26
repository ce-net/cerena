//! The flagship starter content for Cerena — `cerena-default-0.1`.
//!
//! [`default_pack`] builds one coherent, playable game: a dozen spells composed from
//! the [`crate::spell::EffectOp`] primitives, the statuses they apply, a full parkour
//! movement kit, equippable gear that grants those spells and modes, abilities wiring
//! spells to inputs, a four-branch tech tree, organic procedural materials + WGSL
//! shaders, an organic mystery world, creatures, and missions.
//!
//! Every cross-reference is intentional and resolves, so
//! [`crate::pack::ContentPack::validate`] passes: abilities point at real spells,
//! tech nodes unlock real items, and every `ApplyStatus` names a defined status.
//!
//! All ids are stable, kebab/dot-style (`spell.fireball`, `item.ember_staff`). Ids
//! are append-only by convention — re-tuning is fine, renaming breaks saved state.

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
use crate::tech::{TechEffect, TechNode, TechTree};
use crate::triggers::{GameTrigger, RuleAction, TriggerCondition, TriggerDef};
use crate::tuning::TuningConfig;
use crate::worldgen::WorldGenParams;

/// Convenience: box an op for the tree-shaped `EffectOp` continuations.
fn b(op: EffectOp) -> Box<EffectOp> {
    Box::new(op)
}

/// Build a [`SpellDef`], deriving `author_cost` from the graph's complexity so the
/// authored spells are priced on the same scale as player-made ones.
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

/// Build the full default content pack.
pub fn default_pack() -> ContentPack {
    ContentPack {
        label: "cerena-default-0.2".to_string(),
        spells: spells(),
        items: items(),
        abilities: abilities(),
        tech: tech_tree(),
        statuses: statuses(),
        movement_modes: movement_modes(),
        materials: materials(),
        shaders: shaders(),
        worldgen: WorldGenParams::default(),
        mobs: mobs(),
        missions: missions(),
        tuning: TuningConfig::default(),
        game_modes: game_modes(),
        loot_tables: loot_tables(),
        spawn_rules: spawn_rules(),
        triggers: triggers(),
    }
}

// ---------------------------------------------------------------------------
// Spells — composed from the closed EffectOp primitive set.
// ---------------------------------------------------------------------------

fn spells() -> Vec<SpellDef> {
    vec![
        // Fireball: a travelling bolt that bursts into a burning blast.
        spell(
            "spell.fireball",
            "Fireball",
            "Hurl a bolt of fire that explodes on impact, scorching everything nearby.",
            "fire",
            20.0,
            0.4,
            1.5,
            false,
            Scaling { power: 0.8, focus: 0.2, agility: 0.0, level: 0.5 },
            EffectOp::Projectile {
                speed: 42.0,
                gravity: 1.5,
                radius: 0.4,
                lifetime_s: 4.0,
                homing: 0.0,
                on_hit: b(EffectOp::Area {
                    radius: 4.0,
                    faction: Faction::Enemies,
                    falloff: 0.5,
                    then: b(EffectOp::Sequence(vec![
                        EffectOp::Damage { amount: 60.0, element: ElementId::new("fire") },
                        EffectOp::ApplyStatus {
                            status: StatusId::new("status.burning"),
                            duration_s: 4.0,
                            stacks: 1,
                        },
                    ])),
                }),
            },
        ),
        // Frostbolt: single-target bolt that freezes the struck enemy.
        spell(
            "spell.frostbolt",
            "Frostbolt",
            "A shard of ice that bites deep and locks the target in frost.",
            "frost",
            16.0,
            0.3,
            1.2,
            false,
            Scaling { power: 0.7, focus: 0.3, agility: 0.0, level: 0.4 },
            EffectOp::Projectile {
                speed: 50.0,
                gravity: 0.0,
                radius: 0.3,
                lifetime_s: 3.0,
                homing: 0.0,
                on_hit: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 45.0, element: ElementId::new("frost") },
                    EffectOp::ApplyStatus {
                        status: StatusId::new("status.frozen"),
                        duration_s: 2.0,
                        stacks: 1,
                    },
                ])),
            },
        ),
        // Arcane Lance: a piercing instant ray.
        spell(
            "spell.arcane_lance",
            "Arcane Lance",
            "A focused beam of raw arcane force that punches through several foes.",
            "arcane",
            12.0,
            0.2,
            0.8,
            false,
            Scaling { power: 0.6, focus: 0.6, agility: 0.0, level: 0.4 },
            EffectOp::Ray {
                range: 50.0,
                pierce: 3,
                then: b(EffectOp::Damage { amount: 40.0, element: ElementId::new("arcane") }),
            },
        ),
        // Healing Bloom: a lingering field that mends allies.
        spell(
            "spell.healing_bloom",
            "Healing Bloom",
            "Grow a blossom of life energy that heals allies standing within it.",
            "life",
            30.0,
            0.6,
            6.0,
            false,
            Scaling { power: 0.0, focus: 0.9, agility: 0.0, level: 0.6 },
            EffectOp::Field {
                radius: 5.0,
                duration_s: 6.0,
                interval_s: 1.0,
                faction: Faction::Allies,
                tick: b(EffectOp::Heal { amount: 15.0 }),
            },
        ),
        // Meteor: a high-arc heavy projectile with a devastating blast.
        spell(
            "spell.meteor",
            "Meteor",
            "Call down a blazing rock that craters the ground and flings foes aside.",
            "fire",
            55.0,
            1.4,
            10.0,
            false,
            Scaling { power: 1.2, focus: 0.2, agility: 0.0, level: 0.8 },
            EffectOp::Projectile {
                speed: 26.0,
                gravity: 9.0,
                radius: 0.8,
                lifetime_s: 6.0,
                homing: 0.0,
                on_hit: b(EffectOp::Area {
                    radius: 6.0,
                    faction: Faction::Enemies,
                    falloff: 0.4,
                    then: b(EffectOp::Sequence(vec![
                        EffectOp::Damage { amount: 120.0, element: ElementId::new("fire") },
                        EffectOp::ApplyStatus {
                            status: StatusId::new("status.vulnerable"),
                            duration_s: 5.0,
                            stacks: 1,
                        },
                        EffectOp::Impulse { force: 18.0, vertical_bias: 0.7 },
                    ])),
                }),
            },
        ),
        // Chain Lightning: a ray that arcs in rapid repeated strikes.
        spell(
            "spell.chain_lightning",
            "Chain Lightning",
            "A bolt that leaps between targets in a flurry of crackling strikes.",
            "storm",
            28.0,
            0.5,
            4.0,
            false,
            Scaling { power: 0.7, focus: 0.4, agility: 0.0, level: 0.5 },
            EffectOp::Ray {
                range: 35.0,
                pierce: 0,
                then: b(EffectOp::Repeat {
                    count: 5,
                    interval_s: 0.08,
                    op: b(EffectOp::Damage { amount: 30.0, element: ElementId::new("storm") }),
                }),
            },
        ),
        // Void Grasp: a tether that yanks and roots a foe.
        spell(
            "spell.void_grasp",
            "Void Grasp",
            "Seize an enemy with tendrils of void, dragging them in and pinning them.",
            "void",
            22.0,
            0.3,
            3.0,
            false,
            Scaling { power: 0.4, focus: 0.5, agility: 0.0, level: 0.4 },
            EffectOp::Ray {
                range: 30.0,
                pierce: 0,
                then: b(EffectOp::Sequence(vec![
                    // Negative force = pull toward the caster.
                    EffectOp::Impulse { force: -22.0, vertical_bias: 0.2 },
                    EffectOp::ApplyStatus {
                        status: StatusId::new("status.root"),
                        duration_s: 2.0,
                        stacks: 1,
                    },
                ])),
            },
        ),
        // Blink Strike: teleport forward, then sweep a cone of void damage.
        spell(
            "spell.blink_strike",
            "Blink Strike",
            "Flicker through space and erupt into a fan of cutting void energy.",
            "void",
            26.0,
            0.0,
            5.0,
            false,
            Scaling { power: 0.6, focus: 0.3, agility: 0.4, level: 0.5 },
            EffectOp::Sequence(vec![
                EffectOp::Teleport { max_distance: 8.0, to_target: false },
                EffectOp::Cone {
                    range: 5.0,
                    half_angle_rad: 0.6,
                    faction: Faction::Enemies,
                    then: b(EffectOp::Damage { amount: 55.0, element: ElementId::new("void") }),
                },
            ]),
        ),
        // Summon Wisp: call a friendly wisp to fight alongside you.
        spell(
            "spell.summon_wisp",
            "Summon Wisp",
            "Conjure a luminous wisp that harries your enemies for a time.",
            "arcane",
            40.0,
            1.0,
            12.0,
            false,
            Scaling { power: 0.0, focus: 0.8, agility: 0.0, level: 0.7 },
            EffectOp::Summon {
                mob: MobId::new("mob.wisp"),
                count: 1,
                duration_s: 30.0,
            },
        ),
        // Ground Slam: a close burst that staggers and lifts.
        spell(
            "spell.ground_slam",
            "Ground Slam",
            "Smash the earth, sending a shockwave that batters and uproots nearby foes.",
            "earth",
            18.0,
            0.3,
            2.5,
            false,
            Scaling { power: 0.8, focus: 0.0, agility: 0.2, level: 0.4 },
            EffectOp::Area {
                radius: 5.0,
                faction: Faction::Enemies,
                falloff: 0.4,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 50.0, element: ElementId::new("earth") },
                    EffectOp::Impulse { force: 15.0, vertical_bias: 0.8 },
                ])),
            },
        ),
        // Life Siphon: a channeled beam that drains enemies to heal the caster.
        spell(
            "spell.life_siphon",
            "Life Siphon",
            "Channel a draining beam, leeching vitality from your foe into yourself.",
            "void",
            8.0,
            0.0,
            1.0,
            true,
            Scaling { power: 0.5, focus: 0.5, agility: 0.0, level: 0.5 },
            EffectOp::Ray {
                range: 20.0,
                pierce: 0,
                then: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 20.0, element: ElementId::new("void") },
                    EffectOp::Heal { amount: 12.0 },
                ])),
            },
        ),
        // Storm Field: a lingering tempest that shocks and slows.
        spell(
            "spell.storm_field",
            "Storm Field",
            "Summon a churning storm cloud that shocks and hobbles all beneath it.",
            "storm",
            45.0,
            0.8,
            9.0,
            false,
            Scaling { power: 0.6, focus: 0.5, agility: 0.0, level: 0.7 },
            EffectOp::Field {
                radius: 6.0,
                duration_s: 5.0,
                interval_s: 0.5,
                faction: Faction::Enemies,
                tick: b(EffectOp::Sequence(vec![
                    EffectOp::Damage { amount: 10.0, element: ElementId::new("storm") },
                    EffectOp::ApplyStatus {
                        status: StatusId::new("status.slow"),
                        duration_s: 1.0,
                        stacks: 1,
                    },
                ])),
            },
        ),
        // Mana Draught: the spell behind the mana-crystal consumable.
        spell(
            "spell.mana_draught",
            "Mana Draught",
            "Crack a mana crystal to flood yourself with restorative energy.",
            "arcane",
            0.0,
            0.0,
            0.0,
            false,
            Scaling::default(),
            EffectOp::RestoreMana { amount: 80.0 },
        ),
    ]
}

// ---------------------------------------------------------------------------
// Status effects — referenced by the spells above.
// ---------------------------------------------------------------------------

fn statuses() -> Vec<StatusEffectDef> {
    vec![
        StatusEffectDef {
            id: StatusId::new("status.burning"),
            name: "Burning".into(),
            kind: StatusKind::Burning { dps: 12.0 },
            max_stacks: 3,
            tick_interval_s: 0.5,
            duration_default_s: 4.0,
            beneficial: false,
            material: Some(MaterialId::new("material.lava")),
        },
        StatusEffectDef {
            id: StatusId::new("status.frozen"),
            name: "Frozen".into(),
            kind: StatusKind::Frozen,
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 2.0,
            beneficial: false,
            material: Some(MaterialId::new("material.crystal")),
        },
        StatusEffectDef {
            id: StatusId::new("status.slow"),
            name: "Slowed".into(),
            kind: StatusKind::Slow { frac: 0.4 },
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 2.0,
            beneficial: false,
            material: None,
        },
        StatusEffectDef {
            id: StatusId::new("status.root"),
            name: "Rooted".into(),
            kind: StatusKind::Root,
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 2.0,
            beneficial: false,
            material: Some(MaterialId::new("material.void_fog")),
        },
        StatusEffectDef {
            id: StatusId::new("status.regen"),
            name: "Regeneration".into(),
            kind: StatusKind::Regen { hps: 8.0 },
            max_stacks: 3,
            tick_interval_s: 1.0,
            duration_default_s: 6.0,
            beneficial: true,
            material: Some(MaterialId::new("material.mystic_grass")),
        },
        StatusEffectDef {
            id: StatusId::new("status.empower"),
            name: "Empowered".into(),
            kind: StatusKind::Empower { frac: 0.25 },
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 8.0,
            beneficial: true,
            material: None,
        },
        StatusEffectDef {
            id: StatusId::new("status.shielded"),
            name: "Shielded".into(),
            kind: StatusKind::Shielded { amount: 80.0 },
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 6.0,
            beneficial: true,
            material: Some(MaterialId::new("material.crystal")),
        },
        StatusEffectDef {
            id: StatusId::new("status.vulnerable"),
            name: "Vulnerable".into(),
            kind: StatusKind::Vulnerable { frac: 0.2 },
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 5.0,
            beneficial: false,
            material: None,
        },
        StatusEffectDef {
            id: StatusId::new("status.haste"),
            name: "Hastened".into(),
            kind: StatusKind::Haste { frac: 0.3 },
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 6.0,
            beneficial: true,
            material: None,
        },
        StatusEffectDef {
            id: StatusId::new("status.invisible"),
            name: "Invisible".into(),
            kind: StatusKind::Invisible,
            max_stacks: 1,
            tick_interval_s: 0.0,
            duration_default_s: 5.0,
            beneficial: true,
            material: Some(MaterialId::new("material.void_fog")),
        },
    ]
}

// ---------------------------------------------------------------------------
// Movement / parkour modes.
// ---------------------------------------------------------------------------

fn movement_modes() -> Vec<MovementModeDef> {
    vec![
        MovementModeDef {
            id: MovementModeId::new("movement.dash"),
            name: "Dash".into(),
            kind: MovementKind::Dash { distance: 6.0, speed: 30.0 },
            mana_cost: 0.0,
            cooldown: 1.5,
            stamina_cost: 15.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.double_jump"),
            name: "Double Jump".into(),
            kind: MovementKind::DoubleJump { extra_jumps: 1, impulse: 7.5 },
            mana_cost: 0.0,
            cooldown: 0.0,
            stamina_cost: 10.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.wall_run"),
            name: "Wall Run".into(),
            kind: MovementKind::WallRun { max_time_s: 2.5, speed: 9.0, gravity_mult: 0.25 },
            mana_cost: 0.0,
            cooldown: 0.0,
            stamina_cost: 12.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.grapple"),
            name: "Grapple".into(),
            kind: MovementKind::Grapple { range: 30.0, pull_speed: 22.0 },
            mana_cost: 5.0,
            cooldown: 2.0,
            stamina_cost: 0.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.glide"),
            name: "Glide".into(),
            kind: MovementKind::Glide { fall_mult: 0.3, forward_boost: 6.0 },
            mana_cost: 0.0,
            cooldown: 0.0,
            stamina_cost: 5.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.blink"),
            name: "Blink".into(),
            kind: MovementKind::Blink { distance: 10.0 },
            mana_cost: 15.0,
            cooldown: 4.0,
            stamina_cost: 0.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.climb"),
            name: "Climb".into(),
            kind: MovementKind::Climb { speed: 4.0 },
            mana_cost: 0.0,
            cooldown: 0.0,
            stamina_cost: 8.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.ground_slam"),
            name: "Ground Slam".into(),
            kind: MovementKind::GroundSlam { damage: 40.0, radius: 4.0, down_speed: 40.0 },
            mana_cost: 0.0,
            cooldown: 3.0,
            stamina_cost: 20.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.slide"),
            name: "Slide".into(),
            kind: MovementKind::Slide { speed: 14.0, duration_s: 1.0 },
            mana_cost: 0.0,
            cooldown: 1.0,
            stamina_cost: 6.0,
        },
        MovementModeDef {
            id: MovementModeId::new("movement.sprint"),
            name: "Sprint".into(),
            kind: MovementKind::Sprint { speed_mult: 1.6 },
            mana_cost: 0.0,
            cooldown: 0.0,
            stamina_cost: 4.0,
        },
    ]
}

// ---------------------------------------------------------------------------
// Items — gear that grants the spells, modes, and stats above.
// ---------------------------------------------------------------------------

fn items() -> Vec<ItemDef> {
    vec![
        // Reagents (slotless crafting inputs) — dropped by mobs, used in recipes.
        ItemDef {
            id: ItemId::new("item.crystal_shard"),
            name: "Crystal Shard".into(),
            description: "A humming sliver of highland crystal. A crafting reagent.".into(),
            rarity: Rarity::Uncommon,
            slot: EquipSlot::None,
            stat_mods: StatMods::default(),
            grants_spells: vec![],
            grants_movement: vec![],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.crystal")),
            stackable: true,
            max_stack: 99,
            on_use: None,
            level_req: 0,
            craft: None,
        },
        ItemDef {
            id: ItemId::new("item.void_essence"),
            name: "Void Essence".into(),
            description: "Condensed dark matter wrung from a wraith. A crafting reagent.".into(),
            rarity: Rarity::Rare,
            slot: EquipSlot::None,
            stat_mods: StatMods::default(),
            grants_spells: vec![],
            grants_movement: vec![],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.void_fog")),
            stackable: true,
            max_stack: 99,
            on_use: None,
            level_req: 0,
            craft: None,
        },
        // Staves — primary spell sources.
        ItemDef {
            id: ItemId::new("item.ember_staff"),
            name: "Ember Staff".into(),
            description: "A charred oaken staff that smoulders with captured flame.".into(),
            rarity: Rarity::Rare,
            slot: EquipSlot::Staff,
            stat_mods: StatMods { power: 12.0, spell_power_pct: 0.1, ..Default::default() },
            grants_spells: vec![SpellId::new("spell.fireball")],
            grants_movement: vec![],
            grants_abilities: vec![AbilityId::new("ability.fireball")],
            material: Some(MaterialId::new("material.lava")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 1,
            craft: None,
        },
        ItemDef {
            id: ItemId::new("item.frost_staff"),
            name: "Frost Staff".into(),
            description: "Carved from everfrost, its tip never thaws.".into(),
            rarity: Rarity::Rare,
            slot: EquipSlot::Staff,
            stat_mods: StatMods { power: 10.0, focus: 6.0, ..Default::default() },
            grants_spells: vec![SpellId::new("spell.frostbolt")],
            grants_movement: vec![],
            grants_abilities: vec![AbilityId::new("ability.frostbolt")],
            material: Some(MaterialId::new("material.crystal")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 1,
            craft: None,
        },
        // Relic / orb — arcane focus.
        ItemDef {
            id: ItemId::new("item.arcane_orb"),
            name: "Arcane Orb".into(),
            description: "A weightless sphere of latticed light that answers a focused mind.".into(),
            rarity: Rarity::Epic,
            slot: EquipSlot::Relic,
            stat_mods: StatMods { focus: 14.0, max_mana: 50.0, spell_power_pct: 0.08, ..Default::default() },
            grants_spells: vec![SpellId::new("spell.arcane_lance")],
            grants_movement: vec![],
            grants_abilities: vec![AbilityId::new("ability.arcane_lance")],
            material: Some(MaterialId::new("material.crystal")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 5,
            craft: None,
        },
        ItemDef {
            id: ItemId::new("item.void_relic"),
            name: "Void Relic".into(),
            description: "An aching hollow shard. To hold it is to hear the dark whisper back.".into(),
            rarity: Rarity::Legendary,
            slot: EquipSlot::Relic,
            stat_mods: StatMods { power: 18.0, focus: 10.0, spell_power_pct: 0.15, ..Default::default() },
            grants_spells: vec![SpellId::new("spell.void_grasp"), SpellId::new("spell.life_siphon")],
            grants_movement: vec![],
            grants_abilities: vec![AbilityId::new("ability.void_grasp")],
            material: Some(MaterialId::new("material.void_fog")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 12,
            craft: Some(CraftRecipe {
                inputs: vec![(ItemId::new("item.void_essence"), 5), (ItemId::new("item.crystal_shard"), 3)],
                tech_req: Some(TechNodeId::new("tech.arcana_4")),
            }),
        },
        // Boots / mobility gear.
        ItemDef {
            id: ItemId::new("item.swiftboots"),
            name: "Swiftboots".into(),
            description: "Feather-light boots that let you burst across the ground.".into(),
            rarity: Rarity::Uncommon,
            slot: EquipSlot::Boots,
            stat_mods: StatMods { agility: 8.0, move_speed: 1.0, ..Default::default() },
            grants_spells: vec![],
            grants_movement: vec![MovementModeId::new("movement.dash")],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.bark")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 1,
            craft: None,
        },
        ItemDef {
            id: ItemId::new("item.grapple_glove"),
            name: "Grapple Glove".into(),
            description: "A gauntlet that fires a living tendril to haul you skyward.".into(),
            rarity: Rarity::Rare,
            slot: EquipSlot::Trinket,
            stat_mods: StatMods { agility: 6.0, ..Default::default() },
            grants_spells: vec![],
            grants_movement: vec![MovementModeId::new("movement.grapple")],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.bark")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 4,
            craft: None,
        },
        ItemDef {
            id: ItemId::new("item.glider_cloak"),
            name: "Glider Cloak".into(),
            description: "A membranous cloak that catches the wind and stretches your leaps.".into(),
            rarity: Rarity::Rare,
            slot: EquipSlot::Trinket,
            stat_mods: StatMods { agility: 5.0, move_speed: 0.5, ..Default::default() },
            grants_spells: vec![],
            grants_movement: vec![MovementModeId::new("movement.glide")],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.bark")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 6,
            craft: None,
        },
        // Robes.
        ItemDef {
            id: ItemId::new("item.novice_robe"),
            name: "Novice Robe".into(),
            description: "Simple woven cloth. Every mage begins here.".into(),
            rarity: Rarity::Common,
            slot: EquipSlot::Robe,
            stat_mods: StatMods { max_mana: 20.0, mana_regen: 1.0, ..Default::default() },
            grants_spells: vec![],
            grants_movement: vec![],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.mystic_grass")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 1,
            craft: None,
        },
        ItemDef {
            id: ItemId::new("item.archmage_robe"),
            name: "Archmage Robe".into(),
            description: "Woven with crystal thread; the cloth of a master spellwright.".into(),
            rarity: Rarity::Legendary,
            slot: EquipSlot::Robe,
            stat_mods: StatMods {
                focus: 20.0,
                max_mana: 120.0,
                mana_regen: 4.0,
                cooldown_reduction: 0.1,
                spell_power_pct: 0.2,
                ..Default::default()
            },
            grants_spells: vec![],
            grants_movement: vec![],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.crystal")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 15,
            craft: Some(CraftRecipe {
                inputs: vec![(ItemId::new("item.crystal_shard"), 8), (ItemId::new("item.void_essence"), 2)],
                tech_req: Some(TechNodeId::new("tech.arcana_2")),
            }),
        },
        // Amulet.
        ItemDef {
            id: ItemId::new("item.phoenix_amulet"),
            name: "Phoenix Amulet".into(),
            description: "A reliquary holding an ember that has never gone out.".into(),
            rarity: Rarity::Mythic,
            slot: EquipSlot::Amulet,
            stat_mods: StatMods {
                power: 25.0,
                vitality: 15.0,
                max_health: 80.0,
                spell_power_pct: 0.18,
                ..Default::default()
            },
            grants_spells: vec![SpellId::new("spell.meteor")],
            grants_movement: vec![],
            grants_abilities: vec![AbilityId::new("ability.meteor")],
            material: Some(MaterialId::new("material.lava")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 20,
            craft: None,
        },
        // Ring.
        ItemDef {
            id: ItemId::new("item.storm_ring"),
            name: "Storm Ring".into(),
            description: "A band of restless silver that crackles when danger nears.".into(),
            rarity: Rarity::Epic,
            slot: EquipSlot::Ring,
            stat_mods: StatMods { power: 10.0, focus: 8.0, cooldown_reduction: 0.08, ..Default::default() },
            grants_spells: vec![SpellId::new("spell.chain_lightning")],
            grants_movement: vec![],
            grants_abilities: vec![AbilityId::new("ability.chain_lightning")],
            material: Some(MaterialId::new("material.crystal")),
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 8,
            craft: None,
        },
        // Consumable.
        ItemDef {
            id: ItemId::new("item.mana_crystal"),
            name: "Mana Crystal".into(),
            description: "Shatter it to drink down a surge of raw mana.".into(),
            rarity: Rarity::Common,
            slot: EquipSlot::Consumable,
            stat_mods: StatMods::default(),
            grants_spells: vec![],
            grants_movement: vec![],
            grants_abilities: vec![],
            material: Some(MaterialId::new("material.crystal")),
            stackable: true,
            max_stack: 20,
            on_use: Some(SpellId::new("spell.mana_draught")),
            level_req: 1,
            craft: Some(CraftRecipe {
                inputs: vec![(ItemId::new("item.crystal_shard"), 2)],
                tech_req: None,
            }),
        },
    ]
}

// ---------------------------------------------------------------------------
// Abilities — wire spells to inputs / action slots.
// ---------------------------------------------------------------------------

fn abilities() -> Vec<AbilityDef> {
    // Helper to keep this table terse.
    fn ab(id: &str, name: &str, spell: &str, binding: CastInput) -> AbilityDef {
        AbilityDef {
            id: AbilityId::new(id),
            name: name.to_string(),
            spell: SpellId::new(spell),
            binding,
            cooldown_override: None,
            icon_material: None,
        }
    }
    vec![
        ab("ability.fireball", "Fireball", "spell.fireball", CastInput::Primary),
        ab("ability.frostbolt", "Frostbolt", "spell.frostbolt", CastInput::Primary),
        ab("ability.arcane_lance", "Arcane Lance", "spell.arcane_lance", CastInput::Secondary),
        ab("ability.void_grasp", "Void Grasp", "spell.void_grasp", CastInput::Secondary),
        ab("ability.healing_bloom", "Healing Bloom", "spell.healing_bloom", CastInput::Slot(1)),
        ab("ability.meteor", "Meteor", "spell.meteor", CastInput::Slot(2)),
        ab("ability.chain_lightning", "Chain Lightning", "spell.chain_lightning", CastInput::Slot(3)),
        ab("ability.blink_strike", "Blink Strike", "spell.blink_strike", CastInput::Slot(4)),
        ab("ability.summon_wisp", "Summon Wisp", "spell.summon_wisp", CastInput::Slot(5)),
        ab("ability.ground_slam", "Ground Slam", "spell.ground_slam", CastInput::Slot(6)),
        ab("ability.life_siphon", "Life Siphon", "spell.life_siphon", CastInput::Slot(7)),
        ab("ability.storm_field", "Storm Field", "spell.storm_field", CastInput::Slot(8)),
    ]
}

// ---------------------------------------------------------------------------
// Tech tree — four branches, chained tiers.
// ---------------------------------------------------------------------------

fn tech_tree() -> TechTree {
    // Helper for the common node shape.
    fn node(
        id: &str,
        name: &str,
        description: &str,
        branch: &str,
        tier: u32,
        cost: u32,
        prereqs: &[&str],
        effects: Vec<TechEffect>,
        unlock_items: &[&str],
    ) -> TechNode {
        TechNode {
            id: TechNodeId::new(id),
            name: name.to_string(),
            description: description.to_string(),
            branch: branch.to_string(),
            tier,
            cost_skill_points: cost,
            prereqs: prereqs.iter().map(|p| TechNodeId::new(*p)).collect(),
            effects,
            unlock_items: unlock_items.iter().map(|i| ItemId::new(*i)).collect(),
            icon_material: None,
        }
    }

    TechTree {
        nodes: vec![
            // --- Pyromancy ---
            node(
                "tech.pyro_1", "Spark", "Awaken the flame within; learn to hurl fire.",
                "Pyromancy", 0, 1, &[],
                vec![TechEffect::UnlockAbility(AbilityId::new("ability.fireball"))],
                &["item.ember_staff"],
            ),
            node(
                "tech.pyro_2", "Kindling", "Your flames burn hotter and your spellcraft deepens.",
                "Pyromancy", 1, 2, &["tech.pyro_1"],
                vec![
                    TechEffect::StatMult(StatMods { power: 8.0, ..Default::default() }),
                    TechEffect::RaiseSpellBudget { complexity: 2, depth: 1 },
                ],
                &[],
            ),
            node(
                "tech.pyro_3", "Conflagration", "Call fire from the sky itself.",
                "Pyromancy", 2, 3, &["tech.pyro_2"],
                vec![TechEffect::UnlockAbility(AbilityId::new("ability.meteor"))],
                &["item.phoenix_amulet"],
            ),
            node(
                "tech.pyro_4", "Eternal Flame", "Mastery of fire; its power becomes part of you.",
                "Pyromancy", 3, 4, &["tech.pyro_3"],
                vec![TechEffect::StatMult(StatMods { power: 15.0, spell_power_pct: 0.1, ..Default::default() })],
                &[],
            ),
            // --- Cryomancy ---
            node(
                "tech.cryo_1", "First Frost", "Draw the cold into your hands.",
                "Cryomancy", 0, 1, &[],
                vec![TechEffect::UnlockAbility(AbilityId::new("ability.frostbolt"))],
                &["item.frost_staff"],
            ),
            node(
                "tech.cryo_2", "Permafrost", "Your ice clings longer and your focus sharpens.",
                "Cryomancy", 1, 2, &["tech.cryo_1"],
                vec![TechEffect::StatMult(StatMods { focus: 8.0, ..Default::default() })],
                &[],
            ),
            node(
                "tech.cryo_3", "Glacier Mind", "Cold clarity lets you weave intricate spells.",
                "Cryomancy", 2, 3, &["tech.cryo_2"],
                vec![TechEffect::RaiseSpellBudget { complexity: 3, depth: 1 }],
                &[],
            ),
            node(
                "tech.cryo_4", "Absolute Zero", "Bind a foe in stillness at the touch of your will.",
                "Cryomancy", 3, 4, &["tech.cryo_3"],
                vec![TechEffect::GrantSpell(SpellId::new("spell.storm_field"))],
                &[],
            ),
            // --- Arcana ---
            node(
                "tech.arcana_1", "Attunement", "Learn to shape raw arcane force into a lance.",
                "Arcana", 0, 1, &[],
                vec![TechEffect::UnlockAbility(AbilityId::new("ability.arcane_lance"))],
                &["item.arcane_orb"],
            ),
            node(
                "tech.arcana_2", "Weaving", "Unlock the loom of greater spellcraft.",
                "Arcana", 1, 2, &["tech.arcana_1"],
                vec![
                    TechEffect::RaiseSpellBudget { complexity: 4, depth: 2 },
                    TechEffect::UnlockRecipe(ItemId::new("item.archmage_robe")),
                ],
                &["item.archmage_robe"],
            ),
            node(
                "tech.arcana_3", "Conduction", "Make your bolts leap between many foes.",
                "Arcana", 2, 3, &["tech.arcana_2"],
                vec![TechEffect::UnlockAbility(AbilityId::new("ability.chain_lightning"))],
                &["item.storm_ring"],
            ),
            node(
                "tech.arcana_4", "Voidcraft", "Reach into the hollow places between things.",
                "Arcana", 3, 4, &["tech.arcana_3"],
                vec![
                    TechEffect::UnlockAbility(AbilityId::new("ability.void_grasp")),
                    TechEffect::UnlockRecipe(ItemId::new("item.void_relic")),
                ],
                &["item.void_relic"],
            ),
            // --- Mobility ---
            node(
                "tech.mobility_1", "Fleet Step", "Burst forward in a sudden dash.",
                "Mobility", 0, 1, &[],
                vec![TechEffect::UnlockMovement(MovementModeId::new("movement.dash"))],
                &["item.swiftboots"],
            ),
            node(
                "tech.mobility_2", "Grappler", "Fling a tendril and haul yourself across gaps.",
                "Mobility", 1, 2, &["tech.mobility_1"],
                vec![TechEffect::UnlockMovement(MovementModeId::new("movement.grapple"))],
                &["item.grapple_glove"],
            ),
            node(
                "tech.mobility_3", "Windrider", "Catch the air and glide on outstretched cloth.",
                "Mobility", 2, 3, &["tech.mobility_2"],
                vec![TechEffect::UnlockMovement(MovementModeId::new("movement.glide"))],
                &["item.glider_cloak"],
            ),
            node(
                "tech.mobility_4", "Phase Walker", "Step through space in the blink of an eye.",
                "Mobility", 3, 4, &["tech.mobility_3"],
                vec![
                    TechEffect::UnlockMovement(MovementModeId::new("movement.blink")),
                    TechEffect::StatMult(StatMods { agility: 12.0, move_speed: 1.0, ..Default::default() }),
                ],
                &[],
            ),
        ],
    }
}

// ---------------------------------------------------------------------------
// Materials — organic, noise-driven surfaces.
// ---------------------------------------------------------------------------

fn materials() -> Vec<MaterialDef> {
    // Helper for a single warped fbm base layer.
    fn fbm(frequency: f32, octaves: u8, warp: f32, seed: u32) -> NoiseLayer {
        NoiseLayer { kind: NoiseKind::Fbm, frequency, amplitude: 1.0, octaves, lacunarity: 2.0, gain: 0.5, warp, seed }
    }
    vec![
        // Weathered organic stone — the default ground / cliff face.
        MaterialDef {
            id: MaterialId::new("material.organic_stone"),
            name: "Organic Stone".into(),
            layers: vec![
                fbm(3.0, 5, 0.4, 11),
                NoiseLayer { kind: NoiseKind::Worley, frequency: 6.0, amplitude: 0.4, octaves: 2, lacunarity: 2.0, gain: 0.5, warp: 0.2, seed: 12 },
            ],
            ramp: ColorRamp {
                stops: vec![
                    (0.0, [0.18, 0.17, 0.16, 1.0]),
                    (0.5, [0.34, 0.32, 0.30, 1.0]),
                    (1.0, [0.55, 0.53, 0.50, 1.0]),
                ],
            },
            roughness: 0.9,
            metallic: 0.0,
            emissive: 0.0,
            emissive_color: [0.0, 0.0, 0.0],
            triplanar_scale: 0.5,
            displacement: 0.3,
            shader: Some(ShaderId::new("shader.surface_triplanar")),
        },
        // Mystic grass — verdant lowland surface with a faint glow.
        MaterialDef {
            id: MaterialId::new("material.mystic_grass"),
            name: "Mystic Grass".into(),
            layers: vec![fbm(8.0, 4, 0.6, 21), NoiseLayer { kind: NoiseKind::Flow, frequency: 4.0, amplitude: 0.3, octaves: 2, lacunarity: 2.0, gain: 0.5, warp: 0.5, seed: 22 }],
            ramp: ColorRamp {
                stops: vec![
                    (0.0, [0.06, 0.18, 0.07, 1.0]),
                    (0.6, [0.16, 0.42, 0.16, 1.0]),
                    (1.0, [0.42, 0.7, 0.35, 1.0]),
                ],
            },
            roughness: 0.8,
            metallic: 0.0,
            emissive: 0.15,
            emissive_color: [0.2, 0.6, 0.3],
            triplanar_scale: 1.0,
            displacement: 0.15,
            shader: Some(ShaderId::new("shader.surface_triplanar")),
        },
        // Crystal — highland surface, refractive and luminous.
        MaterialDef {
            id: MaterialId::new("material.crystal"),
            name: "Crystal".into(),
            layers: vec![NoiseLayer { kind: NoiseKind::Ridged, frequency: 5.0, amplitude: 1.0, octaves: 5, lacunarity: 2.2, gain: 0.55, warp: 0.1, seed: 31 }],
            ramp: ColorRamp {
                stops: vec![
                    (0.0, [0.2, 0.25, 0.45, 1.0]),
                    (0.5, [0.4, 0.5, 0.85, 1.0]),
                    (1.0, [0.7, 0.85, 1.0, 1.0]),
                ],
            },
            roughness: 0.15,
            metallic: 0.1,
            emissive: 0.4,
            emissive_color: [0.4, 0.55, 0.95],
            triplanar_scale: 0.8,
            displacement: 0.5,
            shader: Some(ShaderId::new("shader.surface_triplanar")),
        },
        // Lava — flowing molten rock.
        MaterialDef {
            id: MaterialId::new("material.lava"),
            name: "Lava".into(),
            layers: vec![NoiseLayer { kind: NoiseKind::Flow, frequency: 2.5, amplitude: 1.0, octaves: 4, lacunarity: 2.0, gain: 0.55, warp: 0.7, seed: 41 }],
            ramp: ColorRamp {
                stops: vec![
                    (0.0, [0.15, 0.02, 0.0, 1.0]),
                    (0.6, [0.9, 0.25, 0.02, 1.0]),
                    (1.0, [1.0, 0.85, 0.3, 1.0]),
                ],
            },
            roughness: 0.6,
            metallic: 0.0,
            emissive: 2.5,
            emissive_color: [1.0, 0.4, 0.1],
            triplanar_scale: 0.6,
            displacement: 0.4,
            shader: Some(ShaderId::new("shader.surface_triplanar")),
        },
        // Void fog — the dark, smoky material of the hollows.
        MaterialDef {
            id: MaterialId::new("material.void_fog"),
            name: "Void Fog".into(),
            layers: vec![NoiseLayer { kind: NoiseKind::DomainWarp, frequency: 1.5, amplitude: 1.0, octaves: 5, lacunarity: 2.0, gain: 0.5, warp: 1.2, seed: 51 }],
            ramp: ColorRamp {
                stops: vec![
                    (0.0, [0.02, 0.0, 0.05, 1.0]),
                    (0.5, [0.1, 0.04, 0.18, 1.0]),
                    (1.0, [0.28, 0.12, 0.4, 1.0]),
                ],
            },
            roughness: 1.0,
            metallic: 0.0,
            emissive: 0.6,
            emissive_color: [0.3, 0.1, 0.5],
            triplanar_scale: 1.2,
            displacement: 0.1,
            shader: Some(ShaderId::new("shader.surface_triplanar")),
        },
        // Bark — organic wood for staves and trees.
        MaterialDef {
            id: MaterialId::new("material.bark"),
            name: "Bark".into(),
            layers: vec![
                NoiseLayer { kind: NoiseKind::Flow, frequency: 10.0, amplitude: 1.0, octaves: 4, lacunarity: 2.0, gain: 0.5, warp: 0.3, seed: 61 },
                fbm(20.0, 3, 0.2, 62),
            ],
            ramp: ColorRamp {
                stops: vec![
                    (0.0, [0.12, 0.08, 0.04, 1.0]),
                    (0.6, [0.3, 0.2, 0.1, 1.0]),
                    (1.0, [0.5, 0.38, 0.22, 1.0]),
                ],
            },
            roughness: 0.95,
            metallic: 0.0,
            emissive: 0.0,
            emissive_color: [0.0, 0.0, 0.0],
            triplanar_scale: 0.7,
            displacement: 0.25,
            shader: Some(ShaderId::new("shader.surface_triplanar")),
        },
    ]
}

// ---------------------------------------------------------------------------
// Shaders — minimal but valid-looking WGSL. The client hot-recompiles these on
// every content swap, so editing a shader re-skins the live world.
// ---------------------------------------------------------------------------

fn shaders() -> Vec<ShaderDef> {
    vec![
        // Triplanar fbm surface shader with simple ramp-lit shading.
        ShaderDef {
            id: ShaderId::new("shader.surface_triplanar"),
            name: "Triplanar Surface".into(),
            stage: ShaderStage::Surface,
            params: vec![("ramp_scale".into(), 1.0), ("light_wrap".into(), 0.3)],
            source: r#"
// Triplanar surface shader: projects fbm-driven colour on world geometry and
// applies wrapped Lambert lighting. Hot-recompiled by arena-client.
struct Surf {
    world_pos: vec3<f32>,
    normal: vec3<f32>,
};

fn hash3(p: vec3<f32>) -> f32 {
    return fract(sin(dot(p, vec3<f32>(12.9898, 78.233, 37.719))) * 43758.5453);
}

fn fbm(p: vec3<f32>) -> f32 {
    var f: f32 = 0.0;
    var amp: f32 = 0.5;
    var pp: vec3<f32> = p;
    for (var i: i32 = 0; i < 4; i = i + 1) {
        f = f + amp * hash3(floor(pp));
        pp = pp * 2.0;
        amp = amp * 0.5;
    }
    return f;
}

fn shade(s: Surf, light_dir: vec3<f32>, base: vec3<f32>, light_wrap: f32) -> vec3<f32> {
    let n = normalize(s.normal);
    let ndl = clamp((dot(n, normalize(light_dir)) + light_wrap) / (1.0 + light_wrap), 0.0, 1.0);
    let detail = fbm(s.world_pos * 0.5);
    return base * (0.25 + 0.75 * ndl) * (0.85 + 0.15 * detail);
}
"#.into(),
        },
        // Sky / atmosphere gradient dome.
        ShaderDef {
            id: ShaderId::new("shader.sky"),
            name: "Sky Dome".into(),
            stage: ShaderStage::Sky,
            params: vec![("turbidity".into(), 2.0)],
            source: r#"
// Sky dome: a vertical gradient from horizon to zenith with a soft sun disc.
fn sky_color(dir: vec3<f32>, sun_dir: vec3<f32>) -> vec3<f32> {
    let t = clamp(dir.y * 0.5 + 0.5, 0.0, 1.0);
    let horizon = vec3<f32>(0.55, 0.62, 0.78);
    let zenith = vec3<f32>(0.12, 0.22, 0.45);
    let base = mix(horizon, zenith, t);
    let sun = pow(clamp(dot(normalize(dir), normalize(sun_dir)), 0.0, 1.0), 256.0);
    return base + vec3<f32>(1.0, 0.9, 0.7) * sun;
}
"#.into(),
        },
        // GPU particle shader: soft additive sprites tinted by element colour.
        ShaderDef {
            id: ShaderId::new("shader.particle"),
            name: "Soft Particle".into(),
            stage: ShaderStage::Particle,
            params: vec![("softness".into(), 0.5)],
            source: r#"
// Soft additive particle: radial falloff times tint times life-fade.
fn particle_rgba(uv: vec2<f32>, tint: vec3<f32>, life: f32) -> vec4<f32> {
    let d = length(uv - vec2<f32>(0.5, 0.5)) * 2.0;
    let alpha = clamp(1.0 - d, 0.0, 1.0);
    let fade = clamp(life, 0.0, 1.0);
    return vec4<f32>(tint * alpha, alpha * alpha * fade);
}
"#.into(),
        },
        // Animated water surface with scrolling normals.
        ShaderDef {
            id: ShaderId::new("shader.water"),
            name: "Water".into(),
            stage: ShaderStage::Water,
            params: vec![("wave_speed".into(), 0.4), ("wave_scale".into(), 1.5)],
            source: r#"
// Animated water: two scrolling sine wave sets perturb the normal; Fresnel tints.
fn water_normal(pos: vec2<f32>, time: f32, scale: f32, speed: f32) -> vec3<f32> {
    let w1 = sin((pos.x + time * speed) * scale) * 0.5;
    let w2 = sin((pos.y - time * speed * 0.8) * scale * 1.3) * 0.5;
    return normalize(vec3<f32>(w1, 1.0, w2));
}

fn water_color(view: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let fresnel = pow(1.0 - clamp(dot(normalize(view), n), 0.0, 1.0), 3.0);
    let deep = vec3<f32>(0.02, 0.12, 0.2);
    let sky = vec3<f32>(0.5, 0.7, 0.9);
    return mix(deep, sky, fresnel);
}
"#.into(),
        },
    ]
}

// ---------------------------------------------------------------------------
// Mobs — creatures and summons.
// ---------------------------------------------------------------------------

fn mobs() -> Vec<MobDef> {
    vec![
        // The wisp: a friendly summon (and a low-level lowland critter).
        MobDef {
            id: MobId::new("mob.wisp"),
            name: "Wisp".into(),
            max_health: 40.0,
            move_speed: 6.0,
            abilities: vec![AbilityId::new("ability.arcane_lance")],
            xp_reward: 10,
            loot_table: vec![(ItemId::new("item.mana_crystal"), 0.25)],
            material: Some(MaterialId::new("material.crystal")),
            scale: 0.5,
            aggressive: false,
            mesh_seed: 0x7001,
        },
        // Forest guardian: a sturdy lowland defender.
        MobDef {
            id: MobId::new("mob.forest_guardian"),
            name: "Forest Guardian".into(),
            max_health: 220.0,
            move_speed: 3.5,
            abilities: vec![AbilityId::new("ability.ground_slam")],
            xp_reward: 60,
            loot_table: vec![
                (ItemId::new("item.crystal_shard"), 0.5),
                (ItemId::new("item.novice_robe"), 0.1),
            ],
            material: Some(MaterialId::new("material.bark")),
            scale: 2.2,
            aggressive: false,
            mesh_seed: 0x7002,
        },
        // Void wraith: a hostile hollows-dweller.
        MobDef {
            id: MobId::new("mob.void_wraith"),
            name: "Void Wraith".into(),
            max_health: 130.0,
            move_speed: 5.5,
            abilities: vec![AbilityId::new("ability.void_grasp"), AbilityId::new("ability.life_siphon")],
            xp_reward: 90,
            loot_table: vec![(ItemId::new("item.void_essence"), 0.6)],
            material: Some(MaterialId::new("material.void_fog")),
            scale: 1.6,
            aggressive: true,
            mesh_seed: 0x7003,
        },
        // Crystal golem: a heavy highland elite.
        MobDef {
            id: MobId::new("mob.crystal_golem"),
            name: "Crystal Golem".into(),
            max_health: 400.0,
            move_speed: 2.8,
            abilities: vec![AbilityId::new("ability.chain_lightning"), AbilityId::new("ability.ground_slam")],
            xp_reward: 150,
            loot_table: vec![
                (ItemId::new("item.crystal_shard"), 0.9),
                (ItemId::new("item.storm_ring"), 0.05),
            ],
            material: Some(MaterialId::new("material.crystal")),
            scale: 3.0,
            aggressive: true,
            mesh_seed: 0x7004,
        },
    ]
}

// ---------------------------------------------------------------------------
// Missions — starter objectives.
// ---------------------------------------------------------------------------

fn missions() -> Vec<MissionDef> {
    vec![
        // Tutorial-flavoured: learn fire by burning wisps.
        MissionDef {
            id: MissionId::new("mission.first_flame"),
            name: "First Flame".into(),
            description: "Prove your spark: scatter the wisps of the lowlands with fire.".into(),
            objectives: vec![
                Objective::Kill { mob: Some(MobId::new("mob.wisp")), count: 5 },
                Objective::CastSpell { spell: Some(SpellId::new("spell.fireball")), count: 10 },
            ],
            xp_reward: 100,
            item_rewards: vec![(ItemId::new("item.ember_staff"), 1)],
            tech_points: 2,
            level_req: 1,
            repeatable: false,
        },
        // Gathering loop: collect crystal shards.
        MissionDef {
            id: MissionId::new("mission.gather_crystals"),
            name: "Shards of the Highlands".into(),
            description: "Harvest crystal shards from the golems and formations of the heights.".into(),
            objectives: vec![Objective::Collect { item: ItemId::new("item.crystal_shard"), count: 10 }],
            xp_reward: 120,
            item_rewards: vec![(ItemId::new("item.mana_crystal"), 3)],
            tech_points: 2,
            level_req: 3,
            repeatable: true,
        },
        // Exploration: reach the sunken monolith.
        MissionDef {
            id: MissionId::new("mission.reach_monolith"),
            name: "The Sunken Monolith".into(),
            description: "Travel to the monolith said to rise from the drowned lowlands.".into(),
            objectives: vec![
                Objective::ReachPoint { point: [512.0, 30.0, -480.0], radius: 20.0 },
                Objective::Explore { zone_count: 2 },
            ],
            xp_reward: 200,
            item_rewards: vec![(ItemId::new("item.arcane_orb"), 1)],
            tech_points: 3,
            level_req: 5,
            repeatable: false,
        },
        // Endurance: survive the void hollows.
        MissionDef {
            id: MissionId::new("mission.survive_hollows"),
            name: "Into the Hollows".into(),
            description: "Descend into the void hollows and endure the wraiths' assault.".into(),
            objectives: vec![
                Objective::Survive { seconds: 180.0 },
                Objective::Kill { mob: Some(MobId::new("mob.void_wraith")), count: 3 },
            ],
            xp_reward: 300,
            item_rewards: vec![(ItemId::new("item.void_essence"), 5)],
            tech_points: 4,
            level_req: 10,
            repeatable: true,
        },
    ]
}
