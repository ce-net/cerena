//! The bridge from the simulation to the **living world** (`arena-mythos`).
//!
//! `arena-mythos` is a pure, deterministic model of the world's soul — seasons, moons,
//! leyline mana, aetherweather, ecology, and the Chronicle that turns deeds into named
//! legends and constellations. This module wires it into [`crate::World`]:
//!
//! - every tick the world soul [`advance`](LivingWorld::advance)s, turning the year and
//!   evolving the leylines/ecology;
//! - every spell's magnitude is scaled by [`spell_power`](LivingWorld::spell_power),
//!   folding season + weather + leyline charge + the risen stars + the caster's personal
//!   attunement into one multiplier (injected in [`World::spell_damage_mult`]);
//! - every cast deepens the caster's [`attunement`](arena_mythos::Attunement) to its
//!   school (and courts corruption for the dark schools);
//! - every kill feeds a [`Deed`] to the Chronicle, which may mint a Legend, hang a
//!   constellation, and queue a world-imprint (a relic site, a haunt, a rising
//!   named foe) for the sim to realise.
//!
//! It is deterministic in the world tick + the deeds the sim feeds it, so an authority
//! and its shadow validators dream the same myth.
//!
//! > Scope note: each zone keeps its own [`LivingWorld`]. The tick-derived layers
//! > (seasons, moons, weather, leylines, omens) agree across zones for free because they
//! > share the world seed; the *deed-driven* mythology (Chronicle, firmament) is local
//! > until the coordinator gossips legends between zones — a future cross-zone sync.

use std::collections::HashMap;

use glam::Vec3;

use arena_content::ContentPack;
use arena_mythos::{
    Attunement, Deed, DeedKind, EcologyEvent, Legend, LeylineNode, Manifestation, Species, Trophic,
    WorldPulse, WorldSoul,
};

/// The fixed world-soul seed. Shared by every node so the tick-derived layers (seasons,
/// weather, leylines, omens) are identical across the whole world.
pub const LIVING_SEED: u64 = 0xCE5E_4A00_5000_0001;

/// The sim-side owner of the world soul plus per-player attunement.
#[derive(Debug, Clone)]
pub struct LivingWorld {
    /// The world's deterministic soul.
    pub soul: WorldSoul,
    /// Per-player (CE node id) magical attunement and corruption.
    pub attunements: HashMap<String, Attunement>,
}

impl LivingWorld {
    /// Create the living world and seed its leylines + ecology from the active content
    /// pack (so the bestiary becomes a food web and the world has wells of power).
    pub fn new(pack: &ContentPack) -> Self {
        let mut w = LivingWorld { soul: WorldSoul::new(LIVING_SEED), attunements: HashMap::new() };
        w.seed_from_content(pack);
        w
    }

    /// Populate the leyline network and ecology from content. Idempotent-ish: only call
    /// once at construction (or after a full reset).
    pub fn seed_from_content(&mut self, pack: &ContentPack) {
        // --- leyline wells: a handful of wells of power, elements cycling, ringed
        //     around the origin so the home region has real magical geography. ---
        let elements = ["fire", "frost", "arcane", "nature", "shadow", "storm", "radiant"];
        let ring = 120.0;
        let n = elements.len();
        for (i, el) in elements.iter().enumerate() {
            let a = (i as f32 / n as f32) * std::f32::consts::TAU;
            let pos = Vec3::new(a.cos() * ring, 0.0, a.sin() * ring);
            let idx = self.soul.leylines.add_node(LeylineNode::new(
                format!("well.{el}"),
                pos,
                1200.0,
                *el,
            ));
            // Link each well to the previous to form a conductive ring.
            if idx > 0 {
                self.soul.leylines.connect(idx - 1, idx, 0.6);
            }
        }
        // Close the ring.
        if n >= 2 {
            self.soul.leylines.connect(n - 1, 0, 0.6);
        }

        // --- ecology: every mob in the pack becomes a species; trophic level and
        //     carrying capacity are heuristics off its xp_reward (a proxy for menace). ---
        let grazers: Vec<String> = pack
            .mobs
            .iter()
            .filter(|m| m.xp_reward < 30)
            .map(|m| m.id.as_str().to_string())
            .collect();
        for m in &pack.mobs {
            let (trophic, cap, growth) = if m.xp_reward < 30 {
                (Trophic::Grazer, 120.0, 0.5)
            } else if m.xp_reward < 120 {
                (Trophic::Predator, 50.0, 0.4)
            } else {
                (Trophic::Apex, 16.0, 0.25)
            };
            let prey = if matches!(trophic, Trophic::Grazer) { Vec::new() } else { grazers.clone() };
            self.soul.ecology.add(Species {
                mob: m.id.as_str().to_string(),
                trophic,
                prey,
                carrying_capacity: cap,
                growth,
                population: cap * 0.4,
            });
        }
    }

    /// Advance the world soul to `world_tick`. Returns the pulse (season turns,
    /// festivals, ecology shifts, legends) for the sim to surface and act on.
    pub fn advance(&mut self, world_tick: u64) -> WorldPulse {
        self.soul.advance(world_tick)
    }

    /// The spell-power multiplier for a spell of `element`, cast at `pos` by the player
    /// whose CE node id is `owner`. Folds the world's `spell_power_at` with the caster's
    /// personal attunement affinity. Owner may be empty (a mob/summon) — then only the
    /// world's ambient multiplier applies.
    pub fn spell_power(&self, element: &str, pos: Vec3, owner: &str) -> f32 {
        let mut m = self.soul.spell_power_at(element, pos);
        if !owner.is_empty() {
            if let Some(att) = self.attunements.get(owner) {
                m *= att.affinity_mult(element);
            }
        }
        m
    }

    /// A cast happened: deepen the caster's attunement to its school (dark schools also
    /// breed corruption). No-op for unowned casters.
    pub fn on_cast(&mut self, owner: &str, element: &str, intensity: f32) {
        if owner.is_empty() {
            return;
        }
        self.attunements.entry(owner.to_string()).or_default().practise(element, intensity);
    }

    /// A kill happened: feed a Slay deed to the Chronicle. Returns any legends born (so
    /// the sim can herald them) — the constellations are already hung and any
    /// world-imprints queued inside the soul.
    pub fn on_slay(
        &mut self,
        killer_owner: &str,
        killer_name: &str,
        victim_name: &str,
        pos: Vec3,
        tick: u64,
        magnitude: f32,
    ) -> Vec<Legend> {
        if killer_owner.is_empty() {
            return Vec::new(); // a beast killing a player earns no renown
        }
        let victim_renown = self.soul.chronicle.renown_of(victim_name).total();
        let deed = Deed {
            actor: killer_owner.to_string(),
            actor_name: killer_name.to_string(),
            kind: DeedKind::Slay { victim: victim_name.to_string(), victim_renown },
            location: pos,
            tick,
            magnitude,
        };
        self.soul.record_deed(&deed)
    }

    /// Drain the world-imprints the Chronicle queued (relic sites, haunts, rising named
    /// foes) for the sim to realise this tick.
    pub fn take_pending(&mut self) -> Vec<Manifestation> {
        self.soul.take_pending()
    }

    /// The mob spawn-weight the ecology currently assigns (a bloomed species floods,
    /// a thinned one becomes rare). The spawn system multiplies its rules by this.
    pub fn spawn_weight(&self, mob: &str) -> f32 {
        self.soul.ecology.spawn_weight(mob)
    }

    /// A player's current honorific from the Chronicle (e.g. "Champion"), if any.
    pub fn title_of(&self, owner: &str) -> Option<String> {
        self.soul.chronicle.title_of(owner)
    }

    /// A player's attunement mark (e.g. "Fire-Touched", "the Corrupted"), if any.
    pub fn mark_of(&self, owner: &str) -> Option<String> {
        self.attunements.get(owner).and_then(|a| a.mark())
    }

    /// Convenience: was this ecology event a population mutation? (The sim turns those
    /// into rising named foes via [`take_pending`].)
    pub fn is_mutation(ev: &EcologyEvent) -> bool {
        matches!(ev, EcologyEvent::Mutation { .. })
    }
}
