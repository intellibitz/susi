#![forbid(unsafe_code)]
//! Runnable example `04_mrtr_and_durable_tasks`: inline MRTR + durable task MRTR & restart persistence (`SDK-018`).

use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use susi_gmcp::mcp::model::{ClientCapabilities, ElicitationCapability, TaskStatus};
use susi_gmcp::protocol::GmcpService;
use susi_gmcp::sdk::{examples::production_reference_server, TestHarness};

pub fn run(scratch: &Path) -> Result<Value, String> {
    let ws = scratch.join("ex04-ws");
    let store = scratch.join("ex04-tasks");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let (svc, _) = production_reference_server(ws.clone(), store.clone())
        .map_err(|e| e.message.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let task_id = rt.block_on(async move {
        let h = TestHarness::spawn(svc);
        let mut caps = ClientCapabilities::builder().enable_tasks().build();
        caps.elicitation = Some(ElicitationCapability::default());
        let step1 = h
            .request_modern(
                "tools/call",
                json!({ "name": "confirm_action", "arguments": { "operation": "deploy" } }),
                caps.clone(),
            )
            .await?;
        let step2 = h
            .request_modern(
                "tools/call",
                json!({
                    "name": "confirm_action",
                    "arguments": { "operation": "deploy" },
                    "requestState": "op:deploy",
                    "inputResponses": { "confirm": { "approved": true } }
                }),
                caps.clone(),
            )
            .await?;
        if step1["result"]["resultType"] != json!("input_required")
            || step2["result"]["structuredContent"]["status"] != json!("confirmed")
        {
            h.shutdown().await;
            return Err("inline MRTR flow failed".into());
        }
        let spawned = h
            .request_modern(
                "tools/call",
                json!({ "name": "approval_workflow", "arguments": { "ticket": "REL-04" } }),
                caps.clone(),
            )
            .await?;
        let tid = spawned["result"]["taskId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for _ in 0..40 {
            let st = h
                .request_modern("tasks/get", json!({ "taskId": tid }), caps.clone())
                .await?;
            if st["result"]["status"] == json!("input_required") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = h
            .request_modern(
                "tasks/update",
                json!({ "taskId": tid, "inputResponses": { "approve": { "decision": "ship" } } }),
                caps.clone(),
            )
            .await?;
        for _ in 0..40 {
            let st = h
                .request_modern("tasks/get", json!({ "taskId": tid }), caps.clone())
                .await?;
            if st["result"]["status"] == json!("completed") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        h.shutdown().await;
        Ok::<String, String>(tid)
    })?;

    let reopened = GmcpService::open_durable(ws, store).map_err(|e| e.message.to_string())?;
    let snap = reopened
        .tasks
        .get_task(&task_id)
        .map_err(|e| e.message.to_string())?;
    let ok = snap.task.status == TaskStatus::Completed;
    Ok(json!({
        "example": "04_mrtr_and_durable_tasks",
        "ok": ok,
        "taskId": task_id,
        "persistedStatus": snap.task.status
    }))
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("susi-gmcp-ex04-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let val = out?;
    if val["ok"] != json!(true) {
        return Err(format!("04_mrtr_and_durable_tasks invariant failed: {val}"));
    }
    println!("{val}");
    Ok(())
}
