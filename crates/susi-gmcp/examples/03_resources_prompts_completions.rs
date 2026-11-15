#![forbid(unsafe_code)]
//! Runnable example `03_resources_prompts_completions`: guarded resources, RFC 6570 templates, prompts, and completions (`SDK-018`).

use serde_json::{json, Value};
use std::path::Path;
use susi_gmcp::mcp::model::ClientCapabilities;
use susi_gmcp::sdk::{examples::production_reference_server, TestHarness};

pub fn run(scratch: &Path) -> Result<Value, String> {
    let ws = scratch.join("ex03-ws");
    let store = scratch.join("ex03-tasks");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let (svc, _) = production_reference_server(ws, store).map_err(|e| e.message.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let h = TestHarness::spawn(svc);
        let c = ClientCapabilities::default();
        let welcome = h
            .request_modern(
                "resources/read",
                json!({ "uri": "memo://welcome" }),
                c.clone(),
            )
            .await?;
        let item = h
            .request_modern(
                "resources/read",
                json!({ "uri": "memo://items/42" }),
                c.clone(),
            )
            .await?;
        let denied = h
            .request_modern(
                "resources/read",
                json!({ "uri": "memo://secret/key" }),
                c.clone(),
            )
            .await?;
        let prompt = h
            .request_modern(
                "prompts/get",
                json!({ "name": "draft_brief", "arguments": { "topic": "Interop" } }),
                c.clone(),
            )
            .await?;
        let comp = h
            .request_modern(
                "completion/complete",
                json!({
                    "ref": { "type": "ref/prompt", "name": "draft_brief" },
                    "argument": { "name": "topic", "value": "au" }
                }),
                c,
            )
            .await?;
        h.shutdown().await;
        let welcome_ok = welcome["result"]["contents"][0]["text"]
            .as_str()
            .is_some_and(|t| t.contains("Welcome"));
        let item_ok = item["result"]["contents"][0]["text"].as_str()
            == Some(r#"{"id":"42","status":"active"}"#);
        let guard_ok = denied["error"].is_object();
        let prompt_ok = prompt["result"]["messages"][0]["content"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("Interop"));
        let comp_ok = comp["result"]["completion"]["values"] == json!(["audit"]);
        let ok = welcome_ok && item_ok && guard_ok && prompt_ok && comp_ok;
        Ok(json!({
            "example": "03_resources_prompts_completions",
            "ok": ok,
            "welcomeOk": welcome_ok,
            "templateOk": item_ok,
            "guardDenied": guard_ok,
            "promptOk": prompt_ok,
            "completionOk": comp_ok
        }))
    })
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("susi-gmcp-ex03-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let val = out?;
    if val["ok"] != json!(true) {
        return Err(format!(
            "03_resources_prompts_completions invariant failed: {val}"
        ));
    }
    println!("{val}");
    Ok(())
}
