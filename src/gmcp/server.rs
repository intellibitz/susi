// GMCP Server Substrate: Model Context Protocol SDK Interface
// 100% Rust implementation serving Tier 1 Swarm via rmcp SDK

use std::path::{Path, PathBuf};
use rmcp::{Server, Tool};
use serde_json::json;

use crate::gmcp::tools::ToolRegistry;

pub struct GmcpServer;

impl GmcpServer {
    pub fn run_stdio(workspace: &Path, _version: &str) {
        eprintln!("[GMCP Server] Started (rmcp stdio substrate).");

        let mut server = Server::new("susi-substrate", crate::SUSI_VERSION);

        // Register all tools from the registry into the rmcp server
        for tool_info in ToolRegistry::list_tools() {
            let tool_name = tool_info.name.clone();
            let workspace = workspace.to_path_buf();

            server.register_tool(Tool::new(
                &tool_name,
                &tool_info.description,
                move |args| {
                    let res = ToolRegistry::execute_tool(&tool_name, &args, &workspace);
                    Ok(json!({
                        "content": [
                            { "type": "text", "text": res }
                        ]
                    }))
                }
            ));
        }

        if let Err(e) = server.run_stdio() {
            eprintln!("[GMCP Error] Stdio substrate failure: {}", e);
        }
    }

    pub fn start_http_server(workspace: PathBuf, server_http: tiny_http::Server) {
        let addr = server_http.server_addr().to_string();
        eprintln!("[GMCP HTTP/SSE] rmcp-bridged substrate active on {}", addr);

        for mut request in server_http.incoming_requests() {
            let workspace = workspace.clone();
            let mut server = Server::new("susi-substrate", crate::SUSI_VERSION);

            // Re-register tools for HTTP session (stateless bridge)
            for tool_info in ToolRegistry::list_tools() {
                let tool_name = tool_info.name.clone();
                let ws = workspace.clone();
                server.register_tool(Tool::new(&tool_name, &tool_info.description, move |args| {
                    Ok(json!({"content": [{"type": "text", "text": ToolRegistry::execute_tool(&tool_name, &args, &ws)}]}))
                }));
            }

            let mut body = String::new();
            let _ = std::io::Read::read_to_string(request.as_reader(), &mut body);

            let response_json = server.handle_message(&body).unwrap_or_else(|e| {
                json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32000, "message": format!("Internal Protocol Error: {}", e) }
                }).to_string()
            });

            let response = tiny_http::Response::from_string(response_json)
                .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap())
                .with_header(tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap());
            let _ = request.respond(response);
        }
    }
}
