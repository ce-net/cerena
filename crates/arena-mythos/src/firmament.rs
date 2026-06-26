//! The Firmament — the night sky as the world's memory.
//!
//! When the Chronicle hangs a bright legend in the stars (a `Manifestation::
//! Constellation`), it becomes a real, named constellation here. The sky is therefore a
//! *readable history of the age*: look up and you see the heroes who came before,
//! arranged by the element of their deeds. And the stars are not mere decoration — a
//! constellation that has *risen* (it is night, and the constellation's season is near)
//! lends its boon to mages of its element. Players will learn to time great workings to
//! the stars of their school.

use serde::{Deserialize, Serialize};

use crate::calendar::{Calendar, Season};

/// One star/constellation in the firmament: a legend made eternal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Star {
    /// The legend's name (e.g. "Vaelith the Emberhearted").
    pub name: String,
    /// The element the constellation governs; it boons that school.
    pub element: String,
    /// The tick it was first lit.
    pub born_tick: u64,
    /// Base brightness — grander legends shine brighter and boon more strongly.
    pub brightness: f32,
    /// Which arc of the sky it occupies (0..1), placed deterministically so two nodes
    /// draw the same sky.
    pub arc: f32,
}

impl Star {
    /// The season this star is ascendant in — derived from its element so the
    /// constellations of a school cluster in that school's time of power.
    pub fn ascendant_season(&self) -> Season {
        match self.element.as_str() {
            "fire" => Season::Kindling,
            "radiant" => Season::Highsun,
            "nature" | "storm" => Season::Emberfall,
            "shadow" | "void" | "blood" => Season::Duskwane,
            "frost" => Season::LongDark,
            _ => Season::Highsun,
        }
    }
}

/// The whole night sky.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Firmament {
    pub stars: Vec<Star>,
}

impl Firmament {
    /// Light a new constellation. Brighter the weightier the legend (passed as a 0..1
    /// renown fraction). Returns the star's name for heralding.
    pub fn ignite(&mut self, name: impl Into<String>, element: impl Into<String>, tick: u64, weight: f32) -> String {
        let name = name.into();
        // Deterministic placement in the sky from the name.
        let mut h: u64 = 1469598103934665603;
        for b in name.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211);
        }
        let arc = (h % 10_000) as f32 / 10_000.0;
        self.stars.push(Star {
            name: name.clone(),
            element: element.into(),
            born_tick: tick,
            brightness: (0.4 + weight).clamp(0.4, 2.0),
            arc,
        });
        name
    }

    /// How visible the sky is right now (1 at deep night, 0 by day). The boons fade in
    /// with the dark.
    pub fn visibility(&self, cal: &Calendar) -> f32 {
        // Peak at midnight (time_of_day 0), zero around noon.
        let t = cal.time_of_day;
        let night = if t < 0.5 { 1.0 - t * 2.0 } else { (t - 0.5) * 2.0 };
        night.clamp(0.0, 1.0)
    }

    /// The constellation boon for `element` right now: a small multiplier on that
    /// school's spell power, summed over every risen star of that element, scaled by how
    /// dark it is and whether their season is near. Daytime ≈ no boon.
    pub fn boon(&self, element: &str, cal: &Calendar) -> f32 {
        let vis = self.visibility(cal);
        if vis <= 0.0 {
            return 1.0;
        }
        let mut bonus = 0.0;
        for star in &self.stars {
            if star.element != element {
                continue;
            }
            // Ascendant in its own season, present (but dimmer) otherwise.
            let season_factor = if star.ascendant_season() == cal.season { 1.0 } else { 0.35 };
            bonus += star.brightness * 0.03 * season_factor;
        }
        1.0 + bonus * vis
    }

    /// The constellations risen tonight, brightest first — for the star-map UI and for
    /// players choosing when to work their greatest magic.
    pub fn risen(&self, cal: &Calendar) -> Vec<&Star> {
        if self.visibility(cal) <= 0.0 {
            return Vec::new();
        }
        let mut v: Vec<&Star> = self.stars.iter().collect();
        v.sort_by(|a, b| {
            let sa = if a.ascendant_season() == cal.season { a.brightness * 2.0 } else { a.brightness };
            let sb = if b.ascendant_season() == cal.season { b.brightness * 2.0 } else { b.brightness };
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        v
    }

    pub fn count(&self) -> usize {
        self.stars.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calendar::DAY_TICKS;

    #[test]
    fn a_lit_star_boons_its_element_at_night() {
        let mut sky = Firmament::default();
        sky.ignite("Vaelith the Emberhearted", "fire", 0, 1.0);
        // Midnight in Kindling (fire's season) -> strong boon.
        let night = Calendar::at(0); // time_of_day 0 = midnight, Kindling
        let boon = sky.boon("fire", &night);
        assert!(boon > 1.0, "a fire constellation should boon fire at night, got {boon}");
        // Frost is unaffected.
        assert_eq!(sky.boon("frost", &night), 1.0);
    }

    #[test]
    fn no_boon_at_noon() {
        let mut sky = Firmament::default();
        sky.ignite("X", "fire", 0, 1.0);
        let noon = Calendar::at(DAY_TICKS / 2);
        assert!((sky.boon("fire", &noon) - 1.0).abs() < 1e-6, "stars don't shine at noon");
    }
}
