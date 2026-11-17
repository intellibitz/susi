#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Generic agent metadata and external execution socket bindings.
//! Catalog and fleet execution are owned by the registered implementation.
pub use susi_core::agent_types::{AgentProfile, GawdAgent, GawdAgentInfo};
pub use susi_core::plane_bus::agents::*;
