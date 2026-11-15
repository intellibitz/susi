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
    let valid_manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "id": "sdk-sample-pack", "name": "SDK Sample Pack", "version": "0.1.0", "apiVersion": "1.0",
        "capabilities": ["mcp.tools.sample"], "requires": ["gmcp.server.v1"], "optional": ["gmcp.tasks.v1"],
        "permissions": ["filesystem.read"], "files": { "tools": "catalog/tools.json" },
        "checksums": { "tools": format!("sha256:{}", "a".repeat(64)) }, "x-vendor-metadata": {"tier": "contract-fixture"}
    })).expect("manifest");
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
    assert!(!root.join(".agents/gmcp/evidence/VC-203-020.jsonl").exists());
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
    assert!(!root.join(".agents/gmcp/evidence/VC-203-020.jsonl").exists());
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
    assert!(!root.join(".agents/gmcp/evidence/VC-203-020.jsonl").exists() && !root.join(".agents/roadmap-verdicts/VC-203-020.json").exists());
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
    assert!(!root.join(".agents/gmcp/evidence/VC-203-020.jsonl").exists() && !root.join(".agents/roadmap-verdicts/VC-203-020.json").exists());
}
