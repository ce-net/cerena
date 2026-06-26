//! Omens — the world whispers before it speaks.
//!
//! Cerena foreshadows its own great turns. Before a Grand Conjunction, before the
//! Hollow swallows the stars, before a blight rises or a meteor swarm falls, the world
//! issues an **omen**: a cryptic line delivered to seers (high-Wisdom mages, or anyone
//! at a shrine) that *later comes true*. Players who read the omens can prepare — stock
//! star-iron before the Starfall, flee the mire before the blight, gather for the
//! Conjunction. The omens are deterministic, so the prophecy a node speaks is the
//! prophecy that comes to pass.

use serde::{Deserialize, Serialize};

use crate::calendar::{Calendar, Festival, DAY_TICKS};
use crate::rng::Rng;

/// What an omen foretells — the concrete event the cryptic line points at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Portent {
    /// A festival approaches in `in_days`.
    Festival { which: Festival, in_days: u64 },
    /// The Hollow moon will be full (the dead will walk) in `in_days`.
    HollowWaxing { in_days: u64 },
    /// Aetherweather of note is coming to the region.
    StormGathering,
    /// A named foe stirs and will rise.
    FoeStirring { name: String },
    /// The leylines run thin — the Doldrums approach.
    ManaEbbing,
}

/// An omen: a cryptic foretelling plus what it really means and when it resolves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Omen {
    /// The cryptic line a seer receives.
    pub verse: String,
    /// What it actually foretells.
    pub portent: Portent,
    /// The tick by which it comes true.
    pub resolves_tick: u64,
}

/// Reads the near future and issues omens. Stateless and deterministic — given the
/// calendar (and a peek at upcoming days) it always speaks the same prophecy.
#[derive(Debug, Clone, Copy, Default)]
pub struct OmenWeaver;

impl OmenWeaver {
    /// Look up to `horizon_days` ahead and gather the omens that should be circulating
    /// now. Returns them most-imminent first.
    pub fn divine(&self, cal: &Calendar, horizon_days: u64) -> Vec<Omen> {
        let mut omens = Vec::new();
        let today = cal.tick / DAY_TICKS;

        for d in 1..=horizon_days {
            let future_tick = (today + d) * DAY_TICKS;
            let future = Calendar::at(future_tick);
            let mut rng = Rng::from_tick(future_tick, "omen");

            // A festival approaches.
            if let Some(fest) = future.festival() {
                omens.push(Omen {
                    verse: festival_verse(fest, &mut rng),
                    portent: Portent::Festival { which: fest, in_days: d },
                    resolves_tick: future_tick,
                });
            } else if future.hollow.is_full() && !cal.hollow.is_full() {
                // The Hollow waxes to full — the dead stir.
                omens.push(Omen {
                    verse: "When the dark disc drinks the stars, the buried do not sleep.".into(),
                    portent: Portent::HollowWaxing { in_days: d },
                    resolves_tick: future_tick,
                });
            }
        }

        // A standing omen when the season turns toward the Long Dark's thin mana.
        if matches!(cal.season, crate::calendar::Season::Duskwane) && cal.day_of_season > 14 {
            omens.push(Omen {
                verse: "The rivers of power run shallow. Hoard your fire against the cold.".into(),
                portent: Portent::ManaEbbing,
                resolves_tick: (today + (21 - cal.day_of_season)) * DAY_TICKS,
            });
        }

        omens
    }

    /// Forge a bespoke omen that a named foe is stirring (the Chronicle calls this when
    /// it decides a `NamedFoeRises` legend will manifest soon).
    pub fn foe_omen(&self, name: &str, resolves_tick: u64) -> Omen {
        Omen {
            verse: format!("A name long buried turns in its grave. {name} will walk again."),
            portent: Portent::FoeStirring { name: name.to_string() },
            resolves_tick,
        }
    }
}

fn festival_verse(fest: Festival, rng: &mut Rng) -> String {
    let lines: &[&str] = match fest {
        Festival::FirstFlame => &["Soon the year wakes, and the first fire calls its own."],
        Festival::Solstice => &["The sun will pause to look upon us; bring your light to the shrines."],
        Festival::Harvest => &["The land prepares to give all at once. Sharpen your sickles."],
        Festival::NightOfTheDead => &["Count the nights: the Hollow comes full, and the dead come with it."],
        Festival::Starfall => &["The dark will weep iron. Stand where the meteors fall, if you dare."],
        Festival::GrandConjunction => &["Three eyes will open as one. On that night, the Weave answers anything."],
    };
    rng.pick(lines).copied().unwrap_or("Something approaches.").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omens_precede_festivals() {
        // A few days before Kindling's First Flame (day 0 of a year), an omen should
        // foretell it. Stand near year-end and look ahead.
        let near_year_end = Calendar::at((crate::calendar::DAYS_PER_YEAR - 2) * DAY_TICKS);
        let omens = OmenWeaver.divine(&near_year_end, 5);
        assert!(
            omens.iter().any(|o| matches!(o.portent, Portent::Festival { which: Festival::FirstFlame, .. })),
            "the First Flame should be foretold"
        );
    }

    #[test]
    fn omens_are_deterministic_and_resolve_in_future() {
        let cal = Calendar::at(12345);
        let a = OmenWeaver.divine(&cal, 10);
        let b = OmenWeaver.divine(&cal, 10);
        assert_eq!(a, b, "prophecy must be the same on every node");
        for o in a {
            assert!(o.resolves_tick > cal.tick, "an omen resolves in the future");
        }
    }
}
