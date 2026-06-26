//! A tiny deterministic PRNG so the living world is *reproducible* across every node.
//!
//! The whole point of Cerena's mesh: thousands of authorities must agree on what the
//! world did without shipping the world between them. So nothing in `arena-mythos`
//! ever reads a clock or a thread RNG — every "roll of the dice" is hashed from the
//! world tick plus some salt. Feed two machines the same seed and the same tick and
//! the same legend is born, the same star ignites, the same storm rolls in.

/// splitmix64 — fast, well-mixed, fully deterministic. Public-domain algorithm.
#[derive(Debug, Clone, Copy)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seed the stream. Mix the seed once so adjacent seeds don't correlate.
    pub fn new(seed: u64) -> Self {
        Self { state: seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03 }
    }

    /// Seed from a world tick + a string salt (an entity id, a place name). This is how
    /// every system derives "randomness" tied to *where and when* it happened.
    pub fn from_tick(tick: u64, salt: &str) -> Self {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ tick;
        for b in salt.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        Self::new(h)
    }

    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform float in `[0, 1)`.
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform float in `[lo, hi)`.
    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + self.unit() * (hi - lo)
    }

    /// Uniform integer in `[0, n)`.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }

    /// True with probability `p`.
    pub fn chance(&mut self, p: f32) -> bool {
        self.unit() < p
    }

    /// Pick one of `slice` (by reference), or `None` if empty.
    pub fn pick<'a, T>(&mut self, slice: &'a [T]) -> Option<&'a T> {
        if slice.is_empty() {
            None
        } else {
            Some(&slice[(self.below(slice.len() as u64)) as usize])
        }
    }
}
