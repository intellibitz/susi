/// GMCP-203-020-01: the GMCP server authoring SDK and plugin lifecycle
/// contract is pinned (`docs/gmcp/contract-203-020.md`) with verified source
/// hashes, confined production-path behavior rows (`SDK-001..SDK-018`), zero
/// vendor types across the public boundary, and positive/negative schema
/// derivation and plugin manifest admission fixtures.
#[test]
fn gmcp_req_203_020_01_contract() {
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Arc;
    use susi_config::extensions::{
        validate_manifest, ExtensionManifest, HOST_PACK_API_MAJOR, KNOWN_PERMISSIONS,
    };
    use susi_gmcp::mcp::model::Tool;
    use susi_gmcp::protocol::GmcpService;

    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let contract_path = root.join("docs/gmcp/contract-203-020.md");
    let contract = std::fs::read_to_string(&contract_path)
        .expect("docs/gmcp/contract-203-020.md must exist for VC-203-020");

    // 1. All 10 normative contract sections are present.
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
            "contract-203-020.md must contain `## {section}`"
        );
    }

    // 2. Pinned ledger & every SHA-256 in the contract's `Pinned sources` table
    // match `.agents/gmcp/sources.json` and `.agents/gmcp/spec/` byte-for-byte.
    let ledger_report = susi_gmcp::requirements_ledger::verify_pinned_ledger(root)
        .expect("pinned GMCP corpus must verify cleanly");
    assert!(ledger_report.rows > 0 && ledger_report.documents > 0);

    let sources: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json"))
            .expect("sources.json must exist"),
    )
    .expect("sources.json must be valid JSON");
    let upstream_commits: BTreeMap<&str, &str> = sources["upstream"]
        .as_object()
        .expect("upstream map")
        .iter()
        .map(|(repo, meta)| (repo.as_str(), meta["commit"].as_str().expect("commit")))
        .collect();
    let source_files: BTreeMap<&str, (&str, &str)> = sources["files"]
        .as_array()
        .expect("files array")
        .iter()
        .map(|f| {
            (
                f["document"].as_str().expect("document"),
                (
                    f["repo"].as_str().expect("repo"),
                    f["sha256"].as_str().expect("sha256"),
                ),
            )
        })
        .collect();

    let pinned_rows: Vec<&str> = contract
        .lines()
        .filter(|line| {
            line.starts_with("| modelcontextprotocol |")
                || line.starts_with("| ext-tasks |")
                || line.starts_with("| ext-apps |")
                || line.starts_with("| ext-auth |")
        })
        .collect();
    assert!(
        pinned_rows.len() >= 14,
        "contract-203-020.md must pin at least 14 authoritative spec/schema documents, got {}",
        pinned_rows.len()
    );
    for row in pinned_rows {
        let cells: Vec<&str> = row
            .trim_matches('|')
            .split('|')
            .map(|cell| cell.trim().trim_matches('`'))
            .collect();
        assert_eq!(
            cells.len(),
            4,
            "pinned source row must have 4 cells: {row}"
        );
        let (repo, doc, commit, sha) = (cells[0], cells[1], cells[2], cells[3]);
        let (expected_repo, expected_sha) = source_files
            .get(doc)
            .copied()
            .unwrap_or_else(|| panic!("pinned doc `{doc}` missing from .agents/gmcp/sources.json"));
        assert_eq!(repo, expected_repo, "repo mismatch for {doc}");
        assert_eq!(
            Some(commit),
            upstream_commits.get(repo).copied(),
            "commit mismatch for {repo} ({doc})"
        );
        assert_eq!(sha, expected_sha, "sources.json sha256 mismatch for {doc}");
        let disk_bytes = std::fs::read(root.join(".agents/gmcp/spec").join(doc))
            .unwrap_or_else(|e| panic!("missing vendored spec `{doc}`: {e}"));
        let disk_sha = Sha256::digest(&disk_bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert_eq!(disk_sha, sha, "on-disk sha256 drift for {doc}");
    }

    // 3. Behavior table binds unique `SDK-xxx` rows with valid RFC levels and
    // confined existing repository paths.
    let sdk_rows: Vec<&str> = contract
        .lines()
        .filter(|line| line.starts_with("| SDK-"))
        .collect();
    assert!(
        sdk_rows.len() >= 18,
        "behavior table must bind at least 18 SDK-* rows, got {}",
        sdk_rows.len()
    );
    let mut ids = BTreeSet::new();
    for row in sdk_rows {
        let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
        assert!(
            cells.len() >= 4,
            "behavior row must carry id | level | clause | production path: {row}"
        );
        let (id, level, clause, path) = (cells[0], cells[1], cells[2], cells[3]);
        assert!(ids.insert(id.to_string()), "duplicate behavior row id {id}");
        assert!(
            ["must", "must_not", "should", "should_not", "may"].contains(&level),
            "{id}: invalid normative level `{level}`"
        );
        assert!(!clause.is_empty(), "{id}: clause must not be empty");
        assert!(
            confined_repo_path(path),
            "{id}: production path must be confined to repository: {path}"
        );
        assert!(
            root.join(path).exists(),
            "{id}: declared production path must exist: {path}"
        );
    }

    // 4. Every normative VC-203-020 mechanism and the zero-bundled-packs rule
    // are explicitly resolved in the contract.
    let lower = contract.to_lowercase();
    for mechanism in [
        "schema",
        "context",
        "middleware",
        "fixture",
        "hot reload",
        "permission",
        "compatibility",
        "transactional",
        "unload",
        "vendor",
        "example",
        "tools",
        "resources",
        "prompts",
        "subscriptions",
        "mrtr",
        "tasks",
        "extensions",
        "susi commercial apps",
    ] {
        assert!(
            lower.contains(mechanism),
            "contract-203-020.md must resolve `{mechanism}`"
        );
    }
    assert!(
        contract.contains("gmcp_req_203_020_01_contract"),
        "contract-203-020.md must bind fixtures to gmcp_req_203_020_01_contract"
    );

    // 5. Positive & negative SDK schema-derivation and tool-registration fixtures.
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
    assert_eq!(
        derived["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(derived["type"], "object");
    assert_eq!(
        derived["properties"]["query"]["description"],
        "Search expression."
    );
    assert_eq!(
        derived["properties"]["filter"]["$ref"],
        "#/$defs/FilterSpec"
    );
    assert_eq!(derived["properties"]["limit"]["default"], 0);
    assert_eq!(
        derived["properties"]["includeArchived"]["type"],
        serde_json::json!(["boolean", "null"])
    );
    assert_eq!(derived["properties"]["labels"]["type"], "array");
    assert_eq!(
        derived["$defs"]["FilterSpec"]["properties"]["tag"]["type"],
        serde_json::json!(["string", "null"])
    );
    let required: BTreeSet<&str> = derived["required"]
        .as_array()
        .expect("required list")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(
        required,
        BTreeSet::from(["query", "filter", "labels"]),
        "`Option<T>` and `#[serde(default)]` fields must be omitted from `required`"
    );

    let scratch =
        std::env::temp_dir().join(format!("gmcp-203-020-contract-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch dir");
    let service = GmcpService::new(scratch.clone());

    let valid_tool = Tool::new_with_raw(
        "sdk_contract_query",
        Some("SDK contract verification tool".into()),
        susi_gmcp::mcp::schema::root::<QueryArgs>(),
    );
    service
        .register_tool(
            valid_tool,
            Arc::new(|_params, _ctx| {
                Box::pin(async {
                    Ok(susi_gmcp::mcp::model::CallToolResult::success(vec![
                        susi_gmcp::mcp::model::ContentBlock::text("ok"),
                    ])
                    .into())
                })
            }),
        )
        .expect("valid derived 2020-12 schema must register cleanly");

    // Negative schema fixture: remote `$ref` must be refused fail-closed without mutating catalog.
    let mut bad_schema = serde_json::Map::new();
    bad_schema.insert(
        "$ref".into(),
        serde_json::json!("https://evil.example/remote-schema.json"),
    );
    let bad_tool = Tool::new_with_raw(
        "sdk_remote_ref_rejected",
        Some("must fail closed".into()),
        bad_schema,
    );
    assert!(
        service
            .register_tool(
                bad_tool,
                Arc::new(|_params, _ctx| {
                    Box::pin(async {
                        Ok(susi_gmcp::mcp::model::CallToolResult::success(vec![]).into())
                    })
                }),
            )
            .is_err(),
        "remote `$ref` schema must be rejected fail-closed at tool registration"
    );
    let _ = std::fs::remove_dir_all(&scratch);

    // 6. Positive & negative plugin manifest lifecycle admission fixtures.
    assert_eq!(HOST_PACK_API_MAJOR, 1);
    assert_eq!(
        KNOWN_PERMISSIONS,
        &[
            "filesystem.read",
            "filesystem.write",
            "network.egress",
            "process.exec"
        ]
    );

    let valid_digest = "a".repeat(64);
    let valid_manifest: ExtensionManifest = serde_json::from_value(serde_json::json!({
        "id": "sdk-sample-pack",
        "name": "SDK Sample Pack",
        "version": "0.1.0",
        "apiVersion": "1.0",
        "capabilities": ["mcp.tools.sample"],
        "requires": ["gmcp.server.v1"],
        "optional": ["gmcp.tasks.v1"],
        "permissions": ["filesystem.read"],
        "files": {
            "tools": "catalog/tools.json"
        },
        "checksums": {
            "tools": format!("sha256:{valid_digest}")
        },
        "x-vendor-metadata": {"tier": "contract-fixture"}
    }))
    .expect("valid manifest JSON must parse");
    validate_manifest(&valid_manifest).expect("valid plugin manifest fixture must pass");
    assert_eq!(
        valid_manifest.extra.get("x-vendor-metadata"),
        Some(&serde_json::json!({"tier": "contract-fixture"})),
        "Mandate 35: unknown manifest keys must round-trip in `extra`"
    );

    // Negative 1: unknown permission string must fail closed.
    let mut unknown_perm = valid_manifest.clone();
    unknown_perm.permissions = vec!["kernel.root".into()];
    assert!(
        validate_manifest(&unknown_perm)
            .expect_err("unknown permission must fail")
            .contains("unknown permission"),
        "unknown permission error must be diagnostic"
    );

    // Negative 2: incompatible host API major version must fail closed.
    let mut wrong_major = valid_manifest.clone();
    wrong_major.api_version = "2.0".into();
    assert!(
        validate_manifest(&wrong_major)
            .expect_err("incompatible api_version major must fail")
            .contains("incompatible with host major"),
        "api_version mismatch error must name host major"
    );

    // Negative 3: path traversal escaping pack root without `filesystem.read` must fail closed.
    let mut path_escape = valid_manifest.clone();
    path_escape.permissions.clear();
    path_escape
        .files
        .insert("tools".into(), "../outside/tools.json".into());
    assert!(
        validate_manifest(&path_escape)
            .expect_err("pack root escape without filesystem.read must fail")
            .contains("without declaring `filesystem.read`"),
        "pack root escape must fail closed"
    );

    // Negative 4: signatures declared without a signer must fail closed.
    let mut unsigned_sig = valid_manifest.clone();
    unsigned_sig.signatures.insert("tools".into(), "b".repeat(128));
    unsigned_sig.signer = None;
    assert!(
        validate_manifest(&unsigned_sig)
            .expect_err("signatures without signer must fail")
            .contains("without a signer"),
        "missing signer must fail closed"
    );

    // Negative 5: malformed SHA-256 digest must fail closed.
    let mut bad_checksum = valid_manifest;
    bad_checksum
        .checksums
        .insert("tools".into(), "sha256:not-a-hex-digest".into());
    assert!(
        validate_manifest(&bad_checksum)
            .expect_err("malformed checksum must fail")
            .contains("invalid SHA-256 checksum"),
        "malformed SHA-256 checksum must fail closed"
    );

    // 7. Zero vendor MCP/schema crate leakage across `susi-gmcp` and `susi-abi`,
    // and `.agents/gmcp/evidence/VC-203-020.jsonl` stays absent until `.M` Mastery.
    for manifest_rel in ["crates/susi-gmcp/Cargo.toml", "crates/susi-abi/Cargo.toml"] {
        let cargo_toml = std::fs::read_to_string(root.join(manifest_rel))
            .unwrap_or_else(|e| panic!("failed reading {manifest_rel}: {e}"));
        for forbidden_vendor in ["rmcp", "schemars", "axum", "tower"] {
            assert!(
                !cargo_toml.contains(forbidden_vendor),
                "{manifest_rel} must not depend on vendor crate `{forbidden_vendor}`"
            );
        }
    }
    assert!(
        !root.join(".agents/gmcp/evidence/VC-203-020.jsonl").exists(),
        "VC-203-020.jsonl must remain absent until T-CODEXGMCPROADMAP-113 (.M Mastery)"
    );
}
