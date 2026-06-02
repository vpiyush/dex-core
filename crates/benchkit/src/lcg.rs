//! Deterministic linear-congruential RNG for reproducible benchmark workloads.
//!
//! No `rand` dependency: benchmarks must be byte-for-byte reproducible across
//! runs, and a seeded LCG is sufficient for picking sides/prices/indices. Uses
//! the Knuth/Numerical-Recipes constants historically inlined in the matcher and
//! orderbook bench files, so ported scenarios reproduce their prior sequences.

/// A 64-bit linear-congruential generator. Deterministic for a given seed.
#[derive(Clone)]
pub struct Lcg(u64);

impl Lcg {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next pseudo-random `u64`. Advances the state.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }

    /// Uniform-ish value in `[0, n)`. `n == 0` returns 0.
    #[inline]
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            // Use the high bits (better distributed than the low bits of an LCG).
            (self.next_u64() >> 1) % n
        }
    }

    /// In-place Fisher-Yates shuffle. Deterministic for a given seed; the same
    /// algorithm the orderbook bench used so cancel-order patterns line up.
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        let n = slice.len();
        for i in (1..n).rev() {
            let j = (self.next_u64() >> 33) as usize % (i + 1);
            slice.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Lcg::new(42);
        let mut b = Lcg::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seed_diverges() {
        let mut a = Lcg::new(1);
        let mut b = Lcg::new(2);
        // Overwhelmingly likely to differ within a few draws.
        let differ = (0..8).any(|_| a.next_u64() != b.next_u64());
        assert!(differ);
    }

    #[test]
    fn below_is_in_range() {
        let mut r = Lcg::new(7);
        for _ in 0..10_000 {
            assert!(r.below(50) < 50);
        }
        assert_eq!(r.below(0), 0);
    }

    #[test]
    fn shuffle_is_a_permutation() {
        let mut v: Vec<u32> = (0..256).collect();
        let original: Vec<u32> = v.clone();
        Lcg::new(0xCAFE_F00D).shuffle(&mut v);
        // Same multiset (nothing lost/duplicated)…
        let mut sorted = v.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, original);
        // …and actually reordered (vanishingly unlikely to be identity at n=256).
        assert_ne!(v, original);
    }

    #[test]
    fn shuffle_is_deterministic() {
        let mut v1: Vec<u32> = (0..100).collect();
        let mut v2: Vec<u32> = (0..100).collect();
        Lcg::new(99).shuffle(&mut v1);
        Lcg::new(99).shuffle(&mut v2);
        assert_eq!(v1, v2);
    }
}
