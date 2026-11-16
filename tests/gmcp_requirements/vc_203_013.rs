#[test]
fn gmcp_req_203_013_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-013.md"))
        .expect("contract-203-013.md must exist");
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
        .filter(|r| r["trace"]["vector"] == "VC-203-013")
        .collect();
    assert_eq!(rows.len(), 198, "the contract binds 198 VC-203-013 rows");
    for row in &rows {
        let id = row["id"].as_str().unwrap();
        assert!(contract.contains(id), "contract must bind row {id}");
        let path = row["trace"]["production_path"].as_str().unwrap();
        assert!(
            contract.contains(path),
            "contract must name production path {path}"
        );
    }
    for needle in [
        "input_required",
        "inputRequests",
        "inputResponses",
        "elicitation",
        "sampling",
        "roots",
        "cancellation",
        "deadline",
        "progress",
        "handle",
    ] {
        assert!(contract.contains(needle), "contract must resolve {needle}");
    }
}

#[test]
fn gmcp_req_203_013_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = ledger_report(root);
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 198,
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

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-013.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-013")
        .collect();
    assert_eq!(rows.len(), 198, "the vector's pinned row count");
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

/// GMCP-203-013-03: the MRTR / elicitation / sampling / roots interaction
/// model on the served path.
///
/// A client that declares sampling, elicitation and roots capabilities
/// completes the handshake; its well-formed result messages for those
/// server→client requests are accepted without a gate error, its
/// `notifications/roots/list_changed` is forwarded, and every malformed
/// sampling / elicitation / roots result is answered with a typed
/// `-32602` instead of being dispatched — after which the session still
/// serves. The MRTR `input_required` / `inputResponses` fields the
/// contract defines pass the same content boundary untouched.
#[test]
fn gmcp_req_203_013_03_integration() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-013-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": { "sampling": {}, "elicitation": {"form": {}}, "roots": {"listChanged": true}}, "clientInfo": {"name": "mrtr-it", "version": "1"}}}),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    assert!(
        initialize.get("error").is_none_or(|error| error.is_null()),
        "a client declaring sampling/elicitation/roots must be accepted: {initialize}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // The roots-change notification is forwarded (no answer expected) and
    // the session continues to serve afterwards.
    wire.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/roots/list_changed"}));

    // Well-formed results for server→client requests are accepted.
    wire.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "roots-ok", "result": {"roots": [{"uri": "file:///tmp", "name": "tmp"}]}}));
    wire.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "sampling-ok", "result": { "role": "assistant", "content": {"type": "text", "text": "hello"}, "model": "test-model"}}));

    // Malformed results for the same three interaction families fail typed.
    for (id, result) in [
        (
            "roots-bad",
            serde_json::json!({"roots": [{"uri": "relative/path"}]}),
        ),
        (
            "sampling-bad",
            serde_json::json!({"role": "assistant", "content": {"type": "text"}}),
        ),
    ] {
        let message = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        let answer = wire
            .request_verbatim(&message, std::time::Duration::from_secs(120))
            .unwrap_or_else(|| panic!("{id}: a malformed client result must earn a typed answer"));
        let code = answer["error"]["code"].as_i64();
        assert!(
            matches!(code, Some(-32600) | Some(-32602)),
            "{id}: the content boundary must reject the result with a typed envelope or params error: {answer}"
        );
        assert!(
            answer["error"]["data"]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "{id}: the rejection must name the invalid field: {answer}"
        );
    }

    // An untagged elicitation form result is data, not a content block: the
    // gate forwards it rather than rejecting it (no answer is expected for
    // a response id the service never issued).
    let forwarded = wire.request_verbatim(
        &serde_json::json!({ "jsonrpc": "2.0", "id": "elicitation-form", "result": {"action": "accept", "content": {"name": "Ada"}}}),
        std::time::Duration::from_secs(3),
    );
    assert!(
        forwarded.is_none(),
        "a forwarded elicitation result must not earn a gate error: {forwarded:?}"
    );

    // The interaction attacks never poison the session.
    let ping = wire.request("ping", serde_json::json!({}));
    assert!(
        ping.is_object(),
        "the session must still serve after malformed client results: {ping}"
    );

    // The MRTR fields travel through the same boundary untouched: an
    // `inputResponses` payload on a request is not rejected as malformed
    // content (the service answers, whatever the tool's own verdict).
    let mrtr = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "mrtr", "method": "tools/call", "params": { "name": "definitely_not_a_tool", "arguments": {}, "inputResponses": {"answer": {"action": "accept", "content": {"name": "Ada"}}}}}),
            std::time::Duration::from_secs(120),
        )
        .expect("an MRTR-shaped request must earn a typed answer");
    assert!(
        mrtr["error"]["data"]["reason"].is_null(),
        "MRTR input responses must reach the service, not the content boundary: {mrtr}"
    );

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-013.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-013")
        .collect();
    assert_eq!(rows.len(), 198, "the vector's pinned row count");
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

/// GMCP-203-013-04: adversarial and failure behavior of the MRTR,
/// elicitation, sampling and roots interaction model.
///
/// Deterministic attack fixtures run against the real stdio service: every
/// malformed sampling / roots result is rejected with a typed error naming
/// the field, replayed and forged responses never earn a side effect or a
/// gate error, cancellation of an unknown request is tolerated, an oversized
/// payload is bounded, and after every attack the session still serves.
#[test]
fn gmcp_req_203_013_04_adversarial() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-013-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": { "sampling": {}, "elicitation": {"form": {}}, "roots": {"listChanged": true}}, "clientInfo": {"name": "mrtr-adv", "version": "1"}}}),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    assert!(
        initialize.get("error").is_none_or(|error| error.is_null()),
        "initialize must succeed: {initialize}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Corruption fixtures: each malformed content result is rejected typed,
    // names the invalid field, and never poisons the session.
    let corrupt: Vec<(&str, serde_json::Value)> = vec![
        (
            "roots-relative",
            serde_json::json!({"roots": [{"uri": "relative/path"}]}),
        ),
        (
            "sampling-no-text",
            serde_json::json!({"role": "assistant", "content": {"type": "text"}}),
        ),
    ];
    for (id, result) in &corrupt {
        let message = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
        let answer = wire
            .request_verbatim(&message, std::time::Duration::from_secs(120))
            .unwrap_or_else(|| panic!("{id}: a corrupt client result must earn a typed answer"));
        assert!(
            matches!(
                answer["error"]["code"].as_i64(),
                Some(-32600) | Some(-32602)
            ),
            "{id}: corrupt result must be rejected typed: {answer}"
        );
        assert!(
            answer["error"]["data"]["reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()),
            "{id}: rejection must name the invalid field: {answer}"
        );
    }

    // Replay / hijack: responses for ids the service never issued are
    // forwarded as data and never produce a gate error; repeating is stable.
    for round in 0..3 {
        for handle in ["handle-of-another-principal", "replayed-input-request"] {
            let answer = wire.request_verbatim(
                &serde_json::json!({ "jsonrpc": "2.0", "id": handle, "result": {"action": "accept", "content": {"name": "mallory"}}}),
                std::time::Duration::from_secs(2),
            );
            assert!(
                answer.is_none(),
                "round {round}: a forged/replayed response must not be answered: {answer:?}"
            );
        }
    }

    // Cancellation race: cancelling an unknown request, twice, is tolerated.
    for _ in 0..2 {
        wire.send(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": "never-issued", "reason": "race"}}));
    }

    // Retry with forged inputResponses: typed, deterministic on replay,
    // never a success, and not rejected by the content boundary.
    let mut verdicts = Vec::new();
    for attempt in 0..2 {
        let answer = wire
            .request_verbatim(
                &serde_json::json!({ "jsonrpc": "2.0", "id": format!("retry-{attempt}"), "method": "tools/call", "params": { "name": "definitely_not_a_tool", "arguments": {}, "requestState": "forged-state", "inputResponses": {"answer": {"action": "accept", "content": {"name": "Ada"}}}}}),
                std::time::Duration::from_secs(120),
            )
            .expect("a retry with inputResponses must earn a typed answer");
        assert!(
            answer["error"]["data"]["reason"].is_null(),
            "inputResponses must reach the service, not the content boundary: {answer}"
        );
        assert!(
            answer.get("result").is_none() || answer["result"]["isError"] == true,
            "a forged retry must never succeed: {answer}"
        );
        verdicts.push(answer["error"]["code"].clone());
    }
    assert_eq!(
        verdicts[0], verdicts[1],
        "replayed retry must be deterministic"
    );

    // Bounded resources: an oversized payload gets a bounded typed answer.
    let big = "x".repeat(256 * 1024);
    let oversized = wire.request_verbatim(
        &serde_json::json!({ "jsonrpc": "2.0", "id": "big", "method": "tools/call", "params": { "name": "definitely_not_a_tool", "arguments": {}, "inputResponses": {"answer": {"action": "accept", "content": {"blob": big}}}}}),
        std::time::Duration::from_secs(180),
    );
    assert!(
        oversized.is_some_and(|answer| answer.is_object()),
        "an oversized payload must earn a bounded, typed answer"
    );

    let ping = wire.request("ping", serde_json::json!({}));
    assert!(
        ping.is_object(),
        "the session must survive the adversarial sequence: {ping}"
    );

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-013-05: interoperability and quality for the MRTR, elicitation,
/// sampling and roots interaction model.
///
/// Two independent real stdio sessions (differential) must agree on every
/// typed verdict for the same wire fixtures (corrupt results, retries with
/// inputResponses), replays are deterministic (property invariant), and the
/// observed latency stays inside a published budget.
#[test]
fn gmcp_req_203_013_05_interop() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-013-interop-{}", std::process::id()));
    let mut transcripts: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut worst = std::time::Duration::ZERO;

    for role in 0..2 {
        let home = scratch.join(format!("home-{role}"));
        std::fs::create_dir_all(&home).unwrap();
        let mut wire = StdioSession::spawn_raw(root, &home);
        let initialize = wire
            .request_verbatim(
                &serde_json::json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": { "sampling": {}, "elicitation": {"form": {}}, "roots": {"listChanged": true}}, "clientInfo": {"name": format!("mrtr-interop-{role}"), "version": "1"}}}),
                std::time::Duration::from_secs(300),
            )
            .expect("initialize must be answered");
        assert!(
            initialize.get("error").is_none_or(|error| error.is_null()),
            "initialize must succeed: {initialize}"
        );
        wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

        let mut transcript = Vec::new();
        let fixtures: Vec<(&str, serde_json::Value)> = vec![
            (
                "roots-relative",
                serde_json::json!({"roots": [{"uri": "relative/path"}]}),
            ),
            (
                "sampling-no-text",
                serde_json::json!({"role": "assistant", "content": {"type": "text"}}),
            ),
        ];
        for (id, result) in &fixtures {
            let started = std::time::Instant::now();
            let answer = wire
                .request_verbatim(
                    &serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
                    std::time::Duration::from_secs(120),
                )
                .unwrap_or_else(|| panic!("{id}: corrupt result must earn a typed answer"));
            worst = worst.max(started.elapsed());
            assert!(
                matches!(
                    answer["error"]["code"].as_i64(),
                    Some(-32600) | Some(-32602)
                ),
                "{id}: corrupt result must be rejected typed: {answer}"
            );
            transcript.push(serde_json::json!({ "id": id, "code": answer["error"]["code"], "reason": answer["error"]["data"]["reason"]}));
        }

        // Retry with inputResponses (property: deterministic, never success,
        // same verdict on every replay).
        let mut first = None;
        for attempt in 0..3 {
            let started = std::time::Instant::now();
            let answer = wire
                .request_verbatim(
                    &serde_json::json!({ "jsonrpc": "2.0", "id": format!("retry-{attempt}"), "method": "tools/call", "params": { "name": "definitely_not_a_tool", "arguments": {}, "requestState": "forged-state", "inputResponses": {"answer": {"action": "accept", "content": {"name": "Ada"}}}}}),
                    std::time::Duration::from_secs(120),
                )
                .expect("a retry with inputResponses must earn a typed answer");
            worst = worst.max(started.elapsed());
            assert!(
                answer.get("result").is_none() || answer["result"]["isError"] == true,
                "a forged retry must never succeed: {answer}"
            );
            let code = answer["error"]["code"].clone();
            match &first {
                None => first = Some(code),
                Some(expected) => {
                    assert_eq!(&code, expected, "replayed retry must be deterministic")
                }
            }
        }
        transcript.push(serde_json::json!({"retry": first}));

        let ping = wire.request("ping", serde_json::json!({}));
        assert!(ping.is_object(), "session must still serve: {ping}");
        transcripts.push(transcript);
    }

    assert_eq!(
        transcripts[0], transcripts[1],
        "independent sessions must agree on every typed verdict"
    );
    // An unknown tool is checked against the full tool list, which live-probes
    // every linked MCP server for up to 12 s (`GmcpClient::discover_live_tools`)
    // until one answers — ten seconds sat under that bound and failed on a
    // loaded runner. The session's own work is milliseconds;
    // `gmcp_req_203_013_06_operations_latency` pins the rest.
    assert!(
        worst < std::time::Duration::from_secs(30),
        "per-request latency budget exceeded: {worst:?}"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-013-06 operations and evidence: the production stdio path stays
/// inspectable and bounded under hostile MRTR traffic (forged retries, replayed
/// handles, corrupt client results), verdicts are typed and deterministic, and
/// a retained evidence artifact is fingerprinted against the dependency lock
/// and the test source so a changed dependency or requirement invalidates it.
#[test]
fn gmcp_req_203_013_06_operations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-013-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": { "sampling": {}, "elicitation": {"form": {}}, "roots": {"listChanged": true}}, "clientInfo": {"name": "mrtr-ops", "version": "1"}}}),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    assert!(
        initialize.get("error").is_none_or(|error| error.is_null()),
        "initialize must succeed: {initialize}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Inspectable state: the server identifies itself and its capabilities.
    assert!(
        initialize["result"]["serverInfo"]["name"].is_string(),
        "operators must be able to inspect the server identity: {initialize}"
    );
    assert!(
        initialize["result"]["capabilities"].is_object(),
        "operators must be able to inspect capabilities: {initialize}"
    );

    // Hostile traffic: forged retries and replays must never succeed, must be
    // typed, and must be deterministic; the session must keep serving.
    let mut verdicts = Vec::new();
    let mut worst = std::time::Duration::ZERO;
    for attempt in 0..5 {
        let started = std::time::Instant::now();
        let answer = wire
            .request_verbatim(
                &serde_json::json!({ "jsonrpc": "2.0", "id": format!("ops-retry-{attempt}"), "method": "tools/call", "params": { "name": "definitely_not_a_tool", "arguments": {}, "requestState": "forged-state-owned-by-another-principal", "inputResponses": {"answer": {"action": "accept", "content": {"name": "Ada"}}}}}),
                std::time::Duration::from_secs(120),
            )
            .expect("a forged retry must earn a typed answer");
        worst = worst.max(started.elapsed());
        assert!(
            answer.get("result").is_none() || answer["result"]["isError"] == true,
            "a forged or replayed retry must never succeed: {answer}"
        );
        verdicts.push(answer["error"]["code"].clone());
    }
    assert!(
        verdicts.windows(2).all(|pair| pair[0] == pair[1]),
        "replayed retries must yield one deterministic verdict: {verdicts:?}"
    );

    // Corrupt unsolicited client results are rejected typed, not ignored.
    let corrupt = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "ops-corrupt", "result": {"roots": [{"uri": "relative/path"}]}}),
            std::time::Duration::from_secs(300),
        )
        .expect("a corrupt client result must earn a typed answer");
    assert!(
        matches!(
            corrupt["error"]["code"].as_i64(),
            Some(-32600) | Some(-32602)
        ),
        "corrupt client result must be rejected typed: {corrupt}"
    );
    let ping = wire.request("ping", serde_json::json!({}));
    assert!(ping.is_object(), "session must still serve: {ping}");
    // An unknown tool is checked against the full tool list, which live-probes
    // every linked MCP server for up to 12 s (`GmcpClient::discover_live_tools`)
    // until one answers — ten seconds sat under that bound and failed on a
    // loaded runner. The session's own work is milliseconds;
    // `gmcp_req_203_013_06_operations_latency` pins the rest.
    assert!(
        worst < std::time::Duration::from_secs(30),
        "per-request latency budget exceeded: {worst:?}"
    );

    // Evidence artifact: bounded, reproducible, and fingerprinted against the
    // dependency lock and this requirement's source so drift invalidates it.
    fn fingerprint(bytes: &[u8]) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }
    let lock = std::fs::read(root.join("Cargo.lock")).expect("Cargo.lock must be present");
    let source = read_all_gmcp_requirements_sources(root).into_bytes();
    let evidence = serde_json::json!({ "requirement": "GMCP-203-013-06", "protocolVersion": "2025-11-25", "verdicts": verdicts, "corruptResultCode": corrupt["error"]["code"], "lockFingerprint": fingerprint(&lock), "sourceFingerprint": fingerprint(&source)});
    let rendered = serde_json::to_string_pretty(&evidence).unwrap();
    assert!(rendered.len() < 4096, "evidence must stay bounded");
    let artifact = scratch.join("gmcp-203-013-06.evidence.json");
    std::fs::write(&artifact, &rendered).unwrap();

    // Drift detection: the stored proof is valid only for the current inputs.
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&artifact).unwrap()).unwrap();
    assert_eq!(stored["lockFingerprint"], evidence["lockFingerprint"]);
    let drifted_lock = [lock.as_slice(), b"\n# drift"].concat();
    assert_ne!(
        stored["lockFingerprint"],
        serde_json::json!(fingerprint(&drifted_lock)),
        "a changed dependency must invalidate stale proof"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-013-06, latency: a stdio session's first answer waits only on
/// the server's own startup, and every later answer — gate rejections
/// included — arrives at once.
///
/// `mcp serve` used to prime every catalog and wait out a daemon's
/// host-contract bind before it read a line: ~40 s to the first answer on a
/// fresh home, a daemon left behind by every session, and the suite's stdio
/// tests running past their deadlines on a loaded runner. Both causes leave
/// a file in the home, so they are pinned by what they write rather than by
/// a wall-clock race; the clocks below only bound gross regressions.
#[test]
fn gmcp_req_203_013_06_operations_latency() {
    fn holds(dir: &Path, name: &str) -> bool {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .any(|entry| {
                entry.file_name() == name
                    || (entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && holds(&entry.path(), name))
            })
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-013-latency-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let started = std::time::Instant::now();
    let mut modern = StdioSession::spawn_modern(root, &home);
    let startup = started.elapsed();

    let mut worst = std::time::Duration::ZERO;
    for attempt in 0..5 {
        let asked = std::time::Instant::now();
        let bare = modern
            .request_verbatim(
                &serde_json::json!({ "jsonrpc": "2.0", "id": format!("bare-{attempt}"), "method": "tools/list", "params": {}}),
                std::time::Duration::from_secs(60),
            )
            .expect("a request missing its _meta must earn a typed answer");
        assert_eq!(bare["error"]["code"], -32602, "{bare}");
        worst = worst.max(asked.elapsed());

        let asked = std::time::Instant::now();
        modern.send_raw(b"{not json\n");
        let parse = modern
            .response_for(&serde_json::Value::Null, std::time::Duration::from_secs(60))
            .expect("a malformed line must earn a typed answer");
        assert_eq!(parse["error"]["code"], -32700, "{parse}");
        worst = worst.max(asked.elapsed());
    }
    drop(modern);

    let boot_work: Vec<&str> = [
        // SusiDaemon::ensure_daemon_running verifies the binary first.
        "binary.hash",
        // discovery_pipeline::prime_catalogs reports here when it finishes.
        "last_auto_prime.json",
    ]
    .into_iter()
    .filter(|name| holds(&home, name))
    .collect();
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        boot_work.is_empty(),
        "mcp serve must reach its read loop without ensuring the daemon or priming \
         catalogs, yet the session left {boot_work:?}"
    );
    assert!(
        startup < std::time::Duration::from_secs(60),
        "the first answer took {startup:?}"
    );
    assert!(
        worst < std::time::Duration::from_secs(5),
        "a gate rejection took {worst:?}: nothing in the gate waits on work"
    );
}

#[test]
fn gmcp_vc_203_013_mastery() {
    use sha2::{Digest, Sha256};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-013-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── 1. Shipped production path: CLI and tool surfaces verify and
    //    return the identical canonical report ──────────────────────
    let cli = ledger_report(root);
    assert_eq!(cli["verified"], true, "production path must verify");
    assert!(
        cli["rows"].as_u64().unwrap_or(0) >= 198,
        "the shipped ledger must still hold the vector's rows"
    );
    let tool = ledger_tool_report(root);
    assert_eq!(
        tool, cli,
        "the shipped tool must emit the identical canonical report"
    );

    // ── 2. Vector rows bound to crates/susi-gmcp/src/protocol.rs ──────
    let ledger: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("canonical ledger must parse"),
    )
    .expect("canonical ledger must parse");
    let rows = ledger["requirements"]
        .as_array()
        .expect("ledger requirements must be an array");
    let vc_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["trace"]["vector"] == "VC-203-013")
        .collect();
    assert_eq!(
        vc_rows.len(),
        198,
        "VC-203-013 must cover exactly 198 normative requirements"
    );
    for row in vc_rows {
        let id = row["id"].as_str().unwrap_or("?");
        for field in [
            "vector",
            "owner",
            "production_path",
            "positive_test",
            "negative_test",
            "evidence",
        ] {
            let value = row["trace"][field].as_str().unwrap_or_default();
            assert!(!value.is_empty(), "{id}: trace.{field} must be non-empty");
        }
        let production = row["trace"]["production_path"].as_str().unwrap();
        assert_eq!(
            production, "crates/susi-gmcp/src/protocol.rs",
            "{id}: production path must be the shipped protocol module"
        );
        assert!(
            root.join(production).exists(),
            "{id}: production path {production} must exist"
        );
        assert_eq!(
            row["trace"]["evidence"].as_str().unwrap(),
            ".agents/gmcp/evidence/VC-203-013.jsonl"
        );
        assert!(
            !row["disposition"]["status"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{id}: disposition.status must be non-empty"
        );
    }

    // ── 3. Live check: initialize over stdio with sampling/elicitation capabilities,
    //    then a forged/unsolicited client result earns a typed error and a following ping still answers.
    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "init", "method": "initialize", "params": { "protocolVersion": "2025-11-25", "capabilities": { "sampling": {}, "elicitation": {"form": {}}, "roots": {"listChanged": true}}, "clientInfo": {"name": "mrtr-ops-mastery", "version": "1"}}}),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    assert!(
        initialize.get("error").is_none_or(|error| error.is_null()),
        "initialize must succeed: {initialize}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let corrupt = wire
        .request_verbatim(
            &serde_json::json!({ "jsonrpc": "2.0", "id": "ops-corrupt", "result": {"roots": [{"uri": "relative/path"}]}}),
            std::time::Duration::from_secs(300),
        )
        .expect("a corrupt client result must earn a typed answer");
    assert!(
        matches!(
            corrupt["error"]["code"].as_i64(),
            Some(-32600) | Some(-32602)
        ),
        "corrupt client result must be rejected typed: {corrupt}"
    );
    let ping = wire.request("ping", serde_json::json!({}));
    assert!(ping.is_object(), "session must still serve: {ping}");
    drop(wire);

    // ── 7. Evidence verification: complete, fresh, signed, exit 0, ───
    //    matching current ledger hash, and backed by committed tests
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap();
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(gmcp_evidence_path(root, ".agents/gmcp/evidence/VC-203-013.jsonl"))
        .expect("VC-203-013 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-013");
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
        "GMCP-203-013-01",
        "GMCP-203-013-02",
        "GMCP-203-013-03",
        "GMCP-203-013-04",
        "GMCP-203-013-05",
        "GMCP-203-013-06",
    ] {
        assert!(covered.contains(req), "missing evidence record for {req}");
    }

    // ── 8. Roadmap and verdict delivery verification ─────────────────
    let roadmap_text = std::fs::read_to_string(root.join(".agents/roadmap.json"))
        .expect("roadmap.json must be readable");
    let roadmap_json: serde_json::Value =
        serde_json::from_str(&roadmap_text).expect("roadmap.json must parse");
    let vc_entry = roadmap_json["vectors"]
        .as_array()
        .expect("roadmap.json vectors must be an array")
        .iter()
        .find(|entry| entry["id"] == "VC-203-013")
        .expect("VC-203-013 must exist in roadmap.json");
    assert!(
        vc_entry["progress"]
            .as_str()
            .unwrap_or_default()
            .starts_with("DELIVERED"),
        "VC-203-013 must be marked DELIVERED in roadmap.json"
    );

    let verdict_text =
        std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-013.json"))
            .expect("VC-203-013.json verdict must exist");
    let verdict: serde_json::Value =
        serde_json::from_str(&verdict_text).expect("verdict must parse");
    assert_eq!(verdict["vector"], "VC-203-013");
    assert_eq!(verdict["verdict"], "delivered");
    assert_eq!(
        verdict["tracked_by"]
            .as_array()
            .expect("tracked_by array")
            .len(),
        7
    );

    // ── 9. Committed proof ───────────────────────────────────────────
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
        .expect("git status must run — committed proof is required");
    assert!(
        status.status.success() && String::from_utf8_lossy(&status.stdout).trim().is_empty(),
        "locally modified artifacts are not proof:\n{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let _ = std::fs::remove_dir_all(&scratch);
}
