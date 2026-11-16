/// GMCP-203-020-06: operations, inspectable CLI/API state, configuration
/// validation, bounded metrics/logs/traces, diagnostics, migration & rollback
/// guidance, incident runbooks, and deterministic drift detection for the GMCP
/// server authoring SDK and plugin host lifecycle.
#[test]
fn gmcp_req_203_020_06_operations() {
    use serde_json::{json, Map, Value};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    use susi_gmcp::extensions::{ExtensionStatus, OFFICIAL_EXTENSIONS};
    use susi_gmcp::mcp::model::{
        CallToolResult, ClientCapabilities, ContentBlock, ReadResourceResult, Resource,
        ResourceContents, ResultType, Tool,
    };
    use susi_gmcp::protocol::{GmcpService, MAX_RESOURCE_BYTES};
    use susi_gmcp::sdk::{
        examples::{production_reference_server, sample_plugin_bundle},
        CatalogUpdate, PluginBundle, PluginError, PluginPolicy, ServerBuilder, TestHarness,
    };
    use susi_gmcp::susi_config::extensions as pack_ext;
    use susi_gmcp::susi_config::validate::{validate_dir, ConfigIssueKind};

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

    let (_lock, _env, home) = gmcp_http_home_guard("020-ops");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    pack_ext::invalidate_extension_caches();

    // ── 1. Inspectable CLI, API snapshot & live SDK server parity ────────
    let report = ledger_report(root);
    assert_eq!(report["verified"], true, "{report}");
    assert_eq!(ledger_tool_report(root), report);
    assert_eq!(susi_json(root, &home, &["mcp", "ledger"]), report);

    let mcp_status = susi_json(root, &home, &["mcp", "status"]);
    assert!(mcp_status.is_array(), "{mcp_status}");
    assert_eq!(susi_json(root, &home, &["mcp", "list"]), mcp_status);

    let cli_ext_status = susi_json(root, &home, &["extensions", "status"]);
    let tool_ext_status: Value = serde_json::from_str(
        &susi_gmcp::tools::CoreTools::extensions_status(&json!({}), root).unwrap(),
    )
    .unwrap();
    assert_eq!(cli_ext_status, tool_ext_status);
    assert_eq!(cli_ext_status["active"], pack_ext::DEFAULT_PACK_ID);

    let cli_ext_list = susi_json(root, &home, &["extensions", "list"]);
    let tool_ext_list: Value = serde_json::from_str(
        &susi_gmcp::tools::CoreTools::extensions_list(&json!({}), root).unwrap(),
    )
    .unwrap();
    assert_eq!(cli_ext_list, tool_ext_list);

    // Runnable production example `06_production_plugin_server` executes in-process
    // and emits inspectable `activeSnapshot` operational state without seeding ~/.susi.
    let ex06_val = sdk_ex_06::run(&scratch.join("ex06-ops")).expect("sdk_ex_06 must succeed");
    assert_eq!(ex06_val["ok"], json!(true), "{ex06_val}");
    assert_eq!(
        ex06_val["activeSnapshot"]["plugins"][0]["id"],
        json!("sdk-sample-plugin")
    );
    assert_eq!(
        susi_json(root, &home, &["extensions", "status"])["active"],
        pack_ext::DEFAULT_PACK_ID
    );

    // Assemble SDK production server + checksummed sample plugin and verify
    // `ops_snapshot()`, `discover_result()`, `ExtensionRegistry::watch`, CLI
    // `susi mcp tools` / `susi mcp call`, direct-call, and Wire SDK parity.
    let sdk_ws = scratch.join("sdk-ws");
    let sdk_tasks = scratch.join("sdk-tasks");
    let plugin_pack = scratch.join("sdk-plugin-pack");
    std::fs::create_dir_all(&sdk_ws).unwrap();
    std::fs::create_dir_all(plugin_pack.join("catalog")).unwrap();
    let plugin_art = br#"{"tools":["plugin_lookup","plugin_watch"]}"#;
    std::fs::write(plugin_pack.join("catalog/plugin.json"), plugin_art).unwrap();
    let plugin_sha = susi_paths::hex::encode(Sha256::digest(plugin_art));
    let files = BTreeMap::from([("catalog".to_string(), "catalog/plugin.json".to_string())]);
    let csums = BTreeMap::from([("catalog".to_string(), format!("sha256:{plugin_sha}"))]);
    let bundle = sample_plugin_bundle(Some(plugin_pack.clone()), csums, files);
    std::fs::write(
        plugin_pack.join("manifest.json"),
        serde_json::to_vec_pretty(&bundle.manifest).unwrap(),
    )
    .unwrap();

    // Read-only `susi extensions validate` inspects the plugin pack without mutating host state.
    let pack_arg = plugin_pack.to_str().unwrap();
    let cli_val = susi_json(root, &home, &["extensions", "validate", pack_arg]);
    let tool_val: Value = serde_json::from_str(
        &susi_gmcp::tools::CoreTools::extensions_validate(&json!({"path": pack_arg}), root)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cli_val, tool_val);
    assert_eq!(cli_val["valid"], json!(true));
    assert_eq!(cli_val["id"], json!("sdk-sample-plugin"));
    assert_eq!(
        susi_json(root, &home, &["extensions", "status"])["active"],
        pack_ext::DEFAULT_PACK_ID
    );

    let (sdk_svc, audit_log) =
        production_reference_server(sdk_ws.clone(), sdk_tasks.clone()).expect("sdk server builds");
    let pre_snap = sdk_svc.ops_snapshot();
    assert_eq!(pre_snap["serverInfo"]["name"], json!("sdk-production-ref"));
    assert_eq!(pre_snap["includeHostTools"], json!(false));
    assert_eq!(pre_snap["middlewareCount"], json!(2));
    assert_eq!(pre_snap["catalog"]["hasResourceGuard"], json!(true));
    assert_eq!(pre_snap["plugins"], json!([]));

    let policy = PluginPolicy::strict().grant_permission("filesystem.read");
    let loaded = sdk_svc
        .attach_plugin(bundle, &policy)
        .expect("plugin attach");
    assert_eq!(loaded.id, "sdk-sample-plugin");
    let post_snap = sdk_svc.ops_snapshot();
    assert_eq!(post_snap["plugins"][0]["id"], json!("sdk-sample-plugin"));
    assert_eq!(post_snap["plugins"][0]["apiVersion"], json!("1.0"));
    assert!(post_snap["catalog"]["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("plugin_lookup")));
    assert!(post_snap["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == "io.susi.plugin/sample" && row["enabled"] == true));
    let watched = sdk_svc.extensions().read().watch(OFFICIAL_EXTENSIONS);
    assert!(watched
        .iter()
        .all(|item| item.starts_with("[GMCP] Adopt newly published official extension ")));

    sdk_svc
        .bind_direct_server("sdk-020-06-direct")
        .expect("bind direct server");

    let token = "susi-020-ops-bearer-secret";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind SDK HTTP /mcp port");
    let port = listener.local_addr().unwrap().port();
    let http_addr = format!("127.0.0.1:{port}");
    let http_svc = sdk_svc.clone();
    let _http_srv = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::serve_http_service(http_svc, vec![listener], None, false);
    });
    for _ in 0..40 {
        if TcpStream::connect(&http_addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // `susi mcp tools` and `susi mcp call` inspect and invoke the live SDK HTTP server.
    let tools_out = susi_command(root, &home, &["mcp", "tools", &http_addr, "--token", token])
        .output()
        .expect("susi mcp tools must run");
    assert!(
        tools_out.status.success(),
        "susi mcp tools failed: {}",
        String::from_utf8_lossy(&tools_out.stderr)
    );
    let tools_stdout = String::from_utf8_lossy(&tools_out.stdout);
    for expected_tool in [
        "summarize_items",
        "confirm_action",
        "approval_workflow",
        "plugin_lookup",
    ] {
        assert!(
            tools_stdout.contains(expected_tool),
            "susi mcp tools missing `{expected_tool}`: {tools_stdout}"
        );
    }

    let cli_call = susi_json(
        root,
        &home,
        &[
            "mcp",
            "call",
            &http_addr,
            "summarize_items",
            r#"{"items":["alpha","beta","gamma"],"scope":{"namespace":"ops"},"maxItems":2}"#,
            "--token",
            token,
        ],
    );
    assert_eq!(
        cli_call["structuredContent"],
        json!({"count": 2, "summary": "alpha, beta", "namespace": "ops"})
    );
    assert!(audit_log
        .read()
        .contains(&"mw1:before:summarize_items".to_string()));

    let cli_plugin_call = susi_json(
        root,
        &home,
        &[
            "mcp",
            "call",
            &http_addr,
            "plugin_lookup",
            r#"{"key":"cli-probe"}"#,
            "--token",
            token,
        ],
    );
    assert_eq!(
        cli_plugin_call["content"][0]["text"],
        json!("plugin:cli-probe")
    );

    let direct_out = susi_gmcp::direct_call::call_tool_direct(
        "sdk-020-06-direct",
        "plugin_lookup",
        &json!({"key": "direct-probe"}),
        &sdk_ws,
    )
    .expect("direct call must succeed");
    assert_eq!(direct_out["isError"], json!(false));
    assert_eq!(
        direct_out["content"][0]["text"],
        json!("plugin:direct-probe")
    );

    // `server/discover` matches across direct API, Wire SDK 2026-07-28, and stdio.
    let direct_disc = serde_json::to_value(sdk_svc.discover_result()).unwrap();
    let wire_2026 = susi_wire_client::mcp::Session::open_2026_07_28(
        &http_addr,
        Some(token),
        Duration::from_secs(15),
    )
    .expect("open 2026-07-28 session");
    assert_eq!(
        Value::Object(wire_2026.server_extensions().unwrap().clone()),
        direct_disc["capabilities"]["extensions"]
    );
    wire_2026.close().expect("close 2026 session");

    let mut stdio = StdioSession::spawn_raw(root, &home);
    let stdio_disc = stdio
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "ops020-stdio", "method": "server/discover", "params": adapter_modern_meta()}),
            Duration::from_secs(60),
        )
        .expect("stdio server/discover");
    assert!(stdio_disc["result"]["capabilities"]["extensions"]["io.modelcontextprotocol/tasks"]
        .is_object());
    drop(stdio);

    // ── 2. Configuration validation & fail-closed startup/pack diagnostics ─
    let bad_flag = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi mcp ledger must run");
    assert!(!bad_flag.status.success());

    let cfg_ok = scratch.join("cfg-ok/.susi");
    std::fs::create_dir_all(&cfg_ok).unwrap();
    std::fs::write(cfg_ok.join("config.json"), r#"{"max_rpc_body_bytes": 4096}"#).unwrap();
    assert!(!validate_dir(&cfg_ok).unwrap().has_blocking());

    let cfg_bad = scratch.join("cfg-bad/.susi");
    std::fs::create_dir_all(&cfg_bad).unwrap();
    std::fs::write(
        cfg_bad.join("config.json"),
        r#"{"max_rpc_body_bytes": "invalid"}"#,
    )
    .unwrap();
    let bad_issues = validate_dir(&cfg_bad).unwrap();
    assert!(bad_issues.has_blocking());
    assert!(bad_issues
        .issues
        .iter()
        .any(|i| i.path == "max_rpc_body_bytes" && i.kind == ConfigIssueKind::TypeMismatch));

    let zero_home = scratch.join("cfg-zero");
    std::fs::create_dir_all(zero_home.join(".susi")).unwrap();
    std::fs::write(
        zero_home.join(".susi/config.json"),
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
            json!({"jsonrpc":"2.0","id":"z1","method":"ping","params":adapter_modern_meta()})
        )
        .unwrap();
    }
    drop(zero_child.stdin.take());
    let zero_out = zero_child.wait_with_output().unwrap();
    assert!(
        zero_out.stdout.is_empty(),
        "zero max_rpc_body_bytes must emit zero stdout bytes"
    );
    let zero_stderr = String::from_utf8_lossy(&zero_out.stderr);
    assert!(
        zero_stderr.contains("max_rpc_body_bytes") && zero_stderr.contains("must be > 0"),
        "unexpected stderr: {zero_stderr}"
    );

    for (label, manifest_doc) in [
        (
            "unknown-perm",
            json!({"id":"bad-perm","name":"Bad","version":"1.0.0","apiVersion":"1","permissions":["shell.root"]}),
        ),
        (
            "bad-api-major",
            json!({"id":"bad-api","name":"Bad","version":"1.0.0","apiVersion":"2.0"}),
        ),
        (
            "escape-no-perm",
            json!({"id":"bad-esc","name":"Bad","version":"1.0.0","apiVersion":"1","files":{"f":"../secret.json"}}),
        ),
    ] {
        let dir = scratch.join(format!("invalid-{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("manifest.json"), manifest_doc.to_string()).unwrap();
        let out = susi_command(root, &home, &["extensions", "validate", dir.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{label} must fail closed");
        assert!(PluginBundle::from_pack_dir(&dir).is_err());
    }

    // Corrupt durable task store JSON fails `GmcpService::open_durable` closed.
    let corrupt_tasks = scratch.join("corrupt-tasks");
    std::fs::create_dir_all(&corrupt_tasks).unwrap();
    std::fs::write(corrupt_tasks.join("tasks.json"), b"{corrupt-json").unwrap();
    assert!(GmcpService::open_durable(sdk_ws.clone(), corrupt_tasks).is_err());

    // Remote `$ref` tool schema fails `ServerBuilder::build` closed.
    let bad_schema_srv = ServerBuilder::new("bad-schema", "0.1.0", &sdk_ws).tool(
        Tool::new_with_raw(
            "remote_ref_tool",
            None,
            serde_json::from_value::<susi_gmcp::mcp::model::JsonObject>(json!({ "type": "object", "properties": { "x": { "$ref": "https://evil.example/schema.json"}}}))
            .unwrap(),
        ),
        Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) })),
    );
    assert!(bad_schema_srv.build().is_err());

    // ── 3. Bounded metrics, logs, W3C traceparent & resource ceilings ────
    let mut deep = json!({"leaf": 1});
    for _ in 0..70 {
        deep = json!({"nested": deep});
    }
    assert_eq!(
        CanonicalContentBlock::from_wire(&json!({"type": "text", "text": "x", "vendor.ops/d": deep})),
        Err(ContentError::TooLarge)
    );

    // Oversized resource read (> `MAX_RESOURCE_BYTES = 8 MiB`) refuses fail-closed without truncation,
    // and oversized `BoundedLines` frame fails with `-32600` while keeping the harness alive.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let bound_svc = ServerBuilder::new("sdk-bounds-probe", "0.1.0", &sdk_ws)
            .resource(
                Resource::new("memo://oversized", "oversized"),
                Arc::new(|req, _ctx| {
                    Box::pin(async move {
                        let huge = "a".repeat(MAX_RESOURCE_BYTES + 1);
                        Ok(ReadResourceResult {
                            result_type: Some(ResultType::COMPLETE),
                            ttl_ms: Some(0),
                            cache_scope: None,
                            contents: vec![ResourceContents::TextResourceContents {
                                uri: req.uri,
                                mime_type: Some("text/plain".into()),
                                text: huge,
                                meta: None,
                            }],
                            meta: None,
                        }
                        .into())
                    })
                }),
            )
            .build()
            .unwrap();
        let h = TestHarness::spawn_with_limit(bound_svc, 512);
        let over_res = h
            .request_modern(
                "resources/read",
                json!({"uri": "memo://oversized"}),
                ClientCapabilities::default(),
            )
            .await
            .unwrap();
        assert!(
            over_res["error"].is_object(),
            "oversized resource must fail closed, never truncate: {over_res}"
        );
        let over_line = h
            .request_raw(json!({ "jsonrpc": "2.0", "id": null, "method": "ping", "params": { "pad": "x".repeat(700), "_meta": adapter_modern_meta()["_meta"]}}))
            .await
            .unwrap();
        assert_eq!(over_line["error"]["code"], json!(-32600), "{over_line}");
        let post_ping = h
            .request_modern("server/discover", json!({}), ClientCapabilities::default())
            .await
            .unwrap();
        assert!(post_ping["result"].is_object(), "{post_ping}");
        h.shutdown().await;
    });

    // HTTP `/mcp` on the SDK-authored server: W3C `traceparent` + `logLevel=info`
    // emits `notifications/message` without leaking Bearer credentials, while
    // omitting `logLevel` suppresses `notifications/message`.
    let http_post = |extra_headers: &[(&str, &str)], body_val: &Value| -> (String, String, Vec<Value>) {
        let body = body_val.to_string();
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\nHost: {http_addr}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\n"
        );
        for (k, v) in extra_headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        let mut stream = TcpStream::connect(&http_addr).expect("connect SDK HTTP /mcp");
        stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
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
    trace_meta["io.modelcontextprotocol/requestId"] = json!("ops020-http-info");
    trace_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    trace_meta["vendor.ops/context"] = json!({"secret": token});
    let info_hdrs = [
        ("MCP-Protocol-Version", "2026-07-28"),
        ("Mcp-Method", "tools/call"),
        ("Mcp-Name", "summarize_items"),
        ("traceparent", "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
    ];
    let info_body = json!({"jsonrpc":"2.0","id":"ops020-http-info","method":"tools/call","params":{"name":"summarize_items","arguments":{"items":["x"],"scope":{"namespace":"trace"}},"_meta":trace_meta}});
    let (http_st, http_raw, http_frames) = http_post(&info_hdrs, &info_body);
    assert!(http_st.starts_with("HTTP/1.1 200"), "{http_st} {http_raw}");
    assert!(http_frames.iter().any(|f| f["id"] == "ops020-http-info"));
    assert!(http_frames.iter().any(|f| f["method"] == "notifications/message"));
    assert!(!http_raw.contains(token), "SSE telemetry must never leak Bearer token");

    let mut quiet_meta = adapter_modern_meta()["_meta"].clone();
    quiet_meta["io.modelcontextprotocol/requestId"] = json!("ops020-http-quiet");
    let quiet_body = json!({"jsonrpc":"2.0","id":"ops020-http-quiet","method":"tools/call","params":{"name":"summarize_items","arguments":{"items":["x"],"scope":{"namespace":"quiet"}},"_meta":quiet_meta}});
    let (_, quiet_raw, quiet_frames) = http_post(&info_hdrs[..3], &quiet_body);
    assert!(quiet_frames.iter().all(|f| f["method"] != "notifications/message"), "{quiet_raw}");

    // ── 4. Live migration, catalog rollback & host pack quarantine/unload ─
    let empty_obj: susi_gmcp::mcp::model::JsonObject =
        serde_json::from_value(json!({"type": "object"})).unwrap();
    let v2_update = CatalogUpdate::new().with_tool(
        Tool::new_with_raw("migrated_v2_tool", Some("v2".into()), empty_obj.clone()),
        Arc::new(|_p, _c| {
            Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("v2")]).into()) })
        }),
    );
    sdk_svc.reload_catalog(v2_update).expect("reload to v2");
    let snap_v2 = sdk_svc.ops_snapshot();
    assert!(snap_v2["catalog"]["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("migrated_v2_tool")));
    assert!(
        snap_v2["catalog"]["tools"]
            .as_array()
            .unwrap()
            .contains(&json!("plugin_lookup")),
        "reload_catalog must preserve active plugin registrations"
    );

    // Roll back to v1 snapshot (`v1_rollback_tool`) and verify clean plugin unload.
    let v1_rollback = CatalogUpdate::new().with_tool(
        Tool::new_with_raw("v1_rollback_tool", Some("v1".into()), empty_obj.clone()),
        Arc::new(|_p, _c| {
            Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("v1")]).into()) })
        }),
    );
    sdk_svc
        .reload_catalog(v1_rollback)
        .expect("rollback catalog to v1");
    assert!(!sdk_svc.ops_snapshot()["catalog"]["tools"]
        .as_array()
        .unwrap()
        .contains(&json!("migrated_v2_tool")));
    let unload_rep = sdk_svc
        .unload_plugin("sdk-sample-plugin")
        .expect("unload sample plugin");
    assert_eq!(unload_rep.removed_tools, 2);
    assert_eq!(sdk_svc.ops_snapshot()["plugins"], json!([]));

    // Host pack load -> on-disk tamper -> `sync_active_pack` drift eviction -> CLI unload rollback.
    susi_gmcp::tools::CoreTools::extensions_create(
        &json!({"id": "ops-managed-pack", "name": "Ops Managed Pack"}),
        root,
    )
    .expect("extensions_create");
    let managed_dir = cfg_dir.join("extensions/ops-managed-pack");
    let managed_rule = managed_dir.join("rules.json");
    let rule_bytes = br#"{"mode":"strict"}"#;
    std::fs::write(&managed_rule, rule_bytes).unwrap();
    let rule_sha = susi_paths::hex::encode(Sha256::digest(rule_bytes));
    std::fs::write(
        managed_dir.join("manifest.json"),
        json!({ "id": "ops-managed-pack", "name": "Ops Managed Pack", "version": "1.0.0", "apiVersion": "1", "permissions": ["filesystem.read"], "files": {"rules": "rules.json"}, "checksums": {"rules": format!("sha256:{rule_sha}")}})
        .to_string(),
    )
    .unwrap();
    let load_cli = susi_json(root, &home, &["extensions", "load", "ops-managed-pack"]);
    assert_eq!(load_cli["active"], json!("ops-managed-pack"));
    pack_ext::invalidate_extension_caches();
    let sync_rep = sdk_svc
        .sync_active_pack(&policy, |b| {
            b.with_tool(
                Tool::new_with_raw("managed_pack_ping", None, empty_obj.clone()),
                Arc::new(|_p, _c| {
                    Box::pin(async {
                        Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into())
                    })
                }),
            )
            .with_extension(
                "io.susi.ops/managed",
                ExtensionStatus::Experimental,
                1,
                Map::new(),
            )
        })
        .expect("sync_active_pack must attach ops-managed-pack");
    assert_eq!(sync_rep.unwrap().id, "ops-managed-pack");
    assert_eq!(
        sdk_svc.ops_snapshot()["plugins"][0]["hostSynced"],
        json!(true)
    );

    // Mutating the managed artifact on disk invalidates proof immediately with `IntegrityFailed`
    // and evicts the compromised plugin from `ops_snapshot()`.
    std::fs::write(&managed_rule, br#"{"mode":"tampered"}"#).unwrap();
    let drift_err = sdk_svc
        .sync_active_pack(&policy, |b| b)
        .expect_err("tampered pack must fail sync_active_pack closed");
    assert!(
        matches!(drift_err, PluginError::IntegrityFailed(_)),
        "expected IntegrityFailed, got: {drift_err}"
    );
    assert_eq!(
        sdk_svc.ops_snapshot()["plugins"],
        json!([]),
        "compromised host pack must be evicted immediately"
    );

    let unload_cli = susi_json(root, &home, &["extensions", "unload", "ops-managed-pack"]);
    assert_eq!(unload_cli["active"], json!(pack_ext::DEFAULT_PACK_ID));

    // ── 5. Pinned VC-203-020 specs & automated standards drift detection ─
    let sources: Value =
        serde_json::from_slice(&std::fs::read(root.join(".agents/gmcp/sources.json")).unwrap()).unwrap();
    let pinned_020_docs = "schema/2026-07-28/schema.json|schema/2026-07-28/schema.ts|schema/2025-11-25/schema.json|docs/specification/2026-07-28/server/tools.mdx|docs/specification/2026-07-28/server/resources.mdx|docs/specification/2026-07-28/server/prompts.mdx|docs/specification/2026-07-28/server/discover.mdx|docs/specification/2026-07-28/server/utilities/completion.mdx|docs/specification/2026-07-28/basic/patterns/subscriptions.mdx|docs/specification/2026-07-28/basic/patterns/mrtr.mdx|docs/docs/2026-07-28/tutorials/security/local-server-security.mdx|ext-tasks/specification/2026-07-28/tasks.md|ext-apps/specification/2026-01-26/apps.mdx|ext-auth/specification/stable/enterprise-managed-authorization.mdx";
    for doc in pinned_020_docs.split('|') {
        let entry = sources["files"].as_array().unwrap().iter().find(|f| f["document"] == doc).unwrap();
        let disk_sha = susi_paths::hex::encode(Sha256::digest(std::fs::read(root.join(".agents/gmcp/spec").join(doc)).unwrap()));
        assert_eq!(entry["sha256"].as_str(), Some(disk_sha.as_str()), "{doc}");
    }

    let refetched = scratch.join("refetched-spec");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let refetched_arg = refetched.to_str().unwrap();
    let drift_args = ["mcp", "ledger", "--drift", "--refetched", refetched_arg];
    let clean_a = susi_json(root, &home, &drift_args);
    assert!(clean_a == susi_json(root, &home, &drift_args) && clean_a["verified"] == json!(true));
    assert!(clean_a["findings"].as_array().unwrap().is_empty());

    let victims = "docs/specification/2026-07-28/server/tools.mdx|docs/specification/2026-07-28/basic/patterns/mrtr.mdx|docs/docs/2026-07-28/tutorials/security/local-server-security.mdx|ext-tasks/specification/2026-07-28/tasks.md|schema/2026-07-28/schema.json";
    for victim_rel in victims.split('|') {
        let victim = refetched.join(victim_rel);
        let original = std::fs::read(&victim).unwrap();
        let mut altered = original.clone();
        altered.extend_from_slice(b"\n<!-- drifted vc-203-020 spec -->\n");
        std::fs::write(&victim, &altered).unwrap();

        let drifted = susi_command(root, &home, &drift_args).output().unwrap();
        assert!(!drifted.status.success(), "{victim_rel}: drift must fail");
        let drift_report: Value = serde_json::from_slice(&drifted.stdout).unwrap();
        assert_eq!(drift_report["verified"], json!(false));
        assert!(drift_report["findings"].as_array().unwrap().iter().any(|f| f["document"] == victim_rel && f["kind"] == "hash_drift"));
        std::fs::write(&victim, &original).unwrap();
    }
    assert_eq!(susi_json(root, &home, &drift_args), clean_a);

    let missing_rel = "docs/specification/2026-07-28/server/discover.mdx";
    std::fs::remove_file(refetched.join(missing_rel)).unwrap();
    let missing_out = susi_command(root, &home, &drift_args).output().unwrap();
    assert!(!missing_out.status.success());
    let missing_report: Value = serde_json::from_slice(&missing_out.stdout).unwrap();
    assert_eq!(missing_report["verified"], json!(false));
    assert!(missing_report["findings"].as_array().unwrap().iter().any(|f| f["document"] == missing_rel && f["kind"] == "unfetchable"));

    // Mutating a vendored standard in a repo copy invalidates `verify_pinned_ledger`.
    let tampered_repo = scratch.join("tampered-repo");
    std::fs::create_dir_all(tampered_repo.join("docs")).unwrap();
    std::fs::create_dir_all(tampered_repo.join(".agents/gmcp")).unwrap();
    copy_tree(&root.join(".agents/gmcp/spec"), &tampered_repo.join(".agents/gmcp/spec"));
    for rel in [".agents/gmcp/requirements-ledger.json", ".agents/gmcp/sources.json", "docs/gmcp-roadmap.md"] {
        std::fs::copy(root.join(rel), tampered_repo.join(rel)).unwrap();
    }
    let tampered_tools = tampered_repo.join(".agents/gmcp/spec/docs/specification/2026-07-28/server/tools.mdx");
    let mut t_bytes = std::fs::read(&tampered_tools).unwrap();
    t_bytes.push(b'\n');
    std::fs::write(&tampered_tools, t_bytes).unwrap();
    assert!(susi_gmcp::requirements_ledger::verify_pinned_ledger(&tampered_repo).is_err());

    // ── 6. Runbook bindings & reproducible append-only evidence checks ───
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-020.md")).unwrap();
    let clauses = "GMCP-203-020-01..06|## Operations and non-claims|## Operations and incident runbook|### Inspectable state|### Configuration validation|### Bounded metrics, logs and traces|### Diagnostics and incident triage|### Examples|### Incident triage|### Migration (SDK authoring, plugin API major upgrades, and re-pinning upstream specs)|### Rollback|### Production guarantees and non-claims|GmcpService::ops_snapshot()|SDK-001|SDK-018|HOST_PACK_API_MAJOR = 1|MAX_DEPTH = 64|MAX_NODES = 10_000|MAX_RESOURCE_BYTES = 8 MiB|DEFAULT_MAX_LINE_LENGTH = 16777216|DEFAULT_MAX_REQUEST_BODY_BYTES = 4194304|IntegrityFailed|PermissionDenied|IncompatibleCapability|SchemaRejected|hash_drift|unfetchable";
    for required in clauses.split('|') {
        assert!(contract.contains(required), "missing `{required}`");
    }

    let ledger_sha = susi_paths::hex::encode(Sha256::digest(std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap()));
    let ev_dir = root.join(".agents/gmcp/evidence");
    let mut ev_count = 0usize;
    for entry in std::fs::read_dir(&ev_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            ev_count += 1;
            let records = newest_records(&std::fs::read_to_string(&path).unwrap());
            assert!(!records.is_empty() && records.iter().all(|r| r["exit"] == 0 && r["artifact_sha256"] == ledger_sha));
        }
    }
    assert!(ev_count >= 16 && ev_dir.join("VC-203-020.jsonl").is_file() && root.join(".agents/roadmap-verdicts/VC-203-020.json").is_file());

    let disk_bytes = adapter_tree_bytes(&sdk_tasks);
    assert!(
        disk_bytes <= 256 * 1024,
        "durable task store exceeded 256 KiB ({disk_bytes})"
    );
    GmcpService::unbind_direct_server("sdk-020-06-direct");
}

/// GMCP-203-020-M: Vector mastery gate for VC-203-020 (GMCP server authoring SDK and plugin lifecycle).
#[test]
fn gmcp_vc_203_020_mastery() {
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::time::Duration;
    use susi_gmcp::direct_call::call_tool_direct;
    use susi_gmcp::mcp::model::{CallToolResult, ClientCapabilities, ContentBlock, Tool};
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::sdk::{
        examples::{production_reference_server, sample_plugin_bundle},
        CatalogUpdate, PluginError, PluginPolicy, TestHarness,
    };
    use susi_gmcp::server::GmcpServer;
    use susi_gmcp::susi_config::extensions::{self as pack_ext, DEFAULT_PACK_ID};
    use susi_wire_client::mcp::{CallToolOutcome, Session, TaskStatus as ClientTaskStatus};

    struct EvidenceCtx<'a> {
        root: &'a Path,
        ledger_hash: &'a str,
        test_src: &'a str,
        head_digest: &'a str,
        now: u64,
    }

    fn verify_vc_203_020_evidence(
        ctx: &EvidenceCtx<'_>,
        evidence_text: &str,
        committed_artifacts: Option<&[String]>,
    ) -> Result<BTreeSet<String>, String> {
        const LINK: [&str; 3] = ["recorded_unix", "identity", "artifact_sha256"];
        let names = |r: &Value, t: &Value| LINK.iter().all(|f| r["supersedes"][*f] == t[*f]);
        let mut order: Vec<String> = Vec::new();
        let mut chains: std::collections::HashMap<String, Vec<Value>> = Default::default();
        for line in evidence_text.lines().filter(|l| !l.trim().is_empty()) {
            let rec: Value = serde_json::from_str(line).map_err(|e| format!("bad JSON: {e}"))?;
            let req = rec["requirement"].as_str().unwrap_or_default().to_string();
            match chains.get_mut(&req) {
                Some(ch) => {
                    if !ch.iter().any(|old| names(&rec, old)) {
                        return Err(format!("{req}: missing supersedes link"));
                    }
                    ch.push(rec);
                }
                None => {
                    order.push(req.clone());
                    chains.insert(req, vec![rec]);
                }
            }
        }
        let newest: Vec<Value> = order
            .into_iter()
            .filter_map(|req| {
                let ch = chains.remove(&req)?;
                ch.iter()
                    .filter(|r| !ch.iter().any(|later| names(later, r)))
                    .max_by_key(|r| r["recorded_unix"].as_u64().unwrap_or(0))
                    .cloned()
            })
            .collect();

        let mut covered = BTreeSet::new();
        let req_fields = "requirement|check|exit|detail|recorded_unix|artifact_sha256|identity";
        for rec in newest {
            let req = rec["requirement"].as_str().unwrap_or_default();
            if req.ends_with("-M") {
                continue;
            }
            if rec.get("waived").is_some() || rec.get("mock").is_some() {
                return Err(format!("{req}: waived or mock evidence rejected"));
            }
            if req_fields.split('|').any(|f| rec[f].is_null()) {
                return Err(format!("{req}: missing required field"));
            }
            if rec["exit"] != 0
                || rec["identity"].as_str().unwrap_or_default().is_empty()
                || rec["detail"].as_str().unwrap_or_default().is_empty()
                || rec["artifact_sha256"].as_str().unwrap_or_default() != ctx.ledger_hash
            {
                return Err(format!("{req}: invalid exit/identity/detail/ledger_hash"));
            }
            let ts = rec["recorded_unix"].as_u64().unwrap_or(0);
            if ts <= 1_792_256_000 || ts > ctx.now + 60 {
                return Err(format!("{req}: stale or future recorded_unix {ts}"));
            }
            let check = rec["check"].as_str().unwrap_or_default();
            let Some(fn_name) =
                check.strip_prefix("cargo test --locked --test gmcp_requirements ")
            else {
                return Err(format!("{req}: unsupported check command"));
            };
            if !fn_name.starts_with("gmcp_")
                || fn_name.contains(char::is_whitespace)
                || !is_test_fn(ctx.test_src, fn_name)
            {
                return Err(format!("{req}: zero-test missing fn {fn_name}"));
            }
            if let Some(artifacts) = committed_artifacts {
                assert_bound_to_tested_commit(ctx.root, &rec, artifacts, ctx.head_digest);
            } else {
                let c = rec["tested_commit"].as_str().unwrap_or_default();
                if c.len() != 40
                    || !c.chars().all(|x| x.is_ascii_hexdigit())
                    || rec["tested_artifacts"].as_str() != Some(ctx.head_digest)
                {
                    return Err(format!("{req}: invalid commit/artifact binding"));
                }
            }
            covered.insert(req.to_string());
        }
        for n in 1..=6 {
            let req = format!("GMCP-203-020-0{n}");
            if !covered.contains(&req) {
                return Err(format!("missing evidence for {req}"));
            }
        }
        Ok(covered)
    }

    let (_lock, _env, home) = gmcp_http_home_guard("020-mastery");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&scratch).unwrap();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "tok-020-mastery-bearer";
    std::fs::write(cfg_dir.join("api_token"), format!("{token}\n")).unwrap();
    pack_ext::invalidate_extension_caches();

    // 1. Contract, ledger parity, zero-vendor boundary & schema derivation.
    let report = ledger_report(root);
    assert!(report["verified"] == true && susi_json(root, &home, &["mcp", "ledger"]) == report);
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-020.md")).unwrap();
    for tok in "Status: **delivered**|GMCP-203-020-01..06 + GMCP-203-020-M|SDK-001|SDK-018|## Operations and incident runbook".split('|') {
        assert!(contract.contains(tok), "missing `{tok}`");
    }
    let sdk_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/sdk.rs")).unwrap();
    for forbidden in ["rmcp::", "schemars::", "axum::", "tower::", "wasmtime::"] {
        assert!(!sdk_src.contains(forbidden), "vendor leak `{forbidden}`");
    }
    susi_gmcp::sdk_schema! {
        /// Mastery schema derivation check.
        struct MasteryInput {
            query: String,
            #[serde(default, rename = "maxResults")]
            max_results: u32,
            tag: Option<String>,
        }
    }
    let derived = Value::Object(susi_gmcp::mcp::schema::root::<MasteryInput>());
    assert_eq!(derived["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!(derived["properties"]["maxResults"]["default"], json!(0));

    // 2. All 6 runnable minimal-through-production examples (`01`..`06`) pass cleanly.
    assert_eq!(DEFAULT_PACK_ID, "default");
    for (name, res) in [
        ("01_minimal_tool", sdk_ex_01::run(&scratch.join("ex01"))),
        ("02_structured_middleware", sdk_ex_02::run(&scratch.join("ex02"))),
        ("03_resources_prompts_completions", sdk_ex_03::run(&scratch.join("ex03"))),
        ("04_mrtr_and_durable_tasks", sdk_ex_04::run(&scratch.join("ex04"))),
        ("05_hot_reload_and_subscriptions", sdk_ex_05::run(&scratch.join("ex05"))),
        ("06_production_plugin_server", sdk_ex_06::run(&scratch.join("ex06"))),
    ] {
        let out = res.unwrap_or_else(|e| panic!("{name} failed: {e}"));
        assert!(out["example"] == name && out["ok"] == true, "{out}");
    }
    assert!(!cfg_dir.join("extensions").exists());

    // 3. Full authoring SDK, transactional plugin lifecycle, cross-transport & hot-reload proof.
    let (sdk_ws, sdk_tasks) = (scratch.join("ws"), scratch.join("tasks"));
    std::fs::create_dir_all(&sdk_ws).unwrap();
    let (svc, audit_log) = production_reference_server(sdk_ws, sdk_tasks).unwrap();
    let pack_dir = scratch.join("signed-pack");
    std::fs::create_dir_all(pack_dir.join("catalog")).unwrap();
    let art = br#"{"tools":["plugin_lookup","plugin_watch"]}"#;
    std::fs::write(pack_dir.join("catalog/plugin.json"), art).unwrap();
    let sha = susi_paths::hex::encode(Sha256::digest(art));
    let files = BTreeMap::from([("catalog".into(), "catalog/plugin.json".into())]);
    let csums = BTreeMap::from([("catalog".into(), format!("sha256:{sha}"))]);
    let sk = SigningKey::from_bytes(&[17u8; 32]);
    let mk_bundle = || {
        let mut b = sample_plugin_bundle(Some(pack_dir.clone()), csums.clone(), files.clone());
        b.manifest.signer = Some(susi_paths::hex::encode(sk.verifying_key().to_bytes()));
        b.manifest.signatures.insert("catalog".into(), susi_paths::hex::encode(sk.sign(art).to_bytes()));
        b
    };
    std::fs::write(pack_dir.join("manifest.json"), serde_json::to_vec_pretty(&mk_bundle().manifest).unwrap()).unwrap();

    // Denied permission & colliding tool fail transactionally with zero partial catalog residue.
    assert!(matches!(svc.attach_plugin(mk_bundle(), &PluginPolicy::strict()), Err(PluginError::PermissionDenied(_, _))));
    let policy = PluginPolicy::strict().grant_permission("filesystem.read");
    let obj_schema: susi_gmcp::mcp::model::JsonObject = serde_json::from_value(json!({"type": "object"})).unwrap();
    let mut colliding = mk_bundle().with_tool(
        Tool::new_with_raw("summarize_items", None, obj_schema.clone()),
        Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) })),
    );
    colliding.manifest.id = "colliding-plugin".into();
    assert!(svc.attach_plugin(colliding, &policy).is_err() && svc.ops_snapshot()["plugins"] == json!([]));

    svc.attach_plugin(mk_bundle(), &policy).expect("attach signed plugin");
    svc.bind_direct_server("sdk-020-mastery").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let http_svc = svc.clone();
    let _http = std::thread::spawn(move || GmcpServer::serve_http_service(http_svc, vec![listener], None, false));
    for _ in 0..50 {
        if TcpStream::connect(&addr).is_ok() { break; }
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut w26 = Session::open_2026_07_28(&addr, Some(token), Duration::from_secs(15)).unwrap();
    let out26 = w26.call_tool("summarize_items", &json!({"items": ["a", "b"], "scope": {"namespace": "m"}}), None, &mut |_| {}).unwrap();
    assert_eq!(out26["structuredContent"]["count"], json!(2));
    assert!(audit_log.read().iter().any(|e| e.starts_with("mw1:before:")));
    let task_h = match w26.call_tool_with_tasks("approval_workflow", &json!({"ticket": "T-1"}), true, &mut |_| {}).unwrap() {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected Task outcome, got {other:?}"),
    };
    let done = w26.poll_task_until_done(&task_h, Some(Duration::from_secs(10)), |_| Ok(json!({"approve": {"decision": "approved"}}))).unwrap();
    assert_eq!(done.handle.status, ClientTaskStatus::Completed);
    let watch_h = match w26.call_tool_with_tasks("plugin_watch", &json!({"key": "watch"}), true, &mut |_| {}).unwrap() {
        CallToolOutcome::Task(h) => h,
        other => panic!("expected plugin_watch Task outcome, got {other:?}"),
    };

    let mut w25 = Session::open(&addr, Some(token), Duration::from_secs(15)).unwrap();
    assert_eq!(w25.call_tool("plugin_lookup", &json!({"key": "leg"}), None, &mut |_| {}).unwrap()["content"][0]["text"], json!("plugin:leg"));
    w25.close().unwrap();
    assert_eq!(call_tool_direct("sdk-020-mastery", "plugin_lookup", &json!({"key": "dir"}), root).unwrap()["content"][0]["text"], json!("plugin:dir"));

    let rt = adapter_runtime(&home);
    rt.block_on(async {
        let h = TestHarness::spawn(svc.clone());
        let caps: ClientCapabilities = serde_json::from_value(json!({"elicitation": {}, "roots": {}})).unwrap();
        let mrtr1 = h.request_modern("tools/call", json!({"name": "confirm_action", "arguments": {"operation": "ship"}}), caps.clone()).await.unwrap();
        assert_eq!(mrtr1["result"]["resultType"], "input_required");
        let mrtr2 = h.request_modern("tools/call", json!({"name": "confirm_action", "arguments": {"operation": "ship"}, "requestState": mrtr1["result"]["requestState"], "inputResponses": {"confirm": {"approved": true}}}), caps).await.unwrap();
        assert_eq!(mrtr2["result"]["resultType"], "complete");

        svc.reload_catalog(CatalogUpdate::new().with_tool(
            Tool::new_with_raw("hot_reloaded_tool", None, obj_schema.clone()),
            Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("hot")]).into()) })),
        )).unwrap();
        assert!(svc.ops_snapshot()["catalog"]["tools"].as_array().unwrap().contains(&json!("plugin_lookup")));

        let unload = svc.unload_plugin("sdk-sample-plugin").unwrap();
        assert!(unload.removed_tools == 2 && svc.ops_snapshot()["plugins"] == json!([]));
        h.shutdown().await;
    });
    for _ in 0..30 {
        if w26.get_task(&watch_h.task_id).unwrap().handle.status == ClientTaskStatus::Cancelled { break; }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(w26.get_task(&watch_h.task_id).unwrap().handle.status, ClientTaskStatus::Cancelled);
    w26.close().unwrap();
    GmcpService::unbind_direct_server("sdk-020-mastery");

    // 4. Strict 01..06 evidence verification & 11-case negative rejection matrix.
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap()));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let artifacts = evidence_artifacts(root, "VC-203-020");
    let head_digest = artifacts_digest(root, "HEAD", &artifacts).expect("HEAD artifacts digest");

    let valid_tests = [
        ("GMCP-203-020-01", "gmcp_req_203_020_01_contract"),
        ("GMCP-203-020-02", "gmcp_req_203_020_02_production"),
        ("GMCP-203-020-03", "gmcp_req_203_020_03_integration"),
        ("GMCP-203-020-04", "gmcp_req_203_020_04_adversarial"),
        ("GMCP-203-020-05", "gmcp_req_203_020_05_interop"),
        ("GMCP-203-020-06", "gmcp_req_203_020_06_operations"),
    ];
    let good_lines: Vec<Value> = valid_tests.iter().map(|(r, t)| json!({"requirement":r,"check":format!("cargo test --locked --test gmcp_requirements {t}"),"exit":0,"detail":"ok","recorded_unix":now,"artifact_sha256":ledger_hash,"identity":"AG2FEATURES","tested_commit":"0".repeat(40),"tested_artifacts":head_digest})).collect();
    let good_text = good_lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
    let ev_ctx = EvidenceCtx { root, ledger_hash: &ledger_hash, test_src: &test_src, head_digest: &head_digest, now };
    let check_ev = |txt: &str, committed: bool| verify_vc_203_020_evidence(&ev_ctx, txt, committed.then_some(artifacts.as_slice()));
    assert!(check_ev(&good_text, false).is_ok());
    assert!(check_ev(&good_lines[..5].iter().map(Value::to_string).collect::<Vec<_>>().join("\n"), false).is_err());

    type Mutate<'a> = Box<dyn Fn(&mut Value) + 'a>;
    let mutators: [Mutate<'_>; 9] = [
        Box::new(|v| v["recorded_unix"] = json!(1_700_000_000_u64)),
        Box::new(|v| v["recorded_unix"] = json!(now + 120)),
        Box::new(|v| v["artifact_sha256"] = json!("deadbeef")),
        Box::new(|v| v["tested_artifacts"] = json!("stale-digest")),
        Box::new(|v| v["identity"] = json!("")),
        Box::new(|v| v["exit"] = json!(1)),
        Box::new(|v| { v.as_object_mut().unwrap().insert("waived".into(), json!(true)); }),
        Box::new(|v| { v.as_object_mut().unwrap().insert("mock".into(), json!(true)); }),
        Box::new(|v| v["check"] = json!("cargo test --locked --test gmcp_requirements gmcp_req_203_020_99_missing")),
    ];
    for mutate in mutators {
        let mut bad = good_lines[0].clone();
        mutate(&mut bad);
        let txt = std::iter::once(&bad).chain(&good_lines[1..]).map(Value::to_string).collect::<Vec<_>>().join("\n");
        assert!(check_ev(&txt, false).is_err());
    }
    assert!(check_ev(&format!("{good_text}\n{}", good_lines[0]), false).is_err());

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-020.jsonl")).unwrap();
    assert_eq!(check_ev(&evidence, true).expect("VC-203-020.jsonl valid").len(), 6);

    // 5. Roadmap, verdict, closed prerequisite tasks & clean working tree checks.
    let roadmap_json: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap()).unwrap();
    let vc_entry = roadmap_json["vectors"].as_array().unwrap().iter().find(|e| e["id"] == "VC-203-020").unwrap();
    assert!(vc_entry["progress"].as_str().unwrap_or_default().starts_with("DELIVERED"));

    let verdict: Value = serde_json::from_str(&std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-020.json")).unwrap()).unwrap();
    assert!(verdict["vector"] == "VC-203-020" && verdict["verdict"] == "delivered");
    let tracked = verdict["tracked_by"].as_array().unwrap();
    assert_eq!(tracked.len(), 7);
    for n in 107..=113 {
        assert!(tracked.contains(&json!(format!("T-CODEXGMCPROADMAP-{n}"))));
    }
    for n in 107..=112 {
        assert!(root.join(format!(".agents/tasks/done/T-CODEXGMCPROADMAP-{n}.json")).is_file());
    }

    let status = Command::new("git")
        .args(["status", "--porcelain", "--", ".agents/gmcp", "docs/gmcp", "tests/gmcp_requirements.rs", "tests/gmcp_requirements", "crates/susi-gmcp", ".agents/roadmap.json", ".agents/roadmap-verdicts"])
        .current_dir(root)
        .output()
        .unwrap();
    assert!(status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(), "{}", String::from_utf8_lossy(&status.stdout));
}
