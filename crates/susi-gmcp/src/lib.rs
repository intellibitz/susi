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

/// GMCP's own MCP server (wire model, sessions, stdio and Streamable HTTP).
pub mod mcp;
/// The path the typed MCP API has always had: consumers name
/// `susi_gmcp::rmcp::model::*` for the typed form the canonical content
/// model adapts into (`content::CanonicalContentBlock::to_rmcp`). It is
/// GMCP's own implementation in [`mcp`]; no MCP SDK is linked.
pub use self::mcp as rmcp;

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod catalog;
pub mod content;
pub mod direct_call;
pub mod embed_provider;
pub mod extensions;
pub mod jsonrpc_gate;
pub mod mcp_call_budgets;
pub mod mcp_probe;
pub mod mcp_profile;
pub mod mcp_registry_trust;
pub mod mcp_wrapper;
pub mod protocol;
pub mod requirements_ledger;
pub mod schema_check;
pub mod server;
pub mod stdio;
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
mod eco_mcp_features;
#[cfg(test)]
#[path = "tests/mcp_bus.rs"]
mod mcp_bus_tests;
#[cfg(test)]
#[path = "tests/protocol.rs"]
mod protocol_tests;
#[cfg(test)]
#[path = "tests/resource_surface.rs"]
mod resource_surface_tests;
#[cfg(test)]
#[path = "tests/vc_201_086_mastery.rs"]
mod vc_201_086_mastery_tests;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "tests/lib_inline.rs"]
mod inline_tests;
