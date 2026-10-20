//! Integer money for the harness ledger.
//!
//! Spend is counted in **micro-USD** (one millionth of a dollar) as a `u64`.
//! The ledger sums integers, never floats: a total that has to reconcile with
//! a provider invoice must not accumulate rounding drift.

use serde::{Deserialize, Serialize};
use std::fmt;

/// An amount of money in micro-USD.
///
/// One micro-dollar is `$0.000001`. A typical coding task costs tens of
/// thousands of micro-USD, which leaves ample headroom in a `u64`
/// (about `$18.4` trillion) for a monthly total.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MicroUsd(u64);

impl MicroUsd {
    /// Zero dollars.
    pub const ZERO: Self = Self(0);

    /// Build an amount from micro-USD.
    #[must_use]
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }

    /// The amount in micro-USD.
    #[must_use]
    pub const fn micros(self) -> u64 {
        self.0
    }

    /// True when nothing has been charged.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Add two amounts, or `None` on overflow.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    /// Subtract an amount, or `None` when it would go negative.
    #[must_use]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }
}

impl fmt::Display for MicroUsd {
    /// Six decimal places, so a single cheap cached call is still visible.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${}.{:06}", self.0 / 1_000_000, self.0 % 1_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_is_integer_and_checked() {
        let a = MicroUsd::from_micros(1_500_000);
        let b = MicroUsd::from_micros(500_000);
        assert_eq!(a.checked_add(b), Some(MicroUsd::from_micros(2_000_000)));
        assert_eq!(b.checked_sub(a), None);
        assert_eq!(MicroUsd::ZERO.checked_add(a), Some(a));
        assert!(MicroUsd::ZERO.is_zero());
        assert_eq!(
            MicroUsd::from_micros(u64::MAX).checked_add(MicroUsd::from_micros(1)),
            None
        );
    }

    #[test]
    fn display_is_six_places() {
        assert_eq!(MicroUsd::from_micros(217_200).to_string(), "$0.217200");
        assert_eq!(MicroUsd::from_micros(1_000_000).to_string(), "$1.000000");
        assert_eq!(MicroUsd::from_micros(3).to_string(), "$0.000003");
    }

    #[test]
    fn serde_is_a_bare_integer() {
        let money = MicroUsd::from_micros(42);
        let json = serde_json::to_string(&money).unwrap();
        assert_eq!(json, "42");
        assert_eq!(serde_json::from_str::<MicroUsd>("42").unwrap(), money);
    }
}
