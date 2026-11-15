#[test]
fn gmcp_req_203_014_06_operations() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_core::plane_bus::{topics, with_watched, PlaneBus, PlaneHandler};
    use susi_gmcp::extensions::OFFICIAL_EXTENSIONS;
    use susi_gmcp::mcp::model::{
        CallToolResult, ContentBlock, TaskStatus as ServerTaskStatus, Tool, TASKS_EXTENSION_ID,
    };
    use susi_gmcp::mcp::task_manager::{
        TaskManager, TaskOptions, TaskPrincipal, DEFAULT_POLL_INTERVAL_MS, DEFAULT_TASK_TTL_MS,
        MAX_ACTIVE_TASKS_PER_PRINCIPAL, MAX_GLOBAL_ACTIVE_TASKS, MAX_INPUT_REQUESTS_PER_TASK,
        MAX_INPUT_RESPONSE_BYTES, MAX_RETAINED_TERMINAL_TASKS, MAX_SUBSCRIPTION_TASK_IDS,
        MAX_TASK_TTL_MS,
    };
    use susi_gmcp::mcp::ServiceExt;
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::server::GmcpServer;
    use susi_gmcp::stdio::BoundedLines;
    use susi_gmcp::susi_config::validate::{validate_dir, ConfigIssueKind};
    use susi_wire_client::mcp::{
        CallToolOutcome, PersistedTaskHandle, PersistedTaskHandleStore, Session,
        TaskStatus as ClientTaskStatus, WireError, PROTOCOL_VERSION_2026_07_28,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), &target).unwrap();
            }
        }
    }

    let (_lock, _env, home) = gmcp_http_home_guard("014-ops");
    struct RestoreOpsPlane;
    impl Drop for RestoreOpsPlane {
        fn drop(&mut self) {
            let bus = PlaneBus::global();
            bus.unregister(topics::TOOLS_LIST);
            bus.unregister(topics::TOOLS_EXISTS);
            bus.unregister(topics::TOOLS_EXECUTE);
            susi_tools::plane_handler::register();
        }
    }
    let _restore_ops_plane = RestoreOpsPlane;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "tok-014-ops-bearer-secret";
    std::fs::write(cfg_dir.join("api_token"), format!("{token}\n")).unwrap();

    let ops_runs = Arc::new(AtomicUsize::new(0));
    struct OpsList;
    impl PlaneHandler for OpsList {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let mut merged = if let Ok(Value::Array(tools)) =
                PlaneBus::global().request_local_prefix(topics::TOOLS_LIST, payload)
            {
                tools
            } else {
                Vec::new()
            };
            if !merged
                .iter()
                .any(|item| item.get("name").and_then(Value::as_str) == Some("ops_task_echo"))
            {
                merged.push(json!({"name": "ops_task_echo", "description": "Ops task echo"}));
            }
            Ok(Value::Array(merged))
        }
    }
    struct OpsExists;
    impl PlaneHandler for OpsExists {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            if payload.get("name").and_then(Value::as_str) == Some("ops_task_echo") {
                Ok(json!({ "exists": true }))
            } else {
                PlaneBus::global().request_local_prefix(topics::TOOLS_EXISTS, payload)
            }
        }
    }
    struct OpsExec {
        runs: Arc<AtomicUsize>,
    }
    impl PlaneHandler for OpsExec {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            if payload.get("name").and_then(Value::as_str) != Some("ops_task_echo") {
                return PlaneBus::global().request_local_prefix(topics::TOOLS_EXECUTE, payload);
            }
            let args = payload.get("args").cloned().unwrap_or(Value::Null);
            let runs = self.runs.clone();
            with_watched(&payload, || {
                runs.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(15));
                let msg = args.get("msg").and_then(Value::as_str).unwrap_or("ok");
                Ok(json!({ "text": format!("ops:{msg}") }))
            })
        }
    }
    susi_daemon::composition::wire_mcp_serve_substrate(&home.join(".susi"));
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(OpsList));
    PlaneBus::global().register(topics::TOOLS_EXISTS, Arc::new(OpsExists));
    PlaneBus::global().register(
        topics::TOOLS_EXECUTE,
        Arc::new(OpsExec {
            runs: ops_runs.clone(),
        }),
    );

    let tasks_meta = |req_id: &str| -> Value {
        json!({
            "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION_2026_07_28,
            "io.modelcontextprotocol/requestId": req_id,
            "io.modelcontextprotocol/clientCapabilities": {
                "extensions": { TASKS_EXTENSION_ID: {} }
            }
        })
    };

    // ── 1. Inspectable CLI, MCP Tool, & Cross-Transport API State Parity ─────
    let report = ledger_report(root);
    assert_eq!(report["verified"], true, "{report}");
    assert_eq!(report["revisions"]["ext-tasks@2026-07-28"], 46, "{report}");
    assert_eq!(ledger_tool_report(root), report);
    let cli_ledger = susi_json(root, &home, &["mcp", "ledger"]);
    assert_eq!(cli_ledger, report);

    let mcp_status = susi_command(root, &home, &["mcp", "status"])
        .output()
        .expect("susi mcp status must execute");
    assert!(
        mcp_status.status.success(),
        "susi mcp status failed: {}",
        String::from_utf8_lossy(&mcp_status.stderr)
    );
    let mcp_status_json: Value =
        serde_json::from_slice(&mcp_status.stdout).expect("susi mcp status must emit valid JSON");
    assert!(mcp_status_json.is_array(), "{mcp_status_json}");
    let cli_mcp_list = susi_json(root, &home, &["mcp", "list"]);
    assert_eq!(cli_mcp_list, mcp_status_json);

    let direct_svc = GmcpService::new(root.to_path_buf());
    let direct_discover = serde_json::to_value(direct_svc.discover_result()).unwrap();
    let direct_ext = direct_discover["capabilities"]["extensions"].clone();
    assert!(
        direct_ext[TASKS_EXTENSION_ID].is_object(),
        "shipped extensions must advertise io.modelcontextprotocol/tasks: {direct_discover}"
    );
    let status_rows = direct_svc.extensions().read().status_report();
    assert!(status_rows
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| {
            row["id"] == TASKS_EXTENSION_ID
                && row["status"] == "official"
                && row["version"] == 1
                && row["enabled"] == true
        })));
    let watched = direct_svc.extensions().read().watch(OFFICIAL_EXTENSIONS);
    assert_eq!(watched.len(), OFFICIAL_EXTENSIONS.len() - 1);
    assert!(watched
        .iter()
        .all(|item| item.starts_with("[GMCP] Adopt newly published official extension ")));

    let runtime = adapter_runtime(&home);
    let (mut embedded, emb_sess) = embedded_adapter(&runtime, root);
    let emb_disc = embedded.call("ops014-emb", "server/discover", adapter_modern_meta());
    assert_eq!(emb_disc["result"]["capabilities"]["extensions"], direct_ext);
    adapter_handshake(&mut embedded, "ops014-emb-init");
    drop(embedded);
    adapter_session_ended(&runtime, emb_sess, "014-ops-embedded");

    #[cfg(unix)]
    {
        let (mut socket, sock_sess) = socket_adapter(&runtime, root);
        let sock_disc = socket.call("ops014-sock", "server/discover", adapter_modern_meta());
        assert_eq!(
            sock_disc["result"]["capabilities"]["extensions"],
            direct_ext
        );
        adapter_handshake(&mut socket, "ops014-sock-init");
        drop(socket);
        adapter_session_ended(&runtime, sock_sess, "014-ops-socket");
    }

    let mut stdio = StdioSession::spawn_raw(root, &home);
    let stdio_disc = stdio
        .request_verbatim(
            &json!({
                "jsonrpc": "2.0",
                "id": "ops014-stdio",
                "method": "server/discover",
                "params": adapter_modern_meta()
            }),
            Duration::from_secs(120),
        )
        .expect("stdio server/discover must answer");
    assert_eq!(
        stdio_disc["result"]["capabilities"]["extensions"],
        direct_ext
    );
    drop(stdio);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback HTTP /mcp");
    let port = listener.local_addr().unwrap().port();
    let http_addr = format!("127.0.0.1:{port}");
    let http_root = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        GmcpServer::start_http_server(http_root, vec![listener], None, false);
    });
    for _ in 0..80 {
        if TcpStream::connect(&http_addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut wire_cli = Session::open_2026_07_28(&http_addr, Some(token), Duration::from_secs(15))
        .expect("2026-07-28 session must discover tasks capability");
    assert!(wire_cli.supports_server_tasks());
    assert_eq!(
        Value::Object(wire_cli.server_extensions().unwrap().clone()),
        direct_ext
    );

    let ops_handle = match wire_cli
        .call_tool_with_tasks(
            "ops_task_echo",
            &json!({"msg": "inspect"}),
            true,
            &mut |_| {},
        )
        .unwrap()
    {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected CreateTaskResult handle, got {other:?}"),
    };
    let handle_store_path = scratch.join("ops-client-handles.json");
    let mut hstore = PersistedTaskHandleStore::open(&handle_store_path).unwrap();
    hstore
        .save(PersistedTaskHandle::from_handle(
            &http_addr,
            PROTOCOL_VERSION_2026_07_28,
            &ops_handle,
        ))
        .unwrap();
    let settled = wire_cli
        .poll_task_until_done(&ops_handle, Some(Duration::from_secs(10)), |_| {
            Ok(json!({}))
        })
        .unwrap();
    assert_eq!(settled.handle.status, ClientTaskStatus::Completed);
    assert_eq!(
        settled.result.as_ref().unwrap()["content"][0]["text"],
        "ops:inspect"
    );
    assert!(hstore.update_from_snapshot(&settled).unwrap());
    let raw_client_store = std::fs::read_to_string(&handle_store_path).unwrap();
    assert!(
        !raw_client_store.contains(token),
        "PersistedTaskHandleStore must never write bearer secrets to disk"
    );
    let server_tasks_file = susi_paths::SusiDirs::data_dir().join("mcp-tasks/tasks.json");
    if server_tasks_file.exists() {
        let raw_server_store = std::fs::read_to_string(&server_tasks_file).unwrap();
        assert!(
            !raw_server_store.contains(token) && raw_server_store.contains("bearer:"),
            "durable server tasks.json must hash bearer principals and omit plaintext tokens"
        );
    }

    // ── 2. Configuration & Durable-Store Validation (Fail-Closed Diagnostics) ─
    let invalid_cli = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(
        !invalid_cli.status.success(),
        "`mcp ledger --refetched` without `--drift` must fail closed"
    );

    let cfg_valid_susi = scratch.join("cfg-valid/.susi");
    std::fs::create_dir_all(&cfg_valid_susi).unwrap();
    std::fs::write(
        cfg_valid_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": 4096}"#,
    )
    .unwrap();
    let valid_cfg = validate_dir(&cfg_valid_susi).expect("valid config validates");
    assert!(!valid_cfg.has_blocking(), "{:?}", valid_cfg.issues);

    let cfg_bad_susi = scratch.join("cfg-bad/.susi");
    std::fs::create_dir_all(&cfg_bad_susi).unwrap();
    std::fs::write(
        cfg_bad_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": "not-a-number"}"#,
    )
    .unwrap();
    let bad_cfg = validate_dir(&cfg_bad_susi).expect("invalid config validates");
    assert!(bad_cfg.has_blocking());
    assert!(bad_cfg
        .issues
        .iter()
        .any(|i| i.path == "max_rpc_body_bytes" && i.kind == ConfigIssueKind::TypeMismatch));

    let zero_home = scratch.join("cfg-zero");
    let zero_susi = zero_home.join(".susi");
    std::fs::create_dir_all(&zero_susi).unwrap();
    std::fs::write(
        zero_susi.join("config.json"),
        r#"{"max_rpc_body_bytes": 0}"#,
    )
    .unwrap();
    let mut zero_child = susi_command(root, &zero_home, &["mcp", "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("susi mcp serve must spawn");
    {
        let stdin = zero_child.stdin.as_mut().unwrap();
        writeln!(
            stdin,
            "{}",
            json!({
                "jsonrpc": "2.0",
                "id": "ops014-zero-ping",
                "method": "ping",
                "params": adapter_modern_meta()
            })
        )
        .unwrap();
    }
    drop(zero_child.stdin.take());
    let zero_out = zero_child.wait_with_output().unwrap();
    assert!(
        zero_out.stdout.is_empty(),
        "zero max_rpc_body_bytes must emit zero bytes on stdout: {}",
        String::from_utf8_lossy(&zero_out.stdout)
    );
    let zero_stderr = String::from_utf8_lossy(&zero_out.stderr);
    assert!(
        zero_stderr.contains("max_rpc_body_bytes") && zero_stderr.contains("must be > 0"),
        "expected stderr refusal diagnostic, got: {zero_stderr}"
    );

    // Durable server TaskManager::open schema & corruption validation.
    let bad_ver_dir = scratch.join("store-bad-ver");
    std::fs::create_dir_all(&bad_ver_dir).unwrap();
    std::fs::write(bad_ver_dir.join("tasks.json"), r#"[99, []]"#).unwrap();
    let ver_err = TaskManager::open(&bad_ver_dir)
        .err()
        .expect("future STORE_VERSION must fail closed");
    assert!(
        ver_err
            .message
            .contains("unsupported task store version 99"),
        "{ver_err:?}"
    );

    let bad_json_dir = scratch.join("store-bad-json");
    std::fs::create_dir_all(&bad_json_dir).unwrap();
    std::fs::write(bad_json_dir.join("tasks.json"), b"{truncated").unwrap();
    assert!(TaskManager::open(&bad_json_dir).is_err());

    let mismatch_dir = scratch.join("store-status-mismatch");
    std::fs::create_dir_all(&mismatch_dir).unwrap();
    std::fs::write(
        mismatch_dir.join("tasks.json"),
        json!([
            1,
            [[
                "local:stdio",
                {
                    "taskId": "11111111-2222-4333-8444-555555555555",
                    "status": "completed",
                    "createdAt": "2026-11-15T00:00:00Z",
                    "lastUpdatedAt": "2026-11-15T00:00:00Z",
                    "ttlMs": 300000,
                    "pollIntervalMs": 1000
                },
                serde_json::to_value(susi_gmcp::mcp::model::TaskPayload::Cancelled).unwrap(),
                [],
                1794700000000_u64,
                1794700001000_u64
            ]]
        ])
        .to_string(),
    )
    .unwrap();
    let mismatch_err = TaskManager::open(&mismatch_dir)
        .err()
        .expect("status/payload mismatch in tasks.json must fail closed");
    assert!(
        mismatch_err.message.contains("corrupt task store entry"),
        "{mismatch_err:?}"
    );

    // Client PersistedTaskHandleStore::open validation.
    let bad_client_ver = scratch.join("client-bad-ver.json");
    std::fs::write(&bad_client_ver, r#"{"version": 99, "handles": []}"#).unwrap();
    assert!(PersistedTaskHandleStore::open(&bad_client_ver).is_err());
    let bad_client_json = scratch.join("client-bad-json.json");
    std::fs::write(&bad_client_json, b"{corrupt").unwrap();
    assert!(PersistedTaskHandleStore::open(&bad_client_json).is_err());
    let dup_client_json = scratch.join("client-dup.json");
    let valid_h_json: Value = serde_json::from_str(&raw_client_store).unwrap();
    let one_h = valid_h_json["handles"][0].clone();
    std::fs::write(
        &dup_client_json,
        json!({"version": 1, "handles": [one_h.clone(), one_h]}).to_string(),
    )
    .unwrap();
    assert!(PersistedTaskHandleStore::open(&dup_client_json).is_err());

    // ── 3. Bounded Metrics, Logs, W3C `traceparent`, & Notification Isolation ─
    assert_eq!(DEFAULT_TASK_TTL_MS, 300_000);
    assert_eq!(DEFAULT_POLL_INTERVAL_MS, 1_000);
    assert_eq!(MAX_TASK_TTL_MS, 86_400_000);
    assert_eq!(MAX_ACTIVE_TASKS_PER_PRINCIPAL, 64);
    assert_eq!(MAX_GLOBAL_ACTIVE_TASKS, 512);
    assert_eq!(MAX_RETAINED_TERMINAL_TASKS, 2_048);
    assert_eq!(MAX_INPUT_REQUESTS_PER_TASK, 128);
    assert_eq!(MAX_SUBSCRIPTION_TASK_IDS, 64);
    assert_eq!(MAX_INPUT_RESPONSE_BYTES, 1_048_576);

    runtime.block_on(async {
        let quota_mgr = TaskManager::new();
        let p_ops = TaskPrincipal::from_bearer_token(token);
        assert!(!p_ops.as_str().contains(token));
        let clamped = quota_mgr
            .try_spawn_for(
                p_ops.clone(),
                TaskOptions {
                    ttl_ms: Some(MAX_TASK_TTL_MS + 99_999),
                    poll_interval_ms: Some(10_000),
                    status_message: Some("ops-bound".into()),
                },
                |ctx| {
                    Box::pin(async move {
                        ctx.cancelled().await;
                        Err(susi_gmcp::mcp::task_manager::TaskExit::Cancelled)
                    })
                },
            )
            .unwrap();
        assert_eq!(clamped.ttl_ms, Some(MAX_TASK_TTL_MS));
        assert_eq!(quota_mgr.running_task_count(), 1);

        // Exhaust poll burst budget (`POLL_BURST_ALLOWANCE = 24.0` at `poll_interval_ms = 10_000`) -> `-32000`.
        let mut throttled = false;
        for _ in 0..32 {
            if let Err(err) = quota_mgr.get_task_for(&clamped.task_id, &p_ops) {
                assert_eq!(err.code.0, -32000, "{err:?}");
                throttled = true;
                break;
            }
        }
        assert!(
            throttled,
            "excessive polling faster than pollIntervalMs must throttle with -32000"
        );
        quota_mgr.cancel_task_for(&clamped.task_id, &p_ops).unwrap();
    });

    let http_post = |extra_headers: &[(&str, &str)],
                     body_val: &Value|
     -> (String, String, Vec<Value>) {
        let body = body_val.to_string();
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {http_addr}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n"
        );
        for (k, v) in extra_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        let mut stream = TcpStream::connect(&http_addr).expect("connect HTTP /mcp");
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        let _ = stream.read_to_string(&mut raw);
        let (head, resp_body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let status_line = head.lines().next().unwrap_or_default().to_string();
        let mut frames = Vec::new();
        let trimmed = resp_body.trim();
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                frames.push(v);
            }
        } else {
            for line in resp_body.lines() {
                if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        frames.push(v);
                    }
                }
            }
        }
        (status_line, raw, frames)
    };

    let mut trace_meta = adapter_modern_meta()["_meta"].clone();
    trace_meta["io.modelcontextprotocol/requestId"] = json!("ops014-http-info");
    trace_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    trace_meta["vendor.ops/context"] = json!({"secret": token});
    let (http_st, http_raw, http_frames) = http_post(
        &[
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
            (
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
        ],
        &json!({
            "jsonrpc": "2.0",
            "id": "ops014-http-info",
            "method": "tools/call",
            "params": {
                "name": "ledger_verify",
                "arguments": {},
                "_meta": trace_meta
            }
        }),
    );
    assert!(http_st.starts_with("HTTP/1.1 200"), "{http_st} {http_raw}");
    let info_res = http_frames
        .iter()
        .find(|f| f["id"] == "ops014-http-info")
        .expect("HTTP tools/call result frame");
    assert_eq!(call_tool_report(&info_res["result"]), report);
    assert!(http_frames
        .iter()
        .any(|f| f["method"] == "notifications/message"));
    assert!(
        !http_raw.contains(token),
        "operational SSE notifications must never expose bearer token"
    );

    let mut quiet_meta = adapter_modern_meta()["_meta"].clone();
    quiet_meta["io.modelcontextprotocol/requestId"] = json!("ops014-http-quiet");
    let (_, quiet_raw, quiet_frames) = http_post(
        &[
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
        ],
        &json!({
            "jsonrpc": "2.0",
            "id": "ops014-http-quiet",
            "method": "tools/call",
            "params": {
                "name": "ledger_verify",
                "arguments": {},
                "_meta": quiet_meta
            }
        }),
    );
    assert!(
        quiet_frames
            .iter()
            .all(|f| f["method"] != "notifications/message"),
        "omitted logLevel must suppress notifications/message: {quiet_raw}"
    );

    // Task `subscriptions/listen` stream isolation: pushes `notifications/tasks` after `acknowledged`
    // and emits zero `notifications/progress` or `notifications/message` frames (`-0507`).
    let sub_store_dir = scratch.join("ops-sub-store");
    let sub_svc = GmcpService::open_durable(root.to_path_buf(), sub_store_dir).unwrap();
    let sub_tool: Tool = serde_json::from_value(json!({
        "name": "ops_sub_probe",
        "description": "Subscription isolation probe",
        "inputSchema": {"type": "object"}
    }))
    .unwrap();
    sub_svc
        .register_task_tool(
            sub_tool,
            Arc::new(|_, ctx| {
                Box::pin(async move {
                    ctx.set_status_message("running-sub-probe");
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    Ok(CallToolResult {
                        content: vec![ContentBlock::text("sub-done")],
                        structured_content: None,
                        is_error: Some(false),
                        meta: None,
                        result_type: None,
                    })
                })
            }),
        )
        .unwrap();
    let (client_half, server_half) = tokio::io::duplex(ADAPTER_LIMIT.max(64 * 1024));
    let (server_read, server_write) = tokio::io::split(server_half);
    let sub_sess = runtime.spawn(async move {
        let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
            BoundedLines::new(server_read, ADAPTER_LIMIT),
            server_write,
        );
        let running = sub_svc
            .serve(transport)
            .await
            .map_err(|e| format!("duplex serve failed: {e:?}"))?;
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
    let mut dup_cli = AdapterClient {
        write: Box::new(move |bytes| {
            let _ = byte_tx.send(bytes.to_vec());
        }),
        rx: line_rx,
        pending: Vec::new(),
    };
    let sub_spawn = dup_cli.call(
        "ops-sub-call",
        "tools/call",
        json!({
            "name": "ops_sub_probe",
            "arguments": {},
            "_meta": tasks_meta("ops-sub-call")
        }),
    );
    let sub_tid = sub_spawn["result"]["taskId"].as_str().unwrap().to_string();
    dup_cli.send(&json!({
        "jsonrpc": "2.0",
        "id": "ops-sub-listen",
        "method": "subscriptions/listen",
        "params": {
            "notifications": {"taskIds": [sub_tid]},
            "_meta": tasks_meta("ops-sub-listen")
        }
    }));
    for idx in 0..20 {
        let gid = format!("ops-sub-get-{idx}");
        let g = dup_cli.call(
            &gid,
            "tasks/get",
            json!({"taskId": sub_tid, "_meta": tasks_meta(&gid)}),
        );
        if g["result"]["status"] == "completed" {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    let mut sub_notes = std::mem::take(&mut dup_cli.pending);
    while let Ok(msg) = dup_cli.rx.recv_timeout(Duration::from_millis(25)) {
        sub_notes.push(msg);
    }
    let mut saw_ack = false;
    let mut saw_task_note = false;
    for msg in &sub_notes {
        let m = msg["method"].as_str().unwrap_or("");
        assert!(
            m != "notifications/progress" && m != "notifications/message",
            "task subscription leaked forbidden notification: {msg}"
        );
        if m == "notifications/subscriptions/acknowledged" {
            saw_ack = true;
        } else if m == "notifications/tasks" && msg["params"]["taskId"] == sub_tid {
            assert!(saw_ack, "acknowledged must precede notifications/tasks");
            saw_task_note = true;
        }
    }
    assert!(saw_ack && saw_task_note, "{sub_notes:?}");
    dup_cli.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": "ops-sub-listen"}
    }));

    // ── 4. Operational Diagnostics & Triage Error Codes ─────────────────────
    // `-32021` missing per-request tasks capability, `-32601` retired RPCs, `-32602` uniform invalid/terminal.
    let no_cap_err = wire_cli
        .request(
            "tasks/get",
            &json!({"taskId": ops_handle.task_id}),
            &mut |_| {},
        )
        .unwrap_err();
    assert!(
        matches!(no_cap_err, WireError::Rpc(-32021, _)),
        "{no_cap_err:?}"
    );

    for retired in ["tasks/list", "tasks/result"] {
        let ret_ans = dup_cli.call(retired, retired, json!({"_meta": tasks_meta(retired)}));
        assert_eq!(ret_ans["error"]["code"], -32601, "{retired}: {ret_ans}");
    }
    drop(dup_cli);
    adapter_session_ended(&runtime, sub_sess, "014-ops-sub-duplex");

    let (bad_hdr_st, bad_hdr_raw, _) = http_post(
        &[
            ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28),
            ("Mcp-Method", "tasks/get"),
            ("Mcp-Name", "wrong-task-id"),
        ],
        &json!({
            "jsonrpc": "2.0",
            "id": "ops014-bad-hdr",
            "method": "tasks/get",
            "params": {
                "taskId": ops_handle.task_id,
                "_meta": tasks_meta("ops014-bad-hdr")
            }
        }),
    );
    assert!(
        bad_hdr_st.starts_with("HTTP/1.1 400"),
        "{bad_hdr_st} {bad_hdr_raw}"
    );
    assert!(bad_hdr_raw.contains("-32020"), "{bad_hdr_raw}");

    assert!(matches!(
        wire_cli.get_task("00000000-0000-4000-8000-000000000099"),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        wire_cli.cancel_task(&ops_handle.task_id),
        Err(WireError::Rpc(-32602, _))
    ));
    assert!(matches!(
        wire_cli.update_task(&ops_handle.task_id, &json!({})),
        Err(WireError::Rpc(-32602, _))
    ));
    wire_cli.close().unwrap();

    // ── 5. Standards Drift Automation, Migration & Durable Rollback Drill ───
    let sources: Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/sources.json")).expect("sources.json readable"),
    )
    .expect("sources.json valid JSON");
    let pinned_014_docs = [
        "ext-tasks/specification/2026-07-28/tasks.md",
        "docs/specification/2025-11-25/basic/utilities/tasks.mdx",
        "docs/specification/2026-07-28/index.mdx",
        "schema/2026-07-28/schema.json",
        "schema/2025-11-25/schema.json",
    ];
    for doc in &pinned_014_docs {
        let entry = sources["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["document"] == *doc)
            .unwrap_or_else(|| panic!("missing pinned sources entry for {doc}"));
        let disk_sha = susi_paths::hex::encode(Sha256::digest(
            std::fs::read(root.join(".agents/gmcp/spec").join(doc))
                .unwrap_or_else(|e| panic!("missing vendored spec {doc}: {e}")),
        ));
        assert_eq!(
            entry["sha256"].as_str(),
            Some(disk_sha.as_str()),
            "vendored spec digest mismatch for {doc}"
        );
    }

    let refetched = scratch.join("refetched-spec");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let refetched_arg = refetched.to_str().unwrap();
    let clean_a = susi_json(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    );
    let clean_b = susi_json(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    );
    assert_eq!(clean_a, clean_b, "drift output must be deterministic");
    assert_eq!(clean_a["verified"], true);
    assert!(clean_a["findings"].as_array().unwrap().is_empty());

    for victim_rel in [
        "ext-tasks/specification/2026-07-28/tasks.md",
        "docs/specification/2025-11-25/basic/utilities/tasks.mdx",
        "schema/2026-07-28/schema.json",
    ] {
        let victim = refetched.join(victim_rel);
        let original = std::fs::read(&victim).unwrap();
        let mut altered = original.clone();
        altered.extend_from_slice(b"\n<!-- drifted tasks spec -->\n");
        std::fs::write(&victim, &altered).unwrap();

        let drifted = susi_command(
            root,
            &home,
            &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
        )
        .output()
        .unwrap();
        assert!(!drifted.status.success(), "{victim_rel}: drift must fail");
        let drift_report: Value = serde_json::from_slice(&drifted.stdout).unwrap();
        assert_eq!(drift_report["verified"], false);
        assert!(
            drift_report["findings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["document"] == victim_rel && f["kind"] == "hash_drift"),
            "expected hash_drift for {victim_rel}: {drift_report}"
        );

        std::fs::write(&victim, &original).unwrap();
    }
    let rolled_back_drift = susi_json(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    );
    assert_eq!(rolled_back_drift, clean_a);

    let missing_rel = "ext-tasks/specification/2026-07-28/tasks.md";
    std::fs::remove_file(refetched.join(missing_rel)).unwrap();
    let missing_out = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    )
    .output()
    .unwrap();
    assert!(!missing_out.status.success());
    let missing_report: Value = serde_json::from_slice(&missing_out.stdout).unwrap();
    assert_eq!(missing_report["verified"], false);
    assert!(missing_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["document"] == missing_rel && f["kind"] == "unfetchable"));

    let tampered_repo = scratch.join("tampered-repo");
    std::fs::create_dir_all(tampered_repo.join("docs")).unwrap();
    std::fs::create_dir_all(tampered_repo.join(".agents/gmcp")).unwrap();
    copy_tree(
        &root.join(".agents/gmcp/spec"),
        &tampered_repo.join(".agents/gmcp/spec"),
    );
    std::fs::copy(
        root.join(".agents/gmcp/requirements-ledger.json"),
        tampered_repo.join(".agents/gmcp/requirements-ledger.json"),
    )
    .unwrap();
    std::fs::copy(
        root.join(".agents/gmcp/sources.json"),
        tampered_repo.join(".agents/gmcp/sources.json"),
    )
    .unwrap();
    std::fs::copy(
        root.join("docs/gmcp-roadmap.md"),
        tampered_repo.join("docs/gmcp-roadmap.md"),
    )
    .unwrap();
    let tampered_tasks =
        tampered_repo.join(".agents/gmcp/spec/ext-tasks/specification/2026-07-28/tasks.md");
    let mut tasks_bytes = std::fs::read(&tampered_tasks).unwrap();
    tasks_bytes.push(b'\n');
    std::fs::write(&tampered_tasks, tasks_bytes).unwrap();
    assert!(
        susi_gmcp::requirements_ledger::verify_pinned_ledger(&tampered_repo).is_err(),
        "changed ext-tasks spec must fail verify_pinned_ledger closed"
    );

    // Durable store crash-reconciliation & STORE_VERSION rollback drill.
    let drill_dir = scratch.join("rollback-drill");
    let (done_id, interrupted_id) = runtime.block_on(async {
        let mgr = TaskManager::open(&drill_dir).unwrap();
        let owner = TaskPrincipal::local();
        let done = mgr
            .try_spawn_for(owner.clone(), TaskOptions::default(), |_| {
                Box::pin(async {
                    Ok(CallToolResult {
                        content: vec![ContentBlock::text("settled-v1")],
                        structured_content: None,
                        is_error: Some(false),
                        meta: None,
                        result_type: None,
                    })
                })
            })
            .unwrap();
        for _ in 0..30 {
            if mgr.get_task_for(&done.task_id, &owner).unwrap().task.status
                == ServerTaskStatus::Completed
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let inflight = mgr
            .try_spawn_for(owner, TaskOptions::default(), |ctx| {
                Box::pin(async move {
                    ctx.cancelled().await;
                    Err(susi_gmcp::mcp::task_manager::TaskExit::Cancelled)
                })
            })
            .unwrap();
        (done.task_id, inflight.task_id)
    });
    let v1_backup = std::fs::read(drill_dir.join("tasks.json")).unwrap();
    std::fs::write(drill_dir.join("tasks.json"), r#"[2, []]"#).unwrap();
    assert!(
        TaskManager::open(&drill_dir).is_err(),
        "rolled-back binary must refuse future STORE_VERSION = 2 closed"
    );
    std::fs::write(drill_dir.join("tasks.json"), &v1_backup).unwrap();
    let recovered = TaskManager::open(&drill_dir).unwrap();
    let rec_done = recovered
        .get_task_for(&done_id, &TaskPrincipal::local())
        .unwrap();
    assert_eq!(rec_done.task.status, ServerTaskStatus::Completed);
    let rec_done_json = serde_json::to_value(&rec_done).unwrap();
    assert_eq!(rec_done_json["result"]["content"][0]["text"], "settled-v1");
    let rec_int = recovered
        .get_task_for(&interrupted_id, &TaskPrincipal::local())
        .unwrap();
    assert_eq!(rec_int.task.status, ServerTaskStatus::Failed);
    assert!(rec_int
        .task
        .status_message
        .as_deref()
        .unwrap_or_default()
        .contains("task interrupted by server restart"));

    // ── 6. Runbook Contract Bindings & Retained Evidence Verification ────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-014.md"))
        .expect("docs/gmcp/contract-203-014.md readable");
    for required in [
        "## Operations and non-claims",
        "## Operations and incident runbook",
        "### Inspectable state",
        "### Configuration validation",
        "### Bounded metrics, logs and traces",
        "### Diagnostics and incident triage",
        "### Examples",
        "### Incident triage",
        "### Migration (schema evolution and upstream specification re-pinning)",
        "### Rollback",
        "### Production guarantees and non-claims",
        "STORE_VERSION = 1",
        "DEFAULT_TASK_TTL_MS = 300_000",
        "DEFAULT_POLL_INTERVAL_MS = 1_000",
        "MAX_TASK_TTL_MS = 86_400_000",
        "MAX_ACTIVE_TASKS_PER_PRINCIPAL = 64",
        "MAX_GLOBAL_ACTIVE_TASKS = 512",
        "MAX_RETAINED_TERMINAL_TASKS = 2048",
        "MAX_INPUT_REQUESTS_PER_TASK = 128",
        "MAX_SUBSCRIPTION_TASK_IDS = 64",
        "MAX_INPUT_RESPONSE_BYTES = 1_048_576",
        "-32021",
        "-32020",
        "-32601",
        "-32602",
        "-32000",
        "-32603",
        "hash_drift",
        "unfetchable",
        "5fc22b9b5ff248703927c69855f751a061c2d6170f4dc3923204452f859f91a5",
        "bef1bef9f939e09eed8f1928da4d3b36924f4a43b72ffc47a5c1e673f1c1a23b",
    ] {
        assert!(
            contract.contains(required),
            "docs/gmcp/contract-203-014.md missing runbook section/token `{required}`"
        );
    }

    let ledger_sha = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence_dir = root.join(".agents/gmcp/evidence");
    assert!(evidence_dir.is_dir(), ".agents/gmcp/evidence must exist");
    let evidence_files: Vec<PathBuf> = std::fs::read_dir(&evidence_dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    assert!(!evidence_files.is_empty());
    for file in evidence_files {
        let records = newest_records(&std::fs::read_to_string(&file).unwrap());
        assert!(
            records
                .iter()
                .all(|rec| rec["exit"] == 0 && rec["artifact_sha256"] == ledger_sha),
            "{}: retained records must pass and match current ledger sha {ledger_sha}",
            file.display()
        );
    }

    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_file(cfg_dir.join("api_token"));
    let _ = std::fs::remove_dir_all(&home);
}
