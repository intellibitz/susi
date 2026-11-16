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
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
pub use susi_config::mcp::{
    authorize_mcp_transport, authorized_stdio_command_literal, spawn_authorized_stdio_peer,
};
use susi_config::{
    mac_policy::MacPolicy,
    mcp::{authorize_mcp_transport as authorize_transport, validate_http_headers, ParsedHttpUrl},
};
use susi_core::plane_bus::PlaneBus;

pub const TOPIC_MCP_CALL: &str = "ecosystem_hub.mcp.call";
pub const TOPIC_MCP_LIST_TOOLS: &str = "ecosystem_hub.mcp.list_tools";

fn config() -> crate::susi_sandbox::manager::SusiConfig {
    crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default()
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

fn http_jsonrpc_exchange(
    parsed: &ParsedHttpUrl,
    headers: &[(String, String)],
    request_body: &Value,
    timeout: Duration,
) -> Result<(Option<String>, Option<Value>), String> {
    let host_lookup = parsed.host().trim_matches(['[', ']']);
    let sock_addr: SocketAddr = if parsed
        .host()
        .trim_end_matches('.')
        .eq_ignore_ascii_case("localhost")
    {
        SocketAddr::from(([127, 0, 0, 1], parsed.port()))
    } else if let Ok(ip) = host_lookup.parse::<IpAddr>() {
        SocketAddr::new(ip, parsed.port())
    } else {
        (host_lookup, parsed.port())
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
        parsed.path(), parsed.host(), parsed.port(), payload.len()
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
    let mut command = if config.extra.contains_key("inherit_parent_env") {
        susi_config::mcp::prepare_authorized_stdio_command(config, MacPolicy::global())?
    } else {
        susi_config::mcp::prepare_stdio_command(config)
    };
    command.spawn().map_err(|error| error.to_string())
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

#[cfg(test)]
#[path = "tests/mcp_budget.rs"]
mod tests;
