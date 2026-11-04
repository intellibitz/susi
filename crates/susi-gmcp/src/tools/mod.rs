//! GMCP Universal Meta MCP Tool Registry
//! Pure Rust implementation for Dynamic MCP Server Proxying, Meta Tool Routing & Wasm Reflexes

mod adapter_tools;
pub mod args;
mod control_plane;
mod core;
mod forward;
mod harness_tools;
mod helpers;
mod host_ops;
mod host_view_tools;
mod provider_tools;
#[cfg(feature = "tools-rich")]
pub mod semantic_index;

pub use crate::tool_registry::{GmcpClient, ToolRegistry};
pub use crate::tool_types::{McpTool, MetaCategory, SusiTool};

pub use core::CoreTools;
