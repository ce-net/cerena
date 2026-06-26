//! The Chronicle — the living myth engine, and the soul of this whole crate.
//!
//! Most games have a kill feed. Cerena has a *mythology*. The Chronicle watches every
//! deed worth remembering — a slaying, a last stand, a discovery, the forging of a
//! legendary relic, a duel between rivals — and it keeps a ledger of **renown** for
//! everyone and everything. When a life (a player's, or a monster's) accrues enough
//! weight, or does something dramatic enough, the Chronicle **mints a Legend**: it
//! gives the deed a mythic name and an epithet, writes a line of saga, and decides how
//! the legend *imprints on the world* — a fallen champion is hung in the stars as a
//! constellation; the site of a terrible duel goes hallowed or haunted; a slain
//! arch-foe seeds a relic where it died; a monster that has killed enough rises again
//! with a true-name and greater power.
//!
//! Because renown is earned by *deeds the sim already produces*, the world's legends are
//! genuinely emergent and player-authored. And because it is all deterministic (names
//! and rolls hashed from the tick), every node weaves the same myth.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use glam::Vec3;

use crate::rng::Rng;

/// The facets of renown. A life is remembered differently for slaughter than for
/// shielding the weak; the dominant facet shapes an actor's title and their legend.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Renown {
    /// Fame won in open battle and slaying mighty foes.
    pub valor: f32,
    /// Fame won by guile — ambushes, escapes, outwitting.
    pub cunning: f32,
    /// Fame won by protecting others, healing, holding the line.
    pub grace: f32,
    /// Infamy — cruelty, betrayal, slaughter of the helpless.
    pub dread: f32,
    /// Fame won by discovery, crafting, and lore.
    pub wisdom: f32,
}

impl Renown {
    /// Total weight of a life — the sum of all it is remembered for.
    pub fn total(&self) -> f32 {
        self.valor + self.cunning + self.grace + self.dread + self.wisdom
    }

    /// The facet this life is *most* known for, and its value.
    pub fn dominant(&self) -> (Facet, f32) {
        let pairs = [
            (Facet::Valor, self.valor),
            (Facet::Cunning, self.cunning),
            (Facet::Grace, self.grace),
            (Facet::Dread, self.dread),
            (Facet::Wisdom, self.wisdom),
        ];
        pairs
            .into_iter()
            .fold((Facet::Valor, f32::MIN), |best, p| if p.1 > best.1 { p } else { best })
    }

    fn add(&mut self, facet: Facet, amount: f32) {
        match facet {
            Facet::Valor => self.valor += amount,
            Facet::Cunning => self.cunning += amount,
            Facet::Grace => self.grace += amount,
            Facet::Dread => self.dread += amount,
            Facet::Wisdom => self.wisdom += amount,
        }
    }
}

/// One axis of renown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Facet {
    Valor,
    Cunning,
    Grace,
    Dread,
    Wisdom,
}

/// A deed fed into the Chronicle. The sim raises these from the events it already has
/// (kills, deaths, discoveries, crafts); the Chronicle decides what becomes myth.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Deed {
    /// The actor's id (a CE node id for a player; a mob instance id for a monster).
    pub actor: String,
    /// A display name for the actor at the time (used when minting a legend).
    pub actor_name: String,
    pub kind: DeedKind,
    pub location: Vec3,
    pub tick: u64,
    /// How weighty the deed is — a wisp is nothing, an arch-lich is everything.
    pub magnitude: f32,
}

/// The kinds of deed the Chronicle understands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeedKind {
    /// Slew `victim` (carry the victim's own renown so killing a legend is legendary).
    Slay { victim: String, victim_renown: f32 },
    /// Fell in battle to `slayer`.
    Fell { slayer: String },
    /// Survived against overwhelming odds (a last stand, a long delve).
    LastStand { foes: u32 },
    /// Discovered a place of meaning for the first time.
    Discover { place: String },
    /// Forged or first-wielded a legendary item.
    Forge { item: String },
    /// Won a duel against a named rival.
    Duel { rival: String },
    /// Protected others (a great heal, a shield-wall, a rescue).
    Ward { saved: u32 },
    /// A dark deed — betrayal, or the slaughter of the defenceless.
    Atrocity,
}

impl DeedKind {
    /// Which facet of renown this deed feeds, and a base weight multiplier.
    fn facet_weight(&self) -> (Facet, f32) {
        match self {
            DeedKind::Slay { .. } => (Facet::Valor, 1.0),
            DeedKind::Fell { .. } => (Facet::Valor, 0.2),
            DeedKind::LastStand { foes } => (Facet::Valor, 0.5 + *foes as f32 * 0.1),
            DeedKind::Discover { .. } => (Facet::Wisdom, 1.0),
            DeedKind::Forge { .. } => (Facet::Wisdom, 1.2),
            DeedKind::Duel { .. } => (Facet::Cunning, 1.0),
            DeedKind::Ward { saved } => (Facet::Grace, 0.5 + *saved as f32 * 0.2),
            DeedKind::Atrocity => (Facet::Dread, 1.5),
        }
    }
}

/// How a minted legend imprints itself on the living world. The Chronicle decides;
/// the `WorldSoul` routes each to the system that realises it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Manifestation {
    /// Hang the legend in the night sky as a named constellation granting a boon.
    Constellation { star_name: String, element: String },
    /// Hallow the site of the deed — a shrine of light, blessing those who visit.
    HallowedGround { place: Vec3 },
    /// Curse the site — it goes haunted, spawning the restless dead.
    HauntedGround { place: Vec3 },
    /// Seed a relic where a great foe fell, for someone bold to claim.
    RelicSite { place: Vec3, rarity_tier: u8 },
    /// A monster that has killed enough *returns*, now bearing a true-name and might.
    NamedFoeRises { base: String, power_mult: f32 },
}

/// A minted Legend — a permanent entry in the world's mythology.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Legend {
    /// Stable id (`"legend.000123"`).
    pub id: String,
    /// The mythic name + epithet, e.g. "Vaelith the Emberhearted, Bane of the Hollow".
    pub name: String,
    /// Who/what this legend is about.
    pub about: String,
    /// The facet it is remembered for.
    pub facet: Facet,
    /// A line of saga — the Chronicle's own telling of the deed.
    pub saga: String,
    pub born_tick: u64,
    pub location: Vec3,
    /// How the legend marks the world.
    pub manifestation: Manifestation,
}

/// The Chronicle: the renown ledger + the minted legends + the name-weaver.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Chronicle {
    /// Running renown for every actor the world has noticed.
    pub renown: HashMap<String, Renown>,
    /// Every legend ever minted, oldest first — the world's myth-history.
    pub legends: Vec<Legend>,
    /// Renown threshold above which the next great deed mints a legend. Rises as the
    /// age fills with legends, so myth stays scarce and precious.
    pub legend_threshold: f32,
    /// Counter for stable legend ids.
    next_id: u64,
}

impl Chronicle {
    pub fn new() -> Self {
        Self { renown: HashMap::new(), legends: Vec::new(), legend_threshold: 100.0, next_id: 0 }
    }

    /// Current renown of an actor (zero if unknown to the world).
    pub fn renown_of(&self, actor: &str) -> Renown {
        self.renown.get(actor).copied().unwrap_or_default()
    }

    /// The honorific an actor currently bears, from their dominant facet and weight.
    pub fn title_of(&self, actor: &str) -> Option<String> {
        let r = self.renown.get(actor)?;
        let total = r.total();
        if total < 10.0 {
            return None; // the unremembered have no title
        }
        let (facet, _) = r.dominant();
        Some(title_for(facet, total).to_string())
    }

    /// Record a deed. Updates renown and, if the moment is mythic enough, mints one or
    /// more Legends. Returns the legends born from this deed (for the world to realise
    /// and announce). The beating heart of the engine.
    pub fn record(&mut self, deed: &Deed) -> Vec<Legend> {
        let mut born = Vec::new();
        let (facet, base) = deed.kind.facet_weight();

        // Slaying carries a slice of the victim's own renown — felling a legend is how
        // you *become* one. This is the feedback loop that makes rivalries epic.
        let inherited = match &deed.kind {
            DeedKind::Slay { victim_renown, .. } => victim_renown * 0.25,
            _ => 0.0,
        };
        let gained = (deed.magnitude * base + inherited).max(0.0);

        let entry = self.renown.entry(deed.actor.clone()).or_default();
        entry.add(facet, gained);
        let total_after = entry.total();
        let crossed = total_after >= self.legend_threshold;

        // A legend is minted when a life crosses the threshold, OR a single deed is
        // inherently mythic (felling a legend, surviving an impossible stand, the
        // Grand-Conjunction forge), regardless of total.
        let inherently_mythic = matches!(
            &deed.kind,
            DeedKind::Slay { victim_renown, .. } if *victim_renown >= self.legend_threshold
        ) || matches!(&deed.kind, DeedKind::LastStand { foes } if *foes >= 20)
            || matches!(&deed.kind, DeedKind::Forge { .. });

        if crossed || inherently_mythic {
            let legend = self.mint(deed, facet);
            born.push(legend.clone());
            self.legends.push(legend);
            // Each minted legend raises the bar — the age must work harder for the next.
            self.legend_threshold *= 1.15;
        }

        born
    }

    /// Weave a Legend from a deed: a mythic name, a saga line, and a world-imprint.
    fn mint(&mut self, deed: &Deed, facet: Facet) -> Legend {
        let id = format!("legend.{:06}", self.next_id);
        self.next_id += 1;

        let mut rng = Rng::from_tick(deed.tick, &deed.actor);
        let truename = weave_truename(&mut rng);
        let epithet = epithet_for(facet, &deed.kind, &mut rng);
        // Players keep their chosen name and gain an epithet; nameless monsters are
        // given a fresh true-name when they ascend to legend.
        let display = if deed.actor_name.trim().is_empty() {
            format!("{truename} {epithet}")
        } else {
            format!("{} {epithet}", deed.actor_name)
        };

        let saga = compose_saga(&display, deed, &mut rng);
        let manifestation = choose_manifestation(deed, facet, &display, &mut rng);

        Legend {
            id,
            name: display,
            about: deed.actor.clone(),
            facet,
            saga,
            born_tick: deed.tick,
            location: deed.location,
            manifestation,
        }
    }

    /// The most recent `n` legends (for a "tales of the age" UI / world banner).
    pub fn recent(&self, n: usize) -> &[Legend] {
        let len = self.legends.len();
        &self.legends[len.saturating_sub(n)..]
    }
}

// ---------------------------------------------------------------------------
// The name-weaver and saga-teller — procedural mythic language.
// ---------------------------------------------------------------------------

const NAME_ONSET: &[&str] = &[
    "Vael", "Mor", "Sel", "Thar", "Ny", "Kael", "Bra", "Ysh", "Dro", "Eil", "Gor", "Wis", "Aza", "Tor", "Lir",
];
const NAME_CODA: &[&str] = &[
    "ith", "anor", "une", "ax", "iel", "oth", "ara", "wyn", "ek", "ondra", "is", "ael", "orn", "ya", "ux",
];

/// Weave a procedural true-name from mythic syllables.
fn weave_truename(rng: &mut Rng) -> String {
    let onset = rng.pick(NAME_ONSET).copied().unwrap_or("Vael");
    let coda = rng.pick(NAME_CODA).copied().unwrap_or("ith");
    // Occasionally a three-part name for the grandest legends.
    if rng.chance(0.3) {
        let mid = rng.pick(NAME_ONSET).copied().unwrap_or("mor").to_lowercase();
        format!("{onset}{mid}{coda}")
    } else {
        format!("{onset}{coda}")
    }
}

/// An epithet drawn from the deed and facet — "the Emberhearted", "Bane of the Hollow".
fn epithet_for(facet: Facet, kind: &DeedKind, rng: &mut Rng) -> String {
    // Deed-specific epithets take precedence; they're the most evocative.
    let specific: &[&str] = match kind {
        DeedKind::Slay { .. } => &["Bane of the Mighty", "the Kingslayer", "Hewer of Legends"],
        DeedKind::LastStand { .. } => &["the Unbroken", "who Held the Line", "the Last to Fall"],
        DeedKind::Discover { .. } => &["the Far-Walker", "Finder of Lost Ways", "the Pathless"],
        DeedKind::Forge { .. } => &["the Relic-Smith", "Hand of Making", "the Runewright"],
        DeedKind::Duel { .. } => &["the Duellist", "the Unbeaten", "Edge of the Dawn"],
        DeedKind::Ward { .. } => &["the Shield of Many", "the Warden", "who Stood Before Them"],
        DeedKind::Atrocity => &["the Cruel", "the Defiler", "Dread of the Mire"],
        DeedKind::Fell { .. } => &["the Fallen", "the Mourned", "Ash of the Field"],
    };
    let facet_pool: &[&str] = match facet {
        Facet::Valor => &["the Emberhearted", "the Lionhearted", "the Bold"],
        Facet::Cunning => &["the Shadow-Subtle", "the Sly", "the Veiled"],
        Facet::Grace => &["the Kind", "the Radiant", "the Gentle Hand"],
        Facet::Dread => &["the Terrible", "the Black-Hearted", "the Wraithkin"],
        Facet::Wisdom => &["the Deep-Read", "Keeper of Lore", "the Starlit-Minded"],
    };
    // 60% deed-specific, 40% facet flavour.
    if rng.chance(0.6) {
        rng.pick(specific).copied().unwrap_or("the Nameless").to_string()
    } else {
        rng.pick(facet_pool).copied().unwrap_or("the Nameless").to_string()
    }
}

/// Compose a one-line saga — the Chronicle's telling of the deed.
fn compose_saga(name: &str, deed: &Deed, rng: &mut Rng) -> String {
    let openings = [
        "In the telling it is said that",
        "When the moons were low,",
        "Let it be remembered:",
        "The Chronicle records that",
    ];
    let open = rng.pick(&openings).copied().unwrap_or("It is said that");
    let body = match &deed.kind {
        DeedKind::Slay { victim, .. } => format!("{name} struck down {victim}, and the land shook."),
        DeedKind::Fell { slayer } => format!("{name} fell at last to {slayer}, and was mourned."),
        DeedKind::LastStand { foes } => format!("{name} stood alone against {foes}, and did not break."),
        DeedKind::Discover { place } => format!("{name} was first to walk {place}, where none had gone."),
        DeedKind::Forge { item } => format!("{name} forged {item}, and its light has not dimmed since."),
        DeedKind::Duel { rival } => format!("{name} bested {rival} in single combat at dawn."),
        DeedKind::Ward { saved } => format!("{name} shielded {saved} souls from certain death."),
        DeedKind::Atrocity => format!("{name} did a dark thing here, and the ground remembers."),
    };
    format!("{open} {body}")
}

/// Decide how a legend marks the world. Grace/Valor tend skyward (constellations) or
/// hallow the ground; Dread haunts it; felling a great foe seeds a relic; a slain
/// *monster* of dread may rise again named.
fn choose_manifestation(deed: &Deed, facet: Facet, name: &str, rng: &mut Rng) -> Manifestation {
    match (&deed.kind, facet) {
        (DeedKind::Fell { .. }, _) | (_, Facet::Valor) | (_, Facet::Grace) | (_, Facet::Wisdom) => {
            // A bright legend is hung in the stars.
            let elements = ["fire", "frost", "storm", "radiant", "arcane", "nature", "void"];
            let element = rng.pick(&elements).copied().unwrap_or("arcane").to_string();
            Manifestation::Constellation { star_name: name.to_string(), element }
        }
        (DeedKind::Atrocity, _) | (_, Facet::Dread) => {
            if rng.chance(0.5) {
                Manifestation::HauntedGround { place: deed.location }
            } else {
                Manifestation::NamedFoeRises { base: deed.actor.clone(), power_mult: rng.range(1.3, 2.5) }
            }
        }
        (DeedKind::Slay { .. }, _) => {
            Manifestation::RelicSite { place: deed.location, rarity_tier: 4 + rng.below(2) as u8 }
        }
        _ => Manifestation::HallowedGround { place: deed.location },
    }
}

/// A title for a facet at a given total renown (tiers of fame).
fn title_for(facet: Facet, total: f32) -> &'static str {
    let tier = if total > 400.0 {
        3
    } else if total > 150.0 {
        2
    } else {
        1
    };
    match (facet, tier) {
        (Facet::Valor, 1) => "the Brave",
        (Facet::Valor, 2) => "Champion",
        (Facet::Valor, _) => "Warlord of the Age",
        (Facet::Cunning, 1) => "the Quick",
        (Facet::Cunning, 2) => "the Trickster",
        (Facet::Cunning, _) => "Master of the Veil",
        (Facet::Grace, 1) => "the Kindly",
        (Facet::Grace, 2) => "Warden",
        (Facet::Grace, _) => "Saint of the Weave",
        (Facet::Dread, 1) => "the Feared",
        (Facet::Dread, 2) => "the Terror",
        (Facet::Dread, _) => "Doom of the Age",
        (Facet::Wisdom, 1) => "the Learned",
        (Facet::Wisdom, 2) => "Loremaster",
        (Facet::Wisdom, _) => "Archmage of the Age",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deed(actor: &str, kind: DeedKind, mag: f32, tick: u64) -> Deed {
        Deed { actor: actor.into(), actor_name: actor.into(), kind, location: Vec3::ZERO, tick, magnitude: mag }
    }

    #[test]
    fn renown_accrues_and_grants_a_title() {
        let mut c = Chronicle::new();
        for t in 0..5 {
            c.record(&deed("hero", DeedKind::Slay { victim: "mob.wisp".into(), victim_renown: 0.0 }, 5.0, t));
        }
        let r = c.renown_of("hero");
        assert!(r.valor > 0.0);
        // Enough valor to earn a title.
        for t in 5..20 {
            c.record(&deed("hero", DeedKind::Slay { victim: "mob.golem".into(), victim_renown: 0.0 }, 5.0, t));
        }
        assert!(c.title_of("hero").is_some(), "a renowned hero should bear a title");
    }

    #[test]
    fn felling_a_legend_is_inherently_mythic() {
        let mut c = Chronicle::new();
        // A fresh actor slays something with huge renown -> instant legend.
        let born = c.record(&deed(
            "upstart",
            DeedKind::Slay { victim: "the Bone Colossus".into(), victim_renown: 500.0 },
            50.0,
            1234,
        ));
        assert_eq!(born.len(), 1, "slaying a legend mints a legend");
        assert!(born[0].name.contains("upstart"));
        assert!(!born[0].saga.is_empty());
    }

    #[test]
    fn legends_are_deterministic() {
        let mut a = Chronicle::new();
        let mut b = Chronicle::new();
        let d = deed("x", DeedKind::Forge { item: "item.void_relic".into() }, 10.0, 999);
        let la = a.record(&d);
        let lb = b.record(&d);
        assert_eq!(la[0].name, lb[0].name, "the same forge mints the same legend everywhere");
        assert_eq!(la[0].manifestation, lb[0].manifestation);
    }

    #[test]
    fn atrocity_breeds_dread_and_dark_manifestations() {
        let mut c = Chronicle::new();
        let mut saw_dark = false;
        for t in 0..40 {
            for l in c.record(&deed("villain", DeedKind::Atrocity, 8.0, t)) {
                if matches!(l.manifestation, Manifestation::HauntedGround { .. } | Manifestation::NamedFoeRises { .. }) {
                    saw_dark = true;
                }
            }
        }
        assert!(c.renown_of("villain").dread > 0.0);
        assert!(saw_dark, "atrocities should curse the world");
    }
}
