//! GMCP Universal Meta MCP Tool Registry
//! Pure Rust implementation for Dynamic MCP Server Proxying, Meta Tool Routing & Wasm Reflexes

mod bootstrap;
mod core;
mod helpers;
#[cfg(feature = "tools-rich")]
pub mod semantic_index;

pub use crate::tool_registry::{GmcpClient, ToolRegistry};
pub use crate::tool_types::{McpTool, MetaCategory, SusiTool};

pub use bootstrap::bootstrap_registry;
pub use core::CoreTools;
