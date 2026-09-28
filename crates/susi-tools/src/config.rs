use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalMcpEntry {
    pub name: String,
    pub description: String,
    pub package: String,
    pub category: String,
    pub trust_score: Option<f32>,
    pub latency_ms: Option<u64>,
    // Mandate 35: catch-all so a field this struct doesn't yet name (e.g. a
    // future registry attribute) round-trips instead of being silently
    // dropped when this entry is re-serialized.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Re-exported from the MCP vendor crate so `mcp_config.json` keeps one shape.
pub use susi_vendor_mcp::McpServerConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}
