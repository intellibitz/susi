#[test]
fn gmcp_req_203_010_01_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-010.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-010")
        .collect();
    assert_eq!(rows.len(), 33, "the contract binds 33 VC-203-010 rows");
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
fn gmcp_req_203_010_02_production() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let report = ledger_report(root);
    assert_eq!(report["verified"], true);
    assert!(
        report["rows"].as_u64().unwrap_or(0) >= 33,
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

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-010.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-010")
        .collect();
    assert_eq!(rows.len(), 33, "the vector's pinned row count");
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

/// GMCP-203-010-03: the complete resource server surface on the served path.
///
/// The `resources` capability is declared (`resources-mdx-0032`), static
/// and templated discovery are deterministic, both resource reads return
/// parsed JSON payloads, URI completion resolves a template argument, and
/// every invalid URI, unknown resource and failed read earns a typed error
/// instead of a silent fallback (`-0386/0409`).
#[test]
fn gmcp_req_203_010_03_integration() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-010-int-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "init", "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "resources-it", "version": "1"}
                }
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    assert!(
        initialize["result"]["capabilities"]["resources"].is_object(),
        "a server that supports resources must declare the capability: {initialize}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Static discovery.
    let listed = wire.request("resources/list", serde_json::json!({}));
    let resources = listed["resources"]
        .as_array()
        .expect("resources/list must return a resources array");
    let tools_resource = resources
        .iter()
        .find(|resource| resource["uri"] == "susi://tools")
        .expect("the tool catalog resource must be discoverable");
    assert_eq!(
        tools_resource["mimeType"], "application/json",
        "resources must declare their MIME type: {tools_resource}"
    );

    // Templated discovery.
    let templates = wire.request("resources/templates/list", serde_json::json!({}));
    let templates = templates["resourceTemplates"]
        .as_array()
        .expect("resources/templates/list must return resourceTemplates");
    assert!(
        templates
            .iter()
            .any(|template| template["uriTemplate"] == "susi://tools/{name}"),
        "the tool template must be discoverable: {templates:?}"
    );

    // Reads: the static catalog and a templated instance both return JSON.
    let catalog = wire.request("resources/read", serde_json::json!({"uri": "susi://tools"}));
    let text = catalog["contents"][0]["text"]
        .as_str()
        .expect("a resource read must carry text content");
    let tools: serde_json::Value =
        serde_json::from_str(text).expect("the tool catalog must be JSON");
    assert!(
        tools.as_array().is_some_and(|list| !list.is_empty()),
        "the tool catalog resource must list tools"
    );

    let instance = wire.request(
        "resources/read",
        serde_json::json!({"uri": "susi://tools/ledger_verify"}),
    );
    let text = instance["contents"][0]["text"]
        .as_str()
        .expect("a templated resource read must carry text content");
    let tool: serde_json::Value =
        serde_json::from_str(text).expect("the templated resource must be JSON");
    assert_eq!(
        tool["name"], "ledger_verify",
        "the template must resolve the requested tool: {tool}"
    );

    // URI completion resolves the template argument.
    let completion = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "complete", "method": "completion/complete",
                "params": {
                    "ref": {"type": "ref/resource", "uri": "susi://tools/{name}"},
                    "argument": {"name": "name", "value": "ledger"}
                }
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("completion must earn a typed answer");
    assert!(
        completion.get("error").is_none_or(|error| error.is_null()),
        "template completion must succeed: {completion}"
    );
    assert!(
        completion["result"]["completion"]["values"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value == "ledger_verify")),
        "completion must offer the matching tool name: {completion}"
    );

    // Invalid and unknown URIs fail closed.
    let unknown = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "unknown-resource", "method": "resources/read",
                "params": {"uri": "susi://tools/definitely_not_a_tool"}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("an unknown resource must earn a typed answer");
    assert!(
        unknown.get("error").is_some_and(|error| !error.is_null()),
        "an unknown resource must be refused: {unknown}"
    );
    let malformed = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "malformed-uri", "method": "resources/read",
                "params": {"uri": "not a uri"}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("a malformed URI must earn a typed answer");
    assert!(
        malformed.get("error").is_some_and(|error| !error.is_null()),
        "an invalid resource URI must be validated: {malformed}"
    );

    // Legacy subscriptions round-trip on the negotiated-legacy session.
    let subscribe = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "subscribe", "method": "resources/subscribe",
                "params": {"uri": "susi://tools"}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("subscribe must answer");
    assert!(
        subscribe.get("error").is_none_or(|error| error.is_null()),
        "a known resource must be subscribable: {subscribe}"
    );
    let unsubscribe = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "unsubscribe", "method": "resources/unsubscribe",
                "params": {"uri": "susi://tools"}
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("unsubscribe must answer");
    assert!(
        unsubscribe.get("error").is_none_or(|error| error.is_null()),
        "a subscription must be removable: {unsubscribe}"
    );

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);

    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-010.md"))
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
        .filter(|r| r["trace"]["vector"] == "VC-203-010")
        .collect();
    assert_eq!(rows.len(), 33, "the vector's pinned row count");
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

/// GMCP-203-010-04: adversarial and failure behavior of the resource
/// server surface on the served path.
///
/// Traversal, malformed, oversized, wrongly typed and unknown requests all
/// earn typed errors, repeated reads stay deterministic, and the session
/// recovers after every refused request (no state is poisoned).
#[test]
fn gmcp_req_203_010_04_adversarial() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-010-adv-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "init", "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "resources-adv", "version": "1"}
                }
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    assert!(initialize["result"]["capabilities"]["resources"].is_object());
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let huge = format!("susi://tools/{}", "a".repeat(100_000));
    let hostile_reads: Vec<(&str, serde_json::Value)> = vec![
        (
            "traversal",
            serde_json::json!("susi://tools/../../etc/passwd"),
        ),
        (
            "encoded-traversal",
            serde_json::json!("susi://tools/..%2f..%2fsecret"),
        ),
        ("file-scheme", serde_json::json!("file:///etc/passwd")),
        (
            "windows-path",
            serde_json::json!("susi://tools/..\\..\\windows\\win.ini"),
        ),
        (
            "nul-byte",
            serde_json::json!("susi://tools/ledger_verify\u{0}"),
        ),
        ("empty", serde_json::json!("")),
        ("oversized", serde_json::json!(huge)),
        ("unknown-host", serde_json::json!("susi://nope")),
        ("number-uri", serde_json::json!(42)),
        ("null-uri", serde_json::Value::Null),
        ("object-uri", serde_json::json!({"uri": "susi://tools"})),
    ];
    let mut read_baseline: Option<String> = None;
    for (index, (label, uri)) in hostile_reads.iter().enumerate() {
        let answer = wire
            .request_verbatim(
                &serde_json::json!({
                    "jsonrpc": "2.0", "id": format!("hostile-{index}"), "method": "resources/read",
                    "params": {"uri": uri}
                }),
                std::time::Duration::from_secs(180),
            )
            .unwrap_or_else(|| panic!("hostile read {label} must earn a typed answer"));
        assert!(
            answer.get("error").is_some_and(|error| !error.is_null()),
            "hostile read {label} must be refused: {answer}"
        );
        assert!(
            answer.get("result").is_none_or(|result| result.is_null()),
            "hostile read {label} must not leak content: {answer}"
        );
        let rendered = answer.to_string();
        assert!(
            !rendered.contains("root:x:") && !rendered.contains("[fonts]"),
            "hostile read {label} must not disclose host files"
        );

        // Recovery: the session still serves a valid read, identically.
        let good = wire.request("resources/read", serde_json::json!({"uri": "susi://tools"}));
        let text = good["contents"][0]["text"]
            .as_str()
            .expect("the session must recover after a refused read")
            .to_string();
        match &read_baseline {
            None => read_baseline = Some(text),
            Some(first) => assert_eq!(first, &text, "reads must stay deterministic after {label}"),
        }
    }

    // Subscribe to unknown / malformed resources fails closed.
    for (index, uri) in [
        "susi://tools/definitely_not_a_tool",
        "not a uri",
        "susi://tools/../x",
    ]
    .iter()
    .enumerate()
    {
        let answer = wire
            .request_verbatim(
                &serde_json::json!({
                    "jsonrpc": "2.0", "id": format!("sub-{index}"), "method": "resources/subscribe",
                    "params": {"uri": uri}
                }),
                std::time::Duration::from_secs(180),
            )
            .expect("hostile subscribe must answer");
        assert!(
            answer.get("error").is_some_and(|error| !error.is_null()),
            "subscribing to {uri} must be refused: {answer}"
        );
    }

    // Completion with a wrong reference or argument never fabricates values.
    let completion = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "complete-miss", "method": "completion/complete",
                "params": {
                    "ref": {"type": "ref/resource", "uri": "susi://tools/{name}"},
                    "argument": {"name": "name", "value": "zzz-no-such-tool-zzz"}
                }
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("a missing completion must answer");
    if completion.get("error").is_none_or(|error| error.is_null()) {
        assert!(
            completion["result"]["completion"]["values"]
                .as_array()
                .is_some_and(|values| values.is_empty()),
            "no tool matches, so none may be offered: {completion}"
        );
    }
    let bad_ref = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "complete-bad-ref", "method": "completion/complete",
                "params": {
                    "ref": {"type": "ref/resource", "uri": "susi://nope/{x}"},
                    "argument": {"name": "x", "value": ""}
                }
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("an unknown template completion must answer");
    assert!(
        bad_ref.get("error").is_some_and(|e| !e.is_null())
            || bad_ref["result"]["completion"]["values"]
                .as_array()
                .is_some_and(|values| values.is_empty()),
        "an unknown template must not complete: {bad_ref}"
    );

    // Missing params earn a typed error and the templated read still works.
    let no_params = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "no-params", "method": "resources/read"
            }),
            std::time::Duration::from_secs(180),
        )
        .expect("missing params must answer");
    assert!(no_params.get("error").is_some_and(|error| !error.is_null()));
    let instance = wire.request(
        "resources/read",
        serde_json::json!({"uri": "susi://tools/ledger_verify"}),
    );
    assert!(instance["contents"][0]["text"].is_string());

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-010-05: interoperability and quality of the resource server
/// surface on the served stdio path.
///
/// A reference-style client drives discovery, templates, completion, reads,
/// subscriptions and negative cases; repeated runs are differentially equal
/// (determinism invariant), refusals recover, and latency stays inside a
/// published budget.
#[test]
fn gmcp_req_203_010_05_interop() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-010-interop-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let mut wire = StdioSession::spawn_raw(root, &home);
    let started = std::time::Instant::now();
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "init", "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "resources-interop", "version": "1"}
                }
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    let startup = started.elapsed();
    assert!(initialize["result"]["capabilities"]["resources"].is_object());
    assert!(
        startup < std::time::Duration::from_secs(300),
        "startup budget exceeded: {startup:?}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Discovery: static list and templates are deterministic.
    let list_a = wire.request("resources/list", serde_json::json!({}));
    let list_b = wire.request("resources/list", serde_json::json!({}));
    assert_eq!(list_a, list_b, "resource listing must be deterministic");
    let resources = list_a["resources"].as_array().expect("resources array");
    assert!(!resources.is_empty());
    for resource in resources {
        assert!(resource["uri"].is_string() && resource["name"].is_string());
    }
    let templates_a = wire.request("resources/templates/list", serde_json::json!({}));
    let templates_b = wire.request("resources/templates/list", serde_json::json!({}));
    assert_eq!(templates_a, templates_b);
    assert!(templates_a["resourceTemplates"]
        .as_array()
        .is_some_and(|t| t.iter().all(|t| t["uriTemplate"].is_string())));

    // Reads: every listed resource reads with its own URI and a mime type
    // inside the published latency budget; repeated reads are identical.
    let mut slowest = std::time::Duration::ZERO;
    for resource in resources.iter().take(8) {
        let uri = resource["uri"].as_str().unwrap();
        let t0 = std::time::Instant::now();
        let first = wire.request("resources/read", serde_json::json!({"uri": uri}));
        slowest = slowest.max(t0.elapsed());
        let second = wire.request("resources/read", serde_json::json!({"uri": uri}));
        assert_eq!(first, second, "read of {uri} must be deterministic");
        let content = &first["contents"][0];
        assert_eq!(content["uri"].as_str(), Some(uri));
        assert!(content["text"].is_string() || content["blob"].is_string());
    }
    assert!(
        slowest < std::time::Duration::from_secs(120),
        "read latency budget exceeded: {slowest:?}"
    );

    // Templated instance and completion agree.
    let instance = wire.request(
        "resources/read",
        serde_json::json!({"uri": "susi://tools/ledger_verify"}),
    );
    assert!(instance["contents"][0]["text"].is_string());
    let completion = wire.request(
        "completion/complete",
        serde_json::json!({
            "ref": {"type": "ref/resource", "uri": "susi://tools/{name}"},
            "argument": {"name": "name", "value": "ledger"}
        }),
    );
    assert!(completion["completion"]["values"].is_array());

    // Negative fixtures: refused with typed errors, then the session recovers.
    for (index, uri) in [
        "susi://tools/../../etc/passwd",
        "file:///etc/passwd",
        "susi://nope",
    ]
    .iter()
    .enumerate()
    {
        let answer = wire
            .request_verbatim(
                &serde_json::json!({
                    "jsonrpc": "2.0", "id": format!("neg-{index}"), "method": "resources/read",
                    "params": {"uri": uri}
                }),
                std::time::Duration::from_secs(180),
            )
            .expect("negative read must answer");
        assert!(
            answer.get("error").is_some_and(|e| !e.is_null()),
            "{uri}: {answer}"
        );
    }
    let after = wire.request(
        "resources/read",
        serde_json::json!({"uri": "susi://tools/ledger_verify"}),
    );
    assert_eq!(after, instance, "session must recover identically");

    drop(wire);

    // Differential: a second fresh session yields the same listing.
    let home2 = scratch.join("home2");
    std::fs::create_dir_all(&home2).unwrap();
    let mut other = StdioSession::spawn_raw(root, &home2);
    other
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "init", "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "resources-interop-2", "version": "1"}
                }
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("second initialize must be answered");
    other.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let list_c = other.request("resources/list", serde_json::json!({}));
    assert_eq!(list_a, list_c, "fresh sessions must list identically");
    drop(other);
    let _ = std::fs::remove_dir_all(&scratch);
}

/// GMCP-203-010-06: operations and evidence for the resource server surface.
///
/// The retained evidence artifact is reproducible, the served stdio path
/// exposes inspectable state, resources read back within bounded content, and
/// the evidence is pinned to the vendored standards corpus so a changed
/// dependency or standard invalidates stale proof and fails closed.
#[test]
fn gmcp_req_203_010_06_operations() {
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

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-010-06-ops-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── Retained evidence artifact: the pinned ledger report is
    //    reproducible, complete and schema-valid ─────────────────────
    let ledger_a = ledger_report(root);
    let ledger_b = ledger_report(root);
    assert_eq!(ledger_a, ledger_b, "evidence report must be reproducible");
    assert_eq!(
        ledger_a["verified"], true,
        "retained evidence must verify: {ledger_a}"
    );
    assert!(
        ledger_a["rows"].as_u64().unwrap_or(0) > 0,
        "the evidence artifact must classify its rows: {ledger_a}"
    );
    assert!(
        ledger_a["revisions"]["2026-07-28"].as_u64().unwrap_or(0) > 0
            && ledger_a["revisions"]["2025-11-25"].as_u64().unwrap_or(0) > 0,
        "the evidence artifact must cover both revisions: {ledger_a}"
    );

    // ── The proof is pinned to the vendored standards corpus ──
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/sources.json")).expect("sources manifest"),
    )
    .expect("sources manifest must be JSON");
    let documents: Vec<String> = manifest["files"]
        .as_array()
        .expect("pinned files")
        .iter()
        .map(|file| file["document"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(!documents.is_empty(), "the corpus pin must be non-empty");

    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let refetched_arg = refetched.to_str().expect("temp path must be unicode");
    for document in &documents {
        assert!(
            refetched.join(document).is_file(),
            "pinned document {document} must be present in the vendored corpus"
        );
    }

    // ── Drift detection: a pristine corpus matches every pin, and the
    //    pass is deterministic ──────────────────────────────────────
    let pristine = susi_json(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    );
    assert_eq!(
        pristine["verified"], true,
        "a pristine corpus must match every pin: {pristine}"
    );
    assert!(
        pristine["findings"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "a pristine corpus must produce no findings: {pristine}"
    );
    let checked = pristine["checked"].as_u64().unwrap_or(0);
    assert!(
        checked > 0 && pristine["matched"].as_u64() == Some(checked),
        "every pin must match: {pristine}"
    );
    let pristine_again = susi_json(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    );
    assert_eq!(
        pristine, pristine_again,
        "drift evidence must be reproducible"
    );

    // ── A changed standard invalidates the stale proof ──
    let target = documents
        .iter()
        .find(|document| document.ends_with("server/resources.mdx"))
        .expect("the resources standard must be pinned")
        .clone();
    let target_path = refetched.join(&target);
    let original = std::fs::read(&target_path).expect("pinned document bytes");
    let mut changed = original.clone();
    changed.extend_from_slice(b"\n<!-- upstream revision -->\n");
    std::fs::write(&target_path, &changed).unwrap();

    let drifted = susi_command(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    )
    .output()
    .expect("the drift check must run");
    assert!(
        !drifted.status.success(),
        "a changed standard must fail the drift check closed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&drifted.stdout),
        String::from_utf8_lossy(&drifted.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&drifted.stdout)
        .expect("the drift check must still print its JSON diagnostics");
    assert_eq!(report["verified"], false, "stale proof must not verify");
    let findings = report["findings"].as_array().expect("findings array");
    assert_eq!(
        findings.len(),
        1,
        "only the changed document may be reported: {report}"
    );
    assert_eq!(findings[0]["document"].as_str(), Some(target.as_str()));
    assert_eq!(findings[0]["kind"].as_str(), Some("hash_drift"));
    assert_eq!(
        report["matched"].as_u64(),
        Some(checked - 1),
        "the untouched pins must still match: {report}"
    );

    // ── Rollback guidance: restoring the pinned bytes restores the proof ──
    std::fs::write(&target_path, &original).unwrap();
    let rolled_back = susi_json(
        root,
        &home,
        &["mcp", "ledger", "--drift", "--refetched", refetched_arg],
    );
    assert_eq!(
        rolled_back, pristine,
        "rollback to the pinned corpus must restore the identical proof"
    );

    // ── Diagnostics: an unreachable corpus fails closed, bounded ──
    let absent = scratch.join("absent-corpus");
    let missing = susi_command(
        root,
        &home,
        &[
            "mcp",
            "ledger",
            "--drift",
            "--refetched",
            absent.to_str().unwrap(),
        ],
    )
    .output()
    .expect("the drift check must run");
    assert!(
        !missing.status.success(),
        "an unreachable corpus must fail closed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&missing.stdout),
        String::from_utf8_lossy(&missing.stderr)
    );
    let missing_report: serde_json::Value = serde_json::from_slice(&missing.stdout)
        .expect("a failing drift check must still print diagnostics");
    assert_eq!(missing_report["verified"], false);
    let missing_findings = missing_report["findings"]
        .as_array()
        .expect("findings array");
    assert_eq!(
        missing_findings.len() as u64,
        checked,
        "every pin must be reported unfetchable: {missing_report}"
    );
    assert!(
        missing_findings
            .iter()
            .all(|finding| finding["kind"].as_str() == Some("unfetchable")),
        "unreachable pins must be classified unfetchable: {missing_report}"
    );

    // ── Inspectable API state on the served stdio path ──
    let mut wire = StdioSession::spawn_raw(root, &home);
    let initialize = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "init", "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "resources-ops", "version": "1"}
                }
            }),
            std::time::Duration::from_secs(300),
        )
        .expect("initialize must be answered");
    assert!(initialize["result"]["capabilities"]["resources"].is_object());
    assert_ne!(
        initialize["result"]["capabilities"]["resources"]["subscribe"],
        serde_json::json!(false),
        "the served surface must advertise resource subscriptions: {initialize}"
    );
    wire.send(&serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    let list_a = wire.request("resources/list", serde_json::json!({}));
    let list_b = wire.request("resources/list", serde_json::json!({}));
    assert_eq!(list_a, list_b, "API state inspection must be deterministic");
    let resources = list_a["resources"].as_array().expect("resources array");
    assert!(!resources.is_empty(), "API state must be inspectable");

    // Bounded reads: every resource answers for its own URI inside the
    // published content bound.
    for resource in resources.iter().take(4) {
        let uri = resource["uri"].as_str().unwrap();
        let read = wire.request("resources/read", serde_json::json!({"uri": uri}));
        let content = &read["contents"][0];
        assert_eq!(content["uri"].as_str(), Some(uri), "read of {uri}");
        assert!(
            content["text"].is_string() || content["blob"].is_string(),
            "read of {uri} must carry text or blob content"
        );
        assert!(
            content["text"]
                .as_str()
                .map_or(0, str::len)
                .max(content["blob"].as_str().map_or(0, str::len))
                <= 8 * 1024 * 1024,
            "resource {uri} must stay inside the bounded streaming budget"
        );
    }

    // Update notifications: subscription on a listed resource is
    // acknowledged and cleanly released.
    let subscribed_uri = "susi://tools";
    let subscribe = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "sub", "method": "resources/subscribe",
                "params": {"uri": subscribed_uri}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("subscribe must answer");
    assert!(
        subscribe.get("error").is_none_or(|error| error.is_null()),
        "subscribe must be acknowledged: {subscribe}"
    );
    let unsubscribe = wire
        .request_verbatim(
            &serde_json::json!({
                "jsonrpc": "2.0", "id": "unsub", "method": "resources/unsubscribe",
                "params": {"uri": subscribed_uri}
            }),
            std::time::Duration::from_secs(120),
        )
        .expect("unsubscribe must answer");
    assert!(
        unsubscribe.get("error").is_none_or(|error| error.is_null()),
        "unsubscribe must be acknowledged: {unsubscribe}"
    );

    // Diagnostics: refusals are typed and the session recovers.
    for (index, uri) in ["susi://tools/../../etc/passwd", "susi://missing-resource"]
        .iter()
        .enumerate()
    {
        let answer = wire
            .request_verbatim(
                &serde_json::json!({
                    "jsonrpc": "2.0", "id": format!("ops-neg-{index}"),
                    "method": "resources/read", "params": {"uri": uri}
                }),
                std::time::Duration::from_secs(180),
            )
            .expect("diagnostic read must answer");
        let error = answer
            .get("error")
            .filter(|error| !error.is_null())
            .unwrap_or_else(|| panic!("{uri} must be refused with a typed error: {answer}"));
        assert!(
            error["code"].is_number() && error["message"].is_string(),
            "refusal of {uri} must be typed: {answer}"
        );
    }
    let recovered = wire.request("resources/read", serde_json::json!({"uri": "susi://tools"}));
    assert!(
        recovered["contents"][0]["text"].is_string(),
        "session must recover after refusals: {recovered}"
    );
    drop(wire);

    let _ = std::fs::remove_dir_all(&scratch);
}

#[test]
fn gmcp_vc_203_010_mastery() {
    use sha2::{Digest, Sha256};

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-010-mastery-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    // ── 1. Shipped production path: CLI and tool surfaces verify and
    //    return the identical canonical report ──────────────────────
    let cli = ledger_report(root);
    assert_eq!(cli["verified"], true, "production path must verify");
    assert!(
        cli["rows"].as_u64().unwrap_or(0) >= 33,
        "the shipped ledger must still hold the vector's rows"
    );
    let tool = ledger_tool_report(root);
    assert_eq!(
        tool, cli,
        "the shipped tool must emit the identical canonical report"
    );

    // ── 2. Vector rows bound to crates/susi-gmcp/src/protocol.rs ─
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
        .filter(|row| row["trace"]["vector"] == "VC-203-010")
        .collect();
    assert_eq!(
        vc_rows.len(),
        33,
        "VC-203-010 must cover exactly 33 normative requirements"
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
            ".agents/gmcp/evidence/VC-203-010.jsonl"
        );
        assert!(
            !row["disposition"]["status"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "{id}: disposition.status must be non-empty"
        );
    }

    // ── 3. Live resource surface on the served stdio transport: static
    //    and templated discovery, text reads, and typed refusal ───────
    let mut wire = StdioSession::spawn(root, &home);
    let listed = wire.request("resources/list", serde_json::json!({}));
    let resources = listed["resources"]
        .as_array()
        .expect("resources/list must return a resources array");
    let catalog_entry = resources
        .iter()
        .find(|resource| resource["uri"] == "susi://tools")
        .expect("the tool catalog resource must be discoverable");
    assert_eq!(
        catalog_entry["mimeType"], "application/json",
        "resources must declare their MIME type: {catalog_entry}"
    );
    let templates = wire.request("resources/templates/list", serde_json::json!({}));
    assert!(
        templates["resourceTemplates"]
            .as_array()
            .is_some_and(|list| list
                .iter()
                .any(|template| template["uriTemplate"] == "susi://tools/{name}")),
        "the tool template must be discoverable: {templates}"
    );
    let instance = wire.request(
        "resources/read",
        serde_json::json!({"uri": "susi://tools/ledger_verify"}),
    );
    let text = instance["contents"][0]["text"]
        .as_str()
        .expect("a templated resource read must carry text content");
    let tool: serde_json::Value =
        serde_json::from_str(text).expect("the templated resource must be JSON");
    assert_eq!(
        tool["name"], "ledger_verify",
        "the template must resolve the requested tool: {tool}"
    );
    for (id, uri) in [
        ("unknown-resource", "susi://tools/susi_no_such_resource_010"),
        ("traversal", "susi://tools/../../etc/passwd"),
        ("encoded-traversal", "susi://tools/%2e%2e%2fsecrets"),
        ("not-a-uri", "not a uri"),
    ] {
        wire.send(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "resources/read",
            "params": {"uri": uri}
        }));
        let refused = wire
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(300))
            .expect("a refused read must still be answered on the served surface");
        assert!(
            refused.get("error").is_some_and(|e| !e.is_null()),
            "{uri} must earn a typed error: {refused}"
        );
        assert!(
            refused.get("result").is_none_or(|r| r.is_null()),
            "{uri} must never be served: {refused}"
        );
    }
    drop(wire);

    // ── 4. The production resource path itself (GmcpService, the service
    //    both entry points serve): admission, the access guard and the
    //    sealed, bounded read hold for any registered provider ────────
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        use susi_gmcp::mcp::model::{ErrorData, ReadResourceResult, Resource};
        use susi_gmcp::mcp::ServiceExt;
        use susi_gmcp::protocol::{GmcpService, MAX_RESOURCE_BYTES};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

        let calls = Arc::new(AtomicUsize::new(0));
        let service = GmcpService::new(root.to_path_buf());
        let register = |uri: &str, contents: serde_json::Value| {
            let calls = calls.clone();
            service.register_resource(
                Resource::new(uri, "mastery"),
                Arc::new(move |_, _| {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let contents = contents.clone();
                    Box::pin(async move {
                        let result: ReadResourceResult =
                            serde_json::from_value(serde_json::json!({ "contents": contents }))
                                .expect("fixture contents");
                        Ok(result.into())
                    })
                }),
            );
        };
        register(
            "m010://blob",
            serde_json::json!([{"uri":"m010://blob","mimeType":"image/png","blob":"iVBORw0KGgo="}]),
        );
        register(
            "m010://bad-blob",
            serde_json::json!([{"uri":"m010://bad-blob","blob":"*not-base64*"}]),
        );
        register("m010://empty", serde_json::json!([]));
        register(
            "m010://huge",
            serde_json::json!([{"uri":"m010://huge","text":"z".repeat(MAX_RESOURCE_BYTES + 1)}]),
        );
        register(
            "sealed010://doc",
            serde_json::json!([{"uri":"sealed010://doc","text":"classified"}]),
        );
        service.set_resource_guard(Arc::new(|uri| {
            if uri.starts_with("sealed010://") {
                return Err(ErrorData::invalid_request("Resource access denied", None));
            }
            Ok(())
        }));

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        runtime.block_on(async {
            let (client, server) = tokio::io::duplex(1 << 20);
            let serving = service.clone();
            tokio::spawn(async move {
                if let Ok(running) = serving.serve(server).await {
                    let _ = running.waiting().await;
                }
            });
            let mut wire = tokio::io::BufReader::new(client);
            let mut ask = async |id: i64, method: &str, mut params: serde_json::Value| {
                params["_meta"] = serde_json::json!({
                    "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                    "io.modelcontextprotocol/clientCapabilities": {}
                });
                let request = serde_json::json!({
                    "jsonrpc": "2.0", "id": id, "method": method, "params": params
                });
                wire.get_mut()
                    .write_all(format!("{request}\n").as_bytes())
                    .await
                    .expect("request written");
                let mut line = String::new();
                tokio::time::timeout(
                    std::time::Duration::from_secs(60),
                    wire.read_line(&mut line),
                )
                .await
                .expect("the service must answer within the bound")
                .expect("response read");
                serde_json::from_str::<serde_json::Value>(&line).expect("response is JSON")
            };
            let discovered = ask(1, "server/discover", serde_json::json!({})).await;
            assert!(discovered.get("error").is_none(), "{discovered}");

            // Binary content is served as strict base64 with its MIME type
            // and cache hints.
            let blob = ask(
                2,
                "resources/read",
                serde_json::json!({"uri":"m010://blob"}),
            )
            .await;
            assert_eq!(
                blob["result"]["contents"][0]["blob"], "iVBORw0KGgo=",
                "{blob}"
            );
            assert_eq!(blob["result"]["contents"][0]["mimeType"], "image/png");
            assert_eq!(blob["result"]["cacheScope"], "private");
            assert_eq!(blob["result"]["ttlMs"], 0);
            let served = calls.load(Ordering::SeqCst);

            // Malformed binary, an empty contents array and an oversized
            // read are internal errors, never results.
            for (id, uri) in [
                (3, "m010://bad-blob"),
                (4, "m010://empty"),
                (5, "m010://huge"),
            ] {
                let refused = ask(id, "resources/read", serde_json::json!({ "uri": uri })).await;
                assert_eq!(refused["error"]["code"], -32603, "{uri}: {refused}");
                assert!(refused.get("result").is_none(), "{uri} served: {refused}");
            }

            // A missing resource is an error naming the URI, never empty.
            let missing = ask(
                6,
                "resources/read",
                serde_json::json!({"uri":"m010://absent"}),
            )
            .await;
            assert!(missing.get("result").is_none(), "{missing}");
            assert_eq!(
                missing["error"]["data"]["uri"], "m010://absent",
                "{missing}"
            );

            // Hostile URIs and a guarded resource are refused before any
            // provider runs.
            let before = calls.load(Ordering::SeqCst);
            assert!(before > served, "the refused providers above did run");
            for (id, uri) in [
                (7, "m010://a/../blob"),
                (8, "m010://a/%2E%2e/blob"),
                (9, "m010://a\\..\\blob"),
                (10, "m010://white space"),
                (11, "no-scheme"),
            ] {
                let refused = ask(id, "resources/read", serde_json::json!({ "uri": uri })).await;
                assert_eq!(refused["error"]["code"], -32602, "{uri}: {refused}");
            }
            let denied = ask(
                12,
                "resources/read",
                serde_json::json!({"uri":"sealed010://doc"}),
            )
            .await;
            assert_eq!(
                denied["error"]["message"], "Resource access denied",
                "{denied}"
            );
            let denied = ask(
                13,
                "resources/subscribe",
                serde_json::json!({"uri":"sealed010://doc"}),
            )
            .await;
            assert!(
                denied.get("error").is_some_and(|e| !e.is_null()),
                "{denied}"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                before,
                "refusal and denial must precede every provider"
            );

            // The session survives every refusal.
            let again = ask(
                14,
                "resources/read",
                serde_json::json!({"uri":"m010://blob"}),
            )
            .await;
            assert_eq!(
                again["result"]["contents"][0]["blob"], "iVBORw0KGgo=",
                "{again}"
            );
        });
    }

    // ── 5. The contract states the behavior proven above ─────────────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-010.md"))
        .expect("contract document must exist");
    for stated in [
        "## Resource surface behavior",
        "MAX_RESOURCE_BYTES",
        "set_resource_guard",
        "percent-encoded",
        "strict base64",
        "crates/susi-gmcp/src/tests/resource_surface.rs",
    ] {
        assert!(contract.contains(stated), "contract must state: {stated}");
    }
    let suite =
        std::fs::read_to_string(root.join("crates/susi-gmcp/src/tests/resource_surface.rs"))
            .expect("the resource surface suite must exist");
    for proof in [
        "malformed_and_traversal_uris_never_reach_a_provider",
        "the_guard_decides_before_read_and_subscribe",
        "binary_text_and_multi_part_reads_are_sealed",
        "reads_are_bounded_and_a_missing_resource_is_never_empty",
        "listing_pages_cover_every_resource_once_and_detect_a_changed_catalog",
        "a_resource_replaced_during_reads_is_always_served_whole",
        "a_cancelled_read_stops_its_provider_and_the_session_continues",
        "updates_and_list_changes_reach_only_what_was_subscribed",
    ] {
        assert!(
            is_test_fn(&suite, proof) || suite.contains(&format!("async fn {proof}()")),
            "the suite must retain {proof}"
        );
    }

    // ── 7. Evidence verification: complete, fresh, signed, exit 0, ───
    //    matching current ledger hash, and backed by committed tests
    let ledger_bytes = std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap();
    let ledger_hash = susi_paths::hex::encode(Sha256::digest(&ledger_bytes));
    let test_src = read_all_gmcp_requirements_sources(root);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let evidence = std::fs::read_to_string(root.join(".agents/gmcp/evidence/VC-203-010.jsonl"))
        .expect("VC-203-010 must retain an evidence artifact");
    let mut covered = std::collections::BTreeSet::new();
    let artifacts = evidence_artifacts(root, "VC-203-010");
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
        "GMCP-203-010-01",
        "GMCP-203-010-02",
        "GMCP-203-010-03",
        "GMCP-203-010-04",
        "GMCP-203-010-05",
        "GMCP-203-010-06",
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
        .find(|entry| entry["id"] == "VC-203-010")
        .expect("VC-203-010 must exist in roadmap.json");
    assert!(
        vc_entry["progress"]
            .as_str()
            .unwrap_or_default()
            .starts_with("DELIVERED"),
        "VC-203-010 must be marked DELIVERED in roadmap.json"
    );

    let verdict_text =
        std::fs::read_to_string(root.join(".agents/roadmap-verdicts/VC-203-010.json"))
            .expect("VC-203-010.json verdict must exist");
    let verdict: serde_json::Value =
        serde_json::from_str(&verdict_text).expect("verdict must parse");
    assert_eq!(verdict["vector"], "VC-203-010");
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
