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
    // Mandate 35: mcp_config.json is read, mutated (admit_mcp_server, enable),
    // and rewritten whole - without this, a field a user hand-added ahead
    // of susi support for it (e.g. a future `cwd` or `transport`) would be
    // silently deleted on the next write-back rather than round-tripped.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn mcp_server_config_serde_roundtrip_preserves_unknown_flattened_fields() {
        let raw = r#"{
            "command": "npx",
            "args": ["-y", "@modelcontextprotocol/server-filesystem"],
            "cwd": "/tmp/work",
            "transport": "stdio"
        }"#;
        let cfg: McpServerConfig = serde_json::from_str(raw).expect("parse config");
        assert_eq!(cfg.command, "npx");
        assert_eq!(cfg.args.len(), 2);
        assert_eq!(cfg.extra.get("cwd"), Some(&serde_json::json!("/tmp/work")));

        let reserialized = serde_json::to_value(&cfg).expect("reserialize");
        assert_eq!(reserialized["transport"], "stdio");
    }

    #[test]
    fn mcp_server_config_deserializes_optional_env_map() {
        let raw = r#"{
            "command": "mcp-bin",
            "args": [],
            "env": {"MCP_MODE": "strict"}
        }"#;
        let cfg: McpServerConfig = serde_json::from_str(raw).expect("parse config");
        assert_eq!(
            cfg.env
                .as_ref()
                .and_then(|e| e.get("MCP_MODE"))
                .map(String::as_str),
            Some("strict")
        );
    }

    #[test]
    fn mcp_server_config_defaults_env_to_none_when_omitted() {
        let raw = r#"{"command": "http://127.0.0.1:9999/mcp", "args": []}"#;
        let cfg: McpServerConfig = serde_json::from_str(raw).expect("parse config");
        assert!(cfg.env.is_none());
        assert!(cfg.extra.is_empty());
    }

    #[test]
    fn mcp_server_config_clone_preserves_command_args_and_extra() {
        let mut extra = HashMap::new();
        extra.insert("timeout_ms".into(), serde_json::json!(5000));
        let cfg = McpServerConfig {
            command: "server".into(),
            args: vec!["--stdio".into()],
            env: None,
            extra,
        };
        let cloned = cfg.clone();
        assert_eq!(cloned.command, "server");
        assert_eq!(cloned.args, vec!["--stdio".to_string()]);
        assert_eq!(
            cloned.extra.get("timeout_ms"),
            Some(&serde_json::json!(5000))
        );
    }

    #[test]
    fn mcp_server_config_debug_and_empty_args_serialization_roundtrip() {
        let cfg = McpServerConfig {
            command: "echo".into(),
            args: Vec::new(),
            env: None,
            extra: HashMap::new(),
        };
        assert!(format!("{cfg:?}").contains("echo"));
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: McpServerConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.command, "echo");
        assert!(back.args.is_empty());
    }
}
