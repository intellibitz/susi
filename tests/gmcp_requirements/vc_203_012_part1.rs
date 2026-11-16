#[test]
fn gmcp_req_203_012_01_contract() {
    use sha2::{Digest, Sha256};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-012.md"))
        .expect("VC-203-012 contract document must exist");
    for section in [
        "Normative clause",
        "Pinned sources",
        "Canonical data model",
        "Behavior table",
        "Compatibility and deprecation decisions",
        "Threat model",
        "SLOs and resource budgets",
        "Fixtures",
        "Evidence locations",
        "Ambiguities resolved",
        "Operations and non-claims",
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a ## {section} section"
        );
    }

    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .expect("requirements array")
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-012")
        .collect();
    assert_eq!(
        rows.len(),
        57,
        "the contract must bind all 57 VC-203-012 rows"
    );

    let mut modern = 0usize;
    let mut optional = 0usize;
    let mut adapter = 0usize;
    let mut docs = std::collections::BTreeSet::new();
    for row in &rows {
        let id = row["id"].as_str().expect("row id");
        let level = row["level"].as_str().expect("row level");
        let path = row["trace"]["production_path"]
            .as_str()
            .expect("production_path");
        let doc = row["source"]["document"].as_str().expect("source document");
        docs.insert(doc);
        assert!(
            contract.contains(&format!("| {id} | {level} |")),
            "behavior table must bind {id} with level {level}"
        );
        assert!(
            contract.contains(path),
            "contract must declare production path {path} for {id}"
        );
        assert!(
            root.join(path).exists(),
            "declared production path must exist: {path}"
        );
        match row["disposition"]["status"]
            .as_str()
            .expect("disposition status")
        {
            "modern" => modern += 1,
            "optional" => optional += 1,
            "adapter" => adapter += 1,
            other => panic!("unexpected disposition {other} for {id}"),
        }
    }
    assert_eq!((modern, optional, adapter), (26, 15, 16));
    assert!(contract.contains("modern=26"));
    assert!(contract.contains("optional=15"));
    assert!(contract.contains("adapter=16"));

    let sources: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json"))
            .expect("sources.json must exist"),
    )
    .expect("sources.json must parse");
    let mcp_commit = sources["upstream"]["modelcontextprotocol"]["commit"]
        .as_str()
        .expect("modelcontextprotocol commit");
    assert!(
        contract.contains(mcp_commit),
        "contract must pin modelcontextprotocol upstream commit {mcp_commit}"
    );
    let source_files = sources["files"].as_array().expect("sources.files array");
    for doc in &docs {
        let entry = source_files
            .iter()
            .find(|f| f["document"].as_str() == Some(*doc))
            .unwrap_or_else(|| panic!("missing sources.json entry for {doc}"));
        let expected_sha = entry["sha256"].as_str().expect("sha256 in sources.json");
        let actual_bytes = std::fs::read(root.join(".agents/gmcp/spec").join(doc))
            .unwrap_or_else(|e| panic!("spec doc {doc} readable: {e}"));
        let actual_sha = susi_paths::hex::encode(Sha256::digest(&actual_bytes));
        assert_eq!(actual_sha, expected_sha, "spec hash drift on {doc}");
        assert!(
            contract.contains(&format!("| {doc} | {expected_sha} |")),
            "pinned sources table must pin {doc} with sha256 {expected_sha}"
        );
    }

    for marker in [
        "subscriptions/listen",
        "notifications/subscriptions/acknowledged",
        "io.modelcontextprotocol/subscriptionId",
        "io.modelcontextprotocol/logLevel",
        "PROGRESS_GAP = 100ms",
        "-32602",
        "-32601",
        "SEP-2577",
        "T1:",
        "T2:",
        "T3:",
        "T4:",
        "T5:",
        "T6:",
        "F-POS-012-01",
        "F-POS-012-06",
        "F-NEG-012-01",
        "F-NEG-012-06",
        "VC-203-012.jsonl",
        "VC-203-012.json",
        "gmcp_req_203_012_01_contract",
    ] {
        assert!(
            contract.contains(marker),
            "VC-203-012 contract missing required invariant or fixture marker `{marker}`"
        );
    }
}

#[test]
fn gmcp_req_203_012_02_production() {
    use std::io::{Read, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use susi_gmcp::mcp::model::*;
    use susi_gmcp::mcp::ServiceExt as _;
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let scratch = std::env::temp_dir().join(format!(
        "susi-gmcp-203-012-02-{}-{unique}",
        std::process::id()
    ));
    let home = scratch.join("home");
    let workspace = scratch.join("workspace");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&workspace).expect("workspace dir");

    fn drain_stdio_notifications(
        session: &mut StdioSession,
        barrier_tag: &str,
        modern_meta: Option<&serde_json::Value>,
    ) -> Vec<serde_json::Value> {
        let barrier = format!("drain-barrier-{barrier_tag}-{}", session.next_id);
        session.next_id += 1;
        let (method, params) = match modern_meta {
            Some(meta) => ("server/discover", serde_json::json!({"_meta": meta})),
            None => ("ping", serde_json::json!({})),
        };
        session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": barrier, "method": method, "params": params}));
        let answer = session
            .response_for(
                &serde_json::json!(barrier),
                std::time::Duration::from_secs(30),
            )
            .expect("ping barrier must answer");
        assert!(answer["error"].is_null(), "barrier ping failed: {answer}");
        let mut drained = Vec::new();
        let mut keep = Vec::new();
        for msg in session.pending.drain(..) {
            if msg.get("id").is_none_or(|id| id.is_null()) && msg.get("method").is_some() {
                drained.push(msg);
            } else {
                keep.push(msg);
            }
        }
        session.pending = keep;
        drained
    }

    fn take_stdio_until<F>(session: &mut StdioSession, what: &str, pred: F) -> serde_json::Value
    where
        F: Fn(&serde_json::Value) -> bool,
    {
        if let Some(pos) = session.pending.iter().position(&pred) {
            return session.pending.remove(pos);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            assert!(!remaining.is_zero(), "timed out waiting for {what}");
            let value = session
                .rx
                .recv_timeout(remaining)
                .unwrap_or_else(|e| panic!("channel error waiting for {what}: {e}"));
            if pred(&value) {
                return value;
            }
            session.pending.push(value);
        }
    }

    let modern_meta = |extra: serde_json::Value| -> serde_json::Value {
        let mut meta = serde_json::json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": { "extensions": {"io.modelcontextprotocol/tasks": {}}}});
        if let (Some(base), Some(add)) = (meta.as_object_mut(), extra.as_object()) {
            for (k, v) in add {
                base.insert(k.clone(), v.clone());
            }
        }
        meta
    };

    // ── 1. Modern Stdio (`mcp serve`, 2026-07-28) wire checks ────────────
    let mut modern = StdioSession::spawn_modern(root, &home);

    // F-NEG-012-06: Legacy-only methods must return -32601 on 2026-07-28.
    for (req_id, method, params) in [
        (
            "neg-set-level",
            "logging/setLevel",
            serde_json::json!({ "level": "info", "_meta": modern_meta(serde_json::json!({}))}),
        ),
        (
            "neg-res-sub",
            "resources/subscribe",
            serde_json::json!({ "uri": "susi://tools", "_meta": modern_meta(serde_json::json!({}))}),
        ),
        (
            "neg-res-unsub",
            "resources/unsubscribe",
            serde_json::json!({ "uri": "susi://tools", "_meta": modern_meta(serde_json::json!({}))}),
        ),
    ] {
        modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": req_id, "method": method, "params": params}));
        let resp = modern
            .response_for(
                &serde_json::json!(req_id),
                std::time::Duration::from_secs(30),
            )
            .expect("legacy method error response");
        assert_eq!(
            resp["error"]["code"], -32601,
            "{method} on 2026-07-28 must reject with -32601: {resp}"
        );
    }

    // F-NEG-012-05: Malformed `_meta["io.modelcontextprotocol/logLevel"]` -> -32602.
    for (idx, bad_level) in [
        serde_json::json!("verbose"),
        serde_json::json!(42),
        serde_json::json!({}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("bad-log-{idx}");
        modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": modern_meta(serde_json::json!({ "io.modelcontextprotocol/logLevel": bad_level}))}}));
        let resp = modern
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(30))
            .expect("bad logLevel must reply");
        assert_eq!(
            resp["error"]["code"], -32602,
            "malformed logLevel must reject with -32602: {resp}"
        );
    }

    // F-POS-012-05: Without logLevel, tools/call emits 0 `notifications/message`;
    // with `logLevel: "info"`, emits `notifications/message`; with `notice`, suppresses `info`.
    let warmup_meta = modern_meta(serde_json::json!({}));
    let _ = drain_stdio_notifications(&mut modern, "warmup", Some(&warmup_meta));
    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "call-no-log", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": modern_meta(serde_json::json!({}))}}));
    let resp = modern
        .response_for(
            &serde_json::json!("call-no-log"),
            std::time::Duration::from_secs(30),
        )
        .expect("call without logLevel must answer");
    assert!(resp["error"].is_null(), "{resp}");
    let silent = drain_stdio_notifications(&mut modern, "no-log", Some(&warmup_meta));
    assert!(
        silent
            .iter()
            .all(|n| n["method"] != "notifications/message"),
        "omitted logLevel must emit zero notifications/message: {silent:?}"
    );

    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "call-info-log", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": modern_meta(serde_json::json!({ "io.modelcontextprotocol/logLevel": "info"}))}}));
    let resp = modern
        .response_for(
            &serde_json::json!("call-info-log"),
            std::time::Duration::from_secs(30),
        )
        .expect("call with info logLevel must answer");
    assert!(resp["error"].is_null(), "{resp}");
    let info_notes = drain_stdio_notifications(&mut modern, "info-log", Some(&warmup_meta));
    assert!(
        info_notes
            .iter()
            .any(|n| n["method"] == "notifications/message" && n["params"]["level"] == "info"),
        "info threshold must emit notifications/message(info): {info_notes:?}"
    );

    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "call-notice-log", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": modern_meta(serde_json::json!({ "io.modelcontextprotocol/logLevel": "notice"}))}}));
    let resp = modern
        .response_for(
            &serde_json::json!("call-notice-log"),
            std::time::Duration::from_secs(30),
        )
        .expect("call with notice logLevel must answer");
    assert!(resp["error"].is_null(), "{resp}");
    let notice_notes = drain_stdio_notifications(&mut modern, "notice-log", Some(&warmup_meta));
    assert!(
        !notice_notes
            .iter()
            .any(|n| n["method"] == "notifications/message" && n["params"]["level"] == "info"),
        "notice threshold must suppress info logs: {notice_notes:?}"
    );

    // F-NEG-012-03 & F-POS-012-03: Malformed progressToken -> -32602; valid token correlates + completes.
    for (idx, bad_tok) in [
        serde_json::json!(""),
        serde_json::json!(true),
        serde_json::json!({"nested": 1}),
    ]
    .into_iter()
    .enumerate()
    {
        let id = format!("bad-prog-{idx}");
        modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": modern_meta(serde_json::json!({ "progressToken": bad_tok}))}}));
        let resp = modern
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(30))
            .expect("bad progressToken must answer");
        assert_eq!(
            resp["error"]["code"], -32602,
            "malformed progressToken must reject with -32602: {resp}"
        );
    }

    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "call-prog-ok", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": modern_meta(serde_json::json!({ "progressToken": "live-prog-1"}))}}));
    let resp = modern
        .response_for(
            &serde_json::json!("call-prog-ok"),
            std::time::Duration::from_secs(30),
        )
        .expect("call with valid progressToken must answer");
    assert!(resp["error"].is_null(), "{resp}");
    let done_prog = take_stdio_until(&mut modern, "prog-ok complete", |n| {
        n["method"] == "notifications/progress"
            && n["params"]["progressToken"] == "live-prog-1"
            && n["params"]["progress"].as_f64() == Some(1.0)
    });
    modern.pending.push(done_prog);
    let prog_notes = drain_stdio_notifications(&mut modern, "prog-ok", Some(&warmup_meta));
    let prog_vals: Vec<f64> = prog_notes
        .iter()
        .filter(|n| {
            n["method"] == "notifications/progress" && n["params"]["progressToken"] == "live-prog-1"
        })
        .map(|n| n["params"]["progress"].as_f64().expect("finite progress"))
        .collect();
    assert!(
        !prog_vals.is_empty(),
        "valid progressToken must emit notifications/progress: {prog_notes:?}"
    );
    for pair in prog_vals.windows(2) {
        assert!(
            pair[1] > pair[0],
            "progress must strictly increase: {prog_vals:?}"
        );
    }

    // F-NEG-012-01 (>256 URIs) & F-POS-012-01 (dedup + auth omission before ack)
    // & F-NEG-012-05 (subscriptions/listen must NOT emit notifications/message).
    let too_many_uris: Vec<String> = (0..257).map(|i| format!("susi://extra-{i}")).collect();
    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "sub-too-many", "method": "subscriptions/listen", "params": { "notifications": { "resourceSubscriptions": too_many_uris}, "_meta": modern_meta(serde_json::json!({}))}}));
    let too_many_resp = modern
        .response_for(
            &serde_json::json!("sub-too-many"),
            std::time::Duration::from_secs(30),
        )
        .expect("over-cap subscriptions/listen must reject");
    assert_eq!(
        too_many_resp["error"]["code"], -32602,
        ">256 URIs must reject with -32602: {too_many_resp}"
    );
    let post_reject = drain_stdio_notifications(&mut modern, "sub-too-many", Some(&warmup_meta));
    assert!(
        !post_reject.iter().any(|n| {
            n["method"] == "notifications/subscriptions/acknowledged"
                && n["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "sub-too-many"
        }),
        "rejected subscription must never emit acknowledged before error: {post_reject:?}"
    );

    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "sub-filtered", "method": "subscriptions/listen", "params": { "notifications": { "toolsListChanged": true, "resourceSubscriptions": [ "susi://tools", "file:///srv/data/../../etc/shadow", "rel/path", "susi://tools"]}, "_meta": modern_meta(serde_json::json!({ "io.modelcontextprotocol/logLevel": "debug"}))}}));
    let ack = take_stdio_until(&mut modern, "sub-filtered ack", |msg| {
        msg["method"] == "notifications/subscriptions/acknowledged"
            && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "sub-filtered"
    });
    assert_eq!(
        ack["params"]["notifications"]["resourceSubscriptions"],
        serde_json::json!(["susi://tools"]),
        "unauthorized/malformed URIs must be omitted and duplicates deduplicated: {ack}"
    );
    assert_eq!(ack["params"]["notifications"]["toolsListChanged"], true);

    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "sub-filtered", "reason": "done"}}));
    let sub_drained = drain_stdio_notifications(&mut modern, "sub-no-logs", Some(&warmup_meta));
    assert!(
        !sub_drained
            .iter()
            .any(|n| n["method"] == "notifications/message"),
        "subscriptions/listen must never emit notifications/message: {sub_drained:?}"
    );

    // F-NEG-012-02: 16 concurrent active subscriptions per session cap; 17th rejects before ack;
    // cancelling one frees a slot.
    for slot in 0..16 {
        let id = format!("cap-sub-{slot}");
        modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": "subscriptions/listen", "params": { "notifications": {"toolsListChanged": true}, "_meta": modern_meta(serde_json::json!({}))}}));
        let _ = take_stdio_until(&mut modern, &id, |msg| {
            msg["method"] == "notifications/subscriptions/acknowledged"
                && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == id
        });
    }
    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "cap-sub-16", "method": "subscriptions/listen", "params": { "notifications": {"toolsListChanged": true}, "_meta": modern_meta(serde_json::json!({}))}}));
    let seventeenth = modern
        .response_for(
            &serde_json::json!("cap-sub-16"),
            std::time::Duration::from_secs(30),
        )
        .expect("17th subscription must be rejected");
    assert_eq!(
        seventeenth["error"]["code"], -32602,
        "17th concurrent subscription must fail with -32602: {seventeenth}"
    );
    let no_17th_ack = drain_stdio_notifications(&mut modern, "no-17th-ack", Some(&warmup_meta));
    assert!(
        !no_17th_ack.iter().any(|n| {
            n["method"] == "notifications/subscriptions/acknowledged"
                && n["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "cap-sub-16"
        }),
        "17th subscription must not emit ack before error: {no_17th_ack:?}"
    );
    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "cap-sub-0"}}));
    let _ = drain_stdio_notifications(&mut modern, "slot-0-freed", Some(&warmup_meta));
    std::thread::sleep(std::time::Duration::from_millis(50));

    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "cap-sub-reacquired", "method": "subscriptions/listen", "params": { "notifications": {"toolsListChanged": true}, "_meta": modern_meta(serde_json::json!({}))}}));
    let _ = take_stdio_until(&mut modern, "reacquired ack", |msg| {
        msg["method"] == "notifications/subscriptions/acknowledged"
            && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                == "cap-sub-reacquired"
    });
    for slot in 1..16 {
        modern.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": format!("cap-sub-{slot}")}}));
    }
    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "cap-sub-reacquired"}}));
    drop(modern);

    // ── 2. Legacy 2025-11-25 Stdio adapter parity (`F-POS-012-06`) ────────
    let mut legacy = StdioSession::spawn(root, &home);
    let set_notice = legacy.request("logging/setLevel", serde_json::json!({"level": "notice"}));
    assert_eq!(set_notice, serde_json::json!({}));
    let _ = drain_stdio_notifications(&mut legacy, "legacy-warmup", None);
    let _ = legacy.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    let legacy_notice = drain_stdio_notifications(&mut legacy, "legacy-notice", None);
    assert!(
        !legacy_notice
            .iter()
            .any(|n| n["method"] == "notifications/message" && n["params"]["level"] == "info"),
        "legacy notice level must suppress info logs: {legacy_notice:?}"
    );
    let set_info = legacy.request("logging/setLevel", serde_json::json!({"level": "info"}));
    assert_eq!(set_info, serde_json::json!({}));
    let _ = legacy.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    let legacy_info = drain_stdio_notifications(&mut legacy, "legacy-info", None);
    assert!(
        legacy_info
            .iter()
            .any(|n| n["method"] == "notifications/message" && n["params"]["level"] == "info"),
        "legacy info level must emit info logs: {legacy_info:?}"
    );
    let sub_ok = legacy.request(
        "resources/subscribe",
        serde_json::json!({"uri": "susi://tools"}),
    );
    assert_eq!(sub_ok, serde_json::json!({}));
    let unsub_ok = legacy.request(
        "resources/unsubscribe",
        serde_json::json!({"uri": "susi://tools"}),
    );
    assert_eq!(unsub_ok, serde_json::json!({}));
    drop(legacy);

    // ── 3. In-process GmcpService: dynamic auth revocation (T2), slow-lag
    //    teardown (F-NEG-012-02), task progress lifecycle & cancel (F-POS-012-04 /
    //    F-NEG-012-04), duplicate active token (F-NEG-012-03), RFC 5424 levels,
    //    per-request log cap, redaction & 16 KiB payload ceiling (T4/T5) ──
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let revocable_denied = Arc::new(AtomicBool::new(false));
    let cancel_observed = Arc::new(AtomicBool::new(false));
    let service = GmcpService::new(workspace);
    service.register_resource(
        Resource::new("dyn://secret", "dynamic secret"),
        Arc::new(|_, _| {
            Box::pin(async move {
                let result: ReadResourceResult = serde_json::from_value(serde_json::json!({ "contents": [{"uri": "dyn://secret", "mimeType": "text/plain", "text": "ok"}]}))
                .expect("valid resource result");
                Ok(result.into())
            })
        }),
    );
    service.register_resource(
        Resource::new("open://ok", "open resource"),
        Arc::new(|_, _| {
            Box::pin(async move {
                let result: ReadResourceResult = serde_json::from_value(serde_json::json!({ "contents": [{"uri": "open://ok", "mimeType": "text/plain", "text": "ok"}]}))
                .expect("valid resource result");
                Ok(result.into())
            })
        }),
    );
    let guard_flag = revocable_denied.clone();
    service.set_resource_guard(Arc::new(move |uri| {
        if uri == "sealed://denied" || (uri == "dyn://secret" && guard_flag.load(Ordering::SeqCst))
        {
            return Err(ErrorData::invalid_request("Resource access denied", None));
        }
        Ok(())
    }));

    // Slow task tool holding work open until task cancellation or release gate.
    let task_gate = Arc::new(tokio::sync::Notify::new());
    let task_gate_clone = task_gate.clone();
    let cancel_flag = cancel_observed.clone();
    let slow_task_tool: Tool = serde_json::from_value(serde_json::json!({ "name": "slow_task_012", "inputSchema": {"type": "object"}}))
    .expect("slow task tool schema");
    service
        .register_task_tool(
            slow_task_tool,
            Arc::new(move |_, task| {
                let gate = task_gate_clone.clone();
                let cancel_flag = cancel_flag.clone();
                Box::pin(async move {
                    task.set_status_message("phase-1");
                    tokio::select! {
                        () = gate.notified() => {
                            Ok(CallToolResult::success(vec![ContentBlock::text("task-finished")]))
                        }
                        () = task.cancelled() => {
                            cancel_flag.store(true, Ordering::SeqCst);
                            Ok(CallToolResult::error(vec![ContentBlock::text("cancelled")]))
                        }
                    }
                })
            }),
        )
        .expect("register slow task tool");

    // Redacting/RFC5424/cap tool returning sensitive oversized error OR emitting >64 logs.
    let log_storm_tool: Tool = serde_json::from_value(serde_json::json!({ "name": "log_storm_012", "inputSchema": {"type": "object"}}))
    .expect("log storm tool schema");
    service
        .register_tool(
            log_storm_tool,
            Arc::new(move |request, context| {
                Box::pin(async move {
                    let threshold = context.meta.log_level();
                    for lvl in [
                        LoggingLevel::Debug,
                        LoggingLevel::Info,
                        LoggingLevel::Notice,
                        LoggingLevel::Warning,
                        LoggingLevel::Error,
                        LoggingLevel::Critical,
                        LoggingLevel::Alert,
                        LoggingLevel::Emergency,
                    ] {
                        let _ = context
                            .emit_logging_message(
                                threshold,
                                lvl,
                                Some("storm-logger".to_string()),
                                serde_json::json!({"tag": format!("{lvl:?}")}),
                            )
                            .await;
                    }
                    let extra = request
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("burst"))
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    for i in 0..extra {
                        let _ = context
                            .emit_logging_message(
                                threshold,
                                LoggingLevel::Warning,
                                Some("burst".to_string()),
                                serde_json::json!({"seq": i}),
                            )
                            .await;
                    }
                    if request
                        .arguments
                        .as_ref()
                        .and_then(|a| a.get("secret_err"))
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                    {
                        let big = "Z".repeat(24 * 1024);
                        return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                            "Bearer sk-gmcp012secret999 password=topsecretvalue {big}"
                        ))])
                        .into());
                    }
                    Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
                })
            }),
        )
        .expect("register log_storm_012");

    // Sync slow progress tool emitting progress steps while checking cancellation.
    let sync_gate = Arc::new(tokio::sync::Notify::new());
    let sync_gate_clone = sync_gate.clone();
    let sync_cancel_observed = Arc::new(AtomicBool::new(false));
    let sync_cancel_flag = sync_cancel_observed.clone();
    let sync_slow_tool: Tool = serde_json::from_value(serde_json::json!({ "name": "sync_slow_012", "inputSchema": {"type": "object"}}))
    .expect("sync slow tool schema");
    service
        .register_tool(
            sync_slow_tool,
            Arc::new(move |_, context| {
                let gate = sync_gate_clone.clone();
                let sync_cancel_flag = sync_cancel_flag.clone();
                Box::pin(async move {
                    if let Some(token) = context.meta.get_progress_token() {
                        let _ = context
                            .peer
                            .notify_progress(ProgressNotificationParam::new(token, 0.25))
                            .await;
                    }
                    tokio::select! {
                        () = gate.notified() => {
                            if let Some(token) = context.meta.get_progress_token() {
                                let _ = context
                                    .peer
                                    .notify_progress(ProgressNotificationParam::new(token, 1.0).with_total(1.0))
                                    .await;
                            }
                            Ok(CallToolResult::success(vec![ContentBlock::text("sync-done")]).into())
                        }
                        () = context.ct.cancelled() => {
                            sync_cancel_flag.store(true, Ordering::SeqCst);
                            Ok(CallToolResult::error(vec![ContentBlock::text("sync-cancelled")]).into())
                        }
                    }
                })
            }),
        )
        .expect("register sync_slow_012");

    let (mut wire_client, wire_sess) = {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let service_for_task = service.clone();
        let (client_half, server_half) = tokio::io::duplex(ADAPTER_LIMIT);
        let (server_read, server_write) = tokio::io::split(server_half);
        let sess = runtime.spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(server_read, ADAPTER_LIMIT),
                server_write,
            );
            let running = service_for_task
                .serve(transport)
                .await
                .map_err(|e| format!("serve failed: {e:?}"))?;
            running.waiting().await.map_err(|e| e.to_string())
        });
        let (client_read, mut client_write) = tokio::io::split(client_half);
        let (line_tx, line_rx) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let mut lines = tokio::io::BufReader::new(client_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(value) = serde_json::from_str(&line) {
                    if line_tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        let (byte_tx, mut byte_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        runtime.spawn(async move {
            while let Some(bytes) = byte_rx.recv().await {
                if client_write.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = client_write.shutdown().await;
        });
        (
            AdapterClient {
                write: Box::new(move |bytes| {
                    let _ = byte_tx.send(bytes.to_vec());
                }),
                rx: line_rx,
                pending: Vec::new(),
            },
            sess,
        )
    };

    fn drain_adapter_notifications(
        client: &mut AdapterClient,
        tag: &str,
        meta: &serde_json::Value,
    ) -> Vec<serde_json::Value> {
        let barrier = format!("w-barrier-{tag}");
        let ping_resp = client.call(
            &barrier,
            "server/discover",
            serde_json::json!({"_meta": meta.clone()}),
        );
        assert!(ping_resp["error"].is_null(), "{ping_resp}");
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for msg in client.pending.drain(..) {
            if msg.get("id").is_none_or(|id| id.is_null()) && msg.get("method").is_some() {
                out.push(msg);
            } else {
                keep.push(msg);
            }
        }
        client.pending = keep;
        out
    }

    // Dynamic guard revocation (`T2`): admit at ack, deny on update.
    wire_client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "dyn-sub", "method": "subscriptions/listen", "params": { "notifications": { "resourceSubscriptions": ["sealed://denied", "dyn://secret", "open://ok", "open://ok"]}, "_meta": modern_meta(serde_json::json!({}))}}));
    let dyn_ack = wire_client.take("dyn ack", |m| {
        m["method"] == "notifications/subscriptions/acknowledged"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "dyn-sub"
    });
    assert_eq!(
        dyn_ack["params"]["notifications"]["resourceSubscriptions"],
        serde_json::json!(["dyn://secret", "open://ok"])
    );
    revocable_denied.store(true, Ordering::SeqCst);
    service.resource_updated("dyn://secret".to_string());
    service.resource_updated("open://ok".to_string());
    let updated = wire_client.take("open://ok update", |m| {
        m["method"] == "notifications/resources/updated" && m["params"]["uri"] == "open://ok"
    });
    assert_eq!(
        updated["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "dyn-sub"
    );
    let dyn_extra = drain_adapter_notifications(&mut wire_client, "dyn-check", &warmup_meta);
    assert!(
        !dyn_extra
            .iter()
            .any(|m| m["method"] == "notifications/resources/updated"
                && m["params"]["uri"] == "dyn://secret"),
        "revoked resource must NEVER deliver notification after guard denial: {dyn_extra:?}"
    );
    wire_client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "dyn-sub"}}));
    let _ = drain_adapter_notifications(&mut wire_client, "dyn-closed", &warmup_meta);

    // Slow-consumer lag fail-closed teardown (`F-NEG-012-02`).
    wire_client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "lag-sub", "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": ["open://ok"]}, "_meta": modern_meta(serde_json::json!({}))}}));
    let _ = wire_client.take("lag ack", |m| {
        m["method"] == "notifications/subscriptions/acknowledged"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "lag-sub"
    });
    for _ in 0..2048 {
        service.resource_updated("open://ok".to_string());
    }
    let lag_end = wire_client.answer("lag-sub");
    assert!(
        lag_end["error"].is_object() || lag_end["result"].is_object(),
        "lagged subscriber must terminate cleanly: {lag_end}"
    );
    assert_eq!(
        lag_end["error"]["code"], -32603,
        "lagged subscriber must terminate fail-closed with an explicit error: {lag_end}"
    );
    let _ = drain_adapter_notifications(&mut wire_client, "after-lag", &warmup_meta);

    // Task-augmented progress lifecycle, active duplicate rejection, and cancel halt
    // (`F-POS-012-04`, `F-NEG-012-03`, `F-NEG-012-04`).
    let task_start = wire_client.call(
        "task-prog-1",
        "tools/call",
        serde_json::json!({ "name": "slow_task_012", "arguments": {}, "task": {}, "_meta": modern_meta(serde_json::json!({"progressToken": "task-tok-1"}))}),
    );
    assert_eq!(task_start["result"]["resultType"], "task", "{task_start}");
    let task_id_1 = task_start["result"]["taskId"]
        .as_str()
        .expect("taskId string")
        .to_string();

    // Concurrent reuse of `task-tok-1` while task is Working MUST reject -32602.
    let dup = wire_client.call(
        "task-prog-dup",
        "server/discover",
        serde_json::json!({ "_meta": modern_meta(serde_json::json!({"progressToken": "task-tok-1"}))}),
    );
    assert_eq!(
        dup["error"]["code"], -32602,
        "duplicate active progressToken across CreateTaskResult lifetime must reject -32602: {dup}"
    );

    // Complete task-1 cleanly -> final progress arrives and token frees for reuse.
    task_gate.notify_one();
    let final_task_prog = wire_client.take("task-1 final progress", |m| {
        m["method"] == "notifications/progress"
            && m["params"]["progressToken"] == "task-tok-1"
            && m["params"]["progress"].as_f64().is_some_and(|p| p >= 1.0)
    });
    assert_eq!(final_task_prog["params"]["progressToken"], "task-tok-1");
    for poll in 0..40 {
        let status = wire_client.call(
            &format!("poll-1-{poll}"),
            "tasks/get",
            serde_json::json!({ "taskId": task_id_1, "_meta": modern_meta(serde_json::json!({}))}),
        );
        if status["result"]["status"] == "completed" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let reuse = wire_client.call(
        "task-prog-reuse",
        "server/discover",
        serde_json::json!({ "_meta": modern_meta(serde_json::json!({"progressToken": "task-tok-1"}))}),
    );
    assert!(
        reuse["error"].is_null(),
        "progressToken must be reusable after task reaches terminal state: {reuse}"
    );
    let _ = drain_adapter_notifications(&mut wire_client, "after-task-1", &warmup_meta);

    // Start task-2 with `task-tok-2`, cancel via `tasks/cancel`, verify NO `1.0` emitted.
    let task_start_2 = wire_client.call(
        "task-prog-2",
        "tools/call",
        serde_json::json!({ "name": "slow_task_012", "arguments": {}, "task": {}, "_meta": modern_meta(serde_json::json!({"progressToken": "task-tok-2"}))}),
    );
    let task_id_2 = task_start_2["result"]["taskId"]
        .as_str()
        .expect("taskId 2")
        .to_string();
    let cancel_resp = wire_client.call(
        "task-cancel-2",
        "tasks/cancel",
        serde_json::json!({ "taskId": task_id_2, "_meta": modern_meta(serde_json::json!({}))}),
    );
    assert!(cancel_resp["error"].is_null(), "{cancel_resp}");
    let deadline_cancel = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !cancel_observed.load(Ordering::SeqCst) && std::time::Instant::now() < deadline_cancel {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let after_task_cancel =
        drain_adapter_notifications(&mut wire_client, "after-task-cancel", &warmup_meta);
    assert!(
        !after_task_cancel.iter().any(|m| {
            m["method"] == "notifications/progress"
                && m["params"]["progressToken"] == "task-tok-2"
                && m["params"]["progress"].as_f64().is_some_and(|p| p >= 1.0)
        }),
        "cancelled task must never emit completion 1.0 progress: {after_task_cancel:?}"
    );

    // Sync cancellation halts progress (`F-NEG-012-04`) and releases token.
    wire_client.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "sync-cancel-req", "method": "tools/call", "params": { "name": "sync_slow_012", "arguments": {}, "_meta": modern_meta(serde_json::json!({"progressToken": "sync-cancel-tok"}))}}));
    let first_sync_prog = wire_client.take("sync initial progress", |m| {
        m["method"] == "notifications/progress" && m["params"]["progressToken"] == "sync-cancel-tok"
    });
    assert_eq!(first_sync_prog["params"]["progress"], 0.25);
    wire_client.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "sync-cancel-req"}}));
    let sync_deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while !sync_cancel_observed.load(Ordering::SeqCst) && std::time::Instant::now() < sync_deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        sync_cancel_observed.load(Ordering::SeqCst),
        "sync handler must observe request cancellation"
    );
    let after_sync_cancel =
        drain_adapter_notifications(&mut wire_client, "after-sync-cancel", &warmup_meta);
    assert!(
        !after_sync_cancel.iter().any(|m| {
            m["method"] == "notifications/progress"
                && m["params"]["progressToken"] == "sync-cancel-tok"
                && m["params"]["progress"].as_f64().is_some_and(|p| p >= 1.0)
        }),
        "cancelled sync request must never emit completion 1.0 progress: {after_sync_cancel:?}"
    );

    // RFC 5424 severity ordering (`Notice`), per-request `64` cap, redaction & `<= 16 KiB` ceiling.
    let storm = wire_client.call(
        "storm-warning",
        "tools/call",
        serde_json::json!({ "name": "log_storm_012", "arguments": {"burst": 100, "secret_err": true}, "_meta": modern_meta(serde_json::json!({ "io.modelcontextprotocol/logLevel": "notice"}))}),
    );
    assert!(storm["error"].is_null(), "{storm}");
    let storm_notes = drain_adapter_notifications(&mut wire_client, "storm-warning", &warmup_meta);
    let storm_logs: Vec<&serde_json::Value> = storm_notes
        .iter()
        .filter(|m| m["method"] == "notifications/message")
        .collect();
    assert!(
        !storm_logs
            .iter()
            .any(|l| matches!(l["params"]["level"].as_str(), Some("debug" | "info"))),
        "notice threshold must suppress debug & info: {storm_logs:?}"
    );
    for expected in [
        "notice",
        "warning",
        "error",
        "critical",
        "alert",
        "emergency",
    ] {
        assert!(
            storm_logs.iter().any(|l| l["params"]["level"] == expected),
            "notice threshold must allow {expected}: {storm_logs:?}"
        );
    }
    assert!(
        storm_logs.len() <= 64,
        "per-request log emission must be capped at 64, got {}",
        storm_logs.len()
    );
    for log_msg in &storm_logs {
        let serialized = serde_json::to_vec(&log_msg["params"]).expect("log params serialize");
        assert!(
            serialized.len() <= 16 * 1024,
            "notification payload exceeded 16 KiB cap: {}",
            serialized.len()
        );
        let text = String::from_utf8_lossy(&serialized);
        assert!(
            !text.contains("sk-gmcp012secret999") && !text.contains("topsecretvalue"),
            "log notification leaked unredacted credential: {text}"
        );
    }

    drop(wire_client);
    wire_sess.abort();

    // ── 4. Live HTTP Streamable SSE routing parity (`POST /mcp`) ──────────
    let (_lock, _env, _http_home) = gmcp_http_home_guard("012-02");
    susi_daemon::composition::wire_mcp_serve_substrate(&_http_home.join(".susi"));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let port = listener.local_addr().expect("bound port").port();
    std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root.to_path_buf(),
            vec![listener],
            None,
            false,
        );
    });

    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to HTTP server");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .expect("set read timeout");
    let body = serde_json::json!({ "jsonrpc": "2.0", "id": "http-012-1", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/logLevel": "info", "progressToken": "http-tok-1"}}})
    .to_string();
    let http_req = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: tools/call\r\nMcp-Name: ledger_verify\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(http_req.as_bytes())
        .expect("write HTTP request");
    let mut http_resp = String::new();
    stream
        .read_to_string(&mut http_resp)
        .expect("read HTTP SSE response");
    assert!(
        http_resp.starts_with("HTTP/1.1 200 OK"),
        "HTTP POST /mcp must succeed: {http_resp}"
    );
    assert!(
        http_resp.to_ascii_lowercase().contains("text/event-stream"),
        "HTTP POST /mcp with streaming notifications must respond with text/event-stream: {http_resp}"
    );
    assert!(
        http_resp.contains("\"notifications/progress\"") && http_resp.contains("\"http-tok-1\""),
        "HTTP SSE stream must carry correlated notifications/progress: {http_resp}"
    );
    assert!(
        http_resp.contains("\"notifications/message\"") && http_resp.contains("\"info\""),
        "HTTP SSE stream must carry request-scoped notifications/message: {http_resp}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
