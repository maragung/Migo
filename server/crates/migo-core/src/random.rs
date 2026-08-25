//! Injectable randomness.
//!
//! Two implementations, and the choice between them is a security decision:
//!
//! * [`OsRandom`] is the only acceptable source for anything that must be
//!   unguessable — keys, nonces, tokens, salts.
//! * [`SeededRandom`] is a fast, reproducible PRNG for tests, simulation
//!   (ADR-0009), jitter, and load generation. It is **not** cryptographic and
//!   is documented as such at every use site.
//!
//! The trait is object-safe so components can hold a `&mut dyn Random` and be
//! driven by either one without generics leaking through every signature.

use rand::RngCore;

/// A source of random bytes.
pub trait Random: Send {
    /// Fills the buffer.
    fn fill_bytes(&mut self, dest: &mut [u8]);

    /// Next 64 random bits.
    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        self.fill_bytes(&mut buf);
        u64::from_le_bytes(buf)
    }

    /// A value in `0..bound`, or `0` when `bound` is zero.
    ///
    /// Uses Lemire's multiply-shift rejection method so the result is unbiased
    /// even when `bound` is not a power of two.
    fn below(&mut self, bound: u64) -> u64 {
        if bound <= 1 {
            return 0;
        }
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let candidate = self.next_u64();
            if candidate >= threshold {
                return candidate % bound;
            }
        }
    }

    /// True with probability `numerator / denominator`. Used by the simulator's
    /// fault injection, where "drop 3% of frames" must be expressible exactly.
    fn chance(&mut self, numerator: u32, denominator: u32) -> bool {
        if denominator == 0 || numerator == 0 {
            return false;
        }
        if numerator >= denominator {
            return true;
        }
        self.below(u64::from(denominator)) < u64::from(numerator)
    }
}

impl<T: Random + ?Sized> Random for &mut T {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        (**self).fill_bytes(dest);
    }

    fn next_u64(&mut self) -> u64 {
        (**self).next_u64()
    }
}

/// The operating system CSPRNG. Use this for every secret.
#[derive(Clone, Copy, Debug, Default)]
pub struct OsRandom;

impl OsRandom {
    /// Returns a fresh array of random bytes.
    #[must_use]
    pub fn array<const N: usize>() -> [u8; N] {
        let mut out = [0u8; N];
        rand::rngs::OsRng.fill_bytes(&mut out);
        out
    }
}

impl Random for OsRandom {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // OsRng panics only when the OS entropy source is unavailable, which is
        // an unrecoverable environment failure rather than a runtime error.
        rand::rngs::OsRng.fill_bytes(dest);
    }
}

/// xoshiro256** — reproducible, fast, and **not** cryptographic.
///
/// Named after the algorithm rather than "TestRandom" so that a reviewer
/// grepping for cryptographic misuse sees immediately what it is.
#[derive(Clone, Debug)]
pub struct SeededRandom {
    state: [u64; 4],
}

impl SeededRandom {
    /// Seeds deterministically from a single integer. Equal seeds produce equal
    /// streams on every platform and every build.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        // SplitMix64 expansion: xoshiro behaves badly if seeded with mostly zeros.
        let mut x = seed;
        let mut state = [0u64; 4];
        for slot in &mut state {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            *slot = z ^ (z >> 31);
        }
        Self { state }
    }

    /// Reads the seed from `SIM_SEED`, defaulting to `1234` so that a plain
    /// `cargo test` is still deterministic.
    #[must_use]
    pub fn from_env() -> Self {
        let seed = std::env::var("SIM_SEED")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(1234);
        Self::new(seed)
    }

    fn next(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }
}

impl Random for SeededRandom {
    fn fill_bytes(&mut self, dest: &mut [u8]) {
        // Eight bytes at a time from successive draws, then the shorter tail from one more draw.
        // The draw count is unchanged, so seeded sequences are too.
        let (blocks, tail) = dest.as_chunks_mut::<8>();
        for block in blocks {
            *block = self.next().to_le_bytes();
        }
        if !tail.is_empty() {
            let bytes = self.next().to_le_bytes();
            tail.copy_from_slice(&bytes[..tail.len()]);
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_seeds_produce_equal_streams() {
        let mut a = SeededRandom::new(42);
        let mut b = SeededRandom::new(42);
        for _ in 0..64 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = SeededRandom::new(1);
        let mut b = SeededRandom::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn fill_bytes_handles_unaligned_lengths() {
        let mut rng = SeededRandom::new(5);
        for len in [0usize, 1, 7, 8, 9, 63, 64, 65] {
            let mut buf = vec![0u8; len];
            rng.fill_bytes(&mut buf);
            assert_eq!(buf.len(), len);
        }
    }

    #[test]
    fn below_stays_in_range() {
        let mut rng = SeededRandom::new(9);
        for bound in [1u64, 2, 3, 10, 1000] {
            for _ in 0..500 {
                assert!(rng.below(bound) < bound.max(1));
            }
        }
        assert_eq!(rng.below(0), 0);
    }

    #[test]
    fn chance_respects_its_bounds() {
        let mut rng = SeededRandom::new(13);
        assert!(!rng.chance(0, 100));
        assert!(rng.chance(100, 100));
        let hits = (0..10_000).filter(|_| rng.chance(3, 100)).count();
        // 3% of 10k is 300; allow generous slack so the test never flakes.
        assert!((150..500).contains(&hits), "hits = {hits}");
    }

    #[test]
    fn os_random_produces_distinct_output() {
        let a: [u8; 32] = OsRandom::array();
        let b: [u8; 32] = OsRandom::array();
        assert_ne!(a, b);
        assert_ne!(a, [0u8; 32]);
    }
}
