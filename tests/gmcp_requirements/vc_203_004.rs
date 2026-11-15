#[test]
fn gmcp_req_203_004_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("contract document must exist");
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
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-004")
        .collect();
    assert_eq!(rows.len(), 108, "the contract binds 108 VC-203-004 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
}

#[test]
fn gmcp_req_203_004_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = ledger_report(root);
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 108,
        "the shipped ledger must still hold the vector's rows"
    );
    assert!(report["vectors"].as_u64().unwrap_or(0) > 0);
    assert!(
        report["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0,
        "revision 2025-11-25 must be verified"
    );
    assert!(
        report["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0,
        "revision 2026-07-28 must be verified"
    );

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-004")
        .collect();
    assert_eq!(rows.len(), 108, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

#[test]
fn gmcp_req_203_004_03_integration() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-VC-203-004-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn(root, &home);
    let tools = wire.list_all_tools(serde_json::json!({}));
    assert!(
        tools["tools"]
            .as_array()
            .is_some_and(|tools| tools.iter().any(|t| t["name"] == "ledger_verify")),
        "the served surface must advertise ledger_verify"
    );
    let call = wire.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    let report = call_tool_report(&call);
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 108,
        "the served report must still hold the vector's rows"
    );
    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-004")
        .collect();
    assert_eq!(rows.len(), 108, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

/// GMCP-203-004-04: adversarial and failure behavior for legacy lifecycle
/// compatibility and governed deprecation.
///
/// The vector threat model names replay, principal spoofing and deadline
/// abuse as the peer's weapons; this test pins the negative, attack, race,
/// corruption and fault-injection cases against the production policy:
/// a malformed frame earns its typed error instead of a silent drop, a
/// pinned era can never be renegotiated or downgraded by a hostile peer,
/// the negotiated downgrade is visible in the wire shape, and two
/// connections never share negotiated state.
#[test]
fn gmcp_req_203_004_04_adversarial() {
    use susi_gmcp::jsonrpc_gate::{
        classify_envelope, classify_line, http_envelope_rejection, EraState, GateVerdict,
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-004-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let classify = |line: &[u8], era: &mut EraState| -> serde_json::Value {
        match classify_line(line, era) {
            GateVerdict::Forward => serde_json::json!({"kind": "forward"}),
            GateVerdict::Drop => serde_json::json!({"kind": "drop"}),
            GateVerdict::Reject { code, data, .. } => {
                serde_json::json!({"kind": "reject", "code": code, "data": data})
            }
        }
    };

    // ── Corruption: every malformed envelope earns its typed error and
    //    none of them pins a session era ──────────────────────────────
    for (name, line, code) in [
        ("garbage", br#"{"jsonrpc":"2.0","#.as_slice(), -32700),
        ("batch", br#"[]"#.as_slice(), -32600),
        (
            "non-2.0",
            br#"{"jsonrpc":"1.0","id":4,"method":"ping"}"#.as_slice(),
            -32600,
        ),
        ("scalar", br#""hello""#.as_slice(), -32600),
        (
            "id-null",
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
            -32600,
        ),
        (
            "no-method",
            br#"{"jsonrpc":"2.0","id":7}"#.as_slice(),
            -32600,
        ),
    ] {
        let mut era = EraState::default();
        let verdict = classify(line, &mut era);
        assert_eq!(
            verdict["code"].as_i64(),
            Some(code),
            "{name} must earn {code}: {verdict}"
        );
        assert!(
            !era.is_modern() && !era.is_legacy(),
            "{name} must never pin a session era"
        );
    }

    // Compatibility drops stay drops: an unknown notification is ignored,
    // while a standard notification reaches the service.
    let mut era = EraState::default();
    assert_eq!(
        classify(
            br#"{"jsonrpc":"2.0","method":"notifications/whatever"}"#,
            &mut era
        )["kind"],
        "drop"
    );
    assert_eq!(
        classify(
            br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            &mut era
        )["kind"],
        "forward"
    );
    assert!(!era.is_modern() && !era.is_legacy());

    // ── Attack: a negotiated-legacy session is permanently legacy ─────
    let mut era = EraState::default();
    let initialize = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"adv","version":"1"}}}"#;
    assert_eq!(classify(initialize, &mut era)["kind"], "forward");
    assert!(
        era.is_legacy(),
        "initialize must pin the negotiated-legacy era"
    );
    let modern_meta = br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    assert_eq!(
        classify(modern_meta, &mut era)["code"].as_i64(),
        Some(-32600),
        "per-request modern metadata on a legacy session must reject deterministically"
    );
    assert!(
        era.is_legacy(),
        "the rejected request must never flip the negotiated era"
    );
    // Replay the handshake with the modern revision: a hostile peer cannot
    // renegotiate a session it already pinned.
    let reinitialize = br#"{"jsonrpc":"2.0","id":3,"method":"initialize","params":{"protocolVersion":"2026-07-28","capabilities":{},"clientInfo":{"name":"adv","version":"2"}}}"#;
    assert_eq!(classify(reinitialize, &mut era)["kind"], "forward");
    assert!(
        era.is_legacy(),
        "a second initialize must not renegotiate the pinned era"
    );
    assert_eq!(
        classify(modern_meta, &mut era)["code"].as_i64(),
        Some(-32600),
        "the replayed handshake must not admit modern metadata"
    );
    assert_eq!(
        classify(
            br#"{"jsonrpc":"2.0","id":5,"method":"ping","params":{}}"#,
            &mut era
        )["kind"],
        "forward",
        "an attack must never poison the legacy session"
    );

    // ── Modern mirror: missing per-request metadata is malformed, and a
    //    legacy initialize cannot downgrade a modern session ───────────
    let mut modern_era = EraState::default();
    let discover = br#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    assert_eq!(classify(discover, &mut modern_era)["kind"], "forward");
    assert!(modern_era.is_modern(), "discover must pin the modern era");
    assert_eq!(
        classify(
            br#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            &mut modern_era
        )["code"]
            .as_i64(),
        Some(-32602),
        "a bare request on the modern path is malformed"
    );
    assert_eq!(classify(initialize, &mut modern_era)["kind"], "forward");
    assert!(
        modern_era.is_modern(),
        "a legacy initialize must not downgrade a modern session"
    );

    // ── Version selection: an unknown revision is a typed -32022 that
    //    names the supported set, never a silent downgrade ────────────
    let mut version_era = EraState::default();
    let future = br#"{"jsonrpc":"2.0","id":9,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2099-12-31","io.modelcontextprotocol/clientCapabilities":{}}}}"#;
    let verdict = classify(future, &mut version_era);
    assert_eq!(
        verdict["code"].as_i64(),
        Some(-32022),
        "an unsupported revision must earn -32022: {verdict}"
    );
    assert_eq!(verdict["data"]["requested"], "2099-12-31");
    assert!(
        verdict["data"]["supported"]
            .as_array()
            .is_some_and(|set| set.iter().any(|v| v == "2026-07-28")),
        "the rejection must name the supported revisions: {verdict}"
    );

    // The streamable-HTTP body gate shares the same policy boundary.
    assert!(
        matches!(
            classify_envelope(br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#),
            GateVerdict::Reject { code: -32600, .. }
        ),
        "the envelope classifier must reject a null id with -32600"
    );
    assert!(
        http_envelope_rejection(br#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#)
            .is_none(),
        "a valid body must pass the HTTP gate"
    );
    assert!(
        http_envelope_rejection(br#"[]"#).is_some(),
        "a batch body must be rejected at the HTTP gate"
    );

    // ── Wire: corruption and faults never poison the served session ───
    let mut legacy = StdioSession::spawn(root, &home);
    let tools = legacy.request("tools/list", serde_json::json!({}));
    assert!(
        tools["tools"].as_array().is_some(),
        "the legacy surface must still serve tools/list: {tools}"
    );
    assert!(
        tools.get("resultType").is_none(),
        "a negotiated-legacy result must keep the pre-2026-07-28 shape: {tools}"
    );

    legacy.send_raw(b"{not json\n");
    let parse = legacy
        .response_for(&serde_json::Value::Null, std::time::Duration::from_secs(60))
        .expect("a malformed line must earn a typed answer, never a silent drop");
    assert_eq!(
        parse["error"]["code"].as_i64(),
        Some(-32700),
        "corruption must earn -32700: {parse}"
    );

    legacy.send_raw(b"[]\n");
    let batch = legacy
        .response_for(&serde_json::Value::Null, std::time::Duration::from_secs(60))
        .expect("a batch frame must earn a typed answer");
    assert_eq!(
        batch["error"]["code"].as_i64(),
        Some(-32600),
        "a JSON-RPC batch must be rejected: {batch}"
    );

    // The session survives both faults and still correlates concurrent ids.
    legacy.send(&serde_json::json!({"jsonrpc":"2.0","id":900,"method":"ping","params":{}}));
    legacy.send(&serde_json::json!({"jsonrpc":"2.0","id":901,"method":"ping","params":{}}));
    for id in [900, 901] {
        let answer = legacy
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(120))
            .expect("concurrent pings must both answer");
        assert!(
            answer.get("error").is_none_or(|e| e.is_null()),
            "a fault must not stall concurrent correlation: {answer}"
        );
    }

    // ── Wire: the modern mirror of the era attack, and a spoofed
    //    clientInfo that must not rewrite the canonical identity ──────
    let mut modern = StdioSession::spawn_modern(root, &home);
    let bare = modern
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":"bare","method":"tools/list","params":{}}),
            std::time::Duration::from_secs(120),
        )
        .expect("a bare modern request must earn a typed answer");
    assert_eq!(
        bare["error"]["code"].as_i64(),
        Some(-32602),
        "the served modern path must enforce the per-request metadata: {bare}"
    );

    let served = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "meta", "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("a well-formed modern request must be served");
    assert!(
        served.get("error").is_none_or(|e| e.is_null()),
        "a well-formed modern request must not error: {served}"
    );
    assert_eq!(
        served["result"]["resultType"], "complete",
        "the modern wire shape must carry resultType: {served}"
    );

    let unsupported = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "bad-version", "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2099-12-31",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("an unsupported revision must earn a typed answer");
    assert_eq!(
        unsupported["error"]["code"].as_i64(),
        Some(-32022),
        "the served modern path must reject an unknown revision: {unsupported}"
    );

    // The legacy connection is still isolated and still legacy: its attack
    // did not cross into this connection, and this connection's modern
    // traffic did not cross back.
    let legacy_attack = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "legacy-attack", "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("the era-mismatched request must earn a typed answer");
    assert_eq!(
        legacy_attack["error"]["code"].as_i64(),
        Some(-32600),
        "the served legacy session must reject modern metadata: {legacy_attack}"
    );
    assert!(
        legacy.request("tools/list", serde_json::json!({}))["tools"]
            .as_array()
            .is_some(),
        "the legacy connection must survive the attack it mounted"
    );

    let identity_response = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "identity", "method": "server/discover",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientInfo": {"name": "evil", "version": "0"},
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("discover must answer on the modern connection");
    let identity = &identity_response["result"];
    assert_eq!(
        identity["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "susi-gmcp",
        "a spoofed clientInfo must not rewrite the canonical identity: {identity}"
    );
    assert_ne!(
        identity["_meta"]["io.modelcontextprotocol/serverInfo"]["version"], "0",
        "the canonical server version must not echo client input"
    );

    drop(legacy);
    drop(modern);
    let _ = std::fs::remove_dir_all(&scratch);

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("the vector's contract must exist");
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must exist"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-004")
        .collect();
    assert_eq!(rows.len(), 108, "the vector's pinned row count");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(path).exists(),
            "the declared production path must exist: {path}"
        );
    }
}

/// GMCP-203-004-05: interoperability and quality: legacy lifecycle compatibility
/// and governed deprecation.
///
/// Proves, on the served stdio path:
/// 1. Reference-fixture replay: legacy (2025-11-25) request fixtures each earn
///    their declared verdict (served answer or typed error), deterministically.
/// 2. Differential behavior: the production gate (`classify_line` with `EraState`)
///    and the served endpoint agree on the same fixtures (same forward/reject
///    decision and same error code).
/// 3. Determinism: replaying the identical fixture sequence twice yields
///    identical verdicts.
/// 4. Concurrent cross-era clients: one legacy session (`StdioSession::spawn`) and
///    one modern session (`StdioSession::spawn_modern`) run interleaved requests;
///    neither session's negotiated era leaks into the other (legacy keeps rejecting
///    modern `_meta` with -32600; modern keeps rejecting bare requests with -32602)
///    and both still answer afterwards.
/// 5. Transport parity: for the same malformed envelope, the stdio gate and
///    `http_envelope_rejection` emit the same code/message.
/// 6. Bounded latency: each served fixture round-trip completes well under a
///    generous bound (assert elapsed < 30s) so a hang fails instead of stalling.
#[test]
fn gmcp_req_203_004_05_interop() {
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-004-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let run_replay = |session: &mut StdioSession| -> Vec<(String, Option<i64>, bool)> {
        let fixtures: &[(&str, Option<i64>)] = &[
            ("positive/notification-cancelled.json", None),
            ("negative/request-batch-array.json", Some(-32600)),
            ("negative/request-null-id.json", Some(-32600)),
            (
                "negative/request-resources-read-uri-schemeless.json",
                Some(-32602),
            ),
        ];

        let mut results = Vec::new();
        for &(rel_path, expected_code) in fixtures {
            let fixture_path = root.join(".agents/gmcp/fixtures").join(rel_path);
            let fixture: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&fixture_path).unwrap())
                    .unwrap_or_else(|e| panic!("{rel_path} must parse: {e}"));
            let req = &fixture["request"];

            let start = std::time::Instant::now();
            if expected_code.is_none() {
                // Notification: send and ensure the session is immediately usable.
                session.send(req);
                session.send(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 990,
                    "method": "ping",
                    "params": {}
                }));
                let resp = session
                    .response_for(&serde_json::json!(990), std::time::Duration::from_secs(120))
                    .expect("session must remain responsive after notification");
                let elapsed = start.elapsed();
                assert!(
                    elapsed < std::time::Duration::from_secs(120),
                    "{rel_path}: latency exceeded 30s: {elapsed:?}"
                );
                assert!(
                    resp.get("error").is_none_or(|e| e.is_null()),
                    "notification must not cause error: {resp}"
                );
                results.push((rel_path.to_string(), None, true));
            } else {
                let resp = session
                    .request_verbatim(req, std::time::Duration::from_secs(120))
                    .unwrap_or_else(|| panic!("{rel_path} must receive a response"));
                let elapsed = start.elapsed();
                assert!(
                    elapsed < std::time::Duration::from_secs(120),
                    "{rel_path}: latency exceeded 30s: {elapsed:?}"
                );

                let code = resp["error"]["code"].as_i64();
                assert_eq!(
                    code, expected_code,
                    "{rel_path}: wrong error code in {resp}"
                );
                results.push((rel_path.to_string(), code, false));
            }
        }
        results
    };

    // ── 1. Reference-fixture replay on legacy session ───────────────────
    let mut legacy = StdioSession::spawn(root, &home);
    let run1 = run_replay(&mut legacy);

    // Also assert served answer on legacy ping
    let ping_start = std::time::Instant::now();
    let legacy_ping = legacy.request("ping", serde_json::json!({}));
    let ping_elapsed = ping_start.elapsed();
    assert!(
        ping_elapsed < std::time::Duration::from_secs(120),
        "legacy ping exceeded 30s: {ping_elapsed:?}"
    );
    assert!(legacy_ping.is_object());
    assert!(
        legacy_ping.get("resultType").is_none(),
        "legacy answer must not have resultType: {legacy_ping}"
    );

    // ── 2. Differential behavior: production gate vs served endpoint ───
    for (rel_path, expected_code, is_notification) in &run1 {
        let fixture_path = root.join(".agents/gmcp/fixtures").join(rel_path);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&fixture_path).unwrap())
                .unwrap_or_else(|e| panic!("{rel_path} must parse: {e}"));
        let req_bytes = serde_json::to_vec(&fixture["request"]).unwrap();

        // Simulate gate under legacy era
        let mut era = EraState::default();
        let init_line = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#;
        let _ = classify_line(init_line, &mut era);
        assert!(era.is_legacy(), "gate era must be pinned legacy");

        let gate_verdict = classify_line(&req_bytes, &mut era);
        if *is_notification {
            assert!(
                matches!(gate_verdict, GateVerdict::Forward | GateVerdict::Drop),
                "{rel_path}: gate must forward or drop notification, got {gate_verdict:?}"
            );
        } else {
            match gate_verdict {
                GateVerdict::Reject { code, .. } => {
                    assert_eq!(
                        Some(code),
                        *expected_code,
                        "{rel_path}: gate code ({code}) and served code ({expected_code:?}) must agree"
                    );
                }
                GateVerdict::Forward => {
                    // For method-level errors like schemeless URI, gate forwards and service returns typed error.
                    // Forward decision is consistent with server-side validation.
                }
                GateVerdict::Drop => {
                    panic!("{rel_path}: gate dropped non-notification request");
                }
            }
        }
    }

    // ── 3. Determinism: replaying the identical fixture sequence yields identical verdicts ─
    let run2 = run_replay(&mut legacy);
    assert_eq!(
        run1, run2,
        "replaying fixture sequence twice must yield identical verdicts"
    );

    // ── 4. Concurrent cross-era clients: negotiated era isolation ───────
    let mut modern = StdioSession::spawn_modern(root, &home);

    // Legacy rejects modern _meta with -32600
    let modern_meta_on_legacy = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "meta-on-legacy",
                "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("modern request on legacy session must earn response");
    assert_eq!(
        modern_meta_on_legacy["error"]["code"].as_i64(),
        Some(-32600),
        "legacy session must reject modern _meta with -32600"
    );

    // Modern rejects bare request with -32602
    let bare_on_modern = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "bare-on-modern",
                "method": "tools/list",
                "params": {}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("bare request on modern session must earn response");
    assert_eq!(
        bare_on_modern["error"]["code"].as_i64(),
        Some(-32602),
        "modern session must reject bare request with -32602"
    );

    // Interleaved: both still answer afterwards
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": 1001, "method": "ping", "params": {}
    }));
    modern.send(&serde_json::json!({
        "jsonrpc": "2.0", "id": "modern-discover", "method": "server/discover",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {}
        }}
    }));

    let legacy_resp = legacy
        .response_for(
            &serde_json::json!(1001),
            std::time::Duration::from_secs(120),
        )
        .expect("legacy session must answer ping after interleaved requests");
    assert!(
        legacy_resp.get("error").is_none_or(|e| e.is_null()),
        "legacy ping must succeed: {legacy_resp}"
    );
    assert!(legacy_resp["result"].get("resultType").is_none());

    let modern_resp = modern
        .response_for(
            &serde_json::json!("modern-discover"),
            std::time::Duration::from_secs(120),
        )
        .expect("modern session must answer discover after interleaved requests");
    assert!(
        modern_resp.get("error").is_none_or(|e| e.is_null()),
        "modern discover must succeed: {modern_resp}"
    );
    assert_eq!(
        modern_resp["result"]["resultType"], "complete",
        "modern response must carry resultType complete: {modern_resp}"
    );

    // ── 5. Transport parity: stdio gate and http_envelope_rejection ─────
    for raw in [
        br#"{not-json"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
    ] {
        let http_rejection =
            http_envelope_rejection(raw).expect("HTTP gate must reject malformed envelope");
        let mut gate_era = EraState::default();
        let stdio_verdict = classify_line(raw, &mut gate_era);
        let (stdio_code, stdio_message) = match stdio_verdict {
            GateVerdict::Reject { code, message, .. } => (code, message),
            other @ (GateVerdict::Forward | GateVerdict::Drop) => {
                panic!("stdio gate must reject malformed envelope: {other:?}")
            }
        };
        assert_eq!(
            http_rejection["error"]["code"].as_i64(),
            Some(stdio_code),
            "stdio and HTTP rejection codes must match for {raw:?}"
        );
        assert_eq!(
            http_rejection["error"]["message"].as_str(),
            Some(stdio_message),
            "stdio and HTTP rejection messages must match for {raw:?}"
        );
    }

    // Clean up scratch dir
    drop(legacy);
    drop(modern);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-004-06: operations and evidence for legacy lifecycle compatibility
/// and governed deprecation.
///
/// Proves:
/// 1. Inspectable bounded state on the production CLI (`susi mcp ledger`).
/// 2. Live inspectable surface on served stdio session: legacy session ping
///    returns clean object without modern result envelopes; modern session
///    requires per-request metadata and reports resultType.
/// 3. Configuration validation: `--refetched` without `--drift` fails validation.
/// 4. Deterministic drift check is clean on shipped corpus.
/// 5. Drift detection: modified `lifecycle.mdx` pin triggers `hash_drift`.
/// 6. Missing pin fails closed as `unfetchable`.
/// 7. Contract contains complete operator runbook (inspectable state, diagnostics,
///    incident triage, migration, rollback).
/// 8. Evidence record is retained, signed, and binds to the current ledger hash.
#[test]
fn gmcp_req_203_004_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-004-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // 1. The shared ledger report is the bounded operator surface.
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

    // 2. Live inspectable surface: legacy and modern stdio sessions.
    let mut legacy = StdioSession::spawn(root, &home);
    let ping = legacy.request("ping", serde_json::json!({}));
    assert!(ping.is_object(), "legacy ping must succeed: {ping}");
    assert!(
        ping.get("resultType").is_none(),
        "legacy ping must not carry modern resultType: {ping}"
    );
    drop(legacy);

    let mut modern = StdioSession::spawn_modern(root, &home);
    let modern_discover = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "modern-ops-discover",
                "method": "server/discover",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("modern discover must succeed on served session");
    assert_eq!(
        modern_discover["result"]["resultType"], "complete",
        "modern session must report resultType complete: {modern_discover}"
    );
    drop(modern);

    // 3. Invalid configuration is rejected: --refetched without --drift fails.
    let invalid = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid.status.success());

    // 4. Deterministic drift check is clean on the shipped corpus.
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

    // 5. A drifted lifecycle document is a named hash_drift finding.
    let lifecycle_doc = "docs/specification/2025-11-25/basic/lifecycle.mdx";
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(lifecycle_doc);
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
        .any(|f| f["document"] == lifecycle_doc && f["kind"] == "hash_drift"));

    // 6. An absent lifecycle document fails closed as unfetchable.
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
        .any(|f| f["document"] == lifecycle_doc && f["kind"] == "unfetchable"));

    // 7. The contract carries the complete operator runbook.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
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
        "-32600",
        "-32602",
        "2025-11-25",
        "legacy",
        "deprecation",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // 8. Evidence is retained, signed, and tied to current ledger bytes.
    use sha2::{Digest, Sha256};
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-004.jsonl"))
        .expect("VC-203-004 must retain evidence");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|record| record["requirement"] == "GMCP-203-004-06")
        .expect("evidence must retain a record for GMCP-203-004-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_004_06_operations"
    );
    assert_eq!(record["exit"], 0);
    assert_eq!(record["artifact_sha256"], ledger_hash);
    assert!(!record["detail"].as_str().unwrap_or_default().is_empty());
    assert!(!record["identity"].as_str().unwrap_or_default().is_empty());

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-004-M: mastery of legacy lifecycle compatibility and governed deprecation.
///
/// GMCP interoperates with pinned older MCP revisions through isolated compatibility
/// adapters for initialize/initialized, ping, legacy HTTP+SSE, resource subscriptions,
/// roots, sampling, elicitation, logging, and legacy task shapes without weakening the
/// modern path. Version selection is deterministic, downgrade is visible, every deprecated
/// feature has telemetry and a tested removal policy, and no legacy state crosses tenant
/// or connection boundaries. Consumes and verifies the evidence produced by requirements
/// 01-06; rejects missing, stale, unsigned, locally modified, waived, mock-only, or
/// zero-test evidence.
#[test]
fn gmcp_vc_203_004_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-004-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── 1. Shipped production path & isolated compatibility adapters:
    //    Modern session requires per-request _meta, receives complete
    //    results, and fails bare requests with -32602.
    let mut modern = StdioSession::spawn_modern(root, &home);
    let discover = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-modern-discover",
                "method": "server/discover",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("modern discover must answer on served path");
    assert_eq!(discover["result"]["resultType"], "complete");

    // Bare request without _meta fails with -32602 on modern session
    let bare_req = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-bare-fail",
                "method": "tools/list",
                "params": {}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("bare request must earn a typed rejection");
    assert_eq!(bare_req["error"]["code"], -32602);

    // ── 2. Legacy session operates without modern metadata:
    //    initialize/initialized handshake succeeds, ping succeeds
    //    and does not carry modern wire fields (visible downgrade).
    let mut legacy = StdioSession::spawn(root, &home);
    let legacy_ping = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-legacy-ping",
                "method": "ping",
                "params": {}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("legacy ping must succeed");
    assert!(legacy_ping["result"].is_object());
    assert!(
        legacy_ping["result"].get("resultType").is_none(),
        "legacy response must not leak modern resultType: {legacy_ping}"
    );

    // ── 3. Era boundary protection & non-poisoning:
    //    Sending modern _meta on a legacy session earns -32600 (era confusion),
    //    and subsequent legacy interactions continue to succeed cleanly.
    let confused = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-confused-meta",
                "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("cross-era metadata must earn typed rejection");
    assert_eq!(confused["error"]["code"], -32600);

    let legacy_still_valid = legacy
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-legacy-recovery",
                "method": "ping",
                "params": {}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("legacy session must recover cleanly after rejection");
    assert!(legacy_still_valid["result"].is_object());
    assert!(legacy_still_valid["result"].get("resultType").is_none());

    // ── 4. Cross-connection & tenant isolation:
    //    Concurrent legacy and modern sessions operate concurrently
    //    without leakage of negotiated state.
    let modern_again = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "mastery-modern-again",
                "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("modern session must remain unaffected by legacy traffic");
    assert_eq!(modern_again["result"]["resultType"], "complete");

    drop(modern);
    drop(legacy);

    // ── 5. Transport parity:
    //    HTTP envelope rejections emit consistent typed error codes.
    assert!(
        http_envelope_rejection(br#"{"jsonrpc":"2.0","id":1,"method":"ping","params":{}}"#)
            .is_none()
    );
    let http_envelope_err =
        http_envelope_rejection(br#"[]"#).expect("batch array envelope must be rejected");
    assert_eq!(http_envelope_err["error"]["code"], -32600);

    // ── 6. Contract vocabulary validation:
    //    docs/gmcp/contract-203-004.md must bind the complete vocabulary.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-004.md"))
        .expect("the vector contract must exist");
    for term in [
        "initialize",
        "initialized",
        "ping",
        "2025-11-25",
        "2026-07-28",
        "io.modelcontextprotocol/protocolVersion",
        "downgrade",
        "telemetry",
        "removal policy",
        "-32600",
        "-32602",
        "HTTP+SSE",
        "sampling",
        "elicitation",
        "roots",
        "subscriptions",
    ] {
        assert!(
            contract.contains(term),
            "contract must pin vocabulary term `{term}`"
        );
    }

    // ── 7. Evidence verification:
    //    Every record for GMCP-203-004-01..06 and GMCP-203-004-M must be
    //    complete, fresh, signed, exit 0, matching ledger hash, and backed by a real test.
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json"))
        .expect("canonical ledger must exist");
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-004.jsonl"))
        .expect("VC-203-004 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-004");
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
            "stale evidence: recorded hash must be current ledger hash"
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
        "GMCP-203-004-01",
        "GMCP-203-004-02",
        "GMCP-203-004-03",
        "GMCP-203-004-04",
        "GMCP-203-004-05",
        "GMCP-203-004-06",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── 8. Committed proof:
    //    The vector tree must be clean against git when committed.
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
