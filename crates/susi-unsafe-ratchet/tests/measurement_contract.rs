//! Adversarial evidence admission tests use fabricated, explicitly test-only
//! metadata. The CLI test also exercises real Cargo metadata and source files.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test assertions expose failed evidence contracts.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use susi_unsafe_ratchet::measurement::{FileMeasurement, Measurement, PackageMeasurement};
use susi_unsafe_ratchet::{inventory, measurement};

fn fixture() -> Measurement {
    let root = std::env::temp_dir().join("susi-unsafe-contract-metadata");
    let package = |id: &str, name: &str, version: &str, folder: &str, source: Value, two: bool| {
        let dir = root.join(folder);
        let mut targets = vec![json!({"kind":["lib"],"src_path":dir.join("src/lib.rs")})];
        if two {
            targets.push(json!({"kind":["bin"],"src_path":dir.join("src/main.rs")}));
        }
        json!({"id":id,"name":name,"version":version,"source":source,"manifest_path":dir.join("Cargo.toml"),"targets":targets})
    };
    let registry = json!("registry+https://github.com/rust-lang/crates.io-index");
    let dep = |id: &str, kind: Value| json!({"pkg":id,"dep_kinds":[{"kind":kind,"target":null}]});
    let metadata = json!({"version":1,"workspace_root":root,"workspace_members":["root"],"packages":[
        package("root","app","1.0.0","app",Value::Null,false),
        package("one","collision","1.0.0","registry-one",registry.clone(),false),
        package("two","collision","2.0.0","registry-two",registry.clone(),true),
        package("build","build-only","1.0.0","build",registry,false),
        package("local","collision","3.0.0","vendor/local",Value::Null,false)
    ],"resolve":{"root":"root","nodes":[
        {"id":"root","features":["default"],"deps":[dep("one",Value::Null),dep("two",Value::Null),dep("local",Value::Null),dep("build",json!("build"))]},
        {"id":"one","features":[],"deps":[]},{"id":"two","features":[],"deps":[]},
        {"id":"build","features":[],"deps":[]},{"id":"local","features":[],"deps":[]}
    ]}});
    let selection_tree = format!("0::app v1.0.0 ({})||default\n1::collision v1.0.0||\n1::collision v2.0.0||\n1::collision v3.0.0 ({})||\n", root.join("app").display(), root.join("vendor/local").display());
    let expected = measurement::expected_packages(&metadata, &root, &selection_tree).unwrap();
    let packages = expected
        .iter()
        .map(|package| PackageMeasurement {
            identity: package.identity.clone(),
            first_party: package.first_party,
            forbids_unsafe: true,
            files: package
                .files
                .iter()
                .map(|path| FileMeasurement {
                    path: path.clone(),
                    sha256: measurement::sha256(b"#![forbid(unsafe_code)]"),
                    bytes: 23,
                    forbids_unsafe: true,
                })
                .collect(),
        })
        .collect();
    Measurement {
        schema: measurement::SCHEMA.into(),
        complete: true,
        source_root: root.to_str().unwrap().into(),
        source_commit: "1".repeat(40),
        source_tree: "2".repeat(40),
        cargo_lock_sha256: "3".repeat(64),
        target: "x86_64-unknown-linux-gnu".into(),
        features: "default".into(),
        unit: measurement::UNIT.into(),
        scanner_sha256: "4".repeat(64),
        scanner_version: "test-only".into(),
        metadata_sha256: measurement::json_sha256(&metadata).unwrap(),
        graph_sha256: measurement::json_sha256(&expected).unwrap(),
        cargo_version: "cargo 1.99.0 test-fixture".into(),
        selection_args: measurement::selection_args(&metadata, "x86_64-unknown-linux-gnu").unwrap(),
        selection_tree_sha256: measurement::sha256(selection_tree.as_bytes()),
        selection_tree,
        metadata,
        packages,
    }
}

fn baseline(measured: &Measurement) -> inventory::Baseline {
    let selected = measurement::validate(measured).unwrap();
    let mut base = measured.clone();
    base.source_commit = "b".repeat(40);
    let historical = susi_unsafe_ratchet::UnsafeBaseline {
        features: measured.features.clone(),
        recorded_unix: 1,
        exposure: measured
            .packages
            .iter()
            .map(|p| susi_unsafe_ratchet::UnsafeExposure {
                crate_name: p.identity.name.clone(),
                unsafe_fns: 1,
                first_party_exception: p.first_party,
            })
            .collect(),
        sbom: measured
            .packages
            .iter()
            .map(|p| susi_unsafe_ratchet::SbomEntry {
                crate_name: p.identity.name.clone(),
                version: p.identity.version.clone(),
                provenance: p.identity.source.clone(),
            })
            .collect(),
    };
    let historical_json = serde_json::to_string_pretty(&historical).unwrap();
    let review = inventory::Review {
        schema: "susi/unsafe-component-review/v1".into(),
        reviewer: "fabricated-test-only".into(),
        historical_baseline_sha256: measurement::sha256(historical_json.as_bytes()),
        base_measurement_sha256: measurement::artifact_sha256(&base).unwrap(),
        candidate_measurement_sha256: measurement::artifact_sha256(measured).unwrap(),
        scanner_sha256: measured.scanner_sha256.clone(),
        components: vec![],
    };
    inventory::Baseline {
        schema: inventory::BASELINE_SCHEMA.into(),
        features: measured.features.clone(),
        exposure: measured
            .packages
            .iter()
            .filter(|p| !p.forbids_unsafe)
            .map(|p| susi_unsafe_ratchet::UnsafeExposure {
                crate_name: p.identity.name.clone(),
                unsafe_fns: 1,
                first_party_exception: p.first_party,
            })
            .collect(),
        sbom: measured
            .packages
            .iter()
            .map(|p| susi_unsafe_ratchet::SbomEntry {
                crate_name: p.identity.name.clone(),
                version: p.identity.version.clone(),
                provenance: p.identity.source.clone(),
            })
            .collect(),
        recorded_unix: 1,
        unit: measured.unit.clone(),
        target: measured.target.clone(),
        historical_baseline_sha256: review.historical_baseline_sha256.clone(),
        base_source_commit: base.source_commit.clone(),
        candidate_source_commit: measured.source_commit.clone(),
        base_measurement_sha256: review.base_measurement_sha256.clone(),
        candidate_measurement_sha256: review.candidate_measurement_sha256.clone(),
        review_sha256: measurement::json_sha256(&review).unwrap(),
        review,
        historical_json,
        base_measurement: base,
        candidate_measurement: measured.clone(),
        allowances: measured
            .packages
            .iter()
            .map(|p| inventory::Allowance {
                identity: p.identity.clone(),
                first_party: p.first_party,
                unsafe_units: u8::from(!p.forbids_unsafe),
                files_sha256: inventory::files_digest(p).unwrap(),
                features: selected
                    .iter()
                    .find(|s| s.identity == p.identity)
                    .unwrap()
                    .features
                    .clone(),
                dependencies: selected
                    .iter()
                    .find(|s| s.identity == p.identity)
                    .unwrap()
                    .dependencies
                    .clone(),
                basis: if p.first_party {
                    "workspace-member"
                } else if p.forbids_unsafe {
                    "forbids-unsafe"
                } else {
                    "matched-historical-exposure"
                }
                .into(),
            })
            .collect(),
    }
}

#[test]
fn exact_normal_graph_retains_versions_and_does_not_exempt_arbitrary_paths() {
    let measured = fixture();
    measurement::validate(&measured).unwrap();
    assert_eq!(measured.packages.len(), 4);
    assert_eq!(
        measured
            .packages
            .iter()
            .filter(|p| p.identity.name == "collision")
            .count(),
        3
    );
    assert!(!measured
        .packages
        .iter()
        .any(|p| p.identity.name == "build-only"));
    assert_eq!(
        measured.packages.iter().filter(|p| p.first_party).count(),
        1
    );
    let local = measured
        .packages
        .iter()
        .find(|p| p.identity.version == "3.0.0")
        .unwrap();
    assert!(!local.first_party);
    assert!(local.identity.source.starts_with("path:"));
    assert!(inventory::check_inventory(&measured, &baseline(&measured))
        .unwrap()
        .is_empty());
}

#[test]
fn missing_or_duplicate_packages_and_entry_points_are_not_zero_exposure() {
    let good = fixture();
    let mut bad = good.clone();
    bad.packages.clear();
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.packages.pop();
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.packages[1] = bad.packages[0].clone();
    assert!(measurement::validate(&bad).is_err());
    let multi = good
        .packages
        .iter()
        .position(|p| p.files.len() == 2)
        .unwrap();
    let mut bad = good.clone();
    bad.packages[multi].files.pop();
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.packages[multi].files[1] = bad.packages[multi].files[0].clone();
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good;
    bad.packages[multi].files[1].path = "src/unknown.rs".into();
    assert!(measurement::validate(&bad).is_err());
}

#[test]
fn malformed_partial_and_mixed_unit_documents_are_rejected() {
    for value in [
        Value::Null,
        json!({}),
        json!({"packages":[]}),
        json!({"packages":[],"packages_without_metrics":[]}),
    ] {
        assert!(serde_json::from_value::<Measurement>(value).is_err());
    }
    let good = fixture();
    let mut bad = good.clone();
    bad.complete = false;
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.unit = "unsafe-functions".into();
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.packages[0].forbids_unsafe = false;
    assert!(measurement::validate(&bad).is_err());
    for value in [Value::Null, json!("true"), json!(1), json!(-1)] {
        let mut bad = serde_json::to_value(&good).unwrap();
        bad["packages"][0]["forbids_unsafe"] = value;
        assert!(serde_json::from_value::<Measurement>(bad).is_err());
    }
    let mut bad = serde_json::to_value(&good).unwrap();
    bad["packages_without_metrics"] = json!(["omitted"]);
    assert!(serde_json::from_value::<Measurement>(bad).is_err());
}

#[test]
fn metadata_source_graph_and_first_party_tampering_fail_closed() {
    let good = fixture();
    let mut bad = good.clone();
    bad.metadata["resolve"]["root"] = json!("one");
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.graph_sha256 = "0".repeat(64);
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.packages[1].first_party = !bad.packages[1].first_party;
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.packages[0].identity.version = "fabricated".into();
    assert!(measurement::validate(&bad).is_err());
    let mut bad = good.clone();
    bad.packages[0].files[0].sha256 = "missing".into();
    assert!(measurement::validate(&bad).is_err());
    let root = Path::new(&good.source_root);
    let mut metadata = good.metadata.clone();
    metadata["resolve"]["nodes"][0]["deps"][0]["pkg"] = json!("missing");
    assert!(measurement::expected_packages(&metadata, root, &good.selection_tree).is_err());
    let mut metadata = good.metadata.clone();
    metadata["resolve"]["nodes"][0]["deps"][0]["dep_kinds"] = json!([]);
    assert!(measurement::expected_packages(&metadata, root, &good.selection_tree).is_err());
    let mut metadata = good.metadata.clone();
    metadata["packages"][0]["source"] = json!("registry+forged-workspace");
    assert!(measurement::expected_packages(&metadata, root, &good.selection_tree).is_err());
    let mut metadata = good.metadata.clone();
    metadata["packages"][4]["manifest_path"] =
        json!(std::env::temp_dir().join("elsewhere/Cargo.toml"));
    assert!(measurement::expected_packages(&metadata, root, &good.selection_tree).is_err());
}

#[test]
fn new_exposure_source_change_and_context_mismatch_still_require_review() {
    let mut measured = fixture();
    let old = baseline(&measured);
    let package = measured
        .packages
        .iter_mut()
        .find(|p| !p.first_party)
        .unwrap();
    package.forbids_unsafe = false;
    for file in &mut package.files {
        file.forbids_unsafe = false;
    }
    assert_eq!(
        inventory::check_inventory(&measured, &old).unwrap().len(),
        1
    );
    let accepted = baseline(&measured);
    assert!(inventory::check_inventory(&measured, &accepted)
        .unwrap()
        .is_empty());
    let package = measured
        .packages
        .iter_mut()
        .find(|p| !p.first_party)
        .unwrap();
    package.files[0].sha256 = "e".repeat(64);
    assert_eq!(
        inventory::check_inventory(&measured, &accepted)
            .unwrap()
            .len(),
        1
    );
    let mut wrong = accepted.clone();
    wrong.target = "other-host".into();
    assert!(inventory::check_inventory(&measured, &wrong).is_err());
    let mut wrong = accepted;
    wrong.review_sha256 = "0".repeat(64);
    assert!(inventory::check_inventory(&measured, &wrong).is_err());
}

#[test]
fn newly_selected_optional_feature_cannot_reuse_an_unselected_feature_review() {
    let mut measured = fixture();
    let package = measured
        .packages
        .iter_mut()
        .find(|p| p.identity.version == "1.0.0" && !p.first_party)
        .unwrap();
    package.forbids_unsafe = false;
    package.files[0].forbids_unsafe = false;
    let old = baseline(&measured);
    measured.metadata["resolve"]["nodes"][1]["features"] = json!(["foreign-code"]);
    measured.selection_tree = measured
        .selection_tree
        .replace("1::collision v1.0.0||", "1::collision v1.0.0||foreign-code");
    measured.metadata_sha256 = measurement::json_sha256(&measured.metadata).unwrap();
    measured.selection_tree_sha256 = measurement::sha256(measured.selection_tree.as_bytes());
    let expected = measurement::expected_packages(
        &measured.metadata,
        Path::new(&measured.source_root),
        &measured.selection_tree,
    )
    .unwrap();
    measured.graph_sha256 = measurement::json_sha256(&expected).unwrap();
    assert_eq!(
        inventory::check_inventory(&measured, &old).unwrap().len(),
        1
    );
    // Removing selected capabilities is a decrease; it does not introduce a
    // newly selected foreign-code boundary.
    let accepted = baseline(&measured);
    let previous = &old.candidate_measurement;
    assert!(inventory::check_inventory(previous, &accepted)
        .unwrap()
        .is_empty());
}

#[test]
fn full_measurement_identity_survives_worktree_relocation() {
    let measured = fixture();
    let old = measured.source_root.clone();
    let new = std::env::temp_dir()
        .join("susi-unsafe-relocated")
        .to_str()
        .unwrap()
        .to_owned();
    let encoded = serde_json::to_string(&measured.metadata)
        .unwrap()
        .replace(&old.replace('\\', "\\\\"), &new.replace('\\', "\\\\"));
    let metadata: Value = serde_json::from_str(&encoded).unwrap();
    let relocated = measurement::expected_packages(
        &metadata,
        Path::new(&new),
        &measured.selection_tree.replace(&old, &new),
    )
    .unwrap();
    assert_eq!(
        measurement::json_sha256(&relocated).unwrap(),
        measured.graph_sha256
    );
}

#[test]
fn root_selection_excludes_workspace_unified_optional_and_other_platform_edges() {
    let measured = fixture();
    let mut metadata = measured.metadata.clone();
    // Metadata can contain normal edges activated only by workspace/dev
    // feature unification, including a dependency for another platform.
    metadata["resolve"]["nodes"][0]["features"] = json!(["default", "dev-only"]);
    metadata["resolve"]["nodes"][0]["deps"][3]["dep_kinds"] =
        json!([{"kind":null,"target":"cfg(target_os = \"macos\")"}]);
    let selected = measurement::expected_packages(
        &metadata,
        Path::new(&measured.source_root),
        &measured.selection_tree,
    )
    .unwrap();
    assert_eq!(selected.len(), 4);
    assert!(!selected.iter().any(|p| p.identity.name == "build-only"));
    assert_eq!(
        selected.iter().find(|p| p.first_party).unwrap().features,
        ["default"]
    );
    assert_eq!(
        measurement::json_sha256(&selected).unwrap(),
        measured.graph_sha256
    );
}

#[test]
fn malformed_ambiguous_or_unexpanded_cargo_tree_contexts_fail_closed() {
    let good = fixture();
    let root = Path::new(&good.source_root);
    for tree in [
        String::new(),
        good.selection_tree
            .replace("1::collision v1.0.0", "3::collision v1.0.0"),
        good.selection_tree
            .replace("||default", "||default,default"),
        good.selection_tree.replace("||default", "||unknown"),
        good.selection_tree.replace("v1.0.0||\n", "v1.0.0|| (*)\n"),
        format!("{}1::build-only v1.0.0||\n", good.selection_tree),
    ] {
        assert!(
            measurement::expected_packages(&good.metadata, root, &tree).is_err(),
            "{tree}"
        );
    }
    let mut metadata = good.metadata.clone();
    let mut duplicate = metadata["packages"][1].clone();
    duplicate["id"] = json!("other-registry");
    duplicate["source"] = json!("registry+https://example.invalid/index");
    metadata["packages"].as_array_mut().unwrap().push(duplicate);
    // Cargo's display cannot distinguish same name/version from registries.
    // Never select whichever metadata row happens to occur first.
    assert!(measurement::expected_packages(&metadata, root, &good.selection_tree).is_err());
}

#[test]
fn baseline_cannot_forge_carry_forward_or_discard_matched_scan_proof() {
    let measured = fixture();
    let good = baseline(&measured);
    inventory::validate_baseline(&good).unwrap();
    let mut bad = good.clone();
    bad.base_measurement.packages.pop();
    assert!(inventory::validate_baseline(&bad).is_err());
    let mut bad = good.clone();
    bad.historical_json = "{}".into();
    assert!(inventory::validate_baseline(&bad).is_err());
    let mut bad = good.clone();
    bad.sbom.clear();
    assert!(inventory::validate_baseline(&bad).is_err());
    let mut bad = good.clone();
    bad.allowances[0].unsafe_units = 1;
    bad.allowances[0].basis = "matched-historical-exposure".into();
    assert!(inventory::validate_baseline(&bad).is_err());
    let mut changed = measured.clone();
    let p = changed
        .packages
        .iter_mut()
        .find(|p| !p.first_party)
        .unwrap();
    p.forbids_unsafe = false;
    for f in &mut p.files {
        f.forbids_unsafe = false;
    }
    let historical = serde_json::from_str(&good.historical_json).unwrap();
    let required =
        inventory::migration_reviews(&historical, &good.base_measurement, &changed).unwrap();
    assert_eq!(required.len(), 1);
    let mut wrong_tool = good.base_measurement.clone();
    wrong_tool.scanner_sha256 = "a".repeat(64);
    assert!(inventory::migration_reviews(&historical, &wrong_tool, &changed).is_err());
    let mut wrong_cargo = good.base_measurement;
    wrong_cargo.cargo_version = "cargo another-version".into();
    assert!(inventory::migration_reviews(&historical, &wrong_cargo, &changed).is_err());
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "susi-unsafe-contract-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
#[cfg(unix)]
fn entrypoint_symlink_cannot_escape_the_measured_package() {
    let temp = Temp::new();
    let package = temp.0.join("package");
    std::fs::create_dir(&package).unwrap();
    std::fs::write(temp.0.join("external.rs"), "#![forbid(unsafe_code)]").unwrap();
    std::os::unix::fs::symlink(temp.0.join("external.rs"), package.join("lib.rs")).unwrap();
    assert!(measurement::contained_file(&package, "lib.rs").is_err());
    assert!(measurement::contained_file(&package, "missing.rs").is_err());
}

fn command(root: &Path, program: &str, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn actual_scanner_handles_legacy_source_and_refuses_dirty_or_unparseable_source() {
    let temp = Temp::new();
    let root = temp.0.join("repo");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname='native-unsafe-fixture'\nversion='0.1.0'\nedition='2015'\n",
    )
    .unwrap();
    let source="#![forbid(unsafe_code)]\n#[allow(bare_trait_objects)]\ntype Action = Fn(&u8) + Send + Sync;\n";
    std::fs::write(root.join("src/lib.rs"), source).unwrap();
    command(&root, env!("CARGO"), &["generate-lockfile", "--offline"]);
    command(&root, "git", &["init", "-q"]);
    command(&root, "git", &["config", "user.name", "unsafe fixture"]);
    command(
        &root,
        "git",
        &["config", "user.email", "fixture@example.invalid"],
    );
    command(
        &root,
        "git",
        &["add", "Cargo.toml", "Cargo.lock", "src/lib.rs"],
    );
    command(
        &root,
        "git",
        &[
            "-c",
            "core.hooksPath=disabled-fixture-hooks",
            "commit",
            "-qm",
            "fixture",
        ],
    );
    let rustc = Command::new("rustc").arg("-vV").output().unwrap();
    let text = String::from_utf8(rustc.stdout).unwrap();
    let host = text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap();
    let output = temp.0.join("measurement.json");
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_unsafe_measure"))
            .args([
                "--root",
                root.to_str().unwrap(),
                "--target",
                host,
                "--features",
                "default",
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .unwrap()
    };
    let result = run();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let measured = measurement::read_measurement(&output).unwrap();
    assert_eq!(measured.packages.len(), 1);
    assert!(measured.packages[0].forbids_unsafe);
    assert_eq!(
        measured.packages[0].files[0].sha256,
        measurement::sha256(source.as_bytes())
    );
    let retained = std::fs::read(&output).unwrap();
    std::fs::write(root.join("src/lib.rs"), "fn broken( {").unwrap();
    assert!(!run().status.success());
    assert_eq!(std::fs::read(&output).unwrap(), retained);
    command(&root, "git", &["add", "src/lib.rs"]);
    command(
        &root,
        "git",
        &[
            "-c",
            "core.hooksPath=disabled-fixture-hooks",
            "commit",
            "-qm",
            "malformed fixture",
        ],
    );
    assert!(!run().status.success());
    assert_eq!(std::fs::read(&output).unwrap(), retained);
}

#[test]
fn rejected_input_cannot_overwrite_a_baseline_or_issue_a_clean_receipt() {
    let temp = Temp::new();
    let report = temp.0.join("partial.json");
    let baseline = temp.0.join("baseline.json");
    let receipt = temp.0.join("receipt.json");
    std::fs::write(&report, "{\"packages\":[],\"packages_without_metrics\":[]}").unwrap();
    std::fs::write(&baseline, "historical sentinel").unwrap();
    for migration in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_unsafe_ratchet"));
        command.args([
            "--measurement",
            report.to_str().unwrap(),
            "--baseline",
            baseline.to_str().unwrap(),
        ]);
        if migration {
            command.args([
                "--write-baseline",
                baseline.to_str().unwrap(),
                "--base-measurement",
                report.to_str().unwrap(),
                "--review",
                report.to_str().unwrap(),
            ]);
        } else {
            command.args(["--receipt", receipt.to_str().unwrap()]);
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            std::fs::read_to_string(&baseline).unwrap(),
            "historical sentinel"
        );
        assert!(!receipt.exists());
    }
}

#[test]
fn duplicate_keys_in_dynamic_metadata_or_baseline_proof_are_rejected() {
    let temp = Temp::new();
    let measured = fixture();
    let path = temp.0.join("measurement.json");
    let encoded = serde_json::to_string(&measured).unwrap();
    for duplicate in [
        encoded.replacen("\"metadata\":{", "\"metadata\":{\"version\":0,", 1),
        encoded.replacen(
            "\"name\":\"app\"",
            "\"name\":\"ignored\",\"name\":\"app\"",
            1,
        ),
    ] {
        assert_ne!(duplicate, encoded);
        std::fs::write(&path, duplicate).unwrap();
        assert!(measurement::read_measurement(&path)
            .unwrap_err()
            .contains("duplicate JSON object key"));
    }
    let mut bad = baseline(&measured);
    bad.historical_json = bad
        .historical_json
        .replacen("{", "{\"features\":\"ignored\",", 1);
    assert!(inventory::validate_baseline(&bad)
        .unwrap_err()
        .contains("duplicate JSON object key"));
}
