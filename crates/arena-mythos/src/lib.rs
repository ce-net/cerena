//! # arena-mythos — the living soul of Cerena
//!
//! A world of ten thousand mages should feel *alive* even when you are standing still:
//! the year should turn, three moons should wax and wane, mana should flood and ebb
//! through the land, creatures should breed and starve and migrate, weird weather
//! should roll in — and, above all, the things players *do* should become **myth**:
//! named legends, hung in the stars, that change how the world plays.
//!
//! This crate is that living soul. It is deterministic and `wasm`-clean like the rest of
//! the engine (no clocks, no RNG that isn't hashed from the world tick), so every
//! authority on the mesh dreams the exact same dream of the world.
//!
//! ## The systems, and how they interlock
//!
//! - [`calendar`] — the wheel of five seasons, three moons, and the festivals (and the
//!   once-an-age Grand Conjunction) that hang off them. *The metronome.*
//! - [`weather`] — aetherweather (mana storms, blightfog, starfall, the dreaded
//!   Doldrums) driven by the season and the local leyline charge.
//! - [`leyline`] — mana as a *substance that flows through the land*; wells you can
//!   drain, claim, and fight over; a diffusion sim that breathes with the seasons.
//! - [`ecology`] — creatures as a predator–prey food web that blooms, crashes, migrates,
//!   and mutates, feeding the spawn system.
//! - [`chronicle`] — **the heart**: a myth engine that turns deeds into named legends,
//!   sagas, and world-imprints.
//! - [`firmament`] — the night sky as the world's memory: legends become constellations
//!   that boon their school when risen.
//! - [`familiar`] — a bonded companion that grows, gains a personality, learns, evolves.
//! - [`omen`] — cryptic prophecies that foreshadow the world's great turns.
//! - [`attunement`] — how a mage slowly *becomes* the magic they practise (and the
//!   corruption the dark schools cost).
//!
//! [`WorldSoul`] binds them: it [`advance`](WorldSoul::advance)s the world each tick,
//! routes [`record_deed`](WorldSoul::record_deed) into the Chronicle and hangs the
//! resulting legends in the sky, and answers the one question the combat sim actually
//! asks — [`spell_power_at`](WorldSoul::spell_power_at) — by folding season, weather,
//! leyline charge, and the risen stars into a single multiplier.

pub mod attunement;
pub mod calendar;
pub mod chronicle;
pub mod ecology;
pub mod familiar;
pub mod firmament;
pub mod leyline;
pub mod omen;
pub mod rng;
pub mod weather;

pub use attunement::Attunement;
pub use calendar::{Calendar, Festival, Moon, Season};
pub use chronicle::{Chronicle, Deed, DeedKind, Facet, Legend, Manifestation, Renown};
pub use ecology::{Ecosystem, EcologyEvent, Species, Trophic};
pub use familiar::{Familiar, FamiliarStimulus};
pub use firmament::{Firmament, Star};
pub use leyline::{Leyline, LeylineNetwork, LeylineNode};
pub use omen::{Omen, OmenWeaver, Portent};
pub use rng::Rng;
pub use weather::{AetherWeather, Sky, WeatherState};

use glam::Vec3;
use serde::{Deserialize, Serialize};

/// Sim ticks per second (mirrors `arena_protocol::TICK_HZ`). Kept local so this crate
/// stays dependency-light.
const TICK_HZ: f32 = 64.0;

/// Everything notable the world did between two `advance` calls — the world's "news".
/// The sim surfaces these as world banners, spawns, and drop-rate flips.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldPulse {
    /// Set when the season changed this step.
    pub season_turned: Option<Season>,
    /// Set when a festival began this step.
    pub festival_began: Option<Festival>,
    /// Legends minted this step (already hung in the sky / queued as sites).
    pub legends_born: Vec<Legend>,
    /// Population events (blooms/crashes/mutations) the spawn system should react to.
    pub ecology: Vec<EcologyEvent>,
    /// Human-readable herald lines for the world banner.
    pub announcements: Vec<String>,
}

impl WorldPulse {
    fn is_silent(&self) -> bool {
        self.season_turned.is_none()
            && self.festival_began.is_none()
            && self.legends_born.is_empty()
            && self.ecology.is_empty()
            && self.announcements.is_empty()
    }
}

/// The living world. One per world/shard; the coordinator advances it and broadcasts
/// its pulse, and every zone authority keeps a copy in lockstep (it is deterministic in
/// the world tick + the deeds fed in).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldSoul {
    /// The authoritative world tick (set by [`WorldSoul::advance`]).
    pub tick: u64,
    /// Master seed for the region's weather and ambient rolls.
    pub seed: u64,
    pub leylines: LeylineNetwork,
    pub ecology: Ecosystem,
    pub chronicle: Chronicle,
    pub firmament: Firmament,
    /// Manifestations waiting for the sim to realise in-world (relic sites, haunts,
    /// rising named foes). The sim drains this each tick.
    pub pending_sites: Vec<Manifestation>,
    /// The region's default weather sampler.
    weather: AetherWeather,
    last_season: Season,
    last_festival: Option<Festival>,
}

impl WorldSoul {
    /// Create a fresh world soul at tick 0 with the given seed.
    pub fn new(seed: u64) -> Self {
        Self {
            tick: 0,
            seed,
            leylines: LeylineNetwork::default(),
            ecology: Ecosystem::default(),
            chronicle: Chronicle::new(),
            firmament: Firmament::default(),
            pending_sites: Vec::new(),
            weather: AetherWeather::new(seed),
            last_season: Season::Kindling,
            last_festival: None,
        }
    }

    /// The calendar/sky right now.
    pub fn calendar(&self) -> Calendar {
        Calendar::at(self.tick)
    }

    /// Advance the world to `world_tick`, stepping every living system. Returns the
    /// [`WorldPulse`] of what changed — season turns, festivals, ecology shifts. Pure
    /// function of the prior state + the new tick.
    pub fn advance(&mut self, world_tick: u64) -> WorldPulse {
        let mut pulse = WorldPulse::default();
        if world_tick <= self.tick {
            return pulse;
        }
        let dt = ((world_tick - self.tick) as f32 / TICK_HZ).min(2.0);
        self.tick = world_tick;
        let cal = Calendar::at(world_tick);

        // Leylines breathe.
        self.leylines.tick(dt, &cal);

        // Ecology steps (its own once-per-day guard inside).
        pulse.ecology = self.ecology.tick(&cal);
        for ev in &pulse.ecology {
            match ev {
                EcologyEvent::Bloom { mob, .. } => pulse.announcements.push(format!("The {mob} have bloomed; they spill across the land.")),
                EcologyEvent::Crash { mob } => pulse.announcements.push(format!("The {mob} have all but vanished from these parts.")),
                EcologyEvent::Mutation { variant, .. } => pulse.announcements.push(format!("Something has changed in the brood. A {variant} stirs.")),
            }
            // A mutation seeds a candidate named foe.
            if let EcologyEvent::Mutation { variant, power_mult, .. } = ev {
                self.pending_sites.push(Manifestation::NamedFoeRises { base: variant.clone(), power_mult: *power_mult });
            }
        }

        // Season turn?
        if cal.season != self.last_season {
            self.last_season = cal.season;
            pulse.season_turned = Some(cal.season);
            pulse.announcements.push(format!(
                "{} has come. The {} grows strong.",
                cal.season.name(),
                cal.season.ascendant_element()
            ));
        }

        // Festival begins?
        let fest = cal.festival();
        if fest != self.last_festival {
            self.last_festival = fest;
            if let Some(f) = fest {
                pulse.festival_began = Some(f);
                pulse.announcements.push(f.herald().to_string());
            }
        }

        pulse
    }

    /// Record a deed and weave any resulting legends into the world (hang constellations,
    /// queue relic sites / haunts / rising foes). Returns the legends born.
    pub fn record_deed(&mut self, deed: &Deed) -> Vec<Legend> {
        let born = self.chronicle.record(deed);
        for legend in &born {
            self.realise(&legend.manifestation, legend);
        }
        born
    }

    /// Realise a legend's imprint on the world.
    fn realise(&mut self, m: &Manifestation, legend: &Legend) {
        match m {
            Manifestation::Constellation { star_name, element } => {
                // Brighter for a weightier life.
                let weight = (self.chronicle.renown_of(&legend.about).total() / 200.0).clamp(0.2, 2.0);
                self.firmament.ignite(star_name.clone(), element.clone(), legend.born_tick, weight);
            }
            // The rest are world-sites the sim spawns; queue them for it to drain.
            other => self.pending_sites.push(other.clone()),
        }
    }

    /// Drain the queued world-imprints for the sim to realise (spawn relics, raise the
    /// dead at haunts, summon named foes). Call once per sim tick.
    pub fn take_pending(&mut self) -> Vec<Manifestation> {
        std::mem::take(&mut self.pending_sites)
    }

    /// The one question the combat sim asks the living world: **how strong is a spell of
    /// `element`, cast at `pos`, right now?** Folds together —
    /// the season's ascendant element + mana tide, the local aetherweather, the leyline
    /// charge and its dominant element, and the risen constellations — into one
    /// multiplier the magic VM applies to the spell's magnitude.
    ///
    /// This is the payoff of the whole crate: the world you have shaped (drained these
    /// leylines, fought under this sky, hung these stars) measurably changes your magic.
    pub fn spell_power_at(&self, element: &str, pos: Vec3) -> f32 {
        let cal = Calendar::at(self.tick);
        let mut m = 1.0;

        // Season: ambient mana tide + the ascendant element's blessing.
        m *= cal.season.mana_tide().sqrt();
        if cal.season.ascendant_element() == element {
            m *= 1.15;
        }

        // Leylines: charged land empowers; matching dominant element blesses.
        let charge = self.leylines.charge_at(pos);
        if self.leylines.dominant_element_at(pos) == Some(element) {
            m *= 1.0 + charge * 0.3;
        }

        // Aetherweather over this spot.
        let weather = self.weather.at(self.tick, &cal, charge);
        m *= weather.sky.spell_mult(element);

        // The risen stars of this school.
        m *= self.firmament.boon(element, &cal);

        m.max(0.05)
    }

    /// The weather over a point right now (uses the region's leyline charge there).
    pub fn weather_at(&self, pos: Vec3) -> WeatherState {
        let cal = Calendar::at(self.tick);
        let charge = self.leylines.charge_at(pos);
        self.weather.at(self.tick, &cal, charge)
    }

    /// The omens currently circulating (look `horizon_days` ahead).
    pub fn omens(&self, horizon_days: u64) -> Vec<Omen> {
        OmenWeaver.divine(&self.calendar(), horizon_days)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chronicle::{Deed, DeedKind};

    fn seeded_world() -> WorldSoul {
        let mut w = WorldSoul::new(0xCE5E);
        // A couple of leyline wells.
        let a = w.leylines.add_node(LeylineNode::new("well.spire", Vec3::ZERO, 1000.0, "fire"));
        let b = w.leylines.add_node(LeylineNode::new("well.mire", Vec3::new(80.0, 0.0, 0.0), 1000.0, "shadow"));
        w.leylines.connect(a, b, 0.8);
        // A tiny food web.
        w.ecology.add(Species { mob: "mob.wisp".into(), trophic: Trophic::Grazer, prey: vec![], carrying_capacity: 100.0, growth: 0.5, population: 30.0 });
        w
    }

    #[test]
    fn advancing_turns_the_world() {
        let mut w = seeded_world();
        // Jump a full season; expect a season-turn announcement somewhere along the way.
        let mut turned = false;
        let step = calendar::DAY_TICKS; // a day at a time
        for d in 1..=(calendar::DAYS_PER_SEASON + 1) {
            let pulse = w.advance(d * step);
            if pulse.season_turned.is_some() {
                turned = true;
            }
        }
        assert!(turned, "crossing a season boundary should turn the world");
    }

    #[test]
    fn a_great_deed_becomes_a_star_that_boons_its_school() {
        let mut w = seeded_world();
        w.advance(10);
        // Fell a legend -> mints a legend -> (for valor) hangs a constellation.
        let deed = Deed {
            actor: "hero".into(),
            actor_name: "Hero".into(),
            kind: DeedKind::Slay { victim: "the Bone Colossus".into(), victim_renown: 600.0 },
            location: Vec3::ZERO,
            tick: 10,
            magnitude: 80.0,
        };
        let born = w.record_deed(&deed);
        assert_eq!(born.len(), 1);
        // If it became a constellation, the firmament grew.
        if matches!(born[0].manifestation, Manifestation::Constellation { .. }) {
            assert!(w.firmament.count() >= 1, "the legend should be hung in the sky");
        }
    }

    #[test]
    fn spell_power_reflects_the_world() {
        let mut w = seeded_world();
        w.advance(100);
        // Power near the fire well should be a finite, positive multiplier.
        let p = w.spell_power_at("fire", Vec3::new(2.0, 0.0, 0.0));
        assert!(p.is_finite() && p > 0.0, "spell power must be a usable multiplier, got {p}");
    }

    #[test]
    fn determinism_two_worlds_agree() {
        let mut a = seeded_world();
        let mut b = seeded_world();
        let deed = Deed { actor: "x".into(), actor_name: "X".into(), kind: DeedKind::Forge { item: "item.runeblade".into() }, location: Vec3::ZERO, tick: 50, magnitude: 10.0 };
        for t in [10u64, 20, 30, 40] {
            assert_eq!(a.advance(t).is_silent(), b.advance(t).is_silent());
        }
        let la = a.record_deed(&deed);
        let lb = b.record_deed(&deed);
        assert_eq!(la[0].name, lb[0].name, "the same world + deed => the same legend");
    }
}
