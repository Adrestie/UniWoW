//! Pseudo-random numbers for the stress tests, seeded so that a failure can be replayed.

pub struct Random(u64);

impl Random {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    /// A number from 0 to `bound` excluded (xorshift).
    pub fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound.max(1)
    }
}
