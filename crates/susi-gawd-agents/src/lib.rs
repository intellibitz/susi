#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Agent metadata contract: the agent profile and trait types. Catalog, fleet
//! execution and the typed agent call wrappers are owned by the registered
//! implementation.
pub use susi_core::agent_types::{AgentProfile, GawdAgent, GawdAgentInfo};
