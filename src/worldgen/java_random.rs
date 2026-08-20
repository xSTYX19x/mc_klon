#[derive(Clone, Debug)]
pub(crate) struct JavaRandom {
    seed: u64,
}

impl JavaRandom {
    const MULTIPLIER: u64 = 0x5DEECE66D;
    const ADDEND: u64 = 0xB;
    const MASK: u64 = (1_u64 << 48) - 1;

    pub(crate) fn new(seed: i64) -> Self {
        let mut rand = Self { seed: 0 };
        rand.set_seed(seed);
        rand
    }

    pub(crate) fn set_seed(&mut self, seed: i64) {
        self.seed = ((seed as u64) ^ Self::MULTIPLIER) & Self::MASK;
    }

    /// Java `Random.next(bits)`
    pub(crate) fn next(&mut self, bits: u32) -> i32 {
        debug_assert!(bits <= 32);
        self.seed = self
            .seed
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::ADDEND)
            & Self::MASK;
        let shift = 48_u32.saturating_sub(bits);
        (self.seed >> shift) as i32
    }

    /// Java `Random.nextInt(bound)`
    pub(crate) fn next_int_bound(&mut self, bound: i32) -> i32 {
        assert!(bound > 0, "bound must be positive");

        if (bound & -bound) == bound {
            return (((bound as i64) * (self.next(31) as i64)) >> 31) as i32;
        }

        loop {
            let bits = self.next(31);
            let val = bits % bound;
            if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                return val;
            }
        }
    }

    /// Java `Random.nextLong()`
    pub(crate) fn next_long(&mut self) -> i64 {
        ((self.next(32) as i64) << 32).wrapping_add(self.next(32) as i64)
    }

    /// Java `Random.nextFloat()`
    pub(crate) fn next_float(&mut self) -> f32 {
        (self.next(24) as f32) / ((1_u32 << 24) as f32)
    }
}
