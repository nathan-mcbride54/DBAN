//! xoshiro256++ pseudo-random generator.
//!
//! Chosen over the `rand` stack on purpose:
//! * extremely fast byte-stream generation (the wipe engine is I/O bound,
//!   never PRNG bound),
//! * fully deterministic from a 64-bit seed, which lets the verify phase
//!   regenerate the exact stream that was written instead of buffering it,
//! * tiny, auditable implementation with no dependency surface.
//!
//! Seeds come from the operating system CSPRNG via `getrandom`.

/// splitmix64 — used to expand a 64-bit seed into xoshiro's 256-bit state.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[derive(Clone, Debug)]
pub struct Prng {
    s: [u64; 4],
}

impl Prng {
    /// Deterministic construction. The same seed always yields the same stream;
    /// the engine relies on this to verify random passes without storing them.
    pub fn from_seed(seed: u64) -> Self {
        let mut sm = seed;
        let mut s = [0u64; 4];
        for slot in &mut s {
            *slot = splitmix64(&mut sm);
        }
        // xoshiro must never be seeded with all-zero state.
        if s == [0u64; 4] {
            s[0] = 0x1;
        }
        Prng { s }
    }

    /// Construct from OS entropy, returning the seed so the caller can
    /// reproduce the stream later (verification, reporting).
    pub fn fresh() -> (Self, u64) {
        let mut bytes = [0u8; 8];
        getrandom::getrandom(&mut bytes).expect("operating system entropy unavailable");
        let seed = u64::from_le_bytes(bytes);
        (Self::from_seed(seed), seed)
    }

    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let s = &mut self.s;
        let result = s[0].wrapping_add(s[3]).rotate_left(23).wrapping_add(s[0]);
        let t = s[1] << 17;
        s[2] ^= s[0];
        s[3] ^= s[1];
        s[1] ^= s[2];
        s[0] ^= s[3];
        s[2] ^= t;
        s[3] = s[3].rotate_left(45);
        result
    }

    /// Fill `buf` with pseudo-random bytes.
    pub fn fill(&mut self, buf: &mut [u8]) {
        let mut chunks = buf.chunks_exact_mut(8);
        for chunk in &mut chunks {
            chunk.copy_from_slice(&self.next_u64().to_le_bytes());
        }
        let rem = chunks.into_remainder();
        if !rem.is_empty() {
            let bytes = self.next_u64().to_le_bytes();
            let len = rem.len();
            rem.copy_from_slice(&bytes[..len]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_stream() {
        let mut a = Prng::from_seed(42);
        let mut b = Prng::from_seed(42);
        let mut buf_a = vec![0u8; 4096 + 5];
        let mut buf_b = vec![0u8; 4096 + 5];
        a.fill(&mut buf_a);
        b.fill(&mut buf_b);
        assert_eq!(buf_a, buf_b);
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Prng::from_seed(1);
        let mut b = Prng::from_seed(2);
        let mut buf_a = vec![0u8; 256];
        let mut buf_b = vec![0u8; 256];
        a.fill(&mut buf_a);
        b.fill(&mut buf_b);
        assert_ne!(buf_a, buf_b);
    }

    #[test]
    fn output_is_not_degenerate() {
        // Sanity: a 1 MiB stream should contain (nearly) every byte value.
        let mut p = Prng::from_seed(0xDEADBEEF);
        let mut buf = vec![0u8; 1 << 20];
        p.fill(&mut buf);
        let mut seen = [false; 256];
        for &b in &buf {
            seen[b as usize] = true;
        }
        assert!(seen.iter().filter(|&&s| s).count() == 256);
    }

    #[test]
    fn zero_seed_is_valid() {
        let mut p = Prng::from_seed(0);
        let mut buf = vec![0u8; 64];
        p.fill(&mut buf);
        assert!(buf.iter().any(|&b| b != 0));
    }
}
