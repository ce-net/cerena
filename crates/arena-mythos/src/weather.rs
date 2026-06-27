//! Aetherweather — the magical weather that rolls across Cerena's zones.
//!
//! This is not rain and sun (though it is that too); it is the *weather of magic*. A
//! mana storm super-charges spells and fries the careless. Blightfog rots the land and
//! hides the dead. Starfall drops aether-iron from the sky. An aurora means the veil to
//! the Astral has thinned. The Doldrums are the dread of every mage: a mana-dead calm
//! where spells gutter and you must rely on steel and wits.
//!
//! Weather is deterministic: a smooth function of tick + a zone seed + the season and
//! leyline charge, so every authority simulating a zone sees the same sky.

use serde::{Deserialize, Serialize};

use crate::calendar::{Calendar, Season};
use crate::rng::Rng;

/// A kind of aetherweather. Each warps casting, movement, visibility, and what spawns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sky {
    /// Calm and clear. The default.
    Clear,
    /// Mundane overcast/rain. Dampens fire, lifts nature.
    Rain,
    /// A mana storm: arcs of wild magic. Spell power up, but miscasts and wild surges.
    ManaStorm,
    /// Blightfog: a creeping shadow-rot. Vision low, the dead bold, nature withers.
    Blightfog,
    /// Starfall: aether meteors rain down — danger and star-iron both.
    Starfall,
    /// Aurora: the veil thins; the Astral bleeds through, summons strengthen.
    Aurora,
    /// Emberrain: glowing motes drift down in Kindling/Emberfall; fire ascendant.
    Emberrain,
    /// Frostveil: a shimmering cold haze of the Long Dark; frost ascendant, stamina drains.
    Frostveil,
    /// The Doldrums: a mana-dead calm. Spells cost far more; a mage's nightmare.
    Doldrums,
}

impl Sky {
    pub fn name(self) -> &'static str {
        match self {
            Sky::Clear => "Clear",
            Sky::Rain => "Rain",
            Sky::ManaStorm => "Mana Storm",
            Sky::Blightfog => "Blightfog",
            Sky::Starfall => "Starfall",
            Sky::Aurora => "Aurora",
            Sky::Emberrain => "Emberrain",
            Sky::Frostveil => "Frostveil",
            Sky::Doldrums => "the Doldrums",
        }
    }

    /// Multiplier on the *effectiveness* of spells of `element` cast in this weather.
    /// This is where the sky becomes tactics: chase a mana storm with your nukes, fear
    /// the Doldrums, lean into the season's weather for your school.
    pub fn spell_mult(self, element: &str) -> f32 {
        match self {
            Sky::ManaStorm => 1.4,
            Sky::Doldrums => 0.5,
            Sky::Aurora if matches!(element, "void" | "arcane" | "chrono") => 1.5,
            Sky::Blightfog if matches!(element, "shadow" | "blood" | "nature") => 1.3,
            Sky::Emberrain if element == "fire" => 1.45,
            Sky::Frostveil if element == "frost" => 1.45,
            Sky::Rain if element == "fire" => 0.7,
            Sky::Rain if element == "storm" => 1.2,
            _ => 1.0,
        }
    }

    /// Ambient visibility 0..1 (fog/haze cut). Drives draw distance and stealth.
    pub fn visibility(self) -> f32 {
        match self {
            Sky::Blightfog => 0.3,
            Sky::Frostveil => 0.55,
            Sky::ManaStorm => 0.7,
            Sky::Rain => 0.7,
            Sky::Starfall => 0.8,
            _ => 1.0,
        }
    }

    /// Does this sky drop a special world resource? (Starfall → star-iron; Emberrain →
    /// ember-motes.) Returned as a reagent id the spawn system can scatter.
    pub fn resource_drop(self) -> Option<&'static str> {
        match self {
            Sky::Starfall => Some("item.star_iron"),
            Sky::Emberrain => Some("item.ember_mote"),
            Sky::Aurora => Some("item.astral_dew"),
            _ => None,
        }
    }
}

/// The live weather over one zone: the sky, how intense it is, and how long until the
/// front shifts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WeatherState {
    pub sky: Sky,
    /// 0..1 — a building storm at 0.2 is a different beast than a raging one at 1.0.
    pub intensity: f32,
    /// Fraction `0..1` through the current front (for smooth fade in/out on the client).
    pub progress: f32,
}

impl WeatherState {
    pub const CALM: WeatherState = WeatherState { sky: Sky::Clear, intensity: 0.0, progress: 0.0 };
}

/// Samples deterministic aetherweather for a zone. Stateless: the sky at any tick is a
/// pure function of the inputs, so no front needs to be replicated — just recompute.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AetherWeather {
    /// Per-zone seed (so neighbouring zones have different, but coherent, weather).
    pub zone_seed: u64,
    /// Mean ticks a weather front lasts before the next is rolled.
    pub front_ticks: u64,
}

impl AetherWeather {
    pub fn new(zone_seed: u64) -> Self {
        Self { zone_seed, front_ticks: crate::calendar::DAY_TICKS / 6 } // ~4 fronts/day
    }

    /// The weather over this zone at `tick`, given the calendar and the zone's current
    /// leyline charge (0..1; charged land breeds mana storms, drained land the Doldrums).
    pub fn at(&self, tick: u64, cal: &Calendar, leyline_charge: f32) -> WeatherState {
        let front = tick / self.front_ticks;
        let progress = (tick % self.front_ticks) as f32 / self.front_ticks as f32;
        let mut rng = Rng::new(self.zone_seed ^ front.wrapping_mul(0x9E37_79B9));

        // The Grand Conjunction overrides everything with an aurora-storm.
        if cal.is_grand_conjunction() {
            return WeatherState { sky: Sky::Aurora, intensity: 1.0, progress };
        }

        // Weighted draw biased by season, leyline charge, and the Hollow moon.
        let mut weights: Vec<(Sky, f32)> = vec![
            (Sky::Clear, 3.0),
            (Sky::Rain, 1.5),
            (Sky::ManaStorm, 0.6 + leyline_charge * 2.5),
            (Sky::Blightfog, 0.4 + cal.hollow.fullness() * 1.5),
            (Sky::Aurora, 0.2 + cal.hollow.fullness() * 0.8),
            (Sky::Doldrums, 0.4 + (1.0 - leyline_charge) * 1.5),
        ];
        match cal.season {
            Season::Kindling | Season::Emberfall => weights.push((Sky::Emberrain, 1.2)),
            Season::LongDark => {
                weights.push((Sky::Frostveil, 2.0));
                weights.push((Sky::Starfall, 0.8));
            }
            Season::Highsun => weights.push((Sky::Clear, 2.0)),
            Season::Duskwane => weights.push((Sky::Blightfog, 1.0)),
        }

        let total: f32 = weights.iter().map(|(_, w)| *w).sum();
        let mut pick = rng.unit() * total;
        let mut sky = Sky::Clear;
        for (s, w) in &weights {
            pick -= *w;
            if pick <= 0.0 {
                sky = *s;
                break;
            }
        }

        // Intensity ramps in and out across the front (a bell), scaled by a roll.
        let bell = (progress * std::f32::consts::PI).sin();
        let peak = rng.range(0.5, 1.0);
        WeatherState { sky, intensity: (bell * peak).clamp(0.0, 1.0), progress }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weather_is_deterministic() {
        let w = AetherWeather::new(0x1234);
        let cal = Calendar::at(50_000);
        let a = w.at(50_000, &cal, 0.5);
        let b = w.at(50_000, &cal, 0.5);
        assert_eq!(a, b, "same inputs must give the same sky");
    }

    #[test]
    fn drained_land_tends_to_doldrums_charged_to_storms() {
        let w = AetherWeather::new(0xBEEF);
        // Sample many fronts and count, comparing charged vs drained land.
        let mut storms_charged = 0;
        let mut doldrums_drained = 0;
        for f in 0..400u64 {
            let t = f * w.front_ticks + 10;
            let cal = Calendar::at(t);
            if w.at(t, &cal, 0.95).sky == Sky::ManaStorm {
                storms_charged += 1;
            }
            if w.at(t, &cal, 0.05).sky == Sky::Doldrums {
                doldrums_drained += 1;
            }
        }
        assert!(storms_charged > 0, "charged land should brew mana storms");
        assert!(doldrums_drained > 0, "drained land should fall to the Doldrums");
    }

    #[test]
    fn conjunction_forces_aurora() {
        // Find a conjunction day and assert the sky is the aurora-storm.
        for d in 0..(7 * 11 * 13u64) {
            let cal = Calendar::at(d * crate::calendar::DAY_TICKS);
            if cal.is_grand_conjunction() {
                let w = AetherWeather::new(7);
                assert_eq!(w.at(cal.tick, &cal, 0.5).sky, Sky::Aurora);
                return;
            }
        }
    }
}
