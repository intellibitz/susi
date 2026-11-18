//! Integer micro-USD wire unit (`$0.000001`) carried by [`crate::event::Event`].
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
