#![forbid(unsafe_code)]
//! Runnable example `05_hot_reload_and_subscriptions`: atomic catalog swap + dual-era notifications (`SDK-018`).

use serde_json::{json, Value};
use std::{collections::BTreeSet, path::Path, sync::Arc, time::Duration};
use susi_gmcp::mcp::model::{
    CallToolResult, ClientCapabilities, ContentBlock, ReadResourceResult, Resource,
    ResourceContents, Tool,
};
use susi_gmcp::sdk::{examples::production_reference_server, CatalogUpdate, TestHarness};

pub fn run(scratch: &Path) -> Result<Value, String> {
    let ws = scratch.join("ex05-ws");
    let store = scratch.join("ex05-tasks");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    let (svc, _) = production_reference_server(ws, store).map_err(|e| e.message.to_string())?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let mut legacy = TestHarness::spawn(svc.clone());
        let init = legacy
            .initialize_legacy("2025-11-25", ClientCapabilities::default())
            .await?;
        let sub = legacy
            .request_session("resources/subscribe", json!({ "uri": "memo://welcome" }))
            .await?;
        while legacy
            .next_notification(Duration::from_millis(15))
            .await
            .is_some()
        {}

        let schema = serde_json::Map::from_iter([("type".into(), json!("object"))]);
        let update = CatalogUpdate::new()
            .with_tool(
                Tool::new_with_raw("hot_v2", Some("Hot v2".into()), schema),
                Arc::new(|_p, _c| {
                    Box::pin(async {
                        Ok(CallToolResult::success(vec![ContentBlock::text("hot-v2-ok")]).into())
                    })
                }),
            )
            .with_resource(
                Resource::new("memo://welcome", "welcome").with_mime_type("text/plain"),
                Arc::new(|req, _c| {
                    Box::pin(async move {
                        Ok(ReadResourceResult {
                            result_type: None,
                            ttl_ms: Some(0),
                            cache_scope: None,
                            contents: vec![ResourceContents::TextResourceContents {
                                uri: req.uri,
                                mime_type: Some("text/plain".into()),
                                text: "Welcome v2".into(),
                                meta: None,
                            }],
                            meta: None,
                        }
                        .into())
                    })
                }),
            );
        let report = svc
            .reload_catalog(update)
            .map_err(|e| e.message.to_string())?;

        let mut seen = BTreeSet::new();
        while let Some(n) = legacy.next_notification(Duration::from_millis(80)).await {
            if let Some(m) = n["method"].as_str() {
                seen.insert(m.to_string());
            }
        }
        let modern = TestHarness::spawn(svc);
        let called = modern
            .request_modern(
                "tools/call",
                json!({ "name": "hot_v2", "arguments": {} }),
                ClientCapabilities::default(),
            )
            .await?;
        modern.shutdown().await;
        legacy.shutdown().await;

        let ok = init["result"]["protocolVersion"] == json!("2025-11-25")
            && sub["result"].is_object()
            && report.tools_changed
            && seen.contains("notifications/tools/list_changed")
            && seen.contains("notifications/resources/updated")
            && called["result"]["content"][0]["text"] == json!("hot-v2-ok");
        Ok(json!({
            "example": "05_hot_reload_and_subscriptions",
            "ok": ok,
            "notifications": seen
        }))
    })
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("susi-gmcp-ex05-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let val = out?;
    if val["ok"] != json!(true) {
        return Err(format!(
            "05_hot_reload_and_subscriptions invariant failed: {val}"
        ));
    }
    println!("{val}");
    Ok(())
}
