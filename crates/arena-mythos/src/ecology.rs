//! Ecology — the world's creatures as a living food web, not a static spawn table.
//!
//! Wisps graze the leylines; forest guardians keep the wisps in check; void wraiths prey
//! on both; an apex like a crystal golem sits atop it all. Populations rise and fall by
//! a deterministic predator–prey simulation tuned by the season: herbivores bloom in
//! Verdance/Emberfall, everything thins in the Long Dark. When a population blooms it
//! floods nearby zones; when it crashes its predators starve and migrate; and now and
//! then a lineage **mutates** into a tougher variant — the seed of a new named foe.
//!
//! The sim's spawn system reads these populations to decide what actually appears, so
//! the world feels like an ecosystem reacting to the players thinning it, rather than a
//! respawn timer.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::calendar::{Calendar, Season};
use crate::rng::Rng;

/// Where a species sits in the food web.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trophic {
    /// Eats ambient mana/flora; grows on its own, bounded by carrying capacity.
    Grazer,
    /// Eats grazers; grows only when prey is plentiful.
    Predator,
    /// Top of the chain; few, slow-breeding, fearsome.
    Apex,
}

/// One species in the web.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Species {
    /// The content mob id this species spawns as (`"mob.wisp"`).
    pub mob: String,
    pub trophic: Trophic,
    /// Mob ids this species preys on (empty for grazers).
    pub prey: Vec<String>,
    /// Equilibrium population the land can support.
    pub carrying_capacity: f32,
    /// Intrinsic growth rate per day.
    pub growth: f32,
    /// Current population (a continuous number; the spawner rounds/quantises).
    pub population: f32,
}

/// Something notable the ecology did this step — fed to the world as flavour + spawn
/// pressure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EcologyEvent {
    /// A population exploded past its capacity — they spill into neighbouring zones.
    Bloom { mob: String, population: f32 },
    /// A population collapsed — its predators will starve and roam.
    Crash { mob: String },
    /// A lineage mutated into a tougher variant (a candidate named foe).
    Mutation { base: String, variant: String, power_mult: f32 },
}

/// The food web for a region and its population dynamics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Ecosystem {
    pub species: Vec<Species>,
    /// Day index of the last `tick` (so we step once per in-world day).
    last_day: u64,
}

impl Ecosystem {
    pub fn add(&mut self, species: Species) {
        self.species.push(species);
    }

    fn pop_of(&self, mob: &str) -> f32 {
        self.species.iter().find(|s| s.mob == mob).map(|s| s.population).unwrap_or(0.0)
    }

    /// Advance the ecology. Runs at most once per in-world day (populations move slowly
    /// relative to combat). Returns the notable events of the day. Deterministic.
    pub fn tick(&mut self, cal: &Calendar) -> Vec<EcologyEvent> {
        let day = cal.tick / crate::calendar::DAY_TICKS;
        if day == self.last_day {
            return Vec::new();
        }
        self.last_day = day;

        // Season modulates growth: a lush Emberfall vs a starving Long Dark.
        let season_growth = match cal.season {
            Season::Kindling => 1.1,
            Season::Highsun => 1.0,
            Season::Emberfall => 1.3,
            Season::Duskwane => 0.8,
            Season::LongDark => 0.5,
        };

        // Snapshot prey availability before mutating (so the step is order-independent).
        let prey_totals: HashMap<String, f32> = self
            .species
            .iter()
            .map(|s| {
                let avail: f32 = s.prey.iter().map(|p| self.pop_of(p)).sum();
                (s.mob.clone(), avail)
            })
            .collect();

        let mut events = Vec::new();
        for s in &mut self.species {
            let prev = s.population;
            let next = match s.trophic {
                Trophic::Grazer => {
                    // Logistic growth toward capacity.
                    let r = s.growth * season_growth;
                    prev + r * prev * (1.0 - prev / s.carrying_capacity.max(1.0))
                }
                Trophic::Predator | Trophic::Apex => {
                    // Grow with prey, decay without it.
                    let prey = prey_totals.get(&s.mob).copied().unwrap_or(0.0);
                    let support = (prey / s.carrying_capacity.max(1.0)).min(1.0);
                    let r = s.growth * season_growth;
                    prev + r * prev * (support - 0.5) - prev * 0.02
                }
            };
            s.population = next.max(0.0);

            // Bloom / crash detection.
            if s.population > s.carrying_capacity * 1.3 && prev <= s.carrying_capacity * 1.3 {
                events.push(EcologyEvent::Bloom { mob: s.mob.clone(), population: s.population });
            }
            if s.population < s.carrying_capacity * 0.1 && prev >= s.carrying_capacity * 0.1 {
                events.push(EcologyEvent::Crash { mob: s.mob.clone() });
            }

            // Rare mutation, likelier under stress (a near-crash population that claws
            // back is where the tough survivors breed).
            let mut rng = Rng::from_tick(cal.tick, &s.mob);
            let stress = if s.population < s.carrying_capacity * 0.2 { 3.0 } else { 1.0 };
            if rng.chance(0.01 * stress) {
                let variant = format!("{}.scion", s.mob);
                events.push(EcologyEvent::Mutation {
                    base: s.mob.clone(),
                    variant,
                    power_mult: rng.range(1.2, 1.9),
                });
            }
        }

        events
    }

    /// The current spawn weight for a mob: its population relative to capacity, so a
    /// thinned-out species becomes rare and an overgrown one floods the zone. The
    /// spawn system multiplies its base rules by this.
    pub fn spawn_weight(&self, mob: &str) -> f32 {
        self.species
            .iter()
            .find(|s| s.mob == mob)
            .map(|s| (s.population / s.carrying_capacity.max(1.0)).clamp(0.0, 2.5))
            .unwrap_or(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::DAY_TICKS;

    fn web() -> Ecosystem {
        let mut e = Ecosystem::default();
        e.add(Species { mob: "mob.wisp".into(), trophic: Trophic::Grazer, prey: vec![], carrying_capacity: 100.0, growth: 0.5, population: 10.0 });
        e.add(Species { mob: "mob.void_wraith".into(), trophic: Trophic::Predator, prey: vec!["mob.wisp".into()], carrying_capacity: 40.0, growth: 0.4, population: 5.0 });
        e
    }

    #[test]
    fn grazers_grow_toward_capacity() {
        let mut e = web();
        for d in 1..40 {
            e.tick(&Calendar::at(d * DAY_TICKS));
        }
        let wisps = e.species[0].population;
        assert!(wisps > 50.0, "wisps should grow toward capacity, got {wisps}");
    }

    #[test]
    fn tick_runs_once_per_day() {
        let mut e = web();
        // Same day -> no change after the first call.
        let first = e.tick(&Calendar::at(DAY_TICKS + 10));
        let _ = first;
        let pop_after_first = e.species[0].population;
        e.tick(&Calendar::at(DAY_TICKS + 20)); // same day index
        assert_eq!(e.species[0].population, pop_after_first, "ecology steps once per day");
    }

    #[test]
    fn spawn_weight_tracks_population() {
        let mut e = web();
        e.species[0].population = 200.0; // way over capacity 100
        assert!(e.spawn_weight("mob.wisp") > 1.5, "a bloom should flood spawns");
        e.species[0].population = 1.0;
        assert!(e.spawn_weight("mob.wisp") < 0.1, "a crash should starve spawns");
    }
}
