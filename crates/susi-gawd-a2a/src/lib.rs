#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
//! Agent2Agent protocol contracts. Serving and execution live with the implementation owner.
/// Agent Client Protocol contracts and external client.
pub use susi_gawd_acp as acp;
pub mod protocol;

#[cfg(test)]
#[path = "tests/wire_contract.rs"]
mod tests;
