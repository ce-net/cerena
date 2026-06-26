//! Leylines — the magical circulatory system of the world.
//!
//! Mana is not an abstract bar that only lives in a player; it is a *substance that
//! flows through the land*. Wells of power (leyline nodes) sit at places of meaning —
//! a crystal spire, a sunken monolith, a barrow — and rivers of mana (leylines)
//! connect them. Cast a big spell and you draw the local land down; the leylines flow
//! to refill it, draining their neighbours. Charged land breeds mana storms and
//! empowers your magic; land you have bled dry falls to the Doldrums.
//!
//! Players and their towers can **claim** a well, tithing its flow — which makes
//! leyline geography worth fighting over. The whole network is a deterministic
//! diffusion simulation, so every authority agrees on where the power is.

use glam::Vec3;
use serde::{Deserialize, Serialize};

use crate::calendar::Calendar;

/// A node in the leyline graph: a well of power at a place.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeylineNode {
    /// Stable id (often a structure / landmark name, e.g. `"well.sunken_monolith"`).
    pub id: String,
    /// World position of the well.
    pub pos: Vec3,
    /// Maximum mana the well can hold.
    pub capacity: f32,
    /// Current stored mana `0..capacity`.
    pub charge: f32,
    /// The element this well resonates with (amplifies that school nearby).
    pub element: String,
    /// The CE node id of the player/tower that has claimed it, if any.
    pub claimed_by: Option<String>,
    /// Radius (metres) within which this well empowers casting and colours the sky.
    pub influence: f32,
}

impl LeylineNode {
    pub fn new(id: impl Into<String>, pos: Vec3, capacity: f32, element: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            pos,
            capacity,
            charge: capacity * 0.5,
            element: element.into(),
            claimed_by: None,
            influence: 40.0,
        }
    }

    /// How charged this well is, `0..1`.
    pub fn saturation(&self) -> f32 {
        if self.capacity > 0.0 { (self.charge / self.capacity).clamp(0.0, 1.0) } else { 0.0 }
    }
}

/// A leyline: a conductive river of mana between two wells.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Leyline {
    /// Index of the two endpoint nodes in the network.
    pub a: usize,
    pub b: usize,
    /// How freely mana flows along it `0..1` (geography: a strong line equalises fast).
    pub conductance: f32,
}

/// The full leyline network for a region, plus its diffusion + recharge simulation.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeylineNetwork {
    pub nodes: Vec<LeylineNode>,
    pub lines: Vec<Leyline>,
}

impl LeylineNetwork {
    /// Add a well, returning its index.
    pub fn add_node(&mut self, node: LeylineNode) -> usize {
        self.nodes.push(node);
        self.nodes.len() - 1
    }

    /// Connect two wells with a leyline of the given conductance.
    pub fn connect(&mut self, a: usize, b: usize, conductance: f32) {
        if a != b && a < self.nodes.len() && b < self.nodes.len() {
            self.lines.push(Leyline { a, b, conductance });
        }
    }

    /// Advance the network one step: each well slowly recharges from the ambient season
    /// tide, and mana diffuses along leylines toward equilibrium. Deterministic.
    pub fn tick(&mut self, dt: f32, cal: &Calendar) {
        // 1) Ambient recharge, scaled by the season's mana tide. The land breathes.
        let tide = cal.season.mana_tide();
        for n in &mut self.nodes {
            let regen = n.capacity * 0.01 * tide * dt;
            n.charge = (n.charge + regen).min(n.capacity);
        }

        // 2) Diffuse along leylines: mana flows from fuller to emptier, proportional to
        //    the saturation gradient and the line's conductance.
        let sats: Vec<f32> = self.nodes.iter().map(|n| n.saturation()).collect();
        let mut delta = vec![0.0f32; self.nodes.len()];
        for line in &self.lines {
            let grad = sats[line.a] - sats[line.b];
            // Move a fraction of the gradient, in absolute mana units.
            let flow = grad * line.conductance * 0.5 * dt
                * self.nodes[line.a].capacity.min(self.nodes[line.b].capacity);
            delta[line.a] -= flow;
            delta[line.b] += flow;
        }
        for (n, d) in self.nodes.iter_mut().zip(delta) {
            n.charge = (n.charge + d).clamp(0.0, n.capacity);
        }
    }

    /// Draw `amount` of mana from the land near `pos` (a big cast tithes the leylines).
    /// Returns how much was actually available — a spell cast on drained land may fizzle
    /// or cost the caster's own reserves to make up the difference.
    pub fn draw(&mut self, pos: Vec3, amount: f32) -> f32 {
        let mut remaining = amount;
        // Pull from the nearest influential wells first.
        let mut order: Vec<usize> = (0..self.nodes.len()).collect();
        order.sort_by(|&i, &j| {
            let di = (self.nodes[i].pos - pos).length_squared();
            let dj = (self.nodes[j].pos - pos).length_squared();
            di.partial_cmp(&dj).unwrap_or(std::cmp::Ordering::Equal)
        });
        for i in order {
            if remaining <= 0.0 {
                break;
            }
            let n = &mut self.nodes[i];
            if (n.pos - pos).length() > n.influence {
                continue;
            }
            let take = remaining.min(n.charge);
            n.charge -= take;
            remaining -= take;
        }
        amount - remaining
    }

    /// The ambient leyline charge `0..1` felt at `pos`: a distance-weighted blend of the
    /// saturation of every well whose influence reaches it. Drives local weather, the
    /// empowerment of spells, and the colour of the sky.
    pub fn charge_at(&self, pos: Vec3) -> f32 {
        let mut weight = 0.0;
        let mut acc = 0.0;
        for n in &self.nodes {
            let d = (n.pos - pos).length();
            if d <= n.influence {
                let w = 1.0 - d / n.influence;
                acc += n.saturation() * w;
                weight += w;
            }
        }
        if weight > 0.0 { (acc / weight).clamp(0.0, 1.0) } else { 0.0 }
    }

    /// The strongest-resonating element at `pos` (whichever influential well dominates),
    /// so casting that school there gets the leyline's blessing.
    pub fn dominant_element_at(&self, pos: Vec3) -> Option<&str> {
        let mut best: Option<(&str, f32)> = None;
        for n in &self.nodes {
            let d = (n.pos - pos).length();
            if d <= n.influence {
                let score = n.saturation() * (1.0 - d / n.influence);
                if best.map_or(true, |(_, s)| score > s) {
                    best = Some((n.element.as_str(), score));
                }
            }
        }
        best.map(|(e, _)| e)
    }

    /// Claim a well for a player/tower. Returns false if already held by someone else.
    pub fn claim(&mut self, idx: usize, who: &str) -> bool {
        if let Some(n) = self.nodes.get_mut(idx) {
            match &n.claimed_by {
                Some(owner) if owner != who => false,
                _ => {
                    n.claimed_by = Some(who.to_string());
                    true
                }
            }
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_well_net() -> LeylineNetwork {
        let mut net = LeylineNetwork::default();
        let a = net.add_node(LeylineNode::new("well.a", Vec3::ZERO, 1000.0, "fire"));
        let b = net.add_node(LeylineNode::new("well.b", Vec3::new(50.0, 0.0, 0.0), 1000.0, "frost"));
        net.nodes[a].charge = 1000.0; // full
        net.nodes[b].charge = 0.0; // empty
        net.connect(a, b, 1.0);
        net
    }

    #[test]
    fn mana_diffuses_from_full_to_empty() {
        let mut net = two_well_net();
        let cal = Calendar::at(0);
        let before = net.nodes[1].charge;
        for _ in 0..30 {
            net.tick(1.0 / 64.0, &cal);
        }
        assert!(net.nodes[1].charge > before, "the empty well should fill from its neighbour");
        assert!(net.nodes[0].charge < 1000.0, "the full well should have drained somewhat");
    }

    #[test]
    fn drawing_drains_the_nearest_well() {
        let mut net = two_well_net();
        // Well A is full and at the origin with influence 40; draw near it.
        let got = net.draw(Vec3::new(5.0, 0.0, 0.0), 300.0);
        assert!(got > 0.0, "should pull mana from the nearby charged well");
        assert!(net.nodes[0].charge < 1000.0);
    }

    #[test]
    fn charge_field_reads_high_at_a_full_well() {
        let net = two_well_net();
        let c = net.charge_at(Vec3::new(2.0, 0.0, 0.0));
        assert!(c > 0.8, "right at the full well the charge should read high, got {c}");
    }
}
