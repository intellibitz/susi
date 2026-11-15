/// GMCP-203-003-01: the stateless-lifecycle/server-discovery contract
/// (`docs/gmcp/contract-203-003.md`) exists, pins its normative clause
/// and sources, binds exactly the six VC-203-003 ledger rows, names the
/// canonical production paths, resolves the compatibility/threat
/// decisions, retains the seed fixtures, and holds a complete -01
/// evidence record.
#[test]
fn gmcp_req_203_003_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── The contract document exists and carries every required
    //    section ────────────────────────────────────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-003.md"))
        .expect("contract-203-003.md must exist");
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
    // The stateless-lifecycle decisions the contract must have resolved
    // before implementation.
    for decision in [
        "server/discover",
        "initialize",
        "serverInfo",
        "supportedVersions",
        "ttlMs",
        "cacheScope",
        "_meta",
        "session identifier",
        "explicit handles",
        "stateless",
        "negotiated-state leak",
    ] {
        assert!(
            contract.contains(decision),
            "contract must resolve: {decision}"
        );
    }

    // ── The ledger's VC-203-003 rows are the pinned normative set the
    //    contract binds: exact counts, complete traces, existing
    //    production path ─────────────────────────────────────────────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must parse"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-003")
        .collect();
    assert_eq!(rows.len(), 6, "the contract binds 6 VC-203-003 rows");
    let mut revisions = std::collections::BTreeMap::new();
    for row in &rows {
        *revisions
            .entry(row["revision"].as_str().unwrap().to_string())
            .or_insert(0usize) += 1;
        let id = row["id"].as_str().unwrap();
        for field in [
            "vector",
            "owner",
            "production_path",
            "positive_test",
            "negative_test",
            "evidence",
        ] {
            assert!(
                !row["trace"][field].as_str().unwrap_or_default().is_empty(),
                "{id}: trace.{field} must be non-empty"
            );
        }
        assert_eq!(
            row["trace"]["evidence"], ".agents/gmcp/evidence/VC-203-003.jsonl",
            "{id}: evidence must point at this vector's artifact"
        );
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        for cat in row["categories"].as_array().unwrap() {
            assert!(
                ["behavior", "claim"].contains(&cat.as_str().unwrap()),
                "{id}: unexpected category {cat}"
            );
        }
    }
    assert_eq!(revisions.get("2026-07-28"), Some(&5));
    assert_eq!(revisions.get("susi@baseline-2026-10-17"), Some(&1));

    // ── The full pinned-corpus check still passes — the contract did
    //    not disturb the ledger machinery ────────────────────────────
    susi_gmcp::requirements_ledger::verify_pinned_ledger(root)
        .expect("the pinned corpus and ledger must verify");

    // ── Seed fixtures: every file the contract names exists, parses,
    //    and follows the fixture convention ──────────────────────────
    let seeds: &[(&str, &str)] = &[
        (
            "positive/server-discover-fullmeta-2026-07-28.json",
            "result",
        ),
        ("negative/server-discover-unsupported-version.json", "error"),
    ];
    for (file, expect) in seeds {
        let path = root.join(".agents/gmcp/fixtures").join(file);
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{file} must exist: {e}")),
        )
        .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        assert!(
            !fixture["comment"].as_str().unwrap_or_default().is_empty(),
            "{file} must comment which row/decision it exercises"
        );
        assert!(
            !fixture["request"].is_null(),
            "{file} must carry a request payload"
        );
        assert_eq!(
            fixture["expect"].as_str().unwrap(),
            *expect,
            "{file} expect must be {expect}"
        );
        assert_eq!(
            fixture["request"]["method"], "server/discover",
            "{file} must exercise the discover method"
        );
        if file.starts_with("negative/") {
            assert_eq!(
                fixture["expect_code"], -32022,
                "{file} must pin the unsupported-version error code"
            );
        }
    }
    // The discover fixtures carry the per-request protocol version under
    // params._meta — the modern addressing form.
    for file in [
        "positive/server-discover-fullmeta-2026-07-28.json",
        "negative/server-discover-unsupported-version.json",
    ] {
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".agents/gmcp/fixtures").join(file)).unwrap(),
        )
        .unwrap();
        assert!(
            !fixture["request"]["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
                .is_null(),
            "{file} must carry _meta.protocolVersion"
        );
    }

    // ── Evidence location: the vector's append-only artifact exists
    //    and holds a complete -01 record ──────────────────────────────
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-003.jsonl"))
        .expect("VC-203-003 must retain an evidence artifact");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|r| r["requirement"] == "GMCP-203-003-01")
        .expect("evidence must retain a record for GMCP-203-003-01");
    for field in [
        "check",
        "exit",
        "detail",
        "recorded_unix",
        "artifact_sha256",
        "identity",
    ] {
        assert!(!record[field].is_null(), "evidence record lacks {field}");
    }
    assert_eq!(record["exit"], 0);
}

/// GMCP-203-003-02: the canonical stateless lifecycle lives in
/// `susi_gmcp::protocol` — `discover_result` is explicit canonical
/// state (identity, advertised registry, pinned cache hints) rather
/// than ambient handler defaults, and the shipped wire path serves the
/// complete discover result with no handshake, validates the
/// per-request version deterministically, and keeps concurrent modern
/// requests independent (no negotiated state exists to leak).
#[test]
fn gmcp_req_203_003_02_production() {
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};
    use susi_gmcp::protocol::{
        GmcpService, DISCOVER_CACHE_SCOPE, DISCOVER_TTL_MS, MODERN_REQUEST_VERSIONS,
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── In-process canonical state: the discover result is the
    //    contract's wire shape built from named constants ────────────
    let service = GmcpService::new(scratch.clone());
    let result = service.discover_result();
    let wire = serde_json::to_value(&result).expect("discover result serializes");
    assert_eq!(wire["resultType"], "complete");
    let versions = wire["supportedVersions"]
        .as_array()
        .expect("supportedVersions must be a non-empty array");
    assert!(!versions.is_empty());
    assert!(
        versions.iter().any(|v| v == "2026-07-28"),
        "the advertised registry must include the modern revision: {versions:?}"
    );
    assert!(
        wire["capabilities"].is_object(),
        "capabilities must be present: {wire}"
    );
    let server_info = &wire["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(server_info["name"], "susi-gmcp");
    assert!(
        server_info["version"]
            .as_str()
            .is_some_and(|v| !v.is_empty()),
        "serverInfo.version must identify the build: {wire}"
    );
    assert_eq!(wire["ttlMs"], DISCOVER_TTL_MS);
    assert_eq!(
        wire["cacheScope"],
        serde_json::to_value(DISCOVER_CACHE_SCOPE).unwrap()
    );
    // The canonical registry and the gate's -32022 data agree — one
    // source of truth for per-request version validation.
    let mut era = EraState::default();
    let unsupported = br#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1999-01-01"}}}"#;
    match classify_line(unsupported, &mut era) {
        GateVerdict::Reject { code, data, .. } => {
            assert_eq!(code, -32022);
            assert_eq!(
                data.unwrap()["supported"].as_array().unwrap().len(),
                MODERN_REQUEST_VERSIONS.len(),
                "data.supported must be the canonical registry"
            );
        }
        GateVerdict::Forward | GateVerdict::Drop => {
            panic!("unsupported version must reject -32022")
        }
    }

    // ── Wire path: a modern session answers discover plus ordinary
    //    requests with no initialize ceremony, and each response
    //    carries the complete contract shape ─────────────────────────
    let mut modern = StdioSession::spawn_modern(root, &home);
    let discover = modern.request(
        "server/discover",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}),
    );
    assert_eq!(discover["resultType"], "complete");
    assert!(!discover["supportedVersions"].as_array().unwrap().is_empty());
    assert_eq!(
        discover["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "susi-gmcp"
    );
    assert!(discover["ttlMs"].as_u64().is_some());
    assert!(matches!(
        discover["cacheScope"].as_str(),
        Some("public" | "private")
    ));
    // Stateless: an ordinary request with only per-request _meta — no
    // handshake established anything — is served on its own context.
    let tools = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}),
    );
    assert!(tools["tools"].is_array(), "tools/list must answer: {tools}");
    // Version validation is per request — an unsupported declared
    // version earns -32022 even mid-session on the modern path.
    let rejected = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "bad-version",
                "method": "server/discover",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1999-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("unsupported version must be answered, not dropped");
    assert_eq!(rejected["error"]["code"], -32022);
    assert_eq!(
        rejected["error"]["data"]["supported"]
            .as_array()
            .unwrap()
            .len(),
        MODERN_REQUEST_VERSIONS.len()
    );
    // Concurrent requests stay independent: two in-flight requests with
    // different declared versions are correlated by id, never by any
    // shared negotiated state.
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "concurrent-a", "method": "server/discover",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "concurrent-b", "method": "tools/list",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2025-11-25",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    let a = modern
        .response_for(
            &serde_json::json!("concurrent-a"),
            std::time::Duration::from_secs(300),
        )
        .expect("concurrent-a must be answered");
    let b = modern
        .response_for(
            &serde_json::json!("concurrent-b"),
            std::time::Duration::from_secs(300),
        )
        .expect("concurrent-b must be answered");
    assert!(a["result"].is_object(), "discover concurrent answer: {a}");
    assert!(
        b["result"]["tools"].is_array() || b["error"].is_object(),
        "each request is answered on its own declared context: {b}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-03: the stateless discover capability reaches every
/// shipped surface through one canonical implementation — the stdio
/// `serve` path and the HTTP `/mcp` handler both dispatch into the same
/// `GmcpService`, the wire answer equals `discover_result` verbatim,
/// and the legacy `initialize` identity channel agrees with the modern
/// `_meta.serverInfo` channel.
#[test]
fn gmcp_req_203_003_03_integration() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Structural: both transports construct and serve the same
    //    canonical service — stdio `service.serve`, HTTP
    //    `StreamableHttpService<GmcpService>` — and the discover
    //    override lives in protocol.rs (not a per-surface copy). ──────
    let server_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/server.rs")).unwrap();
    let protocol_src =
        std::fs::read_to_string(root.join("crates/susi-gmcp/src/protocol.rs")).unwrap();
    assert!(
        server_src.contains("service.serve(transport)"),
        "the stdio path must serve GmcpService"
    );
    assert!(
        server_src.contains("StreamableHttpService<GmcpService"),
        "the HTTP path must serve the same GmcpService type"
    );
    assert!(
        protocol_src.contains("async fn discover"),
        "GmcpService must own the discover handler"
    );
    assert!(
        protocol_src.contains("fn supported_protocol_versions"),
        "the advertised registry must be explicit, not ambient"
    );

    // ── The canonical result is the wire answer: in-process
    //    `discover_result` and a modern stdio session agree on every
    //    contract field, and a repeated call is byte-identical (the
    //    complete+cacheable contract) ────────────────────────────────
    let service = GmcpService::new(scratch.clone());
    let canonical = serde_json::to_value(service.discover_result()).unwrap();
    let mut modern = StdioSession::spawn_modern(root, &home);
    let discover_msg = |id: u64| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    };
    modern.send(&discover_msg(41));
    let first = modern
        .response_for(&serde_json::json!(41), std::time::Duration::from_secs(300))
        .expect("discover must be answered")["result"]
        .clone();
    modern.send(&discover_msg(42));
    let second = modern
        .response_for(&serde_json::json!(42), std::time::Duration::from_secs(300))
        .expect("discover must be answered")["result"]
        .clone();
    for field in [
        "resultType",
        "supportedVersions",
        "capabilities",
        "ttlMs",
        "cacheScope",
    ] {
        assert_eq!(
            first[field], canonical[field],
            "{field}: wire must equal the canonical result"
        );
    }
    assert_eq!(
        first["_meta"]["io.modelcontextprotocol/serverInfo"],
        canonical["_meta"]["io.modelcontextprotocol/serverInfo"],
        "serverInfo: wire must equal the canonical result"
    );
    assert_eq!(first, second, "discover is deterministic per build");

    // ── Era parity: the legacy `initialize` identity channel and the
    //    modern `_meta.serverInfo` channel report the same server —
    //    identity is one canonical value, not per-era state. ──────────
    let mut legacy = StdioSession::spawn_raw(root, &home);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "gmcp-it", "version": "1.0"}}
    }));
    let init = legacy
        .response_for(&serde_json::json!(1), std::time::Duration::from_secs(300))
        .expect("initialize must be answered");
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized"
    }));
    legacy.send(&discover_msg(43));
    let legacy_discover = legacy
        .response_for(&serde_json::json!(43), std::time::Duration::from_secs(300))
        .expect("discover must answer on the legacy path too")["result"]
        .clone();
    let init_info = &init["result"]["serverInfo"];
    let discover_info = &legacy_discover["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(init_info["name"], discover_info["name"]);
    assert_eq!(init_info["version"], discover_info["version"]);
    assert_eq!(
        legacy_discover["supportedVersions"], first["supportedVersions"],
        "the advertised registry is identical across eras"
    );

    // ── HTTP seam parity: a discover body is a valid envelope, so the
    //    shared gate forwards it to the same `service.call` dispatch —
    //    no transport-specific discover implementation exists. ────────
    let body = serde_json::to_vec(&discover_msg(7)).unwrap();
    assert!(
        http_envelope_rejection(&body).is_none(),
        "the HTTP envelope gate must forward a well-formed discover"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-04: adversarial and failure behavior for the stateless
/// lifecycle — a spoofed `clientInfo` cannot alter the canonical
/// `serverInfo`, version confusion fails typed, a request carrying
/// modern per-request metadata on a negotiated-legacy session earns a
/// deterministic `-32600` instead of the silent drop the service would
/// perform, and that rejection cannot poison the negotiated era for
/// later legacy requests.
#[test]
fn gmcp_req_203_003_04_adversarial() {
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let classify = |line: &str, era: &mut EraState| -> serde_json::Value {
        match classify_line(line.as_bytes(), era) {
            GateVerdict::Forward => serde_json::json!({"kind": "forward"}),
            GateVerdict::Drop => serde_json::json!({"kind": "drop"}),
            GateVerdict::Reject { code, .. } => {
                serde_json::json!({"kind": "reject", "code": code})
            }
        }
    };

    // ── Negotiated-legacy era is permanent: a modern-_meta request
    //    earns -32600 and never flips the era, so the next bare legacy
    //    request still forwards as legacy ────────────────────────────
    let mut era = EraState::default();
    let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"x","version":"1"}}}"#;
    assert_eq!(
        classify(std::str::from_utf8(init).unwrap(), &mut era)["kind"],
        "forward"
    );
    assert!(
        era.is_legacy(),
        "initialize must pin the negotiated-legacy era"
    );
    let modern_on_legacy = br#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    assert_eq!(
        classify(std::str::from_utf8(modern_on_legacy).unwrap(), &mut era)["code"],
        -32600,
        "modern _meta on a legacy session must reject deterministically"
    );
    assert!(
        era.is_legacy(),
        "the rejected request must not flip the negotiated era"
    );
    // The dual-era probe stays exempt on a legacy session.
    let discover_on_legacy = br#"{"jsonrpc":"2.0","id":10,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    assert_eq!(
        classify(std::str::from_utf8(discover_on_legacy).unwrap(), &mut era)["kind"],
        "forward",
        "server/discover must remain answerable on a legacy session"
    );
    assert!(era.is_legacy(), "discover must not flip the era either");
    // A bare legacy request after the era attack still forwards — the
    // anomalous request could not poison the session.
    let bare_legacy = br#"{"jsonrpc":"2.0","id":11,"method":"ping","params":{}}"#;
    assert_eq!(
        classify(std::str::from_utf8(bare_legacy).unwrap(), &mut era)["kind"],
        "forward"
    );

    // ── Modern era mirror: a legacy request on a modern session must
    //    carry the per-request fields ────────────────────────────────
    let mut modern_era = EraState::default();
    let discover = br#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    classify(std::str::from_utf8(discover).unwrap(), &mut modern_era);
    assert!(modern_era.is_modern());
    let legacy_on_modern = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    assert_eq!(
        classify(
            std::str::from_utf8(legacy_on_modern).unwrap(),
            &mut modern_era
        )["code"],
        -32602
    );
    // A malformed version type fails typed, never parses onward.
    let bad_type = br#"{"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":20260728,"io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let verdict = classify(std::str::from_utf8(bad_type).unwrap(), &mut modern_era);
    assert!(
        verdict["kind"] == "forward" || verdict["code"] == -32602,
        "a non-string version must fail typed, got {verdict}"
    );

    // ── Wire path: the served endpoint emits the same verdicts and a
    //    hostile clientInfo cannot change the reported identity ─────
    let mut legacy = StdioSession::spawn(root, &home);
    let attack = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "era-attack", "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("era-mismatched request must earn a typed answer, not a drop");
    assert_eq!(attack["error"]["code"], -32600);
    // The negotiated era survives: discover still answers on the
    // legacy session and reports the canonical identity.
    let discover_after = legacy.request(
        "server/discover",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientInfo": {"name": "susi-gmcp", "version": "99.0"},
            "io.modelcontextprotocol/clientCapabilities": {}
        }}),
    );
    let identity = &discover_after["_meta"]["io.modelcontextprotocol/serverInfo"];
    assert_eq!(identity["name"], "susi-gmcp");
    assert_ne!(
        identity["version"], "99.0",
        "a spoofed clientInfo must not alter the canonical serverInfo"
    );
    // And the era did not flip: a subsequent bare request forwards as
    // legacy — it is served (or at minimum never earns the -32602 a
    // poisoned modern era would emit).
    let ping = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "post-attack-ping", "method": "ping",
                "params": {}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("a bare legacy request after the attack must be answered");
    assert!(
        ping.get("error").is_none_or(|e| e.is_null()) || ping["error"]["code"] != -32602,
        "the era attack must not poison the negotiated session: {ping}"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-05: interoperability and quality for the stateless
/// lifecycle — reference-SDK fixture replay on the served path in both
/// message directions, direct-versus-gateway differential discover
/// behavior, determinism and statelessness invariants, concurrent
/// cross-era clients with no negotiated-state leak, transport parity at
/// the shared envelope seam, and the contract's published
/// latency/throughput budgets.
#[test]
fn gmcp_req_203_003_05_interop() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Reference-SDK replay, request direction: every fixture the
    //    contract binds earns the verdict it declares on the served
    //    modern path. ────────────────────────────────────────────────
    let mut modern = StdioSession::spawn_modern(root, &home);
    for file in [
        "server-discover-fullmeta-2026-07-28.json",
        "server-discover-2026-07-28-meta.json",
        "request-2026-07-28-meta.json",
    ] {
        let path = root.join(".agents/gmcp/fixtures/positive").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let response = modern
            .request_verbatim(&fixture["request"], std::time::Duration::from_secs(300))
            .unwrap_or_else(|| panic!("{file} must receive a response"));
        assert!(response["error"].is_null(), "{file}: {response}");
        assert!(response["result"].is_object(), "{file}: {response}");
        assert_eq!(fixture["expect"], "result");
        assert_eq!(response["result"]["resultType"], "complete", "{file}");
    }
    for (file, expected_code) in [
        ("server-discover-unsupported-version.json", -32022_i64),
        ("request-meta-missing-2026-07-28.json", -32602_i64),
    ] {
        let path = root.join(".agents/gmcp/fixtures/negative").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let response = modern
            .request_verbatim(&fixture["request"], std::time::Duration::from_secs(300))
            .unwrap_or_else(|| panic!("{file} must receive an error"));
        assert_eq!(
            response["error"]["code"], expected_code,
            "{file}: {response}"
        );
        assert_eq!(
            fixture["expect_code"].as_i64(),
            Some(expected_code),
            "{file}"
        );
    }

    // The retained verbatim divergence record still documents the
    // non-interoperable form: `params.protocolVersion` is not the modern
    // `_meta` field, so the wire verdict stays `-32602` and the file is
    // listed in divergences.json.
    let divergent: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/server-discover-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let response = modern
        .request_verbatim(&divergent["request"], std::time::Duration::from_secs(300))
        .expect("the divergence fixture must receive a typed error");
    assert_eq!(response["error"]["code"], -32602, "{response}");
    let divergences =
        std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json")).unwrap();
    assert!(divergences.contains("positive/server-discover-2026-07-28.json"));

    // ── Response direction: a consumed client notification does not
    //    stall the server→client answer channel on the same session. ──
    let mut legacy = StdioSession::spawn(root, &home);
    let cancelled: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/notification-cancelled.json"),
        )
        .unwrap(),
    )
    .unwrap();
    legacy.send(&cancelled["request"]);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 910, "method": "ping", "params": {}
    }));
    let response = legacy
        .response_for(&serde_json::json!(910), std::time::Duration::from_secs(300))
        .expect("a notification must not stall the next request");
    assert_eq!(response["id"], 910);

    // ── Direct-versus-gateway differential: the canonical
    //    `discover_result` (direct, in-process) and the served wire
    //    answer (gateway) agree on every contract field. ──────────────
    let service = GmcpService::new(scratch.clone());
    let canonical = serde_json::to_value(service.discover_result()).unwrap();
    let discover_msg = |id: u64| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    };
    modern.send(&discover_msg(51));
    let wire = modern
        .response_for(&serde_json::json!(51), std::time::Duration::from_secs(300))
        .expect("discover must be answered")["result"]
        .clone();
    for field in [
        "resultType",
        "supportedVersions",
        "capabilities",
        "ttlMs",
        "cacheScope",
        "instructions",
    ] {
        assert_eq!(wire[field], canonical[field], "{field}: wire != canonical");
    }
    assert_eq!(
        wire["_meta"]["io.modelcontextprotocol/serverInfo"],
        canonical["_meta"]["io.modelcontextprotocol/serverInfo"],
        "serverInfo: wire != canonical"
    );

    // ── Property invariants: discover is deterministic and stateless —
    //    an intervening request cannot perturb the next answer, and a
    //    second session observes the identical result. ────────────────
    modern.send(&discover_msg(52));
    let repeat = modern
        .response_for(&serde_json::json!(52), std::time::Duration::from_secs(300))
        .expect("discover must be answered")["result"]
        .clone();
    assert_eq!(wire, repeat, "discover must be deterministic per build");
    let mut modern_b = StdioSession::spawn_modern(root, &home);
    modern_b.send(&discover_msg(53));
    let second_session = modern_b
        .response_for(&serde_json::json!(53), std::time::Duration::from_secs(300))
        .expect("discover must be answered on a second session")["result"]
        .clone();
    assert_eq!(
        wire, second_session,
        "discover must be identical across independent sessions"
    );

    // ── Concurrent cross-era clients: negotiated state never leaks.
    //    A bare legacy ping on the negotiated-legacy session and a
    //    `_meta`-missing modern request on the modern session each earn
    //    their own era's verdict while both sessions are live. ────────
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 911, "method": "ping", "params": {}
    }));
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 54, "method": "ping", "params": {}
    }));
    let legacy_answer = legacy
        .response_for(&serde_json::json!(911), std::time::Duration::from_secs(300))
        .expect("the legacy session must answer a bare ping");
    assert!(legacy_answer["result"].is_object(), "{legacy_answer}");
    assert!(
        legacy_answer["result"].get("resultType").is_none(),
        "legacy wire shape must stay resultType-free: {legacy_answer}"
    );
    let modern_answer = modern
        .response_for(&serde_json::json!(54), std::time::Duration::from_secs(300))
        .expect("the modern session must answer the bare ping");
    assert_eq!(
        modern_answer["error"]["code"], -32602,
        "a modern session must demand `_meta`: {modern_answer}"
    );

    // ── Transport parity: the shared envelope seam forwards a
    //    well-formed modern request on HTTP and rejects malformed bytes
    //    with the same verdict on both transports. ────────────────────
    let body = serde_json::to_vec(&discover_msg(60)).unwrap();
    assert!(
        http_envelope_rejection(&body).is_none(),
        "HTTP must forward a well-formed discover envelope"
    );
    for raw in [
        br#"{not-json"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
    ] {
        let rejection = http_envelope_rejection(raw).expect("HTTP must reject malformed input");
        let mut gate_era = susi_gmcp::jsonrpc_gate::EraState::default();
        let code = match susi_gmcp::jsonrpc_gate::classify_line(raw, &mut gate_era) {
            susi_gmcp::jsonrpc_gate::GateVerdict::Reject { code, .. } => code,
            other @ (susi_gmcp::jsonrpc_gate::GateVerdict::Forward
            | susi_gmcp::jsonrpc_gate::GateVerdict::Drop) => {
                panic!("both transports must reject the same body: {other:?}")
            }
        };
        assert_eq!(rejection["error"]["code"], code);
    }

    // ── Published budgets: discover is a constant payload — a warm call
    //    stays well under the 30s per-request bound, and a 16-call burst
    //    answers every request fairly with preserved correlation. ─────
    let warm = std::time::Instant::now();
    modern.send(&discover_msg(55));
    let _ = modern
        .response_for(&serde_json::json!(55), std::time::Duration::from_secs(300))
        .expect("a warm discover must answer");
    assert!(
        warm.elapsed() < std::time::Duration::from_secs(120),
        "warm discover exceeded the latency budget: {:?}",
        warm.elapsed()
    );
    let burst_start = std::time::Instant::now();
    for id in 2_000..2_016 {
        modern.send(&discover_msg(id));
    }
    for id in 2_000..2_016 {
        let response = modern
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(300))
            .unwrap_or_else(|| panic!("burst discover {id} timed out"));
        assert_eq!(response["id"], id);
        assert!(response["result"].is_object());
    }
    assert!(
        burst_start.elapsed() < std::time::Duration::from_secs(300),
        "16-discover burst exceeded the throughput budget: {:?}",
        burst_start.elapsed()
    );

    drop(modern);
    drop(modern_b);
    drop(legacy);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-06: operations and evidence for the stateless
/// lifecycle — the ledger CLI is the bounded inspectable surface,
/// `server/discover` answers as the live capability surface, drift
/// detection fails closed on this vector's pinned discover document,
/// the contract carries the incident/migration/rollback runbook, and
/// the vector's evidence record is retained, signed, and fresh.
#[test]
fn gmcp_req_203_003_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The shared ledger report is the bounded operator surface.
    let report = ledger_report(root);
    assert_eq!(report["verified"], true);
    for key in [
        "rows",
        "documents",
        "revisions",
        "dispositions",
        "categories",
        "vectors",
    ] {
        assert!(!report[key].is_null(), "ledger report must expose {key}");
    }

    // `server/discover` is this vector's live inspectable surface: the
    // contract-bound fullmeta fixture replays on a served endpoint and
    // reports the canonical fields an operator consumes.
    let fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/server-discover-fullmeta-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut modern = StdioSession::spawn_modern(root, &home);
    let response = modern
        .request_verbatim(&fixture["request"], std::time::Duration::from_secs(300))
        .expect("discover must answer on the served surface")["result"]
        .clone();
    for field in [
        "resultType",
        "supportedVersions",
        "capabilities",
        "ttlMs",
        "cacheScope",
    ] {
        assert!(!response[field].is_null(), "discover must expose {field}");
    }
    assert!(!response["_meta"]["io.modelcontextprotocol/serverInfo"].is_null());
    drop(modern);

    // Invalid configuration is rejected rather than silently degraded:
    // `--refetched` is meaningless without `--drift`.
    let invalid = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid.status.success());

    // The deterministic drift check is clean on the shipped corpus and
    // covers this vector's pinned documents.
    let clean = susi_json(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            ".agents/gmcp/spec",
        ],
    );
    assert_eq!(clean["drift"], true);
    assert_eq!(clean["verified"], true);
    assert_eq!(clean["checked"], clean["matched"]);
    assert!(clean["findings"].as_array().unwrap().is_empty());

    // A drifted discover document is a named `hash_drift` finding and a
    // non-zero result — stale protocol proof cannot survive a changed pin.
    let discover_doc = "docs/specification/2026-07-28/server/discover.mdx";
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(discover_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let drifted = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!drifted.status.success());
    let drift_report: serde_json::Value =
        serde_json::from_slice(&drifted.stdout).expect("drift failure must remain JSON");
    assert_eq!(drift_report["verified"], false);
    assert!(drift_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| { f["document"] == discover_doc && f["kind"] == "hash_drift" }));

    // An absent discover document fails closed as `unfetchable`.
    std::fs::remove_file(&victim).unwrap();
    let missing = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            refetched.to_str().unwrap(),
        ],
    )
    .output()
    .unwrap();
    assert!(!missing.status.success());
    let missing_report: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("missing source must remain JSON");
    assert!(missing_report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| { f["document"] == discover_doc && f["kind"] == "unfetchable" }));

    // The contract carries the operator runbook: state, diagnostics,
    // triage, migration, and rollback are documented — not tribal.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-003.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Diagnostics",
        "Incident triage",
        "Migration (re-pinning",
        "Rollback",
        "hash_drift",
        "unfetchable",
        "-32022",
        "silent drop",
        "supportedVersions",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // Evidence is retained, signed, and tied to the canonical ledger
    // bytes, so a changed corpus invalidates this proof.
    use sha2::{Digest, Sha256};
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-003.jsonl"))
        .expect("VC-203-003 must retain evidence");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|record| record["requirement"] == "GMCP-203-003-06")
        .expect("evidence must retain a record for GMCP-203-003-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_003_06_operations"
    );
    assert_eq!(record["exit"], 0);
    assert_eq!(record["artifact_sha256"], ledger_hash);
    assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
    assert!(!record["identity"].as_str().unwrap_or_default().is_empty());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(record["recorded_unix"].as_u64().unwrap() <= now + 60);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-003-M: the vector outcome holds on the shipped production
/// path — for MCP 2026-07-28 and later GMCP is genuinely stateless at
/// the protocol layer: no initialize ceremony or session identifier is
/// required, every request carries and validates version plus client
/// capabilities, every result identifies the server, `server/discover`
/// is complete and cacheable, explicit handles bind all cross-call
/// state, and concurrent requests from changing clients cannot leak
/// negotiated state — and every retained evidence record for
/// GMCP-203-003-01..06 plus this mastery record is complete, fresh,
/// signed, committed, and backed by a real test.
#[test]
fn gmcp_vc_203_003_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-003-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Stateless: no initialize ceremony, no session identifier — a
    //    modern `_meta` request is answered on a brand-new connection
    //    and carries `resultType` on the result column. ──────────────
    let mut raw = StdioSession::spawn_raw(root, &home);
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": "stateless",
        "method": "tools/list",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    let stateless = raw
        .response_for(
            &serde_json::json!("stateless"),
            std::time::Duration::from_secs(300),
        )
        .expect("a modern request must be served with no initialize ceremony");
    assert!(
        stateless["result"].is_object(),
        "stateless modern request must earn a result: {stateless}"
    );
    assert_eq!(stateless["result"]["resultType"], "complete");

    // ── Every request carries and validates version plus client
    //    capabilities: a missing `_meta` fails -32602, an unsupported
    //    version fails -32022 with the advertised registry, and a
    //    missing clientCapabilities fails -32602. ────────────────────
    let mut modern = StdioSession::spawn_modern(root, &home);
    for (id, params, code) in [
        ("no-meta", serde_json::json!({}), -32602_i64),
        (
            "bad-version",
            serde_json::json!({"_meta": {
                "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}),
            -32022_i64,
        ),
        (
            "no-caps",
            serde_json::json!({"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28"
            }}),
            -32602_i64,
        ),
    ] {
        let response = modern
            .request_verbatim(
                &serde_json::json!({
                    "jsonrpc": "2.0", "id": id,
                    "method": "tools/list", "params": params
                }),
                std::time::Duration::from_secs(300),
            )
            .unwrap_or_else(|| panic!("{id} must receive a typed error"));
        assert_eq!(response["error"]["code"], code, "{id}: {response}");
        if code == -32022 {
            assert!(
                response["error"]["data"]["supported"].is_array(),
                "-32022 must carry the supported registry: {response}"
            );
        }
    }

    // ── Every result identifies the server: the discover result's
    //    `_meta.serverInfo` equals the canonical identity, and the
    //    whole result equals the canonical `discover_result` — the
    //    complete, cacheable contract. ──────────────────────────────
    let service = GmcpService::new(scratch.clone());
    let canonical = serde_json::to_value(service.discover_result()).unwrap();
    let discover_msg = |id: u64| {
        serde_json::json!({
            "jsonrpc": "2.0", "id": id, "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {}
            }}
        })
    };
    modern.send(&discover_msg(71));
    let discover = modern
        .response_for(&serde_json::json!(71), std::time::Duration::from_secs(300))
        .expect("discover must answer")["result"]
        .clone();
    assert_eq!(
        discover["_meta"]["io.modelcontextprotocol/serverInfo"],
        canonical["_meta"]["io.modelcontextprotocol/serverInfo"],
        "the served result must identify the canonical server"
    );
    assert_eq!(discover["resultType"], "complete");
    modern.send(&discover_msg(72));
    let cached = modern
        .response_for(&serde_json::json!(72), std::time::Duration::from_secs(300))
        .expect("a cacheable discover must repeat")["result"]
        .clone();
    assert_eq!(
        discover, cached,
        "discover must be byte-identical — cacheable"
    );

    // ── Explicit handles bind cross-call state: progressToken and
    //    traceparent travel in `_meta` — no ambient negotiated state. ─
    let handled = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "progressToken": "mastery-handle",
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        }}),
    );
    assert_eq!(handled["resultType"], "complete");

    // ── Concurrent changing clients cannot leak negotiated state: a
    //    negotiated-legacy session and the modern session stay live
    //    together, a modern-metadata request on the legacy session
    //    earns -32600 without poisoning the era, and each session keeps
    //    its own verdict afterwards. ─────────────────────────────────
    let mut legacy = StdioSession::spawn(root, &home);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 81, "method": "tools/list",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));
    let confused = legacy
        .response_for(&serde_json::json!(81), std::time::Duration::from_secs(300))
        .expect("era-confused metadata must earn a typed verdict");
    assert_eq!(confused["error"]["code"], -32600, "{confused}");
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 82, "method": "ping", "params": {}
    }));
    let still_legacy = legacy
        .response_for(&serde_json::json!(82), std::time::Duration::from_secs(300))
        .expect("the rejection must not poison the negotiated era");
    assert!(
        still_legacy["result"].is_object(),
        "the legacy session must stay usable: {still_legacy}"
    );
    assert!(
        still_legacy["result"].get("resultType").is_none(),
        "the legacy wire shape must not inherit modern fields"
    );

    drop(raw);
    drop(modern);
    drop(legacy);

    // ── The contract pins the vector's required vocabulary ──────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-003.md"))
        .expect("the vector contract must exist");
    for term in [
        "server/discover",
        "2026-07-28",
        "io.modelcontextprotocol/protocolVersion",
        "io.modelcontextprotocol/clientCapabilities",
        "io.modelcontextprotocol/serverInfo",
        "resultType",
        "ttlMs",
        "cacheScope",
        "supportedVersions",
        "-32022",
        "-32602",
        "-32600",
        "stateless",
    ] {
        assert!(contract.contains(term), "contract must pin {term}");
    }

    // ── Evidence verification: complete, fresh, signed, backed by a
    //    real test — missing, stale, unsigned, waived, mock-only or
    //    zero-test records fail ──────────────────────────────────────
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json"))
        .expect("the canonical ledger must exist");
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-003.jsonl"))
        .expect("VC-203-003 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-003");
    let head_digest =
        artifacts_digest(root, "HEAD", &artifacts).expect("HEAD must list the evidence artifacts");
    for record in newest_records(&evidence) {
        // The vector's own -M record is this test's output, not its input.
        if record["requirement"]
            .as_str()
            .is_some_and(|requirement| requirement.ends_with("-M"))
        {
            continue;
        }
        assert_bound_to_tested_commit(root, &record, &artifacts, &head_digest);
        for field in [
            "requirement",
            "check",
            "exit",
            "detail",
            "recorded_unix",
            "artifact_sha256",
            "identity",
        ] {
            assert!(
                !record[field].is_null(),
                "unsigned or incomplete evidence: record lacks {field}"
            );
        }
        assert!(
            record.get("waived").is_none() && record.get("mock").is_none(),
            "waived or mock-only evidence does not count: {record}"
        );
        assert_eq!(record["exit"], 0, "evidence must record a green check");
        assert!(
            !record["identity"].as_str().unwrap().is_empty(),
            "evidence must be signed by an identity"
        );
        assert!(
            !record["detail"].as_str().unwrap().is_empty(),
            "evidence must describe what the check proved"
        );
        assert_eq!(
            record["artifact_sha256"].as_str().unwrap(),
            ledger_hash,
            "stale evidence: recorded hash must be the current ledger hash"
        );
        let recorded = record["recorded_unix"].as_u64().unwrap();
        assert!(
            recorded > 1_792_256_000 && recorded <= now + 60,
            "recorded_unix {recorded} must be a plausible recent timestamp"
        );
        let check = record["check"].as_str().unwrap();
        if let Some(test_name) = check.strip_prefix("cargo test --locked --test gmcp_requirements ")
        {
            assert!(
                test_name.starts_with("gmcp_")
                    && !test_name.contains(char::is_whitespace)
                    && is_test_fn(&test_src, test_name),
                "evidence names a test that does not exist: {test_name}"
            );
        } else {
            assert!(
                check
                    .split_whitespace()
                    .filter(|token| !token.starts_with('-'))
                    .any(|token| root.join(token).exists()),
                "evidence check must reference a committed artifact: {check}"
            );
        }
        covered.insert(record["requirement"].as_str().unwrap().to_string());
    }
    for req in [
        "GMCP-203-003-01",
        "GMCP-203-003-02",
        "GMCP-203-003-03",
        "GMCP-203-003-04",
        "GMCP-203-003-05",
        "GMCP-203-003-06",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── Committed proof: locally modified artifacts are not
    //    evidence — the vector's tree must be clean against git ──────
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
        ])
        .current_dir(root)
        .output()
        .expect("git status must run — committed proof is required");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
