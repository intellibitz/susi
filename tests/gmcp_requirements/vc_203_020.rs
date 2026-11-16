/// GMCP-203-020-01: the GMCP server authoring SDK and plugin lifecycle
/// contract is pinned (`docs/gmcp/contract-203-020.md`) with verified source
/// hashes, confined production-path behavior rows (`SDK-001..SDK-018`), zero
/// vendor types across the public boundary, and positive/negative schema
/// derivation and plugin manifest admission fixtures.
#[test]
fn gmcp_req_203_020_01_contract() {
    use sha2::{Digest, Sha256};
    use std::{collections::{BTreeMap, BTreeSet}, sync::Arc};
    use susi_config::extensions::{validate_manifest, ExtensionManifest, HOST_PACK_API_MAJOR, KNOWN_PERMISSIONS};
    use susi_gmcp::{mcp::model::{CallToolResult, Tool}, protocol::GmcpService};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-020.md")).expect("contract");
    for section in [
        "Normative clause", "Pinned sources", "Canonical data model", "Behavior table",
        "Compatibility and deprecation decisions", "Threat model", "SLOs and resource budgets",
        "Fixtures", "Evidence locations", "Ambiguities resolved",
    ] {
        assert!(contract.contains(&format!("## {section}")), "missing `## {section}`");
    }
    let ledger = susi_gmcp::requirements_ledger::verify_pinned_ledger(root).expect("ledger");
    assert!(ledger.rows > 0 && ledger.documents > 0);

    let sources: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).expect("sources")).expect("json");
    let upstream_commits: BTreeMap<&str, &str> = sources["upstream"].as_object().expect("u").iter().map(|(r, m)| (r.as_str(), m["commit"].as_str().expect("c"))).collect();
    let source_files: BTreeMap<&str, (&str, &str)> = sources["files"].as_array().expect("f").iter().map(|f| (f["document"].as_str().expect("d"), (f["repo"].as_str().expect("r"), f["sha256"].as_str().expect("s")))).collect();

    let pinned_rows: Vec<&str> = contract.lines().filter(|l| ["| modelcontextprotocol |", "| ext-tasks |", "| ext-apps |", "| ext-auth |"].iter().any(|p| l.starts_with(p))).collect();
    assert!(pinned_rows.len() >= 14);
    for row in pinned_rows {
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(|c| c.trim().trim_matches('`')).collect();
        let (repo, doc, commit, sha) = (cells[0], cells[1], cells[2], cells[3]);
        let (expected_repo, expected_sha) = source_files.get(doc).copied().expect("pinned doc");
        assert!(repo == expected_repo && Some(commit) == upstream_commits.get(repo).copied() && sha == expected_sha);
        let disk_sha: String = Sha256::digest(std::fs::read(root.join(".agents/gmcp/spec").join(doc)).expect("spec")).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(disk_sha, sha);
    }

    let sdk_rows: Vec<&str> = contract.lines().filter(|l| l.starts_with("| SDK-")).collect();
    assert!(sdk_rows.len() >= 18);
    let mut ids = BTreeSet::new();
    for row in sdk_rows {
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
        assert!(ids.insert(cells[0].to_string()) && ["must", "must_not", "should", "should_not", "may"].contains(&cells[1]));
        assert!(!cells[2].is_empty() && confined_repo_path(cells[3]) && root.join(cells[3]).exists());
    }
    let lower = contract.to_lowercase();
    for m in [
        "schema", "context", "middleware", "fixture", "hot reload", "permission", "compatibility",
        "transactional", "unload", "vendor", "example", "tools", "resources", "prompts",
        "subscriptions", "mrtr", "tasks", "extensions", "susi commercial apps",
    ] {
        assert!(lower.contains(m), "contract must resolve `{m}`");
    }
    assert!(contract.contains("gmcp_req_203_020_01_contract"));

    susi_gmcp::input_schema! {
        /// Nested filter clause for SDK contract verification.
        struct FilterSpec {
            /// Exact namespace prefix to match.
            prefix: String,
            /// Optional tag filter.
            tag: Option<String>,
        }
        /// Root tool input derived via Susi-owned schema macros.
        struct QueryArgs {
            /// Search expression.
            query: String,
            /// Nested filter descriptor.
            filter: FilterSpec,
            /// Maximum items to return.
            #[serde(default)]
            limit: u32,
            /// Include archived entries in output.
            #[serde(rename = "includeArchived")]
            include_archived: Option<bool>,
            /// Explicit label list.
            labels: Vec<String>,
        }
    }

    let derived = serde_json::Value::Object(susi_gmcp::mcp::schema::root::<QueryArgs>());
    assert_eq!(derived["$schema"], "https://json-schema.org/draft/2020-12/schema");
    assert_eq!((derived["type"].as_str(), derived["properties"]["query"]["description"].as_str(), derived["properties"]["filter"]["$ref"].as_str()), (Some("object"), Some("Search expression."), Some("#/$defs/FilterSpec")));
    assert_eq!((derived["properties"]["limit"]["default"].as_u64(), derived["properties"]["labels"]["type"].as_str()), (Some(0), Some("array")));
    assert_eq!(derived["properties"]["includeArchived"]["type"], serde_json::json!(["boolean", "null"]));
    assert_eq!(derived["$defs"]["FilterSpec"]["properties"]["tag"]["type"], serde_json::json!(["string", "null"]));
    assert_eq!(derived["required"].as_array().expect("req").iter().filter_map(serde_json::Value::as_str).collect::<BTreeSet<_>>(), BTreeSet::from(["query", "filter", "labels"]));

    let scratch = std::env::temp_dir().join(format!("gmcp-203-020-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    let service = GmcpService::new(scratch.clone());
    let noop: susi_gmcp::protocol::ToolHandler = Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }));
    service.register_tool(Tool::new_with_raw("sdk_contract_query", None, susi_gmcp::mcp::schema::root::<QueryArgs>()), Arc::clone(&noop)).expect("valid");
    let bad_schema = serde_json::Map::from_iter([("$ref".into(), serde_json::json!("https://evil.example/s.json"))]);
    assert!(service.register_tool(Tool::new_with_raw("sdk_remote_ref_rejected", None, bad_schema), noop).is_err());
    let _ = std::fs::remove_dir_all(&scratch);

    assert_eq!((HOST_PACK_API_MAJOR, KNOWN_PERMISSIONS), (1, &["filesystem.read", "filesystem.write", "network.egress", "process.exec"][..]));
    let valid_manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({ "id": "sdk-sample-pack", "name": "SDK Sample Pack", "version": "0.1.0", "apiVersion": "1.0", "capabilities": ["mcp.tools.sample"], "requires": ["gmcp.server.v1"], "optional": ["gmcp.tasks.v1"], "permissions": ["filesystem.read"], "files": { "tools": "catalog/tools.json"}, "checksums": { "tools": format!("sha256:{}", "a".repeat(64))}, "x-vendor-metadata": {"tier": "contract-fixture"}})).expect("manifest");
    validate_manifest(&valid_manifest).expect("valid manifest");
    assert_eq!(valid_manifest.extra.get("x-vendor-metadata"), Some(&serde_json::json!({"tier": "contract-fixture"})));

    type MutManifest = fn(&mut ExtensionManifest);
    let cases: [(MutManifest, &str); 5] = [
        (|m| m.permissions = vec!["kernel.root".into()], "unknown permission"),
        (|m| m.api_version = "2.0".into(), "incompatible with host major"),
        (|m| { m.permissions.clear(); m.files.insert("tools".into(), "../outside/tools.json".into()); }, "without declaring `filesystem.read`"),
        (|m| { m.signatures.insert("tools".into(), "b".repeat(128)); m.signer = None; }, "without a signer"),
        (|m| { m.checksums.insert("tools".into(), "sha256:not-a-hex-digest".into()); }, "invalid SHA-256 checksum"),
    ];
    for (mutate, needle) in cases {
        let mut m = valid_manifest.clone();
        mutate(&mut m);
        assert!(validate_manifest(&m).expect_err("must fail").contains(needle));
    }

    for manifest_rel in ["crates/susi-gmcp/Cargo.toml", "crates/susi-abi/Cargo.toml"] {
        let cargo_toml = std::fs::read_to_string(root.join(manifest_rel)).expect("Cargo.toml");
        for v in ["rmcp", "schemars", "axum", "tower"] {
            assert!(!cargo_toml.contains(v), "{manifest_rel} must not depend on `{v}`");
        }
    }
    assert!(root.join(".agents/gmcp/evidence/VC-203-020.jsonl").is_file());
}

/// GMCP-203-020-02: canonical GMCP server authoring SDK and transactional plugin
/// lifecycle execute end-to-end on the shipped GMCP runtime (`SDK-001..SDK-018`).
#[test]
fn gmcp_req_203_020_02_production() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    };
    use std::time::Duration;
    use susi_config::extensions::DEFAULT_PACK_ID;
    use susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
    use susi_gmcp::mcp::model::{
        CallToolResult, ClientCapabilities, ContentBlock, ElicitationCapability,
        ReadResourceResult, Resource, ResourceContents, TaskStatus, Tool,
    };
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::sdk::{
        examples::{minimal_echo_server, production_reference_server, sample_plugin_bundle},
        CatalogUpdate, PluginError, PluginPolicy, ServerBuilder, TestHarness,
    };

    async fn req(h: &TestHarness, m: &str, p: Value) -> Value {
        h.request_modern(m, p, ClientCapabilities::default())
            .await
            .expect("rpc")
    }
    async fn req_c(h: &TestHarness, m: &str, p: Value, c: &ClientCapabilities) -> Value {
        h.request_modern(m, p, c.clone()).await.expect("rpc")
    }
    async fn call(h: &TestHarness, name: &str, args: Value) -> Value {
        req(h, "tools/call", json!({ "name": name, "arguments": args })).await
    }
    async fn poll_status(h: &TestHarness, id: &str, want: &str, c: &ClientCapabilities) -> Value {
        for _ in 0..25 {
            let p = req_c(h, "tasks/get", json!({ "taskId": id }), c).await;
            if p["result"]["status"] == want {
                return p;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        panic!("task {id} did not reach {want}");
    }

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(DEFAULT_PACK_ID, "default");

    let (_plane_lock, _plane_env, plane_home) = gmcp_http_home_guard("020-prod");
    struct RestoreSdkPlane(PathBuf);
    impl Drop for RestoreSdkPlane {
        fn drop(&mut self) {
            for t in [topics::TOOLS_LIST, topics::TOOLS_EXISTS, topics::TOOLS_EXECUTE] {
                PlaneBus::global().unregister(t);
            }
            susi_daemon::composition::wire_mcp_serve_substrate(&self.0.join(".susi"));
        }
    }
    let _restore_sdk_plane = RestoreSdkPlane(plane_home);
    struct AmbientPlane;
    impl PlaneHandler for AmbientPlane {
        fn handle(&self, topic: &str, p: Value) -> Result<Value, String> {
            let ok = p.get("name").and_then(Value::as_str) == Some("ambient_host_probe");
            match topic {
                topics::TOOLS_LIST => Ok(json!([{ "name": "ambient_host_probe", "description": "Ambient" }])),
                topics::TOOLS_EXISTS => Ok(json!({ "exists": ok })),
                topics::TOOLS_EXECUTE if ok => Ok(json!({ "text": "ambient-host-ok" })),
                _ => Err("unknown ambient tool".into()),
            }
        }
    }
    let ambient: Arc<dyn PlaneHandler> = Arc::new(AmbientPlane);
    for t in [topics::TOOLS_LIST, topics::TOOLS_EXISTS, topics::TOOLS_EXECUTE] {
        PlaneBus::global().register(t, Arc::clone(&ambient));
    }

    let scratch = std::env::temp_dir().join(format!("gmcp-203-020-prod-{}-{}", std::process::id(), susi_gmcp::mcp::task_manager::uuid_v4()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("rt");

    rt.block_on(async {
        let minimal_ws = scratch.join("minimal-ws");
        std::fs::create_dir_all(&minimal_ws).expect("ws");
        let min_h = TestHarness::spawn(minimal_echo_server(minimal_ws.clone()).expect("min"));
        let disc = req(&min_h, "server/discover", json!({})).await;
        assert_eq!(disc["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "sdk-minimal-echo");
        assert_eq!(disc["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["version"], "0.1.0");
        assert_eq!((disc["result"]["ttlMs"].as_u64(), disc["result"]["cacheScope"].as_str()), (Some(0), Some("private")));
        let listed = req(&min_h, "tools/list", json!({})).await;
        let names: Vec<_> = listed["result"]["tools"].as_array().expect("tools").iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, vec!["echo"]);
        assert_eq!(call(&min_h, "ambient_host_probe", json!({})).await["error"]["code"], -32602);

        let adapted_h = TestHarness::spawn(ServerBuilder::new("adapted", "0.1.0", minimal_ws).with_host_tools(true).build().expect("adapted"));
        assert!(req(&adapted_h, "tools/list", json!({})).await["result"]["tools"].as_array().expect("tools").iter().any(|t| t["name"] == "ambient_host_probe"));
        assert_eq!(call(&adapted_h, "ambient_host_probe", json!({})).await["result"]["content"][0]["text"], "ambient-host-ok");
        adapted_h.shutdown().await;

        assert_eq!(call(&min_h, "echo", json!({ "message": "hello sdk", "uppercase": true })).await["result"]["content"][0]["text"], "HELLO SDK");
        let bad_echo = call(&min_h, "echo", json!({ "uppercase": true })).await;
        assert!(bad_echo["result"]["isError"] == true && bad_echo["result"]["content"][0]["text"].as_str().expect("err").contains("inputSchema"));
        min_h.shutdown().await;

        let (prod_ws, task_store) = (scratch.join("prod-ws"), scratch.join("task-store"));
        std::fs::create_dir_all(&prod_ws).expect("prod ws");
        let (prod_svc, audit_log) = production_reference_server(prod_ws.clone(), task_store.clone()).expect("prod");
        let harness = TestHarness::spawn(prod_svc.clone());

        let sum = call(&harness, "summarize_items", json!({ "items": ["alpha", "beta", "gamma"], "scope": { "namespace": "core", "tag": "v1" }, "maxItems": 2 })).await;
        assert!(sum["result"]["isError"] == false && sum["result"]["structuredContent"] == json!({ "count": 2, "summary": "alpha, beta", "namespace": "core" }));
        assert_eq!(audit_log.read().clone(), vec!["mw1:before:summarize_items", "mw2:before:summarize_items", "handler:summarize_items", "mw2:after:summarize_items", "mw1:after:summarize_items"]);
        audit_log.write().clear();
        assert!(call(&harness, "summarize_items", json!({ "items": "wrong-type" })).await["result"]["isError"] == true && audit_log.read().is_empty());

        let mut strict_tool = Tool::new_with_raw("strict_out", None, serde_json::Map::from_iter([("type".into(), json!("object"))]));
        strict_tool.output_schema = Some(Arc::new(serde_json::Map::from_iter([("type".into(), json!("object")), ("properties".into(), json!({ "ok": { "type": "boolean" } })), ("required".into(), json!(["ok"]))])));
        prod_svc.register_tool(strict_tool, Arc::new(|_r, _c| Box::pin(async { Ok(CallToolResult::structured(json!({"ok": "bad"})).into()) }))).expect("strict_out");
        let out_err = call(&harness, "strict_out", json!({})).await;
        assert!(out_err["result"]["isError"] == true && out_err["result"]["content"][0]["text"].as_str().expect("msg").contains("outputSchema"));

        // Resources, RFC 6570 URI templates, prompts, and bounded completions (`SDK-004`, `SDK-005`).
        assert!(req(&harness, "resources/read", json!({ "uri": "memo://welcome" })).await["result"]["contents"][0]["text"].as_str().expect("w").contains("Welcome"));
        let tpl_res = req(&harness, "resources/read", json!({ "uri": "memo://items/item-77" })).await;
        assert_eq!(serde_json::from_str::<Value>(tpl_res["result"]["contents"][0]["text"].as_str().expect("i")).expect("j"), json!({"id": "item-77", "status": "active"}));
        assert!(req(&harness, "resources/read", json!({ "uri": "memo://secret/credentials" })).await["error"].is_object());
        assert!(req(&harness, "prompts/get", json!({ "name": "draft_brief", "arguments": {} })).await["error"].is_object());
        assert!(req(&harness, "prompts/get", json!({ "name": "draft_brief", "arguments": { "topic": "Plugin Isolation" } })).await["result"]["messages"][0]["content"]["text"].as_str().expect("p").contains("Plugin Isolation"));
        let comp = req(&harness, "completion/complete", json!({ "ref": { "type": "ref/prompt", "name": "draft_brief" }, "argument": { "name": "topic", "value": "a" } })).await;
        assert_eq!(comp["result"]["completion"]["values"], json!(["architecture", "audit", "availability"]));

        // Inline MRTR (`SDK-007`) + durable task MRTR (`SDK-008`).
        assert!(call(&harness, "confirm_action", json!({ "operation": "rotate-keys" })).await["error"].is_object());
        let mut mrtr_caps = ClientCapabilities::builder().enable_tasks().build();
        mrtr_caps.elicitation = Some(ElicitationCapability::default());
        let step1 = req_c(&harness, "tools/call", json!({ "name": "confirm_action", "arguments": { "operation": "rotate-keys" } }), &mrtr_caps).await;
        assert_eq!((step1["result"]["resultType"].as_str(), step1["result"]["inputRequests"]["confirm"]["method"].as_str(), step1["result"]["requestState"].as_str()), (Some("input_required"), Some("elicitation/create"), Some("op:rotate-keys")));
        let step2 = req_c(&harness, "tools/call", json!({ "name": "confirm_action", "arguments": { "operation": "rotate-keys" }, "requestState": "op:rotate-keys", "inputResponses": { "confirm": { "approved": true } } }), &mrtr_caps).await;
        assert_eq!(step2["result"]["structuredContent"], json!({ "operation": "rotate-keys", "status": "confirmed" }));

        let spawned = req_c(&harness, "tools/call", json!({ "name": "approval_workflow", "arguments": { "ticket": "CHG-203" } }), &mrtr_caps).await;
        let task_id = spawned["result"]["taskId"].as_str().expect("taskId").to_string();
        poll_status(&harness, &task_id, "input_required", &mrtr_caps).await;
        assert!(req_c(&harness, "tasks/update", json!({ "taskId": task_id, "inputResponses": { "approve": { "decision": "approved" } } }), &mrtr_caps).await["result"].is_object());
        let done_task = poll_status(&harness, &task_id, "completed", &mrtr_caps).await;
        assert_eq!(done_task["result"]["result"]["structuredContent"], json!({ "ticket": "CHG-203", "decision": "approved" }));
        let reopened = GmcpService::open_durable(prod_ws.clone(), task_store.clone()).expect("reopen");
        assert_eq!(reopened.tasks.get_task(&task_id).expect("persisted").task.status, TaskStatus::Completed);

        // Namespaced extensions (`SDK-009`), dual-era subscription + atomic hot reload (`SDK-006`, `SDK-013`).
        assert_eq!(req(&harness, "server/discover", json!({})).await["result"]["capabilities"]["extensions"]["io.susi.sdk/reference"]["profile"], "production-reference");
        {
            let ext_reg = prod_svc.extensions();
            let guard = ext_reg.read();
            let peer = BTreeMap::from([("io.susi.sdk/reference".into(), serde_json::Map::new()), ("io.unknown.vendor/custom".into(), serde_json::Map::new())]);
            let neg = guard.negotiate(&peer, &["io.susi.sdk/reference".into()]).expect("neg");
            assert!(neg.active.contains_key("io.susi.sdk/reference") && neg.unknown.contains_key("io.unknown.vendor/custom"));
            assert!(guard.negotiate(&peer, &["io.missing/ext".into()]).is_err());
        }

        let mut legacy = TestHarness::spawn(prod_svc.clone());
        assert_eq!(legacy.initialize_legacy("2025-11-25", ClientCapabilities::default()).await.expect("init")["result"]["serverInfo"]["name"], "sdk-production-ref");
        assert!(legacy.request_session("resources/subscribe", json!({ "uri": "memo://welcome" })).await.expect("sub")["result"].is_object());
        while legacy.next_notification(Duration::from_millis(20)).await.is_some() {}

        let ((entered_tx, entered_rx), (release_tx, release_rx)) = (tokio::sync::oneshot::channel(), tokio::sync::oneshot::channel());
        let (entered_slot, release_slot) = (Mutex::new(Some(entered_tx)), Mutex::new(Some(release_rx)));
        prod_svc
            .register_tool(
                Tool::new_with_raw("inflight_op", None, serde_json::Map::from_iter([("type".into(), json!("object"))])),
                Arc::new(move |_r, _c| {
                    let (e, r) = (entered_slot.lock().expect("l").take(), release_slot.lock().expect("l").take());
                    Box::pin(async move {
                        if let Some(tx) = e { let _ = tx.send(()); }
                        if let Some(rx) = r { let _ = rx.await; }
                        Ok(CallToolResult::success(vec![ContentBlock::text("v1-snapshot")]).into())
                    })
                }),
            )
            .expect("inflight");
        while legacy.next_notification(Duration::from_millis(20)).await.is_some() {}

        let bad_schema = serde_json::Map::from_iter([("$ref".into(), json!("https://evil.example/bad.json"))]);
        assert!(prod_svc.reload_catalog(CatalogUpdate::new().with_tool(Tool::new_with_raw("bad_reload", None, bad_schema), Arc::new(|_r, _c| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) })))).is_err());

        let inflight_h = TestHarness::spawn(prod_svc.clone());
        let inflight_join = tokio::spawn(async move { let r = call(&inflight_h, "inflight_op", json!({})).await; inflight_h.shutdown().await; r });
        entered_rx.await.expect("entered");

        let reload_update = CatalogUpdate::new()
            .with_tool(
                Tool::new_with_raw("reloaded_v2", None, serde_json::Map::from_iter([("type".into(), json!("object"))])),
                Arc::new(|_r, _c| Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("v2-live")]).into()) })),
            )
            .with_resource(
                Resource::new("memo://welcome", "welcome").with_mime_type("text/plain"),
                Arc::new(|req, _c| {
                    Box::pin(async move {
                        let contents = vec![ResourceContents::TextResourceContents { uri: req.uri, mime_type: Some("text/plain".into()), text: "Reloaded welcome v2".into(), meta: None }];
                        Ok(ReadResourceResult { result_type: None, ttl_ms: Some(0), cache_scope: None, contents, meta: None }.into())
                    })
                }),
            );
        let reload_report = prod_svc.reload_catalog(reload_update).expect("reload");
        assert!(reload_report.tools_changed && reload_report.updated_uris == vec!["memo://welcome"]);
        let _ = release_tx.send(());
        assert_eq!(inflight_join.await.expect("join")["result"]["content"][0]["text"], "v1-snapshot");
        assert_eq!(call(&harness, "reloaded_v2", json!({})).await["result"]["content"][0]["text"], "v2-live");

        let mut notifs = BTreeSet::new();
        while let Some(n) = legacy.next_notification(Duration::from_millis(120)).await {
            if let Some(m) = n["method"].as_str() { notifs.insert(m.to_string()); }
        }
        assert!(notifs.contains("notifications/tools/list_changed") && notifs.contains("notifications/resources/updated"));

        // 3. Transactional plugin lifecycle (`SDK-014..SDK-016`).
        let pack_dir = scratch.join("sample-plugin-pack");
        std::fs::create_dir_all(pack_dir.join("catalog")).expect("pack dir");
        let artifact_bytes = br#"{"tools":["plugin_lookup","plugin_watch"]}"#;
        let artifact_path = pack_dir.join("catalog/plugin.json");
        std::fs::write(&artifact_path, artifact_bytes).expect("artifact");
        let digest_hex: String = Sha256::digest(artifact_bytes).iter().map(|b| format!("{b:02x}")).collect();
        let files = BTreeMap::from([("catalog".to_string(), "catalog/plugin.json".to_string())]);
        let checksums = BTreeMap::from([("catalog".to_string(), format!("sha256:{digest_hex}"))]);
        let mk_bundle = || sample_plugin_bundle(Some(pack_dir.clone()), checksums.clone(), files.clone());
        std::fs::write(pack_dir.join("manifest.json"), serde_json::to_vec_pretty(&mk_bundle().manifest).expect("j")).expect("manifest");

        assert!(matches!(prod_svc.attach_plugin(mk_bundle(), &PluginPolicy::strict()), Err(PluginError::PermissionDenied(..))));
        let policy = PluginPolicy::strict().grant_permission("filesystem.read");
        assert!(matches!(prod_svc.attach_plugin(mk_bundle(), &policy.clone().without_host_capability("gmcp.tasks.v1")), Err(PluginError::IncompatibleCapability(..))));

        std::fs::write(&artifact_path, b"{\"tampered\":true}").expect("tamper");
        assert!(matches!(prod_svc.attach_plugin(mk_bundle(), &policy), Err(PluginError::IntegrityFailed(_))));
        std::fs::write(&artifact_path, artifact_bytes).expect("restore");

        assert!(matches!(prod_svc.attach_plugin(mk_bundle().with_initializer(Arc::new(|_| Err("boom".into()))), &policy), Err(PluginError::InitFailed(..))));
        assert!(prod_svc.loaded_plugins().is_empty() && !prod_svc.extensions().read().contains("io.susi.plugin/sample"));

        let tools_before = req(&harness, "tools/list", json!({})).await["result"]["tools"].clone();
        let (unloaded_flag, watcher_stopped) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let (w_init, u_cb) = (Arc::clone(&watcher_stopped), Arc::clone(&unloaded_flag));
        let loaded = prod_svc
            .attach_plugin(
                mk_bundle()
                    .with_initializer(Arc::new(move |ct| {
                        let (ct, s) = (ct.clone(), Arc::clone(&w_init));
                        tokio::spawn(async move { ct.cancelled().await; s.store(true, Ordering::Release); });
                        Ok(())
                    }))
                    .with_on_unload(Arc::new(move || u_cb.store(true, Ordering::Release))),
                &policy,
            )
            .expect("attach");
        assert_eq!((loaded.id.as_str(), loaded.missing_optional.as_slice()), ("sdk-sample-plugin", &["gmcp.telemetry.v99".to_string()][..]));
        assert!(loaded.verified_digests.contains_key("manifest.json") && loaded.verified_digests.contains_key("catalog"));

        assert_eq!(call(&harness, "plugin_lookup", json!({ "key": "alpha" })).await["result"]["content"][0]["text"], "plugin:alpha");
        assert!(req(&harness, "resources/read", json!({ "uri": "plugin://status" })).await["result"]["contents"][0]["text"].as_str().expect("s").contains("sdk-sample-plugin"));
        assert!(req(&harness, "prompts/get", json!({ "name": "plugin_help" })).await["result"]["messages"][0]["content"]["text"].as_str().expect("h").contains("sdk-sample-plugin is active"));

        let p_task_id = req_c(&harness, "tools/call", json!({ "name": "plugin_watch", "arguments": { "key": "bg" } }), &mrtr_caps).await["result"]["taskId"].as_str().expect("id").to_string();
        assert_eq!(prod_svc.tasks.running_task_count(), 1);

        let unload = prod_svc.unload_plugin("sdk-sample-plugin").expect("unload");
        assert_eq!((unload.removed_tools, unload.removed_resources, unload.removed_prompts, unload.removed_extensions, unload.cancelled_tasks), (2, 1, 1, 1, 1));
        assert!(unloaded_flag.load(Ordering::Acquire));
        for _ in 0..20 {
            if watcher_stopped.load(Ordering::Acquire) && prod_svc.tasks.running_task_count() == 0 { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(watcher_stopped.load(Ordering::Acquire) && prod_svc.tasks.running_task_count() == 0);
        assert_eq!(prod_svc.tasks.get_task(&p_task_id).expect("cancelled").task.status, TaskStatus::Cancelled);
        assert_eq!(req(&harness, "tools/list", json!({})).await["result"]["tools"], tools_before);
        assert!(!prod_svc.extensions().read().contains("io.susi.plugin/sample"));

        prod_svc.attach_plugin(mk_bundle(), &policy).expect("re-attach");
        prod_svc.unload_plugin("sdk-sample-plugin").expect("second unload");
        legacy.shutdown().await;
        harness.shutdown().await;
    });

    let _ = std::fs::remove_dir_all(&scratch);
    let sdk_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/sdk.rs")).expect("sdk.rs");
    for forbidden in ["extern crate rmcp", "schemars::", "axum::", "tower::", "unsafe "] {
        assert!(!sdk_src.contains(forbidden), "forbidden token `{forbidden}` in sdk.rs");
    }
    assert!(root.join(".agents/gmcp/evidence/VC-203-020.jsonl").is_file());
}

#[test]
fn gmcp_req_203_020_03_integration() {
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::{collections::BTreeMap, io::{Read, Write}, net::{TcpListener, TcpStream}, sync::{atomic::{AtomicBool, Ordering}, Arc}, time::Duration};
    use susi_gmcp::{
        direct_call::call_tool_direct,
        mcp::model::{CallToolResult, ClientCapabilities, ContentBlock, TaskStatus, Tool},
        protocol::{GmcpService, McpGatewayProxy},
        sdk::{examples::{production_reference_server, sample_plugin_bundle}, PluginPolicy, TestHarness},
        server::GmcpServer,
        susi_config::extensions as pack_ext,
        tools::CoreTools,
        GmcpHost,
    };
    use susi_wire_client::mcp::{Session, PROTOCOL_VERSION_2026_07_28};

    let (_lock, _env, home) = gmcp_http_home_guard("020-03-integ");
    let (root, ws, tasks_dir, cfg_dir) = (Path::new(env!("CARGO_MANIFEST_DIR")), home.join("ws"), home.join("durable-tasks"), susi_paths::SusiDirs::config_dir());
    std::fs::create_dir_all(&ws).expect("ws");
    std::fs::create_dir_all(&cfg_dir).expect("cfg");
    let token = "susi-020-03-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).expect("token");
    pack_ext::invalidate_extension_caches();

    let (daemon_src, server_src, mcp_cli_src) = (
        std::fs::read_to_string(root.join("crates/susi-daemon/src/server.rs")).expect("daemon"),
        std::fs::read_to_string(root.join("crates/susi-gmcp/src/server.rs")).expect("server"),
        std::fs::read_to_string(root.join("src/cli/mcp_cli.rs")).expect("mcp_cli"),
    );
    assert!(daemon_src.contains("Supervisor::global().spawn(\"gmcp-http\"") && daemon_src.contains("start_http_server"));
    assert!(server_src.contains("ServerBuilder::from_identity") && mcp_cli_src.contains("call_tool_direct"));

    let (sdk_svc, audit_log) = production_reference_server(ws.clone(), tasks_dir.clone()).expect("sdk server");
    let probe_schema = serde_json::Map::from_iter([("type".into(), json!("object"))]);
    sdk_svc.register_tool(Tool::new_with_raw("susi_restricted_sdk_probe", Some("Privileged probe".into()), probe_schema), Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("bypass")]).into()) }))).expect("probe");
    sdk_svc.bind_direct_server("sdk-integ").expect("bind direct");

    let wait_port = |l: TcpListener, f: Box<dyn FnOnce(Vec<TcpListener>) + Send>| -> String {
        let addr = format!("127.0.0.1:{}", l.local_addr().expect("addr").port());
        std::thread::spawn(move || f(vec![l]));
        for _ in 0..80 { if TcpStream::connect(&addr).is_ok() { break; } std::thread::sleep(Duration::from_millis(10)); }
        addr
    };
    let sdk_http = sdk_svc.clone();
    let sdk_addr = wait_port(TcpListener::bind("127.0.0.1:0").expect("bind"), Box::new(move |ls| GmcpServer::serve_http_service(sdk_http, ls, None, false)));
    let raw_http = |path: &str, hdrs: &[(&str, &str)], body: &str| -> String {
        let mut s = TcpStream::connect(&sdk_addr).expect("tcp");
        s.set_read_timeout(Some(Duration::from_secs(10))).expect("to");
        let mut r = format!("POST {path} HTTP/1.1\r\nHost: {sdk_addr}\r\nConnection: close\r\n");
        for (k, v) in hdrs { r.push_str(&format!("{k}: {v}\r\n")); }
        r.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        s.write_all(r.as_bytes()).expect("write");
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    };
    let disc_body = json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":PROTOCOL_VERSION_2026_07_28,"io.modelcontextprotocol/clientCapabilities":{}}}}).to_string();
    let bh = [("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream"), ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28), ("Mcp-Method", "server/discover")];
    let bearer = format!("Bearer {token}");
    assert!(raw_http("/mcp", &bh, &disc_body).starts_with("HTTP/1.1 401"));
    assert!(raw_http("/mcp", &[bh[0], bh[1], bh[2], bh[3], ("Authorization", &bearer), ("Origin", "https://evil.example")], &disc_body).starts_with("HTTP/1.1 403"));
    assert!(raw_http("/messages", &[bh[0], bh[1], bh[2], bh[3], ("Authorization", &bearer)], &disc_body).starts_with("HTTP/1.1 200"));

    let mut wire = Session::open_with_capabilities(&sdk_addr, Some(token), &json!({ "elicitation": {}, "extensions": { "io.modelcontextprotocol/tasks": {}, "io.susi.sdk/reference": { "version": 1 } } }), Duration::from_secs(15)).expect("wire");
    assert!(wire.server_extensions().expect("ext").contains_key("io.susi.sdk/reference"));

    let sum_args = json!({ "items": ["alpha", "beta", "gamma"], "scope": { "namespace": "ops" }, "maxItems": 2 });
    let (direct_ok, http_ok) = (call_tool_direct("sdk-integ", "summarize_items", &sum_args, &ws).expect("dok"), wire.call_tool("summarize_items", &sum_args, None, &mut |_| {}).expect("hok"));
    assert_eq!(direct_ok["isError"], false);
    assert_eq!(serde_json::from_str::<Value>(direct_ok["content"][0]["text"].as_str().expect("t")).expect("j"), http_ok["structuredContent"]);
    assert_eq!(audit_log.read().len(), 10);
    assert!(call_tool_direct("sdk-integ", "summarize_items", &json!({"items": "bad"}), &ws).expect("dbad")["isError"] == true && wire.call_tool("summarize_items", &json!({"items": "bad"}), None, &mut |_| {}).expect("hbad")["isError"] == true && audit_log.read().len() == 10);

    assert!(McpGatewayProxy::evaluate_tool_policy("susi_restricted_sdk_probe").is_err());
    for v in [
        call_tool_direct("sdk-integ", "susi_restricted_sdk_probe", &json!({}), &ws).expect("d_pol"),
        call_tool_direct("susi", "susi_restricted_sdk_probe", &json!({}), &ws).expect("c_pol"),
        wire.call_tool("susi_restricted_sdk_probe", &json!({}), None, &mut |_| {}).expect("h_pol"),
    ] {
        assert!(v["isError"] == true && v["content"][0]["text"].as_str().expect("p").starts_with("Governance Violation:"));
    }

    assert_eq!(susi_json(root, &home, &["extensions", "create", "sdk-sample-plugin"])["id"], "sdk-sample-plugin");
    let pack_dir = cfg_dir.join("extensions").join("sdk-sample-plugin");
    std::fs::create_dir_all(pack_dir.join("catalog")).expect("catalog dir");
    let art_bytes = br#"{"tools":["plugin_lookup","plugin_watch"]}"#;
    std::fs::write(pack_dir.join("catalog/plugin.json"), art_bytes).expect("art");
    let digest: String = Sha256::digest(art_bytes).iter().map(|b| format!("{b:02x}")).collect();
    let (files, csums) = (BTreeMap::from([("catalog".to_string(), "catalog/plugin.json".to_string())]), BTreeMap::from([("catalog".to_string(), format!("sha256:{digest}"))]));
    std::fs::write(pack_dir.join("manifest.json"), serde_json::to_vec_pretty(&sample_plugin_bundle(Some(pack_dir.clone()), csums.clone(), files.clone()).manifest).expect("m")).expect("wm");

    susi_daemon::composition::wire_mcp_serve_substrate(&home.join(".susi"));
    assert!(GmcpHost::dispatch("read_file", r#"{"path":"Cargo.toml"}"#, root).contains("susi"));
    let val_arg = json!({ "path": pack_dir.to_str().expect("p") });
    let dc_val_raw = call_tool_direct("susi", "extensions_validate", &val_arg, root).expect("dc");
    let (cli_val, ct_val, dc_val): (Value, Value, Value) = (
        susi_json(root, &home, &["extensions", "validate", pack_dir.to_str().expect("p")]),
        serde_json::from_str(&CoreTools::extensions_validate(&val_arg, root).expect("ct")).expect("j"),
        serde_json::from_str(dc_val_raw["content"][0]["text"].as_str().expect("t")).expect("dj"),
    );
    assert!(cli_val["valid"] == true && cli_val == ct_val && ct_val == dc_val);
    let dc_load = call_tool_direct("susi", "extensions_load", &json!({"id":"sdk-sample-plugin"}), root).expect("load");
    assert_eq!(serde_json::from_str::<Value>(dc_load["content"][0]["text"].as_str().expect("t")).expect("lj")["active"], "sdk-sample-plugin");
    assert_eq!(susi_json(root, &home, &["extensions", "status"])["active"], "sdk-sample-plugin");

    let (mut stdio, daemon_root) = (StdioSession::spawn_modern(root, &home), root.to_path_buf());
    let daemon_addr = wait_port(TcpListener::bind("127.0.0.1:0").expect("bd"), Box::new(move |ls| GmcpServer::start_http_server(daemon_root, ls, None, false)));
    let mut daemon_wire = Session::open_with_capabilities(&daemon_addr, Some(token), &json!({}), Duration::from_secs(15)).expect("dw");
    assert_eq!(stdio.request("server/discover", adapter_modern_meta())["capabilities"]["extensions"], Value::Object(daemon_wire.server_extensions().expect("de").clone()));
    assert_eq!(serde_json::from_str::<Value>(daemon_wire.call_tool("extensions_status", &json!({}), None, &mut |_| {}).expect("es")["content"][0]["text"].as_str().expect("t")).expect("j")["active"], "sdk-sample-plugin");
    drop(stdio);
    daemon_wire.close().expect("cdw");

    let (policy, unloaded_cb) = (PluginPolicy::strict().grant_permission("filesystem.read"), Arc::new(AtomicBool::new(false)));
    let u_cb = Arc::clone(&unloaded_cb);
    let synced = sdk_svc.sync_active_pack(&policy, |base| {
        let mut b = sample_plugin_bundle(Some(pack_dir.clone()), csums.clone(), files.clone());
        b.manifest = base.manifest;
        b.with_on_unload(Arc::new(move || u_cb.store(true, Ordering::Release)))
    }).expect("sync load").expect("loaded report");
    assert_eq!(synced.id, "sdk-sample-plugin");
    sdk_svc.bind_direct_server("sdk-integ").expect("rebind direct");

    assert_eq!(call_tool_direct("sdk-integ", "plugin_lookup", &json!({"key": "cli"}), &ws).expect("d")["content"][0]["text"], "plugin:cli");
    assert_eq!(wire.call_tool("plugin_lookup", &json!({"key": "http"}), None, &mut |_| {}).expect("h")["content"][0]["text"], "plugin:http");
    assert!(wire.request("resources/read", &json!({"uri": "plugin://status"}), &mut |_| {}).expect("r")["contents"][0]["text"].as_str().expect("s").contains("sdk-sample-plugin"));
    assert!(wire.request("prompts/get", &json!({"name": "plugin_help"}), &mut |_| {}).expect("p")["messages"][0]["content"]["text"].as_str().expect("t").contains("active"));

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("rt");
    let persisted_task_id = rt.block_on(async {
        let h = TestHarness::spawn(sdk_svc.clone());
        let caps = serde_json::from_value::<ClientCapabilities>(json!({ "elicitation": {}, "extensions": { "io.modelcontextprotocol/tasks": {} } })).expect("caps");
        assert_eq!(h.request_modern("resources/read", json!({"uri": "memo://items/42"}), Default::default()).await.expect("r")["result"]["contents"][0]["text"], r#"{"id":"42","status":"active"}"#);
        assert!(h.request_modern("resources/read", json!({"uri": "memo://secret/x"}), Default::default()).await.expect("g")["error"]["message"].as_str().expect("m").contains("denied"));
        assert_eq!(h.request_modern("completion/complete", json!({"ref": {"type": "ref/prompt", "name": "draft_brief"}, "argument": {"name": "topic", "value": "sd"}}), Default::default()).await.expect("c")["result"]["completion"]["values"], json!(["sdk"]));
        let _ = h.request_modern("tools/call", json!({"name": "plugin_watch", "arguments": {"key": "bg"}}), caps.clone()).await.expect("watch");
        assert_eq!(sdk_svc.tasks.running_task_count(), 1);
        let tid = h.request_modern("tools/call", json!({"name": "approval_workflow", "arguments": {"ticket": "CHG-109"}}), caps.clone()).await.expect("wf")["result"]["taskId"].as_str().expect("tid").to_string();
        for _ in 0..40 { if h.request_modern("tasks/get", json!({"taskId": tid}), caps.clone()).await.expect("g")["result"]["status"] == "input_required" { break; } tokio::time::sleep(Duration::from_millis(10)).await; }
        let _ = h.request_modern("tasks/update", json!({"taskId": tid, "inputResponses": {"approve": {"decision": "ship"}}}), caps.clone()).await.expect("u");
        for _ in 0..40 { if h.request_modern("tasks/get", json!({"taskId": tid}), caps.clone()).await.expect("g")["result"]["status"] == "completed" { break; } tokio::time::sleep(Duration::from_millis(10)).await; }
        h.shutdown().await;
        tid
    });

    assert_eq!(susi_json(root, &home, &["extensions", "unload", "sdk-sample-plugin"])["active"], "default");
    assert!(sdk_svc.sync_active_pack(&policy, |b| b).expect("sync unload").is_none());
    sdk_svc.bind_direct_server("sdk-integ").expect("rebind after unload");
    assert!(unloaded_cb.load(Ordering::Acquire) && sdk_svc.loaded_plugins().is_empty());
    assert!(wire.call_tool("plugin_lookup", &json!({"key": "gone"}), None, &mut |_| {}).is_err());
    wire.close().expect("close sdk wire");
    drop(sdk_svc);

    let (restarted_svc, _) = production_reference_server(ws, tasks_dir).expect("restarted server");
    assert_eq!(restarted_svc.tasks.get_task(&persisted_task_id).expect("persisted").task.status, TaskStatus::Completed);
    rt.block_on(async {
        let h2 = TestHarness::spawn(restarted_svc);
        let caps = serde_json::from_value::<ClientCapabilities>(json!({ "extensions": { "io.modelcontextprotocol/tasks": {} } })).expect("caps");
        assert_eq!(h2.request_modern("tasks/get", json!({"taskId": persisted_task_id}), caps).await.expect("res")["result"]["result"]["structuredContent"]["decision"], "ship");
        h2.shutdown().await;
    });
    GmcpService::unbind_direct_server("sdk-integ");
    assert!(root.join(".agents/gmcp/evidence/VC-203-020.jsonl").is_file() && root.join(".agents/roadmap-verdicts/VC-203-020.json").is_file());
}

/// GMCP-203-020-04: adversarial and failure behavior across validation, authorization,
/// isolation, privacy, bounded-resource, cancellation, concurrency, crash, retry,
/// rollback, and recovery (`docs/gmcp/contract-203-020.md` Threat Model #1..#10).
#[test]
fn gmcp_req_203_020_04_adversarial() {
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use std::time::Duration;
    use susi_gmcp::{
        extensions::ExtensionStatus,
        mcp::{
            model::{
                CallToolResult, ClientCapabilities, CompleteResult, CompletionInfo, ContentBlock,
                ReadResourceResult, Resource, ResourceContents, TaskStatus, Tool,
            },
            task_manager::{
                TaskExit, TaskOptions, TaskPrincipal, MAX_ACTIVE_TASKS_PER_PRINCIPAL,
                MAX_INPUT_RESPONSE_BYTES,
            },
            ErrorData,
        },
        protocol::{GmcpService, MAX_RESOURCE_BYTES},
        sdk::{
            examples::{production_reference_server, sample_plugin_bundle},
            CatalogUpdate, PluginBundle, PluginError, PluginPolicy, TestHarness,
        },
        susi_config::extensions as pack_ext,
    };

    async fn req(h: &TestHarness, m: &str, p: serde_json::Value) -> serde_json::Value {
        h.request_modern(m, p, Default::default()).await.expect("rpc")
    }

    let (_lock, _env, home) = gmcp_http_home_guard("020-04-adv");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (ws, tasks_dir, cfg_dir) = (home.join("ws"), home.join("tasks"), susi_paths::SusiDirs::config_dir());
    std::fs::create_dir_all(&ws).expect("ws");
    std::fs::create_dir_all(&cfg_dir).expect("cfg");
    pack_ext::invalidate_extension_caches();

    let (svc, audit_log) = production_reference_server(ws.clone(), tasks_dir).expect("sdk");
    let noop: susi_gmcp::protocol::ToolHandler =
        Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }));
    let policy = PluginPolicy::strict().grant_permission("filesystem.read");

    // 1. Validation & bounded-resource rejection (depth bomb, unresolvable/file:// $ref, cyclic $defs).
    let mut deep = json!({ "type": "string" });
    for _ in 0..66 {
        deep = json!({ "type": "array", "items": deep });
    }
    let deep_map = serde_json::Map::from_iter([("type".into(), json!("object")), ("properties".into(), json!({ "x": deep }))]);
    let unres_map = serde_json::Map::from_iter([("$ref".into(), json!("#/$defs/Missing"))]);
    let file_ref = serde_json::Map::from_iter([("$ref".into(), json!("file:///etc/passwd"))]);
    for bad in [deep_map, unres_map, file_ref] {
        let t = Tool::new_with_raw("bad_schema", None, bad);
        assert!(svc.register_tool(t.clone(), Arc::clone(&noop)).is_err());
        assert!(svc.reload_catalog(CatalogUpdate::new().with_tool(t.clone(), Arc::clone(&noop))).is_err());
        let b = sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()).with_tool(t, Arc::clone(&noop));
        assert!(matches!(svc.attach_plugin(b, &policy), Err(PluginError::SchemaRejected(..))));
    }
    let cyc_map = serde_json::Map::from_iter([("$defs".into(), json!({ "L": { "$ref": "#/$defs/L" } })), ("$ref".into(), json!("#/$defs/L"))]);
    svc.register_tool(Tool::new_with_raw("cyc_schema", None, cyc_map), Arc::clone(&noop)).expect("cyc");
    let cyc_res = svc.call_tool_sync("cyc_schema", json!({})).expect("cyc call");
    assert!(cyc_res["result"]["isError"] == true && cyc_res["result"]["content"][0]["text"].as_str().expect("t").contains("inputSchema"));
    for reserved in ["", "susi", "native", "gmcp", "local", "builtin", "core"] {
        assert!(svc.bind_direct_server(reserved).is_err());
    }

    // 2. Principal secret hashing, cross-principal isolation, and bounded active task quotas.
    let raw_secret = "raw-top-secret-bearer-020";
    let (p_a, p_b) = (TaskPrincipal::from_bearer_token(raw_secret), TaskPrincipal::from_bearer_token("other-020"));
    assert!(!p_a.as_str().contains(raw_secret) && p_a != p_b);

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().build().expect("rt");
    rt.block_on(async {
        let wait_fn = |tc: susi_gmcp::mcp::task_manager::TaskContext| -> susi_gmcp::mcp::task_manager::TaskFuture {
            Box::pin(async move { tc.cancelled().await; Err(TaskExit::Cancelled) })
        };
        let t_a = svc.tasks.try_spawn_for(p_a.clone(), TaskOptions::default(), wait_fn).expect("a");
        assert!(svc.tasks.get_task_for(&t_a.task_id, &p_b).is_err());
        assert!(svc.tasks.update_task_for(&t_a.task_id, &p_b, []).is_err());
        assert!(svc.tasks.cancel_task_for(&t_a.task_id, &p_b).is_err());
        let ids_a = vec![t_a.task_id.clone()];
        assert!(svc.tasks.authorize_tasks_for(&p_b, &ids_a).is_err() && svc.tasks.snapshots_for(&p_b, &ids_a).is_empty());
        assert!(svc.tasks.update_task_for(&t_a.task_id, &p_a, [("k".into(), json!("x".repeat(MAX_INPUT_RESPONSE_BYTES + 16)))]).is_err());

        let mut b_ids = Vec::with_capacity(MAX_ACTIVE_TASKS_PER_PRINCIPAL);
        for _ in 0..MAX_ACTIVE_TASKS_PER_PRINCIPAL {
            b_ids.push(svc.tasks.try_spawn_for(p_b.clone(), TaskOptions::default(), wait_fn).expect("slot").task_id);
        }
        assert_eq!(svc.tasks.try_spawn_for(p_b.clone(), TaskOptions::default(), |_tc| Box::pin(async { Ok(CallToolResult::success(vec![])) })).expect_err("65th").code.0, -32000);
        let t_a2 = svc.tasks.try_spawn_for(p_a.clone(), TaskOptions::default(), |_tc| Box::pin(async { Ok(CallToolResult::success(vec![])) })).expect("a2");
        for id in &b_ids { svc.tasks.cancel_task_for(id, &p_b).expect("cb"); }
        svc.tasks.cancel_task_for(&t_a.task_id, &p_a).expect("ca");
        for _ in 0..30 { if svc.tasks.running_task_count() == 0 { break; } tokio::time::sleep(Duration::from_millis(10)).await; }
        assert_eq!(svc.tasks.running_task_count(), 0);
        assert!(svc.tasks.cancel_task_for(&t_a.task_id, &p_a).is_err() && svc.tasks.update_task_for(&t_a2.task_id, &p_a, []).is_err());

        // 3. Framing limit, missing _meta, resource traversal/sealing, completions & stale cursor.
        let tiny_h = TestHarness::spawn_with_limit(svc.clone(), 512);
        assert!(tiny_h.request_modern("server/discover", json!({ "pad": "Z".repeat(700) }), Default::default()).await.is_err());
        tiny_h.shutdown().await;

        let h = TestHarness::spawn(svc.clone());
        audit_log.write().clear();
        let no_meta = h.request_raw(json!({"jsonrpc":"2.0","id":901,"method":"tools/call","params":{"name":"summarize_items","arguments":{}}})).await.expect("nm");
        assert!(no_meta["error"].is_object() && audit_log.read().is_empty());
        assert_eq!(req(&h, "unknown/method", json!({})).await["error"]["code"], -32601);

        let long_uri = format!("memo://items/{}", "a".repeat(2100));
        for u in ["memo://items/../secret", "memo://items/%2e%2e/secret", "memo://bad space", long_uri.as_str()] {
            assert!(req(&h, "resources/read", json!({"uri": u})).await["error"].is_object());
        }
        let mk_res = |u: &str, c: Vec<ResourceContents>| {
            svc.register_resource(Resource::new(u, "adv"), Arc::new(move |_r, _x| {
                let c = c.clone();
                Box::pin(async move { Ok(ReadResourceResult { result_type: None, ttl_ms: None, cache_scope: None, contents: c, meta: None }.into()) })
            }));
        };
        mk_res("memo://adv-empty", vec![]);
        mk_res("memo://adv-b64", vec![ResourceContents::BlobResourceContents { uri: "memo://adv-b64".into(), mime_type: Some("application/octet-stream".into()), blob: "bad!!!".into(), meta: None }]);
        mk_res("memo://adv-mime", vec![ResourceContents::TextResourceContents { uri: "memo://adv-mime".into(), mime_type: Some("bad mime".into()), text: "x".into(), meta: None }]);
        mk_res("memo://adv-giant", vec![ResourceContents::TextResourceContents { uri: "memo://adv-giant".into(), mime_type: Some("text/plain".into()), text: "B".repeat(MAX_RESOURCE_BYTES + 1), meta: None }]);
        for u in ["memo://adv-empty", "memo://adv-b64", "memo://adv-mime", "memo://adv-giant"] {
            assert!(req(&h, "resources/read", json!({"uri": u})).await["error"].is_object());
        }

        svc.register_completion("adv_comp".into(), Arc::new(|_r, _c| Box::pin(async {
            let values = (0..150).map(|i| format!("v{i:03}")).collect();
            Ok(CompleteResult { result_type: None, completion: CompletionInfo { values, total: Some(150), has_more: Some(false) }, meta: None })
        })));
        let comp = req(&h, "completion/complete", json!({"ref":{"type":"ref/prompt","name":"adv_comp"},"argument":{"name":"x","value":""}})).await;
        assert!(comp["result"]["completion"]["values"].as_array().expect("v").len() == 100 && comp["result"]["completion"]["hasMore"] == true);

        let obj_schema = serde_json::Map::from_iter([("type".into(), json!("object"))]);
        let mut many = CatalogUpdate::new();
        for i in 0..102 { many = many.with_tool(Tool::new_with_raw(format!("pg_{i:03}"), None, obj_schema.clone()), Arc::clone(&noop)); }
        svc.reload_catalog(many).expect("102");
        let cursor = req(&h, "tools/list", json!({})).await["result"]["nextCursor"].as_str().expect("cur").to_string();
        svc.reload_catalog(CatalogUpdate::new().with_tool(Tool::new_with_raw("summarize_items", None, obj_schema.clone()), Arc::clone(&noop))).expect("1");
        assert!(req(&h, "tools/list", json!({"cursor": cursor})).await["error"].is_object());

        // 4. Subscription resource_guard filtering + dynamic guard tightening.
        mk_res("memo://welcome", vec![ResourceContents::TextResourceContents { uri: "memo://welcome".into(), mime_type: Some("text/plain".into()), text: "ok".into(), meta: None }]);
        let mut sub_h = TestHarness::spawn(svc.clone());
        assert!(sub_h.initialize_legacy("2025-11-25", ClientCapabilities::default()).await.is_ok());
        assert!(sub_h.request_session("resources/subscribe", json!({"uri": "memo://secret/keys"})).await.expect("d")["error"].is_object());
        assert!(sub_h.request_session("resources/subscribe", json!({"uri": "memo://welcome"})).await.expect("o")["result"].is_object());
        while sub_h.next_notification(Duration::from_millis(15)).await.is_some() {}
        svc.set_resource_guard(Arc::new(|u| (u != "memo://welcome" && !u.starts_with("memo://secret/")).then_some(()).ok_or_else(|| ErrorData::invalid_params("denied", None))));
        svc.resource_updated("memo://welcome".into());
        assert!(sub_h.next_notification(Duration::from_millis(80)).await.is_none());
        svc.set_resource_guard(Arc::new(|u| (!u.starts_with("memo://secret/")).then_some(()).ok_or_else(|| ErrorData::invalid_params("denied", None))));
        sub_h.shutdown().await;

        // 5. Fault-injection: panicking tool, panicking task, & in-flight cancellation drop.
        let att = Arc::new(AtomicUsize::new(0));
        let att_c = Arc::clone(&att);
        svc.register_tool(Tool::new_with_raw("flaky_panic", None, obj_schema.clone()), Arc::new(move |_r, _c| {
            let n = att_c.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { assert!(n > 0, "boom"); Ok(CallToolResult::success(vec![ContentBlock::text("retry-ok")]).into()) })
        })).expect("flaky");
        assert_eq!(req(&h, "tools/call", json!({"name": "flaky_panic", "arguments": {}})).await["error"]["code"], -32603);
        assert_eq!(req(&h, "tools/call", json!({"name": "flaky_panic", "arguments": {}})).await["result"]["content"][0]["text"], "retry-ok");

        let p_task = svc.tasks.spawn(TaskOptions::default(), |_tc| Box::pin(async { panic!("task boom") }));
        for _ in 0..30 { if svc.tasks.running_task_count() == 0 { break; } tokio::time::sleep(Duration::from_millis(10)).await; }
        assert!(svc.tasks.get_task(&p_task.task_id).expect("s").task.status == TaskStatus::Failed && svc.tasks.running_task_count() == 0);

        let (drop_guard, (etx, erx)) = (Arc::new(()), tokio::sync::oneshot::channel());
        let (eslot, dg) = (std::sync::Mutex::new(Some(etx)), Arc::clone(&drop_guard));
        svc.register_tool(Tool::new_with_raw("slow_op", None, obj_schema.clone()), Arc::new(move |_r, _c| {
            let (held, tx) = (Arc::clone(&dg), eslot.lock().expect("l").take());
            Box::pin(async move { if let Some(t) = tx { let _ = t.send(()); } tokio::time::sleep(Duration::from_secs(30)).await; drop(held); Ok(CallToolResult::success(vec![]).into()) })
        })).expect("slow");
        let bg_durable = svc.tasks.spawn(TaskOptions::default(), wait_fn);
        let hc = Arc::new(TestHarness::spawn(svc.clone()));
        let hc2 = Arc::clone(&hc);
        let c_req = json!({"jsonrpc":"2.0","id":955,"method":"tools/call","params":{"name":"slow_op","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}});
        let slow_fut = tokio::spawn(async move { hc2.request_raw(c_req).await });
        erx.await.expect("entered");
        assert_eq!(Arc::strong_count(&drop_guard), 3);
        hc.notify("notifications/cancelled", json!({ "requestId": 955 })).await.expect("cancel");
        for _ in 0..40 { if Arc::strong_count(&drop_guard) == 2 { break; } tokio::time::sleep(Duration::from_millis(10)).await; }
        assert!(Arc::strong_count(&drop_guard) == 2 && svc.tasks.get_task(&bg_durable.task_id).expect("bg").task.status == TaskStatus::Working);
        svc.tasks.cancel_task(&bg_durable.task_id).expect("clean bg");
        slow_fut.abort();
        let _ = slow_fut.await;
        assert!(req(&hc, "server/discover", json!({})).await["result"].is_object());

        // 6. Plugin negative admission, panicking hooks, & 8-way concurrency race.
        for bad_rel in ["NUL.json", "/etc/passwd"] {
            let mut m = sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()).manifest;
            m.files.insert("bad".into(), bad_rel.into());
            assert!(pack_ext::validate_manifest(&m).is_err());
        }
        let bare = PluginBundle::new(sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()).manifest);
        assert!(matches!(svc.attach_plugin(bare.with_extension("bare_ext", ExtensionStatus::Experimental, 1, Default::default()), &policy), Err(PluginError::ExtensionRejected(..))));
        let col = PluginBundle::new(sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()).manifest).with_tool(Tool::new_with_raw("flaky_panic", None, obj_schema.clone()), Arc::clone(&noop));
        assert!(matches!(svc.attach_plugin(col, &policy), Err(PluginError::Collision("tool", ..))));
        assert!(matches!(svc.unload_plugin("missing"), Err(PluginError::NotLoaded(_))));

        let p_init = sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()).with_initializer(Arc::new(|_| panic!("init boom")));
        assert!(matches!(svc.attach_plugin(p_init, &policy), Err(PluginError::InitFailed(_, ref m)) if m.contains("panicked")) && svc.loaded_plugins().is_empty());
        svc.attach_plugin(sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()).with_on_unload(Arc::new(|| panic!("unload boom"))), &policy).expect("attach");
        assert!(matches!(svc.attach_plugin(sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()), &policy), Err(PluginError::AlreadyLoaded(_))));
        svc.unload_plugin("sdk-sample-plugin").expect("unload isolated");
        assert!(svc.loaded_plugins().is_empty());

        let mut joins = Vec::new();
        for i in 0..8usize {
            let (s, pol, sch, nt) = (svc.clone(), policy.clone(), obj_schema.clone(), Arc::clone(&noop));
            joins.push(tokio::spawn(async move {
                let rh = TestHarness::spawn(s.clone());
                for _ in 0..4 {
                    match i % 4 {
                        0 => { let _ = req(&rh, "tools/list", json!({})).await; }
                        1 => { let _ = req(&rh, "resources/read", json!({"uri": "memo://welcome"})).await; }
                        2 => { let _ = s.reload_catalog(CatalogUpdate::new().with_tool(Tool::new_with_raw("flaky_panic", None, sch.clone()), Arc::clone(&nt))); }
                        _ => {
                            let mut b = PluginBundle::new(sample_plugin_bundle(None, BTreeMap::new(), BTreeMap::new()).manifest);
                            b.manifest.id = format!("race-plug-{i}");
                            if s.attach_plugin(b, &pol).is_ok() { let _ = s.unload_plugin(&format!("race-plug-{i}")); }
                        }
                    }
                }
                rh.shutdown().await;
            }));
        }
        for j in joins { j.await.expect("race"); }
        assert!(svc.loaded_plugins().is_empty());

        // 7. Ed25519 signed host pack quarantine, runtime eviction, state corruption & recovery.
        pack_ext::create_pack("adv-signed-pack").expect("create");
        let pack_dir = cfg_dir.join("extensions").join("adv-signed-pack");
        std::fs::create_dir_all(pack_dir.join("catalog")).expect("cat");
        let art_bytes = br#"{"tools":["plugin_lookup","plugin_watch"]}"#;
        std::fs::write(pack_dir.join("catalog/plugin.json"), art_bytes).expect("art");
        let digest: String = Sha256::digest(art_bytes).iter().map(|b| format!("{b:02x}")).collect();
        let (files, csums) = (BTreeMap::from([("catalog".into(), "catalog/plugin.json".into())]), BTreeMap::from([("catalog".into(), format!("sha256:{digest}"))]));
        let write_signed = |sk: &SigningKey| {
            let mut b = sample_plugin_bundle(Some(pack_dir.clone()), csums.clone(), files.clone());
            b.manifest.id = "adv-signed-pack".into();
            b.manifest.signer = Some(susi_paths::hex::encode(sk.verifying_key().to_bytes()));
            b.manifest.signatures.insert("catalog".into(), susi_paths::hex::encode(sk.sign(art_bytes).to_bytes()));
            std::fs::write(pack_dir.join("manifest.json"), serde_json::to_vec_pretty(&b.manifest).expect("j")).expect("wm");
        };
        write_signed(&SigningKey::from_bytes(&[7u8; 32]));
        assert!(pack_ext::load_pack("adv-signed-pack").expect("load").active);
        let decorate = |base: PluginBundle| { let mut b = sample_plugin_bundle(Some(pack_dir.clone()), csums.clone(), files.clone()); b.manifest = base.manifest; b };
        assert_eq!(svc.sync_active_pack(&policy, decorate).expect("s1").expect("l").id, "adv-signed-pack");
        let caps = ClientCapabilities::builder().enable_tasks().build();
        let p_tid = h.request_modern("tools/call", json!({"name": "plugin_watch", "arguments": {"key": "live"}}), caps).await.expect("w")["result"]["taskId"].as_str().expect("id").to_string();
        assert_eq!(svc.tasks.running_task_count(), 1);

        write_signed(&SigningKey::from_bytes(&[9u8; 32]));
        assert!(matches!(svc.sync_active_pack(&policy, |b| b), Err(PluginError::IntegrityFailed(ref m)) if m.contains("signer changed")));
        for _ in 0..30 { if svc.tasks.running_task_count() == 0 { break; } tokio::time::sleep(Duration::from_millis(10)).await; }
        assert!(svc.loaded_plugins().is_empty() && svc.tasks.get_task(&p_tid).expect("ev").task.status == TaskStatus::Cancelled);
        assert!(pack_ext::load_pack("adv-signed-pack").expect_err("q").contains("signer changed"));
        assert!(pack_ext::verify_pack_dir_for_sdk(&pack_dir).expect_err("qe").contains("quarantined"));

        let state_file = cfg_dir.join("extensions").join("state.json");
        let saved = std::fs::read_to_string(&state_file).expect("saved");
        std::fs::write(&state_file, "{corrupt").expect("corrupt");
        assert!(pack_ext::verify_pack_dir_for_sdk(&pack_dir).expect_err("dmg").contains("damaged"));
        std::fs::write(&state_file, saved).expect("restore");
        assert!(pack_ext::readmit_pack("adv-signed-pack").expect("readmit").active);
        assert_eq!(svc.sync_active_pack(&policy, decorate).expect("rec").expect("r").id, "adv-signed-pack");
        assert_eq!(req(&h, "tools/call", json!({"name": "plugin_lookup", "arguments": {"key": "ok"}})).await["result"]["content"][0]["text"], "plugin:ok");
        pack_ext::unload_pack("adv-signed-pack").expect("unload");
        assert!(svc.sync_active_pack(&policy, |b| b).expect("us").is_none());
        h.shutdown().await;
    });

    // 8. Durable task store crash reconciliation (`Working` -> `Failed`) & corruption rejection.
    let crash_dir = home.join("crash-store");
    let crash_svc = GmcpService::open_durable(ws.clone(), crash_dir.clone()).expect("open");
    let orphan_id = rt.block_on(async {
        crash_svc.tasks.spawn(TaskOptions::default(), |tc| Box::pin(async move { tc.cancelled().await; Err(TaskExit::Cancelled) })).task_id
    });
    let working_bytes = std::fs::read(crash_dir.join("tasks.json")).expect("bytes");
    drop(crash_svc);
    std::fs::write(crash_dir.join("tasks.json"), working_bytes).expect("crash");
    let recovered = GmcpService::open_durable(ws.clone(), crash_dir.clone()).expect("reconcile");
    assert_eq!(recovered.tasks.get_task(&orphan_id).expect("orphan").task.status, TaskStatus::Failed);
    drop(recovered);
    for bad_bytes in [&b"{truncated"[..], &b"[99,[]]"[..]] {
        std::fs::write(crash_dir.join("tasks.json"), bad_bytes).expect("bad");
        assert!(GmcpService::open_durable(ws.clone(), crash_dir.clone()).is_err());
    }
    assert!(root.join(".agents/gmcp/evidence/VC-203-020.jsonl").is_file() && root.join(".agents/roadmap-verdicts/VC-203-020.json").is_file());
}

#[allow(dead_code)]
#[path = "../../crates/susi-gmcp/examples/01_minimal_tool.rs"]
mod sdk_ex_01;
#[allow(dead_code)]
#[path = "../../crates/susi-gmcp/examples/02_structured_middleware.rs"]
mod sdk_ex_02;
#[allow(dead_code)]
#[path = "../../crates/susi-gmcp/examples/03_resources_prompts_completions.rs"]
mod sdk_ex_03;
#[allow(dead_code)]
#[path = "../../crates/susi-gmcp/examples/04_mrtr_and_durable_tasks.rs"]
mod sdk_ex_04;
#[allow(dead_code)]
#[path = "../../crates/susi-gmcp/examples/05_hot_reload_and_subscriptions.rs"]
mod sdk_ex_05;
#[allow(dead_code)]
#[path = "../../crates/susi-gmcp/examples/06_production_plugin_server.rs"]
mod sdk_ex_06;

/// GMCP-203-020-05: interoperability and quality — both-role official-suite and
/// reference-SDK interop (`2026-07-28` & `2025-11-25`), direct-vs-gateway
/// differential behavior, property invariants, transport/platform parity,
/// runnable minimal-through-production examples (`01`..`06`), published SLO and
/// fairness budgets, and retained regression fixtures (`R1`..`R3`).
#[test]
fn gmcp_req_203_020_05_interop() {
    use ed25519_dalek::{Signer, SigningKey};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    };
    use std::time::{Duration, Instant};
    use susi_gmcp::{
        direct_call::call_tool_direct,
        mcp::{
            model::{CallToolResult, ClientCapabilities, ContentBlock, Prompt, Resource, Tool},
            schema,
            task_manager::{TaskExit, TaskOptions, TaskPrincipal, MAX_ACTIVE_TASKS_PER_PRINCIPAL},
            ServiceExt,
        },
        protocol::{GmcpService, McpGatewayProxy, ToolMiddleware},
        sdk::{
            examples::{
                production_reference_server, sample_plugin_bundle, SummarizeInput,
            },
            mrtr_elicitation, mrtr_roots_list, mrtr_sampling, CatalogUpdate, PluginBundle,
            PluginPolicy, ServerBuilder, TestHarness,
        },
        server::GmcpServer,
        susi_config::extensions::{
            create_pack, load_pack, unload_pack, validate_manifest, DEFAULT_PACK_ID,
        },
    };
    use susi_wire_client::mcp::{
        CallToolOutcome, Session, TaskStatus as ClientTaskStatus, PROTOCOL_VERSION_2026_07_28,
    };

    let (_lock, _env, home) = gmcp_http_home_guard("020-05-interop");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ws = home.join("ws");
    let store_dir = home.join("durable-tasks");
    let ex_scratch = home.join("examples-scratch");
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&ws).expect("ws");
    std::fs::create_dir_all(&ex_scratch).expect("ex");
    std::fs::create_dir_all(&cfg_dir).expect("cfg");
    let token = "susi-020-05-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).expect("token");
    susi_gmcp::susi_config::extensions::invalidate_extension_caches();
    let baseline_rss = adapter_resident_bytes();

    // 1. Runnable minimal-through-production examples (`01_minimal_tool` .. `06_production_plugin_server`).
    assert_eq!(DEFAULT_PACK_ID, "default");
    for (expected_name, res) in [
        ("01_minimal_tool", sdk_ex_01::run(&ex_scratch.join("01"))),
        ("02_structured_middleware", sdk_ex_02::run(&ex_scratch.join("02"))),
        ("03_resources_prompts_completions", sdk_ex_03::run(&ex_scratch.join("03"))),
        ("04_mrtr_and_durable_tasks", sdk_ex_04::run(&ex_scratch.join("04"))),
        ("05_hot_reload_and_subscriptions", sdk_ex_05::run(&ex_scratch.join("05"))),
        ("06_production_plugin_server", sdk_ex_06::run(&ex_scratch.join("06"))),
    ] {
        let out = res.unwrap_or_else(|e| panic!("{expected_name} failed: {e}"));
        assert_eq!(out["example"], expected_name, "{out}");
        assert_eq!(out["ok"], true, "{expected_name} invariant failed: {out}");
    }
    assert!(!cfg_dir.join("extensions").exists(), "examples must never seed ~/.susi/extensions");

    // 2. Both-role official-suite & reference-SDK interop (`2026-07-28` & `2025-11-25`) +startup SLO.
    let t_startup = Instant::now();
    let (sdk_svc, audit_log) = production_reference_server(ws.clone(), store_dir.clone()).expect("sdk");
    let obj_schema = serde_json::Map::from_iter([("type".into(), json!("object"))]);
    sdk_svc
        .register_task_tool(
            Tool::new_with_raw("mrtr_triad", Some("3-method MRTR task".into()), obj_schema.clone()),
            Arc::new(|_params, tc| {
                Box::pin(async move {
                    let s_req = mrtr_sampling("Summarize release readiness", 64).map_err(TaskExit::Error)?;
                    let s_ans = tc.request_input("sample_step", s_req).await?;
                    let e_schema = json!({"type":"object","properties":{"go":{"type":"boolean"}},"required":["go"]});
                    let e_req = mrtr_elicitation("Proceed with release?", e_schema).map_err(TaskExit::Error)?;
                    let e_ans = tc.request_input("elicit_step", e_req).await?;
                    let r_req = mrtr_roots_list().map_err(TaskExit::Error)?;
                    let r_ans = tc.request_input("roots_step", r_req).await?;
                    Ok(CallToolResult::structured(json!({ "sampled": s_ans["content"]["text"], "approved": e_ans["go"], "rootCount": r_ans["roots"].as_array().map_or(0, Vec::len)})))
                })
            }),
        )
        .expect("mrtr_triad");
    let restricted_runs = Arc::new(AtomicUsize::new(0));
    let rr = Arc::clone(&restricted_runs);
    sdk_svc
        .register_tool(
            Tool::new_with_raw("system_restricted_interop_op", Some("Gateway differential probe".into()), obj_schema.clone()),
            Arc::new(move |_p, _c| {
                rr.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("unreachable")]).into()) })
            }),
        )
        .expect("restricted probe");

    // Attach signed plugin bundle for full server+plugin surface.
    let pack_dir = home.join("interop-signed-pack");
    std::fs::create_dir_all(pack_dir.join("catalog")).expect("pack dir");
    let art_bytes = br#"{"tools":["plugin_lookup","plugin_watch"]}"#;
    std::fs::write(pack_dir.join("catalog/plugin.json"), art_bytes).expect("art");
    let digest = susi_paths::hex::encode(Sha256::digest(art_bytes));
    let (files, csums) = (
        BTreeMap::from([("catalog".to_string(), "catalog/plugin.json".to_string())]),
        BTreeMap::from([("catalog".to_string(), format!("sha256:{digest}"))]),
    );
    let sk = SigningKey::from_bytes(&[11u8; 32]);
    let mk_signed_bundle = || {
        let mut b = sample_plugin_bundle(Some(pack_dir.clone()), csums.clone(), files.clone());
        b.manifest.signer = Some(susi_paths::hex::encode(sk.verifying_key().to_bytes()));
        b.manifest.signatures.insert("catalog".into(), susi_paths::hex::encode(sk.sign(art_bytes).to_bytes()));
        b
    };
    std::fs::write(pack_dir.join("manifest.json"), serde_json::to_vec_pretty(&mk_signed_bundle().manifest).expect("j")).expect("wm");
    let policy = PluginPolicy::strict().grant_permission("filesystem.read");
    sdk_svc.attach_plugin(mk_signed_bundle(), &policy).expect("attach signed plugin");
    sdk_svc.bind_direct_server("sdk-020-05").expect("bind direct");

    let rt = adapter_runtime(&home);
    let startup_ms = rt.block_on(async {
        let h = TestHarness::spawn(sdk_svc.clone());
        let d = h.request_modern("server/discover", json!({}), Default::default()).await.expect("disc");
        assert_eq!(d["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "sdk-production-ref");
        let ms = t_startup.elapsed().as_millis();
        h.shutdown().await;
        ms
    });
    assert!(startup_ms <= 2_000, "cold SDK startup + discover took {startup_ms} ms (> 2000 ms)");

    // Serve over HTTP `/mcp` and test both `2026-07-28` stateless & `2025-11-25` stateful reference clients.
    let wait_port = |l: TcpListener, f: Box<dyn FnOnce(Vec<TcpListener>) + Send>| -> String {
        let addr = format!("127.0.0.1:{}", l.local_addr().expect("addr").port());
        std::thread::spawn(move || f(vec![l]));
        for _ in 0..80 {
            if TcpStream::connect(&addr).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        addr
    };
    let http_svc = sdk_svc.clone();
    let sdk_addr = wait_port(
        TcpListener::bind("127.0.0.1:0").expect("bind"),
        Box::new(move |ls| GmcpServer::serve_http_service(http_svc, ls, None, false)),
    );

    let mut modern_wire = Session::open_2026_07_28(&sdk_addr, Some(token), Duration::from_secs(15)).expect("open 2026");
    assert_eq!(modern_wire.protocol_version(), PROTOCOL_VERSION_2026_07_28);
    assert!(modern_wire.supports_server_tasks());
    assert!(modern_wire.server_extensions().expect("exts").contains_key("io.susi.sdk/reference"));
    assert!(modern_wire.server_extensions().expect("exts").contains_key("io.susi.plugin/sample"));

    let mut mrtr_methods_seen = Vec::new();
    let triad_out = modern_wire
        .call_tool_resolving_tasks("mrtr_triad", &json!({}), Some(Duration::from_secs(10)), |reqs| {
            assert_eq!(reqs.len(), 1);
            mrtr_methods_seen.push(reqs[0].method.clone());
            let ans = match reqs[0].method.as_str() {
                "sampling/createMessage" => json!({ "role": "assistant", "content": { "type": "text", "text": "all-green" }, "model": "ref-client" }),
                "elicitation/create" => json!({ "go": true }),
                "roots/list" => json!({ "roots": [{ "uri": "file:///workspace/susi", "name": "susi" }] }),
                other => panic!("unexpected MRTR method {other}"),
            };
            Ok(json!({ &reqs[0].key: ans }))
        })
        .expect("triad resolved");
    assert_eq!(mrtr_methods_seen, vec!["sampling/createMessage", "elicitation/create", "roots/list"]);
    assert_eq!(triad_out["structuredContent"], json!({ "sampled": "all-green", "approved": true, "rootCount": 1 }));

    let CallToolOutcome::Task(watch_handle) = modern_wire
        .call_tool_with_tasks("plugin_watch", &json!({ "key": "http-cancel" }), true, &mut |_| {})
        .expect("spawn watch")
    else {
        panic!("expected async task handle for plugin_watch");
    };
    modern_wire.cancel_task(&watch_handle.task_id).expect("cancel_task");
    assert_eq!(modern_wire.get_task(&watch_handle.task_id).expect("snap").handle.status, ClientTaskStatus::Cancelled);

    let mut legacy_wire = Session::open_with_capabilities(&sdk_addr, Some(token), &json!({ "elicitation": {} }), Duration::from_secs(15)).expect("open 2025");
    assert_eq!(legacy_wire.protocol_version(), "2025-11-25");
    assert_eq!(legacy_wire.request("ping", &json!({}), &mut |_| {}).expect("ping"), json!({}));

    // 3. Direct-vs-gateway differential behavior & cross-transport transcript parity.
    let sum_args = json!({ "items": ["one", "two", "three"], "scope": { "namespace": "parity", "tag": "t1" }, "maxItems": 2 });
    audit_log.write().clear();
    let sync_res = sdk_svc.call_tool_sync("summarize_items", sum_args.clone()).expect("sync")["result"]["structuredContent"].clone();
    let direct_raw = call_tool_direct("sdk-020-05", "summarize_items", &sum_args, &ws).expect("direct");
    let direct_res: Value = serde_json::from_str(direct_raw["content"][0]["text"].as_str().expect("txt")).expect("json");
    let http_2026_res = modern_wire.call_tool("summarize_items", &sum_args, None, &mut |_| {}).expect("2026")["structuredContent"].clone();
    let http_2025_res = legacy_wire.call_tool("summarize_items", &sum_args, None, &mut |_| {}).expect("2025")["structuredContent"].clone();
    assert_eq!(sync_res, json!({ "count": 2, "summary": "one, two", "namespace": "parity" }));
    assert_eq!((&direct_res, &http_2026_res, &http_2025_res), (&sync_res, &sync_res, &sync_res));
    assert_eq!(audit_log.read().len(), 20, "4 calls x 5 audit entries");

    // Differential policy proof: gateway policy blocks `system_*` across direct/sync/HTTP with 0 handler runs; HTTP auth/origin blocks before middleware while direct in-process runs unauthenticated.
    assert!(McpGatewayProxy::evaluate_tool_policy("system_restricted_interop_op").is_err());
    for gated in [
        sdk_svc.call_tool_sync("system_restricted_interop_op", json!({})).expect("sg")["result"].clone(),
        call_tool_direct("sdk-020-05", "system_restricted_interop_op", &json!({}), &ws).expect("dg"),
        call_tool_direct("susi", "system_restricted_interop_op", &json!({}), &ws).expect("cg"),
        modern_wire.call_tool("system_restricted_interop_op", &json!({}), None, &mut |_| {}).expect("hg"),
    ] {
        assert!(gated["isError"] == true && gated["content"][0]["text"].as_str().expect("t").starts_with("Governance Violation:"));
    }
    assert_eq!(restricted_runs.load(Ordering::Acquire), 0);
    audit_log.write().clear();
    let raw_post = |hdrs: &[(&str, &str)], body: &str| -> String {
        let mut s = TcpStream::connect(&sdk_addr).expect("tcp");
        s.set_read_timeout(Some(Duration::from_secs(5))).expect("to");
        let mut req = format!("POST /mcp HTTP/1.1\r\nHost: {sdk_addr}\r\nConnection: close\r\n");
        for (k, v) in hdrs {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        s.write_all(req.as_bytes()).expect("w");
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    };
    let unauth_body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"summarize_items","arguments":sum_args,"_meta":{"io.modelcontextprotocol/protocolVersion":PROTOCOL_VERSION_2026_07_28,"io.modelcontextprotocol/clientCapabilities":{}}}}).to_string();
    let base_h = [("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream"), ("MCP-Protocol-Version", PROTOCOL_VERSION_2026_07_28), ("Mcp-Method", "tools/call"), ("Mcp-Name", "summarize_items")];
    assert!(raw_post(&base_h, &unauth_body).starts_with("HTTP/1.1 401"));
    let bearer_h = format!("Bearer {token}");
    assert!(raw_post(&[base_h[0], base_h[1], base_h[2], base_h[3], base_h[4], ("Authorization", &bearer_h), ("Origin", "https://attacker.example")], &unauth_body).starts_with("HTTP/1.1 403"));
    assert!(audit_log.read().is_empty(), "gateway edge rejection must never reach SDK middleware");

    // Cross-transport normalized transcript parity: duplex `TestHarness` vs `2026-07-28` HTTP vs `2025-11-25` HTTP + subprocess `StdioSession`.
    let collect_http_transcript = |sess: &mut Session| -> Value {
        let mut t_names: Vec<String> = sess.request("tools/list", &json!({}), &mut |_| {}).expect("tl")["tools"]
            .as_array().expect("a").iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
        t_names.sort();
        let r_text = sess.request("resources/read", &json!({"uri": "memo://items/99"}), &mut |_| {}).expect("rr")["contents"][0]["text"].clone();
        let p_res = sess.request("resources/read", &json!({"uri": "plugin://status"}), &mut |_| {}).expect("pr")["contents"][0]["text"].clone();
        let pr_txt = sess.request("prompts/get", &json!({"name": "draft_brief", "arguments": {"topic": "Parity"}}), &mut |_| {}).expect("pg")["messages"][0]["content"]["text"].clone();
        let cmp_v = sess.request("completion/complete", &json!({"ref":{"type":"ref/prompt","name":"draft_brief"},"argument":{"name":"topic","value":"s"}}), &mut |_| {}).expect("cc")["completion"]["values"].clone();
        let plug_call = sess.call_tool("plugin_lookup", &json!({"key": "x"}), None, &mut |_| {}).expect("pc")["content"][0]["text"].clone();
        json!({ "tools": t_names, "tpl": r_text, "plugRes": p_res, "prompt": pr_txt, "comp": cmp_v, "plugCall": plug_call })
    };
    let t_http_2026 = collect_http_transcript(&mut modern_wire);
    let t_http_2025 = collect_http_transcript(&mut legacy_wire);
    let t_duplex = rt.block_on(async {
        let h = TestHarness::spawn(sdk_svc.clone());
        let c = ClientCapabilities::default();
        let mut t_names: Vec<String> = h.request_modern("tools/list", json!({}), c.clone()).await.expect("tl")["result"]["tools"]
            .as_array().expect("a").iter().filter_map(|t| t["name"].as_str().map(str::to_string)).collect();
        t_names.sort();
        let r_text = h.request_modern("resources/read", json!({"uri": "memo://items/99"}), c.clone()).await.expect("rr")["result"]["contents"][0]["text"].clone();
        let p_res = h.request_modern("resources/read", json!({"uri": "plugin://status"}), c.clone()).await.expect("pr")["result"]["contents"][0]["text"].clone();
        let pr_txt = h.request_modern("prompts/get", json!({"name": "draft_brief", "arguments": {"topic": "Parity"}}), c.clone()).await.expect("pg")["result"]["messages"][0]["content"]["text"].clone();
        let cmp_v = h.request_modern("completion/complete", json!({"ref":{"type":"ref/prompt","name":"draft_brief"},"argument":{"name":"topic","value":"s"}}), c.clone()).await.expect("cc")["result"]["completion"]["values"].clone();
        let plug_call = h.request_modern("tools/call", json!({"name": "plugin_lookup", "arguments": {"key": "x"}}), c).await.expect("pc")["result"]["content"][0]["text"].clone();
        h.shutdown().await;
        json!({ "tools": t_names, "tpl": r_text, "plugRes": p_res, "prompt": pr_txt, "comp": cmp_v, "plugCall": plug_call })
    });
    assert_eq!(t_duplex, t_http_2026);
    assert_eq!(t_http_2026, t_http_2025);
    modern_wire.close().expect("close 2026");
    legacy_wire.close().expect("close 2025");

    let mut stdio_proc = StdioSession::spawn_modern(root, &home);
    assert!(stdio_proc.request("server/discover", adapter_modern_meta())["capabilities"]["extensions"]["io.modelcontextprotocol/tasks"].is_object());
    drop(stdio_proc);

    // Replay pinned official wire conformance corpus (`adapter_wire_fixtures` + `divergences.json`) against SDK-built server in BOTH eras (`R2`).
    let wire_fixtures = adapter_wire_fixtures(root);
    let spawn_sdk_adapter = |svc: GmcpService| -> (AdapterClient, AdapterSession) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let (c_half, s_half) = tokio::io::duplex(ADAPTER_LIMIT);
        let (sr, sw) = tokio::io::split(s_half);
        let sess_svc = svc.session();
        let session: AdapterSession = rt.spawn(async move {
            let transport = susi_gmcp::jsonrpc_gate::StdioGate::new(susi_gmcp::stdio::BoundedLines::new(sr, ADAPTER_LIMIT), sw);
            let running = sess_svc.serve(transport).await.map_err(|e| format!("{e:?}"))?;
            running.waiting().await.map_err(|e| e.to_string())
        });
        let (cr, mut cw) = tokio::io::split(c_half);
        let (ltx, lrx) = std::sync::mpsc::channel();
        rt.spawn(async move {
            let mut lines = tokio::io::BufReader::new(cr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str(&l) {
                    if ltx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        let (btx, mut brx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
        rt.spawn(async move {
            while let Some(b) = brx.recv().await {
                if cw.write_all(&b).await.is_err() {
                    break;
                }
            }
            let _ = cw.shutdown().await;
        });
        (AdapterClient { write: Box::new(move |b| { let _ = btx.send(b.to_vec()); }), rx: lrx, pending: Vec::new() }, session)
    };
    let (mut mod_cli, mod_sess) = spawn_sdk_adapter(sdk_svc.clone());
    adapter_modern_open(&mut mod_cli);
    let mod_replay = adapter_fixture_replay(&mut mod_cli, &wire_fixtures, "modern");
    drop(mod_cli);
    adapter_session_ended(&rt, mod_sess, "sdk-020-modern-fixtures");
    assert!(!mod_replay.is_empty());

    let (mut leg_cli, leg_sess) = spawn_sdk_adapter(sdk_svc.clone());
    let script_tx = transport_adapter_script(&mut leg_cli);
    let limit_code = adapter_limit_step(&mut leg_cli);
    let prop_answers = adapter_property_run(&mut leg_cli, 32);
    let leg_replay = adapter_fixture_replay(&mut leg_cli, &wire_fixtures, "legacy");
    drop(leg_cli);
    adapter_session_ended(&rt, leg_sess, "sdk-020-legacy-fixtures");
    assert!(!script_tx.is_empty() && limit_code.is_i64() && prop_answers.len() == 32 && !leg_replay.is_empty());

    // 4. Platform path-confinement & Mandate 35 `extra` preservation invariants.
    for bad_path in ["../escape.json", "..\\escape.json", "/abs/unix.json", "C:\\abs\\win.json", "CON.txt", "COM1.json"] {
        let mut m = mk_signed_bundle().manifest;
        m.permissions.clear();
        m.files.insert("bad".into(), bad_path.into());
        assert!(validate_manifest(&m).is_err(), "must reject unsafe path `{bad_path}`");
    }
    let ext_root = cfg_dir.join("extensions");
    std::fs::create_dir_all(&ext_root).expect("ext root");
    std::fs::write(ext_root.join("state.json"), serde_json::to_vec_pretty(&json!({ "active": "default", "x-forward-state": { "epoch": 7, "flags": ["a", "b"]}})).expect("j")).expect("write state");
    create_pack("m35-state-probe").expect("create probe");
    load_pack("m35-state-probe").expect("load probe");
    unload_pack("m35-state-probe").expect("unload probe");
    let state_after: Value = serde_json::from_slice(&std::fs::read(ext_root.join("state.json")).expect("read state")).expect("parse state");
    assert_eq!(state_after["x-forward-state"], json!({ "epoch": 7, "flags": ["a", "b"] }));

    // 5. Deterministic property invariants (schema round-trip, order-independence, plugin involution).
    let sum_validator = susi_gmcp::schema_check::Validator::compile(&Value::Object(schema::root::<SummarizeInput>())).expect("compile SummarizeInput");
    let mut seed: u64 = 0x2030_2005;
    let mut next_u64 = || {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_007);
        seed
    };
    let sample_atoms = ["", "a", "αβγ-🚀", "line\nbreak", "rtl-العربية", "quote\"slash\\"];
    for idx in 0..24usize {
        let n_items = (next_u64() % 5) as usize;
        let items: Vec<String> = (0..n_items).map(|_| sample_atoms[(next_u64() as usize) % sample_atoms.len()].to_string()).collect();
        let ns = sample_atoms[(idx + 1) % sample_atoms.len()].to_string();
        let tag = if idx % 2 == 0 { None } else { Some(sample_atoms[idx % sample_atoms.len()].to_string()) };
        let max_items = (idx % 4) as u32;
        let typed = SummarizeInput { items: items.clone(), scope: susi_gmcp::sdk::examples::ScopeFilter { namespace: ns.clone(), tag }, max_items };
        let wire_val = serde_json::to_value(&typed).expect("to_value");
        assert!(sum_validator.is_valid(&wire_val), "derived Validator must accept serialized SummarizeInput: {wire_val}");
        let round: SummarizeInput = serde_json::from_value(wire_val.clone()).expect("from_value");
        assert_eq!(round, typed);
        let res = sdk_svc.call_tool_sync("summarize_items", wire_val).expect("prop call");
        assert_eq!(res["result"]["isError"], false);
        assert_eq!(res["result"]["structuredContent"]["namespace"], ns);
    }
    for bad in [
        json!({ "items": "not-array", "scope": { "namespace": "x" } }),
        json!({ "items": [], "scope": { "namespace": 123 } }),
        json!({ "items": [], "scope": {}, "maxItems": 1 }),
        json!({ "items": [], "scope": { "namespace": "x" }, "maxItems": -5 }),
    ] {
        assert!(!sum_validator.is_valid(&bad), "derived Validator must reject {bad}");
        audit_log.write().clear();
        assert_eq!(sdk_svc.call_tool_sync("summarize_items", bad).expect("bad")["result"]["isError"], true);
        assert!(audit_log.read().is_empty());
    }

    // Registration-order permutation invariance (`forward` vs `reverse` vs `interleaved`).
    let noop_tool: susi_gmcp::protocol::ToolHandler = Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }));
    let noop_res: susi_gmcp::protocol::ResourceHandler = Arc::new(|req, _c| Box::pin(async move {
        Ok(susi_gmcp::mcp::model::ReadResourceResult { result_type: None, ttl_ms: None, cache_scope: None, contents: vec![susi_gmcp::mcp::model::ResourceContents::TextResourceContents { uri: req.uri, mime_type: Some("text/plain".into()), text: "ok".into(), meta: None }], meta: None }.into())
    }));
    let noop_pr: susi_gmcp::protocol::PromptHandler = Arc::new(|_r, _c| Box::pin(async {
        Ok(susi_gmcp::mcp::model::GetPromptResult { result_type: None, description: None, messages: vec![], meta: None }.into())
    }));
    let snapshot_order = |order: &[usize]| -> Value {
        let mut b = ServerBuilder::new("perm", "1.0.0", ws.clone());
        for &k in order {
            b = b
                .tool(Tool::new_with_raw(format!("t_{k:02}"), None, obj_schema.clone()), Arc::clone(&noop_tool))
                .resource(Resource::new(format!("memo://r/{k:02}"), format!("r_{k:02}")), Arc::clone(&noop_res))
                .prompt(Prompt { name: format!("p_{k:02}"), title: None, description: None, arguments: None, icons: None, meta: None }, Arc::clone(&noop_pr));
        }
        let s = b.build().expect("perm build");
        rt.block_on(async {
            let h = TestHarness::spawn(s);
            let tl = h.request_modern("tools/list", json!({}), Default::default()).await.expect("tl")["result"]["tools"].clone();
            let rl = h.request_modern("resources/list", json!({}), Default::default()).await.expect("rl")["result"]["resources"].clone();
            let pl = h.request_modern("prompts/list", json!({}), Default::default()).await.expect("pl")["result"]["prompts"].clone();
            h.shutdown().await;
            json!({ "t": tl, "r": rl, "p": pl })
        })
    };
    let fwd = snapshot_order(&[0, 1, 2, 3, 4, 5, 6, 7]);
    let rev = snapshot_order(&[7, 6, 5, 4, 3, 2, 1, 0]);
    let mix = snapshot_order(&[3, 0, 7, 1, 5, 2, 6, 4]);
    assert_eq!(fwd, rev);
    assert_eq!(fwd, mix);

    // 6. Published SLOs (middleware overhead, <=256 atomic hot-reload under load, plugin load/unload + zero FD leak, fairness & disk/memory bounds).
    let bench_calls = |stages: usize| -> Vec<u128> {
        let mut b = ServerBuilder::new("mw-bench", "1.0.0", ws.clone()).tool(
            Tool::new_with_raw("fast_noop", None, obj_schema.clone()),
            Arc::new(|_p, _c| Box::pin(async { Ok(CallToolResult::success(vec![ContentBlock::text("ok")]).into()) })),
        );
        for _ in 0..stages {
            let mw: ToolMiddleware = Arc::new(|req, ctx, next| Box::pin(async move { next(req, ctx).await }));
            b = b.middleware(mw);
        }
        let s = b.build().expect("bench svc");
        rt.block_on(async {
            let h = TestHarness::spawn(s);
            let _ = h.request_modern("tools/call", json!({"name": "fast_noop", "arguments": {}}), Default::default()).await;
            let mut samples = Vec::with_capacity(32);
            for _ in 0..32 {
                let t0 = Instant::now();
                let r = h.request_modern("tools/call", json!({"name": "fast_noop", "arguments": {}}), Default::default()).await.expect("call");
                samples.push(t0.elapsed().as_micros());
                assert_eq!(r["result"]["isError"], false);
            }
            h.shutdown().await;
            samples.sort_unstable();
            samples
        })
    };
    let (base_us, mw4_us) = (bench_calls(0), bench_calls(4));
    let mw4_median_delta_us = mw4_us[16].saturating_sub(base_us[16]);
    assert!(mw4_median_delta_us <= 25_000, "4-stage middleware median delta {mw4_median_delta_us} us exceeded CI envelope");

    // Atomic catalog hot-reload swap (`160 <= 256` items) under active concurrent reader load + R3 plugin preservation.
    let reader_errors = Arc::new(AtomicUsize::new(0));
    let stop_readers = Arc::new(AtomicBool::new(false));
    let swap_us = rt.block_on(async {
        let mut readers = Vec::new();
        for _ in 0..4 {
            let (s, errs, stop) = (sdk_svc.clone(), Arc::clone(&reader_errors), Arc::clone(&stop_readers));
            readers.push(tokio::spawn(async move {
                let h = TestHarness::spawn(s);
                while !stop.load(Ordering::Acquire) {
                    let r = h.request_modern("tools/call", json!({"name": "plugin_lookup", "arguments": {"key": "hot"}}), Default::default()).await.expect("r");
                    if r["result"]["content"][0]["text"] != json!("plugin:hot") {
                        errs.fetch_add(1, Ordering::Relaxed);
                    }
                }
                h.shutdown().await;
            }));
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
        // R3 negative check: colliding with active plugin tool `plugin_lookup` is refused fail-closed.
        assert!(sdk_svc.reload_catalog(CatalogUpdate::new().with_tool(Tool::new_with_raw("plugin_lookup", None, obj_schema.clone()), Arc::clone(&noop_tool))).is_err());

        let mut big = CatalogUpdate::new();
        for i in 0..120 {
            big = big.with_tool(Tool::new_with_raw(format!("hot_t_{i:03}"), None, obj_schema.clone()), Arc::clone(&noop_tool));
        }
        for i in 0..20 {
            big = big.with_resource(Resource::new(format!("memo://hot/{i:02}"), format!("hr_{i:02}")), Arc::clone(&noop_res));
            big = big.with_prompt(Prompt { name: format!("hot_p_{i:02}"), title: None, description: None, arguments: None, icons: None, meta: None }, Arc::clone(&noop_pr));
        }
        let t_swap = Instant::now();
        let rep = sdk_svc.reload_catalog(big).expect("160 swap");
        let elapsed_us = t_swap.elapsed().as_micros();
        assert!(rep.tools_changed && rep.resources_changed && rep.prompts_changed);
        tokio::time::sleep(Duration::from_millis(15)).await;
        stop_readers.store(true, Ordering::Release);
        for r in readers {
            r.await.expect("reader");
        }
        elapsed_us
    });
    assert_eq!(reader_errors.load(Ordering::Acquire), 0, "0 in-flight calls may fail or drop during reload_catalog");
    assert!(swap_us <= 500_000, "160-entry reload_catalog swap took {swap_us} us (> 500 ms CI ceiling)");

    // Transactional plugin validate + attach + detach p99 budget + OS-level zero-FD-leak check (`remove_dir_all` on Windows/POSIX).
    sdk_svc.unload_plugin("sdk-sample-plugin").expect("unload initial");
    let baseline_tools_after_unload = rt.block_on(async {
        let h = TestHarness::spawn(sdk_svc.clone());
        let t = h.request_modern("tools/list", json!({}), Default::default()).await.expect("tl")["result"]["tools"].clone();
        h.shutdown().await;
        t
    });
    let mut cycle_ms = Vec::with_capacity(8);
    for _ in 0..8 {
        let t0 = Instant::now();
        let disk_bundle = PluginBundle::from_pack_dir(&pack_dir).expect("verify disk pack");
        let mut full = mk_signed_bundle();
        full.manifest = disk_bundle.manifest;
        sdk_svc.attach_plugin(full, &policy).expect("attach cycle");
        sdk_svc.unload_plugin("sdk-sample-plugin").expect("unload cycle");
        cycle_ms.push(t0.elapsed().as_millis());
    }
    cycle_ms.sort_unstable();
    assert!(cycle_ms[7] <= 500, "plugin validate+attach+unload p99 took {} ms (> 500 ms)", cycle_ms[7]);
    std::fs::remove_dir_all(&pack_dir).expect("zero leaked open file handles on pack_dir after unload");
    let post_cycles_tools = rt.block_on(async {
        let h = TestHarness::spawn(sdk_svc.clone());
        let t = h.request_modern("tools/list", json!({}), Default::default()).await.expect("tl")["result"]["tools"].clone();
        h.shutdown().await;
        t
    });
    assert_eq!(post_cycles_tools, baseline_tools_after_unload);
    assert!(sdk_svc.loaded_plugins().is_empty() && !sdk_svc.extensions().read().contains("io.susi.plugin/sample"));

    // Multi-principal fairness + R1 cooperative vs uncooperative cancellation regression fixture.
    let (p_heavy, p_light) = (TaskPrincipal::from_bearer_token("heavy-tenant"), TaskPrincipal::from_bearer_token("light-tenant"));
    rt.block_on(async {
        let mut heavy_ids = Vec::with_capacity(MAX_ACTIVE_TASKS_PER_PRINCIPAL);
        for _ in 0..MAX_ACTIVE_TASKS_PER_PRINCIPAL {
            let t = sdk_svc.tasks.try_spawn_for(p_heavy.clone(), TaskOptions::default(), |tc| {
                Box::pin(async move { tc.cancelled().await; Err(TaskExit::Cancelled) })
            }).expect("heavy slot");
            heavy_ids.push(t.task_id);
        }
        let t_fair = Instant::now();
        let mut light_ids = Vec::with_capacity(8);
        for i in 0..8 {
            let t = sdk_svc.tasks.try_spawn_for(p_light.clone(), TaskOptions::default(), move |_tc| {
                Box::pin(async move { Ok(CallToolResult::success(vec![ContentBlock::text(format!("light-{i}"))])) })
            }).expect("light tenant isolated from heavy tenant quota");
            light_ids.push(t.task_id);
        }
        for id in &light_ids {
            for _ in 0..40 {
                if sdk_svc.tasks.get_task_for(id, &p_light).expect("g").task.status.is_terminal() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            assert_eq!(sdk_svc.tasks.get_task_for(id, &p_light).expect("done").task.status, susi_gmcp::mcp::model::TaskStatus::Completed);
        }
        assert!(t_fair.elapsed() <= Duration::from_secs(2), "light principal starved by heavy principal");
        for id in &heavy_ids {
            sdk_svc.tasks.cancel_task_for(id, &p_heavy).expect("cancel heavy");
        }
        for _ in 0..40 {
            if sdk_svc.tasks.running_task_count() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(sdk_svc.tasks.running_task_count(), 0);

        // R1 regression fixture: cooperative `ctx.ct.cancelled()` cleanup runs before drop; uncooperative stall still aborts cleanly.
        let coop_cleaned = Arc::new(AtomicBool::new(false));
        let (coop_tx, coop_rx) = tokio::sync::oneshot::channel();
        let coop_slot = std::sync::Mutex::new(Some(coop_tx));
        let cc_flag = Arc::clone(&coop_cleaned);
        sdk_svc.register_tool(Tool::new_with_raw("r1_coop_cancel", None, obj_schema.clone()), Arc::new(move |_p, ctx| {
            let (tx, cleaned) = (coop_slot.lock().expect("l").take(), Arc::clone(&cc_flag));
            Box::pin(async move {
                if let Some(t) = tx { let _ = t.send(()); }
                ctx.ct.cancelled().await;
                cleaned.store(true, Ordering::Release);
                Err(susi_gmcp::mcp::ErrorData::internal_error("cancelled cleanly", None))
            })
        })).expect("r1_coop");

        let (uncoop_tx, uncoop_rx) = tokio::sync::oneshot::channel();
        let uncoop_slot = std::sync::Mutex::new(Some(uncoop_tx));
        sdk_svc.register_tool(Tool::new_with_raw("r1_uncoop_stall", None, obj_schema.clone()), Arc::new(move |_p, _ctx| {
            let tx = uncoop_slot.lock().expect("l").take();
            Box::pin(async move {
                if let Some(t) = tx { let _ = t.send(()); }
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(CallToolResult::success(vec![]).into())
            })
        })).expect("r1_uncoop");

        let r1_h = Arc::new(TestHarness::spawn(sdk_svc.clone()));
        let r1_h1 = Arc::clone(&r1_h);
        let f1 = tokio::spawn(async move {
            r1_h1.request_raw(json!({"jsonrpc":"2.0","id":701,"method":"tools/call","params":{"name":"r1_coop_cancel","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}})).await
        });
        coop_rx.await.expect("coop entered");
        r1_h.notify("notifications/cancelled", json!({"requestId": 701})).await.expect("cancel 701");
        for _ in 0..30 {
            if coop_cleaned.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(coop_cleaned.load(Ordering::Acquire), "R1 regression: cooperative cancellation cleanup must run");
        f1.abort();
        let _ = f1.await;

        let r1_h2 = Arc::clone(&r1_h);
        let f2 = tokio::spawn(async move {
            r1_h2.request_raw(json!({"jsonrpc":"2.0","id":702,"method":"tools/call","params":{"name":"r1_uncoop_stall","arguments":{},"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}})).await
        });
        uncoop_rx.await.expect("uncoop entered");
        r1_h.notify("notifications/cancelled", json!({"requestId": 702})).await.expect("cancel 702");
        tokio::time::sleep(Duration::from_millis(60)).await;
        f2.abort();
        let _ = f2.await;
        assert!(r1_h.request_modern("server/discover", json!({}), Default::default()).await.expect("alive")["result"].is_object());
    });

    // Disk & resident memory envelopes.
    let disk_bytes = adapter_tree_bytes(&store_dir);
    assert!(disk_bytes <= 512 * 1024, "durable task store grew to {disk_bytes} bytes (> 512 KiB)");
    if let (Some(before), Some(after)) = (baseline_rss, adapter_resident_bytes()) {
        let growth_mib = after.saturating_sub(before) / (1024 * 1024);
        assert!(growth_mib <= 64, "resident memory grew by {growth_mib} MiB (> 64 MiB)");
    }

    GmcpService::unbind_direct_server("sdk-020-05");
    assert!(root.join(".agents/gmcp/evidence/VC-203-020.jsonl").is_file() && root.join(".agents/roadmap-verdicts/VC-203-020.json").is_file());
}
