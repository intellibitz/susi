#![forbid(unsafe_code)]
//! Runnable example `01_minimal_tool`: minimal local MCP server via `ServerBuilder` (`SDK-018`).

use serde_json::{json, Value};
use std::path::Path;
use susi_gmcp::mcp::model::ClientCapabilities;
use susi_gmcp::sdk::{examples::minimal_echo_server, TestHarness};

pub fn run(scratch: &Path) -> Result<Value, String> {
    let ws = scratch.join("ex01-ws");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let svc = minimal_echo_server(ws).map_err(|e| e.message.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let h = TestHarness::spawn(svc);
        let caps = ClientCapabilities::default();
        let disc = h
            .request_modern("server/discover", json!({}), caps.clone())
            .await?;
        let listed = h
            .request_modern("tools/list", json!({}), caps.clone())
            .await?;
        let ok_call = h
            .request_modern(
                "tools/call",
                json!({ "name": "echo", "arguments": { "message": "hello susi sdk", "uppercase": true } }),
                caps.clone(),
            )
            .await?;
        let bad_call = h
            .request_modern(
                "tools/call",
                json!({ "name": "echo", "arguments": { "uppercase": true } }),
                caps,
            )
            .await?;
        h.shutdown().await;
        let echoed = ok_call["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let schema_rejected = bad_call["result"]["isError"] == json!(true);
        let tool_count = listed["result"]["tools"].as_array().map_or(0, Vec::len);
        let server_name = disc["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let ok = echoed == "HELLO SUSI SDK"
            && schema_rejected
            && tool_count == 1
            && server_name == "sdk-minimal-echo";
        Ok(json!({
            "example": "01_minimal_tool",
            "ok": ok,
            "server": server_name,
            "toolCount": tool_count,
            "echo": echoed,
            "schemaRejected": schema_rejected
        }))
    })
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("susi-gmcp-ex01-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let val = out?;
    if val["ok"] != json!(true) {
        return Err(format!("01_minimal_tool invariant failed: {val}"));
    }
    println!("{val}");
    Ok(())
}
