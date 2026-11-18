#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Generic MCP wire model and tool socket dispatch. The MCP client, catalog,
//! server handlers and concrete tools live with the registered host.
pub use susi_core::plane_bus::tools as plane_tools;
pub mod mcp {
    pub mod model;
}
pub use mcp as rmcp;

/// The existing tool-dispatch entry point over the canonical plane bus.
pub struct GmcpHost;
impl GmcpHost {
    pub fn dispatch(name: &str, arg: &str, workspace: &std::path::Path) -> String {
        let value = serde_json::from_str(arg).unwrap_or(serde_json::json!(arg));
        plane_tools::execute_tool(name, &value, workspace)
            .unwrap_or_else(|error| format!("[Error] {error}"))
    }
}
