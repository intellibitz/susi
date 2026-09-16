// GMCP Server Substrate: Model Context Protocol JSON-RPC 2.0 Interface
// 100% Rust implementation serving Tier 1 Swarm & ToolRegistry
//
// Async Defaults (Mandate 28): connection accept/read/write is tokio-native
// (hyper). Request handling ultimately runs SusiMasterAgent::solve_clean, a
// synchronous, CPU-bound swarm/agent execution graph (rayon-based) — that
// work is dispatched via tokio::task::spawn_blocking rather than pretending
// it is non-blocking, since running it directly on a tokio worker thread
// would stall the whole reactor for every other in-flight connection.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder as AutoBuilder;
use serde_json::json;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::gmcp::tools::ToolRegistry;
use crate::gmcp::ProtocolDispatcher;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

fn full_body<T: Into<Bytes>>(chunk: T) -> BoxBody {
    Full::new(chunk.into()).map_err(|never| match never {}).boxed()
}

pub struct GmcpServer;

impl GmcpServer {
    pub fn run_stdio(workspace: &Path, _version: &str) {
        eprintln!("[GMCP Server] Started (Listening on stdio).");
        let workspace = workspace.to_path_buf();
        let rt = tokio::runtime::Runtime::new()
            .expect("Fatal: failed to start GMCP stdio runtime");

        rt.block_on(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
            let mut lines = BufReader::new(tokio::io::stdin()).lines();
            let mut stdout = tokio::io::stdout();

            while let Ok(Some(line)) = lines.next_line().await {
                let ws = workspace.clone();
                let response = tokio::task::spawn_blocking(move || {
                    GmcpProtocolHandler.handle_request(&line, &ws)
                })
                .await
                .unwrap_or_else(|e| {
                    json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32000, "message": format!("Task join error: {}", e) }
                    })
                    .to_string()
                });

                let _ = stdout
                    .write_all(format!("{}\n", response).as_bytes())
                    .await;
                let _ = stdout.flush().await;
            }
        });
    }

    pub fn start_tcp_server(_workspace: PathBuf, _port: u16, _version: String) {
        // TCP server now consolidated into HTTP/SSE via hyper for reliability
        eprintln!("[GMCP TCP] Protocol deprecated. Use GMCP HTTP/SSE on 9093.");
    }

    pub fn start_http_server(workspace: PathBuf, listener: std::net::TcpListener) {
        let addr = listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_default();
        eprintln!("[GMCP HTTP/SSE] Substrate active on {}", addr);

        let rt = tokio::runtime::Runtime::new()
            .expect("Fatal: failed to start GMCP HTTP runtime");

        rt.block_on(async move {
            listener
                .set_nonblocking(true)
                .expect("Fatal: failed to set GMCP listener non-blocking");
            let listener = tokio::net::TcpListener::from_std(listener)
                .expect("Fatal: failed to adopt GMCP listener into the tokio runtime");
            let workspace = Arc::new(workspace);

            loop {
                let (stream, _peer) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        eprintln!("[GMCP HTTP] Accept error: {}", e);
                        continue;
                    }
                };
                let workspace = Arc::clone(&workspace);

                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let service = service_fn(move |req| {
                        let workspace = Arc::clone(&workspace);
                        async move { handle_gmcp_request(req, workspace).await }
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
) -> Result<Response<BoxBody>, Infallible> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/sse") => {
            let endpoint_event = format!(
                "event: endpoint\ndata: /messages?session={}\n\n",
                "default-session"
            );
            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"))
                .header("Cache-Control", HeaderValue::from_static("no-cache"))
                .header(
                    "Access-Control-Allow-Origin",
                    HeaderValue::from_static("*"),
                )
                .body(full_body(endpoint_event))
                .unwrap())
        }
        (&Method::POST, path) if path.starts_with("/messages") => {
            let body_bytes = req
                .into_body()
                .collect()
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            let body = String::from_utf8_lossy(&body_bytes).to_string();
            let ws = (*workspace).clone();

            let response_json = tokio::task::spawn_blocking(move || {
                GmcpProtocolHandler.handle_request(&body, &ws)
            })
            .await
            .unwrap_or_else(|e| {
                json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32000, "message": format!("Mission Interrupted: {}", e) }
                })
                .to_string()
            });

            Ok(Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
                .header(
                    "Access-Control-Allow-Origin",
                    HeaderValue::from_static("*"),
                )
                .body(full_body(response_json))
                .unwrap())
        }
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(full_body("Not Found"))
            .unwrap()),
    }
}

/// GMCP Protocol Handler: Decoupled JSON-RPC implementation for the Substrate.
pub struct GmcpProtocolHandler;

impl ProtocolDispatcher for GmcpProtocolHandler {
    fn handle_request(&self, line: &str, workspace: &Path) -> String {
        let id = extract_id(line);
        let method = extract_method(line);

        match method.as_deref() {
            Some("initialize") => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": { "listChanged": false }
                    },
                    "serverInfo": { "name": "susi-substrate", "version": crate::SUSI_VERSION }
                }
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
                let tool_name = extract_tool_name(line).unwrap_or_default();
                let tool_arg = extract_tool_val(line).unwrap_or(json!(null));

                // SUSI Is Swarm: Route all GMCP operations through the Substrate Master Agent
                let intent = format!("{} {}", tool_name, tool_arg);
                let ama = crate::gawd::ama::SusiMasterAgent::new();
                let result_text = ama.solve_clean(&intent, workspace, crate::SUSI_VERSION);

                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            { "type": "text", "text": result_text }
                        ]
                    }
                })
                .to_string()
            }
            _ => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": "Method not found" }
            })
            .to_string(),
        }
    }
}

fn extract_id(line: &str) -> serde_json::Value {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        if let Some(id) = v.get("id") {
            return id.clone();
        }
    }
    json!(null)
}

fn extract_method(line: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        if let Some(m) = v.get("method").and_then(|m| m.as_str()) {
            return Some(m.to_string());
        }
    }
    None
}

fn extract_tool_name(line: &str) -> Option<String> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        if let Some(params) = v.get("params") {
            if let Some(name) = params.get("name").and_then(|n| n.as_str()) {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn extract_tool_val(line: &str) -> Option<serde_json::Value> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
        if let Some(params) = v.get("params") {
            if let Some(arguments) = params.get("arguments") {
                return Some(arguments.clone());
            }
        }
    }
    None
}
