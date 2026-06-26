//! Items: equippable gear, consumables, and craftable materials.
//!
//! An [`ItemDef`] is pure data. Items are the main way a player gains *capabilities*
//! (spells, movement modes, abilities) and *stats* in the world: a staff grants a
//! fireball, boots grant a dash, a robe boosts mana. Live inventories store
//! per-character [`crate::ids::ItemId`]s (and, in the sim, rolled *instances* that
//! layer upgrade level, affixes, sockets, and enchants on top), so re-tuning a base
//! item is a hot-reload, never a save migration.
//!
//! Design law for Cerena gear (Leif): **everything you wear has a noticeable effect
//! and impact.** A sword is not "+3 damage" — it is a combo-extending arc that chains
//! lightning on crit and grows hungrier with every kill. Boots leave a fire trail.
//! A robe converts overheal into a shield. To make that expressive *as data*, an item
//! carries not only flat [`StatMods`] but a set of [`ItemTrigger`] procs, sockets,
//! a set membership, and an [`UpgradeProfile`] — all composed by the sim from the
//! same closed primitive sets (the spell VM does the actual work).

use serde::{Deserialize, Serialize};

use crate::ids::{
    AbilityId, AffixId, ElementId, ItemId, MaterialId, MobId, MovementModeId, SetId, SpellId,
    StatusId, TechNodeId,
};

/// How rare / powerful an item is. Drives drop weighting, affix budget, socket count,
/// name tint, and the VFX budget the client spends on its glow. The sim does branch on
/// it for the *roll* (how many affixes / sockets a dropped instance gets), but combat
/// reads the rolled [`StatMods`], not the tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Epic,
    Legendary,
    Mythic,
}

impl Rarity {
    /// The maximum number of *random* affixes an instance of this rarity may roll.
    /// Legendary/Mythic items are largely defined by fixed signature mechanics, so
    /// they roll *fewer* random affixes on top of a richer base.
    pub fn affix_budget(self) -> u8 {
        match self {
            Rarity::Common => 0,
            Rarity::Uncommon => 2,
            Rarity::Rare => 4,
            Rarity::Epic => 5,
            Rarity::Legendary => 3,
            Rarity::Mythic => 4,
        }
    }

    /// The maximum number of sockets an instance of this rarity may roll.
    pub fn socket_budget(self) -> u8 {
        match self {
            Rarity::Common => 0,
            Rarity::Uncommon => 1,
            Rarity::Rare => 2,
            Rarity::Epic => 3,
            Rarity::Legendary => 3,
            Rarity::Mythic => 4,
        }
    }

    /// Multiplier applied to a rolled instance's base stat budget. Higher rarity bases
    /// simply carry more.
    pub fn power_factor(self) -> f32 {
        match self {
            Rarity::Common => 1.0,
            Rarity::Uncommon => 1.15,
            Rarity::Rare => 1.35,
            Rarity::Epic => 1.6,
            Rarity::Legendary => 2.0,
            Rarity::Mythic => 2.6,
        }
    }

    /// Relative drop weight (higher = more common). Magic-find shifts the roll up this
    /// ladder; see `arena_sim::forge`.
    pub fn drop_weight(self) -> f32 {
        match self {
            Rarity::Common => 1000.0,
            Rarity::Uncommon => 420.0,
            Rarity::Rare => 130.0,
            Rarity::Epic => 34.0,
            Rarity::Legendary => 7.0,
            Rarity::Mythic => 1.0,
        }
    }

    /// The next rarity up (saturating at `Mythic`). Used by magic-find upgrade rolls
    /// and by gem/essence fusion that "promotes" an item's tier.
    pub fn promote(self) -> Rarity {
        match self {
            Rarity::Common => Rarity::Uncommon,
            Rarity::Uncommon => Rarity::Rare,
            Rarity::Rare => Rarity::Epic,
            Rarity::Epic => Rarity::Legendary,
            Rarity::Legendary => Rarity::Mythic,
            Rarity::Mythic => Rarity::Mythic,
        }
    }

    /// Client name tint as a linear-ish RGB triple (the client maps it to its palette).
    pub fn tint(self) -> [f32; 3] {
        match self {
            Rarity::Common => [0.78, 0.78, 0.80],
            Rarity::Uncommon => [0.30, 0.85, 0.35],
            Rarity::Rare => [0.30, 0.55, 1.00],
            Rarity::Epic => [0.70, 0.35, 0.95],
            Rarity::Legendary => [1.00, 0.62, 0.18],
            Rarity::Mythic => [1.00, 0.30, 0.45],
        }
    }
}

/// The body slot an item occupies. A character may equip one item per non-`None`
/// slot (relics/trinkets/consumables follow their own inventory rules in the sim).
/// `None` means the item is never equipped (a pure crafting reagent / essence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EquipSlot {
    /// Main hand: a staff/wand/orb. The caster's primary magic implement.
    Staff,
    /// Main hand alternative: a melee weapon (sword/axe/maul). Drives `World::melee_attack`.
    Weapon,
    /// Off-hand focus / tome / shield. Grants block + a defensive proc niche.
    Offhand,
    /// Chest: robe / cuirass. The biggest stat block; mana and survivability.
    Robe,
    /// Head: hat / circlet / helm.
    Helm,
    /// Hands: gloves / gauntlets. Attack/cast speed niche.
    Gloves,
    /// Feet: boots / sabatons. Movement, dashes, parkour grants.
    Boots,
    /// Belt: a quick-slot / utility niche.
    Belt,
    Ring,
    Amulet,
    /// A powerful socketable artifact (one of a kind effects).
    Relic,
    Consumable,
    Trinket,
    None,
}

impl EquipSlot {
    /// Whether this slot is "armor" for the purpose of gem behaviour (gems read
    /// differently in weapons vs armor, a la classic ARPGs) and set accounting.
    pub fn is_armor(self) -> bool {
        matches!(
            self,
            EquipSlot::Robe
                | EquipSlot::Helm
                | EquipSlot::Gloves
                | EquipSlot::Boots
                | EquipSlot::Belt
                | EquipSlot::Offhand
        )
    }

    /// Whether this slot deals damage (weapon/staff) for gem behaviour.
    pub fn is_weapon(self) -> bool {
        matches!(self, EquipSlot::Staff | EquipSlot::Weapon)
    }

    /// Whether a character may hold more than one equipped (rings).
    pub fn is_multi(self) -> bool {
        matches!(self, EquipSlot::Ring)
    }
}

/// Per-element damage / resistance modifiers. Folded into combat by `arena-sim`:
/// `elem_damage` raises outgoing damage of that element, `elem_resist` cuts incoming.
/// Kept as named fields (rather than a map) so it stays `Copy` and folds with the
/// rest of [`StatMods`] in one pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct ElementMods {
    pub fire: f32,
    pub frost: f32,
    pub storm: f32,
    pub arcane: f32,
    pub nature: f32,
    pub necro: f32,
    pub chrono: f32,
    pub radiant: f32,
    /// Physical (melee / non-elemental). Swords and thorns route through here.
    pub physical: f32,
}

impl ElementMods {
    pub fn combine(&self, o: &Self) -> Self {
        Self {
            fire: self.fire + o.fire,
            frost: self.frost + o.frost,
            storm: self.storm + o.storm,
            arcane: self.arcane + o.arcane,
            nature: self.nature + o.nature,
            necro: self.necro + o.necro,
            chrono: self.chrono + o.chrono,
            radiant: self.radiant + o.radiant,
            physical: self.physical + o.physical,
        }
    }

    /// Resolve the modifier for an element by its content id. Unknown elements map to
    /// `physical` so non-elemental hits still benefit from "+physical" gear.
    pub fn get(&self, element: &ElementId) -> f32 {
        match element.as_str() {
            "fire" => self.fire,
            "frost" => self.frost,
            "storm" => self.storm,
            "arcane" => self.arcane,
            "nature" => self.nature,
            "necro" | "void" => self.necro,
            "chrono" | "time" => self.chrono,
            "radiant" | "light" => self.radiant,
            _ => self.physical,
        }
    }

    /// Scale every field (used to roll an affix value between min/max).
    pub fn scaled(&self, t: f32) -> Self {
        Self {
            fire: self.fire * t,
            frost: self.frost * t,
            storm: self.storm * t,
            arcane: self.arcane * t,
            nature: self.nature * t,
            necro: self.necro * t,
            chrono: self.chrono * t,
            radiant: self.radiant * t,
            physical: self.physical * t,
        }
    }
}

/// Flat, additive stat modifiers an item contributes while equipped. Every field is
/// `f32` and defaults to zero, so an item only states what it changes. The sim sums
/// every equipped item's effective mods (base + upgrade + affixes + gems + enchant +
/// set bonus) via [`StatMods::combine`] into the character's effective stats per tick.
///
/// The contract for the designer: **none of these are flavour.** Each maps to a
/// concrete seam the sim reads (see `arena_sim::rpg::derive` and the combat/movement
/// hooks). If you put a number here, a player will feel it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct StatMods {
    // --- core attributes ---
    pub power: f32,
    pub focus: f32,
    pub agility: f32,
    pub vitality: f32,

    // --- pools & regen ---
    pub max_mana: f32,
    pub max_health: f32,
    pub max_stamina: f32,
    pub mana_regen: f32,
    pub health_regen: f32,

    // --- casting / spell shaping (every one is felt) ---
    /// Multiplicative spell-power bonus as a fraction (0.15 = +15%). Summed, applied once.
    pub spell_power_pct: f32,
    /// Fraction (0..1) shaved off ability cooldowns.
    pub cooldown_reduction: f32,
    /// Cast-speed bonus fraction (cuts cast_time).
    pub cast_speed_pct: f32,
    /// Spell/projectile range bonus fraction.
    pub range_pct: f32,
    /// AoE radius bonus fraction (bigger novas, wider beams).
    pub aoe_radius_pct: f32,
    /// Projectile travel-speed bonus fraction.
    pub projectile_speed_pct: f32,
    /// Extra projectiles added to multi-projectile spells (rounded; a literal "+2 bolts").
    pub extra_projectiles: f32,
    /// Extra pierce: how many additional targets a projectile/beam punches through.
    pub pierce: f32,
    /// Fraction of a hit's damage also dealt to nearby foes (cleave / splash).
    pub area_damage: f32,

    // --- crit & sustain ---
    /// Added critical-strike chance (0..1).
    pub crit_chance: f32,
    /// Added critical *bonus* multiplier (0.5 = crits hit +50% harder than baseline).
    pub crit_damage: f32,
    /// Fraction of damage dealt returned as health.
    pub lifesteal: f32,
    /// Fraction of spell damage dealt returned as mana.
    pub mana_leech: f32,
    /// Flat health restored on a kill.
    pub health_on_kill: f32,
    /// Flat mana restored on a kill.
    pub mana_on_kill: f32,

    // --- defence ---
    /// Flat damage subtracted from every incoming hit (after resist), floored at a chip.
    pub armor: f32,
    /// Multiplicative incoming-damage reduction fraction (capped by the sim).
    pub armor_pct: f32,
    /// Chance (0..1) to block, halving a hit (off-hands / shields).
    pub block_chance: f32,
    /// Fraction of incoming damage reflected to the attacker.
    pub thorns: f32,
    /// Crowd-control duration reduction fraction (stuns/slows/roots last less).
    pub tenacity: f32,
    /// Fall-damage reduction fraction.
    pub fall_damage_pct: f32,

    // --- melee & movement (so swords and boots matter) ---
    /// Melee damage bonus fraction (drives `World::melee_attack`).
    pub melee_power_pct: f32,
    /// Added melee reach in metres (a longer sword arc).
    pub melee_range: f32,
    /// Attack/swing/melee-speed bonus fraction (faster combo cadence).
    pub attack_speed_pct: f32,
    /// Knockback bonus fraction on melee and impulse.
    pub knockback_pct: f32,
    /// Flat addition to base movement speed (m/s).
    pub move_speed: f32,
    /// Extra mid-air jumps granted (boots).
    pub jump_count: f32,
    /// Extra burst-movement (dash/blink) charges.
    pub dash_charges: f32,

    // --- summons ---
    /// Summon damage/health bonus fraction.
    pub summon_power_pct: f32,
    /// Extra summons added to summon spells.
    pub summon_count: f32,

    // --- economy / discovery ---
    /// Loot rarity+quantity bonus fraction (shifts drop rolls up the rarity ladder).
    pub magic_find: f32,
    /// Essence/currency find bonus fraction.
    pub gold_find: f32,
    /// XP gain bonus fraction.
    pub xp_gain_pct: f32,

    // --- per-element ---
    /// Outgoing damage bonus per element.
    pub elem_damage: ElementMods,
    /// Incoming damage reduction per element.
    pub elem_resist: ElementMods,
}

/// Fold every field of two [`StatMods`] additively. A local macro keeps the ~40-field
/// sum honest (forgetting a field would silently make gear do nothing).
macro_rules! sum_fields {
    ($a:expr, $b:expr, $($f:ident),+ $(,)?) => {
        StatMods { $($f: $a.$f + $b.$f,)+
            elem_damage: $a.elem_damage.combine(&$b.elem_damage),
            elem_resist: $a.elem_resist.combine(&$b.elem_resist),
        }
    };
}

impl StatMods {
    /// Sum this with another set of mods field-by-field. Used to fold all equipped
    /// items, their affixes/gems/enchants, set bonuses, and tech `StatMult` effects
    /// into one effective modifier.
    pub fn combine(&self, other: &Self) -> Self {
        sum_fields!(
            self, other, power, focus, agility, vitality, max_mana, max_health, max_stamina,
            mana_regen, health_regen, spell_power_pct, cooldown_reduction, cast_speed_pct,
            range_pct, aoe_radius_pct, projectile_speed_pct, extra_projectiles, pierce,
            area_damage, crit_chance, crit_damage, lifesteal, mana_leech, health_on_kill,
            mana_on_kill, armor, armor_pct, block_chance, thorns, tenacity, fall_damage_pct,
            melee_power_pct, melee_range, attack_speed_pct, knockback_pct, move_speed,
            jump_count, dash_charges, summon_power_pct, summon_count, magic_find, gold_find,
            xp_gain_pct,
        )
    }

    /// Scale every numeric field by `t` (used to interpolate an affix roll between its
    /// min and max, and to grow base mods by an upgrade level).
    pub fn scaled(&self, t: f32) -> Self {
        let mut s = *self;
        macro_rules! scale { ($($f:ident),+ $(,)?) => { $( s.$f *= t; )+ } }
        scale!(
            power, focus, agility, vitality, max_mana, max_health, max_stamina, mana_regen,
            health_regen, spell_power_pct, cooldown_reduction, cast_speed_pct, range_pct,
            aoe_radius_pct, projectile_speed_pct, extra_projectiles, pierce, area_damage,
            crit_chance, crit_damage, lifesteal, mana_leech, health_on_kill, mana_on_kill,
            armor, armor_pct, block_chance, thorns, tenacity, fall_damage_pct, melee_power_pct,
            melee_range, attack_speed_pct, knockback_pct, move_speed, jump_count, dash_charges,
            summon_power_pct, summon_count, magic_find, gold_find, xp_gain_pct,
        );
        s.elem_damage = self.elem_damage.scaled(t);
        s.elem_resist = self.elem_resist.scaled(t);
        s
    }

    /// Linear blend between `lo` (t=0) and `hi` (t=1). The affix roller uses this to
    /// turn a min/max range plus a rolled `t in 0..1` into a concrete value.
    pub fn lerp(lo: &Self, hi: &Self, t: f32) -> Self {
        lo.combine(&hi.combine(&lo.scaled(-1.0)).scaled(t))
    }
}

// ------------------------------------------------------------------------------------
// Procs: the "noticeable effect" engine. An item (or affix, gem, set, runeword) can
// carry triggers that fire a spell-VM effect on a sim event. The sim's `item_procs`
// evaluator owns dispatch + per-trigger internal cooldowns.
// ------------------------------------------------------------------------------------

/// When a proc fires. Each maps to a concrete event the sim already raises.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcWhen {
    /// On any damaging hit you land (melee or spell).
    OnHit,
    /// On a critical hit you land.
    OnCrit,
    /// On a melee swing connecting.
    OnMelee,
    /// On a kill you secure.
    OnKill,
    /// Whenever you finish casting a spell.
    OnCast,
    /// When you take damage.
    OnTakeDamage,
    /// When a hit would drop you below `frac` health (panic buttons; respects ICD).
    OnLowHealth { frac: f32 },
    /// When you successfully block.
    OnBlock,
    /// When you dash / blink / use a burst-movement ability.
    OnDash,
    /// Continuously while equipped (an aura). `effect` is applied/refreshed each tick.
    Aura,
    /// On a fixed interval in seconds (heartbeat effects: a pulsing nova, a regen tick).
    Interval { secs: f32 },
}

/// What a proc does. Most route into the spell VM (reuse all of `EffectOp`); a few are
/// direct so common cases need no companion `SpellDef`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProcEffect {
    /// Cast a full spell from the pack at the proc's natural origin/target (impact for
    /// OnHit, self for Aura, etc.). The richest option — anything the VM can express.
    CastSpell { spell: SpellId },
    /// Apply a status to the wearer.
    BuffSelf { status: StatusId, duration_s: f32, stacks: u8 },
    /// Apply a status to the thing that was hit / the attacker.
    DebuffTarget { status: StatusId, duration_s: f32, stacks: u8 },
    /// A burst of elemental damage in a radius around the event point.
    Nova { element: ElementId, radius: f32, damage: f32 },
    /// A bolt that arcs to up to `jumps` nearby foes for `damage` each.
    ChainBolt { element: ElementId, jumps: u8, damage: f32 },
    /// Heal the wearer a flat amount.
    Heal { amount: f32 },
    /// Grant the wearer a temporary shield.
    Shield { amount: f32, duration_s: f32 },
    /// Grant a temporary flat [`StatMods`] buff to the wearer (a "Berserk" surge).
    TempStats { mods: Box<StatMods>, duration_s: f32 },
    /// Summon a creature to fight for the wearer.
    Summon { mob: MobId, count: u8, duration_s: f32 },
}

/// A complete proc: a chance-gated, internal-cooldown-gated [`ProcEffect`] on a
/// [`ProcWhen`] event. This is the unit of "every item does something."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ItemTrigger {
    /// Short label for the tooltip ("Chain Lightning on crit").
    pub label: String,
    pub when: ProcWhen,
    /// Probability (0..1) the proc fires when its event occurs. 1.0 = always.
    pub chance: f32,
    /// Internal cooldown in seconds: the minimum gap between fires (anti-spam).
    pub icd_s: f32,
    pub effect: ProcEffect,
}

// ------------------------------------------------------------------------------------
// Upgrading.
// ------------------------------------------------------------------------------------

/// How an item base grows when forged (+1, +2, ... up to `max_level`). A rolled
/// instance stores only its current `upgrade_level`; the effective mods are
/// `base.scaled(1 + level * stat_growth)` plus the per-level flat/proc additions.
/// The sim's `forge` module spends [`UpgradeProfile::essence`] to raise the level and
/// rolls failure above `safe_until`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpgradeProfile {
    /// Hard cap on the upgrade level.
    pub max_level: u8,
    /// Per-level multiplicative growth of the item's [`StatMods`] (0.08 = +8%/level).
    pub stat_growth: f32,
    /// Per-level growth of the magnitude of the item's procs (bigger novas, etc.).
    pub proc_growth: f32,
    /// The essence/reagent item consumed per upgrade attempt.
    pub essence: ItemId,
    /// Base essence count for a +1; the cost grows with level in the forge.
    pub essence_cost_base: u16,
    /// Levels at and below which an upgrade can never fail.
    pub safe_until: u8,
    /// Per-level failure chance above `safe_until` (a failure consumes essence and,
    /// past a threshold, can shave one level — tuned in the forge config).
    pub fail_chance_per_level: f32,
}

impl Default for UpgradeProfile {
    fn default() -> Self {
        Self {
            max_level: 12,
            stat_growth: 0.08,
            proc_growth: 0.05,
            essence: ItemId::new("item.essence.arcane"),
            essence_cost_base: 1,
            safe_until: 4,
            fail_chance_per_level: 0.06,
        }
    }
}

/// A crafting recipe: a bag of input items (id + quantity) and an optional tech-tree
/// gate. The sim's crafting system consumes the inputs and produces the owning item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftRecipe {
    /// Required reagents as `(item, quantity)` pairs.
    pub inputs: Vec<(ItemId, u16)>,
    /// A tech node that must be unlocked before this recipe is craftable.
    pub tech_req: Option<TechNodeId>,
}

/// A complete item definition: the *base* that drops roll from and that the forge
/// upgrades. The flagship way players gain power — equip it for its [`StatMods`], its
/// procs, the spells / movement modes / abilities it grants, or use a consumable to
/// fire its `on_use` spell.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDef {
    pub id: ItemId,
    pub name: String,
    /// Tooltip flavour text; cosmetic.
    pub description: String,
    pub rarity: Rarity,
    pub slot: EquipSlot,

    /// Base stats granted while equipped (before upgrade/affixes/gems/set).
    #[serde(default)]
    pub stat_mods: StatMods,
    /// Innate procs every instance of this base carries (a Legendary's signature
    /// mechanic lives here). Distinct from affix procs, which are rolled per instance.
    #[serde(default)]
    pub triggers: Vec<ItemTrigger>,

    /// Spells this item adds to the wielder's spellbook while equipped.
    #[serde(default)]
    pub grants_spells: Vec<SpellId>,
    /// Movement / parkour modes unlocked while equipped.
    #[serde(default)]
    pub grants_movement: Vec<MovementModeId>,
    /// Equipped abilities (bound actions) this item provides.
    #[serde(default)]
    pub grants_abilities: Vec<AbilityId>,

    /// Procedural material used to texture the item mesh.
    #[serde(default)]
    pub material: Option<MaterialId>,
    /// Whether multiple copies stack in one inventory slot.
    #[serde(default)]
    pub stackable: bool,
    /// Maximum count per stack (1 if not stackable).
    #[serde(default = "one")]
    pub max_stack: u16,
    /// A spell fired when the item is consumed/activated (potions, scrolls).
    #[serde(default)]
    pub on_use: Option<SpellId>,
    /// Minimum character level required to equip / use.
    #[serde(default)]
    pub level_req: u32,
    /// Optional crafting recipe that produces this item.
    #[serde(default)]
    pub craft: Option<CraftRecipe>,

    // --- the new "build" layer ---
    /// Number of sockets present on a *base* drop of this item (rolled instances may
    /// add more up to the rarity's socket budget). Gems / runes slot here.
    #[serde(default)]
    pub base_sockets: u8,
    /// Which affix tags may roll on this item (e.g. `["caster","fire","universal"]`).
    /// The forge filters the affix pool by slot *and* these tags.
    #[serde(default)]
    pub affix_tags: Vec<String>,
    /// Set this item belongs to (set bonuses activate by equipped-piece count).
    #[serde(default)]
    pub set: Option<SetId>,
    /// Upgrade scaling. `None` means the item cannot be forged (consumables, reagents).
    #[serde(default)]
    pub upgrade: Option<UpgradeProfile>,
    /// True for hand-authored unique/legendary items: they never roll random affixes
    /// over their signature, only sockets and upgrade levels.
    #[serde(default)]
    pub unique: bool,
    /// If true the item soulbinds to the first wielder (can't be looted off your
    /// corpse). Legendaries/Mythics typically bind; everything else is free-floating.
    #[serde(default)]
    pub soulbound: bool,
}

fn one() -> u16 {
    1
}

impl Default for ItemDef {
    /// A blank Common reagent. Exists so the big content packs can use functional-update
    /// syntax (`ItemDef { id, name, ..Default::default() }`) and only state what differs
    /// — the new build-layer fields (sockets/affix_tags/set/upgrade/unique/soulbound)
    /// then default cleanly without touching every existing literal.
    fn default() -> Self {
        Self {
            id: ItemId::new(""),
            name: String::new(),
            description: String::new(),
            rarity: Rarity::Common,
            slot: EquipSlot::None,
            stat_mods: StatMods::default(),
            triggers: Vec::new(),
            grants_spells: Vec::new(),
            grants_movement: Vec::new(),
            grants_abilities: Vec::new(),
            material: None,
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 0,
            craft: None,
            base_sockets: 0,
            affix_tags: Vec::new(),
            set: None,
            upgrade: None,
            unique: false,
            soulbound: false,
        }
    }
}

impl ItemDef {
    /// A minimal base used by builders; fill in the fields you care about. Keeps the
    /// big content packs terse.
    pub fn base(id: &str, name: &str, slot: EquipSlot, rarity: Rarity) -> Self {
        Self {
            id: ItemId::new(id),
            name: name.to_string(),
            description: String::new(),
            rarity,
            slot,
            stat_mods: StatMods::default(),
            triggers: Vec::new(),
            grants_spells: Vec::new(),
            grants_movement: Vec::new(),
            grants_abilities: Vec::new(),
            material: None,
            stackable: false,
            max_stack: 1,
            on_use: None,
            level_req: 0,
            craft: None,
            base_sockets: 0,
            affix_tags: Vec::new(),
            set: None,
            upgrade: if matches!(slot, EquipSlot::Consumable | EquipSlot::None) {
                None
            } else {
                Some(UpgradeProfile::default())
            },
            unique: false,
            soulbound: false,
        }
    }

    /// Builder: set the base stats.
    pub fn with_stats(mut self, mods: StatMods) -> Self {
        self.stat_mods = mods;
        self
    }
    /// Builder: add a signature proc.
    pub fn with_trigger(mut self, t: ItemTrigger) -> Self {
        self.triggers.push(t);
        self
    }
    /// Builder: tag the item for affix rolling.
    pub fn with_tags(mut self, tags: &[&str]) -> Self {
        self.affix_tags = tags.iter().map(|s| s.to_string()).collect();
        self
    }
    /// Builder: set socket count and description, mark unique/bound.
    pub fn with_sockets(mut self, n: u8) -> Self {
        self.base_sockets = n;
        self
    }
    pub fn described(mut self, d: &str) -> Self {
        self.description = d.to_string();
        self
    }
    pub fn in_set(mut self, set: &str) -> Self {
        self.set = Some(SetId::new(set));
        self
    }
    pub fn unique_bound(mut self) -> Self {
        self.unique = true;
        self.soulbound = true;
        self
    }
    pub fn require_level(mut self, lvl: u32) -> Self {
        self.level_req = lvl;
        self
    }
}
