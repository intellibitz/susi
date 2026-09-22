//! GMCP Universal Meta MCP Tool Registry
//! Pure Rust implementation for Dynamic MCP Server Proxying, Meta Tool Routing & Wasm Reflexes

mod bootstrap;
mod core;
mod helpers;
#[cfg(feature = "tools-rich")]
pub(crate) mod semantic_index;

pub use susi_tools::{GmcpClient, McpTool, MetaCategory, SusiTool, ToolRegistry};

pub use bootstrap::bootstrap_registry;
pub use core::CoreTools;
