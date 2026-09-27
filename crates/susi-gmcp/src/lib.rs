#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod catalog;
#[cfg(feature = "tools-rich")]
pub mod embed_provider;
#[cfg(not(feature = "tools-rich"))]
pub mod embed_provider {
    /// No fastembed in this build — the embed surface refuses rather
    /// than fabricating vectors.
    pub fn register_local_embed_provider() {}
}
pub mod mcp_wrapper;
pub mod protocol;
pub mod reflexes;
pub mod server;
mod stdio;
pub mod tool_registry;
pub mod tool_types;
pub mod tools;

use std::path::Path;

pub use crate::susi_core::plane_bus::tools as plane_tools;

/// GMCP Host: The unified execution entry point for the Meta-Intelligence Substrate.
pub struct GmcpHost;

impl GmcpHost {
    pub fn dispatch(name: &str, arg: &str, workspace: &Path) -> String {
        let val = serde_json::from_str(arg).unwrap_or(serde_json::json!(arg));
        plane_tools::execute_tool(name, &val, workspace).unwrap_or_else(|e| format!("[Error] {e}"))
    }
}

#[cfg(test)]
mod protocol_tests;
