#![forbid(unsafe_code)]
//! Runnable example `06_production_plugin_server`: transactional plugin bundle attach, task cancel, and leak-free unload (`SDK-018`).

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, time::Duration};
use susi_gmcp::mcp::model::{ClientCapabilities, TaskStatus};
use susi_gmcp::sdk::{
    examples::{production_reference_server, sample_plugin_bundle},
    PluginPolicy, TestHarness,
};

pub fn run(scratch: &Path) -> Result<Value, String> {
    let ws = scratch.join("ex06-ws");
    let store = scratch.join("ex06-tasks");
    let pack_dir = scratch.join("ex06-plugin-pack");
    std::fs::create_dir_all(&ws).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(pack_dir.join("catalog")).map_err(|e| e.to_string())?;

    let art_bytes = br#"{"tools":["plugin_lookup","plugin_watch"]}"#;
    std::fs::write(pack_dir.join("catalog/plugin.json"), art_bytes).map_err(|e| e.to_string())?;
    let digest = susi_paths::hex::encode(Sha256::digest(art_bytes));
    let files = BTreeMap::from([("catalog".to_string(), "catalog/plugin.json".to_string())]);
    let csums = BTreeMap::from([("catalog".to_string(), format!("sha256:{digest}"))]);
    let bundle = sample_plugin_bundle(Some(pack_dir.clone()), csums, files);
    let manifest_bytes = serde_json::to_vec_pretty(&bundle.manifest).map_err(|e| e.to_string())?;
    std::fs::write(pack_dir.join("manifest.json"), manifest_bytes).map_err(|e| e.to_string())?;

    let (svc, _) = production_reference_server(ws, store).map_err(|e| e.message.to_string())?;
    let policy = PluginPolicy::strict().grant_permission("filesystem.read");
    let loaded = svc
        .attach_plugin(bundle, &policy)
        .map_err(|e| e.to_string())?;
    let active_snap = svc.ops_snapshot();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async move {
        let h = TestHarness::spawn(svc.clone());
        let caps = ClientCapabilities::builder().enable_tasks().build();
        let lookup = h
            .request_modern(
                "tools/call",
                json!({ "name": "plugin_lookup", "arguments": { "key": "prod" } }),
                caps.clone(),
            )
            .await?;
        let watch = h
            .request_modern(
                "tools/call",
                json!({ "name": "plugin_watch", "arguments": { "key": "bg" } }),
                caps,
            )
            .await?;
        let tid = watch["result"]["taskId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let unloaded = svc
            .unload_plugin("sdk-sample-plugin")
            .map_err(|e| e.to_string())?;
        for _ in 0..25 {
            if svc.tasks.running_task_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let cancelled = svc
            .tasks
            .get_task(&tid)
            .is_ok_and(|s| s.task.status == TaskStatus::Cancelled);
        h.shutdown().await;
        let ok = loaded.id == "sdk-sample-plugin"
            && active_snap["plugins"][0]["id"] == json!("sdk-sample-plugin")
            && lookup["result"]["content"][0]["text"] == json!("plugin:prod")
            && unloaded.cancelled_tasks == 1
            && svc.loaded_plugins().is_empty()
            && svc.ops_snapshot()["plugins"] == json!([])
            && svc.tasks.running_task_count() == 0
            && cancelled;
        Ok(json!({
            "example": "06_production_plugin_server",
            "ok": ok,
            "pluginId": loaded.id,
            "cancelledTasks": unloaded.cancelled_tasks,
            "activeSnapshot": active_snap
        }))
    })
}

fn main() -> Result<(), String> {
    let dir = std::env::temp_dir().join(format!("susi-gmcp-ex06-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let out = run(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    let val = out?;
    if val["ok"] != json!(true) {
        return Err(format!(
            "06_production_plugin_server invariant failed: {val}"
        ));
    }
    println!("{val}");
    Ok(())
}
