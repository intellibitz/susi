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

/// One configured MCP server: a stdio command (`command` + `args`, optional
/// `env`, `extra.cwd`) or a streamable-HTTP URL in `command`
/// (`extra.auth_token`, `extra.headers`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl McpServerConfig {
    fn http_url(&self) -> Result<Option<url::Url>, String> {
        if !self.command.split_once("://").is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
        }) {
            return Ok(None);
        }
        let mut parsed =
            url::Url::parse(&self.command).map_err(|_| "invalid registered MCP URL")?;
        if parsed.path() == "/" {
            parsed.set_path("/mcp");
        }
        Ok(Some(parsed))
    }

    /// Canonical capability resource and whether requests stay on loopback.
    /// Never expose URL credentials or query values in denial messages.
    pub fn http_scope(&self) -> Result<Option<(String, bool)>, String> {
        let Some(parsed) = self.http_url()? else {
            return Ok(None);
        };
        let host = parsed.host_str().unwrap_or_default();
        let is_loopback = host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.to_canonical().is_loopback());
        Ok(Some((
            format!("{}{}", parsed.origin().ascii_serialization(), parsed.path()),
            is_loopback,
        )))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn global_mcp_entry_round_trips_unknown_fields_mandate_35() {
        let raw = json!({
            "name": "filesystem",
            "description": "Local file access",
            "package": "@modelcontextprotocol/server-filesystem",
            "category": "io",
            "trust_score": 0.5,
            "latency_ms": 12,
            "future_registry_attr": {"tier": "gold"},
            "publisher": "anthropic"
        });
        let entry: GlobalMcpEntry = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(entry.name, "filesystem");
        assert_eq!(entry.trust_score, Some(0.5));
        assert_eq!(entry.latency_ms, Some(12));
        assert_eq!(entry.extra.len(), 2);
        assert_eq!(entry.extra["publisher"], "anthropic");

        // Re-serialization keeps every field, known or not.
        assert_eq!(serde_json::to_value(&entry).unwrap(), raw);
    }

    #[test]
    fn global_mcp_entry_optional_metrics_may_be_null_or_absent() {
        let entry: GlobalMcpEntry = serde_json::from_value(json!({
            "name": "n", "description": "d", "package": "p", "category": "c",
            "trust_score": null
        }))
        .unwrap();
        assert_eq!(entry.trust_score, None);
        assert_eq!(entry.latency_ms, None);
        assert!(entry.extra.is_empty());

        // A required field missing is a hard error, not a silent default.
        assert!(serde_json::from_value::<GlobalMcpEntry>(json!({"name": "n"})).is_err());
    }

    #[test]
    fn mcp_config_round_trips_top_level_and_per_server_unknown_fields() {
        let raw = json!({
            "mcp_servers": {
                "local": {
                    "command": "npx",
                    "args": ["-y", "server"],
                    "env": {"MODE": "dev"},
                    "cwd": "/srv/mcp"
                },
                "remote": {
                    "command": "https://mcp.example.com/mcp",
                    "args": [],
                    "env": null,
                    "auth_token": "redacted-in-real-life"
                }
            },
            "schema_version": 3,
            "telemetry": false
        });
        let cfg: McpConfig = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(cfg.mcp_servers.len(), 2);
        assert_eq!(cfg.extra["schema_version"], 3);
        assert_eq!(cfg.mcp_servers["local"].extra["cwd"], "/srv/mcp");
        assert_eq!(
            cfg.mcp_servers["local"].env.as_ref().unwrap()["MODE"],
            "dev"
        );
        assert!(cfg.mcp_servers["remote"].env.is_none());

        assert_eq!(serde_json::to_value(&cfg).unwrap(), raw);
    }

    #[test]
    fn mcp_config_and_global_mcp_entry_clone_and_empty_servers_roundtrip() {
        let empty_raw = json!({
            "mcp_servers": {}
        });
        let empty_cfg: McpConfig = serde_json::from_value(empty_raw.clone()).unwrap();
        assert!(empty_cfg.mcp_servers.is_empty());
        assert!(empty_cfg.extra.is_empty());
        let cloned_cfg = empty_cfg.clone();
        assert_eq!(serde_json::to_value(&cloned_cfg).unwrap(), empty_raw);

        let entry_raw = json!({
            "name": "git",
            "description": "Git repository tools",
            "package": "@modelcontextprotocol/server-git",
            "category": "vcs",
            "trust_score": 0.75,
            "latency_ms": 5,
            "custom_tag": "verified"
        });
        let entry: GlobalMcpEntry = serde_json::from_value(entry_raw.clone()).unwrap();
        let cloned_entry = entry.clone();
        assert_eq!(cloned_entry.name, "git");
        assert_eq!(cloned_entry.category, "vcs");
        assert_eq!(cloned_entry.extra["custom_tag"], "verified");
        assert_eq!(serde_json::to_value(&cloned_entry).unwrap(), entry_raw);
    }

    #[test]
    fn mcp_server_config_reexport_roundtrips_command_and_args() {
        let raw = json!({
            "command": "uvx",
            "args": ["mcp-server-sqlite", "--db-path", "test.db"],
            "env": null
        });
        let server: McpServerConfig = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(server.command, "uvx");
        assert_eq!(server.args.len(), 3);
        assert_eq!(serde_json::to_value(&server).unwrap(), raw);
    }
}
