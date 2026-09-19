// GMCP Server Substrate: Model Context Protocol JSON-RPC 2.0 Interface
// 100% Rust implementation serving Tier 1 Swarm & ToolRegistry
//
// Async Defaults (Mandate 28): connection accept/read/write is tokio-native
// (hyper). Request handling ultimately runs SusiMasterAgent::solve_clean, a
// synchronous, CPU-bound swarm/agent execution graph (rayon-based) — that
// work is dispatched via tokio::task::spawn_blocking rather than pretending
// it is non-blocking, since running it directly on a tokio worker thread
// would stall the whole reactor for every other in-flight connection.
//
// Transport honesty: GET /sse is a one-shot MCP endpoint advertisement (not a
// keep-alive event bus). Tool traffic is POST /messages JSON-RPC.

use bytes::{Bytes, BytesMut};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use serde_json::{json, Value};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::tools::ToolRegistry;
use crate::ProtocolDispatcher;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn full_body<T: Into<Bytes>>(chunk: T) -> BoxBody {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}

fn cors_origin_header() -> HeaderValue {
    let origin = susi_sandbox::manager::SusiConfig::load_global()
        .unwrap_or_default()
        .allow_origin();
    HeaderValue::from_str(&origin).unwrap_or_else(|_| HeaderValue::from_static("*"))
}

fn max_rpc_body_bytes() -> usize {
    susi_sandbox::manager::SusiConfig::load_global()
        .unwrap_or_default()
        .max_rpc_body_bytes()
}

fn response_builder(
    status: StatusCode,
    content_type: &'static str,
    body: impl Into<Bytes>,
) -> Response<BoxBody> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, HeaderValue::from_static(content_type))
        .header("Access-Control-Allow-Origin", cors_origin_header())
        .header(
            "Access-Control-Allow-Methods",
            HeaderValue::from_static("GET, POST, OPTIONS"),
        )
        .header(
            "Access-Control-Allow-Headers",
            HeaderValue::from_static("Content-Type, Authorization"),
        )
        .body(full_body(body))
        .unwrap_or_else(|_| {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(full_body("Internal Server Error"))
                .unwrap_or_else(|_| Response::new(full_body("Internal Server Error")))
        })
}

/// Mandate 12: stream-collect the body with a hard byte ceiling.
async fn read_body_bounded(body: Incoming, max_bytes: usize) -> Result<Bytes, String> {
    let mut collected = BytesMut::new();
    let mut body = body;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| format!("Body read error: {}", e))?;
        if let Ok(data) = frame.into_data() {
            if collected.len().saturating_add(data.len()) > max_bytes {
                return Err(format!("Body exceeds {} bytes limit", max_bytes));
            }
            collected.extend_from_slice(&data);
        }
    }
    Ok(collected.freeze())
}

pub struct GmcpServer;

impl GmcpServer {
    pub fn run_stdio(workspace: &Path, version: &str) {
        eprintln!(
            "[GMCP Server] Started v{} (stdio JSON-RPC, bounded limits, full LSP framing).",
            version
        );
        let workspace = workspace.to_path_buf();
        let max_bytes = max_rpc_body_bytes();
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("[GMCP Server] Failed to start stdio runtime: {}", e);
                return;
            }
        };

        rt.block_on(async move {
            use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
            let mut reader = BufReader::new(tokio::io::stdin());
            let mut stdout = tokio::io::stdout();

            loop {
                let mut content_length: usize = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return; // EOF
                    }
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        break;
                    }
                    if trimmed.to_lowercase().starts_with("content-length:") {
                        if let Ok(l) = trimmed[15..].trim().parse::<usize>() {
                            content_length = l;
                        }
                    }
                }

                if content_length == 0 {
                    continue;
                }

                if content_length > max_bytes {
                    let err = json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32600, "message": format!("Request {} > limit {}", content_length, max_bytes) }
                    }).to_string();
                    let out = format!("Content-Length: {}\r\n\r\n{}", err.len(), err);
                    let _ = stdout.write_all(out.as_bytes()).await;
                    let _ = stdout.flush().await;
                    continue; // we don't drain the invalid buffer, we just let it fail on next parse
                }

                let mut buf = vec![0u8; content_length];
                if reader.read_exact(&mut buf).await.is_err() {
                    break;
                }

                let line = match String::from_utf8(buf) {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                let ws = workspace.clone();
                let response = tokio::task::spawn_blocking(move || {
                    GmcpProtocolHandler.handle_request(&line, &ws)
                })
                .await
                .unwrap_or_else(|e| {
                    json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32000, "message": format!("Task join error: {}", e) }
                    })
                    .to_string()
                });

                if response.is_empty() {
                    continue;
                }

                let out = format!("Content-Length: {}\r\n\r\n{}", response.len(), response);
                let _ = stdout.write_all(out.as_bytes()).await;
                let _ = stdout.flush().await;
            }
        });
    }

    pub fn start_http_server(workspace: PathBuf, listener: std::net::TcpListener) {
        let addr = listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        // Honest banner: endpoint discovery + POST JSON-RPC, not a live SSE bus.
        eprintln!(
            "[GMCP HTTP] Endpoint-discovery (/sse) + JSON-RPC (/messages) active on {}",
            addr
        );

        // Runs on its own daemon thread (caller wraps it in catch_unwind), so
        // a failure here only loses the GMCP HTTP surface, not the whole
        // daemon — but log clearly and return instead of panicking with a
        // raw message, since nothing retries this subsystem.
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!(
                    "[GMCP HTTP] Failed to start HTTP runtime: {}. GMCP HTTP is unavailable.",
                    e
                );
                return;
            }
        };
        let max_conns = susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .max_concurrent_agents();
        let active_conns = Arc::new(AtomicUsize::new(0));

        rt.block_on(async move {
            if let Err(e) = listener.set_nonblocking(true) {
                eprintln!("[GMCP HTTP] Failed to set listener non-blocking: {}. GMCP HTTP is unavailable.", e);
                return;
            }
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[GMCP HTTP] Failed to adopt listener into tokio runtime: {}. GMCP HTTP is unavailable.", e);
                    return;
                }
            };
            let workspace = Arc::new(workspace);

            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("[GMCP HTTP] Accept error: {}", e);
                        continue;
                    }
                };

                let current = active_conns.load(Ordering::Relaxed);
                if current >= max_conns {
                    eprintln!(
                        "[GMCP HTTP] Connection rejected: active {} >= max {}",
                        current, max_conns
                    );
                    drop(stream);
                    continue;
                }
                active_conns.fetch_add(1, Ordering::Relaxed);

                let workspace = Arc::clone(&workspace);
                let active_conns = Arc::clone(&active_conns);
                let peer_ip = peer.ip();
                tokio::spawn(async move {
                    struct ConnGuard(Arc<AtomicUsize>);
                    impl Drop for ConnGuard {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::Relaxed);
                        }
                    }
                    let _guard = ConnGuard(active_conns);

                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        let workspace = Arc::clone(&workspace);
                        async move { handle_gmcp_request(req, workspace, peer_ip).await }
                    });
                    if let Err(e) = AutoBuilder::new(TokioExecutor::new())
                        .serve_connection(io, service)
                        .await
                    {
                        eprintln!("[GMCP HTTP] Connection error: {}", e);
                    }
                });
            }
        });
    }
}

async fn handle_gmcp_request(
    req: Request<Incoming>,
    workspace: Arc<PathBuf>,
    peer_ip: std::net::IpAddr,
) -> Result<Response<BoxBody>, Infallible> {
    let path = req.uri().path();

    // CORS preflight never carries an Authorization header (browsers won't
    // send credentials on OPTIONS), so it must always pass through.
    if req.method() == Method::OPTIONS {
        return Ok(response_builder(
            StatusCode::NO_CONTENT,
            "text/plain",
            Bytes::new(),
        ));
    }

    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    if !susi_agents::net_guard::NetGuard::is_authorized(
        req.headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok()),
    ) {
        return Ok(response_builder(
            StatusCode::UNAUTHORIZED,
            "application/json",
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32001, "message": "Unauthorized"}})
                .to_string(),
        ));
    }
    if !susi_agents::net_guard::RateLimiter::global().check(peer_ip, cfg.rate_limit_per_minute()) {
        return Ok(response_builder(
            StatusCode::TOO_MANY_REQUESTS,
            "application/json",
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32002, "message": "Rate limit exceeded"}})
                .to_string(),
        ));
    }

    match (req.method(), path) {
        (&Method::GET, "/sse") => {
            // One-shot endpoint advertisement for MCP clients that expect an
            // `event: endpoint` before POSTing to /messages. Not a keep-alive stream.
            let endpoint_event = "event: endpoint\ndata: /messages\n\n";
            let mut res = response_builder(
                StatusCode::OK,
                "text/event-stream",
                endpoint_event.as_bytes(),
            );
            res.headers_mut().insert(
                hyper::header::CACHE_CONTROL,
                HeaderValue::from_static("no-cache"),
            );
            Ok(res)
        }
        (&Method::POST, "/messages") => {
            let max_bytes = max_rpc_body_bytes();
            let body_bytes = match read_body_bounded(req.into_body(), max_bytes).await {
                Ok(b) => b,
                Err(msg) => {
                    return Ok(response_builder(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "application/json",
                        json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": { "code": -32600, "message": msg }
                        })
                        .to_string(),
                    ));
                }
            };
            let body = String::from_utf8_lossy(&body_bytes).to_string();
            let ws = (*workspace).clone();

            let response_json =
                tokio::task::spawn_blocking(move || GmcpProtocolHandler.handle_request(&body, &ws))
                    .await
                    .unwrap_or_else(|e| {
                        json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": {
                                "code": -32000,
                                "message": format!("Mission Interrupted: {}", e)
                            }
                        })
                        .to_string()
                    });

            // Notifications return empty — acknowledge with 204.
            if response_json.is_empty() {
                return Ok(response_builder(
                    StatusCode::NO_CONTENT,
                    "text/plain",
                    Bytes::new(),
                ));
            }

            Ok(response_builder(
                StatusCode::OK,
                "application/json",
                response_json,
            ))
        }
        _ => Ok(response_builder(
            StatusCode::NOT_FOUND,
            "text/plain",
            "Not Found",
        )),
    }
}

/// GMCP Protocol Handler: Decoupled JSON-RPC implementation for the Substrate.
pub struct GmcpProtocolHandler;

impl ProtocolDispatcher for GmcpProtocolHandler {
    fn handle_request(&self, line: &str, workspace: &Path) -> String {
        let parsed: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                return json!({
                    "jsonrpc": "2.0",
                    "id": null,
                    "error": {
                        "code": -32700,
                        "message": format!("Parse error: {}", e)
                    }
                })
                .to_string();
            }
        };

        let method = parsed
            .get("method")
            .and_then(|m| m.as_str())
            .map(|s| s.to_string());
        let id = parsed.get("id").cloned();

        // JSON-RPC notifications (method present, id absent) must not receive a response.
        if method.is_some() && id.is_none() {
            return String::new();
        }

        let id = id.unwrap_or(Value::Null);

        match method.as_deref() {
            Some("initialize") => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": { "name": "susi-substrate", "version": env!("CARGO_PKG_VERSION") }
                }
            })
            .to_string(),
            Some("ping") => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {}
            })
            .to_string(),
            Some("tools/list") => {
                let tools = ToolRegistry::list_tools();
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "tools": tools
                    }
                })
                .to_string()
            }
            Some("tools/call") => {
                let params = parsed.get("params").cloned().unwrap_or(Value::Null);
                let tool_name = params
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let tool_arg = params.get("arguments").cloned().unwrap_or(Value::Null);

                let result_text = if tool_name.is_empty() {
                    "Error: tools/call missing params.name".to_string()
                } else if ToolRegistry::exists(&tool_name) {
                    ToolRegistry::execute_tool(&tool_name, &tool_arg, workspace)
                } else {
                    format!("Error: Tool '{}' not found in registry", tool_name)
                };

                let is_error = is_tool_result_error(&result_text);

                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "isError": is_error,
                        "content": [
                            { "type": "text", "text": result_text }
                        ]
                    }
                })
                .to_string()
            }
            Some(_) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            })
            .to_string(),
            None => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32600, "message": "Invalid Request: missing method" }
            })
            .to_string(),
        }
    }
}

/// Honest MCP/tool failure detection (Mandate 1): registry and swarm paths use
/// several failure prefixes, not only `Error:`.
fn is_tool_result_error(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("Error:")
        || t.starts_with("[FAIL]")
        || t.starts_with("[CAPABILITY_GAP]")
        || t.starts_with("Protocol Error:")
        || t.starts_with("Reflex Error:")
        || t.starts_with("Governance Violation:")
        || t.starts_with("[RECOVERY]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_parse_error_returns_minus_32700() {
        let out = GmcpProtocolHandler.handle_request("{not-json", &PathBuf::from("."));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["code"], -32700);
    }

    #[test]
    fn test_notification_yields_empty_response() {
        let out = GmcpProtocolHandler.handle_request(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &PathBuf::from("."),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn test_ping_returns_result() {
        let out = GmcpProtocolHandler.handle_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#,
            &PathBuf::from("."),
        );
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v.get("result").is_some());
        assert_eq!(v["id"], 1);
    }

    #[test]
    fn test_is_tool_result_error_covers_fail_prefixes() {
        assert!(is_tool_result_error("[FAIL] boom"));
        assert!(is_tool_result_error("[CAPABILITY_GAP] missing"));
        assert!(is_tool_result_error("Error: nope"));
        assert!(is_tool_result_error("Protocol Error: bad"));
        assert!(!is_tool_result_error("ok content"));
    }

    #[test]
    fn test_susi_solve_is_registered() {
        susi_tools::hooks::init(Box::new(crate::tools::SusiEngineHooks));
        assert!(ToolRegistry::exists("susi_solve"));
    }
}
