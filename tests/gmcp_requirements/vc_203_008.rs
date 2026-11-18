/// GMCP-203-008-01: the schemas/primitives/content/metadata contract
/// (`docs/gmcp/contract-203-008.md`) exists, pins its normative clause
/// and sources, binds exactly the VC-203-008 ledger rows, names the
/// canonical production paths, resolves the compatibility/threat
/// decisions, retains the seed fixtures, and holds a complete -01
/// evidence record.
#[test]
fn gmcp_req_203_008_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));

    // ── The contract document exists and carries every required
    //    section ────────────────────────────────────────────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-008.md"))
        .expect("contract-203-008.md must exist");
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
    for decision in [
        "ContentBlock",
        "Annotations",
        "includeContext",
        "adapter",
        "ttlMs",
        "resultType",
        "unknown",
    ] {
        assert!(
            contract.contains(decision),
            "contract must resolve: {decision}"
        );
    }

    // ── The ledger's VC-203-008 rows are the pinned normative set the
    //    contract binds: exact counts, complete traces, existing
    //    production paths ────────────────────────────────────────────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("ledger must parse"),
    )
    .expect("ledger must parse");
    let rows: Vec<&serde_json::Value> = ledger["requirements"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["trace"]["vector"] == "VC-203-008")
        .collect();
    assert_eq!(rows.len(), 425, "the contract binds 425 VC-203-008 rows");
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
            row["trace"]["evidence"], ".agents/gmcp/evidence/VC-203-008.jsonl",
            "{id}: evidence must point at this vector's artifact"
        );
        for cat in row["categories"].as_array().unwrap() {
            assert!(
                ["behavior", "error", "security", "deprecation", "claim"]
                    .contains(&cat.as_str().unwrap()),
                "{id}: unexpected category {cat}"
            );
        }
    }
    assert_eq!(revisions.get("2026-07-28"), Some(&304));
    assert_eq!(revisions.get("2025-11-25"), Some(&120));
    assert_eq!(revisions.get("susi@baseline-2026-10-17"), Some(&1));

    // ── Every pinned source the contract names exists and is listed in
    //    sources.json, so drift detection covers the bound corpus ────
    let sources: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).unwrap(),
    )
    .unwrap();
    let source_docs: std::collections::BTreeSet<&str> = sources["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["document"].as_str().unwrap())
        .collect();
    let bound_docs: std::collections::BTreeSet<&str> = rows
        .iter()
        .map(|r| r["source"]["document"].as_str().unwrap())
        .collect();
    for doc in &bound_docs {
        if *doc == "docs/gmcp-roadmap.md" {
            // The baseline claim's source is our own roadmap — the spec
            // pins only cover the upstream corpus.
            continue;
        }
        assert!(
            source_docs.contains(doc),
            "bound source {doc} must be a pinned file in sources.json"
        );
    }

    // ── The full pinned-corpus check still passes — the contract did
    //    not disturb the VC-203-001 machinery ─────────────────────────
    susi_gmcp::requirements_ledger::verify_pinned_ledger(root)
        .expect("the pinned corpus and ledger must verify");

    // ── Seed fixtures: every file the contract names exists, parses,
    //    and follows the payload-fixture convention ──────────────────
    let seeds: &[(&str, &str)] = &[
        ("positive/content-block-annotated-2026-07-28.json", "result"),
        ("negative/content-block-bad-mime.json", "error"),
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
        assert!(!fixture["payload"].is_null(), "{file} must carry a payload");
        assert_eq!(fixture["expect"].as_str().unwrap(), *expect);
        assert_eq!(fixture["era"].as_str().unwrap(), "modern");
    }
    // The annotated fixture exercises the lossless surface: icon,
    // audience, priority, _meta, and an unknown extension field.
    let annotated: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/content-block-annotated-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let payload = &annotated["payload"];
    for key in ["icons", "annotations", "_meta", "uri", "mimeType"] {
        assert!(
            !payload[key].is_null(),
            "the annotated fixture must exercise {key}"
        );
    }
    assert!(
        payload.get("io.example/futureExtension").is_some(),
        "the annotated fixture must carry an unknown extension field"
    );

    // ── Evidence location: the vector's append-only artifact exists
    //    and holds a complete -01 record ──────────────────────────────
    let evidence = std::fs::read_to_string(gmcp_evidence_path(root, ".agents/gmcp/evidence/VC-203-008.jsonl"))
        .expect("VC-203-008 must retain an evidence artifact");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|r| r["requirement"] == "GMCP-203-008-01")
        .expect("evidence must retain a record for GMCP-203-008-01");
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

/// GMCP-203-008-02: the canonical content/metadata model lives in
/// `susi_gmcp::content` and runs at the production boundary — the shared
/// `jsonrpc_gate::classify_envelope` both stdio and HTTP walk every
/// payload through. Lossless representation (unknown extension fields
/// round-trip verbatim), boundary validation (URI, MIME, base64,
/// Unicode, annotation ranges → typed errors), the explicit rmcp
/// adapter, bounded payloads, and deterministic canonical serialization
/// are all exercised here — on the module and over the served wire.
#[test]
fn gmcp_req_203_008_02_production() {
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-production-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Lossless round-trip: every variant, plus extension fields a
    //    future revision may define, survive parse → serialize ──────
    let fixture = std::fs::read_to_string(
        root.join(".agents/gmcp/fixtures/positive/content-block-annotated-2026-07-28.json"),
    )
    .unwrap();
    let fixture: serde_json::Value = serde_json::from_str(&fixture).unwrap();
    let payload = &fixture["payload"];
    let block = CanonicalContentBlock::from_wire(payload)
        .expect("annotated resource_link fixture must validate");
    let round_trip = block.to_wire();
    assert_eq!(
        &round_trip, payload,
        "canonical round-trip must be verbatim, extension fields included"
    );
    assert_eq!(
        round_trip["io.example/futureExtension"]["arbitrary"][0], "unknown",
        "the unknown extension field must survive verbatim"
    );

    // Every spec variant validates and round-trips.
    for (variant, value) in [
        (
            "text",
            serde_json::json!({"type":"text","text":"hello \u{4e16}\u{754c}"}),
        ),
        (
            "image",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"image/png"}),
        ),
        (
            "audio",
            serde_json::json!({"type":"audio","data":"AAE=","mimeType":"audio/wav"}),
        ),
        (
            "embedded text resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.txt","mimeType":"text/plain","text":"body"}}),
        ),
        (
            "embedded blob resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.bin","blob":"AAE="}}),
        ),
        ("resource_link", payload.clone()),
    ] {
        let parsed = CanonicalContentBlock::from_wire(&value)
            .unwrap_or_else(|e| panic!("{variant} must validate: {e}"));
        assert_eq!(
            parsed.to_wire(),
            value,
            "{variant} must round-trip losslessly"
        );
    }

    // ── Boundary validation: typed failures, never silent coercion ──
    let bad_mime_fixture = std::fs::read_to_string(
        root.join(".agents/gmcp/fixtures/negative/content-block-bad-mime.json"),
    )
    .unwrap();
    let bad_mime_fixture: serde_json::Value = serde_json::from_str(&bad_mime_fixture).unwrap();
    assert_eq!(
        CanonicalContentBlock::from_wire(&bad_mime_fixture["payload"]),
        Err(ContentError::BadMimeType("image")),
        "the bad-mime fixture must fail typed validation"
    );
    for (label, value, want) in [
        (
            "bad base64",
            serde_json::json!({"type":"image","data":"!!!","mimeType":"image/png"}),
            ContentError::BadBase64("image"),
        ),
        (
            "bad link uri",
            serde_json::json!({"type":"resource_link","uri":"relative/path","name":"x"}),
            ContentError::BadUri("resource_link.uri"),
        ),
        (
            "bad link mime",
            serde_json::json!({"type":"resource_link","uri":"file:///a","name":"x","mimeType":"no mime"}),
            ContentError::BadMimeType("resource_link.mimeType"),
        ),
        (
            "priority out of range",
            serde_json::json!({"type":"text","text":"t","annotations":{"priority":1.5}}),
            ContentError::BadPriority,
        ),
        (
            "bad audience",
            serde_json::json!({"type":"text","text":"t","annotations":{"audience":["admin"]}}),
            ContentError::BadAudience,
        ),
        (
            "control character",
            serde_json::json!({"type":"text","text":"a\u{7}"}),
            ContentError::BadUnicode("text"),
        ),
        (
            "bad blob base64",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a","blob":"%%%="}}),
            ContentError::BadBase64("resource.blob"),
        ),
        (
            "bad embedded uri",
            serde_json::json!({"type":"resource","resource":{"uri":"no scheme","text":"t"}}),
            ContentError::BadUri("resource.uri"),
        ),
        (
            "bad icon src",
            serde_json::json!({"type":"resource_link","uri":"file:///a","name":"x","icons":[{"src":"not a uri"}]}),
            ContentError::BadIcon,
        ),
        (
            "unknown variant",
            serde_json::json!({"type":"hologram","voxels":"AAA="}),
            ContentError::UnknownVariant,
        ),
    ] {
        assert_eq!(
            CanonicalContentBlock::from_wire(&value),
            Err(want),
            "{label} must fail typed validation"
        );
    }

    // Bounded payloads: a nesting deeper than the limit earns a typed
    // TooLarge, never unbounded recursion.
    let mut deep = serde_json::json!("leaf");
    for _ in 0..80 {
        deep = serde_json::json!({"k": deep});
    }
    let deep = serde_json::json!({"type":"text","text":"t","_meta":{"deep":deep}});
    assert_eq!(
        CanonicalContentBlock::from_wire(&deep),
        Err(ContentError::TooLarge)
    );

    // Deterministic canonical serialization: equal values produce
    // byte-identical output across parses and key orders.
    let reordered: serde_json::Value = serde_json::from_str(
        &serde_json::to_string(&serde_json::json!({ "type": "text", "text": "x", "z-ext": 1, "a-ext": 2}))
        .unwrap(),
    )
    .unwrap();
    let first = CanonicalContentBlock::from_wire(&reordered).unwrap();
    let second = CanonicalContentBlock::from_wire(&first.to_wire()).unwrap();
    assert_eq!(first.canonical_json(), second.canonical_json());
    assert!(
        first.canonical_json().find("a-ext").unwrap()
            < first.canonical_json().find("z-ext").unwrap(),
        "canonical serialization must be key-ordered"
    );

    // ── The explicit adapter: canonical → typed rmcp model — every
    //    spec field maps exactly onto the typed representation ───────
    let typed = block.to_rmcp().expect("canonical must adapt to rmcp");
    let typed_wire = serde_json::to_value(&typed).expect("rmcp model must serialize");
    assert_eq!(typed_wire["type"], "resource_link");
    assert_eq!(typed_wire["uri"], "file:///workspace/report.pdf");
    assert_eq!(typed_wire["name"], "quarterly-report");
    let adapted_priority = typed_wire["annotations"]["priority"]
        .as_f64()
        .expect("priority must adapt");
    assert!(
        (adapted_priority - 0.8).abs() < 1e-6,
        "annotations must adapt to the typed model"
    );

    // ── The shipped wire path: the gate's boundary validation is what
    //    `mcp serve` runs on every inbound line ──────────────────────
    let mut session = StdioSession::spawn(root, &home);
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "bad-uri", "method": "resources/read", "params": {"uri": "not a uri at all"}}));
    let response = session
        .response_for(
            &serde_json::json!("bad-uri"),
            std::time::Duration::from_secs(300),
        )
        .expect("malformed resources/read must be answered");
    assert_eq!(response["error"]["code"], -32602);
    assert!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("params.uri"),
        "the typed error must name the offending field: {response}"
    );

    // A malformed client response (sampling-style content) earns a
    // typed -32600 instead of the typed codec's silent drop.
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "bad-content", "result": {"content": {"type": "image", "data": "!!!", "mimeType": "image/png"}}}));
    let response = session
        .response_for(
            &serde_json::json!("bad-content"),
            std::time::Duration::from_secs(300),
        )
        .expect("malformed client response must be answered");
    assert_eq!(response["error"]["code"], -32600);
    assert!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("base64"),
        "the typed error must name the base64 failure: {response}"
    );

    // Malformed roots[] answers are caught the same way.
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "bad-roots", "result": {"roots": [{"uri": "relative/path"}]}}));
    let response = session
        .response_for(
            &serde_json::json!("bad-roots"),
            std::time::Duration::from_secs(300),
        )
        .expect("malformed roots response must be answered");
    assert_eq!(response["error"]["code"], -32600);

    // A well-formed request still reaches the service — the boundary
    // rejects only malformed payloads (any non-gate answer counts).
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "ok-read", "method": "resources/read", "params": {"uri": "file:///workspace/ok.txt"}}));
    let response = session
        .response_for(
            &serde_json::json!("ok-read"),
            std::time::Duration::from_secs(300),
        )
        .expect("well-formed resources/read must be answered");
    assert_ne!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default(),
        "content field `params.uri` is not a valid absolute URI",
        "a well-formed request must pass the boundary: {response}"
    );

    // ── Transport-independence: the HTTP body gate runs the same
    //    classify_envelope the stdio path does ───────────────────────
    for (body, want_code) in [
        (
            r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"bad uri"}}"#,
            -32602,
        ),
        (
            r#"{"jsonrpc":"2.0","id":2,"result":{"content":{"type":"audio","data":"$$","mimeType":"audio/wav"}}}"#,
            -32600,
        ),
        (r#"{"jsonrpc":"2.0","id":3,"method":"ping","params":{}}"#, 0),
    ] {
        let verdict = susi_gmcp::jsonrpc_gate::http_envelope_rejection(body.as_bytes());
        if want_code == 0 {
            assert!(verdict.is_none(), "a clean body must forward: {body}");
        } else {
            assert_eq!(
                verdict
                    .as_ref()
                    .and_then(|v| v.pointer("/error/code"))
                    .and_then(serde_json::Value::as_i64),
                Some(i64::from(want_code)),
                "the HTTP gate must emit the same typed failure: {body}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-03: every surface — `mcp serve` stdio, the daemon/gateway
/// HTTP `/mcp` handler, and the in-process gate — runs the same content
/// and metadata boundary: `jsonrpc_gate::classify_envelope` calling
/// `content::validate_payload` on the canonical `susi_gmcp::content`
/// model. A corpus of malformed and well-formed bodies must produce
/// identical typed verdicts on both transports, and the wire `reason`
/// must be the canonical `ContentError`'s own message — one model, not
/// parallel implementations.
#[test]
fn gmcp_req_203_008_03_integration() {
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-integration-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Verdict parity: identical bodies → identical verdicts on the
    //    stdio line gate and the HTTP body gate ──────────────────────
    // Each entry: (label, body, expected code or None for forward).
    let corpus: &[(&str, &str, Option<i64>)] = &[
        // Envelope rules both transports share.
        ("not json", "{oops", Some(-32700)),
        (
            "batch",
            r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#,
            Some(-32600),
        ),
        (
            "null id",
            r#"{"jsonrpc":"2.0","id":null,"method":"ping","params":{}}"#,
            Some(-32600),
        ),
        (
            "bad envelope version",
            r#"{"jsonrpc":"1.9","id":1,"method":"ping","params":{}}"#,
            Some(-32600),
        ),
        // Canonical content/metadata boundary (VC-203-008).
        (
            "bad resources/read uri",
            r#"{"jsonrpc":"2.0","id":5,"method":"resources/read","params":{"uri":"not a uri"}}"#,
            Some(-32602),
        ),
        (
            "missing resources/read uri",
            r#"{"jsonrpc":"2.0","id":6,"method":"resources/read","params":{}}"#,
            Some(-32602),
        ),
        (
            "bad subscribe uri",
            r#"{"jsonrpc":"2.0","id":7,"method":"resources/subscribe","params":{"uri":"rel/path"}}"#,
            Some(-32602),
        ),
        (
            "bad completion ref uri",
            r#"{"jsonrpc":"2.0","id":8,"method":"completion/complete","params":{"ref":{"type":"ref/resource","uri":"no"},"argument":{"name":"x","value":"y"}}}"#,
            Some(-32602),
        ),
        (
            "bad response content mime",
            r#"{"jsonrpc":"2.0","id":9,"result":{"content":{"type":"image","data":"aGk=","mimeType":"bad mime"}}}"#,
            Some(-32600),
        ),
        (
            "bad response content base64",
            r#"{"jsonrpc":"2.0","id":10,"result":{"content":[{"type":"audio","data":"$$","mimeType":"audio/wav"}]}}"#,
            Some(-32600),
        ),
        (
            "bad roots uri",
            r#"{"jsonrpc":"2.0","id":11,"result":{"roots":[{"uri":"rel"}]}}"#,
            Some(-32600),
        ),
        // Well-formed values forward identically.
        (
            "clean resources/read",
            r#"{"jsonrpc":"2.0","id":12,"method":"resources/read","params":{"uri":"file:///a.txt"}}"#,
            None,
        ),
        (
            "clean content response",
            r#"{"jsonrpc":"2.0","id":13,"result":{"content":{"type":"text","text":"hi","x-future":true}}}"#,
            None,
        ),
        (
            "elicitation form data",
            r#"{"jsonrpc":"2.0","id":14,"result":{"action":"accept","content":{"name":"alice","age":3}}}"#,
            None,
        ),
        (
            "clean ping",
            r#"{"jsonrpc":"2.0","id":15,"method":"ping","params":{}}"#,
            None,
        ),
    ];
    for (label, body, want) in corpus {
        // HTTP surface: the daemon/gateway `/mcp` handler calls this.
        let http = http_envelope_rejection(body.as_bytes());
        // Stdio surface: the same line through the served transport's
        // classifier on a fresh era (no negotiation yet).
        let mut era = EraState::default();
        let stdio = classify_line(body.as_bytes(), &mut era);
        match (want, &http) {
            (None, None) => {
                assert!(
                    matches!(stdio, GateVerdict::Forward | GateVerdict::Drop),
                    "{label}: stdio must forward/drop the clean body, got {stdio:?}"
                );
            }
            (None, Some(http_error)) => {
                panic!("{label}: HTTP gate rejected a clean body: {http_error}");
            }
            (Some(code), Some(http_error)) => {
                assert_eq!(
                    http_error
                        .pointer("/error/code")
                        .and_then(serde_json::Value::as_i64),
                    Some(*code),
                    "{label}: HTTP gate must emit {code}: {http_error}"
                );
                let GateVerdict::Reject {
                    code: stdio_code, ..
                } = stdio
                else {
                    panic!("{label}: stdio gate must reject with {code}, got {stdio:?}")
                };
                assert_eq!(
                    *code, stdio_code,
                    "{label}: stdio and HTTP verdicts must match"
                );
            }
            (Some(code), None) => panic!("{label}: HTTP gate forwarded; expected {code}"),
        }
    }

    // ── One canonical model: the wire `reason` IS the ContentError
    //    message the canonical model emits — no parallel wording ─────
    let bad_uri_body =
        r#"{"jsonrpc":"2.0","id":20,"method":"resources/read","params":{"uri":"no uri"}}"#;
    let http_doc = http_envelope_rejection(bad_uri_body.as_bytes()).expect("must reject");
    let wire_reason = http_doc["error"]["data"]["reason"]
        .as_str()
        .expect("rejection must carry a typed reason");
    let canonical_reason = susi_gmcp::content::ContentError::BadUri("params.uri").wire_message();
    assert_eq!(
        wire_reason, canonical_reason,
        "the gate's reason must be the canonical model's own message"
    );

    // ── The served wire agrees: same corpus member, same verdict ────
    let mut session = StdioSession::spawn(root, &home);
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "intg-bad-uri", "method": "resources/read", "params": {"uri": "no uri"}}));
    let response = session
        .response_for(
            &serde_json::json!("intg-bad-uri"),
            std::time::Duration::from_secs(300),
        )
        .expect("served session must answer the malformed request");
    assert_eq!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default(),
        canonical_reason,
        "served stdio must surface the canonical reason verbatim"
    );
    // And the modern-era surface applies the same boundary — the
    // policy never depends on which era the session negotiated.
    let mut modern = StdioSession::spawn_modern(root, &home);
    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "intg-modern-bad", "method": "resources/read", "params": { "uri": "no uri", "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}));
    let response = modern
        .response_for(
            &serde_json::json!("intg-modern-bad"),
            std::time::Duration::from_secs(300),
        )
        .expect("modern session must answer the malformed request");
    assert_eq!(response["error"]["code"], -32602);
    assert_eq!(
        response["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default(),
        canonical_reason
    );

    // ── Structural proof of one boundary: the only HTTP entry points
    //    the daemon/gateway serve run `http_envelope_rejection`, and
    //    both it and `classify_line` share `classify_envelope` ───────
    let gate_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/jsonrpc_gate.rs"))
        .expect("gate source must be readable");
    assert!(
        gate_src.contains("pub fn classify_envelope")
            && gate_src.contains("crate::content::validate_payload(object)"),
        "classify_envelope must run the canonical content boundary"
    );
    let server_src = std::fs::read_to_string(root.join("crates/susi-gmcp/src/server.rs"))
        .expect("server source must be readable");
    assert!(
        server_src.contains("jsonrpc_gate::http_envelope_rejection")
            && server_src.contains("jsonrpc_gate::StdioGate"),
        "the served transports must both run the shared gate"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-04: adversarial and failure behavior for the
/// content/metadata boundary — extension-field injection, protocol-
/// namespace smuggling, URI/MIME/base64/Unicode spoofs, depth bombs,
/// external `$ref` smuggling, over-validation resistance, deterministic
/// replays, and stream recovery after a rejection. Every probe runs the
/// production path: `susi_gmcp::content`'s canonical validation, the
/// shared gate, and the served `mcp serve` wire.
#[test]
fn gmcp_req_203_008_04_adversarial() {
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    use susi_gmcp::jsonrpc_gate::http_envelope_rejection;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-adversarial-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Extension-field injection: envelope-mimicking keys inside a
    //    content block are preserved verbatim but never interpreted ──
    let injection: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/content-block-envelope-injection.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let injected_block = &injection["payload"]["result"]["content"];
    let block = CanonicalContentBlock::from_wire(injected_block)
        .expect("injected extension keys must still validate — they are data");
    let wire = block.to_wire();
    assert_eq!(
        wire["method"], "tools/call",
        "the forged method must be preserved as data"
    );
    assert_eq!(
        wire["id"], "spoofed-request",
        "the forged id must be preserved as data"
    );
    assert_eq!(
        wire["io.modelcontextprotocol/protocolVersion"], "2999-01-01",
        "the reserved-namespace key is data, not protocol metadata"
    );
    // The gate never treats the injected body as a request: the whole
    // message forwards as the response it is (or drops compatibly).
    assert!(
        http_envelope_rejection(
            serde_json::to_string(&injection["payload"])
                .unwrap()
                .as_bytes()
        )
        .is_none(),
        "an extension-injected response must not be rejected as a forged request"
    );

    // Protocol-reserved keys nested in `arguments` are NOT the
    // `params._meta` protocol fields the modern path requires — a
    // modern request missing its real `_meta` still earns -32602 even
    // when arguments smuggle the namespaced keys.
    let mut modern = StdioSession::spawn_modern(root, &home);
    modern.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "smuggle", "method": "tools/call", "params": { "name": "x", "arguments": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}));
    let response = modern
        .response_for(
            &serde_json::json!("smuggle"),
            std::time::Duration::from_secs(300),
        )
        .expect("the smuggled request must be answered");
    assert_eq!(
        response["error"]["code"], -32602,
        "namespaced keys in arguments must not satisfy the _meta policy: {response}"
    );

    // ── Spoof battery: every malformed class fails typed ────────────
    for (label, value, want) in [
        (
            "mid-string base64 padding",
            serde_json::json!({"type":"audio","data":"QUJD=RAAA","mimeType":"audio/wav"}),
            ContentError::BadBase64("audio"),
        ),
        (
            "CRLF-injected mimeType",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"image/png\r\nSet-Cookie: x"}),
            ContentError::BadMimeType("image"),
        ),
        (
            "control char in resource uri",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a\u{0}b","text":"t"}}),
            ContentError::BadUri("resource.uri"),
        ),
        (
            "negative priority",
            serde_json::json!({"type":"text","text":"t","annotations":{"priority":-0.5}}),
            ContentError::BadPriority,
        ),
        (
            "forged audience role",
            serde_json::json!({"type":"text","text":"t","annotations":{"audience":["system"]}}),
            ContentError::BadAudience,
        ),
        (
            "control char in link name",
            serde_json::json!({"type":"resource_link","uri":"file:///a","name":"x\u{0}"}),
            ContentError::BadUnicode("resource_link.name"),
        ),
    ] {
        assert_eq!(
            CanonicalContentBlock::from_wire(&value),
            Err(want),
            "{label} must fail typed"
        );
    }
    // Document the decision explicitly: `javascript:` is a valid
    // absolute URI at this boundary (SSRF/origin policy is VC-203-032).
    assert!(CanonicalContentBlock::from_wire(
        &serde_json::json!({"type":"resource_link","uri":"javascript:alert(1)","name":"x"})
    )
    .is_ok());

    // ── Bounded resources: a nesting bomb earns a typed -32600 ──────
    let mut deep = serde_json::json!("leaf");
    for _ in 0..80 {
        deep = serde_json::json!({"k": deep});
    }
    let bomb = serde_json::json!({ "jsonrpc": "2.0", "id": "bomb", "result": {"content": [{"type": "text", "text": "x", "_meta": {"deep": deep}}]}});
    let rejection = http_envelope_rejection(serde_json::to_string(&bomb).unwrap().as_bytes())
        .expect("a nesting bomb must be rejected");
    assert_eq!(rejection["error"]["code"], -32600);
    assert!(
        rejection["error"]["data"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("bounded"),
        "the bomb's reason must name the resource bound: {rejection}"
    );

    // ── Schemeless and traversal URIs fail at the boundary ──────────
    let schemeless: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/request-resources-read-uri-schemeless.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut session = StdioSession::spawn(root, &home);
    session.send(&schemeless["request"]);
    let response = session
        .response_for(&serde_json::json!(42), std::time::Duration::from_secs(300))
        .expect("schemeless uri must be answered");
    assert_eq!(response["error"]["code"], -32602);

    // ── External `$ref` smuggling: a tool schema that tries to fetch
    //    a remote reference cannot even register — fail-closed at
    //    compile; local `#/$defs` refs (the pinned corpus) still work ─
    let service = susi_gmcp::protocol::GmcpService::new(scratch.join("workspace"));
    let remote_tool: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"evil","inputSchema":{"$ref":"https://evil.example/x.json"}}),
    )
    .unwrap();
    assert!(
        service
            .register_tool(
                remote_tool,
                std::sync::Arc::new(|_, _| Box::pin(async {
                    Err(susi_gmcp::rmcp::ErrorData::internal_error(
                        "unreachable",
                        None,
                    ))
                }))
            )
            .is_err(),
        "a remote-$ref inputSchema must never register"
    );
    let local_tool: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"ok","inputSchema":{"$defs":{"s":{"type":"string"}},"type":"object","properties":{"v":{"$ref":"#/$defs/s"}}}}),
    )
    .unwrap();
    assert!(
        service
            .register_tool(
                local_tool,
                std::sync::Arc::new(|_, _| Box::pin(async {
                    Err(susi_gmcp::rmcp::ErrorData::internal_error(
                        "unreachable",
                        None,
                    ))
                }))
            )
            .is_ok(),
        "a local $defs ref compiles — the pinned corpus is the only resolver"
    );

    // ── Over-validation resistance: free-form tool `arguments` are
    //    never judged as spec content — a tools/call carrying a
    //    content-shaped argument reaches the service, not the gate ───
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "args-not-content", "method": "tools/call", "params": { "name": "definitely-not-a-real-tool", "arguments": {"type": "image", "data": "%%%", "mimeType": "junk"}}}));
    let response = session
        .response_for(
            &serde_json::json!("args-not-content"),
            std::time::Duration::from_secs(300),
        )
        .expect("tools/call must be answered by the service, not the gate");
    assert!(
        response["error"]["data"]["reason"].is_null()
            || response["error"]["data"]["reason"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
        "free-form arguments must not trip the content boundary: {response}"
    );

    // ── Recovery: a rejection never corrupts the stream — the next
    //    well-formed request on the same session answers normally ────
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "after-reject", "method": "resources/list", "params": {}}));
    let response = session
        .response_for(
            &serde_json::json!("after-reject"),
            std::time::Duration::from_secs(300),
        )
        .expect("the session must keep serving after a rejection");
    assert!(
        response.get("result").is_some(),
        "a well-formed request after a -32602 must reach the service: {response}"
    );

    // ── Deterministic replay: the same attack body yields the same
    //    typed verdict — no state leaks between attempts ─────────────
    let attack =
        r#"{"jsonrpc":"2.0","id":1,"method":"resources/read","params":{"uri":"//etc/passwd"}}"#;
    let first = http_envelope_rejection(attack.as_bytes());
    let second = http_envelope_rejection(attack.as_bytes());
    assert_eq!(
        first, second,
        "the same body must produce identical verdicts"
    );

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-05: interoperability and quality for the canonical
/// content/metadata model — every retained fixture replays its declared
/// outcome through the canonical validator and both wire gates, the
/// explicit adapter is byte-for-byte equivalent to a direct reference-SDK
/// (`rmcp::model`) parse in the server role and round-trips in the client
/// role, and the published bounds (determinism, per-parse latency) hold
/// under replay. Divergences between the canonical model and the typed
/// SDK are the vector's documented rationale — extension preservation and
/// boundary validation — and are asserted, not assumed.
#[test]
fn gmcp_req_203_008_05_interop() {
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};
    use susi_gmcp::jsonrpc_gate::{classify_line, http_envelope_rejection, EraState, GateVerdict};
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Fixture replay: every bound fixture earns its declared
    //    outcome through the canonical model ─────────────────────────
    let positive: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/positive/content-block-annotated-2026-07-28.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(positive["expect"], "result");
    let block = CanonicalContentBlock::from_wire(&positive["payload"])
        .expect("the positive fixture must validate canonically");
    assert_eq!(block.to_wire(), positive["payload"], "lossless round-trip");

    for (file, want) in [
        (
            "content-block-bad-mime.json",
            ContentError::BadMimeType("image"),
        ),
        (
            "content-block-base64-padding-abuse.json",
            ContentError::BadBase64("audio"),
        ),
    ] {
        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".agents/gmcp/fixtures/negative").join(file))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            fixture["expect"], "error",
            "{file} must declare an error outcome"
        );
        assert_eq!(
            CanonicalContentBlock::from_wire(&fixture["payload"]),
            Err(want.clone()),
            "{file} must fail the declared typed validation"
        );
        // The same block as a client response's content earns the gate's
        // typed -32600 carrying the canonical reason — one implementation.
        let wire_doc = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": {"content": fixture["payload"].clone()}});
        let rejection =
            http_envelope_rejection(serde_json::to_string(&wire_doc).unwrap().as_bytes())
                .unwrap_or_else(|| panic!("{file}: the HTTP gate must reject"));
        assert_eq!(rejection["error"]["code"], -32600, "{file}");
        assert_eq!(
            rejection["error"]["data"]["reason"]
                .as_str()
                .unwrap_or_default(),
            want.wire_message(),
            "{file}: the gate reason must be the canonical message"
        );
    }

    // The envelope-injection fixture declares `forward`: preserved as
    // data, never interpreted — on both transports.
    let injection: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/content-block-envelope-injection.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(injection["expect"], "forward");
    let body = serde_json::to_string(&injection["payload"]).unwrap();
    assert!(http_envelope_rejection(body.as_bytes()).is_none());
    let mut era = EraState::default();
    assert!(matches!(
        classify_line(body.as_bytes(), &mut era),
        GateVerdict::Forward | GateVerdict::Drop
    ));

    // The schemeless-uri request fixture replays to -32602 on the served
    // wire — the declared outcome verbatim.
    let schemeless: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            root.join(".agents/gmcp/fixtures/negative/request-resources-read-uri-schemeless.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut session = StdioSession::spawn(root, &home);
    session.send(&schemeless["request"]);
    let response = session
        .response_for(&serde_json::json!(42), std::time::Duration::from_secs(300))
        .expect("the schemeless-uri fixture must be answered");
    assert_eq!(
        response["error"]["code"],
        schemeless["expect_code"].as_i64().unwrap()
    );

    // ── Reference-SDK roles: the canonical adapter is equivalent to a
    //    direct typed-model parse (server role), and a typed model
    //    re-enters the canonical model losslessly (client role) ──────
    let sdk_parsed: susi_gmcp::rmcp::model::ContentBlock =
        serde_json::from_value(positive["payload"].clone())
            .expect("the reference SDK must parse the positive fixture");
    let via_adapter = block.to_rmcp().expect("canonical must adapt");
    assert_eq!(
        serde_json::to_value(&sdk_parsed).unwrap(),
        serde_json::to_value(&via_adapter).unwrap(),
        "the adapter must equal a direct reference-SDK parse"
    );

    // Documented divergences — asserted, not assumed: the typed SDK
    // silently drops the unknown extension field the canonical model
    // preserves, and it accepts the invalid mimeType the canonical
    // boundary rejects (validation is the canonical model's added duty).
    let sdk_wire = serde_json::to_value(&sdk_parsed).unwrap();
    assert!(
        sdk_wire.get("io.example/futureExtension").is_none(),
        "documented: the typed SDK drops unknown extension fields"
    );
    assert!(
        serde_json::from_value::<susi_gmcp::rmcp::model::ContentBlock>(
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"bad mime"})
        )
        .is_ok(),
        "documented: the typed SDK accepts what the canonical boundary rejects"
    );

    // Client-role round-trip: a typed model's serialization re-enters the
    // canonical model with every spec field intact.
    let re_canonical = CanonicalContentBlock::from_wire(&sdk_wire)
        .expect("a reference-SDK serialization must re-enter canonically");
    assert_eq!(re_canonical.to_wire(), sdk_wire);

    // ── Quality bounds: canonical validation is cheap and
    //    deterministic — the published SLOs of the contract ──────────
    let started = std::time::Instant::now();
    let iterations = 200;
    for _ in 0..iterations {
        let parsed = CanonicalContentBlock::from_wire(&positive["payload"])
            .expect("repeated validation must succeed");
        std::hint::black_box(parsed);
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "200 canonical validations must finish well under budget: {elapsed:?}"
    );
    // Determinism across orderings and runs — canonical serialization
    // is the evidence-hash-stable form the contract publishes.
    let a = CanonicalContentBlock::from_wire(&positive["payload"]).unwrap();
    let b = CanonicalContentBlock::from_wire(&a.to_wire()).unwrap();
    assert_eq!(a.canonical_json(), b.canonical_json());
    let c = CanonicalContentBlock::from_wire(&positive["payload"]).unwrap();
    assert_eq!(a.canonical_json(), c.canonical_json());

    // ── No unrecorded wire divergences: every -008 fixture's declared
    //    outcome matched observed behavior in this replay, so the
    //    shared divergences registry needs no -008 entries ───────────
    let divergences: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/fixtures/divergences.json")).unwrap(),
    )
    .unwrap();
    for divergence in divergences["divergences"].as_array().unwrap() {
        let fixture = divergence["fixture"].as_str().unwrap_or_default();
        assert!(
            !fixture.contains("content-block") && !fixture.contains("resources-read-uri"),
            "a -008 fixture regressed into the divergence registry: {divergence}"
        );
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-008-06: operations and evidence for the content/metadata
/// model. The ledger CLI is the bounded operator surface, a served
/// `tools/call` is the live inspectable surface whose content blocks
/// parse through the canonical boundary, `--refetched` without `--drift`
/// is rejected configuration, the deterministic drift check names a
/// drifted schema pin and fails closed on a missing one, the contract
/// carries the runbook, and the evidence artifact is signed and
/// reproducible.
#[test]
fn gmcp_req_203_008_06_operations() {
    use susi_gmcp::content::CanonicalContentBlock;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-ops-{}", std::process::id()));
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

    // A served `tools/call` is this vector's live inspectable surface:
    // the result's `content[]` blocks cross the gate and must parse
    // through the canonical boundary an operator (or a peer) consumes.
    let mut served = StdioSession::spawn(root, &home);
    let result = served.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    let tool_report = call_tool_report(&result);
    assert_eq!(tool_report["verified"], true);
    let block = &result["content"][0];
    assert_eq!(block["type"], "text", "served content must be typed");
    CanonicalContentBlock::from_wire(block)
        .expect("served content blocks must satisfy the canonical model");
    drop(served);

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
    // covers this vector's pinned schema corpus.
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

    // A drifted schema rendering is a named `hash_drift` finding and a
    // non-zero result — stale model proof cannot survive a changed pin.
    let schema_doc = "schema/2026-07-28/schema.json";
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(schema_doc);
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
        .any(|f| { f["document"] == schema_doc && f["kind"] == "hash_drift" }));

    // An absent schema document fails closed as `unfetchable`.
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
        .any(|f| { f["document"] == schema_doc && f["kind"] == "unfetchable" }));

    // The contract carries the operator runbook: state, diagnostics,
    // triage, migration, and rollback are documented — not tribal.
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-008.md"))
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
        "-32602",
        "-32600",
        "CanonicalContentBlock",
        "silent drop",
    ] {
        assert!(contract.contains(token), "contract must document {token}");
    }

    // Evidence is retained, signed, and tied to the canonical ledger
    // bytes, so a changed corpus invalidates this proof.
    use sha2::{Digest, Sha256};
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(
        std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    ));
    let evidence = std::fs::read_to_string(gmcp_evidence_path(root, ".agents/gmcp/evidence/VC-203-008.jsonl"))
        .expect("VC-203-008 must retain evidence");
    let record = newest_records(&evidence)
        .into_iter()
        .find(|record| record["requirement"] == "GMCP-203-008-06")
        .expect("evidence must retain a record for GMCP-203-008-06");
    assert_eq!(
        record["check"],
        "cargo test --locked --test gmcp_requirements gmcp_req_203_008_06_operations"
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

/// GMCP-203-008-M: the vector outcome holds on the shipped production
/// path — every MCP content variant is represented losslessly (text,
/// image, audio, resource, resource link; annotations, icons, `_meta`,
/// audience, priority, unknown extension fields), the canonical model
/// is what the wire gate enforces on the served path, JSON Schema
/// 2020-12 tool contracts compile locally and refuse external `$ref`
/// resolution, canonical serialization is deterministic, and this test
/// consumes and verifies all requirement evidence — missing, stale,
/// unsigned, waived, mock-only, or zero-test records fail.
#[test]
fn gmcp_vc_203_008_mastery() {
    use sha2::{Digest, Sha256};
    use susi_gmcp::content::{CanonicalContentBlock, ContentError};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-008-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Shipped wire path: the served result's content blocks satisfy
    //    the canonical model, and the gate emits typed failures for
    //    malformed params and malformed content-bearing payloads. ────
    let mut session = StdioSession::spawn(root, &home);
    let result = session.request(
        "tools/call",
        serde_json::json!({"name": "ledger_verify", "arguments": {}}),
    );
    for block in result["content"]
        .as_array()
        .expect("content must be an array")
    {
        CanonicalContentBlock::from_wire(block).unwrap_or_else(|e| {
            panic!("served content block must satisfy the canonical model: {e}")
        });
    }
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "m-uri", "method": "resources/read", "params": {"uri": "no-scheme"}}));
    let response = session
        .response_for(
            &serde_json::json!("m-uri"),
            std::time::Duration::from_secs(300),
        )
        .expect("malformed URI must earn a typed verdict");
    assert_eq!(response["error"]["code"], -32602, "{response}");
    session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "m-content", "result": {"content": {"type": "audio", "data": "###", "mimeType": "audio/wav"}}}));
    let response = session
        .response_for(
            &serde_json::json!("m-content"),
            std::time::Duration::from_secs(300),
        )
        .expect("malformed content must earn a typed verdict");
    assert_eq!(response["error"]["code"], -32600, "{response}");
    drop(session);

    // ── Lossless: every content variant round-trips verbatim through
    //    the canonical model, unknown extension fields included. ─────
    for (variant, value) in [
        (
            "text",
            serde_json::json!({"type":"text","text":"héllo \u{4e16}\u{754c}"}),
        ),
        (
            "image",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"image/png"}),
        ),
        (
            "audio",
            serde_json::json!({"type":"audio","data":"AAE=","mimeType":"audio/wav"}),
        ),
        (
            "embedded text resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.txt","mimeType":"text/plain","text":"body"}}),
        ),
        (
            "embedded blob resource",
            serde_json::json!({"type":"resource","resource":{"uri":"file:///a.bin","blob":"AAE="}}),
        ),
        (
            "resource_link",
            serde_json::json!({"type":"resource_link","uri":"file:///r.pdf","name":"r", "mimeType":"application/pdf", "icons":[{"src":"https://i.example/x.png","mimeType":"image/png"}], "annotations":{"audience":["user","assistant"],"priority":0.8, "lastModified":"2026-10-18T00:00:00Z"}, "_meta":{"vendor.example/k":"v"}, "ext.example/future":{"nested":[1,true]}}),
        ),
    ] {
        let parsed = CanonicalContentBlock::from_wire(&value)
            .unwrap_or_else(|e| panic!("{variant} must validate: {e}"));
        assert_eq!(
            parsed.to_wire(),
            value,
            "{variant} must round-trip losslessly"
        );
        // The typed rmcp adapter represents every spec field.
        assert!(parsed.to_rmcp().is_ok(), "{variant} must adapt to rmcp");
    }

    // ── Canonical serialization is deterministic across key order ──
    let a = CanonicalContentBlock::from_wire(&serde_json::json!({ "type":"text","text":"t","_meta":{"z":1,"a":2},"annotations":{"priority":0.5}}))
    .unwrap()
    .canonical_json();
    let b = CanonicalContentBlock::from_wire(&serde_json::json!({ "annotations":{"priority":0.5},"_meta":{"a":2,"z":1},"text":"t","type":"text"}))
    .unwrap()
    .canonical_json();
    assert_eq!(a, b, "canonical_json must be key-order independent");

    // ── Boundaries are typed failures, never silent coercion ────────
    for (label, value, want) in [
        (
            "bad mime",
            serde_json::json!({"type":"image","data":"aGk=","mimeType":"no mime"}),
            ContentError::BadMimeType("image"),
        ),
        (
            "bad base64",
            serde_json::json!({"type":"image","data":"!!!","mimeType":"image/png"}),
            ContentError::BadBase64("image"),
        ),
        (
            "schemeless link uri",
            serde_json::json!({"type":"resource_link","uri":"x","name":"n"}),
            ContentError::BadUri("resource_link.uri"),
        ),
        (
            "priority out of range",
            serde_json::json!({"type":"text","text":"t","annotations":{"priority":1.5}}),
            ContentError::BadPriority,
        ),
        (
            "bad audience",
            serde_json::json!({"type":"text","text":"t","annotations":{"audience":["root"]}}),
            ContentError::BadAudience,
        ),
    ] {
        assert_eq!(
            CanonicalContentBlock::from_wire(&value),
            Err(want.clone()),
            "{label} must fail typed"
        );
    }

    // ── JSON Schema 2020-12 tool contracts: remote `$ref` fails
    //    closed at registration; local `#/$defs` compiles. ───────────
    let service = susi_gmcp::protocol::GmcpService::new(scratch.join("workspace"));
    let handler = || -> susi_gmcp::protocol::ToolHandler {
        std::sync::Arc::new(|_, _| {
            Box::pin(async { Err(susi_gmcp::rmcp::ErrorData::internal_error("x", None)) })
        })
    };
    let remote: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"r","inputSchema":{"$ref":"https://evil.example/x.json"}}),
    )
    .unwrap();
    assert!(
        service.register_tool(remote, handler()).is_err(),
        "a remote-$ref inputSchema must never register"
    );
    let local: susi_gmcp::rmcp::model::Tool = serde_json::from_value(
        serde_json::json!({"name":"l","inputSchema":{"$defs":{"s":{"type":"string"}},"type":"object","properties":{"v":{"$ref":"#/$defs/s"}}}}),
    )
    .unwrap();
    assert!(
        service.register_tool(local, handler()).is_ok(),
        "a local $defs ref must compile — the pinned corpus is the only resolver"
    );

    // ── The contract pins the vector's required vocabulary ──────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-008.md"))
        .expect("the vector contract must exist");
    for term in [
        "CanonicalContentBlock",
        "resource_link",
        "EmbeddedResource",
        "2020-12",
        "lossless",
        "audience",
        "priority",
        "ttlMs",
        "cacheScope",
        "extension",
        "-32602",
        "-32600",
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

    let evidence = std::fs::read_to_string(gmcp_evidence_path(root, ".agents/gmcp/evidence/VC-203-008.jsonl"))
        .expect("VC-203-008 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-008");
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
        "GMCP-203-008-01",
        "GMCP-203-008-02",
        "GMCP-203-008-03",
        "GMCP-203-008-04",
        "GMCP-203-008-05",
        "GMCP-203-008-06",
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
