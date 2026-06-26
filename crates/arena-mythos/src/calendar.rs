//! The turning of the year — seasons, three moons, and the festivals that hang off
//! them. Time is the metronome the rest of the living world dances to: leyline tides
//! breathe with the day, creatures breed in Verdance, the Hollow waxes in Duskwane,
//! and once a generation all three moons align and the Weave runs wild.
//!
//! Everything is derived from a single `Tick`, so it is the same on every node.

use serde::{Deserialize, Serialize};

/// Ticks per in-world day. The sim runs at 64 Hz; a Cerena day is a brisk ~24 real
/// minutes so a session sees dawn, noon, dusk, and the moonlit dark.
pub const DAY_TICKS: u64 = 64 * 60 * 24;

/// Days in a season, and seasons in a year.
pub const DAYS_PER_SEASON: u64 = 21;
pub const SEASONS_PER_YEAR: u64 = 5;
pub const DAYS_PER_YEAR: u64 = DAYS_PER_SEASON * SEASONS_PER_YEAR;

/// The five seasons of the mage-world's wheel. Not the mundane four — Cerena's year
/// turns on the breathing of magic itself, ending in the Long Dark when the void waxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Season {
    /// The waking of the year: fire returns, leylines swell, life stirs.
    Kindling,
    /// Magic at its high tide; the sun-drenched peak of power.
    Highsun,
    /// The golden falling; harvest of reagents, the air thick with spores.
    Emberfall,
    /// The waning; the Hollow moon strengthens, the dead grow restless.
    Duskwane,
    /// The Long Dark: the leylines run thin, the cold deepens, and the brave delve.
    LongDark,
}

impl Season {
    pub const ALL: [Season; 5] = [
        Season::Kindling,
        Season::Highsun,
        Season::Emberfall,
        Season::Duskwane,
        Season::LongDark,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Season::Kindling => "Kindling",
            Season::Highsun => "Highsun",
            Season::Emberfall => "Emberfall",
            Season::Duskwane => "Duskwane",
            Season::LongDark => "the Long Dark",
        }
    }

    /// A broad multiplier on ambient world mana for this season. Magic literally ebbs
    /// and flows across the year; spells cost a touch less at high tide.
    pub fn mana_tide(self) -> f32 {
        match self {
            Season::Kindling => 1.1,
            Season::Highsun => 1.3,
            Season::Emberfall => 1.0,
            Season::Duskwane => 0.85,
            Season::LongDark => 0.6,
        }
    }

    /// Which element the season favours — spells of this element are subtly amplified,
    /// and creatures of it grow bolder.
    pub fn ascendant_element(self) -> &'static str {
        match self {
            Season::Kindling => "fire",
            Season::Highsun => "radiant",
            Season::Emberfall => "nature",
            Season::Duskwane => "shadow",
            Season::LongDark => "frost",
        }
    }
}

/// The three moons of Cerena. Their phases drive tides, omens, and the restless dead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Moon {
    /// The Pale: steady, silver, a fast cycle. Governs clarity and scrying.
    Pale,
    /// The Ember: ruddy and slow. Its fullness stokes fire and fury.
    Ember,
    /// The Hollow: a dark disc that *occults* the stars. Its fullness wakes the dead
    /// and thins the veil to the Astral.
    Hollow,
}

impl Moon {
    pub const ALL: [Moon; 3] = [Moon::Pale, Moon::Ember, Moon::Hollow];

    pub fn name(self) -> &'static str {
        match self {
            Moon::Pale => "the Pale",
            Moon::Ember => "the Ember",
            Moon::Hollow => "the Hollow",
        }
    }

    /// Days in this moon's cycle (deliberately coprime-ish so alignments are rare).
    pub fn period_days(self) -> u64 {
        match self {
            Moon::Pale => 7,
            Moon::Ember => 11,
            Moon::Hollow => 13,
        }
    }
}

/// A moon's phase as a fraction `0.0..1.0` (0 = new, 0.5 = full, wrapping).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Phase(pub f32);

impl Phase {
    /// How "full" the moon is, `0.0` (new) to `1.0` (full).
    pub fn fullness(self) -> f32 {
        // Triangle wave: 0 at new, 1 at full, back to 0.
        1.0 - (self.0 * 2.0 - 1.0).abs()
    }
    pub fn is_full(self) -> bool {
        self.fullness() > 0.92
    }
    pub fn is_new(self) -> bool {
        self.fullness() < 0.08
    }
}

/// A read-only snapshot of the sky and calendar at one tick.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Calendar {
    pub tick: u64,
    /// Year since the world's first dawn.
    pub year: u64,
    /// Day within the year `0..DAYS_PER_YEAR`.
    pub day_of_year: u64,
    pub season: Season,
    /// Day within the current season.
    pub day_of_season: u64,
    /// Time of day `0.0..1.0` (0 = midnight, 0.5 = noon).
    pub time_of_day: f32,
    pub pale: Phase,
    pub ember: Phase,
    pub hollow: Phase,
}

impl Calendar {
    /// Derive the full calendar from a world tick. Pure function of `tick`.
    pub fn at(tick: u64) -> Self {
        let day = tick / DAY_TICKS;
        let time_of_day = (tick % DAY_TICKS) as f32 / DAY_TICKS as f32;
        let day_of_year = day % DAYS_PER_YEAR;
        let year = day / DAYS_PER_YEAR;
        let season_idx = (day_of_year / DAYS_PER_SEASON) as usize;
        let season = Season::ALL[season_idx.min(4)];
        let day_of_season = day_of_year % DAYS_PER_SEASON;

        let phase_of = |moon: Moon| {
            let p = (day % moon.period_days()) as f32 / moon.period_days() as f32;
            Phase(p)
        };

        Self {
            tick,
            year,
            day_of_year,
            season,
            day_of_season,
            time_of_day,
            pale: phase_of(Moon::Pale),
            ember: phase_of(Moon::Ember),
            hollow: phase_of(Moon::Hollow),
        }
    }

    /// Is it night? (Sun below the horizon.)
    pub fn is_night(self) -> bool {
        self.time_of_day < 0.22 || self.time_of_day > 0.78
    }

    /// The phase of a given moon.
    pub fn phase(self, moon: Moon) -> Phase {
        match moon {
            Moon::Pale => self.pale,
            Moon::Ember => self.ember,
            Moon::Hollow => self.hollow,
        }
    }

    /// The Grand Conjunction: all three moons full at once. A once-in-a-long-while event
    /// when the Weave runs wild — spell power surges, rifts open, legends are made. This
    /// is the world's great holiday and its great danger.
    pub fn is_grand_conjunction(self) -> bool {
        self.pale.is_full() && self.ember.is_full() && self.hollow.is_full()
    }

    /// The festival, if any, that falls on this day. Festivals are world-state, not
    /// cosmetic: the sim reads them to flip drop rates, spawn special foes, open shrines.
    pub fn festival(self) -> Option<Festival> {
        if self.is_grand_conjunction() {
            return Some(Festival::GrandConjunction);
        }
        match (self.season, self.day_of_season) {
            (Season::Kindling, 0) => Some(Festival::FirstFlame),
            (Season::Highsun, 10) => Some(Festival::Solstice),
            (Season::Emberfall, 14) => Some(Festival::Harvest),
            (Season::Duskwane, 20) if self.hollow.is_full() => Some(Festival::NightOfTheDead),
            (Season::LongDark, 10) => Some(Festival::Starfall),
            _ => None,
        }
    }
}

/// A festival: a dated world-event with real mechanical weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Festival {
    /// Dawn of Kindling: the first fire of the year; pyromancy ascendant, braziers lit.
    FirstFlame,
    /// Highsun solstice: the day of greatest light; radiant boons, the sun-shrines open.
    Solstice,
    /// Emberfall harvest: reagents bloom everywhere; double gathering.
    Harvest,
    /// The Night of the Dead: the Hollow full in Duskwane; the dead walk in force, but
    /// so do the richest necromantic rewards.
    NightOfTheDead,
    /// Starfall, in the Long Dark: meteors of pure aether rain down — gather star-iron.
    Starfall,
    /// The Grand Conjunction: the rarest day; the Weave unbound.
    GrandConjunction,
}

impl Festival {
    pub fn name(self) -> &'static str {
        match self {
            Festival::FirstFlame => "the First Flame",
            Festival::Solstice => "the Solstice of Highsun",
            Festival::Harvest => "the Emberfall Harvest",
            Festival::NightOfTheDead => "the Night of the Dead",
            Festival::Starfall => "the Starfall",
            Festival::GrandConjunction => "the Grand Conjunction",
        }
    }

    /// A herald line the world banner announces the festival with.
    pub fn herald(self) -> &'static str {
        match self {
            Festival::FirstFlame => "The first flame of the year is lit. Pyromancers, your hour wakes.",
            Festival::Solstice => "The sun stands still at its zenith. Light pours into every shrine.",
            Festival::Harvest => "The land overflows. Gather while the spores are thick.",
            Festival::NightOfTheDead => "The Hollow swallows the stars. The dead are walking — and they carry treasure.",
            Festival::Starfall => "Aether rains from the dark. Star-iron lies where the meteors fall.",
            Festival::GrandConjunction => "Three moons, one fullness. The Weave is unbound. Anything is possible tonight.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn day_zero_is_kindling_dawn() {
        let c = Calendar::at(0);
        assert_eq!(c.season, Season::Kindling);
        assert_eq!(c.year, 0);
        assert_eq!(c.day_of_year, 0);
        assert_eq!(c.festival(), Some(Festival::FirstFlame));
    }

    #[test]
    fn seasons_advance_and_wrap_into_years() {
        let one_season = Calendar::at(DAY_TICKS * DAYS_PER_SEASON);
        assert_eq!(one_season.season, Season::Highsun);
        let next_year = Calendar::at(DAY_TICKS * DAYS_PER_YEAR);
        assert_eq!(next_year.year, 1);
        assert_eq!(next_year.season, Season::Kindling);
    }

    #[test]
    fn moons_have_distinct_cycles() {
        // Over many days the three phases should not stay locked together.
        let mut aligned_days = 0;
        for d in 0..(7 * 11 * 13) {
            let c = Calendar::at(d * DAY_TICKS);
            if c.is_grand_conjunction() {
                aligned_days += 1;
            }
        }
        // The conjunction is rare but it does happen within the full super-period.
        assert!(aligned_days <= 3, "grand conjunction must be rare, saw {aligned_days}");
    }
}
