/// GMCP-203-016-06: operations, configuration validation, bounded telemetry,
/// diagnostics, runbooks, drift detection, and retained evidence for the
/// extension framework and extension governance.
#[test]
fn gmcp_req_203_016_06_operations() {
    use serde_json::{json, Map, Value};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    use susi_gmcp::extensions::{
        ExtensionError, ExtensionMap, ExtensionRegistry, ExtensionStatus, OFFICIAL_EXTENSIONS,
    };
    use susi_gmcp::protocol::GmcpService;
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

    let (_lock, _env, home) = gmcp_http_home_guard("016-ops");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    pack_ext::invalidate_extension_caches();

    // ── 1. Inspectable CLI & tool ledger/status/extensions parity ────────
    let report = ledger_report(root);
    assert_eq!(report["verified"], true, "{report}");
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

    // Extension pack CLI (`susi extensions list|status|validate`) matches CoreTools
    // without mutating host state, and invalid manifests fail closed.
    let cli_ext_status = susi_json(root, &home, &["extensions", "status"]);
    let tool_ext_status: Value = serde_json::from_str(
        &susi_gmcp::tools::CoreTools::extensions_status(&json!({}), root).unwrap(),
    )
    .unwrap();
    assert_eq!(cli_ext_status, tool_ext_status);
    assert_eq!(cli_ext_status["active"], "default");

    let cli_ext_list = susi_json(root, &home, &["extensions", "list"]);
    let tool_ext_list: Value = serde_json::from_str(
        &susi_gmcp::tools::CoreTools::extensions_list(&json!({}), root).unwrap(),
    )
    .unwrap();
    assert_eq!(cli_ext_list, tool_ext_list);

    let valid_pack = scratch.join("valid-pack");
    std::fs::create_dir_all(&valid_pack).unwrap();
    let payload_bytes = br#"{"example":true}"#;
    let payload_sha = susi_paths::hex::encode(Sha256::digest(payload_bytes));
    std::fs::write(valid_pack.join("rules.json"), payload_bytes).unwrap();
    std::fs::write(
        valid_pack.join("manifest.json"),
        json!({ "id": "ops-sample-pack", "name": "Ops Sample Pack", "version": "1.0.0", "apiVersion": "1", "permissions": ["filesystem.read"], "files": {"rules": "rules.json"}, "checksums": {"rules": format!("sha256:{payload_sha}")}})
        .to_string(),
    )
    .unwrap();
    let valid_pack_arg = valid_pack.to_str().unwrap();
    let cli_validate = susi_json(root, &home, &["extensions", "validate", valid_pack_arg]);
    let tool_validate: Value = serde_json::from_str(
        &susi_gmcp::tools::CoreTools::extensions_validate(&json!({"path": valid_pack_arg}), root)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cli_validate, tool_validate);
    assert_eq!(cli_validate["valid"], true);
    assert_eq!(cli_validate["id"], "ops-sample-pack");
    assert_eq!(
        susi_json(root, &home, &["extensions", "status"])["active"],
        "default",
        "read-only validate must not activate or mutate host pack state"
    );

    for (label, manifest_doc) in [
        (
            "unknown-permission",
            json!({ "id": "bad-perm", "name": "Bad Perm", "version": "1.0.0", "apiVersion": "1", "permissions": ["network.unknown"]}),
        ),
        (
            "incompatible-api-version",
            json!({ "id": "bad-api", "name": "Bad API", "version": "1.0.0", "apiVersion": "99"}),
        ),
        (
            "path-escape-without-permission",
            json!({ "id": "bad-escape", "name": "Bad Escape", "version": "1.0.0", "apiVersion": "1", "files": {"rules": "../outside.json"}}),
        ),
    ] {
        let bad_pack = scratch.join(format!("bad-pack-{label}"));
        std::fs::create_dir_all(&bad_pack).unwrap();
        std::fs::write(bad_pack.join("manifest.json"), manifest_doc.to_string()).unwrap();
        let out = susi_command(
            root,
            &home,
            &["extensions", "validate", bad_pack.to_str().unwrap()],
        )
        .output()
        .expect("susi extensions validate must run");
        assert!(
            !out.status.success(),
            "{label}: invalid extension pack must fail validation closed"
        );
        assert!(susi_gmcp::tools::CoreTools::extensions_validate(
            &json!({"path": bad_pack.to_str().unwrap()}),
            root,
        )
        .is_err());
    }

    // ── 2. Inspectable live protocol API state across transports ─────────
    let direct_svc = GmcpService::new(root.to_path_buf());
    let direct_discover = serde_json::to_value(direct_svc.discover_result()).unwrap();
    let direct_ext = direct_discover["capabilities"]["extensions"].clone();
    assert!(
        direct_ext["io.modelcontextprotocol/tasks"].is_object(),
        "shipped extensions must include tasks: {direct_discover}"
    );
    let status_rows = direct_svc.extensions().read().status_report();
    assert!(status_rows
        .as_array()
        .is_some_and(|rows| rows.iter().any(|row| {
            row["id"] == "io.modelcontextprotocol/tasks"
                && row["status"] == "official"
                && row["version"] == 1
                && row["enabled"] == true
        })));
    let watched = direct_svc.extensions().read().watch(OFFICIAL_EXTENSIONS);
    assert_eq!(watched.len(), OFFICIAL_EXTENSIONS.len() - 1);
    assert!(watched
        .iter()
        .all(|item| item.starts_with("[GMCP] Adopt newly published official extension ")));

    let token = "susi-016-ops-bearer-secret";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();

    let runtime = adapter_runtime(&home);
    let (mut embedded, emb_sess) = embedded_adapter(&runtime, root);
    let emb_disc = embedded.call("ops016-emb", "server/discover", adapter_modern_meta());
    assert_eq!(emb_disc["result"]["capabilities"]["extensions"], direct_ext);
    adapter_handshake(&mut embedded, "ops016-emb-init");
    drop(embedded);
    adapter_session_ended(&runtime, emb_sess, "016-ops-embedded");

    #[cfg(unix)]
    {
        let (mut socket, sock_sess) = socket_adapter(&runtime, root);
        let sock_disc = socket.call("ops016-sock", "server/discover", adapter_modern_meta());
        assert_eq!(
            sock_disc["result"]["capabilities"]["extensions"],
            direct_ext
        );
        // Served Unix-socket sessions require `initialize` before disconnect to end cleanly.
        adapter_handshake(&mut socket, "ops016-sock-init");
        drop(socket);
        adapter_session_ended(&runtime, sock_sess, "016-ops-socket");
    }

    let mut stdio = StdioSession::spawn_raw(root, &home);
    let stdio_disc = stdio
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "ops016-stdio", "method": "server/discover", "params": adapter_modern_meta()}),
            Duration::from_secs(120),
        )
        .expect("stdio server/discover must answer");
    assert_eq!(
        stdio_disc["result"]["capabilities"]["extensions"],
        direct_ext
    );
    drop(stdio);

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback HTTP /mcp port");
    let port = listener.local_addr().unwrap().port();
    let http_addr = format!("127.0.0.1:{port}");
    let http_root = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(http_root, vec![listener], None, false);
    });
    for _ in 0..40 {
        if TcpStream::connect(&http_addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let wire_cli = susi_wire_client::mcp::Session::open_with_capabilities(
        &http_addr,
        Some(token),
        &json!({ "extensions": { "io.modelcontextprotocol/tasks": {}, "vendor.ops/probe": {"v": 1}}}),
        Duration::from_secs(15),
    )
    .expect("Wire SDK session must open and capture server extensions");
    assert_eq!(
        Value::Object(wire_cli.server_extensions().unwrap().clone()),
        direct_ext
    );
    wire_cli.close().expect("Wire SDK session close");

    // ── 3. Configuration validation & fail-closed startup diagnostics ────
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
        .any(|i| { i.path == "max_rpc_body_bytes" && i.kind == ConfigIssueKind::TypeMismatch }));

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
            json!({ "jsonrpc": "2.0", "id": "ops-zero-ping", "method": "ping", "params": adapter_modern_meta()})
        )
        .unwrap();
    }
    drop(zero_child.stdin.take());
    let zero_out = zero_child.wait_with_output().unwrap();
    assert!(
        zero_out.stdout.is_empty(),
        "zero max_rpc_body_bytes must emit no non-JSON-RPC bytes or answers on stdout: {}",
        String::from_utf8_lossy(&zero_out.stdout)
    );
    let zero_stderr = String::from_utf8_lossy(&zero_out.stderr);
    assert!(
        zero_stderr.contains("max_rpc_body_bytes") && zero_stderr.contains("must be > 0"),
        "expected stderr refusal diagnostic, got: {zero_stderr}"
    );

    // ── 4. Bounded metrics, logs, traceparent & isolation governance ─────
    // Nesting/node ceiling at content boundary (`MAX_DEPTH = 64` -> TooLarge).
    let mut deep = json!({"vendor.ops/leaf": 1});
    for _ in 0..70 {
        deep = json!({"nested": deep});
    }
    let bomb = json!({"type": "text", "text": "ok", "vendor.ops/deep": deep});
    assert_eq!(
        CanonicalContentBlock::from_wire(&bomb),
        Err(ContentError::TooLarge)
    );

    // HTTP `/mcp` traceparent + bounded/redacted SSE logging at `logLevel=info`
    // vs silent suppression when omitted, and Bearer token redaction.
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
    trace_meta["io.modelcontextprotocol/requestId"] = json!("ops016-http-info");
    trace_meta["io.modelcontextprotocol/logLevel"] = json!("info");
    trace_meta["vendor.ops/context"] = json!({"secret": token});
    let (http_st, http_raw, http_frames) = http_post(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
            (
                "traceparent",
                "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            ),
        ],
        &json!({ "jsonrpc": "2.0", "id": "ops016-http-info", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": trace_meta}}),
    );
    assert!(http_st.starts_with("HTTP/1.1 200"), "{http_st} {http_raw}");
    let info_res = http_frames
        .iter()
        .find(|f| f["id"] == "ops016-http-info")
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
    quiet_meta["io.modelcontextprotocol/requestId"] = json!("ops016-http-quiet");
    let (_, quiet_raw, quiet_frames) = http_post(
        &[
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "tools/call"),
            ("Mcp-Name", "ledger_verify"),
        ],
        &json!({ "jsonrpc": "2.0", "id": "ops016-http-quiet", "method": "tools/call", "params": { "name": "ledger_verify", "arguments": {}, "_meta": quiet_meta}}),
    );
    assert!(
        quiet_frames
            .iter()
            .all(|f| f["method"] != "notifications/message"),
        "omitted logLevel must suppress notifications/message: {quiet_raw}"
    );

    // Registry isolation, collision, downgrade & disablement diagnostics.
    let mut iso_reg = ExtensionRegistry::shipped();
    iso_reg
        .register(
            "vendor.ops/gauge",
            ExtensionStatus::Experimental,
            2,
            Map::new(),
        )
        .unwrap();
    assert_eq!(
        iso_reg.register("bare-ops", ExtensionStatus::Experimental, 1, Map::new()),
        Err(ExtensionError::NotNamespaced("bare-ops".into()))
    );
    assert_eq!(
        iso_reg.register("vendor.ops/gauge", ExtensionStatus::Official, 2, Map::new()),
        Err(ExtensionError::Collision("vendor.ops/gauge".into()))
    );
    let mut old_peer = ExtensionMap::new();
    old_peer.insert(
        "vendor.ops/gauge".into(),
        serde_json::from_value(json!({"version": 1})).unwrap(),
    );
    assert!(matches!(
        iso_reg.negotiate(&old_peer, &["vendor.ops/gauge".into()]),
        Err(ExtensionError::Downgrade {
            required: 1,
            served: 2,
            ..
        })
    ));
    assert!(iso_reg.set_enabled("vendor.ops/gauge", false));
    assert!(!iso_reg.capability_map().contains_key("vendor.ops/gauge"));
    assert_eq!(
        iso_reg.negotiate(&old_peer, &["vendor.ops/gauge".into()]),
        Err(ExtensionError::RequiredUnsupported(vec![
            "vendor.ops/gauge".into()
        ]))
    );
    assert!(iso_reg
        .capability_map()
        .contains_key("io.modelcontextprotocol/tasks"));

    // ── 5. Pinned extension specs & deterministic drift detection ────────
    let sources: Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/sources.json")).expect("sources.json readable"),
    )
    .expect("sources.json valid JSON");
    let pinned_ext_docs = [
        "ext-apps/specification/2026-01-26/apps.mdx",
        "ext-auth/specification/stable/enterprise-managed-authorization.mdx",
        "ext-tasks/specification/2026-07-28/tasks.md",
        "docs/specification/2026-07-28/index.mdx",
        "schema/2026-07-28/schema.json",
        "schema/2026-07-28/schema.ts",
        "schema/2025-11-25/schema.json",
        "schema/2025-11-25/schema.ts",
        "registry/docs/reference/api/generic-registry-api.md",
        "registry/docs/reference/api/official-registry-api.md",
    ];
    for doc in &pinned_ext_docs {
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
    let total_pins = clean_a["checked"].as_u64().unwrap_or(0);
    assert!(total_pins >= pinned_ext_docs.len() as u64);

    // Mutating any VC-203-016 standard or schema file invalidates proof with `hash_drift`,
    // and rollback to the original pinned bytes restores the identical proof.
    for victim_rel in [
        "ext-apps/specification/2026-01-26/apps.mdx",
        "ext-auth/specification/stable/enterprise-managed-authorization.mdx",
        "ext-tasks/specification/2026-07-28/tasks.md",
        "docs/specification/2026-07-28/index.mdx",
        "schema/2026-07-28/schema.json",
    ] {
        let victim = refetched.join(victim_rel);
        let original = std::fs::read(&victim).unwrap();
        let mut altered = original.clone();
        altered.extend_from_slice(b"\n<!-- drifted extension spec -->\n");
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
    let rolled_back = susi_json(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    );
    assert_eq!(rolled_back, clean_a);

    // Missing spec document fails closed with `unfetchable`.
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

    // Mutating a dependency artifact (vendored spec vs pinned sources.json in a repo copy)
    // invalidates `verify_pinned_ledger` fail-closed.
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
    let tampered_apps =
        tampered_repo.join(".agents/gmcp/spec/ext-apps/specification/2026-01-26/apps.mdx");
    let mut app_bytes = std::fs::read(&tampered_apps).unwrap();
    app_bytes.push(b'\n');
    std::fs::write(&tampered_apps, app_bytes).unwrap();
    assert!(
        susi_gmcp::requirements_ledger::verify_pinned_ledger(&tampered_repo).is_err(),
        "changed dependency spec must fail verify_pinned_ledger closed"
    );

    // ── 6. Runbook contract bindings & retained evidence verification ────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-016.md"))
        .expect("docs/gmcp/contract-203-016.md readable");
    for required in [
        "## Operations and non-claims",
        "## Operations and incident runbook",
        "### Inspectable state",
        "### Configuration validation",
        "### Bounded metrics, logs and traces",
        "### Diagnostics and incident triage",
        "### Examples",
        "### Incident triage",
        "### Migration (extension governance and re-pinning upstream specs)",
        "### Rollback",
        "### Production guarantees and non-claims",
        "EXT-001",
        "EXT-011",
        "MAX_NODES = 10_000",
        "MAX_DEPTH = 64",
        "DEFAULT_MAX_LINE_LENGTH = 16777216",
        "DEFAULT_MAX_REQUEST_BODY_BYTES = 4194304",
        "HOST_PACK_API_MAJOR = 1",
        "NotNamespaced",
        "Collision",
        "RequiredUnsupported",
        "Downgrade",
        "-32600",
        "-32602",
        "-32020",
        "-32022",
        "hash_drift",
        "unfetchable",
        "ee452a7d1b9b7fb900acfeb4d6932d3963375b0f3f37d196a4b93eb80312af0e",
        "df4fe01daec0eac6069e17a293d4ed7e197ab20463038d806f71e63b22fd8986",
        "5fc22b9b5ff248703927c69855f751a061c2d6170f4dc3923204452f859f91a5",
        "c8151ab8d7f26ab46785fd34b23f7197db1a98ca8318967faa01e401909667b5",
        "ef70b61f99b6d2e5e3b46863822eab08dff6a45bedc7a08914e0e5b133f40203",
        "742750af0bb8c716e7030c4977c992b55d1adc4407e9e66997db5846baedc2cd",
        "268a5f82ba70fd7e4b6dc4aa1e64f116f74b4d0edcb69dc046829c79dd4e97e7",
        "e74b56e73b2e37bdb595f74ba22e428ad7f07aa3519355ba661d681298ed38ac",
        "f5352a13d8b7309e28a23da998759f14c34945cb4008de10e51bba9bc47bb109",
        "37989fc77551006d7fc3d34954c1f9d81a4edace8afc519d92b1c82a1b031dd9",
    ] {
        assert!(
            contract.contains(required),
            "docs/gmcp/contract-203-016.md missing runbook section/token `{required}`"
        );
    }

    let ledger_sha = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence_files = gmcp_evidence_files(root);
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

    pack_ext::invalidate_extension_caches();
    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&home);
}

/// GMCP-203-016-M: Vector mastery gate for VC-203-016 (First-class extension framework and extension governance).
///
/// Proves the complete VC-203-016 mastery target on the shipped production path, binds all pinned upstream
/// standards and contract clauses, strictly verifies committed non-waived non-mock signed evidence for
/// GMCP-203-016-01..06 (and rejects missing, stale, unsigned, locally modified, waived, mock-only,
/// unlinked-supersession, wrong-digest, or zero-test evidence), and confirms roadmap + verdict delivery.
#[test]
fn gmcp_vc_203_016_mastery() {
    use serde_json::{json, Map, Value};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeSet;
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;
    use susi_gmcp::extensions::{
        ExtensionError, ExtensionMap, ExtensionRegistry, ExtensionStatus, OFFICIAL_EXTENSIONS,
    };
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::susi_config::extensions as pack_ext;
    use susi_gmcp::tools::CoreTools;

    struct EvidenceCtx<'a> {
        root: &'a Path,
        ledger_hash: &'a str,
        test_src: &'a str,
        head_digest: &'a str,
        now: u64,
    }

    fn verify_vc_203_016_evidence(
        ctx: &EvidenceCtx<'_>,
        evidence_text: &str,
        committed_artifacts: Option<&[String]>,
    ) -> Result<BTreeSet<String>, String> {
        const LINK: [&str; 3] = ["recorded_unix", "identity", "artifact_sha256"];
        let names = |record: &Value, target: &Value| {
            LINK.iter()
                .all(|field| record["supersedes"][*field] == target[*field])
        };
        let mut order: Vec<String> = Vec::new();
        let mut chains: std::collections::HashMap<String, Vec<Value>> =
            std::collections::HashMap::new();
        for line in evidence_text.lines().filter(|l| !l.trim().is_empty()) {
            let record: Value = serde_json::from_str(line)
                .map_err(|e| format!("evidence line is not valid JSON: {e}"))?;
            let requirement = record["requirement"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            match chains.get_mut(&requirement) {
                Some(chain) => {
                    if !chain.iter().any(|earlier| names(&record, earlier)) {
                        return Err(format!(
                            "{requirement}: unlinked duplicate record missing supersedes link"
                        ));
                    }
                    chain.push(record);
                }
                None => {
                    order.push(requirement.clone());
                    chains.insert(requirement, vec![record]);
                }
            }
        }
        let newest: Vec<Value> = order
            .into_iter()
            .filter_map(|req| {
                let chain = chains.remove(&req)?;
                let tips: Vec<&Value> = chain
                    .iter()
                    .filter(|r| !chain.iter().any(|later| names(later, r)))
                    .collect();
                tips.into_iter()
                    .max_by_key(|r| r["recorded_unix"].as_u64().unwrap_or(0))
                    .cloned()
            })
            .collect();

        let mut covered = BTreeSet::new();
        for record in newest {
            let req = record["requirement"].as_str().unwrap_or_default();
            if req.ends_with("-M") {
                continue;
            }
            if record.get("waived").is_some() || record.get("mock").is_some() {
                return Err(format!("{req}: waived or mock-only evidence rejected"));
            }
            for field in [
                "requirement",
                "check",
                "exit",
                "detail",
                "recorded_unix",
                "artifact_sha256",
                "identity",
            ] {
                if record[field].is_null() {
                    return Err(format!("{req}: incomplete evidence missing {field}"));
                }
            }
            if record["exit"] != 0 {
                return Err(format!("{req}: non-zero exit evidence rejected"));
            }
            if record["identity"].as_str().unwrap_or_default().is_empty() {
                return Err(format!("{req}: unsigned empty identity rejected"));
            }
            if record["detail"].as_str().unwrap_or_default().is_empty() {
                return Err(format!("{req}: empty detail rejected"));
            }
            if record["artifact_sha256"].as_str().unwrap_or_default() != ctx.ledger_hash {
                return Err(format!("{req}: stale artifact_sha256 rejected"));
            }
            let recorded = record["recorded_unix"].as_u64().unwrap_or(0);
            if recorded <= 1_792_256_000 || recorded > ctx.now + 60 {
                return Err(format!("{req}: stale or future recorded_unix {recorded}"));
            }
            let check = record["check"].as_str().unwrap_or_default();
            if let Some(test_name) =
                check.strip_prefix("cargo test --locked --test gmcp_requirements ")
            {
                if !test_name.starts_with("gmcp_")
                    || test_name.contains(char::is_whitespace)
                    || !is_test_fn(ctx.test_src, test_name)
                {
                    return Err(format!("{req}: zero-test missing fn {test_name}"));
                }
            } else if !check
                .split_whitespace()
                .filter(|t| !t.starts_with('-'))
                .any(|t| ctx.root.join(t).exists())
            {
                return Err(format!("{req}: check references no committed artifact"));
            }
            if let Some(artifacts) = committed_artifacts {
                assert_bound_to_tested_commit(ctx.root, &record, artifacts, ctx.head_digest);
            } else {
                let commit = record["tested_commit"].as_str().unwrap_or_default();
                if commit.len() != 40 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(format!("{req}: invalid tested_commit"));
                }
                if record["tested_artifacts"].as_str() != Some(ctx.head_digest) {
                    return Err(format!("{req}: stale tested_artifacts digest"));
                }
            }
            covered.insert(req.to_string());
        }
        for req in [
            "GMCP-203-016-01",
            "GMCP-203-016-02",
            "GMCP-203-016-03",
            "GMCP-203-016-04",
            "GMCP-203-016-05",
            "GMCP-203-016-06",
        ] {
            if !covered.contains(req) {
                return Err(format!("missing evidence record for {req}"));
            }
        }
        Ok(covered)
    }

    let (_lock, _env, home) = gmcp_http_home_guard("016-mastery");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    pack_ext::invalidate_extension_caches();

    // ── 1. Production ledger report & CLI/tool parity ────────────────────
    let report = ledger_report(root);
    assert_eq!(report["verified"], true, "ledger must verify: {report}");
    assert_eq!(ledger_tool_report(root), report);
    assert_eq!(susi_json(root, &home, &["mcp", "ledger"]), report);

    // ── 2. Contract & 10 pinned specification artifacts verified ──────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-016.md"))
        .expect("contract-203-016.md must exist");
    for token in [
        "EXT-001",
        "EXT-002",
        "EXT-006",
        "EXT-011",
        "## Operations and non-claims",
        "## Operations and incident runbook",
        "### Bounded metrics, logs and traces",
        "### Production guarantees and non-claims",
    ] {
        assert!(
            contract.contains(token),
            "contract-203-016.md missing required clause/section: {token}"
        );
    }
    let sources: Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/sources.json")).expect("sources.json readable"),
    )
    .expect("sources.json valid JSON");
    for doc in [
        "ext-apps/specification/2026-01-26/apps.mdx",
        "ext-auth/specification/stable/enterprise-managed-authorization.mdx",
        "ext-tasks/specification/2026-07-28/tasks.md",
        "docs/specification/2026-07-28/index.mdx",
        "schema/2026-07-28/schema.json",
        "schema/2026-07-28/schema.ts",
        "schema/2025-11-25/schema.json",
        "schema/2025-11-25/schema.ts",
        "registry/docs/reference/api/generic-registry-api.md",
        "registry/docs/reference/api/official-registry-api.md",
    ] {
        let bytes = std::fs::read(root.join(".agents/gmcp/spec").join(doc))
            .unwrap_or_else(|e| panic!("pinned spec {doc} missing: {e}"));
        let hex = susi_paths::hex::encode(Sha256::digest(&bytes));
        let pinned = sources["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["document"] == doc)
            .and_then(|f| f["sha256"].as_str());
        assert_eq!(pinned, Some(hex.as_str()), "pinned hash mismatch for {doc}");
    }

    // ── 3. Mastery target live verification across framework & transports ─
    let direct = GmcpService::new(root.to_path_buf());
    let discover_ext = serde_json::to_value(direct.discover_result()).unwrap()["capabilities"]
        ["extensions"]
        .clone();
    assert!(discover_ext["io.modelcontextprotocol/tasks"].is_object());

    let mut reg = ExtensionRegistry::shipped();
    reg.register(
        "vendor.mastery/audit",
        ExtensionStatus::Experimental,
        2,
        Map::from_iter([("strict".into(), json!(true))]),
    )
    .unwrap();
    assert_eq!(
        reg.register("bad-no-slash", ExtensionStatus::Experimental, 1, Map::new()),
        Err(ExtensionError::NotNamespaced("bad-no-slash".into()))
    );
    assert_eq!(
        reg.register(
            "vendor.mastery/audit",
            ExtensionStatus::Official,
            2,
            Map::new()
        ),
        Err(ExtensionError::Collision("vendor.mastery/audit".into()))
    );
    assert_eq!(
        reg.negotiate(&Default::default(), &["vendor.mastery/missing".into()]),
        Err(ExtensionError::RequiredUnsupported(vec![
            "vendor.mastery/missing".into()
        ]))
    );
    let mut down_peer = ExtensionMap::new();
    let mut down_cfg = Map::new();
    down_cfg.insert("version".into(), json!(1));
    down_peer.insert("vendor.mastery/audit".into(), down_cfg);
    assert_eq!(
        reg.negotiate(&down_peer, &["vendor.mastery/audit".into()]),
        Err(ExtensionError::Downgrade {
            id: "vendor.mastery/audit".into(),
            required: 1,
            served: 2,
        })
    );
    let unknown_only = reg
        .negotiate(
            &[(
                "vendor.peer/custom".into(),
                Map::from_iter([("opaque".into(), json!("ok"))]),
            )]
            .into_iter()
            .collect(),
            &["io.modelcontextprotocol/tasks".into()],
        )
        .unwrap();
    assert_eq!(unknown_only.unknown["vendor.peer/custom"]["opaque"], "ok");
    assert!(reg.set_enabled("vendor.mastery/audit", false));
    assert!(!reg.capability_map().contains_key("vendor.mastery/audit"));
    let watch_items = direct.extensions().read().watch(OFFICIAL_EXTENSIONS);
    assert_eq!(watch_items.len(), OFFICIAL_EXTENSIONS.len() - 1);
    assert!(watch_items
        .iter()
        .all(|t| t.starts_with("[GMCP] Adopt newly published official extension ")));

    let runtime = adapter_runtime(&home);
    let (mut embedded, emb_sess) = embedded_adapter(&runtime, root);
    let emb_disc = embedded.call("m016-emb", "server/discover", adapter_modern_meta());
    assert_eq!(
        emb_disc["result"]["capabilities"]["extensions"],
        discover_ext
    );
    adapter_handshake(&mut embedded, "m016-emb-init");
    drop(embedded);
    adapter_session_ended(&runtime, emb_sess, "016-mastery-embedded");

    #[cfg(unix)]
    {
        let (mut socket, sock_sess) = socket_adapter(&runtime, root);
        let sock_disc = socket.call("m016-sock", "server/discover", adapter_modern_meta());
        assert_eq!(
            sock_disc["result"]["capabilities"]["extensions"],
            discover_ext
        );
        adapter_handshake(&mut socket, "m016-sock-init");
        drop(socket);
        adapter_session_ended(&runtime, sock_sess, "016-mastery-socket");
    }

    let mut stdio = StdioSession::spawn_modern(root, &home);
    let stdio_disc = stdio.request("server/discover", adapter_modern_meta());
    assert_eq!(stdio_disc["capabilities"]["extensions"], discover_ext);
    drop(stdio);

    let token = "susi-016-mastery-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mastery HTTP /mcp");
    let http_addr = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
    let http_root = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(http_root, vec![listener], None, false);
    });
    for _ in 0..50 {
        if TcpStream::connect(&http_addr).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let wire = susi_wire_client::mcp::Session::open_with_capabilities(
        &http_addr,
        Some(token),
        &json!({ "extensions": { "io.modelcontextprotocol/tasks": {}, "vendor.mastery/probe": {"v": 1}}}),
        Duration::from_secs(15),
    )
    .expect("Wire SDK HTTP session must open");
    assert_eq!(
        Value::Object(wire.server_extensions().unwrap().clone()),
        discover_ext
    );
    wire.close().expect("Wire SDK session close");

    assert_eq!(
        susi_json(root, &home, &["extensions", "status"]),
        serde_json::from_str::<Value>(&CoreTools::extensions_status(&json!({}), root).unwrap())
            .unwrap()
    );
    pack_ext::pack_admission_probe(&scratch).expect("signed pack admission probe must pass");

    // ── 4. Negative evidence-rejection suite & live VC-203-016 proof ─────
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap();
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let artifacts = evidence_artifacts(root, "VC-203-016");
    let head_digest =
        artifacts_digest(root, "HEAD", &artifacts).expect("HEAD must digest artifacts");

    let valid_tests = [
        ("GMCP-203-016-01", "gmcp_req_203_016_01_contract"),
        ("GMCP-203-016-02", "gmcp_req_203_016_02_production"),
        ("GMCP-203-016-03", "gmcp_req_203_016_03_integration"),
        ("GMCP-203-016-04", "gmcp_req_203_016_04_adversarial"),
        ("GMCP-203-016-05", "gmcp_req_203_016_05_interop"),
        ("GMCP-203-016-06", "gmcp_req_203_016_06_operations"),
    ];
    let make_row = |req: &str, test_fn: &str| {
        json!({ "requirement": req, "check": format!("cargo test --locked --test gmcp_requirements {test_fn}"), "exit": 0, "detail": "synthetic valid row", "recorded_unix": now, "artifact_sha256": ledger_hash, "identity": "AG2FEATURES", "tested_commit": "0".repeat(40), "tested_artifacts": head_digest})
    };
    let good_lines: Vec<Value> = valid_tests.iter().map(|(r, t)| make_row(r, t)).collect();
    let good_text = good_lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let ev_ctx = EvidenceCtx {
        root,
        ledger_hash: &ledger_hash,
        test_src: &test_src,
        head_digest: &head_digest,
        now,
    };
    let check_ev = |txt: &str, committed: bool| {
        verify_vc_203_016_evidence(&ev_ctx, txt, committed.then_some(artifacts.as_slice()))
    };
    assert!(check_ev(&good_text, false).is_ok());

    // 4a. Reject missing requirement
    let missing_text = good_lines[..5]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(check_ev(&missing_text, false).is_err());

    // 4b. Reject stale/future timestamps, stale hashes, unsigned, non-zero exit,
    //     waived, mock-only, zero-test, and unlinked supersession records.
    for mutate in [
        Box::new(|v: &mut Value| v["recorded_unix"] = json!(1_700_000_000_u64))
            as Box<dyn Fn(&mut Value)>,
        Box::new(|v: &mut Value| v["recorded_unix"] = json!(now + 120)),
        Box::new(|v: &mut Value| v["artifact_sha256"] = json!("deadbeef")),
        Box::new(|v: &mut Value| v["tested_artifacts"] = json!("stale-digest")),
        Box::new(|v: &mut Value| v["identity"] = json!("")),
        Box::new(|v: &mut Value| v["exit"] = json!(1)),
        Box::new(|v: &mut Value| {
            v.as_object_mut()
                .unwrap()
                .insert("waived".into(), json!(true));
        }),
        Box::new(|v: &mut Value| {
            v.as_object_mut()
                .unwrap()
                .insert("mock".into(), json!(true));
        }),
        Box::new(|v: &mut Value| {
            v["check"] = json!(
                "cargo test --locked --test gmcp_requirements gmcp_req_203_016_99_nonexistent"
            );
        }),
    ] {
        let mut bad = good_lines[0].clone();
        mutate(&mut bad);
        let txt = std::iter::once(&bad)
            .chain(&good_lines[1..])
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(check_ev(&txt, false).is_err());
    }
    let unlinked_txt = format!("{good_text}\n{}", good_lines[0]);
    assert!(check_ev(&unlinked_txt, false).is_err());

    // Live VC-203-016 committed evidence must satisfy every single rule strictly.
    let evidence = std::fs::read_to_string(gmcp_evidence_path(root, ".agents/gmcp/evidence/VC-203-016.jsonl"))
        .expect("VC-203-016 must retain an evidence artifact");
    let covered = check_ev(&evidence, true)
        .expect("committed VC-203-016.jsonl must pass full strict verification");
    assert_eq!(covered.len(), 6);

    // ── 5. Roadmap & verdict delivery binding ────────────────────────────
    let roadmap_text = std::fs::read_to_string(root.join(".agents/roadmap.json")).unwrap();
    let roadmap_json: Value = serde_json::from_str(&roadmap_text).unwrap();
    let vc_entry = roadmap_json["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "VC-203-016")
        .expect("VC-203-016 must exist in roadmap.json");
    assert!(
        vc_entry["progress"]
            .as_str()
            .unwrap_or_default()
            .starts_with("DELIVERED"),
        "VC-203-016 must be marked DELIVERED in roadmap.json"
    );

    let verdict_text =
        std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-016.json"))
            .expect("VC-203-016.json verdict must exist");
    let verdict: Value = serde_json::from_str(&verdict_text).unwrap();
    assert_eq!(verdict["vector"], "VC-203-016");
    assert_eq!(verdict["verdict"], "delivered");
    let tracked = verdict["tracked_by"].as_array().unwrap();
    assert_eq!(tracked.len(), 7);
    assert!(tracked.contains(&json!("T-CODEXGMCPROADMAP-79")));
    assert!(tracked.contains(&json!("T-CODEXGMCPROADMAP-85")));

    // ── 6. Reject locally modified proof artifacts Closed ───────────────
    let status = Command::new("git")
        .args([
            "status",
            "--porcelain",
            "--",
            ".agents/gmcp",
            "docs/gmcp",
            "tests/gmcp_requirements.rs",
            "tests/gmcp_requirements",
            "crates/susi-gmcp",
            ".agents/roadmap.json",
            ".agents/roadmap-verdicts",
        ])
        .current_dir(root)
        .output()
        .expect("git status must run");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    pack_ext::invalidate_extension_caches();
    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&home);
}
