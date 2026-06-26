//! Familiars — a bonded companion creature that is *yours*, and grows with you.
//!
//! Not a summon that expires; a familiar is a persistent soul-bonded creature with a
//! name, a personality rolled at hatching, a deepening bond, and tricks it learns by
//! your side. It levels by fighting and exploring with you, shifts mood with the day
//! and the deeds it witnesses, and at bond milestones it **evolves** — a flickering
//! wisp becomes a blazing star-sprite. It even speaks, in a voice shaped by its nature.
//!
//! Deterministic at birth (its personality and name hash from its bond tick + owner), so
//! the same egg always hatches the same soul on every node.

use serde::{Deserialize, Serialize};

use crate::rng::Rng;

/// A familiar's innate temperament — rolled once at hatching, immutable, and the lens
/// for everything it says and does.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Personality {
    /// Bold ↔ timid: how readily it charges into danger.
    pub boldness: f32,
    /// Loyal ↔ aloof: how fast its bond deepens.
    pub loyalty: f32,
    /// Curious ↔ content: how much it wanders and finds things.
    pub curiosity: f32,
    /// Fiery ↔ serene: how sharply its mood swings.
    pub temper: f32,
}

impl Personality {
    fn roll(rng: &mut Rng) -> Self {
        Self {
            boldness: rng.unit(),
            loyalty: rng.unit(),
            curiosity: rng.unit(),
            temper: rng.unit(),
        }
    }

    /// A one-word nature for the UI, from the dominant trait.
    pub fn nature(&self) -> &'static str {
        let traits = [
            ("Valiant", self.boldness),
            ("Devoted", self.loyalty),
            ("Inquisitive", self.curiosity),
            ("Fiery", self.temper),
        ];
        traits
            .into_iter()
            .fold(("Gentle", 0.45), |best, t| if t.1 > best.1 { t } else { best })
            .0
    }
}

/// A familiar's current mood — shifts with events, coloured by temper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mood {
    Content,
    Excited,
    Frightened,
    Affectionate,
    Sulking,
    Fierce,
}

/// The bonded companion itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Familiar {
    pub id: String,
    pub name: String,
    pub owner: String,
    /// What it currently is (evolves). A content mob id for rendering.
    pub form: String,
    /// Its magical element, inherited and reinforced by your casting.
    pub element: String,
    pub personality: Personality,
    pub mood: Mood,
    /// Bond strength 0..1; milestones trigger evolutions and unlock tricks.
    pub bond: f32,
    pub level: u32,
    pub xp: f32,
    /// Tricks/abilities it has learned by your side (content ability ids).
    pub tricks: Vec<String>,
    /// How many times it has evolved.
    pub evolutions: u32,
}

impl Familiar {
    /// Hatch a familiar bonded to `owner` at `tick`. Its soul is deterministic in those.
    pub fn hatch(owner: &str, base_form: &str, element: &str, tick: u64) -> Self {
        let mut rng = Rng::from_tick(tick, owner);
        let personality = Personality::roll(&mut rng);
        let name = weave_pet_name(&mut rng);
        Self {
            id: format!("familiar.{owner}.{tick}"),
            name,
            owner: owner.to_string(),
            form: base_form.to_string(),
            element: element.to_string(),
            personality,
            mood: Mood::Content,
            bond: 0.0,
            level: 1,
            xp: 0.0,
            tricks: Vec::new(),
            evolutions: 0,
        }
    }

    /// Award shared experience (fighting/exploring together). Levels up and, at bond
    /// milestones, evolves. Returns evolution events for the world to announce.
    pub fn gain_xp(&mut self, amount: f32) -> Option<Evolution> {
        self.xp += amount;
        let need = self.level as f32 * 50.0;
        if self.xp >= need {
            self.xp -= need;
            self.level += 1;
            // Loyalty makes bonding faster; capped at 1.
            self.bond = (self.bond + 0.03 * (0.5 + self.personality.loyalty)).min(1.0);
            return self.maybe_evolve();
        }
        None
    }

    /// Strengthen the bond directly (feeding, petting, a gift). May evolve.
    pub fn nurture(&mut self, amount: f32) -> Option<Evolution> {
        self.bond = (self.bond + amount * (0.5 + self.personality.loyalty)).min(1.0);
        self.mood = Mood::Affectionate;
        self.maybe_evolve()
    }

    /// Evolve at the bond milestones 0.33 / 0.66 / 1.0.
    fn maybe_evolve(&mut self) -> Option<Evolution> {
        let milestones = [0.33, 0.66, 1.0];
        let reached = milestones.iter().filter(|m| self.bond >= **m).count() as u32;
        if reached > self.evolutions {
            self.evolutions = reached;
            let new_form = format!("{}.ascended{}", self.form_base(), self.evolutions);
            let old = self.form.clone();
            self.form = new_form.clone();
            // Each evolution teaches a trick echoing its element.
            let trick = format!("ability.familiar.{}_{}", self.element, self.evolutions);
            self.tricks.push(trick.clone());
            return Some(Evolution { from: old, to: new_form, trick, stage: self.evolutions });
        }
        None
    }

    fn form_base(&self) -> String {
        // Strip any prior ".ascendedN" suffix so we keep one base lineage.
        self.form.split(".ascended").next().unwrap_or(&self.form).to_string()
    }

    /// React to a world event, shifting mood through the lens of temperament. The
    /// returned line is what the familiar "says" (or chirps) — flavour for the HUD.
    pub fn react(&mut self, event: FamiliarStimulus) -> String {
        let swingy = self.personality.temper > 0.5;
        self.mood = match event {
            FamiliarStimulus::OwnerHurt if self.personality.boldness > 0.5 => Mood::Fierce,
            FamiliarStimulus::OwnerHurt => Mood::Frightened,
            FamiliarStimulus::Victory => if swingy { Mood::Excited } else { Mood::Content },
            FamiliarStimulus::Discovery if self.personality.curiosity > 0.5 => Mood::Excited,
            FamiliarStimulus::Discovery => Mood::Content,
            FamiliarStimulus::Neglected => if swingy { Mood::Sulking } else { Mood::Content },
            FamiliarStimulus::Fed => Mood::Affectionate,
        };
        self.speak()
    }

    /// A line in the familiar's voice, from its nature + mood. Pure flavour, but the
    /// kind of flavour that makes a companion feel alive.
    pub fn speak(&self) -> String {
        let n = &self.name;
        match self.mood {
            Mood::Content => format!("{n} hums softly at your side."),
            Mood::Excited => format!("{n} darts in circles, crackling with {} sparks!", self.element),
            Mood::Frightened => format!("{n} presses close, trembling."),
            Mood::Affectionate => format!("{n} nuzzles into your robes, warm and glad."),
            Mood::Sulking => format!("{n} turns away, ignoring you. It remembers being forgotten."),
            Mood::Fierce => format!("{n} bares itself between you and the foe, eyes alight."),
        }
    }
}

/// A familiar evolution event, for the world to announce and the renderer to re-form.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evolution {
    pub from: String,
    pub to: String,
    pub trick: String,
    pub stage: u32,
}

/// Things that happen to a familiar's owner that the familiar reacts to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FamiliarStimulus {
    OwnerHurt,
    Victory,
    Discovery,
    Neglected,
    Fed,
}

const PET_SYLL: &[&str] = &["Pip", "Zee", "Mur", "Fen", "Lux", "Sol", "Nib", "Quill", "Ash", "Vey", "Glim", "Tup"];

fn weave_pet_name(rng: &mut Rng) -> String {
    let a = rng.pick(PET_SYLL).copied().unwrap_or("Pip");
    if rng.chance(0.5) {
        let b = rng.pick(PET_SYLL).copied().unwrap_or("kin").to_lowercase();
        format!("{a}{b}")
    } else {
        a.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hatching_is_deterministic() {
        let a = Familiar::hatch("leif", "mob.wisp", "arcane", 42);
        let b = Familiar::hatch("leif", "mob.wisp", "arcane", 42);
        assert_eq!(a.name, b.name);
        assert_eq!(a.personality.boldness, b.personality.boldness);
    }

    #[test]
    fn nurturing_evolves_at_milestones() {
        let mut f = Familiar::hatch("leif", "mob.wisp", "fire", 1);
        let mut evolutions = 0;
        for _ in 0..50 {
            if f.nurture(0.05).is_some() {
                evolutions += 1;
            }
        }
        assert!(evolutions >= 1, "a well-loved familiar should evolve");
        assert!(!f.tricks.is_empty(), "evolution teaches tricks");
        assert!(f.form.contains("ascended"));
    }

    #[test]
    fn it_speaks_in_character() {
        let mut f = Familiar::hatch("leif", "mob.wisp", "storm", 7);
        let line = f.react(FamiliarStimulus::Victory);
        assert!(!line.is_empty());
        assert!(line.contains(&f.name));
    }
}
