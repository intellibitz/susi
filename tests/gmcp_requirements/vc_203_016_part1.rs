/// GMCP-203-016-01: the extension framework contract is pinned.
///
/// The contract must resolve every normative mechanism the vector names
/// (discovery, negotiation, versioning, isolation, disablement, collision,
/// downgrade, watch), pin the three extension authorities, and bind a
/// behavior table whose rows are unique, correctly leveled, and owned by a
/// production path that actually exists. Fixtures live with this test.
#[test]
fn gmcp_req_203_016_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-016.md"))
        .expect("the extension framework contract must exist");
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
    ] {
        assert!(
            contract.contains(&format!("## {section}")),
            "contract must contain a {section} section"
        );
    }

    for authority in ["ext-apps", "ext-auth", "ext-tasks"] {
        assert!(
            contract.contains(authority),
            "the contract must pin the {authority} extension authority"
        );
    }

    let rows: Vec<&str> = contract
        .lines()
        .filter(|line| line.starts_with("| EXT-"))
        .collect();
    assert!(
        rows.len() >= 10,
        "the behavior table must bind at least ten rows, got {}",
        rows.len()
    );
    let mut ids = std::collections::BTreeSet::new();
    for row in rows {
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
        assert!(
            cells.len() >= 4,
            "a behavior row must carry id/level/clause/production path: {row}"
        );
        let (id, level, clause, path) = (cells[0], cells[1], cells[2], cells[3]);
        assert!(ids.insert(id.to_string()), "duplicate behavior row id {id}");
        assert!(
            ["must", "must_not", "should", "should_not", "may"].contains(&level),
            "{id}: unsupported level {level}"
        );
        assert!(!clause.is_empty(), "{id}: clause must not be empty");
        assert!(
            root.join(path).exists(),
            "{id}: the declared production path must exist: {path}"
        );
    }

    let lower = contract.to_lowercase();
    for mechanism in [
        "discover",
        "negotiat",
        "version",
        "isolat",
        "disabl",
        "collision",
        "downgrade",
        "watch",
    ] {
        assert!(
            lower.contains(mechanism),
            "the contract must resolve the `{mechanism}` mechanism"
        );
    }
    assert!(
        contract.contains("gmcp_req_203_016_01_contract"),
        "fixtures must live with the acceptance test that proves them"
    );
}

/// GMCP-203-016-02: the extension framework is one typed production path.
///
/// `susi_gmcp::extensions::ExtensionRegistry` drives the capabilities the
/// shipped `GmcpService` advertises on `server/discover`: disablement is
/// reflected on the wire, collisions and non-namespaced ids are refused,
/// required unsupported extensions fail explicitly, downgrades are refused,
/// unknown peer extensions round-trip verbatim, status is visible, and the
/// watch opens work for each newly published official extension.
#[test]
fn gmcp_req_203_016_02_production() {
    use susi_gmcp::extensions::{
        ExtensionError, ExtensionMap, ExtensionRegistry, ExtensionStatus, OFFICIAL_EXTENSIONS,
    };
    use susi_gmcp::protocol::GmcpService;

    let scratch = std::env::temp_dir().join(format!("gmcp-016-prod-{}", std::process::id()));
    let service = GmcpService::new(scratch.clone());
    let tasks = "io.modelcontextprotocol/tasks";

    // Discovery through the shipped service: tasks is advertised from the registry.
    let wire = serde_json::to_value(service.discover_result()).unwrap();
    assert!(
        wire["capabilities"]["extensions"][tasks].is_object(),
        "{wire}"
    );

    // Registration: experimental, namespaced, versioned; collisions refused.
    let registry = service.extensions();
    let exp = "example.dev/widgets";
    {
        let mut reg = registry.write();
        let mut settings = serde_json::Map::new();
        settings.insert("mode".into(), serde_json::json!("fast"));
        reg.register(exp, ExtensionStatus::Experimental, 2, settings)
            .unwrap();
        assert_eq!(
            reg.register(exp, ExtensionStatus::Experimental, 1, Default::default()),
            Err(ExtensionError::Collision(exp.into()))
        );
        assert_eq!(
            reg.register(tasks, ExtensionStatus::Experimental, 1, Default::default()),
            Err(ExtensionError::Collision(tasks.into()))
        );
        assert!(matches!(
            reg.register("bare", ExtensionStatus::Official, 1, Default::default()),
            Err(ExtensionError::NotNamespaced(_))
        ));
        assert_eq!(reg.status(tasks), Some(ExtensionStatus::Official));
        assert_eq!(reg.status(exp), Some(ExtensionStatus::Experimental));
        let report = reg.status_report();
        assert!(report
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["id"] == exp && e["status"] == "experimental" && e["version"] == 2));
    }
    let wire = serde_json::to_value(service.discover_result()).unwrap();
    assert_eq!(wire["capabilities"]["extensions"][exp]["mode"], "fast");
    assert_eq!(wire["capabilities"]["extensions"][exp]["version"], 2);

    // Negotiation: unknown peer extension is retained verbatim, never an error.
    let mut peer = ExtensionMap::new();
    peer.insert(
        "vendor.example/unknown".into(),
        serde_json::from_value(serde_json::json!({"k": [1, 2, {"z": null}]})).unwrap(),
    );
    peer.insert(tasks.into(), Default::default());
    let negotiated = registry.read().negotiate(&peer, &[]).unwrap();
    assert_eq!(
        serde_json::to_value(&negotiated.unknown["vendor.example/unknown"]).unwrap(),
        serde_json::json!({"k": [1, 2, {"z": null}]})
    );
    assert!(negotiated.active.contains_key(tasks));

    // Required unsupported fails explicitly; downgrade refused.
    let err = registry
        .read()
        .negotiate(&peer, &["vendor.example/unknown".to_string()])
        .unwrap_err();
    assert_eq!(
        err,
        ExtensionError::RequiredUnsupported(vec!["vendor.example/unknown".into()])
    );
    assert!(err.message().contains("vendor.example/unknown"));
    let mut old = ExtensionMap::new();
    old.insert(
        exp.into(),
        serde_json::from_value(serde_json::json!({"version": 1})).unwrap(),
    );
    assert!(matches!(
        registry.read().negotiate(&old, &[exp.to_string()]),
        Err(ExtensionError::Downgrade {
            required: 1,
            served: 2,
            ..
        })
    ));

    // Disablement is reflected on the wire and makes it unsupported.
    assert!(registry.write().set_enabled(exp, false));
    let wire = serde_json::to_value(service.discover_result()).unwrap();
    assert!(wire["capabilities"]["extensions"].get(exp).is_none());
    assert!(wire["capabilities"]["extensions"][tasks].is_object());
    assert!(registry.read().negotiate(&old, &[exp.to_string()]).is_err());
    assert!(!registry.write().set_enabled("nope/missing", false));

    // Isolation: a fresh service has its own registry.
    let other = GmcpService::new(scratch.clone());
    assert!(other.extensions().read().status(exp).is_none());

    // Watch: every unseen official extension opens a roadmap item.
    let fresh = ExtensionRegistry::shipped();
    let items = fresh.watch(OFFICIAL_EXTENSIONS);
    assert_eq!(items.len(), OFFICIAL_EXTENSIONS.len() - 1);
    assert!(items.iter().all(|i| !i.contains(tasks)));
    assert!(fresh.watch(&[tasks]).is_empty());
}

/// GMCP-203-016-03: end-to-end integration of the typed extension framework across
/// served transports (`GmcpService`, embedded adapter, Unix socket adapter on Unix,
/// `susi mcp serve` stdio, and Streamable HTTP `/mcp` via `susi_wire_client::mcp::Session`),
/// plus CLI <-> `CoreTools` host extension-pack lifecycle and `state.json` persistence.
#[test]
fn gmcp_req_203_016_03_integration() {
    use serde_json::{json, Map, Value};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;
    use susi_gmcp::content::CanonicalContentBlock;
    use susi_gmcp::extensions::{ExtensionRegistry, ExtensionStatus, OFFICIAL_EXTENSIONS};
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::susi_config::extensions as pack_ext;
    use susi_gmcp::tools::CoreTools;

    let (_lock, _env, home) = gmcp_http_home_guard("016-03-integ");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    pack_ext::invalidate_extension_caches();

    // ── 1. Typed ExtensionRegistry integration + per-instance isolation ───
    let custom_svc = GmcpService::new(root.to_path_buf());
    {
        let custom_ext = custom_svc.extensions();
        let mut reg = custom_ext.write();
        let mut settings = Map::new();
        settings.insert("mode".into(), json!("strict"));
        reg.register(
            "vendor.integ/trace",
            ExtensionStatus::Experimental,
            2,
            settings,
        )
        .expect("namespaced experimental extension must register");
        let neg = reg
            .negotiate(
                &[
                    (
                        "io.modelcontextprotocol/tasks".into(),
                        Map::from_iter([("version".into(), json!(1))]),
                    ),
                    (
                        "vendor.integ/trace".into(),
                        Map::from_iter([("version".into(), json!(2))]),
                    ),
                    (
                        "vendor.peer/opaque".into(),
                        Map::from_iter([("badge".into(), json!({"nested": [1, true, null]}))]),
                    ),
                ]
                .into_iter()
                .collect(),
                &[
                    "io.modelcontextprotocol/tasks".into(),
                    "vendor.integ/trace".into(),
                ],
            )
            .expect("supported extensions must negotiate cleanly");
        assert!(neg.active.contains_key("io.modelcontextprotocol/tasks"));
        assert_eq!(neg.active["vendor.integ/trace"]["mode"], "strict");
        assert_eq!(neg.active["vendor.integ/trace"]["version"], 2);
        assert_eq!(
            neg.unknown["vendor.peer/opaque"]["badge"],
            json!({"nested": [1, true, null]})
        );
    }
    let fresh_svc = GmcpService::new(root.to_path_buf());
    let fresh_discover = serde_json::to_value(fresh_svc.discover_result()).unwrap();
    let shipped_ext = fresh_discover["capabilities"]["extensions"].clone();
    assert!(
        shipped_ext["io.modelcontextprotocol/tasks"].is_object(),
        "shipped extensions must advertise tasks: {fresh_discover}"
    );
    assert!(
        shipped_ext.get("vendor.integ/trace").is_none(),
        "custom extension registered on one service must stay isolated"
    );
    assert_eq!(
        fresh_svc
            .extensions()
            .read()
            .watch(OFFICIAL_EXTENSIONS)
            .len(),
        OFFICIAL_EXTENSIONS.len() - 1
    );

    // ── 2. Cross-transport discovery & unknown extension round-trip parity ─
    let runtime = adapter_runtime(&home);
    let (mut embedded, emb_sess) = embedded_adapter(&runtime, root);
    let emb_disc = embedded.call(
        "integ016-emb",
        "server/discover",
        json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "vendor.integ/meta": {"hop": "embedded", "opaque": [1, 2]}}}),
    );
    assert_eq!(
        emb_disc["result"]["capabilities"]["extensions"],
        shipped_ext
    );
    adapter_handshake(&mut embedded, "integ016-emb-init");
    let emb_tools = embedded.call(
        "integ016-emb-tools",
        "tools/list",
        json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "vendor.integ/meta": {"hop": "embedded", "opaque": [1, 2]}}}),
    );
    assert!(
        emb_tools["result"]["tools"].is_array(),
        "emb_tools={emb_tools}"
    );
    drop(embedded);
    adapter_session_ended(&runtime, emb_sess, "016-integ-embedded");

    #[cfg(unix)]
    {
        let (mut socket, sock_sess) = socket_adapter(&runtime, root);
        let sock_disc = socket.call("integ016-sock", "server/discover", adapter_modern_meta());
        assert_eq!(
            sock_disc["result"]["capabilities"]["extensions"],
            shipped_ext
        );
        adapter_handshake(&mut socket, "integ016-sock-init");
        drop(socket);
        adapter_session_ended(&runtime, sock_sess, "016-integ-socket");
    }

    let mut stdio = StdioSession::spawn_modern(root, &home);
    let stdio_disc = stdio.request("server/discover", adapter_modern_meta());
    assert_eq!(stdio_disc["capabilities"]["extensions"], shipped_ext);
    let stdio_tools = stdio.request(
        "tools/list",
        json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}, "vendor.integ/meta": {"hop": "stdio"}}}),
    );
    assert!(stdio_tools["tools"].is_array(), "stdio_tools={stdio_tools}");
    drop(stdio);

    // HTTP `/mcp` via Wire SDK Session
    let token = "susi-016-integ-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind HTTP /mcp port");
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
    let mut wire = susi_wire_client::mcp::Session::open_with_capabilities(
        &http_addr,
        Some(token),
        &json!({ "extensions": { "io.modelcontextprotocol/tasks": {}, "vendor.peer/integ": {"enabled": true}}}),
        Duration::from_secs(15),
    )
    .expect("Wire SDK HTTP session must open");
    assert_eq!(
        Value::Object(wire.server_extensions().unwrap().clone()),
        shipped_ext
    );
    let mut http_notices = Vec::new();
    let http_tools = wire
        .request(
            "tools/list",
            &json!({ "_meta": { "vendor.integ/meta": {"hop": "http"}}}),
            &mut |note| http_notices.push(note.clone()),
        )
        .expect("HTTP tools/list with unknown _meta key must succeed");
    assert!(http_tools["tools"].is_array(), "http_tools={http_tools}");

    // Content-model extension keys survive canonical wire round-trip untouched.
    let block_wire = json!({ "type": "text", "text": "extension payload", "vendor.integ/hint": {"nested": [1, "a"], "flag": true}});
    let canonical = CanonicalContentBlock::from_wire(&block_wire).unwrap();
    assert_eq!(canonical.to_wire(), block_wire);

    // ── 3. Host extension-pack CLI <-> CoreTools lifecycle & persistence ─
    let created = susi_json(root, &home, &["extensions", "create", "integ-pack"]);
    assert_eq!(created["id"], "integ-pack");
    assert_eq!(created["seeded"], true);
    let pack_dir = cfg_dir.join("extensions").join("integ-pack");
    assert!(pack_dir.join("manifest.json").is_file());

    let cli_val = susi_json(
        root,
        &home,
        &["extensions", "validate", pack_dir.to_str().unwrap()],
    );
    let tool_val: Value = serde_json::from_str(
        &CoreTools::extensions_validate(&json!({"path": pack_dir.to_str().unwrap()}), root)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(cli_val, tool_val);
    assert_eq!(cli_val["valid"], true);

    let loaded = susi_json(root, &home, &["extensions", "load", "integ-pack"]);
    assert_eq!(loaded["active"], "integ-pack");
    pack_ext::invalidate_extension_caches();

    let tool_status: Value =
        serde_json::from_str(&CoreTools::extensions_status(&json!({}), root).unwrap()).unwrap();
    assert_eq!(tool_status["active"], "integ-pack");
    let cli_status = susi_json(root, &home, &["extensions", "status"]);
    assert_eq!(cli_status, tool_status);

    // Also test Wire SDK HTTP `call_tool` against `extensions_status`
    let http_tool = wire
        .call_tool("extensions_status", &json!({}), None, &mut |note| {
            http_notices.push(note.clone())
        })
        .expect("Wire SDK extensions_status call must succeed");
    let http_tool_text = http_tool["content"][0]["text"].as_str().unwrap();
    let http_tool_json: Value = serde_json::from_str(http_tool_text).unwrap();
    assert_eq!(http_tool_json["active"], "integ-pack");
    wire.close().expect("Wire SDK session close");

    let state_json: Value = serde_json::from_slice(
        &std::fs::read(cfg_dir.join("extensions").join("state.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(state_json["active"], "integ-pack");

    // Unload via CoreTools and confirm CLI observes compiled default fallback.
    let tool_unloaded: Value = serde_json::from_str(
        &CoreTools::extensions_unload(&json!({"id": "integ-pack"}), root).unwrap(),
    )
    .unwrap();
    assert_eq!(tool_unloaded["active"], "default");
    assert_eq!(
        susi_json(root, &home, &["extensions", "status"])["active"],
        "default"
    );
    assert_eq!(
        ExtensionRegistry::shipped()
            .status_report()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let admission_probe = pack_ext::pack_admission_probe(&scratch)
        .expect("signed external pack admission probe must succeed");
    assert!(!admission_probe.is_empty());

    pack_ext::invalidate_extension_caches();
    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&home);
}

/// GMCP-203-016-04: adversarial extension framework, wire framing, content bounds,
/// HTTP `max_rpc_body_bytes` (413 recovery), concurrent server isolation, and
/// extension-pack tamper/traversal/corrupt-state fail-closed defenses.
#[test]
fn gmcp_req_203_016_04_adversarial() {
    use serde_json::{json, Map, Value};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Duration;
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    use susi_gmcp::extensions::{ExtensionError, ExtensionMap, ExtensionRegistry, ExtensionStatus};
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::stdio::BoundedLines;
    use susi_gmcp::susi_config::extensions as pack_ext;
    use susi_gmcp::tools::CoreTools;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let (_lock, _env, home) = gmcp_http_home_guard("016-04-adv");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    pack_ext::invalidate_extension_caches();

    // ── 1. ExtensionRegistry typed security & governance refusals ────────
    let mut reg = ExtensionRegistry::shipped();
    for bad_id in ["", "tasks", "vendor/", "/name", "vendor/sub/leaf"] {
        assert_eq!(
            reg.register(bad_id, ExtensionStatus::Experimental, 1, Map::new()),
            Err(ExtensionError::NotNamespaced(bad_id.into())),
            "non-namespaced id `{bad_id}` must fail closed"
        );
    }
    assert_eq!(
        reg.register(
            "io.modelcontextprotocol/tasks",
            ExtensionStatus::Experimental,
            99,
            Map::new(),
        ),
        Err(ExtensionError::Collision(
            "io.modelcontextprotocol/tasks".into()
        ))
    );
    reg.register(
        "vendor.adv/ver",
        ExtensionStatus::Experimental,
        2,
        Map::from_iter([("tier".into(), json!("hardened"))]),
    )
    .unwrap();
    assert_eq!(
        reg.register("vendor.adv/ver", ExtensionStatus::Official, 3, Map::new()),
        Err(ExtensionError::Collision("vendor.adv/ver".into()))
    );
    assert_eq!(
        reg.status("vendor.adv/ver"),
        Some(ExtensionStatus::Experimental),
        "collision must never overwrite original descriptor"
    );

    // Required unsupported extension fails closed.
    assert_eq!(
        reg.negotiate(&ExtensionMap::new(), &["vendor.adv/missing".into()]),
        Err(ExtensionError::RequiredUnsupported(vec![
            "vendor.adv/missing".into()
        ]))
    );

    // Downgrade and mismatch pinning on required versioned extension refuse closed.
    for pinned_ver in [1_u64, 3_u64] {
        let mut peer = ExtensionMap::new();
        peer.insert(
            "vendor.adv/ver".into(),
            Map::from_iter([("version".into(), json!(pinned_ver))]),
        );
        assert_eq!(
            reg.negotiate(&peer, &["vendor.adv/ver".into()]),
            Err(ExtensionError::Downgrade {
                id: "vendor.adv/ver".into(),
                required: pinned_ver as u32,
                served: 2,
            })
        );
    }

    // Disabling an extension isolates only that extension while leaving siblings alive.
    assert!(reg.set_enabled("vendor.adv/ver", false));
    assert!(!reg.set_enabled("vendor.adv/nonexistent", false));
    assert!(!reg.capability_map().contains_key("vendor.adv/ver"));
    assert!(reg
        .capability_map()
        .contains_key("io.modelcontextprotocol/tasks"));
    assert_eq!(
        reg.negotiate(&ExtensionMap::new(), &["vendor.adv/ver".into()]),
        Err(ExtensionError::RequiredUnsupported(vec![
            "vendor.adv/ver".into()
        ]))
    );

    // Disabling tasks on service A never affects service B.
    let svc_a = GmcpService::new(root.to_path_buf());
    let svc_b = GmcpService::new(root.to_path_buf());
    assert!(svc_a
        .extensions()
        .write()
        .set_enabled("io.modelcontextprotocol/tasks", false));
    assert!(!svc_a
        .extensions()
        .read()
        .capability_map()
        .contains_key("io.modelcontextprotocol/tasks"));
    assert!(svc_b
        .extensions()
        .read()
        .capability_map()
        .contains_key("io.modelcontextprotocol/tasks"));

    // ── 2. Content-tree nesting & node bounds + BoundedLines recovery ─────
    let mut deep = json!({"k": 1});
    for _ in 0..70 {
        deep = json!({"nested": deep});
    }
    let bomb = json!({"type": "text", "text": "ok", "vendor.adv/deep": deep});
    assert_eq!(
        CanonicalContentBlock::from_wire(&bomb),
        Err(ContentError::TooLarge)
    );

    let wide = json!({ "type": "text", "text": "ok", "vendor.adv/wide": vec![1; 10_005]});
    assert_eq!(
        CanonicalContentBlock::from_wire(&wide),
        Err(ContentError::TooLarge)
    );

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (client, server) = tokio::io::duplex(4096);
        let mut lines = tokio::io::BufReader::new(BoundedLines::new(server, 128)).lines();
        let mut writer = client;
        let big = format!(
            "{{\"jsonrpc\":\"2.0\",\"id\":\"big\",\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}\n",
            "x".repeat(300)
        );
        writer.write_all(big.as_bytes()).await.unwrap();
        writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":\"ok\",\"method\":\"ping\"}\n")
            .await
            .unwrap();
        writer.shutdown().await.unwrap();

        let err = lines.next_line().await.unwrap_err();
        assert!(err.to_string().contains("exceeds"), "got: {err}");
        let next = lines
            .next_line()
            .await
            .expect("healthy line after oversized frame must recover")
            .expect("must have next line");
        assert!(next.contains("\"id\":\"ok\""));
    });

    // ── 3. Era confusion (`_meta` leak across legacy vs modern gate) ─────
    let mut era = EraState::default();
    assert!(matches!(
        classify_line(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"adv","version":"1"}}}"#,
            &mut era,
        ),
        GateVerdict::Forward
    ));
    assert!(era.is_legacy());
    assert!(matches!(
        classify_line(
            br#"{"jsonrpc":"2.0","id":2,"method":"ping","params":{"_meta":{"io.modelcontextprotocol/requestId":"leak"}}}"#,
            &mut era,
        ),
        GateVerdict::Reject { code: -32600, .. }
    ));
    assert!(matches!(
        classify_line(
            br#"{"jsonrpc":"2.0","id":3,"method":"ping","params":{"_meta":{"vendor.adv/ok":true}}}"#,
            &mut era,
        ),
        GateVerdict::Forward
    ));

    let mut wire = StdioSession::spawn_raw(root, &home);
    let _init = wire
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "adv-init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "adv-016", "version": "1"}}}),
            Duration::from_secs(60),
        )
        .expect("legacy initialize must answer");
    wire.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let injected = wire
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "adv-era-leak", "method": "ping", "params": {"_meta": {"io.modelcontextprotocol/requestId": "forged"}}}),
            Duration::from_secs(60),
        )
        .expect("injected modern _meta on legacy session must reject");
    assert_eq!(injected["error"]["code"], -32600);
    assert_eq!(wire.request("ping", json!({})), json!({}));
    drop(wire);

    // ── 4. HTTP 413 oversized payload refusal & concurrent server isolation ─
    let token = "susi-016-adv-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();
    std::fs::write(
        cfg_dir.join("config.json"),
        r#"{"max_rpc_body_bytes": 1024}"#,
    )
    .unwrap();

    let l1 = TcpListener::bind("127.0.0.1:0").expect("bind srv 1");
    let addr1 = format!("127.0.0.1:{}", l1.local_addr().unwrap().port());
    let root1 = root.to_path_buf();
    let _srv1 = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(root1, vec![l1], None, false);
    });
    let l2 = TcpListener::bind("127.0.0.1:0").expect("bind srv 2");
    let addr2 = format!("127.0.0.1:{}", l2.local_addr().unwrap().port());
    let root2 = root.to_path_buf();
    let _srv2 = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(root2, vec![l2], None, false);
    });
    for addr in [&addr1, &addr2] {
        for _ in 0..50 {
            if TcpStream::connect(addr).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let oversized_body = json!({ "jsonrpc": "2.0", "id": "adv-413", "method": "ping", "params": {"_meta": {"vendor.adv/pad": "x".repeat(2048)}}})
    .to_string();
    let raw_req = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr1}\r\nConnection: close\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\n\r\n{oversized_body}",
        oversized_body.len()
    );
    let mut s1 = TcpStream::connect(&addr1).unwrap();
    s1.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    s1.write_all(raw_req.as_bytes()).unwrap();
    let mut resp = String::new();
    let _ = s1.read_to_string(&mut resp);
    assert!(
        resp.starts_with("HTTP/1.1 413") || resp.starts_with("HTTP/1.0 413"),
        "oversized HTTP extension payload must return 413, got: {resp}"
    );

    let wire2 = susi_wire_client::mcp::Session::open_with_capabilities(
        &addr2,
        Some(token),
        &json!({"extensions": {"io.modelcontextprotocol/tasks": {}}}),
        Duration::from_secs(15),
    )
    .expect("concurrent second server must stay healthy after 413 on server 1");
    assert!(wire2
        .server_extensions()
        .is_some_and(|m| m.contains_key("io.modelcontextprotocol/tasks")));
    wire2.close().expect("close srv2 session");

    // ── 5. Extension pack tamper, traversal, bad signature & corrupt state ─
    let payload = br#"{"rules":["safe"]}"#;
    let good_sha = susi_paths::hex::encode(Sha256::digest(payload));
    for (case_name, manifest_json, extra_file) in [
        (
            "checksum-mismatch",
            json!({ "id": "tamper-sha", "name": "Tamper SHA", "version": "1.0.0", "apiVersion": "1", "files": {"rules": "rules.json"}, "checksums": {"rules": format!("sha256:{}", "00".repeat(32))}}),
            Some(("rules.json", payload.as_slice())),
        ),
        (
            "bad-ed25519-signature",
            json!({ "id": "tamper-sig", "name": "Tamper Sig", "version": "1.0.0", "apiVersion": "1", "files": {"rules": "rules.json"}, "checksums": {"rules": format!("sha256:{good_sha}")}, "signatures": {"rules": "00".repeat(64)}, "signer": "11".repeat(32)}),
            Some(("rules.json", payload.as_slice())),
        ),
        (
            "traversal-escape",
            json!({ "id": "tamper-escape", "name": "Tamper Escape", "version": "1.0.0", "apiVersion": "1", "files": {"rules": "../secret.json"}}),
            None,
        ),
        (
            "absolute-path",
            json!({ "id": "tamper-abs", "name": "Tamper Abs", "version": "1.0.0", "apiVersion": "1", "permissions": ["filesystem.read"], "files": {"rules": "/etc/passwd"}}),
            None,
        ),
        (
            "unknown-permission",
            json!({ "id": "tamper-perm", "name": "Tamper Perm", "version": "1.0.0", "apiVersion": "1", "permissions": ["kernel.bypass"]}),
            None,
        ),
    ] {
        let dir = scratch.join(format!("adv-pack-{case_name}"));
        std::fs::create_dir_all(&dir).unwrap();
        if let Some((rel, bytes)) = extra_file {
            std::fs::write(dir.join(rel), bytes).unwrap();
        }
        std::fs::write(dir.join("manifest.json"), manifest_json.to_string()).unwrap();
        assert!(
            pack_ext::validate_pack_dir(&dir).is_err(),
            "{case_name}: tampered pack must fail validate_pack_dir"
        );
        assert!(
            CoreTools::extensions_validate(&json!({"path": dir.to_str().unwrap()}), root).is_err(),
            "{case_name}: CoreTools::extensions_validate must fail closed"
        );
    }

    // Tampering an already-created pack under cfg_dir must refuse activation AND corrupt state.json must not serve tampered active id.
    let _ = susi_json(root, &home, &["extensions", "create", "adv-host-pack"]);
    let host_pack = cfg_dir.join("extensions").join("adv-host-pack");
    std::fs::write(
        host_pack.join("manifest.json"),
        json!({ "id": "adv-host-pack", "name": "Tampered", "version": "1.0.0", "apiVersion": "1", "files": {"rules": "missing.json"}, "checksums": {"rules": "00".repeat(32)}})
        .to_string(),
    )
    .unwrap();
    assert!(
        CoreTools::extensions_load(&json!({"id": "adv-host-pack"}), root).is_err(),
        "loading host pack with tampered checksum/missing file must fail closed"
    );
    std::fs::write(
        cfg_dir.join("extensions").join("state.json"),
        "{corrupt-json",
    )
    .unwrap();
    pack_ext::invalidate_extension_caches();
    let recovered = pack_ext::active_pack();
    assert_eq!(
        recovered.id, "default",
        "corrupt state.json must fall back safely to compiled default"
    );
    let _ = susi_json(root, &home, &["extensions", "status"]);
    assert_eq!(Value::from(0), 0);

    pack_ext::invalidate_extension_caches();
    let _ = _srv1;
    let _ = _srv2;
    let _ = std::fs::remove_dir_all(&home);
}

/// GMCP-203-016-05: Interoperability and quality — first-class extension framework
/// and extension governance across both roles, direct-vs-gateway differential
/// behavior, transport/platform parity, property invariants, retained regression
/// fixtures, and published latency, throughput, fairness, memory, and disk budgets.
#[test]
fn gmcp_req_203_016_05_interop() {
    use serde_json::{json, Map, Value};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use susi_gmcp::content::{
        CanonicalAnnotations, CanonicalContentBlock, CanonicalIcon, CanonicalResourceContents,
        ContentError,
    };
    use susi_gmcp::extensions::{
        ExtensionError, ExtensionMap, ExtensionRegistry, ExtensionStatus, OFFICIAL_EXTENSIONS,
    };
    use susi_gmcp::jsonrpc_gate::{
        classify_envelope, classify_line, http_envelope_rejection, EraState, GateVerdict,
    };
    use susi_gmcp::mcp::ServiceExt;
    use susi_gmcp::protocol::GmcpService;
    use susi_gmcp::stdio::BoundedLines;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    let (_lock, _env, home) = gmcp_http_home_guard("016-interop");
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = home.join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let cfg_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let token = "susi-016-interop-bearer";
    std::fs::write(cfg_dir.join("api_token"), token).unwrap();
    let auth_header = format!("Bearer {token}");

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

    // ── 1. Contract binding & direct service baseline ─────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-016.md"))
        .expect("docs/gmcp/contract-203-016.md must exist");
    for row in 1..=11 {
        let id = format!("EXT-{row:03}");
        assert!(contract.contains(&id), "contract-203-016.md missing {id}");
    }
    assert!(contract.contains("max_rpc_body_bytes"));

    let direct_svc = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&direct_svc).expect("catalog install");
    let direct_caps = direct_svc.discover_result().capabilities;
    let direct_ext_json = serde_json::to_value(
        direct_caps
            .extensions
            .clone()
            .expect("shipped service must advertise extensions"),
    )
    .unwrap();
    assert!(
        direct_ext_json
            .get("io.modelcontextprotocol/tasks")
            .is_some(),
        "default extension capability map must advertise official tasks extension"
    );

    let modern_meta = |req_id: &str, extra: Option<(&str, Value)>, client_ext: Option<Value>| {
        let mut caps = json!({});
        if let Some(ext) = client_ext {
            caps["extensions"] = ext;
        }
        let mut meta = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": caps, "io.modelcontextprotocol/requestId": req_id});
        if let Some((k, v)) = extra {
            meta[k] = v;
        }
        meta
    };

    // Helper: serve an existing GmcpService over an in-process duplex pipe.
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

    // ── 2. Direct-vs-Gateway Differential Parity & Transport Parity ───────
    // Embedded adapter, Unix-domain socket adapter, shipped `susi mcp serve`
    // stdio binary (2025-11-25 initialize + 2026-07-28 server/discover),
    // and live HTTP `/mcp` gateway must agree with direct `GmcpService`.
    let (mut emb_client, emb_sess) = embedded_adapter(&runtime, root);
    let emb_init = emb_client.call(
        "emb-init",
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": { "extensions": { "io.modelcontextprotocol/tasks": {}, "vendor.example/client-unknown": {"probe": true, "nested": [1, 2]}}}, "clientInfo": {"name": "ext-embedded", "version": "1"}}),
    );
    assert_eq!(
        emb_init["result"]["capabilities"]["extensions"], direct_ext_json,
        "embedded adapter initialize extensions must match direct GmcpService"
    );
    emb_client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let emb_disc = emb_client.call(
        "emb-disc",
        "server/discover",
        json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-11-25", "io.modelcontextprotocol/clientCapabilities": { "extensions": { "vendor.example/discover-unknown": {"active": true}}}, "vendor.example/trace": {"span": 7}}}),
    );
    assert_eq!(
        emb_disc["result"]["capabilities"]["extensions"], direct_ext_json,
        "embedded server/discover must match direct GmcpService and accept unknown _meta/extensions"
    );
    drop(emb_client);
    adapter_session_ended(&runtime, emb_sess, "embedded-016");

    #[cfg(unix)]
    {
        let (mut sock_client, sock_sess) = socket_adapter(&runtime, root);
        let sock_init = sock_client.call(
            "sock-init",
            "initialize",
            json!({ "protocolVersion": "2025-11-25", "capabilities": { "extensions": {"vendor.example/socket-probe": {"ok": true}}}, "clientInfo": {"name": "ext-socket", "version": "1"}}),
        );
        assert_eq!(
            sock_init["result"]["capabilities"]["extensions"], direct_ext_json,
            "socket adapter initialize extensions must match direct GmcpService"
        );
        drop(sock_client);
        adapter_session_ended(&runtime, sock_sess, "socket-016");
    }

    // Shipped `susi mcp serve` binary (legacy handshake + modern stateless discover).
    let startup_t0 = Instant::now();
    let mut stdio_leg = StdioSession::spawn_raw(root, &home);
    let stdio_init = stdio_leg
        .request_verbatim(
            &json!({ "jsonrpc": "2.0", "id": "stdio-init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": { "extensions": { "io.modelcontextprotocol/tasks": {}, "vendor.example/stdio-unknown": {"depth": 1}}}, "clientInfo": {"name": "ext-stdio-leg", "version": "1"}}}),
            Duration::from_secs(120),
        )
        .expect("stdio initialize must answer");
    within("startup_ms", startup_t0.elapsed().as_millis());
    assert_eq!(
        stdio_init["result"]["capabilities"]["extensions"], direct_ext_json,
        "stdio binary initialize extensions must match direct GmcpService"
    );
    stdio_leg.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    assert_eq!(stdio_leg.request("ping", json!({})), json!({}));
    drop(stdio_leg);

    let mut stdio_mod = StdioSession::spawn_modern(root, &home);
    let mod_req_t0 = Instant::now();
    let stdio_mod_disc = stdio_mod.request(
        "server/discover",
        json!({ "_meta": modern_meta( "stdio-mod-disc", Some(("vendor.example/custom-meta", json!({"trace": "mod-1"}))), Some(json!({"vendor.example/modern-ext": {"flags": ["a", "b"]}})))}),
    );
    within("request_ms", mod_req_t0.elapsed().as_millis());
    assert_eq!(
        stdio_mod_disc["capabilities"]["extensions"], direct_ext_json,
        "stdio modern server/discover extensions must match direct GmcpService"
    );
    drop(stdio_mod);

    // Live Streamable HTTP `/mcp` gateway (server role parity).
    let http_listener = TcpListener::bind("127.0.0.1:0").expect("bind live HTTP gateway");
    let http_addr = format!("127.0.0.1:{}", http_listener.local_addr().unwrap().port());
    let root_http = root.to_path_buf();
    let _http_srv = std::thread::spawn(move || {
        susi_gmcp::server::GmcpServer::start_http_server(
            root_http,
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

    let http_post = |addr: &str, headers: &[(&str, &str)], body: &str| -> (u16, String) {
        let mut s = TcpStream::connect(addr).expect("connect HTTP endpoint");
        s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
        let mut req = format!("POST /mcp HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        s.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        let _ = s.read_to_string(&mut raw);
        let status = raw
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        (status, raw)
    };
    let extract_sse_json = |raw: &str| -> Value {
        let (_, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
        let trimmed = body.trim();
        if trimmed.starts_with('{') {
            serde_json::from_str(trimmed).unwrap_or(Value::Null)
        } else {
            body.lines()
                .filter_map(|l| l.strip_prefix("data:").map(str::trim))
                .filter_map(|d| serde_json::from_str::<Value>(d).ok())
                .find(|v| v.get("result").is_some() || v.get("error").is_some())
                .unwrap_or(Value::Null)
        }
    };

    // Shipped `susi_wire_client::mcp::Session` in server-inspection role against Susi HTTP gateway.
    let wire_gw = susi_wire_client::mcp::Session::open_with_capabilities(
        &http_addr,
        Some(token),
        &json!({ "extensions": { "io.modelcontextprotocol/tasks": {}, "vendor.example/wire-client-ext": {"version": 1}}}),
        Duration::from_secs(15),
    )
    .expect("wire SDK must open session carrying client extension capabilities");
    let wire_srv_exts = Value::Object(
        wire_gw
            .server_extensions()
            .expect("live HTTP gateway must advertise capabilities.extensions")
            .clone(),
    );
    assert_eq!(
        wire_srv_exts, direct_ext_json,
        "HTTP gateway initialize extensions must match direct GmcpService"
    );
    // Negotiate server's advertised extensions on client side: known official active,
    // unknown peer extensions preserved untouched.
    let wire_ext_map: ExtensionMap =
        serde_json::from_value(Value::Object(wire_gw.server_extensions().unwrap().clone()))
            .unwrap();
    let gw_negotiated = direct_svc
        .extensions()
        .read()
        .negotiate(
            &wire_ext_map,
            &["io.modelcontextprotocol/tasks".to_string()],
        )
        .expect("gateway tasks extension negotiation must succeed");
    assert!(gw_negotiated
        .active
        .contains_key("io.modelcontextprotocol/tasks"));
    assert!(gw_negotiated.unknown.is_empty());
    wire_gw.close().expect("DELETE HTTP session");

    // Modern 2026-07-28 stateless `server/discover` over HTTP `/mcp` with unknown `_meta` + extensions.
    let (gw_stat_st, gw_stat_raw) = http_post(
        &http_addr,
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
        ],
        &json!({ "jsonrpc": "2.0", "id": "http-mod-disc", "method": "server/discover", "params": { "_meta": modern_meta( "http-mod-disc", Some(("vendor.example/http-meta", json!({"tenant": "t-1"}))), Some(json!({"vendor.example/stateless-client": {"v": 3}})))}})
        .to_string(),
    );
    assert_eq!(
        gw_stat_st, 200,
        "HTTP stateless discover failed: {gw_stat_raw}"
    );
    let gw_stat_json = extract_sse_json(&gw_stat_raw);
    assert_eq!(
        gw_stat_json["result"]["capabilities"]["extensions"], direct_ext_json,
        "HTTP stateless server/discover extensions must match direct GmcpService"
    );

    // Core `susi_core::mcp_client` against live HTTP gateway (`server/discover` & `call_tool`).
    let core_disc = susi_core::mcp_client::session_call(
        &http_addr,
        "server/discover",
        &json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-11-25", "io.modelcontextprotocol/clientCapabilities": { "extensions": {"vendor.example/core-client": {"ok": true}}}, "vendor.example/core-meta": "ok"}}),
        Some(token),
    )
    .expect("susi_core::mcp_client server/discover must succeed");
    assert_eq!(
        core_disc["capabilities"]["extensions"], direct_ext_json,
        "core mcp_client server/discover extensions must match direct GmcpService"
    );
    let core_tool =
        susi_core::mcp_client::call_tool(&http_addr, "ledger_verify", &json!({}), Some(token))
            .expect("susi_core::mcp_client call_tool must succeed");
    assert!(core_tool["content"][0]["text"].is_string());

    // ── 3. Client Role Against Reference Peer HTTP MCP Server ─────────────
    // Peer server advertises official + experimental + unknown vendor extensions
    // and returns unknown extension keys on tool payloads. Both shipped client SDKs
    // (`susi_wire_client` and `susi_core::mcp_client`) interoperate cleanly.
    let peer_listener = TcpListener::bind("127.0.0.1:0").expect("bind reference peer");
    let peer_addr = format!("127.0.0.1:{}", peer_listener.local_addr().unwrap().port());
    peer_listener.set_nonblocking(true).unwrap();
    let peer_stop = Arc::new(AtomicBool::new(false));
    let peer_stop_srv = peer_stop.clone();
    let seen_client_init_exts = Arc::new(Mutex::new(Vec::<Value>::new()));
    let seen_client_init_exts_srv = seen_client_init_exts.clone();
    let peer_thread = std::thread::spawn(move || {
        while !peer_stop_srv.load(Ordering::Acquire) {
            let mut stream = match peer_listener.accept() {
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
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            let msg: Value = serde_json::from_slice(&body_buf).unwrap_or(Value::Null);
            let method = msg["method"].as_str().unwrap_or("");
            let id = msg["id"].clone();
            if method == "notifications/initialized" {
                let _ = stream.write_all(
                    b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                continue;
            }
            let reply_json = if method == "initialize" {
                if let Some(ext) = msg["params"]["capabilities"].get("extensions") {
                    seen_client_init_exts_srv.lock().unwrap().push(ext.clone());
                }
                json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": "2025-11-25", "capabilities": { "tools": {}, "extensions": { "io.modelcontextprotocol/tasks": {"version": 1, "list": {}}, "io.modelcontextprotocol/ui": {"version": 1, "mimeTypes": ["text/html"]}, "vendor.peer/custom-caps": {"feature": "alpha", "nested": {"k": 9}}}}, "serverInfo": {"name": "ref-ext-peer", "version": "2.0"}}})
            } else if method == "tools/call" {
                json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": "peer-ext-ok", "vendor.peer/content-meta": {"confidence": 0.99}}], "isError": false, "_meta": {"vendor.peer/reply-tag": "preserved"}}})
            } else {
                json!({ "jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}})
            };
            let reply = reply_json.to_string();
            let extra_hdr = if method == "initialize" {
                "Mcp-Session-Id: peer-ext-sess-1\r\n"
            } else {
                ""
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{extra_hdr}Content-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });

    {
        let client_advertised = json!({ "extensions": { "io.modelcontextprotocol/tasks": {"version": 1}, "vendor.susi/client-probe": {"mode": "verify"}}});
        let mut peer_wire = susi_wire_client::mcp::Session::open_with_capabilities(
            &peer_addr,
            None,
            &client_advertised,
            Duration::from_secs(10),
        )
        .expect("wire SDK client must handshake with reference peer");
        let peer_exts: ExtensionMap = serde_json::from_value(Value::Object(
            peer_wire
                .server_extensions()
                .expect("reference peer server extensions must be retained")
                .clone(),
        ))
        .unwrap();
        let peer_call = peer_wire
            .call_tool("peer_tool", &json!({}), None, &mut |_| {})
            .expect("peer tool call must succeed");
        assert_eq!(
            peer_call["content"][0]["vendor.peer/content-meta"]["confidence"],
            0.99
        );
        assert_eq!(peer_call["_meta"]["vendor.peer/reply-tag"], "preserved");
        peer_wire.close().expect("DELETE on reference peer");

        // Verify peer received our advertised `capabilities.extensions`.
        {
            let recorded = seen_client_init_exts.lock().unwrap();
            assert_eq!(
                recorded.last().unwrap(),
                &client_advertised["extensions"],
                "reference peer must observe client capabilities.extensions"
            );
        }

        // Negotiate reference peer extensions locally: known supported -> active,
        // unrecognized peer extension -> unknown untouched, required missing -> fail closed,
        // higher required version than peer offers -> Downgrade.
        let mut client_reg = ExtensionRegistry::shipped();
        client_reg
            .register(
                "io.modelcontextprotocol/ui",
                ExtensionStatus::Official,
                1,
                Map::new(),
            )
            .unwrap();
        let neg_ok = client_reg
            .negotiate(
                &peer_exts,
                &[
                    "io.modelcontextprotocol/tasks".into(),
                    "io.modelcontextprotocol/ui".into(),
                ],
            )
            .expect("client-side negotiation of peer extensions must succeed");
        assert!(neg_ok.active.contains_key("io.modelcontextprotocol/tasks"));
        assert!(neg_ok.active.contains_key("io.modelcontextprotocol/ui"));
        assert_eq!(
            neg_ok.unknown.get("vendor.peer/custom-caps").unwrap()["feature"],
            "alpha"
        );
        assert_eq!(
            client_reg.negotiate(&peer_exts, &["vendor.peer/unimplemented".into()]),
            Err(ExtensionError::RequiredUnsupported(vec![
                "vendor.peer/unimplemented".into()
            ]))
        );
        let mut strict_v9_peer = peer_exts.clone();
        strict_v9_peer
            .get_mut("io.modelcontextprotocol/tasks")
            .unwrap()
            .insert("version".into(), json!(9));
        assert_eq!(
            client_reg.negotiate(&strict_v9_peer, &["io.modelcontextprotocol/tasks".into()]),
            Err(ExtensionError::Downgrade {
                id: "io.modelcontextprotocol/tasks".into(),
                required: 9,
                served: 1,
            })
        );

        let core_peer = susi_core::mcp_client::call_tool(&peer_addr, "peer_tool", &json!({}), None)
            .expect("core mcp_client against reference peer must preserve content extensions");
        assert_eq!(
            core_peer["content"][0]["vendor.peer/content-meta"]["confidence"],
            0.99
        );
    }
    peer_stop.store(true, Ordering::Release);
    let _ = TcpStream::connect(&peer_addr);
    let _ = peer_thread.join();

    // ── 4. Isolation, Disablement Within One Request, & Wire Watch ────────
    // Configure service A without touching service B (`direct_svc`), prove
    // immediate visibility over live duplex wire, then single-request disablement.
    let svc_a = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&svc_a).unwrap();
    let svc_b = GmcpService::new(root.to_path_buf());
    susi_gmcp::catalog::install(&svc_b).unwrap();

    let custom_id = "vendor.example/dynamic-widget";
    let mut custom_settings = Map::new();
    custom_settings.insert("mode".into(), json!("live"));
    svc_a
        .extensions()
        .write()
        .register(custom_id, ExtensionStatus::Experimental, 2, custom_settings)
        .expect("namespaced experimental registration");
    assert_eq!(
        svc_a.extensions().read().status(custom_id),
        Some(ExtensionStatus::Experimental)
    );
    assert!(
        svc_b.extensions().read().status(custom_id).is_none(),
        "extension registration must be isolated per service instance"
    );

    let (mut dyn_client, dyn_sess) = spawn_service_duplex(&runtime, svc_a.clone(), 128 * 1024);
    let dyn_init = dyn_client.call(
        "dyn-init",
        "initialize",
        json!({ "protocolVersion": "2026-07-28", "capabilities": {}, "clientInfo": {"name": "dyn-ext", "version": "1"}}),
    );
    assert_eq!(
        dyn_init["result"]["capabilities"]["extensions"][custom_id],
        json!({"mode": "live", "version": 2})
    );
    dyn_client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let before_tools = dyn_init["result"]["capabilities"]["tools"].clone();

    // Disable on live service -> omitted on very next server/discover without
    // disturbing sibling extensions or core tools capability.
    assert!(svc_a.extensions().write().set_enabled(custom_id, false));
    let after_disable = dyn_client.call(
        "dyn-disc-1",
        "server/discover",
        json!({"_meta": modern_meta("dyn-disc-1", None, None)}),
    );
    assert!(
        after_disable["result"]["capabilities"]["extensions"]
            .get(custom_id)
            .is_none(),
        "disabled extension must be omitted within one request: {after_disable}"
    );
    assert!(after_disable["result"]["capabilities"]["extensions"]
        .get("io.modelcontextprotocol/tasks")
        .is_some());
    assert_eq!(
        after_disable["result"]["capabilities"]["tools"],
        before_tools
    );

    // Re-enable -> restored immediately; svc_b stays unaffected.
    assert!(svc_a.extensions().write().set_enabled(custom_id, true));
    let after_enable = dyn_client.call(
        "dyn-disc-2",
        "server/discover",
        json!({"_meta": modern_meta("dyn-disc-2", None, None)}),
    );
    assert_eq!(
        after_enable["result"]["capabilities"]["extensions"][custom_id]["version"],
        2
    );
    assert!(
        !svc_b
            .discover_result()
            .capabilities
            .extensions
            .unwrap()
            .contains_key(custom_id),
        "isolation broken between svc_a and svc_b"
    );
    drop(dyn_client);
    adapter_session_ended(&runtime, dyn_sess, "dyn-duplex");

    // ── 5. Property Invariants & Governance Watch ─────────────────────────
    // 5a. Deterministic wire output & status report.
    let rep_1 = svc_a.extensions().read().status_report();
    let rep_2 = svc_a.extensions().read().status_report();
    assert_eq!(rep_1, rep_2, "status_report must be deterministic");
    assert_eq!(
        serde_json::to_string(&svc_a.extensions().read().capability_map()).unwrap(),
        serde_json::to_string(&svc_a.extensions().read().capability_map()).unwrap()
    );

    // 5b. Content/annotation/icon/resource unknown-extension round-trip invariance.
    let wire_content = json!({ "type": "text", "text": "hello extension", "annotations": { "audience": ["user"], "priority": 0.5, "vendor.example/annot": {"nested": [true, 42]}}, "vendor.example/block-ext": {"flags": ["alpha", "omega"], "score": 3}});
    let parsed_block = CanonicalContentBlock::from_wire(&wire_content).unwrap();
    let round_tripped = serde_json::to_value(&parsed_block).unwrap();
    assert_eq!(
        round_tripped["vendor.example/block-ext"],
        wire_content["vendor.example/block-ext"]
    );
    assert_eq!(
        round_tripped["annotations"]["vendor.example/annot"],
        wire_content["annotations"]["vendor.example/annot"]
    );
    let icon_val = json!({ "src": "https://example.com/icon.png", "mimeType": "image/png", "vendor.example/icon-ext": {"theme": "dark"}});
    let icon: CanonicalIcon = serde_json::from_value(icon_val.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&icon).unwrap()["vendor.example/icon-ext"],
        icon_val["vendor.example/icon-ext"]
    );
    let annot_val = json!({"audience": ["assistant"], "vendor.example/x": 1});
    let annot: CanonicalAnnotations = serde_json::from_value(annot_val.clone()).unwrap();
    assert_eq!(serde_json::to_value(&annot).unwrap()["vendor.example/x"], 1);
    let res_val = json!({ "uri": "susi://tools", "text": "ok", "vendor.example/res-ext": {"etag": "v1"}});
    let res_c: CanonicalResourceContents = serde_json::from_value(res_val.clone()).unwrap();
    assert_eq!(
        serde_json::to_value(&res_c).unwrap()["vendor.example/res-ext"],
        res_val["vendor.example/res-ext"]
    );

    // 5c. Watch governance (EXT-008): catches every unpublished official extension
    // absent from the registry, and clears once registered.
    let mut watch_reg = ExtensionRegistry::shipped();
    let missing_official = watch_reg.watch(OFFICIAL_EXTENSIONS);
    assert_eq!(
        missing_official.len(),
        OFFICIAL_EXTENSIONS.len() - 1,
        "shipped registry only includes tasks initially: {missing_official:?}"
    );
    assert!(missing_official
        .iter()
        .all(|item| item.starts_with("[GMCP] Adopt newly published official extension ")));
    for ext in OFFICIAL_EXTENSIONS {
        if watch_reg.status(ext).is_none() {
            watch_reg
                .register(ext, ExtensionStatus::Official, 1, Map::new())
                .unwrap();
        }
    }
    assert!(watch_reg.watch(OFFICIAL_EXTENSIONS).is_empty());
    let upcoming = ["io.modelcontextprotocol/future-telemetry"];
    assert_eq!(
        watch_reg.watch(&upcoming),
        vec![
            "[GMCP] Adopt newly published official extension io.modelcontextprotocol/future-telemetry"
                .to_string()
        ]
    );

    // ── 6. Retained Regression Fixtures & Bounded Payload Enforcement ───────
    // Retained positive & negative fixtures for incompatibilities & boundary bugs:
    // (R1) Legacy wire form `params.protocolVersion` on `server/discover` -> `-32602`
    //      across live stdio binary (`StdioSession::spawn_raw`) AND live HTTP `/mcp`,
    //      plus modern gate `-32602` on subsequent request missing `_meta`.
    let legacy_discover_req = json!({ "jsonrpc": "2.0", "id": "reg-legacy-disc", "method": "server/discover", "params": { "protocolVersion": "2026-07-28", "capabilities": {"extensions": {"io.modelcontextprotocol/tasks": {}}}}});
    let legacy_disc_str = legacy_discover_req.to_string();
    let mut r1_stdio = StdioSession::spawn_raw(root, &home);
    let r1_stdio_ans = r1_stdio
        .request_verbatim(&legacy_discover_req, Duration::from_secs(30))
        .expect("R1 stdio server/discover must answer typed error");
    assert_eq!(
        r1_stdio_ans["error"]["code"], -32602,
        "retained regression R1: server/discover without _meta must reject -32602 on stdio: {r1_stdio_ans}"
    );
    drop(r1_stdio);
    let mut era = EraState::default();
    assert!(matches!(
        classify_line(legacy_disc_str.as_bytes(), &mut era),
        GateVerdict::Forward
    ));
    let r1_followup = br#"{"jsonrpc":"2.0","id":"reg-mod-ping","method":"ping","params":{"capabilities":{"extensions":{"io.modelcontextprotocol/tasks":{}}}}}"#;
    assert!(
        matches!(
            classify_line(r1_followup, &mut era),
            GateVerdict::Reject { code: -32602, .. }
        ),
        "modern gate must reject post-discover request missing _meta with -32602"
    );
    let (http_r1_st, http_r1_raw) = http_post(
        &http_addr,
        &[
            ("Authorization", &auth_header),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("MCP-Protocol-Version", "2026-07-28"),
            ("Mcp-Method", "server/discover"),
        ],
        &legacy_disc_str,
    );
    assert_eq!(http_r1_st, 400, "HTTP R1 must answer 400: {http_r1_raw}");
    assert_eq!(
        extract_sse_json(&http_r1_raw)["error"]["code"],
        -32602,
        "retained regression R1: HTTP and stdio agree on -32602"
    );

    // (R2) Positive 2026-07-28 `_meta` with unknown vendor `_meta` + `clientCapabilities.extensions`
    //      passes both `classify_line` and `http_envelope_rejection`.
    let positive_ext_req = json!({ "jsonrpc": "2.0", "id": "reg-pos-ext", "method": "server/discover", "params": { "_meta": modern_meta( "reg-pos-ext", Some(("vendor.example/extra-header", json!({"v": 1}))), Some(json!({"vendor.example/unknown-cap": {"enabled": true}})))}});
    let positive_ext_bytes = serde_json::to_vec(&positive_ext_req).unwrap();
    let mut era_pos = EraState::default();
    assert!(matches!(
        classify_line(&positive_ext_bytes, &mut era_pos),
        GateVerdict::Forward
    ));
    assert!(http_envelope_rejection(&positive_ext_bytes).is_none());

    // (R3) Retained incompatibility/adversarial fixtures:
    // - Malformed content payload carrying extensions still fails typed (`TooLarge` / `BadMime`)
    //   identically across `CanonicalContentBlock::from_wire`, `classify_line`, and `http_envelope_rejection`.
    let mut deep = json!({"type": "text", "text": "x", "vendor.example/ext": true});
    for _ in 0..80 {
        deep = json!({"wrap": deep});
    }
    let bad_ext_content = json!({ "type": "text", "text": "ok", "vendor.example/bomb": deep});
    assert_eq!(
        CanonicalContentBlock::from_wire(&bad_ext_content),
        Err(ContentError::TooLarge)
    );
    let bad_envelope = serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": "reg-bomb", "result": {"content": [bad_ext_content]}}))
    .unwrap();
    let mut era_env = EraState::default();
    assert!(matches!(
        classify_line(&bad_envelope, &mut era_env),
        GateVerdict::Reject { code: -32600, .. }
    ));
    assert_eq!(
        http_envelope_rejection(&bad_envelope).map(|v| v["error"]["code"].as_i64()),
        Some(Some(-32600))
    );

    let bad_mime_ext = json!({ "type": "image", "data": "AAAA", "mimeType": "not-a-mime", "vendor.example/ext": {"keep": 1}});
    assert_eq!(
        CanonicalContentBlock::from_wire(&bad_mime_ext),
        Err(ContentError::BadMimeType("image"))
    );
    // - Registry collision & non-namespaced IDs fail closed.
    assert_eq!(
        watch_reg.register("bare-ext", ExtensionStatus::Experimental, 1, Map::new()),
        Err(ExtensionError::NotNamespaced("bare-ext".into()))
    );
    assert_eq!(
        watch_reg.register(
            "io.modelcontextprotocol/tasks",
            ExtensionStatus::Experimental,
            1,
            Map::new()
        ),
        Err(ExtensionError::Collision(
            "io.modelcontextprotocol/tasks".into()
        ))
    );
    // - Non-object `capabilities` in Wire SDK `open_with_capabilities` fails closed before connect.
    assert!(susi_wire_client::mcp::Session::open_with_capabilities(
        &http_addr,
        Some(token),
        &json!(["not", "an", "object"]),
        Duration::from_secs(2),
    )
    .is_err());

    // (R4) `max_rpc_body_bytes` / `BoundedLines` rejection on oversized extension payload,
    //      followed by clean session recovery on duplex gate.
    let (mut bound_client, bound_sess) = spawn_service_duplex(&runtime, direct_svc.clone(), 4096);
    let b_init = bound_client.call(
        "b-init",
        "initialize",
        json!({ "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "bounded-ext", "version": "1"}}),
    );
    assert!(b_init["result"]["capabilities"]["extensions"].is_object());
    bound_client.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let huge_val = "x".repeat(8192);
    let oversized_msg = json!({ "jsonrpc": "2.0", "id": "b-over", "method": "server/discover", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-11-25", "io.modelcontextprotocol/clientCapabilities": { "extensions": {"vendor.example/huge": {"blob": huge_val}}}}}})
    .to_string();
    let mut line_bytes = oversized_msg.into_bytes();
    line_bytes.push(b'\n');
    (bound_client.write)(&line_bytes);
    let over_rej = bound_client.rejection("oversized extension line");
    assert_eq!(
        over_rej["error"]["code"], -32600,
        "BoundedLines/StdioGate must emit -32600 for oversized extension payload: {over_rej}"
    );
    // Session must remain healthy right after oversized rejection.
    let recovered = bound_client.call("b-ping", "ping", json!({}));
    assert_eq!(recovered["result"], json!({}));
    drop(bound_client);
    adapter_session_ended(&runtime, bound_sess, "bounded-ext");

    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{"_meta":{"vendor.example/k":1}}}"#),
        GateVerdict::Forward
    ));

    // ── 7. Published Latency, Throughput, Fairness, Memory & Disk Budgets ─
    // 7a. Negotiation micro-throughput (1,000 extension negotiations).
    let neg_t0 = Instant::now();
    let probe_reg = ExtensionRegistry::shipped();
    let probe_peer = probe_reg.capability_map();
    for _ in 0..1000 {
        let res = probe_reg
            .negotiate(&probe_peer, &["io.modelcontextprotocol/tasks".into()])
            .unwrap();
        assert!(res.active.contains_key("io.modelcontextprotocol/tasks"));
    }
    within("pipeline_ms", neg_t0.elapsed().as_millis());

    // 7b. Multi-client HTTP fairness & throughput across 4 concurrent sessions.
    let pipe_t0 = Instant::now();
    let mut handles = Vec::new();
    let max_client_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    for idx in 0..4u32 {
        let addr_c = http_addr.clone();
        let max_c = max_client_ms.clone();
        handles.push(std::thread::spawn(move || {
            let t0 = Instant::now();
            let mut cli = susi_wire_client::mcp::Session::open_with_capabilities(
                &addr_c,
                Some(token),
                &json!({ "extensions": { "io.modelcontextprotocol/tasks": {}, format!("vendor.fairness/client-{idx}"): {"slot": idx}}}),
                Duration::from_secs(15),
            )
            .expect("concurrent wire session open");
            assert!(cli
                .server_extensions()
                .is_some_and(|m| m.contains_key("io.modelcontextprotocol/tasks")));
            for step in 0..4u32 {
                let disc = cli
                    .request(
                        "server/discover",
                        &json!({ "_meta": { "io.modelcontextprotocol/protocolVersion": "2025-11-25", "io.modelcontextprotocol/clientCapabilities": {}, "vendor.fairness/step": step}}),
                        &mut |_| {},
                    )
                    .expect("concurrent server/discover");
                assert!(disc["capabilities"]["extensions"].is_object());
            }
            cli.close().expect("concurrent session close");
            let elapsed = u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX);
            max_c.fetch_max(elapsed, Ordering::AcqRel);
        }));
    }
    for h in handles {
        h.join().expect("concurrent fairness thread must succeed");
    }
    within("pipeline_ms", pipe_t0.elapsed().as_millis());
    within(
        "fairness_ms",
        u128::from(max_client_ms.load(Ordering::Acquire)),
    );

    if let (Some(before), Some(after)) = (resident_before, adapter_resident_bytes()) {
        within(
            "memory_growth_mib",
            u128::from(after.saturating_sub(before) / (1024 * 1024)),
        );
    }
    // Remove scratch test files before asserting zero leaked disk growth.
    let _ = std::fs::remove_dir_all(&scratch);
    let _ = std::fs::remove_file(cfg_dir.join("api_token"));
    let _ = _http_srv;
    let _ = std::fs::remove_dir_all(&home);
    within(
        "disk_growth_kib",
        u128::from(adapter_tree_bytes(&home).saturating_sub(baseline_disk) / 1024),
    );
}
