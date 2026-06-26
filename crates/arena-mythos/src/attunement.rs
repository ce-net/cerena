//! Attunement — a mage slowly *becomes* the magic they practise.
//!
//! Cast fire long enough and the fire answers you faster, warmer, cheaper — but lean
//! too hard into shadow or blood and you court **corruption**, a creeping mark that
//! grants dark power at a cost. Dwell in a place and you bond to its leyline. Attunement
//! is the slow, personal counterpart to the world's seasons: it makes a veteran
//! pyromancer mechanically *different* from a veteran cryomancer, and it makes the path
//! you walk leave a mark on you.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A player's resonance with the elements and the corruption they've courted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attunement {
    /// Per-element resonance `0..1` (soft cap; grows with use, decays with neglect).
    pub elements: HashMap<String, f32>,
    /// Corruption `0..1` from over-practising the dark schools. Power with a price.
    pub corruption: f32,
    /// Places (leyline well ids) the mage has bonded to, and how strongly.
    pub places: HashMap<String, f32>,
}

/// The "dark" schools whose mastery feeds corruption.
fn is_dark(element: &str) -> bool {
    matches!(element, "shadow" | "blood" | "void")
}

impl Attunement {
    /// Practise an element (one cast). Builds resonance, and — for the dark schools —
    /// a little corruption. `intensity` scales with the spell's weight.
    pub fn practise(&mut self, element: &str, intensity: f32) {
        let e = self.elements.entry(element.to_string()).or_insert(0.0);
        // Diminishing returns toward 1.0.
        *e = (*e + intensity * 0.01 * (1.0 - *e)).clamp(0.0, 1.0);
        if is_dark(element) {
            self.corruption = (self.corruption + intensity * 0.004).clamp(0.0, 1.0);
        } else if self.corruption > 0.0 {
            // Practising the bright schools cleanses a little corruption.
            self.corruption = (self.corruption - intensity * 0.001).max(0.0);
        }
    }

    /// Daily drift: unused resonances fade slightly, so attunement reflects who you
    /// *are now*, not who you once were. Corruption fades only slowly.
    pub fn decay(&mut self) {
        for v in self.elements.values_mut() {
            *v = (*v - 0.005).max(0.0);
        }
        self.corruption = (self.corruption - 0.001).max(0.0);
    }

    /// Bond to a place (a leyline well) by dwelling there.
    pub fn visit(&mut self, well_id: &str, amount: f32) {
        let p = self.places.entry(well_id.to_string()).or_insert(0.0);
        *p = (*p + amount).clamp(0.0, 1.0);
    }

    /// Resonance with an element `0..1`.
    pub fn resonance(&self, element: &str) -> f32 {
        self.elements.get(element).copied().unwrap_or(0.0)
    }

    /// The spell-power multiplier this mage gets for `element`, folding in resonance and
    /// the wild edge corruption lends the dark schools.
    pub fn affinity_mult(&self, element: &str) -> f32 {
        let base = 1.0 + self.resonance(element) * 0.25; // up to +25% from mastery
        if is_dark(element) {
            // Corruption empowers the dark — temptation made mechanical.
            base + self.corruption * 0.3
        } else {
            base
        }
    }

    /// The element this mage is most attuned to (their "true school"), if any.
    pub fn primary_element(&self) -> Option<&str> {
        self.elements
            .iter()
            .filter(|(_, v)| **v > 0.1)
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k.as_str())
    }

    /// A visible mark/title for the mage's path, e.g. "Fire-Touched", or the dread
    /// "Corrupted" once they've fallen far enough.
    pub fn mark(&self) -> Option<String> {
        if self.corruption > 0.6 {
            return Some("the Corrupted".to_string());
        }
        let (el, v) = self.elements.iter().max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
        if *v < 0.5 {
            return None;
        }
        Some(match el.as_str() {
            "fire" => "Fire-Touched".into(),
            "frost" => "Frost-Touched".into(),
            "storm" => "Storm-Touched".into(),
            "nature" => "Green-Bonded".into(),
            "radiant" => "Light-Hallowed".into(),
            "shadow" | "void" => "Shadow-Steeped".into(),
            "blood" => "Blood-Marked".into(),
            "arcane" => "Arcane-Woven".into(),
            other => return Some(format!("{other}-Attuned")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn practise_builds_resonance_and_affinity() {
        let mut a = Attunement::default();
        for _ in 0..200 {
            a.practise("fire", 1.0);
        }
        assert!(a.resonance("fire") > 0.5, "sustained practice should build resonance");
        assert!(a.affinity_mult("fire") > 1.0, "resonance should empower the school");
        assert_eq!(a.primary_element(), Some("fire"));
        assert_eq!(a.mark().as_deref(), Some("Fire-Touched"));
    }

    #[test]
    fn dark_practice_breeds_corruption_with_power() {
        let mut a = Attunement::default();
        for _ in 0..300 {
            a.practise("shadow", 1.0);
        }
        assert!(a.corruption > 0.5, "the dark schools corrupt, got {}", a.corruption);
        // Corruption makes shadow hit harder — the bargain.
        assert!(a.affinity_mult("shadow") > a.affinity_mult("fire"));
        assert_eq!(a.mark().as_deref(), Some("the Corrupted"));
    }

    #[test]
    fn bright_practice_cleanses() {
        let mut a = Attunement::default();
        for _ in 0..100 {
            a.practise("shadow", 1.0);
        }
        let dirty = a.corruption;
        for _ in 0..500 {
            a.practise("radiant", 1.0);
        }
        assert!(a.corruption < dirty, "light should cleanse corruption");
    }
}
