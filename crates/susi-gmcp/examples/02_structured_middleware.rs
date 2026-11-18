#![forbid(unsafe_code)]
//! Runnable example `02_structured_middleware`: structured schemas + composable middleware (`SDK-018`).

use serde_json::{json, Value};
use std::path::Path;
use susi_gmcp::mcp::model::ClientCapabilities;
use susi_gmcp::sdk::{examples::production_reference_server, TestHarness};

pub fn run(scratch: &Path) -> Result<Value, String> {
    let ws = scratch.join("ex02-ws");
    let store = scratch.join("ex02-tasks");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let (svc, audit_log) =
        production_reference_server(ws, store).map_err(|e| e.message.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let h = TestHarness::spawn(svc);
        let caps = ClientCapabilities::default();
        let res = h
            .request_modern(
                "tools/call",
                json!({
                    "name": "summarize_items",
                    "arguments": {
                        "items": ["alpha", "beta", "gamma"],
                        "scope": { "namespace": "sdk", "tag": "v1" },
                        "maxItems": 2
                    }
                }),
                caps.clone(),
            )
            .await?;
        let trace_after_ok = audit_log.read().clone();
        audit_log.write().clear();
        let invalid = h
            .request_modern(
                "tools/call",
                json!({ "name": "summarize_items", "arguments": { "items": 42 } }),
                caps,
            )
            .await?;
        let skipped_on_invalid = audit_log.read().is_empty();
        h.shutdown().await;
        let expected_trace = vec![
            "mw1:before:summarize_items",
            "mw2:before:summarize_items",
            "handler:summarize_items",
            "mw2:after:summarize_items",
            "mw1:after:summarize_items",
        ];
        let structured = res["result"]["structuredContent"].clone();
        let ok = structured == json!({ "count": 2, "summary": "alpha, beta", "namespace": "sdk" })
            && trace_after_ok == expected_trace
            && invalid["result"]["isError"] == json!(true)
            && skipped_on_invalid;
        Ok(json!({
            "example": "02_structured_middleware",
            "ok": ok,
            "structuredContent": structured,
            "stages": trace_after_ok.len(),
            "skippedMiddlewareOnInvalidInput": skipped_on_invalid
        }))
    })
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("susi-gmcp-ex02-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let val = out?;
    if val["ok"] != json!(true) {
        return Err(format!("02_structured_middleware invariant failed: {val}"));
    }
    println!("{val}");
    Ok(())
}
