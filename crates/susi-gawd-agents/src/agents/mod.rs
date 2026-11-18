//! Native GAWD agents and fleet synthesizer (Mandate 3 split).
//!
//! Goal-shape classifiers live in [`crate::goal_shape`] so `native` does not
//! depend on `fleet`.

mod fleet;
mod native;

pub use fleet::*;
pub use native::*;
