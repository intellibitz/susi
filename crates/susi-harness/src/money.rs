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

    /// Add, saturating at `u64::MAX` rather than wrapping into a free total.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Multiply by a whole number, saturating rather than wrapping.
    ///
    /// A projected cost is an estimate; a wrapped estimate would look cheap and
    /// let a mission spend money it does not have.
    #[must_use]
    pub fn saturating_mul(self, factor: u64) -> Self {
        Self(self.0.saturating_mul(factor))
    }

    /// The smaller of two amounts.
    #[must_use]
    pub const fn min(self, other: Self) -> Self {
        if self.0 < other.0 {
            self
        } else {
            other
        }
    }

    /// Parse a decimal dollar amount such as `0.25` or `12` into micro-USD.
    ///
    /// Integer arithmetic only: a float would reintroduce exactly the rounding
    /// this type exists to avoid. More than six decimal places are refused
    /// rather than silently truncated.
    #[must_use]
    pub fn parse_usd(text: &str) -> Option<Self> {
        let text = text.trim();
        let (dollars, cents) = match text.split_once('.') {
            Some((dollars, cents)) => (dollars, cents),
            None => (text, ""),
        };
        if dollars.is_empty() && cents.is_empty() {
            return None;
        }
        if !dollars.chars().all(|c| c.is_ascii_digit())
            || !cents.chars().all(|c| c.is_ascii_digit())
            || cents.len() > 6
        {
            return None;
        }
        let dollars: u64 = if dollars.is_empty() {
            0
        } else {
            dollars.parse().ok()?
        };
        let mut micros = cents.to_string();
        while micros.len() < 6 {
            micros.push('0');
        }
        let cents: u64 = if micros.is_empty() {
            0
        } else {
            micros.parse().ok()?
        };
        Some(Self(dollars.checked_mul(1_000_000)?.checked_add(cents)?))
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
    fn saturating_arithmetic_never_wraps() {
        let big = MicroUsd::from_micros(u64::MAX - 1);
        assert_eq!(
            big.saturating_add(MicroUsd::from_micros(10)),
            MicroUsd::from_micros(u64::MAX)
        );
        assert_eq!(big.saturating_mul(2), MicroUsd::from_micros(u64::MAX));
        assert_eq!(
            MicroUsd::from_micros(5).min(MicroUsd::from_micros(3)),
            MicroUsd::from_micros(3)
        );
    }

    #[test]
    fn dollars_parse_without_floats() {
        assert_eq!(
            MicroUsd::parse_usd("0.25"),
            Some(MicroUsd::from_micros(250_000))
        );
        assert_eq!(
            MicroUsd::parse_usd("12"),
            Some(MicroUsd::from_micros(12_000_000))
        );
        assert_eq!(
            MicroUsd::parse_usd("1.000001"),
            Some(MicroUsd::from_micros(1_000_001))
        );
        assert_eq!(MicroUsd::parse_usd("0.1234567"), None);
        assert_eq!(MicroUsd::parse_usd("abc"), None);
        assert_eq!(MicroUsd::parse_usd("1.2.3"), None);
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
