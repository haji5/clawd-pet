//! A tiny xorshift64* RNG for blink/fidget timing (no `rand` dependency).

pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    pub fn from_clock() -> Self {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
        Rng::new(t.as_nanos() as u64 ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform-ish in `lo..hi` (`lo` if the range is empty).
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo { lo } else { lo + self.next() % (hi - lo) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_stays_in_bounds() {
        let mut r = Rng::new(42);
        for _ in 0..1000 {
            let v = r.range(3, 8);
            assert!((3..8).contains(&v));
        }
        assert_eq!(r.range(5, 5), 5);
    }
}
