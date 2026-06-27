//! The forge: how item instances are *rolled* on drop and *changed* by the player —
//! upgrading (+N), reforging (reroll affixes), socketing/unsocketing gems, imprinting
//! (locking) an affix, applying an enchant, and fusing gems up a tier.
//!
//! Everything here is **deterministic** given its seed, so an authority and its shadow
//! cross-validators (`arena-karma`) roll the *same* drop and the *same* forge outcome
//! from the same inputs. The RNG is a small inline splitmix64 — no external dep, no
//! hidden global state — seeded from `(world_seed, actor, instance, action_nonce)` so a
//! player can't re-roll the same upgrade by retrying.
//!
//! Forge actions consume reagents from the caller's [`crate::inventory::Inventory`]; the
//! cost tables live in the hot-reloadable [`arena_content::forge::ForgeConfig`].

use arena_content::ids::{EnchantId, GemId, ItemId};
use arena_content::item::{EquipSlot, Rarity};
use arena_content::registry::ContentRegistry;

use crate::inventory::Inventory;
use crate::item_instance::{InstanceId, ItemInstance, RolledAffix, SocketFill};

/// A tiny deterministic RNG (splitmix64). Reproducible across nodes and across the
/// wasm/native split, which the `glam` f32 sim deliberately is not — forge rolls must
/// agree exactly, so they use integer math.
#[derive(Clone)]
pub struct ForgeRng {
    state: u64,
}

impl ForgeRng {
    /// Seed from any mix of inputs (world seed, actor key bytes, instance id, nonce).
    pub fn seed(parts: &[u64]) -> Self {
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        for &p in parts {
            s ^= p.wrapping_add(0x9E37_79B9_7F4A_7C15);
            s = s.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            s ^= s >> 27;
        }
        Self { state: s | 1 }
    }
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    /// Uniform in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / ((1u64 << 24) as f32)
    }
    /// Uniform integer in `[0, n)` (n > 0).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
    /// Roll a `t in 0..1` biased toward 1.0 by `bias` (magic-find): the max of
    /// `1 + bias_rolls` uniforms, so better gear rolls fatter.
    pub fn biased_t(&mut self, bias_rolls: u32) -> f32 {
        let mut t = self.unit();
        for _ in 0..bias_rolls {
            t = t.max(self.unit());
        }
        t
    }
}

/// The outcome of a forge action that can fail.
#[derive(Debug, Clone, PartialEq)]
pub enum ForgeOutcome {
    /// The action succeeded.
    Success,
    /// An upgrade attempt failed but the item survived (essence burned).
    UpgradeFailed,
    /// An upgrade attempt failed and shaved a level.
    UpgradeDowngraded,
    /// The caller lacked the reagents / level / sockets.
    Rejected(&'static str),
}

/// Roll a fresh dropped instance of `base`, honouring `magic_find` (0.. ; shifts rarity
/// and affix rolls upward) and `ilvl` (the monster/zone level that gates affix tiers).
/// Deterministic in `seed`.
pub fn roll_drop(
    content: &ContentRegistry,
    base: &ItemId,
    id: InstanceId,
    magic_find: f32,
    ilvl: u32,
    seed: u64,
) -> ItemInstance {
    let mut rng = ForgeRng::seed(&[seed, id.0, ilvl as u64]);
    let mut inst = ItemInstance::plain(id, base, content);
    let Some(def) = content.item(base) else {
        return inst;
    };
    let forge = content.forge_config();

    // Unique/mythic bases keep their fixed rarity & signature; they only roll sockets.
    if !def.unique {
        // Rarity: start at the base rarity, roll magic-find "promotion" chances upward.
        let promote_rolls = (magic_find * forge.mf_to_promote).floor() as u32
            + if rng.unit() < (magic_find * forge.mf_to_promote).fract() { 1 } else { 0 };
        let mut rarity = def.rarity;
        for _ in 0..promote_rolls {
            // Each promotion is a coin-flip so magic-find raises the *ceiling*, not a
            // guarantee — chase, not entitlement.
            if rng.unit() < 0.5 {
                rarity = rarity.promote();
            }
        }
        inst.rarity = rarity;

        // Affixes: draw up to the rarity budget from the eligible pool, no dupes.
        let budget = rarity.affix_budget() as usize;
        let tier_bias = (magic_find * forge.mf_to_tier) as u32;
        let pool = content.affix_pool(def.slot, &def.affix_tags, ilvl);
        let mut used: Vec<&str> = Vec::new();
        let total_weight: f32 = pool.iter().map(|a| a.weight).sum();
        for _ in 0..budget {
            if pool.is_empty() || total_weight <= 0.0 {
                break;
            }
            // Weighted pick.
            let mut pick = rng.unit() * total_weight;
            let mut chosen = None;
            for a in &pool {
                pick -= a.weight;
                if pick <= 0.0 {
                    chosen = Some(*a);
                    break;
                }
            }
            let Some(a) = chosen.or_else(|| pool.last().copied()) else { break };
            if used.contains(&a.id.as_str()) {
                continue; // skip a dupe this round; budget may underfill, which is fine
            }
            used.push(a.id.as_str());
            inst.affixes.push(RolledAffix {
                affix: a.id.clone(),
                roll_t: rng.biased_t(tier_bias),
                imprinted: false,
            });
        }
    }

    // Sockets: between the base count and the rarity budget.
    let socket_budget = inst.rarity.socket_budget() as usize;
    let min_sockets = def.base_sockets as usize;
    let extra = if socket_budget > min_sockets {
        rng.below(socket_budget - min_sockets + 1)
    } else {
        0
    };
    let total_sockets = (min_sockets + extra).min(socket_budget.max(min_sockets));
    inst.sockets = vec![SocketFill::Empty; total_sockets];

    // Quality: a small starting roll, fatter with magic-find.
    inst.quality = (rng.biased_t(tier_bias_for(magic_find, forge.mf_to_tier)) * 40.0) as u8;

    inst.bound_to = if def.soulbound { Some([0u8; 32]) } else { None };
    inst
}

fn tier_bias_for(mf: f32, k: f32) -> u32 {
    (mf * k) as u32
}

/// Attempt to raise an instance's upgrade level by one, consuming essence from `bag`.
/// Honours the item's [`arena_content::item::UpgradeProfile`] and the global
/// [`arena_content::forge::ForgeConfig`]. Deterministic in `seed`.
pub fn upgrade(
    content: &ContentRegistry,
    bag: &mut Inventory,
    inst: &mut ItemInstance,
    seed: u64,
) -> ForgeOutcome {
    let Some(def) = content.item(&inst.base) else {
        return ForgeOutcome::Rejected("unknown base");
    };
    let Some(profile) = def.upgrade.clone() else {
        return ForgeOutcome::Rejected("item cannot be forged");
    };
    if inst.upgrade_level >= profile.max_level {
        return ForgeOutcome::Rejected("already max level");
    }
    let forge = content.forge_config();

    // Cost climbs with the current level.
    let cost = (profile.essence_cost_base as f32
        * (1.0 + inst.upgrade_level as f32 * forge.upgrade_cost_growth))
        .ceil() as u16;
    if bag.count(&profile.essence) < cost {
        return ForgeOutcome::Rejected("not enough essence");
    }
    bag.remove_item(&profile.essence, cost);

    // Safe levels never fail.
    if inst.upgrade_level < profile.safe_until {
        inst.upgrade_level += 1;
        return ForgeOutcome::Success;
    }

    let mut rng = ForgeRng::seed(&[seed, inst.id.0, inst.upgrade_level as u64]);
    let fail_chance = profile.fail_chance_per_level
        * (inst.upgrade_level + 1 - profile.safe_until) as f32;
    if rng.unit() < fail_chance {
        // Failure: maybe shave a level.
        if rng.unit() < forge.downgrade_on_fail && inst.upgrade_level > profile.safe_until {
            inst.upgrade_level -= 1;
            ForgeOutcome::UpgradeDowngraded
        } else {
            ForgeOutcome::UpgradeFailed
        }
    } else {
        inst.upgrade_level += 1;
        ForgeOutcome::Success
    }
}

/// Reforge: reroll every non-imprinted affix. Imprinted affixes are preserved; one
/// imprint is cleared per reforge (it is "spent" keeping that affix this time).
pub fn reforge(
    content: &ContentRegistry,
    bag: &mut Inventory,
    inst: &mut ItemInstance,
    ilvl: u32,
    magic_find: f32,
    seed: u64,
) -> ForgeOutcome {
    let Some(def) = content.item(&inst.base) else {
        return ForgeOutcome::Rejected("unknown base");
    };
    if def.unique {
        return ForgeOutcome::Rejected("unique items have no random affixes");
    }
    let forge = content.forge_config();
    let (reagent, qty) = forge.reforge_cost.clone();
    if bag.count(&reagent) < qty {
        return ForgeOutcome::Rejected("not enough chaos shards");
    }
    bag.remove_item(&reagent, qty);

    let kept: Vec<RolledAffix> = inst
        .affixes
        .iter()
        .filter(|a| a.imprinted)
        .cloned()
        .map(|mut a| {
            a.imprinted = false; // imprint is consumed by surviving this reforge
            a
        })
        .collect();

    let mut rng = ForgeRng::seed(&[seed, inst.id.0, 0x12EF_0000_u64.wrapping_add(inst.upgrade_level as u64)]);
    let budget = inst.rarity.affix_budget() as usize;
    let tier_bias = (magic_find * forge.mf_to_tier) as u32;
    let pool = content.affix_pool(def.slot, &def.affix_tags, ilvl);
    let mut new_affixes = kept;
    let mut used: Vec<String> = new_affixes.iter().map(|a| a.affix.0.clone()).collect();
    let total_weight: f32 = pool.iter().map(|a| a.weight).sum();
    while new_affixes.len() < budget && !pool.is_empty() && total_weight > 0.0 {
        let mut pick = rng.unit() * total_weight;
        let mut chosen = None;
        for a in &pool {
            pick -= a.weight;
            if pick <= 0.0 {
                chosen = Some(*a);
                break;
            }
        }
        let Some(a) = chosen.or_else(|| pool.last().copied()) else { break };
        if used.contains(&a.id.0) {
            continue;
        }
        used.push(a.id.0.clone());
        new_affixes.push(RolledAffix {
            affix: a.id.clone(),
            roll_t: rng.biased_t(tier_bias),
            imprinted: false,
        });
    }
    inst.affixes = new_affixes;
    ForgeOutcome::Success
}

/// Imprint (lock) the affix at `index` so the next reforge keeps it.
pub fn imprint(
    content: &ContentRegistry,
    bag: &mut Inventory,
    inst: &mut ItemInstance,
    index: usize,
) -> ForgeOutcome {
    if index >= inst.affixes.len() {
        return ForgeOutcome::Rejected("no such affix");
    }
    let (reagent, qty) = content.forge_config().imprint_cost.clone();
    if bag.count(&reagent) < qty {
        return ForgeOutcome::Rejected("not enough binding sigils");
    }
    bag.remove_item(&reagent, qty);
    inst.affixes[index].imprinted = true;
    ForgeOutcome::Success
}

/// Bore an additional empty socket, up to the rarity's socket budget.
pub fn add_socket(
    content: &ContentRegistry,
    bag: &mut Inventory,
    inst: &mut ItemInstance,
) -> ForgeOutcome {
    if inst.sockets.len() >= inst.rarity.socket_budget() as usize {
        return ForgeOutcome::Rejected("socket budget full");
    }
    let (reagent, qty) = content.forge_config().socket_cost.clone();
    if bag.count(&reagent) < qty {
        return ForgeOutcome::Rejected("not enough drills");
    }
    bag.remove_item(&reagent, qty);
    inst.sockets.push(SocketFill::Empty);
    ForgeOutcome::Success
}

/// Socket `gem` into the first empty socket (the gem must be carried; it is consumed).
pub fn socket_gem(
    content: &ContentRegistry,
    bag: &mut Inventory,
    inst: &mut ItemInstance,
    gem: &GemId,
) -> ForgeOutcome {
    let gem_item = ItemId::new(gem.as_str()); // gems are also carried as items by id
    if bag.count(&gem_item) == 0 {
        return ForgeOutcome::Rejected("gem not carried");
    }
    let Some(slot) = inst.sockets.iter_mut().find(|s| matches!(s, SocketFill::Empty)) else {
        return ForgeOutcome::Rejected("no empty socket");
    };
    let _ = content; // gem validity is the caller's concern; unknown gems just do nothing
    *slot = SocketFill::Gem(gem.clone());
    bag.remove_item(&gem_item, 1);
    ForgeOutcome::Success
}

/// Pop the gem at `index` back into the bag intact (costs a solvent).
pub fn unsocket(
    content: &ContentRegistry,
    bag: &mut Inventory,
    inst: &mut ItemInstance,
    index: usize,
) -> ForgeOutcome {
    let Some(SocketFill::Gem(g)) = inst.sockets.get(index).cloned() else {
        return ForgeOutcome::Rejected("nothing socketed there");
    };
    let (reagent, qty) = content.forge_config().unsocket_cost.clone();
    if bag.count(&reagent) < qty {
        return ForgeOutcome::Rejected("not enough solvent");
    }
    bag.remove_item(&reagent, qty);
    inst.sockets[index] = SocketFill::Empty;
    bag.add_item(ItemId::new(g.as_str()), 1);
    ForgeOutcome::Success
}

/// Apply a permanent enchant (one per item; replaces any prior).
pub fn apply_enchant(
    content: &ContentRegistry,
    inst: &mut ItemInstance,
    enchant: &EnchantId,
) -> ForgeOutcome {
    let Some(ed) = content.enchant(enchant) else {
        return ForgeOutcome::Rejected("unknown enchant");
    };
    if let Some(slot) = inst.slot(content) {
        if !ed.slots.is_empty() && !ed.slots.contains(&slot) {
            return ForgeOutcome::Rejected("enchant not valid on this slot");
        }
    }
    inst.enchant = Some(enchant.clone());
    ForgeOutcome::Success
}

/// Polish: raise quality toward the cap, consuming a whetstone. Quality never resets.
pub fn polish(
    content: &ContentRegistry,
    bag: &mut Inventory,
    inst: &mut ItemInstance,
    seed: u64,
) -> ForgeOutcome {
    let forge = content.forge_config();
    if inst.quality >= forge.quality_cap {
        return ForgeOutcome::Rejected("already flawless");
    }
    let stone = ItemId::new("item.essence.polish");
    if bag.count(&stone) == 0 {
        return ForgeOutcome::Rejected("no whetstone");
    }
    bag.remove_item(&stone, 1);
    let mut rng = ForgeRng::seed(&[seed, inst.id.0, inst.quality as u64]);
    let gain = 3 + rng.below(6) as u8; // +3..+8 quality
    inst.quality = (inst.quality + gain).min(forge.quality_cap);
    ForgeOutcome::Success
}

/// Fuse lower-tier gems into one of `target` per its `fuse_from` recipe.
pub fn fuse_gems(
    content: &ContentRegistry,
    bag: &mut Inventory,
    target: &GemId,
) -> ForgeOutcome {
    let Some(gd) = content.gem(target) else {
        return ForgeOutcome::Rejected("unknown gem");
    };
    let Some((lower, count)) = gd.fuse_from.clone() else {
        return ForgeOutcome::Rejected("gem does not fuse");
    };
    let lower_item = ItemId::new(lower.as_str());
    if bag.count(&lower_item) < count as u16 {
        return ForgeOutcome::Rejected("not enough lower gems");
    }
    bag.remove_item(&lower_item, count as u16);
    bag.add_item(ItemId::new(target.as_str()), 1);
    ForgeOutcome::Success
}

/// Whether `slot` can accept the gem family at all (a convenience for UI gating).
pub fn slot_accepts_gems(slot: EquipSlot) -> bool {
    slot != EquipSlot::Consumable && slot != EquipSlot::None
}

/// Convenience for tests/tools: roll a guaranteed-rarity instance (skips magic-find
/// promotion rolls but still rolls affixes/sockets at that tier).
pub fn roll_at_rarity(
    content: &ContentRegistry,
    base: &ItemId,
    id: InstanceId,
    rarity: Rarity,
    ilvl: u32,
    seed: u64,
) -> ItemInstance {
    let mut inst = roll_drop(content, base, id, 0.0, ilvl, seed);
    if !content.item(base).map(|d| d.unique).unwrap_or(false) {
        inst.rarity = rarity;
    }
    inst
}
