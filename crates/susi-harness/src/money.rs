//! Integer micro-USD wire unit (`$0.000001`) carried by harness events.
use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct MicroUsd(u64);

impl MicroUsd {
    pub const ZERO: Self = Self(0);
    #[must_use]
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros)
    }
    #[must_use]
    pub const fn micros(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::MicroUsd;

    #[test]
    fn micro_usd_roundtrips_zero_and_non_zero_amounts() {
        assert_eq!(MicroUsd::ZERO.micros(), 0);
        assert_eq!(MicroUsd::from_micros(42).micros(), 42);
    }
}
