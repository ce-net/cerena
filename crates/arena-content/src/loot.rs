//! Loot tables — named, weighted drop pools, as hot-reloadable data.
//!
//! Mobs, missions, and death-drops all reference a [`LootTableDef`] by
//! [`crate::ids::LootTableId`] instead of carrying an inline drop list. That gives
//! the designer one place to retune the whole economy live ("the boss table feels
//! stingy") and lets several sources share a table. Drops are the backbone of the
//! "kill -> items spill out" mechanic.
//!
//! ## Determinism
//!
//! [`LootTableDef::roll`] takes an explicit `seed` and uses a tiny inline splitmix64
//! generator. There is **no wall-clock time and no global/thread RNG** anywhere in
//! this crate — the authority feeds in a per-event deterministic seed (e.g. derived
//! from the kill's tick + entity ids) so every node that replays the event computes
//! the *same* drops. That is mandatory for a lockstep-ish 10k-player simulation: loot
//! must be reproducible, not a local dice roll.

use serde::{Deserialize, Serialize};

use crate::ids::{ItemId, LootTableId};

/// One weighted entry in a loot table. On a successful roll it yields a random count
/// in `[min, max]` of `item`. `weight` is relative within the table (not a 0..1
/// probability), so adding a rare entry does not require re-normalizing the others.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LootEntry {
    pub item: ItemId,
    /// Relative selection weight. Higher = more likely. Must be > 0 to ever drop.
    pub weight: f32,
    /// Minimum quantity granted when this entry is selected.
    pub min: u16,
    /// Maximum quantity granted when this entry is selected.
    pub max: u16,
    /// Extra weight granted as the killer's "magic find" / level scaling rises; the
    /// sim folds this in before rolling so rarer items skew toward higher rolls.
    pub rarity_bonus: f32,
}

/// A named loot pool. `rolls` independent selections are made; each can hit a
/// different entry, so one table can drop several stacks at once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LootTableDef {
    pub id: LootTableId,
    pub name: String,
    pub entries: Vec<LootEntry>,
    /// How many independent weighted selections to perform per invocation.
    pub rolls: u8,
    /// Per-level multiplier applied to dropped quantities, so the same table scales
    /// from the lowlands to the deep hollows without a second table.
    pub level_scaling: f32,
}

impl LootTableDef {
    /// Roll this table deterministically. Returns the `(item, count)` drops for one
    /// kill/mission/death event. Same `seed` + same table = same result on every
    /// node. NO time, NO global rng — purely a function of the inputs.
    ///
    /// `weight` and `rarity_bonus` are summed per entry to pick a winner; counts are
    /// drawn uniformly in `[min, max]`. `level_scaling` is intentionally *not* applied
    /// here (the sim multiplies by the actual victim/killer level it knows about) so
    /// this stays a pure, side-effect-free selection.
    pub fn roll(&self, seed: u64) -> Vec<(ItemId, u16)> {
        let mut out: Vec<(ItemId, u16)> = Vec::new();
        if self.entries.is_empty() {
            return out;
        }
        // Effective, non-negative weight of an entry.
        let eff = |e: &LootEntry| (e.weight + e.rarity_bonus).max(0.0);
        let total: f32 = self.entries.iter().map(eff).sum();
        if total <= 0.0 {
            return out;
        }

        let mut state = seed;
        for _ in 0..self.rolls {
            // One uniform in [0, total) selects the entry.
            let pick = next_unit(&mut state) * total;
            let mut acc = 0.0;
            for e in &self.entries {
                acc += eff(e);
                if pick < acc {
                    // Uniform count in [min, max].
                    let span = e.max.saturating_sub(e.min);
                    let extra = if span == 0 {
                        0
                    } else {
                        // +1 so `max` is inclusive.
                        (next_u64(&mut state) % (span as u64 + 1)) as u16
                    };
                    let count = e.min.saturating_add(extra);
                    if count > 0 {
                        out.push((e.item.clone(), count));
                    }
                    break;
                }
            }
        }
        out
    }
}

/// splitmix64: a tiny, fast, fully-deterministic PRNG. Advances `state` and returns a
/// well-mixed 64-bit value. Inline here so the crate pulls in no rng dependency and
/// stays wasm-clean. Reference algorithm (public domain).
fn next_u64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E3779B97F4A7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^ (z >> 31)
}

/// A deterministic uniform float in `[0, 1)` drawn from the splitmix64 stream.
fn next_unit(state: &mut u64) -> f32 {
    // Use the top 24 bits for an exactly-representable f32 mantissa fraction.
    let bits = next_u64(state) >> 40; // 24 bits
    (bits as f32) / ((1u32 << 24) as f32)
}
