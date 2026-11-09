//! SUSI policy and built-in/PlaneBus MCP client transport: lease and handshake
//! budgets come from the global `SusiConfig` (execution lease, cloud-scout
//! timeout). When `susi-ecosystem-hub` is attached on `PlaneBus`, requests can
//! route through `"ecosystem_hub.mcp.call"` / `"ecosystem_hub.mcp.list_tools"`;
//! otherwise the std JSON-RPC 2.0 client (stdio and loopback/HTTP) executes
//! directly with connect-failure cooldown.

use crate::McpServerConfig;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::{BufRead, BufReader, Read, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    process::{Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use susi_core::{
    mac_policy::{actions, MacPolicy},
    plane_bus::PlaneBus,
};

pub const TOPIC_MCP_CALL: &str = "ecosystem_hub.mcp.call";
pub const TOPIC_MCP_LIST_TOOLS: &str = "ecosystem_hub.mcp.list_tools";

#[derive(Debug, Clone)]
struct ParsedHttpUrl {
    scheme: String,
    host: String,
    port: u16,
    default_port: bool,
    path: String,
    is_loopback: bool,
}

impl McpServerConfig {
    fn parse_http_url(&self) -> Result<Option<ParsedHttpUrl>, String> {
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

fn config() -> crate::susi_sandbox::manager::SusiConfig {
    crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default()
}

fn authorize_transport(server: &McpServerConfig, policy: &MacPolicy) -> Result<(), String> {
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

/// Per-server connect-failure memory. A spawn/handshake failure is cooled
/// for `CONNECT_COOLDOWN` so every subsequent caller doesn't re-pay the
/// spawn cost of a remote that cannot start.
static CONNECT_FAILURES: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
const CONNECT_COOLDOWN: Duration = Duration::from_secs(300);

fn check_cooldown(key: &str) -> Result<(), String> {
    let failures = CONNECT_FAILURES.get_or_init(Default::default);
    let guard = failures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(at) = guard.get(key) {
        if at.elapsed() < CONNECT_COOLDOWN {
            return Err("MCP server connect cooled after a recent failure".to_string());
        }
    }
    Ok(())
}

fn record_connect_failure(key: &str) {
    let failures = CONNECT_FAILURES.get_or_init(Default::default);
    let mut guard = failures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.len() > 64 {
        guard.clear();
    }
    guard.insert(key.to_owned(), Instant::now());
}

fn clear_connect_failure(key: &str) {
    let failures = CONNECT_FAILURES.get_or_init(Default::default);
    let mut guard = failures
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.remove(key);
}

fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn validate_http_headers(config: &McpServerConfig) -> Result<Vec<(String, String)>, String> {
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

fn http_jsonrpc_exchange(
    parsed: &ParsedHttpUrl,
    headers: &[(String, String)],
    request_body: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    let host_lookup = parsed.host.trim_matches(['[', ']']);
    let sock_addr: SocketAddr = if parsed
        .host
        .trim_end_matches('.')
        .eq_ignore_ascii_case("localhost")
    {
        SocketAddr::from(([127, 0, 0, 1], parsed.port))
    } else if let Ok(ip) = host_lookup.parse::<IpAddr>() {
        SocketAddr::new(ip, parsed.port)
    } else {
        (host_lookup, parsed.port)
            .to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or_else(|| "unable to resolve MCP HTTP host".to_string())?
    };
    let connect_budget = timeout.min(Duration::from_secs(10));
    let mut stream =
        TcpStream::connect_timeout(&sock_addr, connect_budget).map_err(|e| e.to_string())?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let payload = serde_json::to_vec(request_body).map_err(|e| e.to_string())?;
    let mut req = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n",
        parsed.path, parsed.host, parsed.port, payload.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    stream
        .write_all(req.as_bytes())
        .and_then(|()| stream.write_all(&payload))
        .and_then(|()| stream.flush())
        .map_err(|e| e.to_string())?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw).map_err(|e| e.to_string())?;
    let (_, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response from MCP server".to_string())?;
    serde_json::from_str::<Value>(body.trim()).map_err(|e| e.to_string())
}

fn spawn_stdio_command(config: &McpServerConfig) -> Result<std::process::Child, String> {
    let mut command = Command::new(&config.command);
    // A stdio server is a console program launched unattended (from the
    // detached daemon too); without this Windows opens a window for it.
    susi_paths::console::without_window(&mut command)
        .args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.env_clear();
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
    if let Some(env) = &config.env {
        command.envs(env);
    }
    if let Some(cwd) = config.extra.get("cwd").and_then(Value::as_str) {
        if cwd.contains("{workspace}") {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            command.current_dir(cwd.replace("{workspace}", &ws.to_string_lossy()));
        } else {
            command.current_dir(cwd);
        }
    }
    command.spawn().map_err(|e| e.to_string())
}

fn read_jsonrpc_response(
    reader: &mut BufReader<std::process::ChildStdout>,
    expected_id: u64,
) -> Result<Value, String> {
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("MCP stdio server closed stdout before replying".to_string());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if msg.get("id").and_then(Value::as_u64) == Some(expected_id) {
            if let Some(err) = msg.get("error") {
                return Err(err.to_string());
            }
            return msg
                .get("result")
                .cloned()
                .ok_or_else(|| "MCP JSON-RPC response missing `result`".to_string());
        }
    }
}

fn execute_mcp_rpc(
    config: &McpServerConfig,
    method: &str,
    params: Value,
    lease: Duration,
    handshake: Duration,
) -> Result<Value, String> {
    let key = serde_json::to_value(config)
        .map_err(|e| e.to_string())?
        .to_string();
    check_cooldown(&key)?;

    if let Some(parsed) = match config.parse_http_url() {
        Ok(p) => p,
        Err(e) => {
            record_connect_failure(&key);
            return Err(e);
        }
    } {
        let headers = match validate_http_headers(config) {
            Ok(h) => h,
            Err(e) => {
                record_connect_failure(&key);
                return Err(e);
            }
        };
        let init_req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "susi", "version": "0.1.0" }
            }
        });
        if let Err(e) = http_jsonrpc_exchange(&parsed, &headers, &init_req, handshake) {
            record_connect_failure(&key);
            return Err(e);
        }
        clear_connect_failure(&key);
        let req = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": method,
            "params": params
        });
        let resp = http_jsonrpc_exchange(&parsed, &headers, &req, lease)?;
        if let Some(err) = resp.get("error") {
            return Err(err.to_string());
        }
        return resp
            .get("result")
            .cloned()
            .ok_or_else(|| "MCP HTTP response missing `result`".to_string());
    }

    let mut child = match spawn_stdio_command(config) {
        Ok(c) => c,
        Err(e) => {
            record_connect_failure(&key);
            return Err(e);
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        record_connect_failure(&key);
        return Err("MCP stdio child missing stdin".to_string());
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        record_connect_failure(&key);
        return Err("MCP stdio child missing stdout".to_string());
    };

    let method_owned = method.to_owned();
    let (tx, rx) =
        std::sync::mpsc::sync_channel::<Result<(bool, Result<Value, String>), String>>(1);
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let init_line = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "susi", "version": "0.1.0" }
            }
        })
        .to_string();
        if let Err(e) = writeln!(stdin, "{init_line}").and_then(|()| stdin.flush()) {
            let _ = tx.send(Ok((false, Err(e.to_string()))));
            return;
        }
        if let Err(e) = read_jsonrpc_response(&mut reader, 1) {
            let _ = tx.send(Ok((false, Err(e))));
            return;
        }
        let initialized_line = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        })
        .to_string();
        let _ = writeln!(stdin, "{initialized_line}").and_then(|()| stdin.flush());
        let call_line = json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": method_owned,
            "params": params
        })
        .to_string();
        if let Err(e) = writeln!(stdin, "{call_line}").and_then(|()| stdin.flush()) {
            let _ = tx.send(Ok((true, Err(e.to_string()))));
            return;
        }
        let res = read_jsonrpc_response(&mut reader, 2);
        let _ = tx.send(Ok((true, res)));
    });

    let total_budget = lease.max(handshake);
    let outcome = rx.recv_timeout(total_budget);
    let _ = child.kill();
    let _ = child.wait();

    match outcome {
        Ok(Ok((false, Err(e)))) => {
            record_connect_failure(&key);
            Err(e)
        }
        Ok(Ok((true, res))) => {
            clear_connect_failure(&key);
            res
        }
        Ok(Ok((false, Ok(v)))) => {
            clear_connect_failure(&key);
            Ok(v)
        }
        Ok(Err(e)) => {
            record_connect_failure(&key);
            Err(e)
        }
        Err(_) => {
            record_connect_failure(&key);
            Err("MCP execution lease expired".to_string())
        }
    }
}

/// Call `name` on the server with object `arguments`, bounded by `lease`
/// (whole call) and `handshake` (cold connect).
pub fn call_blocking_result(
    config: McpServerConfig,
    name: String,
    arguments: Value,
    lease: Duration,
    handshake: Duration,
) -> Result<String, String> {
    let bus = PlaneBus::global();
    if bus.is_wired(TOPIC_MCP_CALL) {
        let payload = json!({
            "config": config,
            "name": name,
            "arguments": arguments,
            "lease_ms": lease.as_millis() as u64,
            "handshake_ms": handshake.as_millis() as u64,
        });
        let resp = bus.request(TOPIC_MCP_CALL, payload)?;
        if let Some(err) = resp.get("error").and_then(Value::as_str) {
            return Err(err.to_owned());
        }
        if let Some(serialized) = resp.get("result").and_then(Value::as_str) {
            return Ok(serialized.to_owned());
        }
    }
    if !arguments.is_object() {
        return Err("Tool arguments must be an object".to_string());
    }
    let result = execute_mcp_rpc(
        &config,
        "tools/call",
        json!({
            "name": name,
            "arguments": arguments,
        }),
        lease,
        handshake,
    )?;
    let serialized = serde_json::to_string(&result).map_err(|e| e.to_string())?;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        Err(serialized)
    } else {
        Ok(serialized)
    }
}

/// Probe a configured MCP server for its live tool catalog (name +
/// description), bounded by `lease` and `handshake`.
pub fn list_tools_blocking(
    config: McpServerConfig,
    lease: Duration,
    handshake: Duration,
) -> Result<Vec<(String, String)>, String> {
    let bus = PlaneBus::global();
    if bus.is_wired(TOPIC_MCP_LIST_TOOLS) {
        let payload = json!({
            "config": config,
            "lease_ms": lease.as_millis() as u64,
            "handshake_ms": handshake.as_millis() as u64,
        });
        let resp = bus.request(TOPIC_MCP_LIST_TOOLS, payload)?;
        if let Some(err) = resp.get("error").and_then(Value::as_str) {
            return Err(err.to_owned());
        }
        if let Some(arr) = resp.get("tools").and_then(Value::as_array) {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                if let Some(name) = item.get("name").and_then(Value::as_str) {
                    let desc = item
                        .get("description")
                        .and_then(Value::as_str)
                        .map_or_else(|| format!("MCP tool '{name}'"), str::to_owned);
                    out.push((name.to_owned(), desc));
                }
            }
            return Ok(out);
        }
    }
    let result = execute_mcp_rpc(&config, "tools/list", json!({}), lease, handshake)?;
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| "MCP tools/list response missing `tools` array".to_string())?;
    let mut out = Vec::with_capacity(tools.len());
    for tool in tools {
        let Some(name) = tool.get("name").and_then(Value::as_str) else {
            continue;
        };
        let desc = tool
            .get("description")
            .and_then(Value::as_str)
            .map_or_else(|| format!("MCP tool '{name}'"), str::to_owned);
        out.push((name.to_owned(), desc));
    }
    Ok(out)
}

/// Tool call: full execution lease, full cloud-scout handshake budget.
pub(crate) fn call(
    config_: McpServerConfig,
    name: String,
    arguments: Value,
) -> Result<String, String> {
    authorize_transport(&config_, MacPolicy::global())?;
    let cfg = config();
    call_blocking_result(
        config_,
        name,
        arguments,
        Duration::from_secs(cfg.execution_lease_secs()),
        Duration::from_secs(cfg.cloud_scout_timeout_secs()),
    )
}

/// Catalog probe: lease capped at 30s, handshake at 10s.
pub(crate) fn list_tools(config_: McpServerConfig) -> Result<Vec<(String, String)>, String> {
    authorize_transport(&config_, MacPolicy::global())?;
    let cfg = config();
    list_tools_blocking(
        config_,
        Duration::from_secs(cfg.execution_lease_secs().min(30)),
        Duration::from_secs(cfg.cloud_scout_timeout_secs().min(10)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_core::mac_policy::PrivacyMode;

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
        let err = authorize_transport(&server("npx", None), &local_only()).unwrap_err();
        assert!(err.contains(actions::PROCESS_EXEC), "{err}");
        assert!(err.contains("`npx`"), "{err}");

        let balanced = MacPolicy::new([7u8; 32], PrivacyMode::Balanced, false);
        assert_eq!(authorize_transport(&server("npx", None), &balanced), Ok(()));

        let policy = local_only();
        policy
            .grant("susi", actions::PROCESS_EXEC, "npx", None)
            .unwrap();
        assert_eq!(authorize_transport(&server("npx", None), &policy), Ok(()));
    }

    #[test]
    fn remote_http_requires_network_egress_but_loopback_does_not() {
        let policy = local_only();
        let err =
            authorize_transport(&server("https://mcp.example.com/mcp", None), &policy).unwrap_err();
        assert!(err.contains(actions::NETWORK_EGRESS), "{err}");

        for loopback in [
            "http://127.0.0.1:9090/mcp",
            "http://localhost:9093",
            "http://[::1]:9090/mcp",
        ] {
            assert_eq!(
                authorize_transport(&server(loopback, None), &policy),
                Ok(()),
                "{loopback} must stay local"
            );
        }

        let open = MacPolicy::new([7u8; 32], PrivacyMode::Open, false);
        assert_eq!(
            authorize_transport(&server("https://mcp.example.com/mcp", None), &open),
            Ok(())
        );
    }

    #[test]
    fn declared_capabilities_are_validated_and_gated() {
        let policy = local_only();
        let local = "http://127.0.0.1:9090/mcp";

        assert_eq!(
            authorize_transport(
                &server(local, Some(json!(["tools", "filesystem"]))),
                &policy
            ),
            Ok(())
        );

        let err =
            authorize_transport(&server(local, Some(json!(["network"]))), &policy).unwrap_err();
        assert!(err.contains(actions::NETWORK_EGRESS), "{err}");

        let err = authorize_transport(&server(local, Some(json!(["tools", "telepathy"]))), &policy)
            .unwrap_err();
        assert_eq!(err, "MCP capability `telepathy` is not supported");

        let err = authorize_transport(&server(local, Some(json!([42]))), &policy).unwrap_err();
        assert_eq!(err, "MCP capability must be a string");

        let err = authorize_transport(&server(local, Some(json!("tools"))), &policy).unwrap_err();
        assert_eq!(err, "MCP capabilities must be a list");

        assert_eq!(
            authorize_transport(&server(local, Some(json!([]))), &policy),
            Ok(())
        );
    }

    #[test]
    fn transport_denial_precedes_capability_validation() {
        let err = authorize_transport(&server("npx", Some(json!("not-a-list"))), &local_only())
            .unwrap_err();
        assert!(err.contains(actions::PROCESS_EXEC), "{err}");
    }

    #[test]
    fn balanced_policy_permits_loopback_and_subprocess_with_filesystem_capability() {
        let balanced = MacPolicy::new([9u8; 32], PrivacyMode::Balanced, false);
        assert_eq!(
            authorize_transport(
                &server("npx", Some(json!(["tools", "filesystem"]))),
                &balanced
            ),
            Ok(())
        );
    }

    #[test]
    fn registered_http_scope_uses_parsed_authority_and_omits_credentials() {
        let mut config = http_config(HashMap::new());
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

    fn http_config(extra: HashMap<String, Value>) -> McpServerConfig {
        McpServerConfig {
            command: "http://127.0.0.1:1".to_string(),
            args: vec![],
            env: None,
            extra,
        }
    }

    #[test]
    fn connect_failure_cooldown_short_circuits_respawn() {
        let config = McpServerConfig {
            command: "/nonexistent-susi-test-binary-xyz".to_string(),
            args: vec![],
            env: None,
            extra: HashMap::new(),
        };
        let first = list_tools_blocking(
            config.clone(),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );
        assert!(first.is_err());
        let key = serde_json::to_value(&config).expect("json").to_string();
        assert!(CONNECT_FAILURES
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .contains_key(&key));
        let start = Instant::now();
        let second = list_tools_blocking(config, Duration::from_secs(2), Duration::from_secs(2));
        assert!(second
            .unwrap_err()
            .contains("connect cooled after a recent failure"));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn http_transport_builds_config_and_fails_closed() {
        let mut extra = HashMap::new();
        extra.insert("auth_token".to_string(), Value::String("tok".to_string()));
        extra.insert("headers".to_string(), json!({"x-susi": "1"}));
        let err = list_tools_blocking(
            http_config(extra),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect_err("port 1 must refuse");
        assert!(!err.is_empty());
    }

    #[test]
    fn http_transport_rejects_non_string_header_values() {
        let mut extra = HashMap::new();
        extra.insert("headers".to_string(), json!({"x-susi": 1}));
        let err = list_tools_blocking(
            http_config(extra),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .expect_err("numeric header must be rejected");
        assert!(err.contains("header values must be strings"));
    }

    #[test]
    fn http_transport_rejects_invalid_header_names() {
        let mut extra = HashMap::new();
        extra.insert("headers".to_string(), json!({"bad header name": "v"}));
        assert!(list_tools_blocking(
            http_config(extra),
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .is_err());
    }

    #[test]
    fn http_transport_rejects_unparseable_command_url() {
        let mut config = http_config(HashMap::new());
        config.command = "http://".to_string();
        assert!(
            list_tools_blocking(config, Duration::from_secs(1), Duration::from_secs(1)).is_err()
        );
    }

    #[test]
    fn stdio_transport_honours_cwd_workspace_template() {
        let mut extra = HashMap::new();
        extra.insert("cwd".to_string(), Value::String("{workspace}".to_string()));
        let config = McpServerConfig {
            command: "/nonexistent-susi-test-binary-abc".to_string(),
            args: vec![],
            env: None,
            extra,
        };
        assert!(
            list_tools_blocking(config, Duration::from_secs(1), Duration::from_secs(1)).is_err()
        );
    }

    #[test]
    fn call_blocking_result_returns_errors_not_panics() {
        let config = McpServerConfig {
            command: "/nonexistent-susi-test-binary-xyz".to_string(),
            args: vec![],
            env: None,
            extra: HashMap::new(),
        };
        let res = call_blocking_result(
            config,
            "any_tool".to_string(),
            json!({}),
            Duration::from_secs(2),
            Duration::from_secs(2),
        );
        assert!(res.is_err());
    }

    /// The stdio sink starts its child with `CREATE_NO_WINDOW`, observed in
    /// the child: it is alone in a console of its own (a child without the
    /// flag shares the runner's console) and no window shows that console (a
    /// child of a console-less parent, such as the detached daemon, would
    /// get a visible one). Args, `config.env` and the cleared environment
    /// are kept. The probe is PowerShell reading Win32, not an MCP server.
    #[cfg(windows)]
    #[test]
    fn stdio_child_runs_in_its_own_hidden_console() {
        let powershell = std::path::PathBuf::from(
            std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()),
        )
        .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let script = "Add-Type -Name W -Namespace N -MemberDefinition \
            '[DllImport(\"kernel32.dll\")] public static extern System.IntPtr GetConsoleWindow(); \
             [DllImport(\"user32.dll\")] public static extern bool IsWindowVisible(System.IntPtr h); \
             [DllImport(\"kernel32.dll\")] public static extern uint GetConsoleProcessList(uint[] l, uint n);'; \
            $h = [N.W]::GetConsoleWindow(); $l = New-Object 'System.UInt32[]' 64; \
            Write-Output ('attached=' + [N.W]::GetConsoleProcessList($l, 64) + \
            ' visible=' + ($h -ne [System.IntPtr]::Zero -and [N.W]::IsWindowVisible($h)) + \
            ' marker=' + $env:SUSI_NOWINDOW_MARKER + ' cargo=' + [bool]$env:CARGO_MANIFEST_DIR)";
        let config = McpServerConfig {
            command: powershell.to_string_lossy().into_owned(),
            args: ["-NoProfile", "-NonInteractive", "-Command", script]
                .map(String::from)
                .to_vec(),
            env: Some(HashMap::from([(
                "SUSI_NOWINDOW_MARKER".to_string(),
                "kept".to_string(),
            )])),
            extra: HashMap::new(),
        };
        let mut child = spawn_stdio_command(&config).unwrap();
        drop(child.stdin.take());
        // Bounded: a test fails, never hangs.
        let deadline = Instant::now() + Duration::from_secs(120);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("powershell probe did not finish within 120 s");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "attached=1 visible=False marker=kept cargo=False",
            "{out:?}"
        );
    }
}
