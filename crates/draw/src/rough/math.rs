//! rough's random numbers. Ported from rough.js 4.6.4 `math.ts` (MIT, see
//! `crates/draw/PORTED.md`).

use crate::js;

/// rough's seeded random sequence: a Park–Miller generator, so a seed
/// always gives the same drawing.
///
/// Without a seed (0) rough uses `Math.random`, and a few fills use
/// `Math.random` always. Here those draw from a second, deterministic
/// sequence kept in the same value (SplitMix64), so the port never needs
/// the platform's randomness and its output is reproducible; in rough those
/// numbers differ on every run anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Random {
    seed: i32,
    unseeded: u64,
}

impl Random {
    /// A sequence starting from `seed`.
    pub fn new(seed: i32) -> Random {
        Random {
            seed,
            unseeded: 0x9e37_79b9_7f4a_7c15,
        }
    }

    /// The next number in `[0, 1)` (`Random.next`).
    pub fn next_f64(&mut self) -> f64 {
        if self.seed != 0 {
            self.seed = js::imul(48271, self.seed);
            f64::from(self.seed & i32::MAX) / 2f64.powi(31)
        } else {
            self.math_random()
        }
    }

    /// A stand-in for `Math.random()`: the next number in `[0, 1)` of the
    /// unseeded sequence.
    pub fn math_random(&mut self) -> f64 {
        self.unseeded = self.unseeded.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.unseeded;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        (z >> 11) as f64 / 2f64.powi(53)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_sequence_is_park_miller() {
        let mut r = Random::new(1);
        // 48271, 48271², … modulo 2^32 as int32, low 31 bits over 2^31 (the
        // second and third have the sign bit set, which is dropped).
        assert_eq!(r.next_f64(), 48271.0 / 2f64.powi(31));
        assert_eq!(r.next_f64(), 182_605_793.0 / 2f64.powi(31));
        assert_eq!(r.next_f64(), 1_291_342_511.0 / 2f64.powi(31));
    }

    #[test]
    fn sequence_follows_imul() {
        let mut r = Random::new(162_451_595);
        let mut seed = 162_451_595i32;
        for _ in 0..1000 {
            seed = seed.wrapping_mul(48271);
            let expected = f64::from(seed & 0x7fff_ffff) / 2_147_483_648.0;
            assert_eq!(r.next_f64(), expected);
        }
    }

    #[test]
    fn unseeded_is_deterministic_and_in_range() {
        let mut a = Random::new(0);
        let mut b = Random::new(0);
        for _ in 0..1000 {
            let x = a.next_f64();
            assert_eq!(x, b.next_f64());
            assert!((0.0..1.0).contains(&x));
        }
        // The stand-in for Math.random doesn't move the seeded sequence.
        let mut c = Random::new(7);
        let mut d = Random::new(7);
        c.math_random();
        assert_eq!(c.next_f64(), d.next_f64());
    }
}
