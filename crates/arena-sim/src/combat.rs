//! Combat primitives shared across the sim: team/faction damage rules, the
//! armour/health damage split, and the deterministic integer hashing used wherever
//! the sim needs "randomness" without an RNG (`Chance` ops, spread).
//!
//! In the FPS prototype this module owned weapon firing; in Cerena that role moved
//! to [`crate::magic`] (player fire casts a spell). What remains here are the small,
//! pure, reusable building blocks the spell interpreter and the world tick lean on.

use arena_content::ids::ElementId;
use arena_protocol::world::Team;

/// True if a source on `src` may damage a target on `dst`. Free-for-all
/// ([`Team::None`] on either side) means everyone is fair game.
pub fn can_damage(src: Team, dst: Team) -> bool {
    src == Team::None || dst == Team::None || src != dst
}

/// Subtract `damage` from a target, splitting it across armour then health. Armour
/// soaks half of each hit until depleted; the remainder (and everything once armour
/// is gone) bites health. Rounded to match the `i16` wire fields.
pub fn apply_armor_damage(damage: f32, health: &mut i16, armor: &mut i16) {
    let to_armor = (damage * 0.5).min(*armor as f32).max(0.0);
    let to_health = damage - to_armor;
    *armor = (*armor as f32 - to_armor).round().max(0.0) as i16;
    *health = (*health as f32 - to_health).round() as i16;
}

/// SplitMix64: a tiny, well-distributed, platform-independent integer hash. The
/// basis of every deterministic "roll" in the sim.
pub fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Map a 64-bit hash to an `f32` in `[0, 1)` (top 24 bits → f32 mantissa).
pub fn unit_f32(h: u64) -> f32 {
    ((h >> 40) as f32) / (1u64 << 24) as f32
}

/// Combine a tick, an entity id and a salt into a deterministic seed. Used so a
/// roll (chance op, spread) replays identically on client and server.
pub fn hash_seed(tick: u32, entity: u32, salt: u64) -> u64 {
    let base = ((tick as u64) << 32) ^ (entity as u64);
    splitmix64(base ^ salt.rotate_left(17))
}

/// Hash a string id (element, spell) into a salt, so distinct ids roll independently.
pub fn str_salt(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Salt derived from an element id.
pub fn element_salt(e: &ElementId) -> u64 {
    str_salt(&e.0)
}
