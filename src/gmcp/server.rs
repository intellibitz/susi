// GMCP Server Substrate: Model Context Protocol JSON-RPC 2.0 Interface
// 100% Rust implementation serving Tier 1 Swarm & ToolRegistry

use serde_json::json;
use std::path::{Path, PathBuf};
use tiny_http::{Header, Method, Response, Server};

use crate::gmcp::tools::ToolRegistry;
use crate::gmcp::ProtocolDispatcher;

pub struct GmcpServer;

impl GmcpServer {
    pub fn run_stdio(workspace: &Path, _version: &str) {
        eprintln!("[GMCP Server] Started (Listening on stdio).");
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        let server = GmcpProtocolHandler;

        for line in std::io::BufRead::lines(stdin.lock()) {
            let line = match line {
                Ok(l) => l,
                Err(_) => break,
            };

            let response = server.handle_request(&line, workspace);
            let _ = std::io::Write::write_all(&mut stdout, format!("{}\n", response).as_bytes());
            let _ = std::io::Write::flush(&mut stdout);
        }
    }

    pub fn start_tcp_server(_workspace: PathBuf, _port: u16, _version: String) {
        // TCP server now consolidated into HTTP/SSE via tiny_http for reliability
        eprintln!("[GMCP TCP] Protocol deprecated. Use GMCP HTTP/SSE on 9093.");
    }

    pub fn start_http_server(workspace: PathBuf, server: Server) {
        let addr = server.server_addr().to_string();
        eprintln!("[GMCP HTTP/SSE] Substrate active on {}", addr);

        for mut request in server.incoming_requests() {
            let workspace = workspace.clone();
            let method = request.method().clone();
            let url = request.url().to_string();
            let server_handler = GmcpProtocolHandler;

            match (method, url.as_str()) {
                (Method::Get, "/sse") => {
                    let endpoint_event = format!(
                        "event: endpoint\ndata: /messages?session={}\n\n",
                        "default-session"
                    );
                    let response = Response::from_string(endpoint_event)
                        .with_header(
                            Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..])
                                .unwrap(),
                        )
                        .with_header(
                            Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..]).unwrap(),
                        )
                        .with_header(
                            Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..])
                                .unwrap(),
                        );
                    let _ = request.respond(response);
                }
                (Method::Post, path) if path.starts_with("/messages") => {
                    let mut body = String::new();
                    let _ = std::io::Read::read_to_string(request.as_reader(), &mut body);

                    let workspace_thread = workspace.clone();
                    let body_thread = body.clone();
                    let server_handler_thread = server_handler;

                    rayon::spawn(move || {
                        let (tx, rx) = flume::bounded(1);
                        let b_thread = body_thread.clone();
                        let w_thread = workspace_thread.clone();

                        rayon::spawn(move || {
                            let response_json =
                                server_handler_thread.handle_request(&b_thread, &w_thread);
                            let _ = tx.send(response_json);
                        });

                        let response_json = rx.recv().unwrap_or_else(|_| {
                            json!({
                                "jsonrpc": "2.0",
                                "error": { "code": -32000, "message": "Mission Interrupted: Task cancelled or failed." }
                            }).to_string()
                        });

                        let response = Response::from_string(response_json)
                            .with_header(
                                Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                                    .unwrap(),
                            )
                            .with_header(
                                Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..])
                                    .unwrap(),
                            );
                        let _ = request.respond(response);
                    });
                }
                _ => {
                    let _ =
                        request.respond(Response::from_string("Not Found").with_status_code(404));
                }
            }
        }
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
