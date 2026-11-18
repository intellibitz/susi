//! Shared MCP registry wire types and strict file decoding.
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, fs, net::IpAddr, path::PathBuf};

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
/// `extra.native_command` may carry a canonical native path whose permission
/// identity is `command`; parent environment inheritance requires an explicit
/// flag and an exact executable capability in addition to execution permission.
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
