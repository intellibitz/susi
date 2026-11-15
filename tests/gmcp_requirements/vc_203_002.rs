/// GMCP-203-002-01: the JSON-RPC-substrate contract exists, is pinned to
/// the ledger's VC-203-002 rows, resolves its ambiguities, names the
/// canonical production path, and ships its seed fixtures and evidence
/// location — everything later work packages build against.
#[test]
fn gmcp_req_203_002_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── The contract document exists and carries every required
    //    section ────────────────────────────────────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-002.md"))
        .expect("contract-203-002.md must exist");
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
    // Ambiguities the contract must have resolved before implementation.
    for decision in ["batches", "id: null", "resultType", "initialize"] {
        assert!(
            contract.contains(decision),
            "contract must resolve: {decision}"
        );
    }

    // ── The ledger's VC-203-002 rows are the pinned normative set the
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
        .filter(|r| r["trace"]["vector"] == "VC-203-002")
        .collect();
    assert_eq!(rows.len(), 157, "the contract binds 157 VC-203-002 rows");
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
            row["trace"]["evidence"], ".agents/gmcp/evidence/VC-203-002.jsonl",
            "{id}: evidence must point at this vector's artifact"
        );
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        for cat in row["categories"].as_array().unwrap() {
            assert!(
                ["behavior", "error", "security"].contains(&cat.as_str().unwrap()),
                "{id}: unexpected category {cat}"
            );
        }
    }
    assert_eq!(revisions.get("2026-07-28"), Some(&107));
    assert_eq!(revisions.get("2025-11-25"), Some(&50));

    // ── The full pinned-corpus check still passes — the contract did
    //    not disturb the VC-203-001 machinery ─────────────────────────
    susi_gmcp::requirements_ledger::verify_pinned_ledger(root)
        .expect("the pinned corpus and ledger must verify");

    // ── Seed fixtures: every file the contract names exists, parses,
    //    and follows the fixture convention ──────────────────────────
    let seeds: &[(&str, &str)] = &[
        ("positive/request-2026-07-28-meta.json", "result"),
        ("positive/request-progress-token.json", "result"),
        ("positive/request-trace-context.json", "result"),
        ("positive/notification-cancelled.json", "none"),
        ("negative/request-batch-array.json", "error"),
        ("negative/request-meta-missing-2026-07-28.json", "error"),
        ("negative/request-unsupported-version.json", "error"),
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
        if file.starts_with("negative/") && fixture.get("expect_code").is_some() {
            assert!(
                fixture["expect_code"].is_i64() || fixture["expect_code"].is_u64(),
                "{file} expect_code must be a JSON-RPC error code integer"
            );
        }
    }
    // The modern-path required _meta fields the positive fixtures carry.
    let meta_fixture: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/request-2026-07-28-meta.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let meta = &meta_fixture["request"]["params"]["_meta"];
    for key in [
        "io.modelcontextprotocol/protocolVersion",
        "io.modelcontextprotocol/clientCapabilities",
    ] {
        assert!(
            !meta[key].is_null(),
            "the modern-path fixture must carry required _meta key {key}"
        );
    }
    // The batch fixture is malformed wire input — a non-object payload.
    let batch: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/request-batch-array.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        batch["request"].is_array(),
        "the batch fixture must carry the array payload it exercises"
    );

    // ── Evidence location: the vector's append-only artifact exists
    //    and holds a complete -01 record ──────────────────────────────
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-002.jsonl"))
        .expect("VC-203-002 must retain an evidence artifact");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|r| r["requirement"] == "GMCP-203-002-01")
        .expect("evidence must retain a record for GMCP-203-002-01");
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

/// GMCP-203-002-02: the canonical JSON-RPC gate (`susi_gmcp::jsonrpc_gate`)
/// is the production wire path — the served stdio surface answers the
/// contract's failure modes with typed errors instead of the SDK codec's
/// silent drops, enforces era-specific metadata rules, and stamps the
/// required `resultType` on results. Both roles are exercised: the
/// in-process classifier must agree with what the wire emits.
#[test]
fn gmcp_req_203_002_02_production() {
    use susi_gmcp::jsonrpc_gate::{classify_line, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-prod-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── The classifier is the canonical policy: same verdicts the wire
    //    emits, decidable without spawning a server ────────────────────
    let classify = |line: &str, era: &mut EraState| -> serde_json::Value {
        match classify_line(line.as_bytes(), era) {
            GateVerdict::Forward => serde_json::json!({"kind": "forward"}),
            GateVerdict::Drop => serde_json::json!({"kind": "drop"}),
            GateVerdict::Reject { code, data, .. } => {
                serde_json::json!({"kind": "reject", "code": code, "data": data})
            }
        }
    };
    let expect_reject = |line: &str, code: i64, era: &mut EraState| {
        let verdict = classify(line, era);
        assert_eq!(
            verdict["code"], code,
            "{line}: expected reject {code}, got {verdict}"
        );
    };

    // Malformed input fails closed, never silently.
    let mut era = EraState::default();
    expect_reject("{not-a-json-line}", -32700, &mut era);
    expect_reject("not even json", -32700, &mut era);
    expect_reject(
        r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
        -32600,
        &mut era,
    );
    expect_reject(
        r#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#,
        -32600,
        &mut era,
    );
    expect_reject(
        r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#,
        -32600,
        &mut era,
    );
    expect_reject(r#"{"jsonrpc":"2.0","id":1}"#, -32600, &mut era);
    // Valid envelopes forward.
    assert_eq!(
        classify(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#, &mut era)["kind"],
        "forward"
    );
    // A non-standard notification is a compatibility drop, not an error.
    assert_eq!(
        classify(
            r#"{"jsonrpc":"2.0","method":"notifications/lsp/something"}"#,
            &mut era
        )["kind"],
        "drop"
    );
    // Unsupported per-request versions are a typed -32022 carrying the
    // supported set, and they mark the era modern.
    let verdict = classify(
        r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"1900-01-01","io.modelcontextprotocol/clientCapabilities":{}}}}"#,
        &mut era,
    );
    assert_eq!(verdict["code"], -32022);
    assert!(
        verdict["data"]["supported"]
            .as_array()
            .is_some_and(|s| s.iter().any(|v| v == "2026-07-28")),
        "-32022 must carry data.supported: {verdict}"
    );
    assert!(
        era.is_modern(),
        "a _meta version field marks the modern era"
    );
    // On the modern path a request missing required _meta is -32602.
    expect_reject(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        -32602,
        &mut era,
    );
    // Legacy-era requests never required the modern fields.
    let mut legacy = EraState::default();
    assert_eq!(
        classify(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#,
            &mut legacy
        )["kind"],
        "forward"
    );
    assert_eq!(
        classify(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            &mut legacy
        )["kind"],
        "forward",
        "a 2025-11-25 session must not demand modern _meta"
    );

    // ── The shipped path: the served wire emits exactly the classified
    //    verdicts on the legacy era ────────────────────────────────────
    let mut wire = StdioSession::spawn(root, &home);

    // On the legacy era results keep the historical wire shape —
    // `resultType` is a 2026-07-28 field the compatibility rules strip
    // (absent is read as `complete`); only the modern path requires it.
    let result = wire.request("ping", serde_json::json!({}));
    assert!(
        result.get("resultType").is_none(),
        "a 2025-11-25 result must not carry resultType: {result}"
    );
    let result = wire.request("tools/list", serde_json::json!({}));
    assert!(result.get("resultType").is_none());

    // A null id is -32600, not a silent drop.
    let response = wire
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":null,"method":"tools/list"}),
            std::time::Duration::from_secs(300),
        )
        .expect("a null id must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // Unparseable input is -32700, not a silent drop.
    wire.send_raw(b"{not-a-json-line}\n");
    let response = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("a malformed line must be answered");
    assert_eq!(response["error"]["code"], -32700);

    // A batch array is -32600 — batches are not defined by MCP.
    wire.send_raw(br#"[{"jsonrpc":"2.0","id":9,"method":"ping"}]"#);
    wire.send_raw(b"\n");
    let response = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("a batch must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // A request id without a method is -32600.
    let response = wire
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":"no-method"}),
            std::time::Duration::from_secs(300),
        )
        .expect("a methodless request must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // A cancellation notification is silent, and malformed input never
    // corrupts the session: the next request still answers.
    wire.send(&serde_json::json!({
        "jsonrpc":"2.0","method":"notifications/cancelled",
        "params":{"requestId":"in-flight-x","reason":"probe"}
    }));
    std::thread::sleep(std::time::Duration::from_millis(300));
    let result = wire.request("ping", serde_json::json!({}));
    assert!(result.get("resultType").is_none());

    // ── The modern era: per-request _meta rules apply only there ──────
    let mut modern = StdioSession::spawn_modern(root, &home);

    // server/discover with _meta establishes the modern era.
    let response = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"discover-1","method":"server/discover",
                "params":{"_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities":{}
                }}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("server/discover must be answered");
    assert!(response.get("result").is_some(), "discover: {response}");

    // A request missing required _meta is -32602 on the modern path.
    let response = modern
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":"meta-missing","method":"tools/list","params":{}}),
            std::time::Duration::from_secs(300),
        )
        .expect("a meta-missing request must be answered");
    assert_eq!(response["error"]["code"], -32602);

    // A request carrying the fields is served, with resultType.
    let response = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"meta-ok","method":"tools/list",
                "params":{"_meta":{
                    "io.modelcontextprotocol/protocolVersion":"2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities":{}
                }}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("a conforming modern request must be answered");
    assert_eq!(
        response["result"]["resultType"], "complete",
        "modern results carry resultType: {response}"
    );

    // An unsupported per-request version is -32022 with data.supported.
    let response = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc":"2.0","id":"bad-version","method":"tools/list",
                "params":{"_meta":{
                    "io.modelcontextprotocol/protocolVersion":"1900-01-01",
                    "io.modelcontextprotocol/clientCapabilities":{}
                }}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("an unsupported version must be answered");
    assert_eq!(response["error"]["code"], -32022);
    assert!(
        response["error"]["data"]["supported"]
            .as_array()
            .is_some_and(|s| s.iter().any(|v| v == "2026-07-28")),
        "-32022 must carry data.supported: {response}"
    );

    // The era persists: a later request without _meta still fails -32602.
    let response = modern
        .request_verbatim(
            &serde_json::json!({"jsonrpc":"2.0","id":"era-persist","method":"tools/list","params":{}}),
            std::time::Duration::from_secs(300),
        )
        .expect("a late meta-missing request must be answered");
    assert_eq!(response["error"]["code"], -32602);

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-03: every applicable surface shares one implementation
/// and one policy boundary. The canonical envelope classification
/// (`jsonrpc_gate::classify_envelope`) decides identically for the stdio
/// line transport and the daemon/gateway `/mcp` HTTP transport —
/// `http_envelope_rejection` is the same function `handle_request` runs
/// before `service.call`, so a body the wire rejects produces the same
/// error document on either transport, and tool dispatch stays the
/// shared plane-bus registry (the same report over direct and wire).
#[test]
fn gmcp_req_203_002_03_integration() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── The shipped handler runs the canonical gate — the daemon's only
    //    JSON-RPC ingress (`/mcp` on 9093) classifies through
    //    `http_envelope_rejection` before the session layer sees a body.
    let server_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/server.rs")).unwrap();
    let handler = server_src
        .split("fn handle_request")
        .nth(1)
        .expect("handle_request must exist");
    let gate_call = handler
        .find("http_envelope_rejection(&effective)")
        .expect("handle_request must run the canonical envelope gate");
    let dispatch = handler
        .find("service.handle(req)")
        .expect("handle_request must dispatch to the service");
    assert!(
        gate_call < dispatch,
        "the envelope gate must run before the body reaches the service"
    );

    // ── Direct and served surfaces share tool dispatch: the same
    //    canonical report over `mcp call` and the stdio wire. ─────────
    let cli_report = ledger_report(root);
    let direct = ledger_tool_report(root);
    let mut wire = StdioSession::spawn(root, &home);
    let mut wire_modern = StdioSession::spawn_modern(root, &home);
    let wire_report = call_tool_report(&wire.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    ));
    assert_eq!(
        direct, cli_report,
        "direct dispatch must equal the CLI report"
    );
    assert_eq!(
        wire_report, cli_report,
        "served dispatch must equal the CLI report"
    );

    // ── Transport parity: every retained fixture produces the same
    //    verdict on the served wire and through the HTTP body gate. ────
    let mut envelope_rejects = 0;
    for class in ["positive", "negative"] {
        for entry in std::fs::read_dir(root.join(".agents/gmcp/fixtures").join(class))
            .unwrap()
            .flatten()
        {
            let fixture: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(entry.path()).unwrap()).unwrap();
            let Some(request) = fixture.get("request") else {
                continue;
            };
            let name = format!("{class}/{}", entry.file_name().to_str().unwrap());
            let era = fixture["era"].as_str().unwrap_or("legacy");
            let session = if era == "modern" {
                &mut wire_modern
            } else {
                &mut wire
            };
            let body = serde_json::to_vec(request).unwrap();
            let observed =
                match session.request_verbatim(request, std::time::Duration::from_secs(300)) {
                    None => serde_json::json!({"kind": "silent_drop"}),
                    Some(response) if response.get("error").is_some() => {
                        serde_json::json!({"kind": "error", "code": response["error"]["code"]})
                    }
                    Some(_) => serde_json::json!({"kind": "result"}),
                };
            let http = http_envelope_rejection(&body);
            match observed["kind"].as_str().unwrap() {
                "error" => {
                    if let Some(rejection) = &http {
                        envelope_rejects += 1;
                        assert_eq!(
                            rejection["error"]["code"], observed["code"],
                            "{name}: HTTP gate rejected with a different code than the wire"
                        );
                    }
                    // When the HTTP gate forwards, the wire's error came
                    // from a deeper layer — era/meta policy on HTTP lives
                    // in the SDK's header validation, so parity means "an
                    // error was answered", not "the same layer rejected".
                }
                "result" | "silent_drop" => {
                    assert!(
                        http.is_none(),
                        "{name}: the HTTP gate rejected a body the wire forwards: {http:?}"
                    );
                }
                other => panic!("{name}: unexpected wire outcome {other}"),
            }
        }
    }
    assert!(
        envelope_rejects >= 2,
        "the HTTP gate must share rejections with the wire, got {envelope_rejects}"
    );

    // ── The HTTP gate emits the identical error document the stdio
    //    transport writes — one canonical wire shape. ──────────────────
    for (body, code) in [
        (b"{not-a-json".as_slice(), -32700i64),
        (
            br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
            -32600,
        ),
        (
            br#"{"jsonrpc":"2.0","id":null,"method":"tools/list"}"#.as_slice(),
            -32600,
        ),
        (br#"{"jsonrpc":"2.0","id":1}"#.as_slice(), -32600),
        (
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_slice(),
            -32600,
        ),
    ] {
        let rejection = http_envelope_rejection(body)
            .unwrap_or_else(|| panic!("HTTP gate must reject {}", String::from_utf8_lossy(body)));
        assert_eq!(rejection["error"]["code"], code, "{}", rejection);
        assert_eq!(rejection["jsonrpc"], "2.0");
        assert!(rejection["error"]["message"].is_string());
    }
    // Valid envelopes and compatibility drops both pass the HTTP gate.
    assert!(http_envelope_rejection(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).is_none());
    assert!(http_envelope_rejection(
        br#"{"jsonrpc":"2.0","method":"notifications/lsp/something"}"#
    )
    .is_none());

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-04: adversarial JSON-RPC input is rejected with typed errors,
/// bounded before the SDK can buffer it, and isolated from neighboring
/// requests. Cancellation notifications never create a response or corrupt
/// the request stream, duplicate IDs remain independently correlated, and
/// modern-version metadata failures stay deterministic after malformed input.
#[test]
fn gmcp_req_203_002_04_adversarial() {
    use susi_gmcp::jsonrpc_gate::{classify_envelope, classify_line, EraState, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The in-process policy must fail closed before the SDK sees hostile
    // envelopes, and each classification must be independent of its peers.
    let mut era = EraState::default();
    for (line, code) in [
        ("{not-json}", -32700),
        ("[]", -32600),
        (r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#, -32600),
        (r#"{"jsonrpc":"2.0","id":1}"#, -32600),
        (r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#, -32600),
    ] {
        match classify_line(line.as_bytes(), &mut era) {
            GateVerdict::Reject { code: observed, .. } => assert_eq!(observed, code, "{line}"),
            other @ (GateVerdict::Forward | GateVerdict::Drop) => {
                panic!("{line} must reject, got {other:?}")
            }
        }
    }
    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","id":9,"method":"ping"}"#),
        GateVerdict::Forward
    ));
    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","method":"notifications/progress"}"#),
        GateVerdict::Forward
    ));

    let mut wire = StdioSession::spawn(root, &home);

    // A malformed frame produces a typed response and the following valid
    // request still reaches the service.
    wire.send_raw(b"{not-json}\n");
    let malformed = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("malformed input must receive a typed error");
    assert_eq!(malformed["error"]["code"], -32700);
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 700,
        "method": "ping",
        "params": {}
    }));
    let recovered = wire
        .response_for(&serde_json::json!(700), std::time::Duration::from_secs(300))
        .expect("a malformed frame must not poison its neighbor");
    assert_eq!(recovered["id"], 700);
    assert!(recovered["result"].is_object());

    // Duplicate IDs are a sender error (the protocol requires request IDs to
    // be unique), so the server need not promise two responses. It must not
    // let the duplicate poison a later, correctly identified request.
    for _ in 0..2 {
        wire.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 701,
            "method": "ping",
            "params": {}
        }));
    }
    let response = wire
        .response_for(&serde_json::json!(701), std::time::Duration::from_secs(300))
        .expect("a duplicate request must not prevent one valid response");
    assert_eq!(response["id"], 701);
    assert!(response["result"].is_object());
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 705,
        "method": "ping",
        "params": {}
    }));
    let response = wire
        .response_for(&serde_json::json!(705), std::time::Duration::from_secs(300))
        .expect("a duplicate request must not poison a later request");
    assert_eq!(response["id"], 705);
    assert!(response["result"].is_object());

    // Concurrent requests can complete in either order, but their IDs must
    // stay attached to their own response.
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 702,
        "method": "tools/list",
        "params": {}
    }));
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 703,
        "method": "ping",
        "params": {}
    }));
    for id in [702, 703] {
        let response = wire
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(300))
            .expect("concurrent requests must both complete");
        assert_eq!(response["id"], id);
        assert!(response["result"].is_object());
    }

    // Cancellation is a notification: it produces no response, including
    // invalid/late request references, and the next request remains usable.
    for request_id in ["in-flight", "missing"] {
        wire.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": {"requestId": request_id, "reason": "adversarial probe"}
        }));
    }
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 704,
        "method": "ping",
        "params": {}
    }));
    let response = wire
        .response_for(&serde_json::json!(704), std::time::Duration::from_secs(300))
        .expect("cancellation notifications must not block the next request");
    assert_eq!(response["id"], 704);
    assert!(
        wire.pending.iter().all(|value| !value["id"].is_null()),
        "cancellation must not emit a response"
    );

    // A cancellation notification cannot cancel initialize: a raw session
    // still completes its handshake and emits only the initialize response.
    let mut raw = StdioSession::spawn_raw(root, &home);
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "gmcp-adv", "version": "1"}
        }
    }));
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": 1}
    }));
    let initialize = raw
        .response_for(&serde_json::json!(1), std::time::Duration::from_secs(300))
        .expect("initialize must not be cancelled");
    assert!(initialize["result"].is_object());
    assert!(
        raw.pending.iter().all(|value| !value["id"].is_null()),
        "initialize cancellation must be silent"
    );
    raw.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));

    // Modern policy remains deterministic after an unsupported version and a
    // metadata omission; a fully populated request still succeeds.
    let mut modern = StdioSession::spawn_modern(root, &home);
    let unsupported = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "bad-version",
                "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("unsupported versions must be answered");
    assert_eq!(unsupported["error"]["code"], -32022);
    assert!(unsupported["error"]["data"]["supported"].is_array());
    let missing_meta = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "missing-meta",
                "method": "tools/list",
                "params": {}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("modern metadata failures must be answered");
    assert_eq!(missing_meta["error"]["code"], -32602);
    let modern_result = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "progressToken": "adv-1",
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        }}),
    );
    assert_eq!(modern_result["resultType"], "complete");

    drop(raw);
    drop(modern);
    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-05: retained reference fixtures interoperate with the served
/// rmcp SDK path in both protocol eras, direct and wire tool reports agree,
/// transport gates preserve the same malformed-envelope decisions, and a
/// bounded concurrent request set keeps response IDs and result shapes stable.
#[test]
fn gmcp_req_203_002_05_interop() {
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The direct CLI/call surfaces and the served SDK path must agree on a
    // stable, non-network capability before any fixture replay begins.
    let cli_report = ledger_report(root);
    let direct_report = ledger_tool_report(root);
    assert_eq!(direct_report, cli_report);

    let mut legacy = StdioSession::spawn(root, &home);
    let legacy_start = std::time::Instant::now();
    let legacy_ping = legacy.request("ping", serde_json::json!({}));
    assert!(
        legacy_start.elapsed() < std::time::Duration::from_secs(60),
        "legacy SDK round trip exceeded the interoperability budget"
    );
    assert!(legacy_ping.is_object());
    assert!(legacy_ping.get("resultType").is_none());

    // Replay every positive request fixture that has a wire payload. A
    // modern session is established once, matching how a reference client
    // keeps negotiated capabilities and then issues subsequent calls.
    let mut modern = StdioSession::spawn_modern(root, &home);
    for file in [
        "request-2026-07-28-meta.json",
        "request-progress-token.json",
        "request-trace-context.json",
        "server-discover-2026-07-28-meta.json",
        "server-discover-2026-07-28.json",
    ] {
        let path = root.join(".agents/gmcp/fixtures/positive").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let request = &fixture["request"];
        let response = modern
            .request_verbatim(request, std::time::Duration::from_secs(300))
            .unwrap_or_else(|| panic!("{file} must receive a response"));
        if file == "server-discover-2026-07-28.json" {
            // This fixture is intentionally retained verbatim as a
            // compatibility regression: params.protocolVersion is not the
            // modern per-request _meta field, so the production path rejects
            // it and the corrected *_meta fixture proves the interoperable
            // form above.
            assert_eq!(response["error"]["code"], -32602, "{file}: {response}");
            let divergences =
                std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json"))
                    .unwrap();
            assert!(divergences.contains("positive/server-discover-2026-07-28.json"));
            continue;
        }
        assert!(response["error"].is_null(), "{file}: {response}");
        assert!(response["result"].is_object(), "{file}: {response}");
        assert_eq!(fixture["expect"], "result");
        assert_eq!(response["result"]["resultType"], "complete", "{file}");
    }

    // The notification fixture is silent, but a reference client can keep
    // using the same session immediately afterward.
    let cancelled: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/notification-cancelled.json"),
        )
        .unwrap(),
    )
    .unwrap();
    legacy.send(&cancelled["request"]);
    legacy.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 900,
        "method": "ping",
        "params": {}
    }));
    let response = legacy
        .response_for(&serde_json::json!(900), std::time::Duration::from_secs(300))
        .expect("a notification must not stall the next request");
    assert_eq!(response["id"], 900);
    assert!(legacy.pending.iter().all(|value| !value["id"].is_null()));

    // Negative fixtures exercise the same error codes through the served
    // reference path. Invalid metadata stays on the modern session; wire
    // envelope errors use an independent legacy session to avoid ambiguity
    // around null correlation IDs.
    let negative: &[(&str, i64, bool)] = &[
        ("request-meta-missing-2026-07-28.json", -32602, true),
        ("request-unsupported-version.json", -32022, true),
        ("request-batch-array.json", -32600, false),
        ("request-null-id.json", -32600, false),
    ];
    for (file, expected_code, modern_path) in negative {
        let path = root.join(".agents/gmcp/fixtures/negative").join(file);
        let fixture: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
                .unwrap_or_else(|e| panic!("{file} must parse: {e}"));
        let response = if *modern_path {
            modern
                .request_verbatim(&fixture["request"], std::time::Duration::from_secs(300))
                .unwrap_or_else(|| panic!("{file} must receive an error"))
        } else {
            legacy
                .request_verbatim(&fixture["request"], std::time::Duration::from_secs(300))
                .unwrap_or_else(|| panic!("{file} must receive an error"))
        };
        assert_eq!(
            response["error"]["code"], *expected_code,
            "{file}: {response}"
        );
        assert!(
            response["error"]["message"].is_string(),
            "{file}: {response}"
        );
        if let Some(code) = fixture["expect_code"].as_i64() {
            assert_eq!(code, *expected_code, "{file} fixture drifted");
        }
    }

    // HTTP and stdio retain the same transport-independent verdict for every
    // malformed envelope that has no era-specific metadata policy.
    for body in [
        br#"{not-json"#.as_slice(),
        br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#.as_slice(),
        br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
    ] {
        let rejection = http_envelope_rejection(body).expect("HTTP must reject malformed input");
        let mut gate_era = susi_gmcp::jsonrpc_gate::EraState::default();
        let code = match susi_gmcp::jsonrpc_gate::classify_line(body, &mut gate_era) {
            susi_gmcp::jsonrpc_gate::GateVerdict::Reject { code, .. } => code,
            other @ (susi_gmcp::jsonrpc_gate::GateVerdict::Forward
            | susi_gmcp::jsonrpc_gate::GateVerdict::Drop) => {
                panic!("HTTP and stdio must reject the same body: {other:?}")
            }
        };
        assert_eq!(rejection["error"]["code"], code);
    }

    // A bounded burst models a reference client with multiple in-flight
    // requests. Every response must preserve its request ID and finish under
    // the same 30-second per-request budget used by the acceptance harness.
    let burst_start = std::time::Instant::now();
    for id in 1_000..1_016 {
        legacy.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "ping",
            "params": {}
        }));
    }
    for id in 1_000..1_016 {
        let response = legacy
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(300))
            .unwrap_or_else(|| panic!("burst request {id} timed out"));
        assert_eq!(response["id"], id);
        assert!(response["result"].is_object());
    }
    assert!(
        burst_start.elapsed() < std::time::Duration::from_secs(120),
        "concurrent burst exceeded the interoperability budget"
    );

    drop(modern);
    drop(legacy);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-002-06: the JSON-RPC substrate exposes bounded operator state,
/// validates its configuration, detects pinned-source drift fail closed, and
/// retains actionable diagnostics, migration, rollback, and evidence guidance.
#[test]
fn gmcp_req_203_002_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-jrpc-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // The production CLI is the inspectable operator surface. Its report is
    // bounded and verified before an operator or dashboard consumes it.
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
    assert_eq!(report["rows"].as_u64(), Some(1713));
    assert_eq!(report["documents"].as_u64(), Some(72));

    // --refetched is only meaningful with --drift; invalid configuration is
    // rejected rather than silently skipping the requested comparison.
    let invalid = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--refetched", ".agents/gmcp/spec"],
    )
    .output()
    .expect("susi must run");
    assert!(!invalid.status.success());

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
    assert_eq!(clean["checked"].as_u64(), Some(72));
    assert_eq!(clean["checked"], clean["matched"]);
    assert!(clean["findings"].as_array().unwrap().is_empty());

    // A changed pinned document is a diagnostic finding and a non-zero
    // result, never a warning that allows stale protocol behavior through.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).unwrap(),
    )
    .unwrap();
    let files = manifest["files"].as_array().unwrap();
    let drifted_doc = files[0]["document"].as_str().unwrap().to_string();
    let missing_doc = files[1]["document"].as_str().unwrap().to_string();
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(&drifted_doc);
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
        .any(|finding| finding["document"] == drifted_doc && finding["kind"] == "hash_drift"));

    // An unavailable source is separately diagnosable and also fails closed.
    std::fs::remove_file(refetched.join(&missing_doc)).unwrap();
    std::fs::write(&victim, &bytes[..bytes.len() - 1]).unwrap();
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
        .any(|finding| finding["document"] == missing_doc && finding["kind"] == "unfetchable"));

    // The contract is the retained incident handoff: it must tell an
    // operator what state exists, how to diagnose it, and how to migrate or
    // roll back a pin without inventing an undocumented recovery path.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-002.md"))
        .expect("contract must exist");
    for token in [
        "Operations and incident runbook",
        "Inspectable state",
        "Diagnostics",
        "Incident triage",
        "Migration (re-pinning the JSON-RPC standard)",
        "Rollback",
        "--refetched",
        "hash_drift",
        "unfetchable",
        "traceparent",
        "resultType",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // Evidence is append-only and tied to the current pinned ledger bytes,
    // so a changed dependency invalidates stale proof instead of preserving
    // a green record by wording alone.
    use sha2::{Digest, Sha256};
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-002.jsonl"))
        .expect("VC-203-002 must retain evidence");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|record| record["requirement"] == "GMCP-203-002-06")
        .expect("evidence must retain a record for GMCP-203-002-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_002_06_operations"
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

/// GMCP-203-002-M: the vector outcome holds on the shipped production
/// path — malformed, duplicated, reordered, oversized, and adversarial
/// messages fail predictably without corrupting another request — and
/// every retained evidence record for GMCP-203-002-01..06 plus the
/// mastery record is complete, fresh, signed, committed, and backed by
/// a real test.
#[test]
fn gmcp_vc_203_002_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::jsonrpc_gate::{classify_envelope, http_envelope_rejection, GateVerdict};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-002-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Vector outcome on the shipped wire path: an oversized line is
    //    answered once, bounded before the SDK buffers it, and cannot
    //    corrupt the request that shares its read chunk or any request
    //    after it ────────────────────────────────────────────────────
    // susi_command explicitly selects this fixture-owned runtime via SUSI_HOME.
    // Write the limit to that same root; automatic dev discovery is bypassed.
    let susi_home = home.join(".susi");
    std::fs::create_dir_all(&susi_home).unwrap();
    std::fs::write(
        susi_home.join("config.json"),
        r#"{"max_rpc_body_bytes": 1024}"#,
    )
    .unwrap();
    let mut wire = StdioSession::spawn(root, &home);
    let giant = format!(
        r#"{{"jsonrpc":"2.0","id":800,"method":"ping","params":{{"pad":"{}"}}}}"#,
        "x".repeat(4096)
    );
    // The dead line's tail shares a read chunk with the next request —
    // the bound must recover inside the same burst.
    let mut burst = giant.into_bytes();
    burst.push(b'\n');
    burst.extend_from_slice(br#"{"jsonrpc":"2.0","id":801,"method":"ping","params":{}}"#);
    burst.push(b'\n');
    wire.send_raw(&burst);
    let over = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("an oversized line must receive a typed error, not silence");
    assert_eq!(over["error"]["code"], -32600);
    let neighbour = wire
        .response_for(&serde_json::json!(801), std::time::Duration::from_secs(300))
        .expect("the request sharing the dead line's chunk must still be answered");
    assert!(neighbour["result"].is_object());

    // Malformed, batched, and null-id envelopes each fail typed and the
    // stream stays usable after every one of them.
    wire.send_raw(b"{corrupt\n");
    let parse = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("malformed input must be answered");
    assert_eq!(parse["error"]["code"], -32700);
    wire.send_raw(b"[]\n");
    let batch = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("a batch must be answered");
    assert_eq!(batch["error"]["code"], -32600);
    wire.send_raw(br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#);
    wire.send_raw(b"\n");
    let null_id = wire
        .rx
        .recv_timeout(std::time::Duration::from_secs(300))
        .expect("a null request id must be answered");
    assert_eq!(null_id["error"]["code"], -32600);
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 802,
        "method": "ping",
        "params": {}
    }));
    let alive = wire
        .response_for(&serde_json::json!(802), std::time::Duration::from_secs(300))
        .expect("the stream must remain usable after every failure kind");
    assert!(alive["result"].is_object());

    // Reordered concurrent answers keep their request identity.
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 803,
        "method": "tools/list",
        "params": {}
    }));
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 804,
        "method": "ping",
        "params": {}
    }));
    for id in [803, 804] {
        let response = wire
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(300))
            .expect("concurrent requests must both complete");
        assert_eq!(response["id"], id);
    }

    // A notification is never answered, and a client response envelope
    // (a result-bearing line answering a server→client request) is a
    // valid message, not an invalid request.
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": "never", "reason": "mastery probe"}
    }));
    wire.send(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 805,
        "method": "ping",
        "params": {}
    }));
    let response = wire
        .response_for(&serde_json::json!(805), std::time::Duration::from_secs(300))
        .expect("a notification must not block the next request");
    assert_eq!(response["id"], 805);
    assert!(
        wire.pending.iter().all(|value| !value["id"].is_null()),
        "notifications must not emit a response"
    );
    assert!(matches!(
        classify_envelope(br#"{"jsonrpc":"2.0","id":9,"result":{}}"#),
        GateVerdict::Forward
    ));

    // Transport parity: the HTTP envelope seam reaches the same verdict
    // for every envelope class the stdio gate rejects.
    for (line, code) in [
        (
            br#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#.as_slice(),
            -32600,
        ),
        (br#"[]"#.as_slice(), -32600),
        (
            br#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#.as_slice(),
            -32600,
        ),
    ] {
        let rejection =
            http_envelope_rejection(line).expect("HTTP seam must reject the same envelope");
        assert_eq!(rejection["error"]["code"], code);
    }

    // The modern era stays deterministic: missing `_meta` fails -32602,
    // an unsupported version fails -32022 with `data.supported`, and a
    // fully populated request returns a stamped `resultType`.
    let mut modern = StdioSession::spawn_modern(root, &home);
    let missing_meta = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "m-meta",
                "method": "tools/list",
                "params": {}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("modern metadata failures must be answered");
    assert_eq!(missing_meta["error"]["code"], -32602);
    let unsupported = modern
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "m-vers",
                "method": "tools/list",
                "params": {"_meta": {
                    "io.modelcontextprotocol/protocolVersion": "1900-01-01",
                    "io.modelcontextprotocol/clientCapabilities": {}
                }}
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("unsupported versions must be answered");
    assert_eq!(unsupported["error"]["code"], -32022);
    assert!(unsupported["error"]["data"]["supported"].is_array());
    let modern_result = modern.request(
        "tools/list",
        serde_json::json!({"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
            "progressToken": "mastery-1",
            "traceparent": "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
        }}),
    );
    assert_eq!(modern_result["resultType"], "complete");
    drop(wire);
    drop(modern);

    // ── The contract pins the vector's required vocabulary ──────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-002.md"))
        .expect("the vector contract must exist");
    for term in [
        "-32700",
        "-32600",
        "-32022",
        "resultType",
        "_meta",
        "notifications/cancelled",
        "progressToken",
        "traceparent",
        "2026-07-28",
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

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-002.jsonl"))
        .expect("VC-203-002 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-002");
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
        // Staleness: the recorded hash must be the canonical ledger's
        // current bytes — proof of a since-changed artifact is stale.
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
        // Zero-test evidence: the check must be a real command on a
        // committed artifact — a `cargo test` command must name a test
        // that exists in the named file.
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
        "GMCP-203-002-01",
        "GMCP-203-002-02",
        "GMCP-203-002-03",
        "GMCP-203-002-04",
        "GMCP-203-002-05",
        "GMCP-203-002-06",
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
