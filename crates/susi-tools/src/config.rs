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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    // Mandate 35: mcp_config.json is read, mutated (auto_configure_server),
    // and rewritten whole - without this, a field a user hand-added ahead
    // of susi support for it (e.g. a future `cwd` or `transport`) would be
    // silently deleted on the next write-back rather than round-tripped.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}
