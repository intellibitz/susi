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

/// Protocol Dispatcher: Trait for handling cross-protocol JSON-RPC requests.
pub trait ProtocolDispatcher: Send + Sync {
    fn handle_request(&self, line: &str, workspace: &Path) -> String;
}

/// Capability Resolver: Trait for dynamic discovery and resolution of tool capabilities.
pub trait CapabilityResolver: Send + Sync {
    fn resolve(&self, name: &str) -> Option<String>;
}

#[cfg(test)]
mod protocol_tests;
