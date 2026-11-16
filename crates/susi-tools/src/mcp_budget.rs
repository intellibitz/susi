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
) -> Result<(Option<String>, Option<Value>), String> {
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
    let mut stream = TcpStream::connect_timeout(&sock_addr, timeout.min(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let payload = serde_json::to_vec(request_body).map_err(|e| e.to_string())?;
    let mut req = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n",
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
    let mut raw = Vec::new();
    stream
        .take(16 * 1024 * 1024)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, raw_body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed HTTP response from MCP server".to_string())?;
    let status: u16 = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| "malformed HTTP status line from MCP server".to_string())?;
    if status >= 400 {
        return Err(format!("MCP HTTP {status}: {}", raw_body.trim()));
    }
    let mut session_id = None;
    let mut chunked = false;
    for line in head.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("mcp-session-id") {
                session_id = Some(v.trim().to_string());
            } else if k.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }
    let body = if chunked {
        decode_chunked_text(raw_body)
    } else {
        raw_body.to_string()
    };
    let trimmed = body.trim();
    if trimmed.is_empty() || status == 202 {
        return Ok((session_id, None));
    }
    if let Ok(val) = serde_json::from_str::<Value>(trimmed) {
        return Ok((session_id, Some(val)));
    }
    let want_id = request_body.get("id");
    for line in body.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            if let Ok(val) = serde_json::from_str::<Value>(data.trim()) {
                if want_id.is_none() || val.get("id") == want_id {
                    return Ok((session_id, Some(val)));
                }
            }
        }
    }
    Err("MCP HTTP response carried no matching JSON-RPC payload".to_string())
}

fn decode_chunked_text(mut input: &str) -> String {
    let mut out = String::new();
    while let Some((size_line, rest)) = input.split_once("\r\n") {
        let hex = size_line.split(';').next().unwrap_or("").trim();
        let Ok(len) = usize::from_str_radix(hex, 16) else {
            break;
        };
        if len == 0 || rest.len() < len {
            break;
        }
        out.push_str(&rest[..len]);
        input = rest[len..].strip_prefix("\r\n").unwrap_or(&rest[len..]);
    }
    out
}

fn spawn_stdio_command(config: &McpServerConfig) -> Result<std::process::Child, String> {
    stdio_command(config, false)
        .spawn()
        .map_err(|e| e.to_string())
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
            if let Ok(value) = crate::susi_config::env_or_cloud_env(key) {
                if !value.is_empty() {
                    command.env(key, value);
                }
            }
        }
    }
    if let Some(env) = &config.env {
        for (k, v) in env {
            if !literal_environment && v.trim().is_empty() {
                if let Ok(resolved) = crate::susi_config::env_or_cloud_env(k) {
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
        let init_req = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"susi","version":"0.1.0"}}});
        let (sid, init_val) = match http_jsonrpc_exchange(&parsed, &headers, &init_req, handshake) {
            Ok(pair) => pair,
            Err(e) => {
                record_connect_failure(&key);
                return Err(e);
            }
        };
        clear_connect_failure(&key);
        let mut sess_headers = headers;
        if let Some(id) = sid {
            sess_headers.push(("Mcp-Session-Id".to_string(), id));
        }
        let ver = init_val
            .as_ref()
            .and_then(|v| v.get("result"))
            .and_then(|r| r.get("protocolVersion"))
            .and_then(Value::as_str)
            .unwrap_or("2025-11-25");
        sess_headers.push(("MCP-Protocol-Version".to_string(), ver.to_string()));
        let _ = http_jsonrpc_exchange(
            &parsed,
            &sess_headers,
            &json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
            handshake,
        );
        let req = json!({"jsonrpc":"2.0","id":2,"method":method,"params":params});
        let (_, resp_opt) = http_jsonrpc_exchange(&parsed, &sess_headers, &req, lease)?;
        let resp = resp_opt.ok_or_else(|| "MCP HTTP response carried empty body".to_string())?;
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

/// Enforce `MacPolicy` transport and capability authorization for `server` (`CLAIM-04` / `HOST-002`).
pub fn authorize_mcp_transport(server: &McpServerConfig, policy: &MacPolicy) -> Result<(), String> {
    authorize_transport(server, policy)
}

/// Authorize `config` against `policy` (`CLAIM-04` / `HOST-002`) and spawn an
/// environment-isolated, windowless stdio MCP child process ready for [`McpHost`]
/// duplex attachment.
pub fn spawn_authorized_stdio_peer(
    config: &McpServerConfig,
    policy: &MacPolicy,
) -> Result<std::process::Child, String> {
    if config.parse_http_url()?.is_some() {
        return Err("spawn_authorized_stdio_peer requires a stdio command, not an HTTP URL".into());
    }
    authorize_transport(config, policy)?;
    spawn_stdio_command(config)
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
    authorize_transport(config, policy)?;
    Ok(stdio_command(config, true))
}

#[cfg(test)]
#[path = "tests/mcp_budget.rs"]
mod tests;
