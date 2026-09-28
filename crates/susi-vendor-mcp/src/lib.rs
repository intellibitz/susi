#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! # susi-vendor-mcp
//!
//! The one crate that links the MCP client SDK (`rmcp`) and its `reqwest`
//! streamable-HTTP transport. Callers see SUSI-shaped blocking calls —
//! [`list_tools_blocking`], [`call_blocking_result`] — over a
//! [`McpServerConfig`]; no rmcp type crosses this boundary. Pooled
//! connections, connect-failure cooldown and lease-bounded calls live here;
//! policy (lease / handshake budgets from `SusiConfig`) stays with the
//! caller (`susi-tools`).

mod client;

pub use client::{call_blocking_result, list_tools_blocking};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// One configured MCP server: a stdio command (`command` + `args`, optional
/// `env`, `extra.cwd`) or a streamable-HTTP URL in `command`
/// (`extra.auth_token`, `extra.headers`).
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
