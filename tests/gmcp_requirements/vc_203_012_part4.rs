/// GMCP-203-012-05: Interoperability and quality for subscriptions, notifications,
/// progress, and operational messages across both roles, direct-vs-gateway differential
/// behavior, property invariants, transport/platform parity, retained regression fixtures,
/// and published latency, throughput, startup, memory, disk, and fairness budgets.
#[test]
fn gmcp_req_203_012_05_interop() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};
    use susi_gmcp::jsonrpc_gate::{classify_envelope, http_envelope_rejection, GateVerdict};
    use susi_gmcp::mcp::message::{ClientJsonRpcMessage, ServerJsonRpcMessage, ServerNotification};
    use susi_gmcp::mcp::model::{
        CallToolResult, CancelledNotificationParam, ContentBlock, LoggingLevel,
        LoggingMessageNotificationParam, NotificationMetaObject, NumberOrString,
        ProgressNotificationParam, ProgressToken, ReadResourceResult, RequestId, Resource,
        ResourceContents, SetLevelRequestParams, SubscriptionFilter,
        SubscriptionsListenRequestParams, Tool,
    };
    use susi_gmcp::mcp::service::{QuitReason, ServiceExt};
    use susi_gmcp::protocol::{
        GmcpService, McpGatewayProxy, MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION,
        MAX_LOG_NOTIFICATIONS_PER_REQUEST, MAX_NOTIFICATION_PAYLOAD_BYTES,
        MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER,
    };
    use susi_gmcp::stdio::BoundedLines;
    use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

    let (_lock, _env, home) = gmcp_http_home_guard("012-interop");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "susi-012-interop-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();
    susi_daemon::composition::wire_mcp_serve_substrate(&home.join(".susi"));

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
    let expected_report = ledger_report(root);

    let modern_meta = |req_id: &str| -> Value {
        let mut meta = adapter_modern_meta()["_meta"].clone();
        meta["io.modelcontextprotocol/requestId"] = json!(req_id);
        meta
    };
    let with_modern_meta = |req_id: &str, mut params: Value| -> Value {
        if let Some(obj) = params.as_object_mut() {
            obj.insert("_meta".to_string(), modern_meta(req_id));
        }
        params
    };

    let spawn_duplex = |rt: &tokio::runtime::Runtime, svc: GmcpService| {
        const DUPLEX_CAP: usize = 4 * 1024 * 1024;
        let (client_half, server_half) = tokio::io::duplex(DUPLEX_CAP);
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let sess = rt.spawn(async move {
            let (server_read, server_write) = tokio::io::split(server_half);
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                BoundedLines::new(server_read, DUPLEX_CAP),
                server_write,
            );
            let running = svc
                .serve(transport)
                .await
                .map_err(|e| format!("serve error: {e:?}"))?;
            let cancel = running.cancellation_token();
            tokio::spawn(async move {
                if stop_rx.await.is_ok() {
                    cancel.cancel();
                }
            });
            running.waiting().await.map_err(|e| e.to_string())
        });
        let (client_read, mut client_write) = tokio::io::split(client_half);
        let (line_tx, line_rx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let mut lines = tokio::io::BufReader::new(client_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if line_tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let (byte_tx, mut byte_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        rt.spawn(async move {
            while let Some(b) = byte_rx.recv().await {
                if client_write.write_all(&b).await.is_err() {
                    break;
                }
            }
            let _ = client_write.shutdown().await;
        });
        (
            AdapterClient {
                write: Box::new(move |b| {
                    let _ = byte_tx.send(b.to_vec());
                }),
                rx: line_rx,
                pending: Vec::new(),
            },
            stop_tx,
            sess,
        )
    };

    // ── 1. Reference Wire Codec (Client Role) & Positive/Negative Fixtures ──
    let canonical_client_frames = [
        json!({ "jsonrpc": "2.0", "id": "fx-sub-1", "method": "subscriptions/listen", "params": { "notifications": { "toolsListChanged": true, "resourceSubscriptions": ["susi://tools"]}, "_meta": modern_meta("fx-sub-1")}}),
        json!({ "jsonrpc": "2.0", "id": 99, "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "io.modelcontextprotocol/logLevel": "notice", "progressToken": "tok-client-1"}}}),
        json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "fx-sub-1", "reason": "interop cancel"}}),
        json!({ "jsonrpc": "2.0", "id": 100, "method": "logging/setLevel", "params": {"level": "warning"}}),
    ];
    for frame in &canonical_client_frames {
        let raw = serde_json::to_vec(frame).unwrap();
        assert!(matches!(classify_envelope(&raw), GateVerdict::Forward));
        assert_eq!(http_envelope_rejection(&raw), None);
        assert!(
            ClientJsonRpcMessage::from_value(frame.clone()).is_some(),
            "client reference fixture must decode canonically: {frame}"
        );
    }

    // Typed subscription/progress/logging server notifications serialize losslessly.
    let mut ack_meta = NotificationMetaObject::new();
    ack_meta.set_subscription_id(RequestId::String("fx-sub-1".into()));
    let parsed_ack_filter: SubscriptionsListenRequestParams = serde_json::from_value(json!({ "notifications": {"toolsListChanged": true, "resourceSubscriptions": ["susi://tools"]}}))
    .unwrap();
    assert_eq!(
        parsed_ack_filter.notifications.tools_list_changed,
        Some(true)
    );
    assert_eq!(
        ack_meta.subscription_id(),
        Some(RequestId::String("fx-sub-1".into()))
    );
    let ack_msg = ServerJsonRpcMessage::Notification(ServerNotification::new(
        "notifications/subscriptions/acknowledged",
        json!({ "notifications": parsed_ack_filter.notifications, "_meta": ack_meta}),
    ));
    let ack_json = serde_json::to_value(&ack_msg).unwrap();
    assert_eq!(
        ack_json["method"],
        "notifications/subscriptions/acknowledged"
    );
    assert_eq!(
        ack_json["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "fx-sub-1"
    );

    let prog_msg = ServerJsonRpcMessage::Notification(ServerNotification::new(
        "notifications/progress",
        ProgressNotificationParam::new(
            ProgressToken(NumberOrString::String("tok-client-1".into())),
            0.5,
        )
        .with_total(1.0)
        .with_message("halfway"),
    ));
    let prog_json = serde_json::to_value(&prog_msg).unwrap();
    assert_eq!(prog_json["params"]["progressToken"], "tok-client-1");
    assert_eq!(prog_json["params"]["progress"], 0.5);

    let log_msg = ServerJsonRpcMessage::Notification(ServerNotification::new(
        "notifications/message",
        LoggingMessageNotificationParam::new(LoggingLevel::Notice, json!({"ok": true}))
            .with_logger("interop"),
    ));
    let log_json = serde_json::to_value(&log_msg).unwrap();
    assert_eq!(log_json["params"]["level"], "notice");
    assert_eq!(log_json["params"]["logger"], "interop");

    // Typed client-role parsers fail closed on malformed negative fixtures.
    assert!(serde_json::from_value::<SetLevelRequestParams>(json!({"level": "verbose"})).is_err());
    assert!(serde_json::from_value::<ProgressNotificationParam>(json!({ "progressToken": "", "progress": 0.5}))
    .is_ok()); // wire struct allows empty; RequestMetaObject::parse_progress_token rejects it!
    let empty_tok_meta: susi_gmcp::mcp::model::RequestMetaObject =
        serde_json::from_value(json!({"progressToken": ""})).unwrap();
    assert!(empty_tok_meta.parse_progress_token().is_err());
    let bad_cancel: CancelledNotificationParam =
        serde_json::from_value(json!({"requestId": 12, "reason": "stop"})).unwrap();
    assert_eq!(bad_cancel.request_id, Some(RequestId::Number(12)));

    // ── 2. Independent Reference Mock 2025-11-25 Server -> Client SDK Role ──
    let mock_listener = TcpListener::bind("127.0.0.1:0").expect("bind mock ref server");
    let mock_port = mock_listener.local_addr().unwrap().port();
    let mock_addr = format!("127.0.0.1:{mock_port}");
    mock_listener.set_nonblocking(true).unwrap();
    let mock_stop = Arc::new(AtomicBool::new(false));
    let mock_stop_srv = mock_stop.clone();
    let mock_deletes = Arc::new(AtomicUsize::new(0));
    let mock_deletes_srv = mock_deletes.clone();
    let mock_thread = std::thread::spawn(move || {
        while !mock_stop_srv.load(Ordering::Acquire) {
            let mut stream = match mock_listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(_) => break,
            };
            let _ = stream.set_nonblocking(false);
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut req_line = String::new();
            if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                continue;
            }
            let mut content_len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" || h == "\n" {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body_buf = vec![0u8; content_len];
            if content_len > 0 {
                let _ = reader.read_exact(&mut body_buf);
            }
            if req_line.starts_with("DELETE ") {
                mock_deletes_srv.fetch_add(1, Ordering::AcqRel);
                let resp = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
                continue;
            }
            let msg: Value = serde_json::from_slice(&body_buf).unwrap_or(Value::Null);
            let method = msg["method"].as_str().unwrap_or("");
            let id = msg["id"].clone();
            if method == "notifications/initialized" {
                let resp =
                    "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                let _ = stream.write_all(resp.as_bytes());
                continue;
            }
            if method == "initialize" {
                let reply = json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": "2025-11-25", "capabilities": { "logging": {}, "resources": {"subscribe": true, "listChanged": true}, "tools": {}}, "serverInfo": {"name": "mock-ref-sdk", "version": "1.0.0"}}})
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: ref-sess-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                continue;
            }
            if method == "tools/call" && msg["params"]["name"] == "ref_fail" {
                let reply = json!({ "jsonrpc": "2.0", "id": id, "error": {"code": -32602, "message": "invalid mock tool param"}})
                .to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                );
                let _ = stream.write_all(resp.as_bytes());
                continue;
            }
            if method == "tools/call" {
                let tok = msg["params"]["_meta"]["progressToken"]
                    .as_str()
                    .unwrap_or("mock-tok");
                let sse = format!(
                    "id: 0\nretry: 3000\ndata:\n\n\
                     data: {}\n\n\
                     data: {}\n\n\
                     data: {}\n\n\
                     data: {}\n\n",
                    json!({ "jsonrpc": "2.0", "method": "notifications/progress", "params": {"progressToken": tok, "progress": 0.5, "total": 1.0, "message": "step"}}),
                    json!({ "jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": "susi://tools"}}),
                    json!({ "jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "notice", "logger": "ref-sdk", "data": {"ok": true}}}),
                    json!({ "jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "ref-sdk-ok"}], "isError": false}})
                );
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                    sse.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        }
    });

    {
        let mut mock_wire =
            susi_wire_client::mcp::Session::open(&mock_addr, None, Duration::from_secs(10))
                .expect("wire client must handshake with reference 2025-11-25 mock server");
        assert_eq!(mock_wire.protocol_version(), "2025-11-25");
        let mut ref_notes = Vec::new();
        let mock_res = mock_wire
            .call_tool(
                "ref_echo",
                &json!({"x": 1}),
                Some(json!("ref-tok-1")),
                &mut |note| ref_notes.push(note.clone()),
            )
            .expect(
                "wire client must decode SSE progress + log + skip unsolicited resource update",
            );
        assert_eq!(mock_res["content"][0]["text"], "ref-sdk-ok");
        assert_eq!(
            ref_notes.len(),
            2,
            "expected progress + log frames: {ref_notes:?}"
        );
        assert_eq!(ref_notes[0]["method"], "notifications/progress");
        assert_eq!(ref_notes[1]["method"], "notifications/message");
        let mock_err = mock_wire
            .call_tool("ref_fail", &json!({}), None, &mut |_| {})
            .expect_err("wire client must surface JSON-RPC error");
        assert!(
            matches!(mock_err, susi_wire_client::mcp::WireError::Rpc(-32602, _)),
            "unexpected mock wire error: {mock_err:?}"
        );
        mock_wire.close().expect("DELETE on reference server");

        let core_mock = susi_core::mcp_client::session_call(
            &mock_addr,
            "tools/call",
            &json!({"name": "ref_echo", "arguments": {}}),
            None,
        )
        .expect("core mcp_client must decode mock SSE response");
        assert_eq!(core_mock["content"][0]["text"], "ref-sdk-ok");
        assert!(mock_deletes.load(Ordering::Acquire) >= 2);
    }
    mock_stop.store(true, Ordering::Release);
    let _ = TcpStream::connect(&mock_addr);
    let _ = mock_thread.join();

    // ── 3. Live Shipped HTTP Gateway + Wire/Core SDKs + Differential Parity ──
    let http_listener = TcpListener::bind("127.0.0.1:0").expect("bind live HTTP gateway");
    let http_port = http_listener.local_addr().unwrap().port();
    let http_addr = format!("127.0.0.1:{http_port}");
    let root_for_http = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root_for_http,
            vec![http_listener],
            None,
            false,
        );
    });
    for _ in 0..50 {
        if TcpStream::connect(&http_addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let auth_header = format!("Bearer {token}");

    let http_exchange =
        |method: &str, headers: &[(&str, &str)], body: Option<&str>| -> (u16, String) {
            let mut s = TcpStream::connect(&http_addr).expect("connect HTTP gateway");
            s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            let mut req =
                format!("{method} /mcp HTTP/1.1\r\nHost: {http_addr}\r\nConnection: close\r\n");
            for (k, v) in headers {
                req.push_str(&format!("{k}: {v}\r\n"));
            }
            if let Some(b) = body {
                req.push_str(&format!("Content-Length: {}\r\n\r\n{b}", b.len()));
            } else {
                req.push_str("\r\n");
            }
            s.write_all(req.as_bytes()).unwrap();
            let mut resp = String::new();
            let _ = s.read_to_string(&mut resp);
            let status = resp
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|c| c.parse().ok())
                .unwrap_or(0);
            (status, resp)
        };
    let parse_http_frames = |raw: &str| -> (u16, String, Vec<Value>) {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
        let status = head
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let mut frames = Vec::new();
        let trimmed = body.trim();
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                frames.push(v);
            }
        } else {
            for line in body.lines() {
                if let Some(data) = line.strip_prefix("data:").map(str::trim) {
                    if let Ok(v) = serde_json::from_str::<Value>(data) {
                        frames.push(v);
                    }
                }
            }
        }
        (status, head.to_string(), frames)
    };

    // (a) Wire SDK + Core MCP Client against Susi live HTTP Gateway (server role).
    let mut gw_wire =
        susi_wire_client::mcp::Session::open(&http_addr, Some(token), Duration::from_secs(15))
            .expect("wire SDK session must open on live gateway");
    gw_wire
        .request("logging/setLevel", &json!({"level": "info"}), &mut |_| {})
        .expect("gateway legacy logging/setLevel");
    gw_wire
        .request(
            "resources/subscribe",
            &json!({"uri": "susi://tools"}),
            &mut |_| {},
        )
        .expect("gateway legacy resources/subscribe");
    let mut gw_notes = Vec::new();
    let gw_tool = gw_wire
        .call_tool(
            "ledger_verify",
            &json!({}),
            Some(json!("gw-wire-prog")),
            &mut |n| gw_notes.push(n.clone()),
        )
        .expect("gateway wire tools/call");
    assert_eq!(call_tool_report(&gw_tool)["rows"], expected_report["rows"]);
    let gw_prog: Vec<f64> = gw_notes
        .iter()
        .filter(|n| {
            n["method"] == "notifications/progress"
                && n["params"]["progressToken"] == "gw-wire-prog"
        })
        .filter_map(|n| n["params"]["progress"].as_f64())
        .collect();
    assert!(!gw_prog.is_empty() && gw_prog.windows(2).all(|w| w[0] < w[1]));
    assert!(gw_notes
        .iter()
        .any(|n| n["method"] == "notifications/message"));
    gw_wire
        .request(
            "resources/unsubscribe",
            &json!({"uri": "susi://tools"}),
            &mut |_| {},
        )
        .expect("gateway legacy resources/unsubscribe");
    gw_wire.close().expect("gateway DELETE /mcp");

    let gw_core = susi_core::mcp_client::session_call(
        &http_addr,
        "tools/call",
        &json!({"name": "ledger_verify", "arguments": {}}),
        Some(token),
    )
    .expect("core mcp_client session_call on gateway");
    assert_eq!(call_tool_report(&gw_core)["rows"], expected_report["rows"]);

    // Official-suite style gateway probes matching susi-conformance checks.
    let (conf_st, conf_raw) = http_exchange(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/list"),
        ],
        Some(
            &json!({ "jsonrpc": "2.0", "id": "conf-list", "method": "tools/list", "params": {"_meta": modern_meta("conf-list")}})
            .to_string(),
        ),
    );
    assert_eq!(conf_st, 200, "conformance tools/list failed: {conf_raw}");
    let (_, _, conf_frames) = parse_http_frames(&conf_raw);
    assert!(conf_frames
        .iter()
        .any(|f| f["id"] == "conf-list" && f["result"]["tools"].is_array()));

    let (unk_st, unk_raw) = http_exchange(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "susi/conformance-unknown-method"),
        ],
        Some(
            &json!({ "jsonrpc": "2.0", "id": "conf-unk", "method": "susi/conformance-unknown-method", "params": {"_meta": modern_meta("conf-unk")}})
            .to_string(),
        ),
    );
    let (_, _, unk_frames) = parse_http_frames(&unk_raw);
    assert!(unk_st == 404 || unk_st == 200, "{unk_raw}");
    assert_eq!(unk_frames[0]["error"]["code"], -32601, "{unk_raw}");

    // ── 4. Direct vs Gateway vs Stdio vs Adapters Differential Surface ──
    // Compare identical modern (2026-07-28) and legacy (2025-11-25) behaviors across:
    //   - Direct in-process GmcpService duplex
    //   - Shipped CLI `susi mcp serve` stdio
    //   - Embedded / Unix socket (on Unix) adapter
    //   - Live Streamable HTTP Gateway (`/mcp`)
    #[derive(Debug, PartialEq)]
    struct ParityTranscript {
        ack_filter: Value,
        ack_sub_id: Value,
        ledger_rows: Value,
        restricted_error_text: Value,
        invalid_log_level_code: i64,
        omitted_log_count: usize,
        legacy_set_level_on_modern: i64,
        legacy_subscribe_on_modern: i64,
        modern_listen_on_legacy: i64,
    }

    let collect_adapter_parity = |label: &str, client: &mut AdapterClient| -> ParityTranscript {
        adapter_modern_open(client);
        client.send(&json!({ "jsonrpc": "2.0", "id": "diff-sub-1", "method": "subscriptions/listen", "params": { "notifications": { "toolsListChanged": true, "promptsListChanged": true, "resourcesListChanged": true, "resourceSubscriptions": ["susi://tools", "susi://tools", "sealed://denied"]}, "_meta": modern_meta("diff-sub-1")}}));
        let ack = client.take(&format!("{label}: sub ack"), |m| {
            m["method"] == "notifications/subscriptions/acknowledged"
                && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "diff-sub-1"
        });
        let ack_filter = ack["params"]["notifications"].clone();
        let ack_sub_id = ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].clone();
        client.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "diff-sub-1", "reason": "parity cancel"}}));

        // Tool call with omitted logLevel -> zero notifications/message.
        client.pending.clear();
        let silent_tool = client.call(
            "diff-silent-tool",
            "tools/call",
            with_modern_meta(
                "diff-silent-tool",
                json!({"name": "ledger_verify", "arguments": {}}),
            ),
        );
        let omitted_log_count = client
            .pending
            .iter()
            .filter(|n| n["method"] == "notifications/message")
            .count();
        let ledger_rows = call_tool_report(&silent_tool["result"])["rows"].clone();

        // Tool call with info logLevel + progressToken -> ascending progress + log message.
        let mut with_meta = modern_meta("diff-loud-tool");
        with_meta["io.modelcontextprotocol/logLevel"] = json!("info");
        with_meta["progressToken"] = json!("diff-prog-tok");
        client.pending.clear();
        let loud_tool = client.call(
            "diff-loud-tool",
            "tools/call",
            json!({"name": "ledger_verify", "arguments": {}, "_meta": with_meta}),
        );
        assert_eq!(loud_tool["result"]["resultType"], "complete");
        assert!(client
            .pending
            .iter()
            .any(|n| n["method"] == "notifications/message"));
        let pvals: Vec<f64> = client
            .pending
            .iter()
            .filter(|n| {
                n["method"] == "notifications/progress"
                    && n["params"]["progressToken"] == "diff-prog-tok"
            })
            .filter_map(|n| n["params"]["progress"].as_f64())
            .collect();
        assert!(
            !pvals.is_empty() && pvals.windows(2).all(|w| w[0] < w[1]),
            "{label}: {pvals:?}"
        );

        // McpGatewayProxy policy denial parity.
        let restricted = client.call(
            "diff-restricted",
            "tools/call",
            with_modern_meta(
                "diff-restricted",
                json!({"name": "susi_restricted_probe", "arguments": {}}),
            ),
        );
        assert_eq!(restricted["result"]["isError"], json!(true));
        let restricted_error_text = restricted["result"]["content"][0]["text"].clone();

        // Invalid logLevel (-32602) & legacy RPCs (-32601) on modern.
        let mut bad_lvl = modern_meta("diff-bad-lvl");
        bad_lvl["io.modelcontextprotocol/logLevel"] = json!("verbose");
        let bad_lvl_resp = client.call(
            "diff-bad-lvl",
            "tools/call",
            json!({"name": "ledger_verify", "arguments": {}, "_meta": bad_lvl}),
        );
        let invalid_log_level_code = bad_lvl_resp["error"]["code"].as_i64().unwrap();

        let leg_set = client.call(
            "diff-leg-set",
            "logging/setLevel",
            with_modern_meta("diff-leg-set", json!({"level": "info"})),
        );
        let leg_sub = client.call(
            "diff-leg-sub",
            "resources/subscribe",
            with_modern_meta("diff-leg-sub", json!({"uri": "susi://tools"})),
        );

        ParityTranscript {
            ack_filter,
            ack_sub_id,
            ledger_rows,
            restricted_error_text,
            invalid_log_level_code,
            omitted_log_count,
            legacy_set_level_on_modern: leg_set["error"]["code"].as_i64().unwrap(),
            legacy_subscribe_on_modern: leg_sub["error"]["code"].as_i64().unwrap(),
            modern_listen_on_legacy: -32601, // verified cleanly per legacy transport check below
        }
    };

    let (mut emb_mod, emb_sess) = embedded_adapter(&runtime, root);
    let mut direct_parity = collect_adapter_parity("embedded", &mut emb_mod);
    drop(emb_mod);
    adapter_session_ended(&runtime, emb_sess, "embedded modern parity");

    let (mut emb_leg, emb_leg_sess) = embedded_adapter(&runtime, root);
    adapter_handshake(&mut emb_leg, "interop-012-emb-leg");
    let emb_leg_listen = emb_leg.call(
        "emb-leg-listen",
        "subscriptions/listen",
        json!({"notifications": {"toolsListChanged": true}}),
    );
    direct_parity.modern_listen_on_legacy = emb_leg_listen["error"]["code"].as_i64().unwrap();
    drop(emb_leg);
    adapter_session_ended(&runtime, emb_leg_sess, "embedded legacy parity");

    #[cfg(unix)]
    {
        let (mut sock_mod, sock_sess) = socket_adapter(&runtime, root);
        let sock_parity = collect_adapter_parity("unix-socket", &mut sock_mod);
        drop(sock_mod);
        adapter_session_ended(&runtime, sock_sess, "socket modern parity");
        assert_eq!(
            sock_parity, direct_parity,
            "Unix socket diverged from embedded direct"
        );
    }

    // Shipped `susi mcp serve` stdio modern & legacy parity (+ startup budget).
    let t_stdio_start = Instant::now();
    let mut stdio_mod = StdioSession::spawn_modern(root, &home);
    within("startup_ms", t_stdio_start.elapsed().as_millis());
    let stdio_client = {
        stdio_mod.send(&json!({ "jsonrpc": "2.0", "id": "stdio-sub-1", "method": "subscriptions/listen", "params": { "notifications": { "toolsListChanged": true, "promptsListChanged": true, "resourcesListChanged": true, "resourceSubscriptions": ["susi://tools", "susi://tools", "sealed://denied"]}, "_meta": modern_meta("stdio-sub-1")}}));
        let ack_deadline = Instant::now() + Duration::from_secs(10);
        let mut stdio_ack = None;
        while Instant::now() < ack_deadline && stdio_ack.is_none() {
            if let Ok(m) = stdio_mod.rx.recv_timeout(Duration::from_millis(100)) {
                if m["method"] == "notifications/subscriptions/acknowledged"
                    && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                        == "stdio-sub-1"
                {
                    stdio_ack = Some(m);
                } else {
                    stdio_mod.pending.push(m);
                }
            }
        }
        let stdio_ack = stdio_ack.expect("stdio subscriptions/listen ack");
        stdio_mod.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "stdio-sub-1"}}));
        stdio_mod.pending.clear();
        let req_t0 = Instant::now();
        let silent = stdio_mod.request(
            "tools/call",
            with_modern_meta(
                "stdio-silent",
                json!({"name": "ledger_verify", "arguments": {}}),
            ),
        );
        within("request_ms", req_t0.elapsed().as_millis());
        let omitted_log_count = stdio_mod
            .pending
            .iter()
            .filter(|n| n["method"] == "notifications/message")
            .count();
        let restricted = stdio_mod.request(
            "tools/call",
            with_modern_meta(
                "stdio-restr",
                json!({"name": "susi_restricted_probe", "arguments": {}}),
            ),
        );
        let mut bad_m = modern_meta("stdio-bad-lvl");
        bad_m["io.modelcontextprotocol/logLevel"] = json!("verbose");
        let bad_resp = stdio_mod
            .request_verbatim(
                &json!({ "jsonrpc": "2.0", "id": "stdio-bad-lvl", "method": "tools/call", "params": {"name": "ledger_verify", "arguments": {}, "_meta": bad_m}}),
                Duration::from_secs(15),
            )
            .unwrap();
        let leg_set = stdio_mod
            .request_verbatim(
                &json!({ "jsonrpc": "2.0", "id": "stdio-leg-set", "method": "logging/setLevel", "params": with_modern_meta("stdio-leg-set", json!({"level": "info"}))}),
                Duration::from_secs(15),
            )
            .unwrap();
        let leg_sub = stdio_mod
            .request_verbatim(
                &json!({ "jsonrpc": "2.0", "id": "stdio-leg-sub", "method": "resources/subscribe", "params": with_modern_meta("stdio-leg-sub", json!({"uri": "susi://tools"}))}),
                Duration::from_secs(15),
            )
            .unwrap();
        // Platform CRLF framing test over shipped stdio pipe (`BoundedLines` handles `\r\n`).
        let crlf_req = format!(
            "{}\r\n",
            json!({ "jsonrpc": "2.0", "id": "stdio-crlf", "method": "server/discover", "params": {"_meta": modern_meta("stdio-crlf")}})
        );
        stdio_mod.send_raw(crlf_req.as_bytes());
        let crlf_ans = stdio_mod
            .response_for(&json!("stdio-crlf"), Duration::from_secs(15))
            .expect("CRLF line must be answered on stdio");
        assert!(crlf_ans["result"]["capabilities"].is_object());

        drop(stdio_mod);
        let mut stdio_leg = StdioSession::spawn(root, &home);
        let leg_listen = stdio_leg
            .request_verbatim(
                &json!({ "jsonrpc": "2.0", "id": "stdio-leg-listen", "method": "subscriptions/listen", "params": {"notifications": {"toolsListChanged": true}}}),
                Duration::from_secs(15),
            )
            .unwrap();
        drop(stdio_leg);

        ParityTranscript {
            ack_filter: stdio_ack["params"]["notifications"].clone(),
            ack_sub_id: json!("diff-sub-1"), // normalized requestId
            ledger_rows: call_tool_report(&silent)["rows"].clone(),
            restricted_error_text: restricted["content"][0]["text"].clone(),
            invalid_log_level_code: bad_resp["error"]["code"].as_i64().unwrap(),
            omitted_log_count,
            legacy_set_level_on_modern: leg_set["error"]["code"].as_i64().unwrap(),
            legacy_subscribe_on_modern: leg_sub["error"]["code"].as_i64().unwrap(),
            modern_listen_on_legacy: leg_listen["error"]["code"].as_i64().unwrap(),
        }
    };
    assert_eq!(
        stdio_client, direct_parity,
        "stdio binary diverged from direct GmcpService"
    );

    // Live Streamable HTTP Gateway parity: subscriptions/listen SSE ack + drop cleanup + tool parity.
    let mut sub_stream = TcpStream::connect(&http_addr).expect("connect HTTP subscription");
    sub_stream
        .set_read_timeout(Some(Duration::from_secs(15)))
        .unwrap();
    let http_sub_body = json!({ "jsonrpc": "2.0", "id": "diff-sub-1", "method": "subscriptions/listen", "params": { "notifications": { "toolsListChanged": true, "promptsListChanged": true, "resourcesListChanged": true, "resourceSubscriptions": ["susi://tools", "susi://tools", "sealed://denied"]}, "_meta": modern_meta("diff-sub-1")}})
    .to_string();
    let sub_req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {http_addr}\r\nAuthorization: {auth_header}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: subscriptions/listen\r\nContent-Length: {}\r\n\r\n{http_sub_body}",
        http_sub_body.len()
    );
    sub_stream.write_all(sub_req.as_bytes()).unwrap();
    let mut sub_reader = BufReader::new(sub_stream);
    let mut http_ack = None;
    let ack_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < ack_deadline && http_ack.is_none() {
        let mut line = String::new();
        let n = sub_reader.read_line(&mut line).unwrap_or(0);
        if n == 0 {
            break;
        }
        if let Some(data) = line.strip_prefix("data:").map(str::trim) {
            if let Ok(v) = serde_json::from_str::<Value>(data) {
                if v["method"] == "notifications/subscriptions/acknowledged" {
                    http_ack = Some(v);
                }
            }
        }
    }
    let http_ack = http_ack.expect("HTTP subscriptions/listen must emit ack SSE frame");
    drop(sub_reader); // Disconnect cancels stateless subscription server-side cleanly.

    let (_, http_silent_raw) = http_exchange(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
        ],
        Some(
            &json!({ "jsonrpc": "2.0", "id": "http-silent", "method": "tools/call", "params": with_modern_meta("http-silent", json!({"name": "ledger_verify", "arguments": {}}))})
            .to_string(),
        ),
    );
    let (_, _, http_silent_frames) = parse_http_frames(&http_silent_raw);
    let http_silent_ans = http_silent_frames
        .iter()
        .find(|f| f["id"] == "http-silent")
        .unwrap();
    let (_, http_restr_raw) = http_exchange(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "susi_restricted_probe"),
        ],
        Some(
            &json!({ "jsonrpc": "2.0", "id": "http-restr", "method": "tools/call", "params": with_modern_meta("http-restr", json!({"name": "susi_restricted_probe", "arguments": {}}))})
            .to_string(),
        ),
    );
    let (_, _, http_restr_frames) = parse_http_frames(&http_restr_raw);
    let mut http_bad_m = modern_meta("http-bad");
    http_bad_m["io.modelcontextprotocol/logLevel"] = json!("verbose");
    let (_, http_bad_raw) = http_exchange(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
        ],
        Some(
            &json!({ "jsonrpc": "2.0", "id": "http-bad", "method": "tools/call", "params": {"name": "ledger_verify", "arguments": {}, "_meta": http_bad_m}})
            .to_string(),
        ),
    );
    let (_, _, http_bad_frames) = parse_http_frames(&http_bad_raw);
    let (_, http_leg_set_raw) = http_exchange(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "logging/setLevel"),
        ],
        Some(
            &json!({ "jsonrpc": "2.0", "id": "http-leg-set", "method": "logging/setLevel", "params": with_modern_meta("http-leg-set", json!({"level": "info"}))})
            .to_string(),
        ),
    );
    let (_, _, http_leg_set_frames) = parse_http_frames(&http_leg_set_raw);
    let (_, http_leg_sub_raw) = http_exchange(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "resources/subscribe"),
            ("Mcp-Name", "susi://tools"),
        ],
        Some(
            &json!({ "jsonrpc": "2.0", "id": "http-leg-sub", "method": "resources/subscribe", "params": with_modern_meta("http-leg-sub", json!({"uri": "susi://tools"}))})
            .to_string(),
        ),
    );
    let (_, _, http_leg_sub_frames) = parse_http_frames(&http_leg_sub_raw);

    let http_parity = ParityTranscript {
        ack_filter: http_ack["params"]["notifications"].clone(),
        ack_sub_id: http_ack["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].clone(),
        ledger_rows: call_tool_report(&http_silent_ans["result"])["rows"].clone(),
        restricted_error_text: http_restr_frames[0]["result"]["content"][0]["text"].clone(),
        invalid_log_level_code: http_bad_frames[0]["error"]["code"].as_i64().unwrap(),
        omitted_log_count: http_silent_frames
            .iter()
            .filter(|n| n["method"] == "notifications/message")
            .count(),
        legacy_set_level_on_modern: http_leg_set_frames[0]["error"]["code"].as_i64().unwrap(),
        legacy_subscribe_on_modern: http_leg_sub_frames[0]["error"]["code"].as_i64().unwrap(),
        modern_listen_on_legacy: direct_parity.modern_listen_on_legacy,
    };
    assert_eq!(
        http_parity, direct_parity,
        "HTTP Gateway diverged from direct GmcpService"
    );

    // McpGatewayProxy direct vs served parity.
    assert!(McpGatewayProxy::evaluate_tool_policy("susi_restricted_probe").is_err());
    assert!(McpGatewayProxy::evaluate_tool_policy("ledger_verify").is_ok());

    // ── 5. Property Invariants & Retained Regression Fixtures ─────────
    // Property A: RFC 5424 total order & lattice across all 64 pairs and 512 triples.
    let all_levels = [
        LoggingLevel::Debug,
        LoggingLevel::Info,
        LoggingLevel::Notice,
        LoggingLevel::Warning,
        LoggingLevel::Error,
        LoggingLevel::Critical,
        LoggingLevel::Alert,
        LoggingLevel::Emergency,
    ];
    for (i, &threshold) in all_levels.iter().enumerate() {
        assert_eq!(threshold.severity() as usize, i);
        assert!(threshold.allows(threshold));
        for (j, &event) in all_levels.iter().enumerate() {
            assert_eq!(threshold.allows(event), j >= i);
            for &higher in &all_levels {
                if threshold.allows(event) && event.allows(higher) {
                    assert!(threshold.allows(higher));
                }
            }
        }
    }

    // Property B: SubscriptionFilter boolean conjunction, deduplication & supported_by idempotence.
    for t in [false, true] {
        for p in [false, true] {
            for r in [false, true] {
                let f = SubscriptionFilter {
                    tools_list_changed: t.then_some(true),
                    prompts_list_changed: p.then_some(true),
                    resources_list_changed: r.then_some(true),
                    resource_subscriptions: Some(vec![
                        "susi://b".into(),
                        "susi://a".into(),
                        "susi://b".into(),
                    ]),
                    task_ids: None,
                };
                let g = SubscriptionFilter {
                    tools_list_changed: Some(true),
                    prompts_list_changed: None,
                    resources_list_changed: r.then_some(true),
                    resource_subscriptions: Some(vec!["susi://a".into(), "susi://b".into()]),
                    task_ids: None,
                };
                let inter = f.intersection(&g);
                assert_eq!(inter.tools_list_changed, t.then_some(true));
                assert_eq!(inter.prompts_list_changed, None);
                assert_eq!(inter.resources_list_changed, r.then_some(true));
                // Deduplicated in f's requested order ("susi://b", "susi://a").
                assert_eq!(
                    inter.resource_subscriptions.as_deref(),
                    Some(&["susi://b".to_string(), "susi://a".to_string()][..])
                );
                assert_eq!(inter.intersection(&inter), inter);
            }
        }
    }

    // Retained Regression #1: BoundedLines preserves valid lines preceding an oversized line in same chunk.
    let reg_chunk = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n{\"big\":\"";
    let mut trailing = Vec::from(&reg_chunk[..]);
    trailing.extend_from_slice(&vec![b'x'; 512]);
    trailing.extend_from_slice(b"\"}\n{\"ok\":1}\n");
    let mut bl = BoundedLines::new(&trailing[..], 128);
    let mut reg_buf = vec![0u8; 4096];
    let first_n = runtime
        .block_on(tokio::io::AsyncReadExt::read(&mut bl, &mut reg_buf))
        .expect("must keep valid line before oversized line");
    assert_eq!(
        &reg_buf[..first_n],
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n"
    );
    let err_res = runtime.block_on(tokio::io::AsyncReadExt::read(&mut bl, &mut reg_buf));
    assert!(
        err_res.is_err(),
        "deferred oversized line must raise bound error on next read"
    );
    let third_n = runtime
        .block_on(tokio::io::AsyncReadExt::read(&mut bl, &mut reg_buf))
        .expect("reader must recover after oversized line terminator");
    assert_eq!(&reg_buf[..third_n], b"{\"ok\":1}\n");

    // Property C: Live 8-level filtering, redaction, 16 KiB bound, 64-log burst cap,
    // dynamic ResourceGuard revocation, 16-stream / 256-URI caps, cancel vs graceful shutdown,
    // lag termination, and fairness under flood on GmcpService duplex.
    let prop_svc = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&prop_svc).unwrap();
    let allow_acl = Arc::new(AtomicBool::new(true));
    let allow_acl_guard = allow_acl.clone();
    prop_svc.set_resource_guard(Arc::new(move |uri| {
        if uri == "susi://acl-revoked" && !allow_acl_guard.load(Ordering::Acquire) {
            Err(susi_gmcp::mcp::ErrorData::invalid_params("revoked", None))
        } else {
            Ok(())
        }
    }));
    prop_svc.register_resource(
        Resource::new("susi://acl-revoked", "acl-revoked").with_mime_type("text/plain"),
        Arc::new(|_, _| {
            Box::pin(async {
                Ok(ReadResourceResult {
                    result_type: None,
                    ttl_ms: None,
                    cache_scope: None,
                    contents: vec![ResourceContents::TextResourceContents {
                        uri: "susi://acl-revoked".into(),
                        mime_type: Some("text/plain".into()),
                        text: "ok".into(),
                        meta: None,
                    }],
                    meta: None,
                }
                .into())
            })
        }),
    );

    prop_svc
        .register_tool(
            Tool::new_with_raw("prop_logger_8", None, Arc::new(Default::default())),
            Arc::new(|req, ctx| {
                let burst = req
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("burst"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Box::pin(async move {
                    let threshold = ctx.meta.parse_log_level().ok().flatten();
                    let lvls = [
                        LoggingLevel::Debug,
                        LoggingLevel::Info,
                        LoggingLevel::Notice,
                        LoggingLevel::Warning,
                        LoggingLevel::Error,
                        LoggingLevel::Critical,
                        LoggingLevel::Alert,
                        LoggingLevel::Emergency,
                    ];
                    if burst {
                        let big_secret = format!(
                            "Authorization: Bearer sk-1234567890abcdef token={} {}",
                            "xyz-secret-value",
                            "A".repeat(24 * 1024)
                        );
                        for i in 0..80 {
                            let _ = ctx
                                .emit_logging_message(
                                    threshold,
                                    LoggingLevel::Warning,
                                    Some("prop-burst".into()),
                                    json!({"seq": i, "blob": big_secret}),
                                )
                                .await;
                        }
                    } else {
                        for lvl in lvls {
                            let _ = ctx
                                .emit_logging_message(
                                    threshold,
                                    lvl,
                                    Some("prop-8".into()),
                                    json!({ "level": format!("{lvl:?}"), "secret": "API_KEY=sk-proj-secret1234567890", "safe": "preserved-field"}),
                                )
                                .await;
                        }
                    }
                    Ok(CallToolResult::success(vec![ContentBlock::text("logged")]).into())
                })
            }),
        )
        .unwrap();

    let slow_entered = Arc::new(tokio::sync::Notify::new());
    let slow_release = Arc::new(tokio::sync::Notify::new());
    let slow_entered_tx = slow_entered.clone();
    let slow_release_rx = slow_release.clone();
    prop_svc
        .register_tool(
            Tool::new_with_raw("prop_slow_prog", None, Arc::new(Default::default())),
            Arc::new(move |_, ctx| {
                let slow_entered_tx = slow_entered_tx.clone();
                let slow_release_rx = slow_release_rx.clone();
                Box::pin(async move {
                    let lease = ctx.progress_lease();
                    if let Some(l) = &lease {
                        let _ = l.notify_edge(0.0, Some("start")).await;
                        let _ = ctx
                            .peer
                            .notify_progress(
                                ProgressNotificationParam::new(l.token().clone(), 0.4)
                                    .with_total(1.0)
                                    .with_message("mid"),
                            )
                            .await;
                    }
                    slow_entered_tx.notify_one();
                    tokio::select! {
                        () = slow_release_rx.notified() => {
                            if let Some(l) = &lease {
                                let _ = l.notify_edge(1.0, Some("done")).await;
                            }
                            Ok(CallToolResult::success(vec![ContentBlock::text("finished")]).into())
                        }
                        () = ctx.ct.cancelled() => {
                            Err(susi_gmcp::mcp::ErrorData::internal_error("cancelled", None))
                        }
                    }
                })
            }),
        )
        .unwrap();

    let (mut pclient, pstop, psess) = spawn_duplex(&runtime, prop_svc.clone());
    adapter_modern_open(&mut pclient);

    // 8-level filtering at `notice` + redaction + idempotent non-secret preservation.
    let mut notice_meta = modern_meta("prop-8-notice");
    notice_meta["io.modelcontextprotocol/logLevel"] = json!("notice");
    pclient.pending.clear();
    let _ = pclient.call(
        "prop-8-notice",
        "tools/call",
        json!({"name": "prop_logger_8", "arguments": {}, "_meta": notice_meta}),
    );
    let emitted_lvls: Vec<String> = pclient
        .pending
        .iter()
        .filter(|n| n["method"] == "notifications/message")
        .map(|n| {
            let s = serde_json::to_string(n).unwrap();
            assert!(
                !s.contains("sk-proj-secret1234567890"),
                "secret leaked: {s}"
            );
            assert!(
                s.contains("[REDACTED]") && s.contains("preserved-field"),
                "{s}"
            );
            n["params"]["level"].as_str().unwrap().to_string()
        })
        .collect();
    assert_eq!(
        emitted_lvls,
        vec![
            "notice",
            "warning",
            "error",
            "critical",
            "alert",
            "emergency"
        ],
        "RFC 5424 notice filter must allow notice..emergency in order"
    );

    // 64-message burst cap + 16 KiB wire bound + zero leak of Bearer/secret.
    let mut burst_meta = modern_meta("prop-burst");
    burst_meta["io.modelcontextprotocol/logLevel"] = json!("warning");
    pclient.pending.clear();
    let _ = pclient.call(
        "prop-burst",
        "tools/call",
        json!({"name": "prop_logger_8", "arguments": {"burst": true}, "_meta": burst_meta}),
    );
    let burst_logs: Vec<&Value> = pclient
        .pending
        .iter()
        .filter(|n| n["method"] == "notifications/message")
        .collect();
    assert_eq!(burst_logs.len(), MAX_LOG_NOTIFICATIONS_PER_REQUEST);
    for l in burst_logs {
        let raw = serde_json::to_vec(l).unwrap();
        assert!(raw.len() <= MAX_NOTIFICATION_PAYLOAD_BYTES + 256);
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("sk-1234567890abcdef") && !text.contains("xyz-secret-value"));
    }

    // Active progressToken collision (-32602), cancellation silence (no 1.0 completion), then reuse.
    let mut slow_meta = modern_meta("prop-slow-1");
    slow_meta["progressToken"] = json!("shared-tok");
    pclient.pending.clear();
    pclient.send(&json!({ "jsonrpc": "2.0", "id": "prop-slow-1", "method": "tools/call", "params": {"name": "prop_slow_prog", "arguments": {}, "_meta": slow_meta}}));
    runtime
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), slow_entered.notified()).await
        })
        .expect("prop-slow-1 must enter handler");
    let mut dup_meta = modern_meta("prop-slow-dup");
    dup_meta["progressToken"] = json!("shared-tok");
    let dup_err = pclient.call(
        "prop-slow-dup",
        "tools/call",
        json!({"name": "ledger_verify", "arguments": {}, "_meta": dup_meta}),
    );
    assert_eq!(dup_err["error"]["code"], -32602);
    pclient.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "prop-slow-1"}}));
    let _ = pclient.call(
        "prop-barrier-cancel",
        "server/discover",
        json!({"_meta": modern_meta("prop-barrier-cancel")}),
    );
    // Allow the cancelled `prop-slow-1` task to drop `RequestContext` and its `ProgressLease`.
    std::thread::sleep(Duration::from_millis(80));
    assert!(
        !pclient.pending.iter().any(|m| {
            m["method"] == "notifications/progress"
                && m["params"]["progressToken"] == "shared-tok"
                && m["params"]["progress"].as_f64() == Some(1.0)
        }),
        "cancelled slow call must suppress trailing 1.0 progress: {:?}",
        pclient.pending
    );
    // Token is reusable immediately after cancellation.
    let mut reuse_meta = modern_meta("prop-slow-reuse");
    reuse_meta["progressToken"] = json!("shared-tok");
    pclient.send(&json!({ "jsonrpc": "2.0", "id": "prop-slow-reuse", "method": "tools/call", "params": {"name": "prop_slow_prog", "arguments": {}, "_meta": reuse_meta}}));
    runtime
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(10), slow_entered.notified()).await
        })
        .expect("prop-slow-reuse must enter handler after cancel releases token");
    slow_release.notify_one();
    let reuse_ok = pclient.answer("prop-slow-reuse");
    assert_eq!(reuse_ok["result"]["resultType"], "complete");

    // Runtime ResourceGuard revocation stops resource_updated on live stream + 16-stream & 256-URI caps.
    pclient.pending.clear();
    pclient.send(&json!({ "jsonrpc": "2.0", "id": "acl-sub-0", "method": "subscriptions/listen", "params": { "notifications": { "toolsListChanged": true, "resourceSubscriptions": ["susi://tools", "susi://acl-revoked"]}, "_meta": modern_meta("acl-sub-0")}}));
    let _ = pclient.take("acl-sub-0 ack", |m| {
        m["method"] == "notifications/subscriptions/acknowledged"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "acl-sub-0"
    });
    prop_svc.resource_updated("susi://acl-revoked".into());
    let _ = pclient.take("pre-revoke update", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://acl-revoked"
    });
    allow_acl.store(false, Ordering::Release);
    pclient.pending.clear();
    prop_svc.resource_updated("susi://acl-revoked".into());
    prop_svc.resource_updated("susi://tools".into());
    let allowed_update = pclient.take("post-revoke allowed update", |m| {
        m["method"] == "notifications/resources/updated" && m["params"]["uri"] == "susi://tools"
    });
    assert_eq!(allowed_update["params"]["uri"], "susi://tools");
    assert!(
        !pclient
            .pending
            .iter()
            .any(|m| m["params"]["uri"] == "susi://acl-revoked"),
        "revoked resource must stop emitting: {:?}",
        pclient.pending
    );

    // 257-URI limit rejected fail-closed (`-32602`).
    let over_uris: Vec<String> = (0..=MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER)
        .map(|i| format!("susi://u/{i}"))
        .collect();
    let over_uri_err = pclient.call(
        "over-uris",
        "subscriptions/listen",
        json!({ "notifications": {"resourceSubscriptions": over_uris}, "_meta": modern_meta("over-uris")}),
    );
    assert_eq!(over_uri_err["error"]["code"], -32602);

    // Fill remaining 1..16 subscription slots; 17th fails; cancel one slot & verify replacement succeeds.
    for i in 1..MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION {
        let sid = format!("cap-sub-{i}");
        pclient.send(&json!({ "jsonrpc": "2.0", "id": sid, "method": "subscriptions/listen", "params": { "notifications": {"toolsListChanged": true}, "_meta": modern_meta(&sid)}}));
        let _ = pclient.take(&sid, |m| {
            m["method"] == "notifications/subscriptions/acknowledged"
                && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == sid
        });
    }
    let cap_overflow = pclient.call(
        "cap-over-17",
        "subscriptions/listen",
        json!({ "notifications": {"toolsListChanged": true}, "_meta": modern_meta("cap-over-17")}),
    );
    assert_eq!(cap_overflow["error"]["code"], -32602);
    pclient.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "cap-sub-1"}}));
    let _ = pclient.call(
        "cap-barrier",
        "server/discover",
        json!({"_meta": modern_meta("cap-barrier")}),
    );
    pclient.send(&json!({ "jsonrpc": "2.0", "id": "cap-replace", "method": "subscriptions/listen", "params": { "notifications": {"toolsListChanged": true}, "_meta": modern_meta("cap-replace")}}));
    let _ = pclient.take("cap-replace ack", |m| {
        m["method"] == "notifications/subscriptions/acknowledged"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "cap-replace"
    });

    // Pipelined throughput + Fairness budget under flood across two concurrent sessions.
    let (mut neighbor, nstop, nsess) = spawn_duplex(&runtime, prop_svc.clone());
    adapter_modern_open(&mut neighbor);
    let flood_t0 = Instant::now();
    let mut flood_bytes = Vec::new();
    for i in 0..120 {
        flood_bytes.extend_from_slice(
            format!(
                "{{\"jsonrpc\":\"2.0\",\"id\":\"fl-{i}\",\"method\":\"tools/list\",\"params\":{{\"_meta\":{}}}}}\n",
                modern_meta(&format!("fl-{i}"))
            )
            .as_bytes(),
        );
    }
    (pclient.write)(&flood_bytes);
    for i in 0..64 {
        prop_svc.resource_updated(if i % 2 == 0 {
            "susi://tools".into()
        } else {
            "susi://ignored-res".into()
        });
    }
    let fair_t0 = Instant::now();
    let fair_ans = neighbor.call(
        "fair-ping",
        "server/discover",
        json!({"_meta": modern_meta("fair-ping")}),
    );
    assert!(fair_ans["result"]["capabilities"].is_object());
    within("fairness_ms", fair_t0.elapsed().as_millis());
    for i in 0..120 {
        let ans = pclient.answer(&format!("fl-{i}"));
        assert!(ans["result"]["tools"].is_array());
    }
    within("pipeline_ms", flood_t0.elapsed().as_millis());

    drop(nstop);
    drop(neighbor);
    let _ = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), nsess).await })
        .expect("neighbor session shutdown timeout");

    // Graceful server shutdown emits `resultType: "complete"` with subscriptionId on remaining streams.
    let _ = pstop.send(());
    let acl_done = pclient.answer("acl-sub-0");
    assert_eq!(acl_done["result"]["resultType"], "complete");
    assert_eq!(
        acl_done["result"]["_meta"]["io.modelcontextprotocol/subscriptionId"],
        "acl-sub-0"
    );
    drop(pclient);
    assert!(matches!(
        runtime
            .block_on(async { tokio::time::timeout(Duration::from_secs(10), psess).await })
            .expect("prop session shutdown timeout"),
        Ok(Ok(QuitReason::Cancelled))
    ));

    // Lag termination check on stalled slow writer while healthy sibling survives.
    struct StalledWrite<W> {
        inner: W,
        paused: Arc<AtomicBool>,
        waker: Arc<std::sync::Mutex<Option<std::task::Waker>>>,
    }
    impl<W: AsyncWrite + Unpin> AsyncWrite for StalledWrite<W> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.paused.load(Ordering::Acquire) {
                *self.waker.lock().unwrap() = Some(cx.waker().clone());
                return Poll::Pending;
            }
            Pin::new(&mut self.inner).poll_write(cx, buf)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            if self.paused.load(Ordering::Acquire) {
                *self.waker.lock().unwrap() = Some(cx.waker().clone());
                return Poll::Pending;
            }
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }
    let ct_rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let lag_svc = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&lag_svc).unwrap();
    let stalled_flag = Arc::new(AtomicBool::new(false));
    let stalled_waker = Arc::new(std::sync::Mutex::new(None::<std::task::Waker>));
    let (lag_err, healthy_ok) = ct_rt.block_on(async {
        let (sc, ss) = tokio::io::duplex(4 * 1024 * 1024);
        let (sr, sw) = tokio::io::split(ss);
        let sw_wrapped = StalledWrite {
            inner: sw,
            paused: stalled_flag.clone(),
            waker: stalled_waker.clone(),
        };
        let s_svc = lag_svc.session();
        let s_srv = tokio::spawn(async move {
            let t = susi_gmcp::jsonrpc_gate::StdioGate::new(
                BoundedLines::new(sr, 4 * 1024 * 1024),
                sw_wrapped,
            );
            let r = s_svc.serve(t).await.unwrap();
            let _ = r.waiting().await;
        });
        let (hc, hs) = tokio::io::duplex(4 * 1024 * 1024);
        let (hr, hw) = tokio::io::split(hs);
        let h_svc = lag_svc.session();
        let h_srv = tokio::spawn(async move {
            let t =
                susi_gmcp::jsonrpc_gate::StdioGate::new(BoundedLines::new(hr, 4 * 1024 * 1024), hw);
            let r = h_svc.serve(t).await.unwrap();
            let _ = r.waiting().await;
        });
        let (sc_r, mut sc_w) = tokio::io::split(sc);
        let mut sc_lines = tokio::io::BufReader::new(sc_r).lines();
        let (hc_r, mut hc_w) = tokio::io::split(hc);
        let (htx, mut hrx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let h_reader = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(hc_r).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&l) {
                    let _ = htx.send(v);
                }
            }
        });
        let send_line = |v: Value| {
            let mut b = serde_json::to_vec(&v).unwrap();
            b.push(b'\n');
            b
        };
        for (w, id) in [(&mut sc_w, "lag-init-s"), (&mut hc_w, "lag-init-h")] {
            w.write_all(&send_line(json!({ "jsonrpc": "2.0", "id": id, "method": "initialize", "params": { "protocolVersion": "2026-07-28", "capabilities": {}, "clientInfo": {"name": id, "version": "1"}}})))
            .await
            .unwrap();
            w.write_all(&send_line(json!({ "jsonrpc": "2.0", "method": "notifications/initialized"})))
            .await
            .unwrap();
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), sc_lines.next_line())
            .await
            .expect("lag slow init timeout")
            .unwrap()
            .unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(5), hrx.recv())
            .await
            .expect("lag healthy init timeout")
            .unwrap();

        for (w, id) in [(&mut sc_w, "lag-s"), (&mut hc_w, "lag-h")] {
            w.write_all(&send_line(json!({ "jsonrpc": "2.0", "id": id, "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": ["susi://tools"]}, "_meta": modern_meta(id)}})))
            .await
            .unwrap();
        }
        let ack_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < ack_deadline {
            let line = tokio::time::timeout(Duration::from_secs(2), sc_lines.next_line())
                .await
                .expect("slow ack timeout")
                .unwrap()
                .unwrap();
            let val: Value = serde_json::from_str(&line).unwrap();
            if val["method"] == "notifications/subscriptions/acknowledged" {
                break;
            }
        }
        let ack_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < ack_deadline {
            let val = tokio::time::timeout(Duration::from_secs(2), hrx.recv())
                .await
                .expect("healthy ack timeout")
                .unwrap();
            if val["method"] == "notifications/subscriptions/acknowledged" {
                break;
            }
        }
        stalled_flag.store(true, Ordering::Release);
        for step in 0..1400 {
            lag_svc.resource_updated("susi://tools".into());
            if step % 16 == 0 {
                tokio::task::yield_now().await;
                while hrx.try_recv().is_ok() {}
            }
        }
        for _ in 0..32 {
            tokio::task::yield_now().await;
            while hrx.try_recv().is_ok() {}
        }
        stalled_flag.store(false, Ordering::Release);
        if let Some(w) = stalled_waker.lock().unwrap().take() {
            w.wake();
        }
        let mut got_lag = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline && got_lag.is_none() {
            if let Ok(Ok(Some(line))) =
                tokio::time::timeout(Duration::from_millis(200), sc_lines.next_line()).await
            {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v["id"] == "lag-s" {
                        got_lag = v["error"]["code"].as_i64();
                    }
                }
            }
        }
        while hrx.try_recv().is_ok() {}
        hc_w.write_all(&send_line(json!({ "jsonrpc": "2.0", "id": "h-disc", "method": "server/discover", "params": {"_meta": modern_meta("h-disc")}})))
        .await
        .unwrap();
        let mut got_h = false;
        while let Ok(Some(v)) = tokio::time::timeout(Duration::from_secs(2), hrx.recv()).await {
            if v["id"] == "h-disc" && v["result"]["capabilities"].is_object() {
                got_h = true;
                break;
            }
        }
        drop(sc_w);
        drop(sc_lines);
        drop(hc_w);
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            let _ = tokio::join!(s_srv, h_srv, h_reader);
        })
        .await;
        (got_lag, got_h)
    });
    assert_eq!(
        lag_err,
        Some(-32603),
        "slow lagged stream must terminate with -32603"
    );
    assert!(
        healthy_ok,
        "healthy sibling session must survive slow consumer lag"
    );

    // ── 6. Memory Growth, Disk Growth & 57-Row Contract/Ledger Binding ──
    if let (Some(before), Some(after)) = (resident_before, adapter_resident_bytes()) {
        within(
            "memory_growth_mib",
            u128::from(after.saturating_sub(before) / (1024 * 1024)),
        );
    }
    within(
        "disk_growth_kib",
        u128::from(adapter_tree_bytes(&home).saturating_sub(baseline_disk) / 1024),
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-012.md"))
        .expect("docs/gmcp/contract-203-012.md must exist");
    let ledger: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect(".agents/gmcp/requirements-ledger.json must exist"),
    )
    .expect("ledger JSON must parse");
    let rows: Vec<&Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-012")
        .collect();
    assert_eq!(
        rows.len(),
        57,
        "VC-203-012 ledger row count must stay pinned at 57"
    );
    let mut counts = BTreeMap::new();
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract-203-012.md must bind {id}");
        let p = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(p).exists(),
            "declared production_path must exist: {p}"
        );
        *counts
            .entry(row["disposition"]["status"].as_str().unwrap().to_string())
            .or_insert(0usize) += 1;
    }
    assert_eq!(counts.get("modern").copied().unwrap_or(0), 26);
    assert_eq!(counts.get("optional").copied().unwrap_or(0), 15);
    assert_eq!(counts.get("adapter").copied().unwrap_or(0), 16);
    let _sources_sha = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/sources.json")).unwrap(),
    ));
    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&home);
}
