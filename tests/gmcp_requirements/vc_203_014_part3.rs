/// GMCP-203-014-05: Interoperability and quality — Official Tasks extension and
/// durable long-running work across server & client roles, direct-vs-gateway
/// differential parity, cross-transport/platform parity, deterministic property
/// invariants, retained regression fixtures, and published SLO / resource budgets.
#[test]
fn gmcp_req_203_014_05_interop() {
    use serde_json::{json, Value};
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use susi_core::plane_bus::{topics, with_watched, PlaneBus, PlaneHandler};
    use susi_gmcp::mcp::model::{
        CallToolResult, ContentBlock, ErrorData, InputRequest, TaskStatus as ServerTaskStatus,
        Tool, TASKS_EXTENSION_ID,
    };
    use susi_gmcp::mcp::task_manager::{
        TaskExit, TaskManager, TaskOptions, TaskPrincipal, DEFAULT_POLL_INTERVAL_MS,
        DEFAULT_TASK_TTL_MS, MAX_ACTIVE_TASKS_PER_PRINCIPAL, MAX_GLOBAL_ACTIVE_TASKS,
        MAX_INPUT_REQUESTS_PER_TASK, MAX_RETAINED_TERMINAL_TASKS, MAX_SUBSCRIPTION_TASK_IDS,
        MAX_TASK_TTL_MS,
    };
    use susi_gmcp::mcp::ServiceExt;
    use susi_gmcp::protocol::{GmcpService, McpGatewayProxy};
    use susi_gmcp::server::GmcpServer;
    use susi_gmcp::stdio::BoundedLines;
    use susi_wire_client::mcp::{
        CallToolOutcome, PersistedTaskHandle, PersistedTaskHandleStore, Session,
        TaskStatus as ClientTaskStatus, WireError, PROTOCOL_VERSION_2026_07_28,
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let (_http_lock, _http_env, home) = gmcp_http_home_guard("014-interop");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let budgets = adapter_budgets(root);
    let within = |name: &str, measured: u128| {
        let ceiling = budgets[name];
        assert!(
            measured <= ceiling,
            "{name}: measured {measured}, contract ceiling {ceiling}"
        );
    };
    let baseline_disk = adapter_tree_bytes(&home);
    let resident_before = adapter_resident_bytes();
    let runtime = adapter_runtime(&home);

    // Scoped PlaneBus tools shared by direct GmcpService, embedded/socket adapters, and HTTP GmcpServer.
    let interop_echo_runs = Arc::new(AtomicUsize::new(0));
    let interop_wait_cancelled = Arc::new(AtomicBool::new(false));
    struct InteropList;
    impl PlaneHandler for InteropList {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let extra = [
                json!({"name": "interop_echo", "description": "Interop echo"}),
                json!({"name": "interop_soft_err", "description": "Interop soft tool error"}),
                json!({"name": "interop_wait", "description": "Interop cancellable wait"}),
            ];
            let mut merged = if let Ok(Value::Array(tools)) =
                PlaneBus::global().request_local_prefix(topics::TOOLS_LIST, payload)
            {
                tools
            } else {
                Vec::new()
            };
            for tool in extra {
                let name = tool.get("name").and_then(Value::as_str);
                if !merged
                    .iter()
                    .any(|item| item.get("name").and_then(Value::as_str) == name)
                {
                    merged.push(tool);
                }
            }
            Ok(Value::Array(merged))
        }
    }
    struct InteropExists;
    impl PlaneHandler for InteropExists {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            if matches!(name, "interop_echo" | "interop_soft_err" | "interop_wait") {
                Ok(json!({ "exists": true}))
            } else {
                PlaneBus::global().request_local_prefix(topics::TOOLS_EXISTS, payload)
            }
        }
    }
    struct InteropExec {
        echo_runs: Arc<AtomicUsize>,
        wait_cancelled: Arc<AtomicBool>,
    }
    impl PlaneHandler for InteropExec {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            let name = payload.get("name").and_then(Value::as_str).unwrap_or("");
            if !matches!(name, "interop_echo" | "interop_soft_err" | "interop_wait") {
                return PlaneBus::global().request_local_prefix(topics::TOOLS_EXECUTE, payload);
            }
            let args = payload.get("args").cloned().unwrap_or(Value::Null);
            let echo_runs = self.echo_runs.clone();
            let wait_cancelled = self.wait_cancelled.clone();
            with_watched(&payload, || match name {
                "interop_echo" => {
                    echo_runs.fetch_add(1, Ordering::SeqCst);
                    let msg = args.get("msg").and_then(Value::as_str).unwrap_or("");
                    Ok(json!({ "text": format!("interop:{msg}")}))
                }
                "interop_soft_err" => Err("Tool Execution Error: synthetic soft tool fault".into()),
                "interop_wait" => {
                    for _ in 0..300 {
                        if susi_core::progress::is_cancelled() {
                            wait_cancelled.store(true, Ordering::Release);
                            return Ok(json!({ "text": "cancelled"}));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(json!({ "text": "done"}))
                }
                other => Err(format!("unexpected tool {other}")),
            })
        }
    }
    struct RestoreInteropPlane;
    impl Drop for RestoreInteropPlane {
        fn drop(&mut self) {
            let bus = PlaneBus::global();
            bus.unregister(topics::TOOLS_LIST);
            bus.unregister(topics::TOOLS_EXISTS);
            bus.unregister(topics::TOOLS_EXECUTE);
            susi_tools::plane_handler::register();
        }
    }
    let _restore_plane = RestoreInteropPlane;
    PlaneBus::global().register(topics::TOOLS_LIST, Arc::new(InteropList));
    PlaneBus::global().register(topics::TOOLS_EXISTS, Arc::new(InteropExists));
    PlaneBus::global().register(
        topics::TOOLS_EXECUTE,
        Arc::new(InteropExec {
            echo_runs: interop_echo_runs.clone(),
            wait_cancelled: interop_wait_cancelled.clone(),
        }),
    );

    let tasks_meta = |req_id: &str| -> Value {
        json!({ "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION_2026_07_28, "io.modelcontextprotocol/requestId": req_id, "io.modelcontextprotocol/clientCapabilities": { "extensions": {TASKS_EXTENSION_ID: {}}}})
    };
    let plain_modern_meta = |req_id: &str| -> Value {
        json!({ "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION_2026_07_28, "io.modelcontextprotocol/requestId": req_id, "io.modelcontextprotocol/clientCapabilities": {}})
    };
    let is_uuid_v4 = |s: &str| -> bool {
        let b = s.as_bytes();
        b.len() == 36
            && b[8] == b'-'
            && b[13] == b'-'
            && b[18] == b'-'
            && b[23] == b'-'
            && b[14] == b'4'
            && matches!(b[19], b'8' | b'9' | b'a' | b'b')
            && b.iter()
                .enumerate()
                .all(|(i, c)| matches!(i, 8 | 13 | 18 | 23) || c.is_ascii_hexdigit())
    };

    // ── 1. Contract & SLO Binding + Server-Role Official Wire & SDK Coexistence ──
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-014.md"))
        .expect("docs/gmcp/contract-203-014.md must exist");
    for idx in 1..=8 {
        assert!(contract.contains(&format!("F-POS-014-0{idx}")));
        assert!(contract.contains(&format!("F-NEG-014-0{idx}")));
    }
    assert_eq!(DEFAULT_TASK_TTL_MS, 300_000);
    assert_eq!(MAX_TASK_TTL_MS, 86_400_000);
    assert_eq!(DEFAULT_POLL_INTERVAL_MS, 1_000);
    assert_eq!(MAX_ACTIVE_TASKS_PER_PRINCIPAL, 64);
    assert_eq!(MAX_GLOBAL_ACTIVE_TASKS, 512);
    assert_eq!(MAX_RETAINED_TERMINAL_TASKS, 2_048);
    assert_eq!(MAX_INPUT_REQUESTS_PER_TASK, 128);
    assert_eq!(MAX_SUBSCRIPTION_TASK_IDS, 64);

    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "tok-014-interop-bearer";
    std::fs::write(cfg_dir.join("api_token"), format!("{token}\n")).unwrap();
    let auth_header = format!("Bearer {token}");

    let startup_t0 = Instant::now();
    let direct_store = scratch.join("direct-svc-tasks");
    let direct_svc = GmcpService::open_durable(root.to_path_buf(), direct_store).unwrap();
    susi_gmcp::catalog::install(&direct_svc).expect("catalog install");
    let mrtr_tool: Tool = serde_json::from_value(json!({ "name": "interop_mrtr", "description": "Two-key MRTR task tool", "inputSchema": {"type": "object"}}))
    .unwrap();
    direct_svc
        .register_task_tool(
            mrtr_tool,
            Arc::new(|args, ctx| {
                Box::pin(async move {
                    let r1: InputRequest =
                        serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                    let r2: InputRequest = serde_json::from_value(json!({ "method": "elicitation/create", "params": { "mode": "form", "message": "Confirm interop", "requestedSchema": {"type": "object"}}}))
                    .unwrap();
                    let (a1, a2) =
                        tokio::try_join!(ctx.request_input("k1", r1), ctx.request_input("k2", r2))?;
                    let tag = args
                        .arguments
                        .as_ref()
                        .and_then(|m| m.get("tag"))
                        .and_then(Value::as_str)
                        .unwrap_or("default");
                    Ok(CallToolResult {
                        content: vec![ContentBlock::text(format!("mrtr:{tag}"))],
                        structured_content: Some(json!({"k1": a1, "k2": a2, "tag": tag})),
                        is_error: Some(false),
                        meta: None,
                        result_type: None,
                    })
                })
            }),
        )
        .unwrap();

    let http_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let http_port = http_listener.local_addr().unwrap().port();
    let http_addr = format!("127.0.0.1:{http_port}");
    let root_http = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        GmcpServer::start_http_server(root_http, vec![http_listener], None, false);
    });
    for _ in 0..80 {
        if TcpStream::connect(&http_addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    within("startup_ms", startup_t0.elapsed().as_millis());

    let spawn_service_duplex = |rt: &tokio::runtime::Runtime, svc: GmcpService, limit: usize| {
        let (client_half, server_half) = tokio::io::duplex(limit.max(64 * 1024));
        let (server_read, server_write) = tokio::io::split(server_half);
        let sess = rt.spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                BoundedLines::new(server_read, limit),
                server_write,
            );
            let running = svc
                .serve(transport)
                .await
                .map_err(|e| format!("duplex serve failed: {e:?}"))?;
            running.waiting().await.map_err(|e| e.to_string())
        });
        let (client_read, mut client_write) = tokio::io::split(client_half);
        let (line_tx, line_rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
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
        rt.spawn(async move {
            while let Some(bytes) = byte_rx.recv().await {
                if client_write.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = client_write.shutdown().await;
        });
        let client = AdapterClient {
            write: Box::new(move |bytes| {
                let _ = byte_tx.send(bytes.to_vec());
            }),
            rx: line_rx,
            pending: Vec::new(),
        };
        (client, sess)
    };

    // 1a. Server-role reference `ext-tasks@2026-07-28` wire client over gated duplex:
    //     vendor `_meta` preservation + server/discover + MRTR partial/final tasks/update + subscriptions/listen.
    let (mut ref_dup, ref_dup_sess) =
        spawn_service_duplex(&runtime, direct_svc.clone(), ADAPTER_LIMIT);
    let mut disc_meta = tasks_meta("ref-disc");
    disc_meta["vendor.reference/trace"] = json!({"sdk": "ext-tasks-ref", "rev": "2026-07-28"});
    disc_meta["io.modelcontextprotocol/clientCapabilities"]["extensions"]
        ["vendor.reference/extra"] = json!({"ok": true});
    let req_t0 = Instant::now();
    let disc_ans = ref_dup.call("ref-disc", "server/discover", json!({"_meta": disc_meta}));
    within("request_ms", req_t0.elapsed().as_millis());
    assert!(disc_ans["result"]["capabilities"]["extensions"][TASKS_EXTENSION_ID].is_object());

    let mrtr_spawn = ref_dup.call(
        "ref-mrtr-call",
        "tools/call",
        json!({ "name": "interop_mrtr", "arguments": {"tag": "ref-wire"}, "_meta": tasks_meta("ref-mrtr-call")}),
    );
    assert_eq!(mrtr_spawn["result"]["resultType"], "task", "{mrtr_spawn}");
    let mrtr_tid = mrtr_spawn["result"]["taskId"].as_str().unwrap().to_string();
    assert!(
        is_uuid_v4(&mrtr_tid),
        "taskId must be CSPRNG UUIDv4: {mrtr_tid}"
    );
    assert_eq!(
        mrtr_spawn["result"]["ttlMs"].as_u64(),
        Some(DEFAULT_TASK_TTL_MS)
    );
    assert_eq!(
        mrtr_spawn["result"]["pollIntervalMs"].as_u64(),
        Some(DEFAULT_POLL_INTERVAL_MS)
    );

    // Subscribe to task notifications before answering MRTR keys.
    ref_dup.send(&json!({ "jsonrpc": "2.0", "id": "ref-sub-1", "method": "subscriptions/listen", "params": { "notifications": {"taskIds": [mrtr_tid]}, "_meta": tasks_meta("ref-sub-1")}}));

    // Wait for `input_required` carrying both `k1` and `k2`.
    let mut saw_both_keys = false;
    for idx in 0..15 {
        let gid = format!("ref-get-ir-{idx}");
        let g = ref_dup.call(
            &gid,
            "tasks/get",
            json!({"taskId": mrtr_tid, "_meta": tasks_meta(&gid)}),
        );
        assert_eq!(g["result"]["resultType"], "complete", "{g}");
        if g["result"]["status"] == "input_required"
            && g["result"]["inputRequests"]["k1"].is_object()
            && g["result"]["inputRequests"]["k2"].is_object()
        {
            saw_both_keys = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        saw_both_keys,
        "MRTR task must surface both inputRequests keys"
    );

    // Partial update (`k1` + unknown stale key `stale_ignored`) -> stays `input_required` with only `k2` left.
    let upd_1 = ref_dup.call(
        "ref-upd-1",
        "tasks/update",
        json!({ "taskId": mrtr_tid, "inputResponses": { "k1": {"roots": [{"uri": "file:///workspace"}]}, "stale_ignored": {"ignored": true}}, "_meta": tasks_meta("ref-upd-1")}),
    );
    assert_eq!(upd_1["result"]["resultType"], "complete", "{upd_1}");
    let after_p1 = ref_dup.call(
        "ref-get-after-p1",
        "tasks/get",
        json!({"taskId": mrtr_tid, "_meta": tasks_meta("ref-get-after-p1")}),
    );
    assert_eq!(after_p1["result"]["status"], "input_required");
    assert!(after_p1["result"]["inputRequests"].get("k1").is_none());
    assert!(after_p1["result"]["inputRequests"]["k2"].is_object());

    // Final update (`k2` + replayed `k1` which must be ignored cleanly) -> settles `completed`.
    let upd_2 = ref_dup.call(
        "ref-upd-2",
        "tasks/update",
        json!({ "taskId": mrtr_tid, "inputResponses": { "k1": {"roots": [{"uri": "file:///tampered"}]}, "k2": {"action": "accept", "content": {"ok": true}}}, "_meta": tasks_meta("ref-upd-2")}),
    );
    assert_eq!(upd_2["result"]["resultType"], "complete", "{upd_2}");

    let mut settled_mrtr = None;
    for idx in 0..15 {
        let gid = format!("ref-get-done-{idx}");
        let g = ref_dup.call(
            &gid,
            "tasks/get",
            json!({"taskId": mrtr_tid, "_meta": tasks_meta(&gid)}),
        );
        if g["result"]["status"] == "completed" {
            settled_mrtr = Some(g);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let settled_mrtr = settled_mrtr.expect("MRTR task must complete");
    assert_eq!(
        settled_mrtr["result"]["result"]["structuredContent"]["k1"]["roots"][0]["uri"],
        "file:///workspace",
        "replayed k1 must not overwrite original k1 response"
    );
    assert_eq!(
        settled_mrtr["result"]["result"]["structuredContent"]["tag"],
        "ref-wire"
    );

    // Verify `notifications/subscriptions/acknowledged` + `notifications/tasks` arrived with zero progress/message leak.
    let mut drain_notes = std::mem::take(&mut ref_dup.pending);
    while let Ok(msg) = ref_dup.rx.recv_timeout(Duration::from_millis(30)) {
        drain_notes.push(msg);
    }
    let mut ack_pos = None;
    let mut completed_note = false;
    for (idx, msg) in drain_notes.iter().enumerate() {
        let m = msg["method"].as_str().unwrap_or("");
        assert!(
            m != "notifications/progress" && m != "notifications/message",
            "subscriptions/listen leaked forbidden notification: {msg}"
        );
        if m == "notifications/subscriptions/acknowledged"
            && msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "ref-sub-1"
        {
            ack_pos.get_or_insert(idx);
        } else if m == "notifications/tasks" && msg["params"]["taskId"] == mrtr_tid {
            assert!(
                ack_pos.is_some(),
                "acknowledged must precede notifications/tasks"
            );
            if msg["params"]["status"] == "completed" {
                completed_note = true;
            }
        }
    }
    assert!(ack_pos.is_some() && completed_note, "{drain_notes:?}");
    ref_dup.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "ref-sub-1"}}));
    drop(ref_dup);
    adapter_session_ended(&runtime, ref_dup_sess, "014-interop ref-dup");

    // 1b. Coexistence on live HTTP `/mcp`: `2026-07-28` SDK + `2025-11-25` SDK + `susi_core::mcp_client`.
    let mut http_mod_cli =
        Session::open_2026_07_28(&http_addr, Some(token), Duration::from_secs(15)).unwrap();
    assert!(http_mod_cli.supports_server_tasks());
    let bridge_res = http_mod_cli
        .call_tool_resolving_tasks(
            "interop_echo",
            &json!({"msg": "sdk-bridge"}),
            Some(Duration::from_secs(10)),
            |_| Ok(json!({})),
        )
        .unwrap();
    assert_eq!(bridge_res["content"][0]["text"], "interop:sdk-bridge");
    let bridge_soft = http_mod_cli
        .call_tool_resolving_tasks(
            "interop_soft_err",
            &json!({}),
            Some(Duration::from_secs(10)),
            |_| Ok(json!({})),
        )
        .unwrap();
    assert_eq!(bridge_soft["isError"], json!(true));

    let mut http_leg_cli =
        Session::open(&http_addr, Some(token), Duration::from_secs(15)).expect("2025-11-25 open");
    assert!(http_leg_cli.supports_server_tasks());
    let leg_sync = http_leg_cli
        .call_tool(
            "interop_echo",
            &json!({"msg": "leg-coexist"}),
            None,
            &mut |_| {},
        )
        .unwrap();
    assert_eq!(leg_sync["content"][0]["text"], "interop:leg-coexist");
    for retired in ["tasks/list", "tasks/result"] {
        assert!(matches!(
            http_leg_cli.request(retired, &json!({}), &mut |_| {}),
            Err(WireError::Rpc(-32601, _))
        ));
    }
    http_leg_cli.close().unwrap();

    let core_sync = susi_core::mcp_client::call_tool(
        &http_addr,
        "interop_echo",
        &json!({"msg": "core-coexist"}),
        Some(token),
    )
    .unwrap();
    assert_eq!(core_sync["content"][0]["text"], "interop:core-coexist");

    // ── 2. Client-Role Interoperability Against Reference Peer `ext-tasks@2026-07-28` HTTP Server ──
    let peer_listener = TcpListener::bind("127.0.0.1:0").unwrap();
    peer_listener.set_nonblocking(false).unwrap();
    let peer_addr = format!("127.0.0.1:{}", peer_listener.local_addr().unwrap().port());
    let peer_stop = Arc::new(AtomicBool::new(false));
    let peer_stop_srv = peer_stop.clone();
    let peer_tool_calls = Arc::new(AtomicUsize::new(0));
    let peer_tool_calls_srv = peer_tool_calls.clone();
    let peer_flaky_tool_calls = Arc::new(AtomicUsize::new(0));
    let peer_flaky_tool_calls_srv = peer_flaky_tool_calls.clone();
    let peer_get_attempts = Arc::new(AtomicUsize::new(0));
    let peer_get_attempts_srv = peer_get_attempts.clone();
    let peer_recorded_headers = Arc::new(Mutex::new(Vec::<(String, Option<String>)>::new()));
    let peer_recorded_headers_srv = peer_recorded_headers.clone();

    let peer_srv = std::thread::spawn(move || {
        type ParsedHttp = (BTreeMap<String, String>, Vec<u8>);
        fn read_full_http(stream: &mut TcpStream) -> Option<ParsedHttp> {
            let mut buf = Vec::with_capacity(2048);
            let mut chunk = [0u8; 1024];
            let hdr_end = loop {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return None,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            };
            let hdr_str = String::from_utf8_lossy(&buf[..hdr_end]);
            let mut headers = BTreeMap::new();
            for line in hdr_str.lines().skip(1) {
                if let Some((k, v)) = line.split_once(':') {
                    headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let clen = headers
                .get("content-length")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            while buf.len() < hdr_end + clen {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
            }
            let end = (hdr_end + clen).min(buf.len());
            Some((headers, buf[hdr_end..end].to_vec()))
        }

        fn send_http(stream: &mut TcpStream, status: &str, body: &str) {
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Write);
            let _ = stream.read(&mut [0u8; 16]);
        }

        let peer_tid = "4f8a2b10-7c41-4e92-9a11-001122334455";
        let mut mrtr_answered = false;
        let mut peer_cancelled = false;

        while let Ok((mut stream, _)) = peer_listener.accept() {
            if peer_stop_srv.load(Ordering::Acquire) {
                break;
            }
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let Some((headers, body)) = read_full_http(&mut stream) else {
                continue;
            };
            if headers.get("authorization").map(String::as_str) != Some("Bearer peer-ref-tok") {
                send_http(&mut stream, "401 Unauthorized", r#"{"error":"unauth"}"#);
                continue;
            }
            assert_eq!(
                headers.get("mcp-protocol-version").map(String::as_str),
                Some(PROTOCOL_VERSION_2026_07_28),
                "client must send MCP-Protocol-Version: 2026-07-28"
            );
            let Ok(rpc) = serde_json::from_slice::<Value>(&body) else {
                send_http(&mut stream, "400 Bad Request", r#"{"error":"bad json"}"#);
                continue;
            };
            let id = rpc.get("id").cloned().unwrap_or(Value::Null);
            let method = rpc["method"].as_str().unwrap_or("").to_string();
            let mcp_method = headers.get("mcp-method").cloned().unwrap_or_default();
            assert_eq!(
                mcp_method, method,
                "client must attach matching Mcp-Method header"
            );
            let mcp_name = headers.get("mcp-name").cloned();
            peer_recorded_headers_srv
                .lock()
                .unwrap()
                .push((mcp_method, mcp_name.clone()));

            // Every 2026-07-28 request must carry `_meta["io.modelcontextprotocol/protocolVersion"]` and `clientCapabilities`.
            assert_eq!(
                rpc["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
                PROTOCOL_VERSION_2026_07_28
            );
            assert!(
                rpc["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"].is_object()
            );

            match method.as_str() {
                "server/discover" => {
                    let ans = json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": PROTOCOL_VERSION_2026_07_28, "supportedVersions": [PROTOCOL_VERSION_2026_07_28, "2025-11-25"], "serverInfo": {"name": "ref-ext-tasks-peer", "version": "1.0.0"}, "capabilities": { "tools": {}, "extensions": {TASKS_EXTENSION_ID: {}}}}});
                    send_http(&mut stream, "200 OK", &ans.to_string());
                }
                "tools/call" => {
                    let tname = rpc["params"]["name"].as_str().unwrap_or("");
                    assert_eq!(mcp_name.as_deref(), Some(tname));
                    if tname == "flaky_non_idempotent" {
                        peer_flaky_tool_calls_srv.fetch_add(1, Ordering::SeqCst);
                        send_http(
                            &mut stream,
                            "503 Service Unavailable",
                            r#"{"error":"upstream transient"}"#,
                        );
                        continue;
                    }
                    peer_tool_calls_srv.fetch_add(1, Ordering::SeqCst);
                    assert!(
                        rpc["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]
                            ["extensions"][TASKS_EXTENSION_ID]
                            .is_object(),
                        "task-opted tools/call must declare io.modelcontextprotocol/tasks per request"
                    );
                    let tid = if tname == "peer_cancel_me" {
                        "9c8b7a60-1234-4abc-8def-556677889900"
                    } else {
                        peer_tid
                    };
                    let ans = json!({ "jsonrpc": "2.0", "id": id, "result": { "resultType": "task", "taskId": tid, "status": "working", "createdAt": "2026-11-15T12:00:00Z", "lastUpdatedAt": "2026-11-15T12:00:00Z", "ttlMs": 15000, "pollIntervalMs": 10}});
                    send_http(&mut stream, "200 OK", &ans.to_string());
                }
                "tasks/get" => {
                    let tid = rpc["params"]["taskId"].as_str().unwrap_or("");
                    assert_eq!(
                        mcp_name.as_deref(),
                        Some(tid),
                        "client must attach Mcp-Name == params.taskId on tasks/get"
                    );
                    assert!(
                        rpc["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]
                            ["extensions"][TASKS_EXTENSION_ID]
                            .is_object()
                    );
                    if tid == "9c8b7a60-1234-4abc-8def-556677889900" {
                        let st = if peer_cancelled {
                            "cancelled"
                        } else {
                            "working"
                        };
                        let ans = json!({ "jsonrpc": "2.0", "id": id, "result": { "resultType": "complete", "taskId": tid, "status": st, "createdAt": "2026-11-15T12:00:00Z", "lastUpdatedAt": "2026-11-15T12:00:01Z", "ttlMs": 15000, "pollIntervalMs": 10}});
                        send_http(&mut stream, "200 OK", &ans.to_string());
                        continue;
                    }
                    let attempt = peer_get_attempts_srv.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        // First tasks/get returns transient 503 to prove idempotent client retry!
                        send_http(
                            &mut stream,
                            "503 Service Unavailable",
                            r#"{"error":"transient gateway overload"}"#,
                        );
                        continue;
                    }
                    let res_obj = if !mrtr_answered {
                        json!({ "resultType": "complete", "taskId": peer_tid, "status": "input_required", "createdAt": "2026-11-15T12:00:00Z", "lastUpdatedAt": "2026-11-15T12:00:01Z", "ttlMs": 15000, "pollIntervalMs": 10, "inputRequests": { "ask_root": {"method": "roots/list"}}})
                    } else {
                        json!({ "resultType": "complete", "taskId": peer_tid, "status": "completed", "createdAt": "2026-11-15T12:00:00Z", "lastUpdatedAt": "2026-11-15T12:00:02Z", "ttlMs": 15000, "pollIntervalMs": 10, "result": { "content": [{"type": "text", "text": "peer-mrtr-settled"}], "isError": false}})
                    };
                    send_http(
                        &mut stream,
                        "200 OK",
                        &json!({"jsonrpc": "2.0", "id": id, "result": res_obj}).to_string(),
                    );
                }
                "tasks/update" => {
                    let tid = rpc["params"]["taskId"].as_str().unwrap_or("");
                    assert_eq!(mcp_name.as_deref(), Some(tid));
                    assert!(
                        rpc["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]
                            ["extensions"][TASKS_EXTENSION_ID]
                            .is_object()
                    );
                    assert!(rpc["params"]["inputResponses"]["ask_root"].is_object());
                    mrtr_answered = true;
                    send_http(
                        &mut stream,
                        "200 OK",
                        &json!({"jsonrpc": "2.0", "id": id, "result": {"resultType": "complete"}})
                            .to_string(),
                    );
                }
                "tasks/cancel" => {
                    let tid = rpc["params"]["taskId"].as_str().unwrap_or("");
                    assert_eq!(mcp_name.as_deref(), Some(tid));
                    assert!(
                        rpc["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"]
                            ["extensions"][TASKS_EXTENSION_ID]
                            .is_object()
                    );
                    peer_cancelled = true;
                    send_http(
                        &mut stream,
                        "200 OK",
                        &json!({"jsonrpc": "2.0", "id": id, "result": {"resultType": "complete"}})
                            .to_string(),
                    );
                }
                other => {
                    send_http(
                        &mut stream,
                        "400 Bad Request",
                        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": other}})
                            .to_string(),
                    );
                }
            }
        }
    });

    let mut peer_cli =
        Session::open_2026_07_28(&peer_addr, Some("peer-ref-tok"), Duration::from_secs(10))
            .unwrap();
    assert!(peer_cli.supports_server_tasks());

    // Non-idempotent `tools/call` MUST NOT be retried on HTTP 503.
    let flaky_err = peer_cli
        .call_tool_with_tasks("flaky_non_idempotent", &json!({}), true, &mut |_| {})
        .unwrap_err();
    assert!(
        matches!(flaky_err, WireError::Http(503, _)),
        "{flaky_err:?}"
    );
    assert_eq!(
        peer_flaky_tool_calls.load(Ordering::SeqCst),
        1,
        "non-idempotent tools/call must never be retried on transient HTTP 503"
    );

    // Spawn async task on Reference Peer, persist handle without credentials, survive transient 503 on `tasks/get`,
    // fulfill MRTR `inputRequests` via `tasks/update`, and settle `completed`.
    let peer_handle = match peer_cli
        .call_tool_with_tasks("peer_long_work", &json!({"step": 1}), true, &mut |_| {})
        .unwrap()
    {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected Task outcome from reference peer, got {other:?}"),
    };
    assert_eq!(peer_tool_calls.load(Ordering::SeqCst), 1);

    let handle_store_path = scratch.join("peer-handles.json");
    let mut hstore = PersistedTaskHandleStore::open(&handle_store_path).unwrap();
    hstore
        .save(PersistedTaskHandle::from_handle(
            &peer_addr,
            PROTOCOL_VERSION_2026_07_28,
            &peer_handle,
        ))
        .unwrap();
    let raw_store_str = std::fs::read_to_string(&handle_store_path).unwrap();
    assert!(
        !raw_store_str.contains("peer-ref-tok"),
        "PersistedTaskHandleStore must never write bearer secrets to disk"
    );

    let mut ir_invocations = 0usize;
    let settled_peer = peer_cli
        .poll_task_until_done(&peer_handle, Some(Duration::from_secs(10)), |entries| {
            ir_invocations += 1;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].key, "ask_root");
            assert_eq!(entries[0].method, "roots/list");
            Ok(json!({ "ask_root": {"roots": [{"uri": "file:///client-root"}]}}))
        })
        .unwrap();
    assert_eq!(ir_invocations, 1);
    assert!(
        peer_get_attempts.load(Ordering::SeqCst) >= 3,
        "tasks/get must retry across transient 503 and complete after tasks/update"
    );
    assert_eq!(settled_peer.handle.status, ClientTaskStatus::Completed);
    assert_eq!(
        settled_peer.result.as_ref().unwrap()["content"][0]["text"],
        "peer-mrtr-settled"
    );
    assert!(hstore.update_from_snapshot(&settled_peer).unwrap());
    let reopened_hstore = PersistedTaskHandleStore::open(&handle_store_path).unwrap();
    assert_eq!(
        reopened_hstore.get(&peer_handle.task_id).unwrap().status,
        ClientTaskStatus::Completed
    );

    // Exercise client `cancel_task` against Reference Peer.
    let cancel_h = match peer_cli
        .call_tool_with_tasks("peer_cancel_me", &json!({}), true, &mut |_| {})
        .unwrap()
    {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected Task outcome for peer_cancel_me, got {other:?}"),
    };
    peer_cli.cancel_task(&cancel_h.task_id).unwrap();
    let cancelled_snap = peer_cli.get_task(&cancel_h.task_id).unwrap();
    assert_eq!(cancelled_snap.handle.status, ClientTaskStatus::Cancelled);

    peer_stop.store(true, Ordering::Release);
    let _ = TcpStream::connect(&peer_addr);
    peer_srv.join().unwrap();
    let recorded = peer_recorded_headers.lock().unwrap().clone();
    for required_method in [
        "server/discover",
        "tools/call",
        "tasks/get",
        "tasks/update",
        "tasks/cancel",
    ] {
        assert!(
            recorded.iter().any(|(m, _)| m == required_method),
            "reference peer must observe {required_method}: {recorded:?}"
        );
    }

    // ── 3. Direct-vs-Gateway Differential Parity & Cross-Transport / Platform Parity ──
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct SurfaceTranscript {
        discover_has_tasks: bool,
        policy_deny_sync: bool,
        task_spawn_type: String,
        settled_status: String,
        settled_verified: bool,
        missing_cap_code: i64,
        unknown_task_code: i64,
        update_terminal_code: i64,
        cancel_terminal_code: i64,
        tasks_list_code: i64,
        tasks_result_code: i64,
        no_id_code: i64,
    }

    let run_adapter_transcript = |client: &mut AdapterClient, tag: &str| -> SurfaceTranscript {
        let disc = client.call(
            &format!("{tag}-disc"),
            "server/discover",
            json!({"_meta": tasks_meta(&format!("{tag}-disc"))}),
        );
        let discover_has_tasks =
            disc["result"]["capabilities"]["extensions"][TASKS_EXTENSION_ID].is_object();

        let pol = client.call(
            &format!("{tag}-pol"),
            "tools/call",
            json!({ "name": "susi_restricted_probe", "arguments": {}, "_meta": tasks_meta(&format!("{tag}-pol"))}),
        );
        let policy_deny_sync =
            pol["result"]["resultType"] == "complete" && pol["result"]["isError"] == true;

        let sp = client.call(
            &format!("{tag}-spawn"),
            "tools/call",
            json!({ "name": "ledger_verify", "arguments": {}, "_meta": tasks_meta(&format!("{tag}-spawn"))}),
        );
        let task_spawn_type = sp["result"]["resultType"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let tid = sp["result"]["taskId"].as_str().unwrap().to_string();
        let mut settled_status = String::new();
        let mut settled_verified = false;
        for i in 0..25 {
            let gid = format!("{tag}-get-{i}");
            let g = client.call(
                &gid,
                "tasks/get",
                json!({"taskId": tid, "_meta": tasks_meta(&gid)}),
            );
            if g["result"]["status"] == "completed" {
                settled_status = "completed".into();
                settled_verified = call_tool_report(&g["result"]["result"])["verified"] == true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let missing_cap = client.call(
            &format!("{tag}-nocap"),
            "tasks/get",
            json!({"taskId": tid, "_meta": plain_modern_meta(&format!("{tag}-nocap"))}),
        );
        let unknown_task = client.call(
            &format!("{tag}-unk"),
            "tasks/get",
            json!({ "taskId": "00000000-0000-4000-8000-000000000099", "_meta": tasks_meta(&format!("{tag}-unk"))}),
        );
        let update_term = client.call(
            &format!("{tag}-updt"),
            "tasks/update",
            json!({ "taskId": tid, "inputResponses": {}, "_meta": tasks_meta(&format!("{tag}-updt"))}),
        );
        let cancel_term = client.call(
            &format!("{tag}-cant"),
            "tasks/cancel",
            json!({"taskId": tid, "_meta": tasks_meta(&format!("{tag}-cant"))}),
        );
        let t_list = client.call(
            &format!("{tag}-list"),
            "tasks/list",
            json!({"_meta": tasks_meta(&format!("{tag}-list"))}),
        );
        let t_res = client.call(
            &format!("{tag}-res"),
            "tasks/result",
            json!({"taskId": tid, "_meta": tasks_meta(&format!("{tag}-res"))}),
        );
        (client.write)(
            br#"{"jsonrpc":"2.0","method":"tasks/get","params":{"taskId":"tid-x"}}"#.as_slice(),
        );
        (client.write)(b"\n");
        let no_id = client.rejection("no-id tasks/get");

        SurfaceTranscript {
            discover_has_tasks,
            policy_deny_sync,
            task_spawn_type,
            settled_status,
            settled_verified,
            missing_cap_code: missing_cap["error"]["code"].as_i64().unwrap_or(0),
            unknown_task_code: unknown_task["error"]["code"].as_i64().unwrap_or(0),
            update_terminal_code: update_term["error"]["code"].as_i64().unwrap_or(0),
            cancel_terminal_code: cancel_term["error"]["code"].as_i64().unwrap_or(0),
            tasks_list_code: t_list["error"]["code"].as_i64().unwrap_or(0),
            tasks_result_code: t_res["error"]["code"].as_i64().unwrap_or(0),
            no_id_code: no_id["error"]["code"].as_i64().unwrap_or(0),
        }
    };

    // Surface A: Direct `GmcpService` duplex.
    let (mut dir_cli, dir_sess) = spawn_service_duplex(&runtime, direct_svc.clone(), ADAPTER_LIMIT);
    let tr_direct = run_adapter_transcript(&mut dir_cli, "diff-direct");
    drop(dir_cli);
    adapter_session_ended(&runtime, dir_sess, "014-interop diff-direct");
    assert_eq!(
        tr_direct,
        SurfaceTranscript {
            discover_has_tasks: true,
            policy_deny_sync: true,
            task_spawn_type: "task".into(),
            settled_status: "completed".into(),
            settled_verified: true,
            missing_cap_code: -32021,
            unknown_task_code: -32602,
            update_terminal_code: -32602,
            cancel_terminal_code: -32602,
            tasks_list_code: -32601,
            tasks_result_code: -32601,
            no_id_code: -32600,
        }
    );

    // Surface B: Embedded adapter.
    let (mut emb_cli, emb_sess) = embedded_adapter(&runtime, root);
    let tr_embedded = run_adapter_transcript(&mut emb_cli, "diff-emb");
    drop(emb_cli);
    adapter_session_ended(&runtime, emb_sess, "014-interop diff-emb");
    assert_eq!(
        tr_embedded, tr_direct,
        "embedded adapter must match direct GmcpService"
    );

    // Surface C: Unix-domain socket adapter (on Unix platforms).
    #[cfg(unix)]
    {
        let (mut sock_cli, sock_sess) = socket_adapter(&runtime, root);
        let tr_socket = run_adapter_transcript(&mut sock_cli, "diff-sock");
        drop(sock_cli);
        adapter_session_ended(&runtime, sock_sess, "014-interop diff-sock");
        assert_eq!(
            tr_socket, tr_direct,
            "unix socket adapter must match direct GmcpService"
        );
    }

    // Surface D: Shipped CLI `susi mcp serve` stdio binary (`StdioSession`).
    let stdio_home = scratch.join("stdio-parity-home");
    std::fs::create_dir_all(&stdio_home).unwrap();
    let mut stdio_cli = StdioSession::spawn_raw(root, &stdio_home);
    let s_disc = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-disc", "method": "server/discover", "params": {"_meta": tasks_meta("s-disc")}}),
            Duration::from_secs(30),
        )
        .unwrap();
    let s_pol = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-pol", "method": "tools/call", "params": {"name": "susi_restricted_probe", "arguments": {}, "_meta": tasks_meta("s-pol")}}),
            Duration::from_secs(15),
        )
        .unwrap();
    let s_sp = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-sp", "method": "tools/call", "params": {"name": "ledger_verify", "arguments": {}, "_meta": tasks_meta("s-sp")}}),
            Duration::from_secs(30),
        )
        .unwrap();
    let s_tid = s_sp["result"]["taskId"].as_str().unwrap().to_string();
    let mut s_settled_status = String::new();
    let mut s_settled_verified = false;
    for i in 0..25 {
        let gid = format!("s-get-{i}");
        let g = stdio_cli
            .request_verbatim(
                &json!({"jsonrpc": "2.0", "id": gid, "method": "tasks/get", "params": {"taskId": s_tid, "_meta": tasks_meta(&gid)}}),
                Duration::from_secs(15),
            )
            .unwrap();
        if g["result"]["status"] == "completed" {
            s_settled_status = "completed".into();
            s_settled_verified = call_tool_report(&g["result"]["result"])["verified"] == true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let s_nocap = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-nocap", "method": "tasks/get", "params": {"taskId": s_tid, "_meta": plain_modern_meta("s-nocap")}}),
            Duration::from_secs(10),
        )
        .unwrap();
    let s_unk = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-unk", "method": "tasks/get", "params": {"taskId": "00000000-0000-4000-8000-000000000099", "_meta": tasks_meta("s-unk")}}),
            Duration::from_secs(10),
        )
        .unwrap();
    let s_updt = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-updt", "method": "tasks/update", "params": {"taskId": s_tid, "inputResponses": {}, "_meta": tasks_meta("s-updt")}}),
            Duration::from_secs(10),
        )
        .unwrap();
    let s_cant = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-cant", "method": "tasks/cancel", "params": {"taskId": s_tid, "_meta": tasks_meta("s-cant")}}),
            Duration::from_secs(10),
        )
        .unwrap();
    let s_list = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-list", "method": "tasks/list", "params": {"_meta": tasks_meta("s-list")}}),
            Duration::from_secs(10),
        )
        .unwrap();
    let s_res = stdio_cli
        .request_verbatim(
            &json!({"jsonrpc": "2.0", "id": "s-res", "method": "tasks/result", "params": {"taskId": s_tid, "_meta": tasks_meta("s-res")}}),
            Duration::from_secs(10),
        )
        .unwrap();
    stdio_cli
        .send(&json!({"jsonrpc": "2.0", "method": "tasks/get", "params": {"taskId": "tid-x"}}));
    let s_noid = stdio_cli
        .response_for(&json!(null), Duration::from_secs(10))
        .unwrap();
    drop(stdio_cli);

    let tr_stdio = SurfaceTranscript {
        discover_has_tasks: s_disc["result"]["capabilities"]["extensions"][TASKS_EXTENSION_ID]
            .is_object(),
        policy_deny_sync: s_pol["result"]["resultType"] == "complete"
            && s_pol["result"]["isError"] == true,
        task_spawn_type: s_sp["result"]["resultType"].as_str().unwrap_or("").into(),
        settled_status: s_settled_status,
        settled_verified: s_settled_verified,
        missing_cap_code: s_nocap["error"]["code"].as_i64().unwrap_or(0),
        unknown_task_code: s_unk["error"]["code"].as_i64().unwrap_or(0),
        update_terminal_code: s_updt["error"]["code"].as_i64().unwrap_or(0),
        cancel_terminal_code: s_cant["error"]["code"].as_i64().unwrap_or(0),
        tasks_list_code: s_list["error"]["code"].as_i64().unwrap_or(0),
        tasks_result_code: s_res["error"]["code"].as_i64().unwrap_or(0),
        no_id_code: s_noid["error"]["code"].as_i64().unwrap_or(0),
    };
    assert_eq!(
        tr_stdio, tr_direct,
        "susi mcp serve stdio must match direct GmcpService"
    );

    // Surface E: Live Streamable HTTP `/mcp` gateway (differential check: identical JSON-RPC semantics
    // when HTTP routing headers match, plus gateway-specific `400` / `-32020` on `Mcp-Name` mismatch).
    let http_rpc = |method: &str, name_hdr: Option<&str>, body: &Value| -> (u16, Value) {
        let body_str = body.to_string();
        let mut s = TcpStream::connect(&http_addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {http_addr}\r\nConnection: close\r\nAuthorization: {auth_header}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: {PROTOCOL_VERSION_2026_07_28}\r\nMcp-Method: {method}\r\n"
        );
        if let Some(nh) = name_hdr {
            req.push_str(&format!("Mcp-Name: {nh}\r\n"));
        }
        req.push_str(&format!(
            "Content-Length: {}\r\n\r\n{body_str}",
            body_str.len()
        ));
        s.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        let _ = s.read_to_string(&mut raw);
        let status = raw
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse::<u16>().ok())
            .unwrap_or(0);
        let body_part = raw
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.trim())
            .unwrap_or("");
        let parsed = if let Ok(v) = serde_json::from_str::<Value>(body_part) {
            v
        } else {
            body_part
                .lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim))
                .filter_map(|d| serde_json::from_str::<Value>(d).ok())
                .find(|v| v.get("result").is_some() || v.get("error").is_some())
                .unwrap_or_else(|| json!({"raw": raw}))
        };
        (status, parsed)
    };

    let (_, h_disc) = http_rpc(
        "server/discover",
        None,
        &json!({"jsonrpc": "2.0", "id": "h-disc", "method": "server/discover", "params": {"_meta": tasks_meta("h-disc")}}),
    );
    let (_, h_pol) = http_rpc(
        "tools/call",
        Some("susi_restricted_probe"),
        &json!({"jsonrpc": "2.0", "id": "h-pol", "method": "tools/call", "params": {"name": "susi_restricted_probe", "arguments": {}, "_meta": tasks_meta("h-pol")}}),
    );
    let (_, h_sp) = http_rpc(
        "tools/call",
        Some("ledger_verify"),
        &json!({"jsonrpc": "2.0", "id": "h-sp", "method": "tools/call", "params": {"name": "ledger_verify", "arguments": {}, "_meta": tasks_meta("h-sp")}}),
    );
    let h_tid = h_sp["result"]["taskId"].as_str().unwrap().to_string();
    let mut h_settled_status = String::new();
    let mut h_settled_verified = false;
    for i in 0..25 {
        let gid = format!("h-get-{i}");
        let (_, g) = http_rpc(
            "tasks/get",
            Some(&h_tid),
            &json!({"jsonrpc": "2.0", "id": gid, "method": "tasks/get", "params": {"taskId": h_tid, "_meta": tasks_meta(&gid)}}),
        );
        if g["result"]["status"] == "completed" {
            h_settled_status = "completed".into();
            h_settled_verified = call_tool_report(&g["result"]["result"])["verified"] == true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_, h_nocap) = http_rpc(
        "tasks/get",
        Some(&h_tid),
        &json!({"jsonrpc": "2.0", "id": "h-nocap", "method": "tasks/get", "params": {"taskId": h_tid, "_meta": plain_modern_meta("h-nocap")}}),
    );
    let unk_uuid = "00000000-0000-4000-8000-000000000099";
    let (_, h_unk) = http_rpc(
        "tasks/get",
        Some(unk_uuid),
        &json!({"jsonrpc": "2.0", "id": "h-unk", "method": "tasks/get", "params": {"taskId": unk_uuid, "_meta": tasks_meta("h-unk")}}),
    );
    let (_, h_updt) = http_rpc(
        "tasks/update",
        Some(&h_tid),
        &json!({"jsonrpc": "2.0", "id": "h-updt", "method": "tasks/update", "params": {"taskId": h_tid, "inputResponses": {}, "_meta": tasks_meta("h-updt")}}),
    );
    let (_, h_cant) = http_rpc(
        "tasks/cancel",
        Some(&h_tid),
        &json!({"jsonrpc": "2.0", "id": "h-cant", "method": "tasks/cancel", "params": {"taskId": h_tid, "_meta": tasks_meta("h-cant")}}),
    );
    let (_, h_list) = http_rpc(
        "tasks/list",
        None,
        &json!({"jsonrpc": "2.0", "id": "h-list", "method": "tasks/list", "params": {"_meta": tasks_meta("h-list")}}),
    );
    let (_, h_res) = http_rpc(
        "tasks/result",
        None,
        &json!({"jsonrpc": "2.0", "id": "h-res", "method": "tasks/result", "params": {"taskId": h_tid, "_meta": tasks_meta("h-res")}}),
    );
    let (h_noid_st, h_noid) = http_rpc(
        "tasks/get",
        Some(&h_tid),
        &json!({"jsonrpc": "2.0", "method": "tasks/get", "params": {"taskId": h_tid}}),
    );
    assert_eq!(h_noid_st, 400);

    let tr_http = SurfaceTranscript {
        discover_has_tasks: h_disc["result"]["capabilities"]["extensions"][TASKS_EXTENSION_ID]
            .is_object(),
        policy_deny_sync: h_pol["result"]["resultType"] == "complete"
            && h_pol["result"]["isError"] == true,
        task_spawn_type: h_sp["result"]["resultType"].as_str().unwrap_or("").into(),
        settled_status: h_settled_status,
        settled_verified: h_settled_verified,
        missing_cap_code: h_nocap["error"]["code"].as_i64().unwrap_or(0),
        unknown_task_code: h_unk["error"]["code"].as_i64().unwrap_or(0),
        update_terminal_code: h_updt["error"]["code"].as_i64().unwrap_or(0),
        cancel_terminal_code: h_cant["error"]["code"].as_i64().unwrap_or(0),
        tasks_list_code: h_list["error"]["code"].as_i64().unwrap_or(0),
        tasks_result_code: h_res["error"]["code"].as_i64().unwrap_or(0),
        no_id_code: h_noid["error"]["code"].as_i64().unwrap_or(0),
    };
    assert_eq!(
        tr_http, tr_direct,
        "HTTP /mcp gateway must match direct GmcpService on all shared task semantics"
    );

    // Documented direct-vs-gateway differential: HTTP gateway additionally enforces `Mcp-Name == params.taskId` (`400` / `-32020`).
    let (mismatch_st, mismatch_val) = http_rpc(
        "tasks/get",
        Some("wrong-header-id"),
        &json!({"jsonrpc": "2.0", "id": "h-mismatch", "method": "tasks/get", "params": {"taskId": h_tid, "_meta": tasks_meta("h-mismatch")}}),
    );
    assert_eq!(mismatch_st, 400);
    assert_eq!(mismatch_val["error"]["code"], -32020);
    assert!(McpGatewayProxy::evaluate_tool_policy("susi_restricted_probe").is_err());

    // ── 4. Deterministic Property Invariants & Retained Regression Fixtures ──
    runtime.block_on(async {
        let prop_dir = scratch.join("prop-tasks");
        let mgr = TaskManager::open(&prop_dir).unwrap();
        let fast_opts = TaskOptions {
            ttl_ms: Some(60_000),
            poll_interval_ms: Some(1),
            status_message: None,
        };
        let p_alice = TaskPrincipal::from_bearer_token("prop-alice");
        let p_bob = TaskPrincipal::from_bearer_token("prop-bob");
        let mut minted_ids = BTreeSet::new();
        let mut rng_state: u64 = 0x5EED_2030_1405_CAFE;
        let mut next_u64 = || -> u64 {
            rng_state ^= rng_state << 13;
            rng_state ^= rng_state >> 7;
            rng_state ^= rng_state << 17;
            rng_state
        };

        // Property P1 + P2 + P4: 36 seeded lifecycle runs across Completed, Failed, and Cancelled;
        // verify UUIDv4 uniqueness, principal partition, and terminal absorbing-state immutability.
        for step in 0..36usize {
            let mode = next_u64() % 3;
            let created = match mode {
                0 => mgr
                    .try_spawn_for(p_alice.clone(), fast_opts.clone(), move |_ctx| {
                        Box::pin(async move {
                            Ok(CallToolResult {
                                content: vec![ContentBlock::text(format!("ok-{step}"))],
                                structured_content: Some(json!({"step": step})),
                                is_error: Some(step % 2 == 1),
                                meta: None,
                                result_type: None,
                            })
                        })
                    })
                    .unwrap(),
                1 => mgr
                    .try_spawn_for(p_alice.clone(), fast_opts.clone(), move |_ctx| {
                        Box::pin(async move {
                            Err(TaskExit::Error(ErrorData::internal_error(
                                format!("fail-{step}"),
                                None,
                            )))
                        })
                    })
                    .unwrap(),
                _ => {
                    let t = mgr
                        .try_spawn_for(p_alice.clone(), fast_opts.clone(), |ctx| {
                            Box::pin(async move {
                                ctx.cancelled().await;
                                Err(TaskExit::Cancelled)
                            })
                        })
                        .unwrap();
                    mgr.cancel_task_for(&t.task_id, &p_alice).unwrap();
                    t
                }
            };
            assert!(is_uuid_v4(&created.task_id));
            assert!(
                minted_ids.insert(created.task_id.clone()),
                "duplicate UUIDv4 minted"
            );

            // Wait until task settles.
            for _ in 0..50 {
                if mgr.running_task_count() == 0 {
                    break;
                }
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let snap_0 = mgr.get_task_for(&created.task_id, &p_alice).unwrap();
            assert!(snap_0.task.status.is_terminal());
            let expected_status = match mode {
                0 => ServerTaskStatus::Completed,
                1 => ServerTaskStatus::Failed,
                _ => ServerTaskStatus::Cancelled,
            };
            assert_eq!(snap_0.task.status, expected_status);
            let frozen_json = serde_json::to_value(&snap_0).unwrap();

            // Arbitrary post-terminal operations by owner and foreign principal never mutate state or payload!
            assert_eq!(
                mgr.get_task_for(&created.task_id, &p_bob)
                    .unwrap_err()
                    .code
                    .0,
                -32602
            );
            for _ in 0..3 {
                assert_eq!(
                    mgr.cancel_task_for(&created.task_id, &p_alice)
                        .unwrap_err()
                        .code
                        .0,
                    -32602
                );
                assert_eq!(
                    mgr.update_task_for(&created.task_id, &p_alice, [("k".to_string(), json!(1))])
                        .unwrap_err()
                        .code
                        .0,
                    -32602
                );
                let after = mgr.get_task_for(&created.task_id, &p_alice).unwrap();
                assert_eq!(serde_json::to_value(&after).unwrap(), frozen_json);
            }
        }

        // Property P3: Partial `tasks/update` commutativity (`(k1, k2)` vs `(k2, k1)` produce identical output).
        let (mgr_ref, alice_ref, opts_ref) = (&mgr, &p_alice, &fast_opts);
        let run_order = |first_k1: bool| async move {
            let t = mgr_ref
                .try_spawn_for(alice_ref.clone(), opts_ref.clone(), |ctx| {
                    Box::pin(async move {
                        let r1: InputRequest =
                            serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                        let r2: InputRequest =
                            serde_json::from_value(json!({"method": "roots/list"})).unwrap();
                        let (v1, v2) = tokio::try_join!(
                            ctx.request_input("k1", r1),
                            ctx.request_input("k2", r2)
                        )?;
                        Ok(CallToolResult {
                            content: vec![ContentBlock::text("comm")],
                            structured_content: Some(json!({"v1": v1, "v2": v2})),
                            is_error: Some(false),
                            meta: None,
                            result_type: None,
                        })
                    })
                })
                .unwrap();
            for _ in 0..30 {
                let d = mgr_ref.get_task_for(&t.task_id, alice_ref).unwrap();
                let dj = serde_json::to_value(&d).unwrap();
                if d.task.status == ServerTaskStatus::InputRequired
                    && dj["inputRequests"]
                        .as_object()
                        .is_some_and(|m| m.len() == 2)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let seq = if first_k1 {
                [("k1", json!(10)), ("k2", json!(20))]
            } else {
                [("k2", json!(20)), ("k1", json!(10))]
            };
            for (k, v) in seq {
                mgr_ref
                    .update_task_for(&t.task_id, alice_ref, [(k.to_string(), v)])
                    .unwrap();
            }
            for _ in 0..30 {
                let d = mgr_ref.get_task_for(&t.task_id, alice_ref).unwrap();
                if d.task.status == ServerTaskStatus::Completed {
                    return serde_json::to_value(d.payload).unwrap();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            panic!("commutative MRTR task did not complete");
        };
        let out_12 = run_order(true).await;
        let out_21 = run_order(false).await;
        assert_eq!(out_12, out_21, "partial tasks/update order must commute");
    });

    // Retained Regression R-01: Legacy `params.task` without `_meta` tasks capability never spawns a task.
    let (mut leg_dup, leg_dup_sess) =
        spawn_service_duplex(&runtime, direct_svc.clone(), ADAPTER_LIMIT);
    let leg_aug = leg_dup.call(
        "reg-params-task",
        "tools/call",
        json!({ "name": "interop_echo", "arguments": {"msg": "no-force"}, "task": {"ttl": 5000}, "_meta": plain_modern_meta("reg-params-task")}),
    );
    assert_eq!(
        leg_aug["result"]["resultType"], "complete",
        "client params.task must never force CreateTaskResult without per-request tasks capability: {leg_aug}"
    );
    drop(leg_dup);
    adapter_session_ended(&runtime, leg_dup_sess, "014-interop reg-params-task");

    // Retained Regression R-02: PersistedTaskHandleStore fail-closed corruption & duplicate-ID guards.
    let bad_store = scratch.join("corrupt-handles.json");
    std::fs::write(&bad_store, b"{\"version\": 99, \"handles\": []}").unwrap();
    assert!(matches!(
        PersistedTaskHandleStore::open(&bad_store),
        Err(WireError::Protocol(_))
    ));
    let dup_entry = json!({ "version": 1, "handles": [{"addr": "127.0.0.1:9", "taskId": "dup-1", "protocolVersion": "2026-07-28", "status": "working", "createdAt": "t0", "lastUpdatedAt": "t0"}, {"addr": "127.0.0.1:9", "taskId": "dup-1", "protocolVersion": "2026-07-28", "status": "completed", "createdAt": "t0", "lastUpdatedAt": "t1"}]});
    std::fs::write(&bad_store, dup_entry.to_string()).unwrap();
    assert!(matches!(
        PersistedTaskHandleStore::open(&bad_store),
        Err(WireError::Protocol(_))
    ));

    // ── 5. Published Latency, Throughput, Startup, Fairness, Memory & Disk Budgets ──
    runtime.block_on(async {
        let slo_dir = scratch.join("slo-durable-tasks");
        let slo_mgr = TaskManager::open(&slo_dir).unwrap();
        let slo_owner = TaskPrincipal::local();
        let slo_opts = TaskOptions {
            ttl_ms: Some(300_000),
            poll_interval_ms: Some(1),
            status_message: Some("slo-probe".into()),
        };

        // 5a. Synchronous durable creation write before CreateTaskResult (zero polling race).
        let create_t0 = Instant::now();
        let created = slo_mgr
            .try_spawn_for(slo_owner.clone(), slo_opts.clone(), |ctx| {
                Box::pin(async move {
                    ctx.cancelled().await;
                    Err(TaskExit::Cancelled)
                })
            })
            .unwrap();
        let create_ms = create_t0.elapsed().as_millis();
        within("request_ms", create_ms);
        assert!(
            slo_dir.join("tasks.json").exists(),
            "tasks.json must exist synchronously before try_spawn_for returns"
        );
        let immediate = slo_mgr.get_task_for(&created.task_id, &slo_owner).unwrap();
        assert_eq!(immediate.task.status, ServerTaskStatus::Working);

        // 5b. Cancel propagation SLO (`< 100 ms` contract target).
        let mut ev_rx = slo_mgr.subscribe_events();
        let cancel_t0 = Instant::now();
        slo_mgr
            .cancel_task_for(&created.task_id, &slo_owner)
            .unwrap();
        loop {
            let (_, dt) = tokio::time::timeout(Duration::from_millis(500), ev_rx.recv())
                .await
                .unwrap()
                .unwrap();
            if dt.task.task_id == created.task_id && dt.task.status == ServerTaskStatus::Cancelled {
                break;
            }
        }
        let cancel_ms = cancel_t0.elapsed().as_millis();
        assert!(
            cancel_ms < 100,
            "cancel propagation SLO < 100 ms violated: measured {cancel_ms} ms"
        );

        // 5c. 2,048-record durable store startup reload & crash reconciliation SLO (`<= 2,000 ms`).
        let seed_dir = scratch.join("seed-2048");
        let seed_mgr = TaskManager::open(&seed_dir).unwrap();
        let done_seed = seed_mgr
            .try_spawn_for(slo_owner.clone(), slo_opts.clone(), |_ctx| {
                Box::pin(async move {
                    Ok(CallToolResult {
                        content: vec![ContentBlock::text("seed-done")],
                        structured_content: None,
                        is_error: Some(false),
                        meta: None,
                        result_type: None,
                    })
                })
            })
            .unwrap();
        for _ in 0..30 {
            if seed_mgr.running_task_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let live_seed = seed_mgr
            .try_spawn_for(
                slo_owner.clone(),
                TaskOptions {
                    status_message: None,
                    ..slo_opts.clone()
                },
                |ctx| {
                    Box::pin(async move {
                        ctx.cancelled().await;
                        Err(TaskExit::Cancelled)
                    })
                },
            )
            .unwrap();
        let seed_json: Value =
            serde_json::from_slice(&std::fs::read(seed_dir.join("tasks.json")).unwrap()).unwrap();
        let seed_items = seed_json[1].as_array().unwrap();
        let completed_tpl = seed_items
            .iter()
            .find(|it| it[1]["taskId"] == done_seed.task_id)
            .unwrap()
            .clone();
        let working_tpl = seed_items
            .iter()
            .find(|it| it[1]["taskId"] == live_seed.task_id)
            .unwrap()
            .clone();
        let _ = seed_mgr.cancel_task_for(&live_seed.task_id, &slo_owner);

        let mut bulk_items = Vec::with_capacity(MAX_RETAINED_TERMINAL_TASKS);
        for i in 0..2000usize {
            let mut item = completed_tpl.clone();
            item[1]["taskId"] = json!(format!("10000000-0000-4000-8000-{i:012x}"));
            bulk_items.push(item);
        }
        for i in 0..48usize {
            let mut item = working_tpl.clone();
            item[1]["taskId"] = json!(format!("20000000-0000-4000-8000-{i:012x}"));
            bulk_items.push(item);
        }
        let reload_dir = scratch.join("reload-2048");
        std::fs::create_dir_all(&reload_dir).unwrap();
        std::fs::write(
            reload_dir.join("tasks.json"),
            serde_json::to_vec(&json!([1, bulk_items])).unwrap(),
        )
        .unwrap();

        let reload_t0 = Instant::now();
        let reloaded_mgr = TaskManager::open(&reload_dir).unwrap();
        let reload_ms = reload_t0.elapsed().as_millis();
        assert!(
            reload_ms <= 2_000,
            "2,048-record durable store startup reload SLO <= 2,000 ms violated: {reload_ms} ms"
        );
        let rep_done = reloaded_mgr
            .get_task_for("10000000-0000-4000-8000-0000000007cf", &slo_owner)
            .unwrap();
        assert_eq!(rep_done.task.status, ServerTaskStatus::Completed);
        let rep_reconciled = reloaded_mgr
            .get_task_for("20000000-0000-4000-8000-00000000002f", &slo_owner)
            .unwrap();
        assert_eq!(rep_reconciled.task.status, ServerTaskStatus::Failed);
        assert!(rep_reconciled
            .task
            .status_message
            .as_deref()
            .unwrap_or("")
            .contains("interrupted by server restart"));

        // 5d. 4-principal concurrent TaskManager fairness (4 * 16 = 64 concurrent tasks, zero starvation or leakage).
        let fair_mgr = TaskManager::new();
        let mut fair_joins = Vec::new();
        for p_idx in 0..4usize {
            let m = fair_mgr.clone();
            let owner = TaskPrincipal::from_bearer_token(&format!("fair-principal-{p_idx}"));
            let foreign =
                TaskPrincipal::from_bearer_token(&format!("fair-principal-{}", (p_idx + 1) % 4));
            let opts = TaskOptions {
                ttl_ms: Some(60_000),
                poll_interval_ms: Some(1),
                status_message: None,
            };
            fair_joins.push(tokio::spawn(async move {
                let mut ids = Vec::with_capacity(16);
                for t_idx in 0..16usize {
                    let t = m
                        .try_spawn_for(owner.clone(), opts.clone(), move |_ctx| {
                            Box::pin(async move {
                                tokio::task::yield_now().await;
                                Ok(CallToolResult {
                                    content: vec![ContentBlock::text(format!("p{p_idx}-t{t_idx}"))],
                                    structured_content: None,
                                    is_error: Some(false),
                                    meta: None,
                                    result_type: None,
                                })
                            })
                        })
                        .unwrap();
                    ids.push(t.task_id);
                }
                for id in &ids {
                    assert!(m.get_task_for(id, &foreign).is_err());
                    for _ in 0..50 {
                        let snap = m.get_task_for(id, &owner).unwrap();
                        if snap.task.status == ServerTaskStatus::Completed {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    assert_eq!(
                        m.get_task_for(id, &owner).unwrap().task.status,
                        ServerTaskStatus::Completed
                    );
                }
            }));
        }
        for j in fair_joins {
            j.await.unwrap();
        }
        assert_eq!(fair_mgr.running_task_count(), 0);
    });

    // 5e. 4-client concurrent HTTP task pipeline throughput & fairness (`pipeline_ms` & `fairness_ms`).
    let pipe_t0 = Instant::now();
    let max_client_ms = Arc::new(AtomicU64::new(0));
    let mut http_threads = Vec::new();
    for idx in 0..4u32 {
        let addr_c = http_addr.clone();
        let max_c = max_client_ms.clone();
        http_threads.push(std::thread::spawn(move || {
            let t0 = Instant::now();
            let mut cli =
                Session::open_2026_07_28(&addr_c, Some(token), Duration::from_secs(15)).unwrap();
            for step in 0..3u32 {
                let tag = format!("fair-{idx}-{step}");
                let out = cli
                    .call_tool_resolving_tasks(
                        "interop_echo",
                        &json!({"msg": tag}),
                        Some(Duration::from_secs(10)),
                        |_| Ok(json!({})),
                    )
                    .unwrap();
                assert_eq!(out["content"][0]["text"], format!("interop:{tag}"));
            }
            cli.close().unwrap();
            let elapsed = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
            max_c.fetch_max(elapsed, Ordering::AcqRel);
        }));
    }
    for th in http_threads {
        th.join()
            .expect("concurrent HTTP task fairness thread must succeed");
    }
    within("pipeline_ms", pipe_t0.elapsed().as_millis());
    within(
        "fairness_ms",
        u128::from(max_client_ms.load(Ordering::Acquire)),
    );
    assert!(interop_echo_runs.load(Ordering::SeqCst) >= 12);

    drop(http_mod_cli);
    if let (Some(before), Some(after)) = (resident_before, adapter_resident_bytes()) {
        within(
            "memory_growth_mib",
            u128::from(after.saturating_sub(before) / (1024 * 1024)),
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_file(cfg_dir.join("api_token"));
    let _ = std::fs::remove_dir_all(&home);
    within(
        "disk_growth_kib",
        u128::from(adapter_tree_bytes(&home).saturating_sub(baseline_disk) / 1024),
    );
}
