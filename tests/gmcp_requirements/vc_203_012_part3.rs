/// GMCP-203-012-04: adversarial & failure behavior across validation,
/// authorization, per-session and stream isolation, privacy/redaction and
/// covert-channel bounds, concurrency races, bounded resources, slow-consumer
/// lag, cancellation, monotonic progress/tasks, HTTP fault injection, crash
/// drop recovery, and normative ledger binding.
#[test]
fn gmcp_req_203_012_04_adversarial() {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use std::time::{Duration, Instant};
    use susi_gmcp::jsonrpc_gate::{
        classify_envelope, classify_line, http_envelope_rejection, EraState, GateVerdict,
    };
    use susi_gmcp::mcp::model::{
        CallToolResult, ContentBlock, LoggingLevel, NumberOrString, ProgressNotificationParam,
        ProgressToken, ReadResourceResult, RequestMetaObject, Resource, ResourceContents, Tool,
    };
    use susi_gmcp::mcp::service::{QuitReason, ServiceExt};
    use susi_gmcp::mcp::task_manager::TaskOptions;
    use susi_gmcp::protocol::GmcpService;
    use tokio::io::AsyncWrite;

    let (_http_lock, _http_env, home) = gmcp_http_home_guard("012-adv");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();

    let modern_meta = |req_id: &str| -> Value {
        let mut meta = adapter_modern_meta()["_meta"].clone();
        meta["io.modelcontextprotocol/requestId"] = json!(req_id);
        meta
    };

    let runtime = adapter_runtime(&home);

    // Local duplex harness built over `StdioGate` + `BoundedLines`.
    let spawn_duplex = |rt: &tokio::runtime::Runtime, svc: GmcpService| {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let (client_half, server_half) = tokio::io::duplex(ADAPTER_LIMIT);
        let (server_read, server_write) = tokio::io::split(server_half);
        let sess = rt.spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(server_read, ADAPTER_LIMIT),
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

    let init_session = |client: &mut AdapterClient, version: &str, caps: Value, name: &str| {
        let init = client.call(
            "init",
            "initialize",
            json!({ "protocolVersion": version, "capabilities": caps, "clientInfo": {"name": name, "version": "1"}}),
        );
        assert!(
            init["result"]["capabilities"].is_object(),
            "{name} init failed: {init}"
        );
        client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    };

    let register_adv_res = |svc: &GmcpService, uri: &str, text: &str| {
        let payload = text.to_string();
        let uri_owned = uri.to_string();
        svc.register_resource(
            Resource::new(uri, "adv-res").with_mime_type("text/plain"),
            Arc::new(move |_, _| {
                let u = uri_owned.clone();
                let p = payload.clone();
                Box::pin(async move {
                    Ok(ReadResourceResult {
                        result_type: None,
                        ttl_ms: None,
                        cache_scope: None,
                        contents: vec![ResourceContents::TextResourceContents {
                            uri: u,
                            mime_type: Some("text/plain".to_string()),
                            text: p,
                            meta: None,
                        }],
                        meta: None,
                    }
                    .into())
                })
            }),
        );
    };

    // ── A. Wire Gate & Progress-Token Validation ─────────────────────────────
    assert!(matches!(
        classify_envelope(b"{\"jsonrpc\":\"2.0\","),
        GateVerdict::Reject { code: -32700, .. }
    ));
    assert_eq!(
        http_envelope_rejection(b"{\"jsonrpc\":\"2.0\",").and_then(|v| v["error"]["code"].as_i64()),
        Some(-32700)
    );
    for invalid_env in [
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
        br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
    ] {
        assert!(matches!(
            classify_line(invalid_env, &mut EraState::default()),
            GateVerdict::Reject { code: -32600, .. }
        ));
        assert_eq!(
            http_envelope_rejection(invalid_env).and_then(|v| v["error"]["code"].as_i64()),
            Some(-32600)
        );
    }

    // Era mixing is rejected fail-closed (`-32600`).
    let mut leg_era = EraState::default();
    let leg_init_bytes = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"era-mix","version":"1"}}}"#;
    assert!(matches!(
        classify_line(leg_init_bytes, &mut leg_era),
        GateVerdict::Forward
    ));
    let modern_listen_bytes = serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {"_meta": modern_meta("2")}}))
    .unwrap();
    assert!(matches!(
        classify_line(&modern_listen_bytes, &mut leg_era),
        GateVerdict::Reject { code: -32600, .. }
    ));

    // Direct `RequestMetaObject::parse_progress_token()` boundary & rejection tests.
    for bad_token in [
        json!(""),
        json!("p".repeat(1025)),
        json!("bad\ntok"),
        json!("bad\rtok"),
        json!("bad\0tok"),
        json!("bad\ttok"),
        json!(1.5),
        json!(true),
        json!(null),
        json!([]),
        json!({}),
    ] {
        let meta: RequestMetaObject =
            serde_json::from_value(json!({"progressToken": bad_token})).unwrap();
        let err = meta.parse_progress_token().unwrap_err();
        assert_eq!(
            err.code.0, -32602,
            "bad progressToken must fail -32602: {bad_token}"
        );
    }
    for ok_token in [
        json!("p".repeat(1024)),
        json!("tok-ok"),
        json!(42),
        json!(-7),
    ] {
        let meta: RequestMetaObject =
            serde_json::from_value(json!({"progressToken": ok_token})).unwrap();
        assert!(
            meta.parse_progress_token().unwrap().is_some(),
            "valid progressToken must parse: {ok_token}"
        );
    }

    // Shipped `susi mcp serve` validation checks (modern 2026-07-28 + legacy 2025-11-25).
    {
        let mut cli_mod = StdioSession::spawn_modern(root, &home);

        // Unknown `_meta.protocolVersion` -> -32022.
        let mut bad_ver_meta = modern_meta("bad-ver");
        bad_ver_meta["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
        let bad_ver_resp = cli_mod
            .request_verbatim(
                &json!({ "jsonrpc": "2.0", "id": "bad-ver", "method": "tools/list", "params": {"_meta": bad_ver_meta}}),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(bad_ver_resp["error"]["code"], -32022);

        // Invalid `io.modelcontextprotocol/logLevel` types & names -> -32602.
        for bad_lvl in [json!(123), json!(true), json!("verbose"), json!("INFO")] {
            let mut m = modern_meta("bad-lvl");
            m["io.modelcontextprotocol/logLevel"] = bad_lvl.clone();
            let err = cli_mod
                .request_verbatim(
                    &json!({ "jsonrpc": "2.0", "id": "bad-lvl", "method": "tools/call", "params": {"name": "ledger_verify", "arguments": {}, "_meta": m}}),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(
                err["error"]["code"], -32602,
                "bad logLevel {bad_lvl} must fail -32602: {err}"
            );
        }

        // Invalid `progressToken` values on wire -> -32602.
        for bad_tok in [
            json!(""),
            json!(1.25),
            json!(false),
            json!("bad\ntok"),
            json!("t".repeat(1025)),
        ] {
            let mut m = modern_meta("bad-tok");
            m["progressToken"] = bad_tok.clone();
            let err = cli_mod
                .request_verbatim(
                    &json!({ "jsonrpc": "2.0", "id": "bad-tok", "method": "tools/call", "params": {"name": "ledger_verify", "arguments": {}, "_meta": m}}),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(
                err["error"]["code"], -32602,
                "bad wire progressToken {bad_tok} must fail -32602: {err}"
            );
        }

        // Over-limit URI arrays (>256 unique & duplicate-padded 257) -> -32602.
        let uris_257: Vec<String> = (0..257).map(|i| format!("susi://r/{i}")).collect();
        let dup_257 = vec!["susi://tools".to_string(); 257];
        for list in [uris_257, dup_257] {
            let err = cli_mod
                .request_verbatim(
                    &json!({ "jsonrpc": "2.0", "id": "over-uri-cap", "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": list}, "_meta": modern_meta("over-uri-cap")}}),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(err["error"]["code"], -32602, "{err}");
        }

        // Malformed filter & invalid URI shapes on `subscriptions/listen`.
        let bad_shape = cli_mod
            .request_verbatim(
                &json!({ "jsonrpc": "2.0", "id": "bad-filter", "method": "subscriptions/listen", "params": { "notifications": "not-an-object", "_meta": modern_meta("bad-filter")}}),
                Duration::from_secs(10),
            )
            .unwrap();
        assert!(
            matches!(bad_shape["error"]["code"].as_i64(), Some(-32602..=-32600)),
            "malformed filter must be rejected: {bad_shape}"
        );

        // Forged `_meta["io.modelcontextprotocol/subscriptionId"]` is overwritten by real id.
        let mut forged_meta = modern_meta("real-sub-99");
        forged_meta["io.modelcontextprotocol/subscriptionId"] = json!("forged-sub-id");
        cli_mod.send(&json!({ "jsonrpc": "2.0", "id": "real-sub-99", "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": ["susi://tools"]}, "_meta": forged_meta}}));
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ack_sub_id = None;
        while Instant::now() < deadline {
            if let Some(frame) = cli_mod.response_for(&json!(null), Duration::from_millis(200)) {
                if frame["method"] == "notifications/subscriptions/acknowledged" {
                    ack_sub_id = Some(
                        frame["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"].clone(),
                    );
                    break;
                }
            }
        }
        assert_eq!(
            ack_sub_id,
            Some(json!("real-sub-99")),
            "server must overwrite forged subscriptionId with real request id"
        );
        cli_mod.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "real-sub-99"}}));

        // Client spoofing server-only notifications is silently absorbed without poisoning session.
        for spoof in [
            json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": {"progressToken": "spoof", "progress": 0.5}}),
            json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "error", "data": "spoof"}}),
            json!({"jsonrpc": "2.0", "method": "notifications/subscriptions/acknowledged", "params": {}}),
            json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": "susi://tools"}}),
        ] {
            let res = cli_mod.request_verbatim(&spoof, Duration::from_secs(5));
            assert!(
                res.is_none(),
                "spoofed server notification must not elicit a response: {res:?}"
            );
        }
        for legacy_method in [
            "resources/subscribe",
            "resources/unsubscribe",
            "logging/setLevel",
        ] {
            let err = cli_mod
                .request_verbatim(
                    &json!({ "jsonrpc": "2.0", "id": format!("leg-{legacy_method}"), "method": legacy_method, "params": {"uri": "susi://tools", "level": "info", "_meta": modern_meta("x")}}),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(
                err["error"]["code"], -32601,
                "{legacy_method} must fail -32601 on modern: {err}"
            );
        }
        let verify_ok = cli_mod.request(
            "tools/call",
            json!({"name": "ledger_verify", "arguments": {}, "_meta": modern_meta("verify-ok")}),
        );
        assert_eq!(call_tool_report(&verify_ok)["verified"], true);
    }

    // Legacy 2025-11-25 CLI session validation: `subscriptions/listen` and invalid `logging/setLevel` shape fail `-32601`.
    {
        let mut cli_leg = StdioSession::spawn(root, &home);
        let listen_err = cli_leg
            .request_verbatim(
                &json!({ "jsonrpc": "2.0", "id": "leg-listen", "method": "subscriptions/listen", "params": {"notifications": {"toolsListChanged": true}}}),
                Duration::from_secs(10),
            )
            .unwrap();
        assert_eq!(listen_err["error"]["code"], -32601);

        for bad in [json!("trace"), json!(42), json!("")] {
            let err = cli_leg
                .request_verbatim(
                    &json!({ "jsonrpc": "2.0", "id": "leg-bad-log", "method": "logging/setLevel", "params": {"level": bad}}),
                    Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(
                err["error"]["code"], -32601,
                "invalid legacy logging/setLevel must reject: {err}"
            );
        }
        let ok = cli_leg.request(
            "tools/call",
            json!({"name": "ledger_verify", "arguments": {}}),
        );
        assert_eq!(call_tool_report(&ok)["verified"], true);
    }

    // ── B. Authorization, ResourceGuard Revocation/Recovery & Session Isolation ──
    let svc = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&svc).unwrap();
    let allow_guarded = Arc::new(AtomicBool::new(true));
    let allow_flag = allow_guarded.clone();
    svc.set_resource_guard(Arc::new(move |uri| {
        if uri == "susi://adv-denied"
            || (uri == "susi://adv-guarded" && !allow_flag.load(Ordering::Acquire))
        {
            Err(susi_gmcp::mcp::ErrorData::invalid_params(
                "adv resource revoked",
                None,
            ))
        } else {
            Ok(())
        }
    }));
    register_adv_res(&svc, "susi://adv-guarded", "guarded-v1");
    register_adv_res(&svc, "susi://adv-iso", "iso-v1");

    // Probe tool covering RFC 5424 levels, log floods, secret redaction, and progress payload bounds.
    let slow_entered = Arc::new(tokio::sync::Notify::new());
    let slow_release = Arc::new(tokio::sync::Notify::new());
    let slow_exited = Arc::new(tokio::sync::Notify::new());
    let se_tx = slow_entered.clone();
    let sr_rx = slow_release.clone();
    let sx_tx = slow_exited.clone();
    let register_tool_raw = |name: &str, handler: susi_gmcp::protocol::ToolHandler| {
        let tool: Tool = serde_json::from_value(json!({ "name": name, "inputSchema": { "type": "object", "properties": { "fail": {"type": "boolean"}, "mode": {"type": "string"}}, "required": []}}))
        .unwrap();
        svc.register_tool(tool, handler).unwrap();
    };

    register_tool_raw(
        "adv_log_levels",
        Arc::new(|_, ctx| {
            Box::pin(async move {
                let th = ctx.meta.log_level();
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
                    let _ = ctx
                        .emit_logging_message(
                            th,
                            lvl,
                            Some("adv-levels".to_string()),
                            json!({"lvl": format!("{lvl:?}")}),
                        )
                        .await;
                }
                Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
            })
        }),
    );

    register_tool_raw(
        "adv_t4_probe",
        Arc::new(|args, ctx| {
            Box::pin(async move {
                let th = ctx.meta.log_level();
                let mode = args
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("secrets");
                if mode == "storm" {
                    for i in 0..150u64 {
                        let _ = ctx
                            .emit_logging_message(
                                th,
                                LoggingLevel::Warning,
                                Some("storm".to_string()),
                                json!({"seq": i}),
                            )
                            .await;
                    }
                } else {
                    let huge_utf8 = "é🚀".repeat(16 * 1024);
                    let mut deep = json!({"leaf": format!("Authorization: Bearer supersecret999 {huge_utf8}")});
                    for depth in 0..20 {
                        deep = json!({format!("k{depth}"): deep});
                    }
                    let mut wide = serde_json::Map::new();
                    for i in 0..200 {
                        wide.insert(format!("w{i}"), json!(i));
                    }
                    let payload = json!({ "banner": "sk-proj-secret1234567890abcdef ghp_abcdefghijklmnopqrstuvwxyz123456 password=mytopsecretpw api_key: superkeyval", "apiKey": "nest-secret", "token": "tok-val", "secret": "env-secret-val", "safe": "visible-safe-marker", "deep": deep, "wide": Value::Object(wide), "blob": huge_utf8});
                    let _ = ctx
                        .emit_logging_message(
                            th,
                            LoggingLevel::Error,
                            Some("t4".to_string()),
                            payload,
                        )
                        .await;
                    if let Some(tok) = ctx.meta.get_progress_token() {
                        let prog_msg = format!(
                            "Authorization: Bearer supersecret999 sk-proj-secret1234567890abcdef ghp_abcdefghijklmnopqrstuvwxyz123456 password=mytopsecretpw api_key: superkeyval {}",
                            "é🚀".repeat(12 * 1024)
                        );
                        let _ = ctx
                            .peer
                            .notify_progress(
                                ProgressNotificationParam::new(tok, 0.5)
                                    .with_total(1.0)
                                    .with_message(prog_msg),
                            )
                            .await;
                    }
                }
                Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
            })
        }),
    );

    register_tool_raw(
        "adv_slow_or_fail",
        Arc::new(move |args, ctx| {
            let se_tx = se_tx.clone();
            let sr_rx = sr_rx.clone();
            let sx_tx = sx_tx.clone();
            Box::pin(async move {
                let mode = args
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("fast");
                match mode {
                    "wait" => {
                        se_tx.notify_one();
                        tokio::select! {
                            () = sr_rx.notified() => Ok(CallToolResult::success(vec![ContentBlock::text("released")]).into()),
                            () = ctx.ct.cancelled() => {
                                sx_tx.notify_one();
                                Ok(CallToolResult::error(vec![ContentBlock::text("cancelled")]).into())
                            }
                        }
                    }
                    "err" => Ok(CallToolResult::error(vec![ContentBlock::text("boom")]).into()),
                    _ => Ok(CallToolResult::success(vec![ContentBlock::text("fast")]).into()),
                }
            })
        }),
    );

    // Connect isolated `GmcpService::session()` instances to the same `svc` for isolation & guard checks.
    let (mut mod_a, stop_mod_a, sess_mod_a) = spawn_duplex(&runtime, svc.session());
    init_session(&mut mod_a, "2026-07-28", json!({}), "mod-a");

    let (mut leg_a, stop_leg_a, sess_leg_a) = spawn_duplex(&runtime, svc.session());
    init_session(
        &mut leg_a,
        "2025-11-25",
        json!({"extensions": {"io.modelcontextprotocol/tasks": {}}}),
        "leg-a",
    );

    let (mut leg_b, stop_leg_b, sess_leg_b) = spawn_duplex(&runtime, svc.session());
    init_session(&mut leg_b, "2025-11-25", json!({}), "leg-b");

    // Traversal, encoded traversal, >2048-byte, whitespace & unknown URIs on legacy `resources/subscribe`.
    let huge_uri = format!("susi://{}", "u".repeat(2050));
    for bad_uri in [
        "susi://../secret",
        "susi://a/%2e%2e/b",
        huge_uri.as_str(),
        "susi://has space",
        "susi://adv-denied",
    ] {
        let err = leg_a.call("bad-sub", "resources/subscribe", json!({"uri": bad_uri}));
        assert_eq!(
            err["error"]["code"], -32602,
            "invalid URI {bad_uri} must fail -32602: {err}"
        );
    }
    let unknown_sub = leg_a.call(
        "unk-sub",
        "resources/subscribe",
        json!({"uri": "susi://missing-adv-uri"}),
    );
    assert_eq!(unknown_sub["error"]["code"], -32002);

    // Modern `subscriptions/listen` omits traversal, oversized, whitespace & guard-denied URIs from ack.
    mod_a.send(&json!({ "jsonrpc": "2.0", "id": "sub-filter-adv", "method": "subscriptions/listen", "params": { "notifications": { "resourceSubscriptions": [ "susi://tools", "susi://../secret", "susi://a/%2e%2e/b", huge_uri, "susi://has space", "susi://adv-denied"]}, "_meta": modern_meta("sub-filter-adv")}}));
    let f_ack = mod_a.take("filter ack", |m| {
        m["method"] == "notifications/subscriptions/acknowledged"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "sub-filter-adv"
    });
    assert_eq!(
        f_ack["params"]["notifications"]["resourceSubscriptions"],
        json!(["susi://tools"]),
        "unadmitted URIs must be omitted from subscription ack: {f_ack}"
    );
    mod_a.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "sub-filter-adv"}}));

    // Dynamic `ResourceGuard` revocation halts updates on BOTH `subscriptions/listen` & `resources/subscribe`, then re-grant recovers.
    mod_a.send(&json!({ "jsonrpc": "2.0", "id": "guard-mod", "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": ["susi://adv-guarded"]}, "_meta": modern_meta("guard-mod")}}));
    let _ = mod_a.take("guard-mod ack", |m| {
        m["method"] == "notifications/subscriptions/acknowledged"
            && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "guard-mod"
    });
    assert!(leg_a
        .call(
            "guard-leg",
            "resources/subscribe",
            json!({"uri": "susi://adv-guarded"})
        )
        .get("result")
        .is_some());

    svc.resource_updated("susi://adv-guarded".to_string());
    let _ = mod_a.take("guard-mod v1", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://adv-guarded"
    });
    let _ = leg_a.take("guard-leg v1", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://adv-guarded"
    });

    allow_guarded.store(false, Ordering::Release);
    let read_revoked = mod_a.call(
        "read-revoked",
        "resources/read",
        json!({"uri": "susi://adv-guarded", "_meta": modern_meta("read-revoked")}),
    );
    assert_eq!(read_revoked["error"]["code"], -32602);
    mod_a.pending.clear();
    leg_a.pending.clear();
    svc.resource_updated("susi://adv-guarded".to_string());
    let _ = mod_a.call(
        "guard-barrier-m",
        "server/discover",
        json!({"_meta": modern_meta("guard-barrier-m")}),
    );
    let _ = leg_a.call("guard-barrier-l", "ping", json!({}));
    assert!(
        !mod_a
            .pending
            .iter()
            .any(|m| m["method"] == "notifications/resources/updated"),
        "revoked ResourceGuard must block modern subscription updates: {:?}",
        mod_a.pending
    );
    assert!(
        !leg_a
            .pending
            .iter()
            .any(|m| m["method"] == "notifications/resources/updated"),
        "revoked ResourceGuard must block legacy subscription updates: {:?}",
        leg_a.pending
    );

    allow_guarded.store(true, Ordering::Release);
    svc.resource_updated("susi://adv-guarded".to_string());
    let _ = mod_a.take("guard-mod recovered", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://adv-guarded"
    });
    let _ = leg_a.take("guard-leg recovered", |m| {
        m["method"] == "notifications/resources/updated"
            && m["params"]["uri"] == "susi://adv-guarded"
    });
    mod_a.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "guard-mod"}}));

    // Strict log level enforcement (`None` -> 0 logs, `emergency` -> only 1, `warning` -> 5) & legacy session isolation.
    mod_a.pending.clear();
    let _ = mod_a.call(
        "log-none",
        "tools/call",
        json!({"name": "adv_log_levels", "arguments": {}, "_meta": modern_meta("log-none")}),
    );
    assert!(
        !mod_a
            .pending
            .iter()
            .any(|m| m["method"] == "notifications/message"),
        "omitted logLevel must emit zero logs"
    );

    let mut emerg_meta = modern_meta("log-emerg");
    emerg_meta["io.modelcontextprotocol/logLevel"] = json!("emergency");
    mod_a.pending.clear();
    let _ = mod_a.call(
        "log-emerg",
        "tools/call",
        json!({"name": "adv_log_levels", "arguments": {}, "_meta": emerg_meta}),
    );
    let emerg_msgs: Vec<_> = mod_a
        .pending
        .drain(..)
        .filter(|m| m["method"] == "notifications/message")
        .collect();
    assert_eq!(emerg_msgs.len(), 1, "{emerg_msgs:?}");
    assert_eq!(emerg_msgs[0]["params"]["level"], "emergency");

    let mut warn_meta = modern_meta("log-warn");
    warn_meta["io.modelcontextprotocol/logLevel"] = json!("warning");
    mod_a.pending.clear();
    let _ = mod_a.call(
        "log-warn",
        "tools/call",
        json!({"name": "adv_log_levels", "arguments": {}, "_meta": warn_meta}),
    );
    let warn_msgs: Vec<_> = mod_a
        .pending
        .drain(..)
        .filter(|m| m["method"] == "notifications/message")
        .collect();
    assert_eq!(warn_msgs.len(), 5, "warning..emergency = 5: {warn_msgs:?}");

    assert!(leg_a
        .call("leg-a-lvl", "logging/setLevel", json!({"level": "error"}))
        .get("result")
        .is_some());
    assert!(leg_a
        .call(
            "leg-a-iso",
            "resources/subscribe",
            json!({"uri": "susi://adv-iso"})
        )
        .get("result")
        .is_some());
    assert!(leg_b
        .call("leg-b-lvl", "logging/setLevel", json!({"level": "debug"}))
        .get("result")
        .is_some());

    leg_a.pending.clear();
    leg_b.pending.clear();
    svc.resource_updated("susi://adv-iso".to_string());
    let _ = leg_a.take("leg-a iso update", |m| {
        m["method"] == "notifications/resources/updated" && m["params"]["uri"] == "susi://adv-iso"
    });
    let _ = leg_b.call("leg-b-ping", "ping", json!({}));
    assert!(
        !leg_b
            .pending
            .iter()
            .any(|m| m["method"] == "notifications/resources/updated"),
        "unsubscribed session leg_b must stay isolated from leg_a: {:?}",
        leg_b.pending
    );
    let _ = leg_a.call(
        "leg-a-verify-1",
        "tools/call",
        json!({"name": "ledger_verify", "arguments": {}}),
    );
    let leg_a_logs: Vec<_> = leg_a
        .pending
        .drain(..)
        .filter(|m| m["method"] == "notifications/message")
        .collect();
    assert_eq!(
        leg_a_logs.len(),
        0,
        "leg_a error floor must suppress info tool completion logs: {leg_a_logs:?}"
    );
    let _ = leg_b.call(
        "leg-b-verify-1",
        "tools/call",
        json!({"name": "ledger_verify", "arguments": {}}),
    );
    let leg_b_logs: Vec<_> = leg_b
        .pending
        .drain(..)
        .filter(|m| m["method"] == "notifications/message")
        .collect();
    assert!(
        leg_b_logs.iter().any(|m| m["params"]["level"] == "info"),
        "leg_b debug floor must emit info tool completion log independently: {leg_b_logs:?}"
    );
    // Flip levels across sessions to prove bidirectional per-session isolation.
    assert!(leg_a
        .call("leg-a-lvl-2", "logging/setLevel", json!({"level": "info"}))
        .get("result")
        .is_some());
    assert!(leg_b
        .call(
            "leg-b-lvl-2",
            "logging/setLevel",
            json!({"level": "emergency"})
        )
        .get("result")
        .is_some());
    leg_a.pending.clear();
    leg_b.pending.clear();
    let _ = leg_a.call(
        "leg-a-verify-2",
        "tools/call",
        json!({"name": "ledger_verify", "arguments": {}}),
    );
    assert!(
        leg_a
            .pending
            .drain(..)
            .any(|m| m["method"] == "notifications/message" && m["params"]["level"] == "info"),
        "leg_a flipped to info must emit info log"
    );
    let _ = leg_b.call(
        "leg-b-verify-2",
        "tools/call",
        json!({"name": "ledger_verify", "arguments": {}}),
    );
    assert!(
        !leg_b
            .pending
            .drain(..)
            .any(|m| m["method"] == "notifications/message"),
        "leg_b flipped to emergency must suppress info log"
    );

    // ── C. Privacy, Redaction, UTF-8 Clamping & Notification Storm Cap (T4) ──
    let mut t4_meta = modern_meta("t4-secrets");
    t4_meta["io.modelcontextprotocol/logLevel"] = json!("debug");
    t4_meta["progressToken"] = json!("t4-prog");
    mod_a.pending.clear();
    let _ = mod_a.call(
        "t4-secrets",
        "tools/call",
        json!({"name": "adv_t4_probe", "arguments": {"mode": "secrets"}, "_meta": t4_meta}),
    );
    let t4_frames: Vec<_> = mod_a
        .pending
        .drain(..)
        .filter(|m| {
            m["method"] == "notifications/message" || m["method"] == "notifications/progress"
        })
        .collect();
    assert!(
        t4_frames.len() >= 2,
        "expected log and progress frames: {t4_frames:?}"
    );
    for frame in &t4_frames {
        let encoded = serde_json::to_string(frame).expect("frame must encode as valid UTF-8");
        assert!(
            encoded.len() <= 16 * 1024 + 512,
            "clamped notification frame exceeded T4 ceiling: {} bytes",
            encoded.len()
        );
        for leaked in [
            "supersecret999",
            "sk-proj-secret1234567890abcdef",
            "ghp_abcdefghijklmnopqrstuvwxyz123456",
            "mytopsecretpw",
            "superkeyval",
            "nest-secret",
            "tok-val",
            "env-secret-val",
        ] {
            assert!(
                !encoded.contains(leaked),
                "notification leaked secret token {leaked}: {encoded}"
            );
        }
        assert!(
            frame["params"]
                .get("_meta")
                .and_then(|m| m.get("io.modelcontextprotocol/subscriptionId"))
                .is_none(),
            "log/progress notification must never carry subscriptionId: {frame}"
        );
    }

    // 150-emit log storm is capped at <= 64 per request (`MAX_LOG_NOTIFICATIONS_PER_REQUEST`).
    let mut storm_meta = modern_meta("t4-storm");
    storm_meta["io.modelcontextprotocol/logLevel"] = json!("debug");
    mod_a.pending.clear();
    let _ = mod_a.call(
        "t4-storm",
        "tools/call",
        json!({"name": "adv_t4_probe", "arguments": {"mode": "storm"}, "_meta": storm_meta}),
    );
    let storm_count = mod_a
        .pending
        .drain(..)
        .filter(|m| m["method"] == "notifications/message")
        .count();
    assert_eq!(storm_count, 64, "log storm must cap strictly at 64");

    // ── D. Active Subscription Cap Race (24 -> 16/8 + Cancel Recovery) & Slow Lag ──
    mod_a.pending.clear();
    for i in 0..24 {
        let sub_id = format!("race-{i:02}");
        mod_a.send(&json!({ "jsonrpc": "2.0", "id": sub_id, "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": ["susi://tools"]}, "_meta": modern_meta(&sub_id)}}));
    }
    let mut admitted_ids = Vec::new();
    let mut rejected_ids = Vec::new();
    let race_deadline = Instant::now() + Duration::from_secs(20);
    while admitted_ids.len() + rejected_ids.len() < 24 && Instant::now() < race_deadline {
        let msg = mod_a.take("sub race response", |m| {
            m["method"] == "notifications/subscriptions/acknowledged" || m["error"]["code"].is_i64()
        });
        if msg["method"] == "notifications/subscriptions/acknowledged" {
            let sid = msg["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"]
                .as_str()
                .unwrap()
                .to_string();
            admitted_ids.push(sid);
        } else {
            assert_eq!(msg["error"]["code"], -32602, "{msg}");
            rejected_ids.push(msg["id"].as_str().unwrap().to_string());
        }
    }
    assert_eq!(
        admitted_ids.len(),
        16,
        "must admit exactly 16 subscriptions"
    );
    assert_eq!(
        rejected_ids.len(),
        8,
        "must reject 8 excess subscriptions with -32602"
    );

    for sid in admitted_ids.iter().take(4) {
        mod_a.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": sid}}));
    }
    let _ = mod_a.call(
        "post-race-cancel",
        "server/discover",
        json!({"_meta": modern_meta("post-race-cancel")}),
    );
    for j in 0..4 {
        let sub_id = format!("recover-{j}");
        mod_a.send(&json!({ "jsonrpc": "2.0", "id": sub_id, "method": "subscriptions/listen", "params": { "notifications": {"resourceSubscriptions": ["susi://tools"]}, "_meta": modern_meta(&sub_id)}}));
        let ack = mod_a.take("recovered sub ack", |m| {
            m["method"] == "notifications/subscriptions/acknowledged"
                && m["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == sub_id
        });
        assert_eq!(ack["method"], "notifications/subscriptions/acknowledged");
    }

    // Deterministic single-threaded `RecvError::Lagged` on stalled writer while concurrent healthy stream stays alive.
    struct ControllableWrite<W> {
        inner: W,
        paused: Arc<AtomicBool>,
        waker: Arc<std::sync::Mutex<Option<std::task::Waker>>>,
    }
    impl<W: AsyncWrite + Unpin> AsyncWrite for ControllableWrite<W> {
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
    register_adv_res(&lag_svc, "susi://lag-adv", "v1");
    let slow_paused = Arc::new(AtomicBool::new(false));
    let slow_waker = Arc::new(std::sync::Mutex::new(None::<std::task::Waker>));
    let (slow_lag_code, healthy_survived) = ct_rt.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let (slow_c, slow_s) = tokio::io::duplex(ADAPTER_LIMIT);
        let (slow_r, slow_w) = tokio::io::split(slow_s);
        let wrapped_w = ControllableWrite {
            inner: slow_w,
            paused: slow_paused.clone(),
            waker: slow_waker.clone(),
        };
        let slow_svc = lag_svc.session();
        let slow_srv = tokio::spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(slow_r, ADAPTER_LIMIT),
                wrapped_w,
            );
            let running = slow_svc.serve(transport).await.unwrap();
            let _ = running.waiting().await;
        });

        let (ok_c, ok_s) = tokio::io::duplex(ADAPTER_LIMIT);
        let (ok_r, ok_w) = tokio::io::split(ok_s);
        let ok_svc = lag_svc.session();
        let ok_srv = tokio::spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(ok_r, ADAPTER_LIMIT),
                ok_w,
            );
            let running = ok_svc.serve(transport).await.unwrap();
            let _ = running.waiting().await;
        });

        let write_json = |v: Value| {
            let mut b = serde_json::to_vec(&v).unwrap();
            b.push(b'\n');
            b
        };
        let (sr, mut sw) = tokio::io::split(slow_c);
        let mut slines = tokio::io::BufReader::new(sr).lines();
        let (hr, mut hw) = tokio::io::split(ok_c);
        let (htx, mut hrx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let ok_reader = tokio::spawn(async move {
            let mut hlines = tokio::io::BufReader::new(hr).lines();
            while let Ok(Some(l)) = hlines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&l) {
                    let _ = htx.send(v);
                }
            }
        });

        for (cw, id) in [(&mut sw, "init-slow"), (&mut hw, "init-ok")] {
            cw.write_all(&write_json(json!({ "jsonrpc": "2.0", "id": id, "method": "initialize", "params": {"protocolVersion": "2026-07-28", "capabilities": {}, "clientInfo": {"name": id, "version": "1"}}})))
            .await
            .unwrap();
            cw.write_all(&write_json(
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            ))
            .await
            .unwrap();
        }
        let _ = slines.next_line().await.unwrap();
        let _ = hrx.recv().await.unwrap();

        sw.write_all(&write_json(json!({ "jsonrpc": "2.0", "id": "slow-lag", "method": "subscriptions/listen", "params": {"notifications": {"resourceSubscriptions": ["susi://lag-adv"]}, "_meta": modern_meta("slow-lag")}})))
        .await
        .unwrap();
        while let Ok(Some(l)) = slines.next_line().await {
            if let Ok(v) = serde_json::from_str::<Value>(&l) {
                if v["method"] == "notifications/subscriptions/acknowledged" {
                    break;
                }
            }
        }
        hw.write_all(&write_json(json!({ "jsonrpc": "2.0", "id": "healthy-sub", "method": "subscriptions/listen", "params": {"notifications": {"resourceSubscriptions": ["susi://lag-adv"]}, "_meta": modern_meta("healthy-sub")}})))
        .await
        .unwrap();
        while let Some(v) = hrx.recv().await {
            if v["method"] == "notifications/subscriptions/acknowledged" {
                break;
            }
        }

        // Stall slow writer, overflow outbound + broadcast (>1400 writes with yields so healthy drains continuously).
        slow_paused.store(true, Ordering::Release);
        for step in 0..1400 {
            lag_svc.resource_updated("susi://lag-adv".to_string());
            if step % 16 == 0 {
                tokio::task::yield_now().await;
                while hrx.try_recv().is_ok() {}
            }
        }
        for _ in 0..32 {
            tokio::task::yield_now().await;
            while hrx.try_recv().is_ok() {}
        }

        // Unpause slow writer and observe terminal -32603 while healthy subscriber keeps receiving updates.
        slow_paused.store(false, Ordering::Release);
        if let Some(w) = slow_waker.lock().unwrap().take() {
            w.wake();
        }
        let mut slow_code = None;
        while let Ok(Ok(Some(l))) = tokio::time::timeout(Duration::from_secs(5), slines.next_line()).await {
            if let Ok(v) = serde_json::from_str::<Value>(&l) {
                if v["id"] == "slow-lag" {
                    slow_code = v["error"]["code"].as_i64();
                    break;
                }
            }
        }

        while hrx.try_recv().is_ok() {}
        lag_svc.resource_updated("susi://lag-adv".to_string());
        let mut healthy_ok = false;
        let dead = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < dead {
            if let Ok(Some(v)) = tokio::time::timeout(Duration::from_millis(300), hrx.recv()).await {
                if v["method"] == "notifications/resources/updated"
                    && v["params"]["_meta"]["io.modelcontextprotocol/subscriptionId"] == "healthy-sub"
                {
                    healthy_ok = true;
                    break;
                }
            } else {
                break;
            }
        }

        drop(sw);
        drop(slines);
        drop(hw);
        ok_reader.abort();
        let _ = slow_srv.await;
        let _ = ok_srv.await;
        (slow_code, healthy_ok)
    });
    assert_eq!(slow_lag_code, Some(-32603));
    assert!(
        healthy_survived,
        "healthy subscriber must survive another reader lagging"
    );

    // ── E. Cancellation, Progress Token Reuse, Monotonicity & Tasks (T5) ─────
    let reusable = "reusable-tok-04";
    for (req_id, mode) in [("reuse-ok", "fast"), ("reuse-err", "err")] {
        let mut m = modern_meta(req_id);
        m["progressToken"] = json!(reusable);
        let _ = mod_a.call(
            req_id,
            "tools/call",
            json!({"name": "adv_slow_or_fail", "arguments": {"mode": mode}, "_meta": m}),
        );
    }
    // Schema-invalid tool call (`isError: true`) & unknown tool (`-32602`) must not leak `progressToken`.
    let mut m_schema = modern_meta("reuse-schema");
    m_schema["progressToken"] = json!(reusable);
    let schema_err = mod_a.call(
        "reuse-schema",
        "tools/call",
        json!({"name": "adv_slow_or_fail", "arguments": {"mode": 42}, "_meta": m_schema}),
    );
    assert_eq!(
        schema_err["result"]["isError"], true,
        "schema-invalid tool call must fail with isError: {schema_err}"
    );
    let mut m_unknown = modern_meta("reuse-unknown-tool");
    m_unknown["progressToken"] = json!(reusable);
    let unknown_err = mod_a.call(
        "reuse-unknown-tool",
        "tools/call",
        json!({"name": "no_such_adv_tool", "arguments": {}, "_meta": m_unknown}),
    );
    assert_eq!(unknown_err["error"]["code"], -32602, "{unknown_err}");
    // Cancel active tool holding `reusable`, then reuse it again on completion.
    let mut m_cancel = modern_meta("reuse-cancel");
    m_cancel["progressToken"] = json!(reusable);
    mod_a.send(&json!({ "jsonrpc": "2.0", "id": "reuse-cancel", "method": "tools/call", "params": {"name": "adv_slow_or_fail", "arguments": {"mode": "wait"}, "_meta": m_cancel}}));
    runtime.block_on(slow_entered.notified());
    mod_a.send(&json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "reuse-cancel"}}));
    runtime.block_on(slow_exited.notified());
    std::thread::sleep(std::time::Duration::from_millis(25));
    let _ = mod_a.call("reuse-barrier", "server/discover", json!({}));
    assert!(
        !mod_a.pending.iter().any(|m| m["id"] == "reuse-cancel"),
        "cancelled request must not emit a response: {:?}",
        mod_a.pending
    );
    let mut m_after = modern_meta("reuse-after");
    m_after["progressToken"] = json!(reusable);
    let reuse_ok = mod_a.call(
        "reuse-after",
        "tools/call",
        json!({"name": "adv_slow_or_fail", "arguments": {"mode": "fast"}, "_meta": m_after}),
    );
    assert_eq!(reuse_ok["result"]["resultType"], "complete");

    // Forged/duplicate notifications/cancelled are safe no-ops.
    for bogus_cancel in [
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "never-existed-999"}}),
        json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "reuse-cancel"}}),
    ] {
        mod_a.send(&bogus_cancel);
    }

    // Direct `Peer::notify_progress` monotonicity & negative/NaN/Inf rejection without state corruption.
    {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let p_svc = svc.session();
        runtime.block_on(async move {
            let (c_half, s_half) = tokio::io::duplex(ADAPTER_LIMIT);
            let (sr, sw) = tokio::io::split(s_half);
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(
                susi_gmcp::stdio::BoundedLines::new(sr, ADAPTER_LIMIT),
                sw,
            );
            let (cr, mut cw) = tokio::io::split(c_half);
            let reader = tokio::spawn(async move {
                let mut lines = tokio::io::BufReader::new(cr).lines();
                let mut seen = Vec::new();
                while let Ok(Some(l)) = lines.next_line().await {
                    seen.push(l);
                }
                seen
            });
            let init = serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2026-07-28", "capabilities": {}, "clientInfo": {"name": "mono", "version": "1"}}}))
            .unwrap();
            cw.write_all(&init).await.unwrap();
            cw.write_all(b"\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
                .await
                .unwrap();
            let running = p_svc.serve(transport).await.unwrap();
            let peer = running.peer().clone();
            let tok = ProgressToken(NumberOrString::String(Arc::from("mono-tok")));
            let lease = peer.enter_progress(tok.clone()).expect("enter progress lease");
            assert!(peer
                .notify_progress(ProgressNotificationParam::new(tok.clone(), 0.5).with_total(1.0))
                .await
                .is_ok());
            for (bad_p, bad_t) in [
                (0.25, Some(1.0)),
                (0.5, Some(1.0)),
                (-0.1, Some(1.0)),
                (f64::NAN, Some(1.0)),
                (f64::INFINITY, Some(1.0)),
                (0.6, Some(-1.0)),
                (0.6, Some(f64::NAN)),
            ] {
                let mut p = ProgressNotificationParam::new(tok.clone(), bad_p);
                if let Some(t) = bad_t {
                    p = p.with_total(t);
                }
                assert!(
                    peer.notify_progress(p).await.is_err(),
                    "invalid progress ({bad_p}, {bad_t:?}) must be refused"
                );
            }
            assert!(
                peer.notify_progress(
                    ProgressNotificationParam::new(tok.clone(), 0.6).with_total(1.0)
                )
                .await
                .is_ok(),
                "invalid progress attempts must not corrupt next valid monotonic step 0.6"
            );
            drop(lease);
            assert!(peer.enter_progress(tok).is_ok(), "dropped lease must free token");
            let _ = cw.shutdown().await;
            drop(cw);
            let _ = running.cancel().await;
            let _ = reader.await;
        });
    }

    // `tasks/get` & `tasks/cancel`: capability gate (-32021), unknown task (-32602), active cancel, post-terminal cancel (-32602).
    let no_cap_task = mod_a.call(
        "task-nocap",
        "tasks/get",
        json!({"taskId": "missing", "_meta": modern_meta("task-nocap")}),
    );
    assert_eq!(no_cap_task["error"]["code"], -32021);
    let no_cap_cancel = mod_a.call(
        "task-nocap-cancel",
        "tasks/cancel",
        json!({"taskId": "missing", "_meta": modern_meta("task-nocap-cancel")}),
    );
    assert_eq!(no_cap_cancel["error"]["code"], -32021);

    assert_eq!(
        leg_a.call(
            "task-unk-get",
            "tasks/get",
            json!({"taskId": "no-such-task"})
        )["error"]["code"],
        -32602
    );
    assert_eq!(
        leg_a.call(
            "task-unk-cancel",
            "tasks/cancel",
            json!({"taskId": "no-such-task"})
        )["error"]["code"],
        -32602
    );
    let active_task = {
        let _g = runtime.enter();
        svc.tasks.spawn(TaskOptions::default(), |ctx| {
            Box::pin(async move {
                ctx.cancelled().await;
                Err(susi_gmcp::mcp::task_manager::TaskExit::Cancelled)
            })
        })
    };
    assert!(leg_a
        .call(
            "task-cancel-active",
            "tasks/cancel",
            json!({"taskId": active_task.task_id})
        )
        .get("result")
        .is_some());
    runtime.block_on(async {
        for _ in 0..50 {
            if matches!(
                svc.tasks
                    .get_task(&active_task.task_id)
                    .map(|d| d.task.status),
                Ok(susi_gmcp::mcp::model::TaskStatus::Cancelled)
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });
    assert_eq!(
        leg_a.call(
            "task-cancel-terminal",
            "tasks/cancel",
            json!({"taskId": active_task.task_id})
        )["error"]["code"],
        -32602
    );

    // Tear down duplex control sessions cleanly (both cancel and EOF paths).
    let _ = stop_mod_a.send(());
    assert!(matches!(
        runtime.block_on(sess_mod_a),
        Ok(Ok(QuitReason::Cancelled))
    ));
    drop(mod_a);
    let _ = stop_leg_a;
    let _ = stop_leg_b;
    drop(leg_a);
    drop(leg_b);
    assert!(matches!(
        runtime.block_on(sess_leg_a),
        Ok(Ok(QuitReason::Closed))
    ));
    assert!(matches!(
        runtime.block_on(sess_leg_b),
        Ok(Ok(QuitReason::Closed))
    ));

    // ── F. HTTP Fault Injection, Session Teardown & CLI Crash/Stderr Recovery ─
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "susi-012-adv-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();
    let mut http_cfg = susi_config::SusiConfig::default();
    http_cfg
        .settings
        .insert("max_rpc_body_bytes".to_string(), json!(16 * 1024));
    http_cfg.save(&cfg_dir).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP adversarial listener");
    let port = listener.local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let root_http = root.to_path_buf();
    std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(root_http, vec![listener], None, false);
    });
    for _ in 0..40 {
        if TcpStream::connect(&addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let http_send = |method: &str, headers: &[(&str, &str)], body: Option<&str>| -> String {
        let mut s = TcpStream::connect(&addr).expect("connect HTTP server");
        s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        let mut req = format!("{method} /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
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
        resp
    };
    let auth_header = format!("Bearer {token}");

    let r401 = http_send(
        "POST",
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    );
    assert!(
        r401.starts_with("HTTP/1.1 401") && r401.to_ascii_lowercase().contains("www-authenticate:")
    );

    let r403 = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Origin", "https://evil.example"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    );
    assert!(r403.starts_with("HTTP/1.1 403"));

    let r406 = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    );
    assert!(r406.starts_with("HTTP/1.1 406"), "{r406}");

    let r415 = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "text/plain"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    );
    assert!(r415.starts_with("HTTP/1.1 415"), "{r415}");

    let r400 = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some("{"),
    );
    assert!(
        r400.starts_with("HTTP/1.1 400") && r400.contains("-32700"),
        "{r400}"
    );

    let big_body =
        json!({"jsonrpc":"2.0","id":1,"method":"ping","params":{"pad":"x".repeat(17 * 1024)}})
            .to_string();
    let r413 = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(&big_body),
    );
    assert!(r413.starts_with("HTTP/1.1 413"), "{r413}");

    let r405 = http_send(
        "GET",
        &[
            ("Authorization", &auth_header),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Accept", "text/event-stream"),
        ],
        None,
    );
    assert!(r405.starts_with("HTTP/1.1 405"));

    let r404 = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Mcp-Session-Id", "bogus-session-id"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#),
    );
    assert!(r404.starts_with("HTTP/1.1 404"), "{r404}");

    // Legacy 2025-11-25 HTTP session: bogus `Last-Event-Id` non-fatal; `DELETE /mcp` invalidates to 404.
    let init_resp = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"http-adv","version":"1"}}}"#,
        ),
    );
    assert!(init_resp.starts_with("HTTP/1.1 200"));
    let sid = init_resp
        .split_once("\r\n\r\n")
        .unwrap()
        .0
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("mcp-session-id")
                .then(|| v.trim().to_string())
        })
        .expect("Mcp-Session-Id");
    {
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let req = format!(
            "GET /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth_header}\r\nMcp-Session-Id: {sid}\r\nLast-Event-Id: invalid-evt\r\nMCP-Protocol-Version: 2025-11-25\r\nAccept: text/event-stream\r\n\r\n"
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut line = String::new();
        let mut r = BufReader::new(s);
        r.read_line(&mut line).unwrap();
        assert!(
            line.starts_with("HTTP/1.1 200"),
            "bogus Last-Event-Id must be non-fatal: {line}"
        );
    }
    let del_resp = http_send(
        "DELETE",
        &[
            ("Authorization", &auth_header),
            ("Mcp-Session-Id", &sid),
            ("MCP-Protocol-Version", "2025-11-25"),
        ],
        None,
    );
    assert!(
        del_resp.starts_with("HTTP/1.1 200") || del_resp.starts_with("HTTP/1.1 202"),
        "{del_resp}"
    );
    let post_deleted = http_send(
        "POST",
        &[
            ("Authorization", &auth_header),
            ("Mcp-Session-Id", &sid),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        Some(r#"{"jsonrpc":"2.0","id":2,"method":"ping"}"#),
    );
    assert!(post_deleted.starts_with("HTTP/1.1 404"), "{post_deleted}");

    // Mid-stream TCP reset of SSE `subscriptions/listen` recovers cleanly on follow-up `tools/call`.
    {
        let sub_body = json!({ "jsonrpc": "2.0", "id": "http-drop-sub", "method": "subscriptions/listen", "params": {"notifications": {"resourceSubscriptions": ["susi://tools"]}, "_meta": modern_meta("http-drop-sub")}})
        .to_string();
        let mut s = TcpStream::connect(&addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nAuthorization: {auth_header}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMCP-Protocol-Version: 2026-07-28\r\nMcp-Method: subscriptions/listen\r\nContent-Length: {}\r\n\r\n{sub_body}",
            sub_body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut rdr = BufReader::new(s);
        let mut line = String::new();
        loop {
            line.clear();
            if rdr.read_line(&mut line).unwrap() == 0 {
                break;
            }
            if line.contains("notifications/subscriptions/acknowledged") {
                break;
            }
        }
    }
    let follow_res =
        susi_core::mcp_client::call_tool(&addr, "ledger_verify", &json!({}), Some(token))
            .expect("follow-up tool call after dropped SSE must succeed");
    assert_eq!(call_tool_report(&follow_res)["verified"], true);

    // Shipped `susi mcp serve` truncated JSON, NUL byte, oversized line, clean recovery & clean stderr.
    {
        let cli_home = home.join("cli-adv-home");
        let cli_cfg_dir = cli_home.join(".susi");
        std::fs::create_dir_all(&cli_cfg_dir).unwrap();
        let mut cli_cfg = susi_config::SusiConfig::default();
        cli_cfg
            .settings
            .insert("max_rpc_body_bytes".to_string(), json!(4096));
        cli_cfg.save(&cli_cfg_dir).unwrap();

        let mut child = susi_command(root, &cli_home, &["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let (out_tx, out_rx) = std::sync::mpsc::channel::<Value>();
        let out_handle = std::thread::spawn(move || {
            let mut frames = Vec::new();
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let parsed: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("corrupt stdout line {line:?}: {e}"));
                let _ = out_tx.send(parsed.clone());
                frames.push(parsed);
            }
            frames
        });
        let err_handle = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = BufReader::new(stderr).read_to_string(&mut s);
            s
        });

        stdin.write_all(br#"{"jsonrpc":"2.0","id":1,"#).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":2,\0}\n")
            .unwrap();
        let over_line = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}\n",
            "x".repeat(4500)
        );
        stdin.write_all(over_line.as_bytes()).unwrap();
        let recover_init = json!({ "jsonrpc": "2.0", "id": "rec-init", "method": "initialize", "params": {"protocolVersion": "2026-07-28", "capabilities": {}, "clientInfo": {"name": "cli-rec", "version": "1"}}});
        stdin
            .write_all(recover_init.to_string().as_bytes())
            .unwrap();
        stdin
            .write_all(b"\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .unwrap();
        stdin.flush().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let rem = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("timed out waiting for CLI rec-init response");
            let frame = out_rx
                .recv_timeout(rem)
                .expect("CLI stdout closed before rec-init response");
            if frame["id"] == "rec-init" {
                assert!(
                    frame["result"].is_object(),
                    "rec-init must succeed after bad frames: {frame}"
                );
                break;
            }
        }
        let recover_call = json!({ "jsonrpc": "2.0", "id": "rec-call", "method": "tools/call", "params": {"name": "ledger_verify", "arguments": {}, "_meta": modern_meta("rec-call")}});
        stdin
            .write_all(recover_call.to_string().as_bytes())
            .unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
        loop {
            let rem = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("timed out waiting for CLI rec-call response");
            let frame = out_rx
                .recv_timeout(rem)
                .expect("CLI stdout closed before rec-call response");
            if frame["id"] == "rec-call" {
                break;
            }
        }
        drop(stdin);

        let status = child.wait().unwrap();
        assert!(status.success(), "susi mcp serve must exit cleanly on EOF");
        let frames = out_handle.join().unwrap();
        let stderr_text = err_handle.join().unwrap();
        assert!(
            frames.iter().any(|f| f["error"]["code"] == -32700),
            "truncated/nul input must yield -32700: {frames:?}"
        );
        assert!(
            frames.iter().any(|f| f["error"]["code"] == -32600),
            "oversized line must yield -32600: {frames:?}"
        );
        let rec = frames
            .iter()
            .find(|f| f["id"] == "rec-call")
            .expect("follow-up ledger_verify must succeed after bad frames");
        assert_eq!(call_tool_report(&rec["result"])["verified"], true);
        assert!(
            !stderr_text.contains("panicked"),
            "unexpected panic on stderr: {stderr_text}"
        );
        assert!(
            !stderr_text.contains(token),
            "stderr leaked secret token: {stderr_text}"
        );
    }

    // ── G. Normative Ledger & Contract Binding (VC-203-012) ──────────────────
    let ledger: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    )
    .unwrap();
    let rows_012: Vec<&Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["trace"]["vector"] == "VC-203-012")
        .collect();
    assert_eq!(rows_012.len(), 57);
    for row in rows_012 {
        assert_eq!(
            row["trace"]["negative_test"],
            "tests/gmcp_requirements.rs::gmcp_req_203_012_04_adversarial"
        );
    }

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-012.md")).unwrap();
    for token_str in [
        "VC-203-012",
        "gmcp_req_203_012_04_adversarial",
        "F-NEG-012-01",
        "F-NEG-012-06",
    ] {
        assert!(
            contract.contains(token_str),
            "contract must document {token_str}"
        );
    }

    let _ = std::fs::remove_dir_all(&scratch);
}
