//! `gear.rs` — the flagship demonstration of the item system as **pure data**.
//!
//! [`apply`] grafts a complete gear economy onto a [`ContentPack`] without a single
//! new engine primitive: essences (the upgrade currency), a deep affix pool, five gem
//! families that fuse and read differently in weapons vs armor, runewords, permanent
//! enchants, two full sets, and a row of signature legendary/mythic items whose effects
//! are felt the instant you equip them. Everything routes through the same closed
//! [`StatMods`] + [`ItemTrigger`] vocabulary the sim already interprets, so a designer
//! triples the loot table live with no code deploy — the keystone of Cerena.
//!
//! Design law honoured here (Leif): *every* piece does something you notice. There is
//! no "+2 vitality, the end" gear in this pack — the worst common drop still rolls an
//! affix, and the chase pieces rewrite how you play.

use crate::affix::{AffixKind, affix};
use crate::enchant::{EnchantDef, RunewordDef};
use crate::gem::gem;
use crate::ids::{ItemId, SetId};
use crate::item::{
    ElementMods, EquipSlot, ItemDef, ItemTrigger, ProcEffect, ProcWhen, Rarity, StatMods,
};
use crate::itemset::{SetBonus, SetDef};
use crate::pack::ContentPack;
use crate::status::{StatusEffectDef, StatusKind};

// --- tiny constructors so the tables below read like a spreadsheet -----------------

fn sm() -> StatMods {
    StatMods::default()
}
fn em(fire: f32, frost: f32, storm: f32, arcane: f32) -> ElementMods {
    ElementMods { fire, frost, storm, arcane, ..Default::default() }
}
/// An always-on proc (chance 1.0, no internal cooldown) — used for auras / on-equip.
fn aura(label: &str, effect: ProcEffect) -> ItemTrigger {
    ItemTrigger { label: label.into(), when: ProcWhen::Aura, chance: 1.0, icd_s: 0.0, effect }
}
fn proc(label: &str, when: ProcWhen, chance: f32, icd_s: f32, effect: ProcEffect) -> ItemTrigger {
    ItemTrigger { label: label.into(), when, chance, icd_s, effect }
}

/// Graft the whole gear economy onto `pack`. Purely additive — like
/// [`crate::expansion::apply`], it introduces no new engine variants.
pub fn apply(pack: &mut ContentPack) {
    statuses(pack);
    essences(pack);
    affixes(pack);
    gems(pack);
    runewords(pack);
    enchants(pack);
    legendaries(pack);
    sets(pack);
}

// ------------------------------------------------------------------------------------
// Proc statuses. The procs below lean on these; kept short and synergistic.
// ------------------------------------------------------------------------------------

fn statuses(pack: &mut ContentPack) {
    let mut s = |id: &str, name: &str, kind: StatusKind, dur: f32, max: u8, good: bool| {
        pack.statuses.push(StatusEffectDef {
            id: id.into(),
            name: name.into(),
            kind,
            max_stacks: max,
            tick_interval_s: 0.5,
            duration_default_s: dur,
            beneficial: good,
            material: None,
        });
    };
    s("status.gear.ignite", "Ignited", StatusKind::Burning { dps: 18.0 }, 4.0, 5, false);
    s("status.gear.chill", "Chilled", StatusKind::Slow { frac: 0.35 }, 3.0, 1, false);
    s("status.gear.sunder", "Sundered", StatusKind::Vulnerable { frac: 0.18 }, 6.0, 3, false);
    s("status.gear.bleed", "Rent", StatusKind::DamageOverTime { element: "physical".into(), dps: 22.0 }, 5.0, 5, false);
    s("status.gear.frenzy", "Frenzied", StatusKind::Haste { frac: 0.30 }, 4.0, 1, true);
    s("status.gear.berserk", "Berserk", StatusKind::Empower { frac: 0.25 }, 5.0, 1, true);
    s("status.gear.aegis", "Wardlight", StatusKind::Shielded { amount: 120.0 }, 4.0, 1, true);
    s("status.gear.regen", "Mending", StatusKind::Regen { hps: 14.0 }, 6.0, 1, true);
}

// ------------------------------------------------------------------------------------
// Essences — the upgrade currency. Stackable reagents, never equipped.
// ------------------------------------------------------------------------------------

fn essences(pack: &mut ContentPack) {
    let mut e = |id: &str, name: &str, desc: &str| {
        let mut d = ItemDef::base(id, name, EquipSlot::None, Rarity::Uncommon);
        d.description = desc.into();
        d.stackable = true;
        d.max_stack = 999;
        d.upgrade = None;
        pack.items.push(d);
    };
    e("item.essence.arcane", "Arcane Essence", "Forge fuel. Spent to raise an item's upgrade level.");
    e("item.essence.chaos", "Chaos Shard", "Reforges an item, rerolling all of its random affixes.");
    e("item.essence.binding", "Binding Sigil", "Imprints one affix so the next reforge keeps it.");
    e("item.essence.boring", "Boring Drill", "Bores a new socket into a piece of gear.");
    e("item.essence.solvent", "Aether Solvent", "Pops a socketed gem back out intact.");
    e("item.essence.polish", "Whetstone", "Raises an item's quality toward a flawless 100.");
}

// ------------------------------------------------------------------------------------
// Affixes — the rollable soul of common/rare loot. Prefixes shape offence, suffixes
// shape utility/defence, every one of them does something you feel.
// ------------------------------------------------------------------------------------

fn affixes(pack: &mut ContentPack) {
    use AffixKind::{Prefix, Suffix};
    let p = &mut pack.affixes;

    // --- PREFIXES (leading adjective) ---
    p.push(
        affix("affix.flaming", Prefix, "Flaming", 2)
            .tagged(&["caster", "weapon", "fire"])
            .on_slots(&[EquipSlot::Staff, EquipSlot::Weapon, EquipSlot::Gloves])
            .range(
                StatMods { elem_damage: em(0.08, 0.0, 0.0, 0.0), ..sm() },
                StatMods { elem_damage: em(0.22, 0.0, 0.0, 0.0), ..sm() },
            )
            .with_proc(proc(
                "Ignite on hit",
                ProcWhen::OnHit,
                0.20,
                1.0,
                ProcEffect::DebuffTarget { status: "status.gear.ignite".into(), duration_s: 4.0, stacks: 1 },
            )),
    );
    p.push(
        affix("affix.glacial", Prefix, "Glacial", 2)
            .tagged(&["caster", "weapon", "frost"])
            .range(
                StatMods { elem_damage: em(0.0, 0.08, 0.0, 0.0), ..sm() },
                StatMods { elem_damage: em(0.0, 0.24, 0.0, 0.0), ..sm() },
            )
            .with_proc(proc(
                "Chill on hit",
                ProcWhen::OnHit,
                0.30,
                0.0,
                ProcEffect::DebuffTarget { status: "status.gear.chill".into(), duration_s: 3.0, stacks: 1 },
            )),
    );
    p.push(
        affix("affix.savage", Prefix, "Savage", 3)
            .tagged(&["weapon", "melee"])
            .on_slots(&[EquipSlot::Weapon])
            .range(
                StatMods { melee_power_pct: 0.10, crit_chance: 0.03, ..sm() },
                StatMods { melee_power_pct: 0.30, crit_chance: 0.08, ..sm() },
            ),
    );
    p.push(
        affix("affix.vampiric", Prefix, "Vampiric", 4)
            .tagged(&["weapon", "melee", "universal"])
            .range(
                StatMods { lifesteal: 0.03, ..sm() },
                StatMods { lifesteal: 0.09, ..sm() },
            ),
    );
    p.push(
        affix("affix.arcane", Prefix, "Arcane", 3)
            .tagged(&["caster", "armor"])
            .range(
                StatMods { spell_power_pct: 0.06, max_mana: 20.0, ..sm() },
                StatMods { spell_power_pct: 0.18, max_mana: 70.0, ..sm() },
            ),
    );
    p.push(
        affix("affix.fleet", Prefix, "Fleet", 2)
            .tagged(&["boots", "armor"])
            .on_slots(&[EquipSlot::Boots])
            .range(
                StatMods { move_speed: 0.5, agility: 4.0, ..sm() },
                StatMods { move_speed: 1.8, agility: 12.0, ..sm() },
            ),
    );
    p.push(
        affix("affix.titan", Prefix, "Titanic", 4)
            .tagged(&["armor", "universal"])
            .range(
                StatMods { max_health: 60.0, armor: 6.0, ..sm() },
                StatMods { max_health: 220.0, armor: 22.0, ..sm() },
            ),
    );
    p.push(
        affix("affix.cruel", Prefix, "Cruel", 6)
            .tagged(&["weapon", "caster"])
            .range(
                StatMods { crit_damage: 0.15, crit_chance: 0.04, ..sm() },
                StatMods { crit_damage: 0.55, crit_chance: 0.12, ..sm() },
            ),
    );

    // --- SUFFIXES ("of the X") ---
    p.push(
        affix("affix.of_bear", Suffix, "of the Bear", 2)
            .range(
                StatMods { vitality: 6.0, max_health: 40.0, ..sm() },
                StatMods { vitality: 18.0, max_health: 130.0, ..sm() },
            ),
    );
    p.push(
        affix("affix.of_fox", Suffix, "of the Fox", 2)
            .range(
                StatMods { agility: 6.0, cooldown_reduction: 0.03, ..sm() },
                StatMods { agility: 16.0, cooldown_reduction: 0.10, ..sm() },
            ),
    );
    p.push(
        affix("affix.of_storms", Suffix, "of Storms", 5)
            .tagged(&["weapon", "caster", "storm"])
            .with_proc(proc(
                "Chain lightning on hit",
                ProcWhen::OnHit,
                0.15,
                2.0,
                ProcEffect::ChainBolt { element: "storm".into(), jumps: 3, damage: 60.0 },
            ))
            .range(
                StatMods { elem_damage: em(0.0, 0.0, 0.10, 0.0), ..sm() },
                StatMods { elem_damage: em(0.0, 0.0, 0.28, 0.0), ..sm() },
            ),
    );
    p.push(
        affix("affix.of_leech", Suffix, "of Leeching", 3)
            .tagged(&["caster", "universal"])
            .range(
                StatMods { mana_leech: 0.04, mana_on_kill: 8.0, ..sm() },
                StatMods { mana_leech: 0.12, mana_on_kill: 30.0, ..sm() },
            ),
    );
    p.push(
        affix("affix.of_warding", Suffix, "of Warding", 4)
            .tagged(&["armor", "universal"])
            .range(
                StatMods { armor_pct: 0.03, tenacity: 0.05, elem_resist: em(0.04, 0.04, 0.04, 0.04), ..sm() },
                StatMods { armor_pct: 0.10, tenacity: 0.20, elem_resist: em(0.12, 0.12, 0.12, 0.12), ..sm() },
            ),
    );
    p.push(
        affix("affix.of_fortune", Suffix, "of Fortune", 5)
            .tagged(&["universal"])
            .range(
                StatMods { magic_find: 0.05, gold_find: 0.10, xp_gain_pct: 0.03, ..sm() },
                StatMods { magic_find: 0.25, gold_find: 0.40, xp_gain_pct: 0.12, ..sm() },
            ),
    );
    p.push(
        affix("affix.of_thorns", Suffix, "of Thorns", 3)
            .tagged(&["armor", "universal"])
            .range(
                StatMods { thorns: 0.06, ..sm() },
                StatMods { thorns: 0.22, ..sm() },
            ),
    );
}

// ------------------------------------------------------------------------------------
// Gems & runes. Five colours, each fusing chipped -> flawed -> perfect, reading
// differently in weapon vs armor vs jewellery. The lettered ones double as runes.
// ------------------------------------------------------------------------------------

fn gems(pack: &mut ContentPack) {
    let g = &mut pack.gems;
    // Ruby — fire / health.
    g.push(gem("gem.ruby.1", "Chipped Ruby", 1)
        .weapon(StatMods { elem_damage: em(0.06, 0.0, 0.0, 0.0), ..sm() })
        .armor(StatMods { max_health: 40.0, ..sm() })
        .jewel(StatMods { elem_resist: em(0.05, 0.0, 0.0, 0.0), ..sm() })
        .rune("Ruby"));
    g.push(gem("gem.ruby.2", "Flawed Ruby", 2)
        .weapon(StatMods { elem_damage: em(0.13, 0.0, 0.0, 0.0), ..sm() })
        .armor(StatMods { max_health: 90.0, ..sm() })
        .jewel(StatMods { elem_resist: em(0.10, 0.0, 0.0, 0.0), ..sm() })
        .rune("Ruby")
        .fuses_from("gem.ruby.1", 3));
    g.push(gem("gem.ruby.3", "Perfect Ruby", 3)
        .weapon(StatMods { elem_damage: em(0.24, 0.0, 0.0, 0.0), crit_damage: 0.10, ..sm() })
        .armor(StatMods { max_health: 180.0, ..sm() })
        .jewel(StatMods { elem_resist: em(0.18, 0.0, 0.0, 0.0), ..sm() })
        .rune("Ruby")
        .fuses_from("gem.ruby.2", 3));
    // Sapphire — frost / mana.
    g.push(gem("gem.sapphire.1", "Chipped Sapphire", 1)
        .weapon(StatMods { elem_damage: em(0.0, 0.06, 0.0, 0.0), ..sm() })
        .armor(StatMods { max_mana: 35.0, ..sm() })
        .jewel(StatMods { mana_regen: 2.0, ..sm() })
        .rune("Sapphire"));
    g.push(gem("gem.sapphire.3", "Perfect Sapphire", 3)
        .weapon(StatMods { elem_damage: em(0.0, 0.24, 0.0, 0.0), ..sm() })
        .armor(StatMods { max_mana: 160.0, ..sm() })
        .jewel(StatMods { mana_regen: 8.0, cooldown_reduction: 0.05, ..sm() })
        .rune("Sapphire")
        .fuses_from("gem.sapphire.1", 9));
    // Topaz — storm / magic-find.
    g.push(gem("gem.topaz.3", "Perfect Topaz", 3)
        .weapon(StatMods { elem_damage: em(0.0, 0.0, 0.24, 0.0), ..sm() })
        .armor(StatMods { magic_find: 0.10, ..sm() })
        .jewel(StatMods { magic_find: 0.18, gold_find: 0.20, ..sm() })
        .rune("Topaz"));
    // Emerald — nature / crit.
    g.push(gem("gem.emerald.3", "Perfect Emerald", 3)
        .weapon(StatMods { crit_chance: 0.06, elem_damage: ElementMods { nature: 0.18, ..Default::default() }, ..sm() })
        .armor(StatMods { health_regen: 6.0, tenacity: 0.08, ..sm() })
        .jewel(StatMods { crit_chance: 0.04, ..sm() })
        .rune("Emerald"));
    // Diamond — radiant / all-resist, and the proc gem.
    g.push(gem("gem.diamond.3", "Perfect Diamond", 3)
        .weapon(StatMods { elem_damage: ElementMods { radiant: 0.20, physical: 0.12, ..Default::default() }, ..sm() })
        .armor(StatMods { elem_resist: em(0.06, 0.06, 0.06, 0.06), armor_pct: 0.04, ..sm() })
        .jewel(StatMods { crit_damage: 0.20, ..sm() })
        .rune("Diamond")
        .with_proc(proc(
            "Radiant nova on crit",
            ProcWhen::OnCrit,
            0.25,
            3.0,
            ProcEffect::Nova { element: "radiant".into(), radius: 4.0, damage: 90.0 },
        )));
    // Pure runes (no stat value alone — they exist for runewords).
    g.push(gem("rune.kor", "Kor Rune", 2).rune("Kor"));
    g.push(gem("rune.vaal", "Vaal Rune", 3).rune("Vaal"));
    g.push(gem("rune.sol", "Sol Rune", 3).rune("Sol"));
    g.push(gem("rune.eth", "Eth Rune", 4).rune("Eth"));
}

// ------------------------------------------------------------------------------------
// Runewords. Worthless apart, build-defining together.
// ------------------------------------------------------------------------------------

fn runewords(pack: &mut ContentPack) {
    pack.runewords.push(RunewordDef {
        id: "runeword.stormbringer".into(),
        name: "Stormbringer".into(),
        sequence: vec!["Sol".into(), "Topaz".into(), "Vaal".into()],
        base_slots: vec![EquipSlot::Staff, EquipSlot::Weapon],
        level_req: 20,
        mods: StatMods {
            attack_speed_pct: 0.20,
            elem_damage: em(0.0, 0.0, 0.35, 0.0),
            crit_chance: 0.08,
            ..sm()
        },
        triggers: vec![proc(
            "Lightning storm on hit",
            ProcWhen::OnHit,
            0.25,
            1.5,
            ProcEffect::ChainBolt { element: "storm".into(), jumps: 5, damage: 110.0 },
        )],
        grants_spells: vec![],
    });
    pack.runewords.push(RunewordDef {
        id: "runeword.bulwark".into(),
        name: "Bulwark".into(),
        sequence: vec!["Eth".into(), "Diamond".into(), "Ruby".into()],
        base_slots: vec![EquipSlot::Robe, EquipSlot::Offhand],
        level_req: 24,
        mods: StatMods {
            max_health: 350.0,
            armor_pct: 0.12,
            elem_resist: em(0.15, 0.15, 0.15, 0.15),
            thorns: 0.20,
            ..sm()
        },
        triggers: vec![proc(
            "Wardlight at low health",
            ProcWhen::OnLowHealth { frac: 0.30 },
            1.0,
            12.0,
            ProcEffect::BuffSelf { status: "status.gear.aegis".into(), duration_s: 5.0, stacks: 1 },
        )],
        grants_spells: vec![],
    });
}

// ------------------------------------------------------------------------------------
// Enchants — the final polish layer.
// ------------------------------------------------------------------------------------

fn enchants(pack: &mut ContentPack) {
    pack.enchants.push(EnchantDef {
        id: "enchant.brilliant".into(),
        name: "Brilliant".into(),
        slots: vec![],
        level_req: 10,
        mods: StatMods { spell_power_pct: 0.08, cast_speed_pct: 0.06, ..sm() },
        proc: None,
        name_word: Some("Brilliant".into()),
    });
    pack.enchants.push(EnchantDef {
        id: "enchant.bloodthirsty".into(),
        name: "Bloodthirsty".into(),
        slots: vec![EquipSlot::Weapon, EquipSlot::Staff],
        level_req: 16,
        mods: StatMods { lifesteal: 0.05, ..sm() },
        proc: Some(proc(
            "Frenzy on kill",
            ProcWhen::OnKill,
            1.0,
            0.0,
            ProcEffect::BuffSelf { status: "status.gear.frenzy".into(), duration_s: 4.0, stacks: 1 },
        )),
        name_word: Some("Bloodthirsty".into()),
    });
}

// ------------------------------------------------------------------------------------
// Legendaries & Mythics — hand-authored signature items. Each rewrites a verb.
// ------------------------------------------------------------------------------------

fn legendaries(pack: &mut ContentPack) {
    // A growing sword: stronger with every kill, resets a little on death.
    pack.items.push(
        ItemDef::base("item.legendary.hungering_edge", "The Hungering Edge", EquipSlot::Weapon, Rarity::Legendary)
            .described("A blade that remembers every life it has taken. Each kill makes it hungrier — and it never forgets a duel.")
            .with_stats(StatMods { melee_power_pct: 0.25, crit_chance: 0.08, lifesteal: 0.06, attack_speed_pct: 0.10, ..sm() })
            .with_trigger(proc(
                "Devour: +1% damage per recent kill (decays)",
                ProcWhen::OnKill,
                1.0,
                0.0,
                ProcEffect::TempStats { mods: Box::new(StatMods { melee_power_pct: 0.01, ..sm() }), duration_s: 12.0 },
            ))
            .with_trigger(proc(
                "Rend on hit",
                ProcWhen::OnHit,
                0.30,
                0.0,
                ProcEffect::DebuffTarget { status: "status.gear.bleed".into(), duration_s: 5.0, stacks: 1 },
            ))
            .with_sockets(2)
            .require_level(20)
            .unique_bound(),
    );
    // Boots that leave a fire trail and grant a second dash.
    pack.items.push(
        ItemDef::base("item.legendary.ember_striders", "Ember Striders", EquipSlot::Boots, Rarity::Legendary)
            .described("You do not walk. You burn a path, and the world hurries to follow.")
            .with_stats(StatMods { move_speed: 2.2, agility: 14.0, dash_charges: 1.0, fall_damage_pct: 0.5, elem_resist: em(0.20, 0.0, 0.0, 0.0), ..sm() })
            .with_trigger(proc(
                "Scorched trail while moving",
                ProcWhen::Interval { secs: 0.5 },
                1.0,
                0.0,
                ProcEffect::Nova { element: "fire".into(), radius: 2.5, damage: 24.0 },
            ))
            .with_trigger(proc(
                "Blink-ignite on dash",
                ProcWhen::OnDash,
                1.0,
                0.0,
                ProcEffect::Nova { element: "fire".into(), radius: 4.0, damage: 80.0 },
            ))
            .with_sockets(1)
            .require_level(18)
            .unique_bound(),
    );
    // A staff that copies the last spell you cast at a chance.
    pack.items.push(
        ItemDef::base("item.legendary.echo_of_creation", "Echo of Creation", EquipSlot::Staff, Rarity::Legendary)
            .described("The Weave stutters around it, and your magic happens twice.")
            .with_stats(StatMods { spell_power_pct: 0.22, max_mana: 140.0, cast_speed_pct: 0.12, extra_projectiles: 1.0, range_pct: 0.15, ..sm() })
            .with_trigger(proc(
                "Echo: recast the spell on cast",
                ProcWhen::OnCast,
                0.20,
                0.0,
                ProcEffect::BuffSelf { status: "status.gear.berserk".into(), duration_s: 2.0, stacks: 1 },
            ))
            .with_sockets(2)
            .require_level(22)
            .unique_bound(),
    );
    // A robe that turns overheal into a shield and frost-novas attackers.
    pack.items.push(
        ItemDef::base("item.legendary.winterheart_mantle", "Winterheart Mantle", EquipSlot::Robe, Rarity::Legendary)
            .described("A heart of permanent winter. What cannot mend you, armours you instead.")
            .with_stats(StatMods { max_health: 240.0, max_mana: 120.0, armor_pct: 0.08, elem_resist: em(0.0, 0.30, 0.0, 0.0), health_regen: 10.0, ..sm() })
            .with_trigger(proc(
                "Frost nova when struck",
                ProcWhen::OnTakeDamage,
                0.25,
                2.5,
                ProcEffect::Nova { element: "frost".into(), radius: 5.0, damage: 70.0 },
            ))
            .with_trigger(aura(
                "Overheal becomes ward",
                ProcEffect::Shield { amount: 60.0, duration_s: 3.0 },
            ))
            .with_sockets(3)
            .require_level(20)
            .unique_bound(),
    );
    // The Mythic ring: doubles a random equipped gem and warps the night.
    pack.items.push(
        ItemDef::base("item.mythic.signet_of_the_conjunction", "Signet of the Conjunction", EquipSlot::Ring, Rarity::Mythic)
            .described("Forged in the once-an-age Grand Conjunction. Reality treats its bearer as a suggestion.")
            .with_stats(StatMods { power: 20.0, focus: 20.0, spell_power_pct: 0.18, crit_chance: 0.10, crit_damage: 0.40, cooldown_reduction: 0.12, magic_find: 0.20, ..sm() })
            .with_trigger(proc(
                "Conjunction: empower on crit",
                ProcWhen::OnCrit,
                0.40,
                4.0,
                ProcEffect::TempStats { mods: Box::new(StatMods { spell_power_pct: 0.25, attack_speed_pct: 0.20, ..sm() }), duration_s: 5.0 },
            ))
            .with_sockets(2)
            .require_level(30)
            .unique_bound(),
    );
}

// ------------------------------------------------------------------------------------
// Sets — the aspirational complete look.
// ------------------------------------------------------------------------------------

fn sets(pack: &mut ContentPack) {
    // --- The Pyrelord Regalia (fire caster set) ---
    let pyre: &[(&str, &str, EquipSlot)] = &[
        ("item.set.pyrelord.helm", "Pyrelord Crown", EquipSlot::Helm),
        ("item.set.pyrelord.robe", "Pyrelord Robe", EquipSlot::Robe),
        ("item.set.pyrelord.gloves", "Pyrelord Gauntlets", EquipSlot::Gloves),
        ("item.set.pyrelord.boots", "Pyrelord Sandals", EquipSlot::Boots),
        ("item.set.pyrelord.staff", "Pyrelord Brand", EquipSlot::Staff),
    ];
    for (id, name, slot) in pyre {
        pack.items.push(
            ItemDef::base(id, name, *slot, Rarity::Epic)
                .described("Part of the Pyrelord Regalia. The fire remembers its king.")
                .with_stats(StatMods { spell_power_pct: 0.06, max_mana: 50.0, elem_damage: em(0.10, 0.0, 0.0, 0.0), ..sm() })
                .in_set("set.pyrelord")
                .with_sockets(1)
                .require_level(16),
        );
    }
    pack.item_sets.push(SetDef {
        id: SetId::new("set.pyrelord"),
        name: "Pyrelord Regalia".into(),
        pieces: pyre.iter().map(|(id, _, _)| ItemId::new(*id)).collect(),
        bonuses: vec![
            SetBonus {
                pieces_required: 2,
                description: "(2) +20% fire damage".into(),
                mods: StatMods { elem_damage: em(0.20, 0.0, 0.0, 0.0), ..sm() },
                triggers: vec![],
                grants_spells: vec![],
            },
            SetBonus {
                pieces_required: 3,
                description: "(3) +25% spell power, your fire ignites".into(),
                mods: StatMods { spell_power_pct: 0.25, ..sm() },
                triggers: vec![proc(
                    "Ignite on every fire hit",
                    ProcWhen::OnHit,
                    1.0,
                    0.0,
                    ProcEffect::DebuffTarget { status: "status.gear.ignite".into(), duration_s: 4.0, stacks: 1 },
                )],
                grants_spells: vec![],
            },
            SetBonus {
                pieces_required: 5,
                description: "(5) Critical fire spells erupt in a meteor nova".into(),
                mods: StatMods { crit_chance: 0.10, crit_damage: 0.50, elem_damage: em(0.30, 0.0, 0.0, 0.0), ..sm() },
                triggers: vec![proc(
                    "Meteor on fire crit",
                    ProcWhen::OnCrit,
                    1.0,
                    1.0,
                    ProcEffect::Nova { element: "fire".into(), radius: 7.0, damage: 240.0 },
                )],
                grants_spells: vec![],
            },
        ],
    });

    // --- The Stoneward Bastion (tank/melee set) ---
    let stone: &[(&str, &str, EquipSlot)] = &[
        ("item.set.stoneward.helm", "Stoneward Helm", EquipSlot::Helm),
        ("item.set.stoneward.robe", "Stoneward Cuirass", EquipSlot::Robe),
        ("item.set.stoneward.boots", "Stoneward Greaves", EquipSlot::Boots),
        ("item.set.stoneward.weapon", "Stoneward Maul", EquipSlot::Weapon),
    ];
    for (id, name, slot) in stone {
        pack.items.push(
            ItemDef::base(id, name, *slot, Rarity::Epic)
                .described("Part of the Stoneward Bastion. The mountain does not move.")
                .with_stats(StatMods { max_health: 120.0, armor: 12.0, tenacity: 0.08, ..sm() })
                .in_set("set.stoneward")
                .with_sockets(1)
                .require_level(16),
        );
    }
    pack.item_sets.push(SetDef {
        id: SetId::new("set.stoneward"),
        name: "Stoneward Bastion".into(),
        pieces: stone.iter().map(|(id, _, _)| ItemId::new(*id)).collect(),
        bonuses: vec![
            SetBonus {
                pieces_required: 2,
                description: "(2) +12% damage reduction, +30% thorns".into(),
                mods: StatMods { armor_pct: 0.12, thorns: 0.30, ..sm() },
                triggers: vec![],
                grants_spells: vec![],
            },
            SetBonus {
                pieces_required: 4,
                description: "(4) Below 35% health, become an unbreakable bastion".into(),
                mods: StatMods { max_health: 400.0, block_chance: 0.20, ..sm() },
                triggers: vec![proc(
                    "Bastion at low health",
                    ProcWhen::OnLowHealth { frac: 0.35 },
                    1.0,
                    15.0,
                    ProcEffect::TempStats { mods: Box::new(StatMods { armor_pct: 0.40, tenacity: 0.50, ..sm() }), duration_s: 6.0 },
                )],
                grants_spells: vec![],
            },
        ],
    });
}
