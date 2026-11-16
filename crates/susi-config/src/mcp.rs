//! Shared MCP configuration, transport authorization, and isolated child preparation.

use crate::mac_policy::{actions, MacPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    net::IpAddr,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalMcpEntry {
    pub name: String,
    pub description: String,
    pub package: String,
    pub category: String,
    pub trust_score: Option<f32>,
    pub latency_ms: Option<u64>,
    // Preserve unknown registry attributes across read-modify-write.
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
    // Preserve unknown server fields across read-modify-write.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    pub mcp_servers: HashMap<String, McpServerConfig>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Resolve the shared MCP configuration file, retaining the client's directory setup.
pub fn mcp_config_path() -> PathBuf {
    let directory = susi_paths::SusiDirs::config_dir();
    if !directory.exists() {
        let _ = fs::create_dir_all(&directory);
    }
    directory.join("mcp_config.json")
}

/// Load `mcp_config.json` for a read-modify-write. Absent means no
/// servers yet. A torn or unreadable file is an error: callers must not
/// rewrite it as an empty config, which would drop every other server.
pub fn read_mcp_config(path: &std::path::Path) -> Result<McpConfig, &'static str> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).map_err(|_| "ERROR_CONFIG_DAMAGED"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(McpConfig {
            mcp_servers: HashMap::new(),
            extra: HashMap::new(),
        }),
        Err(_) => Err("ERROR_CONFIG_UNREADABLE"),
    }
}

/// Normalized HTTP target used by the existing MCP client.
#[derive(Debug, Clone)]
pub struct ParsedHttpUrl {
    scheme: String,
    host: String,
    port: u16,
    default_port: bool,
    path: String,
    is_loopback: bool,
}

impl ParsedHttpUrl {
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> u16 {
        self.port
    }
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl McpServerConfig {
    /// Parse an HTTP target using the same normalization as authorization.
    pub fn parse_http_url(&self) -> Result<Option<ParsedHttpUrl>, String> {
        let Some((scheme_raw, rest)) = self.command.split_once("://") else {
            return Ok(None);
        };
        if !scheme_raw.eq_ignore_ascii_case("http") && !scheme_raw.eq_ignore_ascii_case("https") {
            return Ok(None);
        }
        let scheme = scheme_raw.to_ascii_lowercase();
        let default_port_num: u16 = if scheme == "https" { 443 } else { 80 };
        let without_frag = rest.split_once('#').map_or(rest, |(before, _)| before);
        let without_query = without_frag
            .split_once('?')
            .map_or(without_frag, |(before, _)| before);
        let (authority, raw_path) = match without_query.split_once('/') {
            Some((auth, path_rest)) => (auth, format!("/{path_rest}")),
            None => (without_query, "/".to_string()),
        };
        let path = if raw_path == "/" {
            "/mcp".to_string()
        } else {
            raw_path
        };
        let hostport = authority
            .rsplit_once('@')
            .map_or(authority, |(_, hp)| hp)
            .trim();
        if hostport.is_empty() {
            return Err("invalid registered MCP URL".to_string());
        }
        let (host, port) = if hostport.starts_with('[') {
            let Some(end_bracket) = hostport.find(']') else {
                return Err("invalid registered MCP URL".to_string());
            };
            let host_str = hostport[..=end_bracket].to_ascii_lowercase();
            let inner = &host_str[1..host_str.len() - 1];
            if inner.parse::<std::net::Ipv6Addr>().is_err() {
                return Err("invalid registered MCP URL".to_string());
            }
            let after = &hostport[end_bracket + 1..];
            let port_num = if after.is_empty() {
                default_port_num
            } else if let Some(p_str) = after.strip_prefix(':') {
                p_str
                    .parse::<u16>()
                    .map_err(|_| "invalid registered MCP URL".to_string())?
            } else {
                return Err("invalid registered MCP URL".to_string());
            };
            (host_str, port_num)
        } else {
            let (host_part, port_num) = if let Some((h, p_str)) = hostport.rsplit_once(':') {
                (
                    h,
                    p_str
                        .parse::<u16>()
                        .map_err(|_| "invalid registered MCP URL".to_string())?,
                )
            } else {
                (hostport, default_port_num)
            };
            if host_part.is_empty()
                || host_part
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b == b'/' || b == b'@')
            {
                return Err("invalid registered MCP URL".to_string());
            }
            (host_part.to_ascii_lowercase(), port_num)
        };
        let is_loopback = host.trim_end_matches('.').eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.to_canonical().is_loopback());
        Ok(Some(ParsedHttpUrl {
            scheme,
            host,
            port,
            default_port: port == default_port_num,
            path,
            is_loopback,
        }))
    }

    /// Canonical capability resource and whether requests stay on loopback.
    /// Never expose URL credentials or query values in denial messages.
    pub fn http_scope(&self) -> Result<Option<(String, bool)>, String> {
        let Some(parsed) = self.parse_http_url()? else {
            return Ok(None);
        };
        let port_suffix = if parsed.default_port {
            String::new()
        } else {
            format!(":{}", parsed.port)
        };
        Ok(Some((
            format!(
                "{}://{}{}{}",
                parsed.scheme, parsed.host, port_suffix, parsed.path
            ),
            parsed.is_loopback,
        )))
    }
}

/// Enforce transport and declared capability permissions before dispatch.
pub fn authorize_mcp_transport(server: &McpServerConfig, policy: &MacPolicy) -> Result<(), String> {
    let endpoint = server.http_scope()?;
    let resource = endpoint
        .as_ref()
        .map_or(server.command.as_str(), |(url, _)| url.as_str());
    let require = |action: &str| {
        if policy.is_permitted("susi", action, resource) {
            Ok(())
        } else {
            Err(format!(
                "MCP access denied: `{resource}` requires `{action}`"
            ))
        }
    };
    match endpoint.as_ref() {
        None => require(actions::PROCESS_EXEC)?,
        Some((_, false)) => require(actions::NETWORK_EGRESS)?,
        Some((_, true)) => {}
    }
    if let Some(declared) = server.extra.get("capabilities") {
        for capability in declared
            .as_array()
            .ok_or("MCP capabilities must be a list")?
        {
            match capability.as_str() {
                Some("tools") => {}
                Some("filesystem") => {
                    require(actions::FILESYSTEM_READ)?;
                    require(actions::FILESYSTEM_WRITE)?;
                }
                Some("network") => require(actions::NETWORK_EGRESS)?,
                Some(name) => return Err(format!("MCP capability `{name}` is not supported")),
                None => return Err("MCP capability must be a string".into()),
            }
        }
    }
    Ok(())
}

fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// Resolve configured authentication and validate custom HTTP headers.
pub fn validate_http_headers(config: &McpServerConfig) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    if let Some(token) = config.extra.get("auth_token").and_then(Value::as_str) {
        out.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    if let Some(headers) = config.extra.get("headers").and_then(Value::as_object) {
        for (name, value) in headers {
            if !is_valid_header_name(name) {
                return Err(format!("invalid HTTP header name `{name}`"));
            }
            let val_str = value
                .as_str()
                .ok_or_else(|| "MCP header values must be strings".to_string())?;
            if val_str.bytes().any(|b| b == b'\r' || b == b'\n') {
                return Err("MCP header values must not contain CR/LF".to_string());
            }
            out.push((name.clone(), val_str.to_string()));
        }
    }
    Ok(out)
}

/// Prepare an isolated command after transport and capability authorization.
pub fn prepare_authorized_stdio_command(
    config: &McpServerConfig,
    policy: &MacPolicy,
) -> Result<Command, String> {
    if config.parse_http_url()?.is_some() {
        return Err("spawn_authorized_stdio_peer requires a stdio command, not an HTTP URL".into());
    }
    authorize_mcp_transport(config, policy)?;
    Ok(prepare_stdio_command(config))
}

/// Prepare stored configuration without performing policy authorization.
/// Transport owners must authorize before dispatch, or use
/// [`prepare_authorized_stdio_command`] to perform both steps.
pub fn prepare_stdio_command(config: &McpServerConfig) -> Command {
    stdio_command(config, false)
}

fn stdio_command(config: &McpServerConfig, literal_environment: bool) -> Command {
    let mut command = Command::new(&config.command);
    susi_paths::console::without_window(&mut command)
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.env_clear();
    susi_paths::test_env::inherit_registry_isolation(&mut command);
    for key in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
        "HOME",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    susi_paths::console::apply_non_interactive_env(&mut command);
    if let Some(env_keys) = config
        .extra
        .get("env_keys")
        .filter(|_| !literal_environment)
        .and_then(Value::as_array)
    {
        for key in env_keys.iter().filter_map(Value::as_str) {
            if let Ok(value) = crate::env_or_cloud_env(key) {
                if !value.is_empty() {
                    command.env(key, value);
                }
            }
        }
    }
    if let Some(env) = &config.env {
        for (k, v) in env {
            if !literal_environment && v.trim().is_empty() {
                if let Ok(resolved) = crate::env_or_cloud_env(k) {
                    if !resolved.is_empty() {
                        command.env(k, resolved);
                        continue;
                    }
                }
            }
            command.env(k, v);
        }
    }
    if let Some(cwd) = config.extra.get("cwd").and_then(Value::as_str) {
        if !literal_environment && cwd.contains("{workspace}") {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            command.current_dir(cwd.replace("{workspace}", &ws.to_string_lossy()));
        } else {
            command.current_dir(cwd);
        }
    }
    command
}

/// Authorize and spawn an isolated, windowless stdio MCP child.
pub fn spawn_authorized_stdio_peer(
    config: &McpServerConfig,
    policy: &MacPolicy,
) -> Result<std::process::Child, String> {
    prepare_authorized_stdio_command(config, policy)?
        .spawn()
        .map_err(|error| error.to_string())
}

/// Prepare a client-supplied MCP peer for the caller's process containment.
/// Unlike stored host configurations, empty values never resolve credentials
/// from the host environment and cwd never expands a workspace template.
pub fn authorized_stdio_command_literal(
    config: &McpServerConfig,
    policy: &MacPolicy,
) -> Result<Command, String> {
    if config.parse_http_url()?.is_some() {
        return Err("stdio MCP requires an executable, not an HTTP URL".into());
    }
    authorize_mcp_transport(config, policy)?;
    Ok(stdio_command(config, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn global_mcp_entry_round_trips_unknown_fields() {
        let raw = json!({
            "name": "filesystem",
            "description": "Local file access",
            "package": "@modelcontextprotocol/server-filesystem",
            "category": "io",
            "trust_score": 0.5,
            "latency_ms": 12,
            "future_registry_attr": {"tier": "gold"},
            "publisher": "registry"
        });
        let entry: GlobalMcpEntry = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(entry.name, "filesystem");
        assert_eq!(entry.trust_score, Some(0.5));
        assert_eq!(entry.latency_ms, Some(12));
        assert_eq!(entry.extra.len(), 2);
        assert_eq!(entry.extra["publisher"], "registry");

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
    use crate::mac_policy::PrivacyMode;
    fn server(command: &str, capabilities: Option<Value>) -> McpServerConfig {
        let mut extra = HashMap::new();
        if let Some(c) = capabilities {
            extra.insert("capabilities".to_string(), c);
        }
        McpServerConfig {
            command: command.to_string(),
            args: Vec::new(),
            env: None,
            extra,
        }
    }
    fn local_only() -> MacPolicy {
        MacPolicy::new([7u8; 32], PrivacyMode::LocalOnly, false)
    }
    #[test]
    fn subprocess_server_requires_process_exec() {
        let err = authorize_mcp_transport(&server("npx", None), &local_only()).unwrap_err();
        assert!(err.contains(actions::PROCESS_EXEC), "{err}");
        assert!(err.contains("`npx`"), "{err}");
        let balanced = MacPolicy::new([7u8; 32], PrivacyMode::Balanced, false);
        assert_eq!(
            authorize_mcp_transport(&server("npx", None), &balanced),
            Ok(())
        );
        let policy = local_only();
        policy
            .grant("susi", actions::PROCESS_EXEC, "npx", None)
            .unwrap();
        assert_eq!(
            authorize_mcp_transport(&server("npx", None), &policy),
            Ok(())
        );
    }
    #[test]
    fn remote_http_requires_network_egress_but_loopback_does_not() {
        let policy = local_only();
        let err = authorize_mcp_transport(&server("https://mcp.example.com/mcp", None), &policy)
            .unwrap_err();
        assert!(err.contains(actions::NETWORK_EGRESS), "{err}");
        for loopback in [
            "http://127.0.0.1:9090/mcp",
            "http://localhost:9093",
            "http://[::1]:9090/mcp",
        ] {
            assert_eq!(
                authorize_mcp_transport(&server(loopback, None), &policy),
                Ok(()),
                "{loopback} must stay local"
            );
        }
        let open = MacPolicy::new([7u8; 32], PrivacyMode::Open, false);
        assert_eq!(
            authorize_mcp_transport(&server("https://mcp.example.com/mcp", None), &open),
            Ok(())
        );
    }
    #[test]
    fn declared_capabilities_are_validated_and_gated() {
        let policy = local_only();
        let local = "http://127.0.0.1:9090/mcp";
        assert_eq!(
            authorize_mcp_transport(
                &server(local, Some(json!(["tools", "filesystem"]))),
                &policy
            ),
            Ok(())
        );
        let err =
            authorize_mcp_transport(&server(local, Some(json!(["network"]))), &policy).unwrap_err();
        assert!(err.contains(actions::NETWORK_EGRESS), "{err}");
        let err =
            authorize_mcp_transport(&server(local, Some(json!(["tools", "telepathy"]))), &policy)
                .unwrap_err();
        assert_eq!(err, "MCP capability `telepathy` is not supported");
        let err = authorize_mcp_transport(&server(local, Some(json!([42]))), &policy).unwrap_err();
        assert_eq!(err, "MCP capability must be a string");
        let err =
            authorize_mcp_transport(&server(local, Some(json!("tools"))), &policy).unwrap_err();
        assert_eq!(err, "MCP capabilities must be a list");
        assert_eq!(
            authorize_mcp_transport(&server(local, Some(json!([]))), &policy),
            Ok(())
        );
    }
    #[test]
    fn transport_denial_precedes_capability_validation() {
        let err = authorize_mcp_transport(&server("npx", Some(json!("not-a-list"))), &local_only())
            .unwrap_err();
        assert!(err.contains(actions::PROCESS_EXEC), "{err}");
    }
    #[test]
    fn balanced_policy_permits_loopback_and_subprocess_with_filesystem_capability() {
        let balanced = MacPolicy::new([9u8; 32], PrivacyMode::Balanced, false);
        assert_eq!(
            authorize_mcp_transport(
                &server("npx", Some(json!(["tools", "filesystem"]))),
                &balanced
            ),
            Ok(())
        );
    }
    #[test]
    fn registered_http_scope_uses_parsed_authority_and_omits_credentials() {
        let mut config = server("http://127.0.0.1:1", None);
        for (url, resource, local) in [
            (
                "HTTP://user:password@EXAMPLE.org:80?token=secret#ignore",
                "http://example.org/mcp",
                false,
            ),
            (
                "http://localhost.evil.test/private",
                "http://localhost.evil.test/private",
                false,
            ),
            (
                "http://127.0.0.1@bad.invalid/",
                "http://bad.invalid/mcp",
                false,
            ),
            ("http://192.168.1.4/", "http://192.168.1.4/mcp", false),
            ("http://localhost:4321/", "http://localhost:4321/mcp", true),
            ("http://[::1]:4321/", "http://[::1]:4321/mcp", true),
            ("http://127.0.0.2/", "http://127.0.0.2/mcp", true),
        ] {
            config.command = url.to_string();
            assert_eq!(
                config.http_scope().unwrap(),
                Some((resource.to_string(), local))
            );
        }
    }

    #[test]
    fn config_load_distinguishes_absent_damaged_and_unreadable_without_writes() {
        let root = susi_paths::tempfile::tempdir().unwrap();
        let path = root.path().join("mcp_config.json");
        assert!(read_mcp_config(&path).unwrap().mcp_servers.is_empty());
        assert!(!path.exists());
        fs::write(&path, "{broken").unwrap();
        assert_eq!(read_mcp_config(&path).unwrap_err(), "ERROR_CONFIG_DAMAGED");
        assert_eq!(fs::read_to_string(&path).unwrap(), "{broken");
        assert_eq!(
            read_mcp_config(root.path()).unwrap_err(),
            "ERROR_CONFIG_UNREADABLE"
        );
    }

    #[test]
    fn stored_and_literal_command_preparation_preserve_their_distinct_inputs() {
        let policy = MacPolicy::new([7u8; 32], PrivacyMode::Balanced, false);
        let mut config = server("fixture-peer", None);
        config.args.push("--literal-argument".into());
        config.extra.insert("cwd".into(), json!("{workspace}/peer"));
        config.env = Some(HashMap::from([("FIXTURE_VALUE".into(), "value".into())]));
        let stored = prepare_authorized_stdio_command(&config, &policy).unwrap();
        assert_eq!(
            stored.get_current_dir(),
            Some(std::env::current_dir().unwrap().join("peer").as_path())
        );
        config
            .env
            .as_mut()
            .unwrap()
            .insert("FIXTURE_VALUE".into(), String::new());
        config
            .extra
            .insert("env_keys".into(), json!(["FIXTURE_UNDECLARED"]));
        let literal = authorized_stdio_command_literal(&config, &policy).unwrap();
        assert_eq!(literal.get_program(), "fixture-peer");
        assert_eq!(
            literal.get_args().collect::<Vec<_>>(),
            ["--literal-argument"]
        );
        assert_eq!(
            literal.get_current_dir(),
            Some(std::path::Path::new("{workspace}/peer"))
        );
        let environment: HashMap<_, _> = literal.get_envs().collect();
        assert_eq!(
            environment.get(std::ffi::OsStr::new("FIXTURE_VALUE")),
            Some(&Some(std::ffi::OsStr::new("")))
        );
        assert!(!environment.contains_key(std::ffi::OsStr::new("FIXTURE_UNDECLARED")));
        assert!(prepare_authorized_stdio_command(&config, &local_only()).is_err());
    }
}
