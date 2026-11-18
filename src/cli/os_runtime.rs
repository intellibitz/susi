//! `susi os runtime`: the CLI side of the runtime view. The view itself is
//! `susi_daemon::runtime_view` (the MCP `os_runtime` tool returns the same
//! document); this module reads the daemon's inference cache over the
//! daemon's MCP socket and renders the table.

/// Ask the daemon's GEMI plane which weights its inference cache holds (the
/// MCP `runtime_models_list` tool). This is the authoritative "loaded"
/// signal; the `/proc` scan above only sees mapped or open files.
pub(crate) fn daemon_loaded_models() -> Result<serde_json::Value, String> {
    let value = daemon_tool("runtime_models_list", &serde_json::json!({}), 3)?;
    if value.get("models").is_some_and(serde_json::Value::is_array) {
        Ok(value)
    } else {
        Err("response has no `models` list".to_string())
    }
}

/// One authenticated `tools/call` on the daemon's loopback MCP socket
/// (GMCP-HTTP), with `timeout_secs` as the receive window for the call.
/// Returns the tool's JSON document, else the tool's error text.
pub(crate) fn daemon_tool(
    tool: &str,
    arguments: &serde_json::Value,
    timeout_secs: u64,
) -> Result<serde_json::Value, String> {
    let port = susi_config::SusiConfig::load_global()
        .unwrap_or_default()
        .gmcp_http_port();
    let addr = format!("127.0.0.1:{port}");
    let bearer = susi_paths::host_token();
    // The std-only wire client never signs or seals. A peers.json row can
    // bind a pubkey to the loopback address through the host-only fallback
    // in `cluster_key::bound_pubkey_for_addr` (cluster_key.rs:694-717); the
    // sealing peer client keeps that case.
    let result = if susi_config::cluster_key::bound_pubkey_for_addr(&addr).is_some() {
        susi_core::mcp_client::call_tool_with_timeout(
            &addr,
            tool,
            arguments,
            bearer.as_deref(),
            timeout_secs,
        )
    } else {
        susi_wire_client::mcp::call_tool(&addr, tool, arguments, bearer.as_deref(), timeout_secs)
            .map_err(|e| e.to_string())
    }
    .map_err(|e| format!("daemon MCP socket {addr}: {e}"))?;
    tool_document(&result)
}

/// The JSON document a `tools/call` result carries in its first text
/// block, or that block as the error when the tool reported one.
fn tool_document(result: &serde_json::Value) -> Result<serde_json::Value, String> {
    let text = result
        .pointer("/content/0/text")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if result.get("isError").and_then(serde_json::Value::as_bool) == Some(true) {
        return Err(if text.is_empty() {
            "the daemon tool reported an error".to_string()
        } else {
            text.to_string()
        });
    }
    serde_json::from_str(text).map_err(|e| format!("tool result is not a JSON document: {e}"))
}

/// The runtime view, with the daemon's loaded models read over the daemon's
/// MCP socket (`runtime_models_list`).
pub(crate) fn view() -> serde_json::Value {
    susi_daemon::runtime_view::view(daemon_loaded_models)
}

pub(crate) fn print(json: bool) -> crate::anyhow::Result<()> {
    let body = view();
    if json {
        println!("{}", serde_json::to_string_pretty(&body)?);
        return Ok(());
    }
    let summary = &body["summary"];
    println!("SUSI OS — runtime");
    println!(
        "daemon:     {}",
        body["daemon_pid"]
            .as_u64()
            .map_or_else(|| "not running".to_string(), |pid| format!("pid {pid}"))
    );
    println!(
        "processes:  {} AI process(es) — {} SUSI-owned, {} external",
        summary["processes"], summary["susi_owned"], summary["external"]
    );
    for p in body["processes"].as_array().into_iter().flatten() {
        println!(
            "  {:>7}  {:<8} {:<13} {:>8}  {}{}",
            p["pid"],
            p["owner"].as_str().unwrap_or_default(),
            p["kind"].as_str().unwrap_or_default(),
            p["gpu_mib"]
                .as_u64()
                .map_or_else(String::new, |mib| format!("{mib} MiB")),
            p["command"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(70)
                .collect::<String>(),
            if p["stale_binary"] == true {
                "  [stale binary]"
            } else {
                ""
            },
        );
    }
    let models = &body["models"];
    println!(
        "models:     {} managed in {}",
        summary["managed_models"],
        models["managed_dir"].as_str().unwrap_or_default()
    );
    for m in models["managed"].as_array().into_iter().flatten() {
        let open_by = m["open_by"].as_array().map_or(0, Vec::len);
        println!(
            "  {}{}{}{}",
            m["name"].as_str().unwrap_or_default(),
            if m["selected"] == true {
                "  [selected]"
            } else {
                ""
            },
            if m["loaded_in_daemon"] == true {
                "  [loaded]"
            } else {
                ""
            },
            if open_by > 0 {
                format!("  [mapped/open by {}]", m["open_by"])
            } else {
                String::new()
            },
        );
    }
    if body["models"]["open_by_observed"] != true {
        println!(
            "  mapped/open residency: Linux-only (/proc maps and fds); not observed on {}",
            std::env::consts::OS
        );
    }
    for m in models["external_in_use"].as_array().into_iter().flatten() {
        println!(
            "  external: {}  [open by {}]",
            m["path"].as_str().unwrap_or_default(),
            m["open_by"]
        );
    }
    let loaded = &models["daemon_loaded"];
    match loaded["models"].as_array() {
        Some(list) => {
            println!(
                "daemon inference cache (pid {}): {} model(s) loaded",
                loaded["pid"],
                list.len()
            );
            for m in list {
                let gpu = match (
                    m["backend"]["gpu_layers"].as_u64(),
                    m["backend"]["total_layers"].as_u64(),
                ) {
                    (Some(on), Some(total)) => format!(", {on}/{total} layers on GPU"),
                    (Some(_) | None, _) => String::new(),
                };
                println!(
                    "  {}  [{} on {}{}, {} in flight]",
                    m["path"].as_str().unwrap_or_default(),
                    m["state"].as_str().unwrap_or_default(),
                    m["device"].as_str().unwrap_or("?"),
                    gpu,
                    m["in_flight_users"].as_u64().unwrap_or(0),
                );
            }
        }
        None => println!(
            "daemon inference cache: unavailable ({})",
            loaded["error"].as_str().unwrap_or("unknown error")
        ),
    }
    if summary["stale_binaries"].as_u64().unwrap_or(0) > 0 {
        println!();
        println!(
            "warning: {} process(es) run a binary that was replaced on disk — restart them to pick up the installed build",
            summary["stale_binaries"]
        );
    }
    println!();
    println!("note: {}", body["note"].as_str().unwrap_or_default());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_results_surface_the_document_on_success_and_the_text_on_error() {
        let ok = tool_document(&serde_json::json!({
            "content": [{"type": "text", "text": "{\"pid\":9,\"models\":[]}"}],
            "isError": false
        }))
        .unwrap();
        assert_eq!(ok["pid"], 9);
        assert_eq!(
            tool_document(&serde_json::json!({
                "content": [{"type": "text", "text": "model 'x' not found"}],
                "isError": true
            }))
            .unwrap_err(),
            "model 'x' not found"
        );
        assert_eq!(
            tool_document(&serde_json::json!({"content": [], "isError": true})).unwrap_err(),
            "the daemon tool reported an error"
        );
        assert!(tool_document(&serde_json::json!({
            "content": [{"type": "text", "text": "not json"}]
        }))
        .is_err());
    }

    #[test]
    fn tool_document_parses_nested_models_array_when_is_error_is_absent() {
        let doc = tool_document(&serde_json::json!({
            "content": [{"type": "text", "text": "{\"models\":[{\"path\":\"qwen.gguf\"}]}"}]
        }))
        .unwrap();
        assert_eq!(doc["models"][0]["path"], "qwen.gguf");
    }

    #[test]
    fn tool_document_rejects_empty_content_when_not_marked_error() {
        let err = tool_document(&serde_json::json!({"content": []})).unwrap_err();
        assert!(err.contains("tool result is not a JSON document"));
    }

    #[test]
    fn view_returns_structured_runtime_object_with_summary_and_models() {
        let v = view();
        assert!(v.is_object());
        assert!(v["summary"].is_object());
        assert!(v["models"].is_object());
    }
}
