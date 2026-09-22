pub mod catalog;
pub mod mcp_wrapper;
pub mod protocol;
pub mod reflexes;
pub mod server;
mod stdio;
pub mod tools;

use std::path::Path;

pub use susi_tools::{GlobalMcpEntry, McpConfig, McpServerConfig};

/// GMCP Host: The unified execution entry point for the Meta-Intelligence Substrate.
pub struct GmcpHost;

impl GmcpHost {
    pub fn dispatch(name: &str, arg: &str, workspace: &Path) -> String {
        let val = serde_json::from_str(arg).unwrap_or(serde_json::json!(arg));
        susi_tools::ToolRegistry::execute_tool(name, &val, workspace)
    }
}

#[cfg(test)]
mod protocol_tests;
