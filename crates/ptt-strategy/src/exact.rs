//! Minimal exact non-negative rational arithmetic.
//!
//! The Electron algorithms did this work in `f64` and then papered over the
//! damage with `Math.abs(x - Math.round(x)) < 1e-6` comparisons. Profit here
//! is the difference of two nearly equal numbers, which is precisely where
//! binary floating point loses the answer, so the strategy layer carries its
//! own rational instead. Values stay reduced after every operation and every
//! operation is checked, so an overflow surfaces as `None` rather than as a
//! silently wrong ratio.

/// A non-negative rational held as a reduced `num / den` with `den > 0`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Rational {
    num: u128,
    den: u128,
}

impl Rational {
    pub(crate) fn new(num: u128, den: u128) -> Option<Self> {
        if den == 0 {
            return None;
        }
        let divisor = gcd(num, den).max(1);
        Some(Self {
            num: num / divisor,
            den: den / divisor,
        })
    }

    pub(crate) const fn from_u128(value: u128) -> Self {
        Self { num: value, den: 1 }
    }

    pub(crate) const fn one() -> Self {
        Self { num: 1, den: 1 }
    }

    pub(crate) const fn is_zero(self) -> bool {
        self.num == 0
    }

    pub(crate) fn checked_mul(self, other: Self) -> Option<Self> {
        // Cross-reduce before multiplying: a 4-hop product of raw panel
        // ratios overflows u128 otherwise.
        let left = gcd(self.num, other.den).max(1);
        let right = gcd(other.num, self.den).max(1);
        let num = (self.num / left).checked_mul(other.num / right)?;
        let den = (self.den / right).checked_mul(other.den / left)?;
        Self::new(num, den)
    }

    pub(crate) fn checked_div(self, other: Self) -> Option<Self> {
        if other.num == 0 {
            return None;
        }
        self.checked_mul(Self {
            num: other.den,
            den: other.num,
        })
    }

    /// The largest integer not greater than this value.
    pub(crate) fn floor_u64(self) -> Option<u64> {
        u64::try_from(self.num / self.den).ok()
    }

    /// The reduced pair, when both halves fit a `u64`.
    pub(crate) fn to_u64_pair(self) -> Option<(u64, u64)> {
        Some((u64::try_from(self.num).ok()?, u64::try_from(self.den).ok()?))
    }
}

const fn gcd(left: u128, right: u128) -> u128 {
    let mut a = left;
    let mut b = right;
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn products_stay_reduced_and_exact() {
        // 1:10.33 four times over: the f64 version of this drifts.
        let rate = Rational::new(1033, 100).expect("rate");
        let mut value = Rational::one();
        for _ in 0..4 {
            value = value.checked_mul(rate).expect("no overflow");
        }
        assert_eq!(
            value,
            Rational::new(1033 * 1033 * 1033 * 1033, 100_000_000).expect("expected")
        );
    }

    #[test]
    fn division_inverts_multiplication_exactly() {
        let amount = Rational::from_u128(920);
        let rate = Rational::new(19_725, 100).expect("rate");
        let out = amount.checked_mul(rate).expect("mul");
        assert_eq!(out.checked_div(rate).expect("div"), amount);
    }

    #[test]
    fn floor_truncates_and_never_rounds_up() {
        assert_eq!(Rational::new(999, 100).expect("value").floor_u64(), Some(9));
        assert_eq!(
            Rational::new(1000, 100).expect("value").floor_u64(),
            Some(10)
        );
    }

    #[test]
    fn zero_denominator_is_rejected_rather_than_panicking() {
        assert!(Rational::new(1, 0).is_none());
        assert!(
            Rational::one()
                .checked_div(Rational::from_u128(0))
                .is_none()
        );
    }
}
