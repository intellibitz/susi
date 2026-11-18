/// Serialises in-process HTTP tests that swap this process's HOME so a
/// parallel gmcp_requirements thread cannot race the env and re-expose the
/// operator's seeded `api_token` mid-exchange.
static GMCP_HTTP_HOME: Mutex<()> = Mutex::new(());

/// New operational proof is local; existing source proof remains readable.
/// A present but unreadable/malformed local record must never fall back.
fn gmcp_evidence_path(root: &Path, legacy: &str) -> PathBuf {
    let name = legacy
        .strip_prefix(".agents/gmcp/evidence/")
        .expect("legacy evidence path");
    assert!(!name.contains('/') && name.ends_with(".jsonl"));
    let local = root.join(".susi/records/gmcp/evidence").join(name);
    match std::fs::symlink_metadata(&local) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => root.join(legacy),
        Ok(_) | Err(_) => local,
    }
}

fn gmcp_evidence_files(root: &Path) -> Vec<PathBuf> {
    let mut files = std::collections::BTreeMap::new();
    for directory in [".agents/gmcp/evidence", ".susi/records/gmcp/evidence"] {
        let entries = match std::fs::read_dir(root.join(directory)) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("cannot read {directory}: {error}"),
        };
        for entry in entries {
            let path = entry.expect("evidence entry").path();
            if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                files.insert(
                    path.file_name().expect("evidence filename").to_owned(),
                    path,
                );
            }
        }
    }
    files.into_values().collect()
}

fn gmcp_requirement_inputs_digest(records: &[serde_json::Value]) -> String {
    use sha2::Digest;
    let ordered: std::collections::BTreeMap<&str, &serde_json::Value> = records
        .iter()
        .map(|record| (record["requirement"].as_str().expect("requirement"), record))
        .collect();
    assert!(!ordered.is_empty(), "mastery requires requirement evidence");
    susi_paths::hex::encode(sha2::Sha256::digest(serde_json::to_vec(&ordered).unwrap()))
}

/// Local mastery always requires an exact-input binding, including shallow
/// clones. Only a real legacy source record may use the historical fallback.
fn gmcp_mastery_inputs_bound(
    root: &Path,
    path: &Path,
    record: &serde_json::Value,
    requirements: &[serde_json::Value],
) -> bool {
    if !path.starts_with(root.join(".susi/records/gmcp/evidence"))
        && record.get("requirements_sha256").is_none()
    {
        return false;
    }
    assert_eq!(
        record["requirements_sha256"].as_str(),
        Some(gmcp_requirement_inputs_digest(requirements).as_str()),
        "mastery did not test the current requirement evidence"
    );
    true
}

/// Legacy append-only chains and current one-record-per-requirement snapshots
/// share this reader. Linked legacy branches select their newest tip; an
/// unlinked duplicate fails. A current snapshot needs no historical chain.
fn newest_records(evidence: &str) -> Vec<serde_json::Value> {
    const LINK: [&str; 3] = ["recorded_unix", "identity", "artifact_sha256"];
    let names = |record: &serde_json::Value, target: &serde_json::Value| {
        LINK.iter()
            .all(|field| record["supersedes"][*field] == target[*field])
    };
    let mut order: Vec<String> = Vec::new();
    let mut chains: std::collections::HashMap<String, Vec<serde_json::Value>> =
        std::collections::HashMap::new();
    for line in evidence.lines().filter(|line| !line.trim().is_empty()) {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("evidence records must be single-line JSON");
        let requirement = record["requirement"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        match chains.get_mut(&requirement) {
            Some(chain) => {
                assert!(
                    chain.iter().any(|earlier| names(&record, earlier)),
                    "{requirement}: a later record must name, in `supersedes`, an earlier \
                     record of its requirement: {record}"
                );
                chain.push(record);
            }
            None => {
                order.push(requirement.clone());
                chains.insert(requirement, vec![record]);
            }
        }
    }
    order
        .into_iter()
        .filter_map(|requirement| {
            let chain = chains.remove(&requirement)?;
            let tips: Vec<&serde_json::Value> = chain
                .iter()
                .filter(|record| !chain.iter().any(|later| names(later, record)))
                .collect();
            // max_by_key keeps the last of equal keys: file order breaks ties.
            tips.into_iter()
                .max_by_key(|record| record["recorded_unix"].as_u64().unwrap_or(0))
                .cloned()
        })
        .collect()
}

#[test]
fn gmcp_local_mastery_requires_exact_inputs_even_when_source_is_shallow() {
    let root = Path::new("fixture");
    let local = root.join(".susi/records/gmcp/evidence/VC-203-001.jsonl");
    let legacy = root.join(".agents/gmcp/evidence/VC-203-001.jsonl");
    let inputs = vec![serde_json::json!({"requirement":"R-1","exit":0,"tested_commit":"source"})];
    for record in [
        serde_json::json!({}),
        serde_json::json!({"requirements_sha256":null}),
        serde_json::json!({"requirements_sha256":"wrong"}),
    ] {
        assert!(std::panic::catch_unwind(|| {
            let shallow = true;
            let _legacy_check =
                !gmcp_mastery_inputs_bound(root, &local, &record, &inputs) && !shallow;
        })
        .is_err());
    }
    assert!(!gmcp_mastery_inputs_bound(
        root,
        &legacy,
        &serde_json::json!({}),
        &inputs
    ));
    let bound = serde_json::json!({"requirements_sha256":gmcp_requirement_inputs_digest(&inputs)});
    assert!(gmcp_mastery_inputs_bound(root, &local, &bound, &inputs));
    let changed = vec![serde_json::json!({"requirement":"R-1","exit":0,"tested_commit":"other"})];
    assert!(
        std::panic::catch_unwind(|| gmcp_mastery_inputs_bound(root, &local, &bound, &changed))
            .is_err()
    );
}

#[test]
fn gmcp_local_records_shadow_legacy_without_hiding_corruption_or_missing_history() {
    let root = std::env::temp_dir().join(format!("gmcp-local-reader-{}", std::process::id()));
    std::fs::create_dir_all(root.join(".agents/gmcp/evidence")).unwrap();
    let legacy = ".agents/gmcp/evidence/VC-203-001.jsonl";
    std::fs::write(root.join(legacy), "legacy").unwrap();
    assert_eq!(gmcp_evidence_path(&root, legacy), root.join(legacy));
    let local = root.join(".susi/records/gmcp/evidence/VC-203-001.jsonl");
    std::fs::create_dir_all(local.parent().unwrap()).unwrap();
    std::fs::write(&local, "corrupt").unwrap();
    assert_eq!(gmcp_evidence_path(&root, legacy), local);
    assert_eq!(gmcp_evidence_files(&root), vec![local.clone()]);
    assert!(
        std::panic::catch_unwind(|| newest_records(&std::fs::read_to_string(&local).unwrap()))
            .is_err()
    );
    std::fs::remove_file(&local).unwrap();
    std::fs::remove_file(root.join(legacy)).unwrap();
    assert!(gmcp_evidence_files(&root).is_empty());
    assert!(std::fs::read_to_string(gmcp_evidence_path(&root, legacy)).is_err());
    std::fs::remove_dir_all(root).unwrap();
}
/// A ledger path the repository can hold: relative, with no `..` (or `.`)
/// component, so an absolute path or a traversal to a host file never
/// counts as present. The production verifier confines its documents the
/// same way.
fn confined_repo_path(path: &str) -> bool {
    let path = Path::new(path);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// Whether `name` is a test item in `src`: a top-level `fn name()` whose
/// own attribute lines, the ones directly above it, include `#[test]`. Doc
/// and line comments between the attributes are skipped, never read: a
/// comment that mentions `#[test]` above a helper does not make it a test.
fn is_test_fn(src: &str, name: &str) -> bool {
    let lines: Vec<&str> = src.lines().collect();
    let Some(at) = lines
        .iter()
        .position(|line| *line == format!("fn {name}() {{"))
    else {
        return false;
    };
    for line in lines[..at].iter().rev().map(|line| line.trim()) {
        if line == "#[test]" {
            return true;
        }
        if line.starts_with("#[") || line.starts_with("//") {
            continue;
        }
        return false;
    }
    false
}

/// The files a vector's GMCP evidence was proven against: every confined
/// production path the vector's rows name that exists, plus the inputs all
/// the checks share (below). `scripts/gmcp-record-evidence.sh` builds the
/// same set.
fn evidence_artifacts(root: &Path, vector: &str) -> Vec<String> {
    let ledger: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/requirements-ledger.json"))
            .expect("the canonical ledger must exist"),
    )
    .expect("the ledger must parse");
    let mut paths: std::collections::BTreeSet<String> = ledger["requirements"]
        .as_array()
        .expect("requirements")
        .iter()
        .filter(|row| row["trace"]["vector"] == vector)
        .filter_map(|row| row["trace"]["production_path"].as_str())
        .filter(|path| confined_repo_path(path) && root.join(path).exists())
        .map(str::to_string)
        .collect();
    // What every recorded check reads besides the vector's production
    // code: the tests, the ledger and its pinned corpus, the contracts and
    // fixtures, the ledger checker, and the GMCP crate's sources. Never
    // `.agents/gmcp/evidence`: a record cannot digest itself. The GMCP
    // manifest is retained, with only package.version normalized below.
    for shared in [
        "tests/gmcp_requirements.rs",
        "tests/gmcp_requirements",
        ".agents/gmcp/requirements-ledger.json",
        ".agents/gmcp/sources.json",
        ".agents/gmcp/spec",
        ".agents/gmcp/fixtures",
        "docs/gmcp",
        "docs/gmcp-roadmap.md",
        "scripts/check-gmcp-ledger.sh",
        "crates/susi-gmcp/src",
        "crates/susi-gmcp/Cargo.toml",
    ] {
        if root.join(shared).exists() {
            paths.insert(shared.to_string());
        }
    }
    paths.into_iter().collect()
}

/// Concatenates the root `tests/gmcp_requirements.rs` manifest and every split
/// file under `tests/gmcp_requirements/*.rs` so zero-test checks see all `#[test]`
/// functions regardless of file placement.
fn read_all_gmcp_requirements_sources(root: &Path) -> String {
    let mut out = std::fs::read_to_string(root.join("tests/gmcp_requirements.rs"))
        .expect("the GMCP tests must be readable");
    let dir = root.join("tests/gmcp_requirements");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("rs"))
            .collect();
        files.sort();
        for file in files {
            out.push('\n');
            out.push_str(
                &std::fs::read_to_string(&file)
                    .unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display())),
            );
        }
    }
    out
}

/// Digest source object IDs and the GMCP manifest with only its release
/// package.version omitted. Features and dependency specifications remain bound.
fn artifact_tree_digest(tree: &[u8], manifest: &str) -> String {
    use sha2::Digest as _;
    let mut digest = sha2::Sha256::new();
    for row in tree.split_inclusive(|byte| *byte == b'\n') {
        if !row.ends_with(b"\tcrates/susi-gmcp/Cargo.toml\n") {
            digest.update(row);
        }
    }
    digest.update(b"susi-gmcp manifest without package.version\n");
    let mut package = false;
    for line in manifest.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            package = trimmed == "[package]";
        }
        if package
            && line
                .split_once('=')
                .is_some_and(|(key, _)| key.trim() == "version")
        {
            continue;
        }
        digest.update(line.as_bytes());
    }
    susi_paths::hex::encode(digest.finalize())
}

/// Git objects, not the working tree: shallow CI and full clones hash the
/// same committed bytes. A missing revision or manifest fails closed.
fn artifacts_digest(root: &Path, rev: &str, artifacts: &[String]) -> Option<String> {
    let out = Command::new("git")
        .args(["ls-tree", "-r", "--full-tree", rev, "--"])
        .args(artifacts)
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    let manifest = Command::new("git")
        .args(["show", &format!("{rev}:crates/susi-gmcp/Cargo.toml")])
        .current_dir(root)
        .output()
        .ok()?;
    if !manifest.status.success() {
        return None;
    }
    Some(artifact_tree_digest(
        &out.stdout,
        std::str::from_utf8(&manifest.stdout).ok()?,
    ))
}

#[test]
fn evidence_digest_ignores_only_release_version_not_manifest_or_source_changes() {
    let tree = b"100644 blob old-version-object\tcrates/susi-gmcp/Cargo.toml\n100644 blob source-object\tcrates/susi-gmcp/src/lib.rs\n100644 blob test-object\ttests/gmcp_requirements.rs\n100644 blob spec-object\t.agents/gmcp/spec/protocol.md\n";
    let manifest = "[package]\nname = \"susi-gmcp\"\nversion = \"0.21.37\"\n[features]\ndefault = [\"tools-rich\"]\n[dependencies.example]\nversion = \"1\"\n";
    let before = artifact_tree_digest(tree, manifest);
    // Retain every other byte, including an absent final newline.
    assert_ne!(
        before,
        artifact_tree_digest(tree, manifest.trim_end_matches('\n'))
    );
    let changed_version_tree =
        String::from_utf8_lossy(tree).replace("old-version-object", "new-version-object");
    assert_eq!(
        before,
        artifact_tree_digest(
            changed_version_tree.as_bytes(),
            &manifest.replace("0.21.37", "0.21.38")
        )
    );
    assert_ne!(
        before,
        artifact_tree_digest(tree, &manifest.replace("tools-rich", "minimal"))
    );
    assert_ne!(
        before,
        artifact_tree_digest(
            tree,
            &manifest.replace("version = \"1\"", "version = \"2\"")
        )
    );
    for object in ["source-object", "test-object", "spec-object"] {
        let changed = String::from_utf8_lossy(tree).replace(object, "changed-object");
        assert_ne!(before, artifact_tree_digest(changed.as_bytes(), manifest));
    }
}

/// A vector's evidence binds to the code it ran on: each record names
/// `tested_commit`, the commit its check passed on, and `tested_artifacts`,
/// the [`artifacts_digest`] of [`evidence_artifacts`] there. A record counts
/// only while that digest is still HEAD's: a change to any file its checks
/// read makes it stale until the checks run again and are re-recorded
/// (`scripts/gmcp-record-evidence.sh`).
/// In a full clone the commit must exist, be in HEAD's history and digest to
/// the recorded value; a shallow clone (the CI test shards) cannot look it up
/// and rests on the digest alone.
fn assert_bound_to_tested_commit(
    root: &Path,
    record: &serde_json::Value,
    artifacts: &[String],
    head_digest: &str,
) {
    let requirement = record["requirement"].as_str().unwrap_or("?");
    let commit = record["tested_commit"].as_str().unwrap_or_default();
    assert!(
        commit.len() == 40 && commit.chars().all(|c| c.is_ascii_hexdigit()),
        "{requirement}: evidence must name the commit its check ran on (tested_commit)"
    );
    assert_eq!(
        record["tested_artifacts"].as_str(),
        Some(head_digest),
        "{requirement}: stale evidence: a file its checks read (the tests, the ledger and \
         its corpus, a contract or fixture, the GMCP crate or the vector's production \
         code) changed since {commit}; run the checks again and re-record them \
         (scripts/gmcp-record-evidence.sh)"
    );
    let shallow = is_shallow(root);
    let at_commit = artifacts_digest(root, commit, artifacts);
    // A full clone holds every commit HEAD descends from: a tested_commit it
    // cannot find was never one of them (an invented id fails here). Only a
    // shallow clone, which cannot tell, rests on the digest above.
    assert!(
        at_commit.is_some() || shallow,
        "{requirement}: tested_commit {commit} is not in this repository"
    );
    if let Some(at_commit) = at_commit {
        assert_eq!(
            at_commit, head_digest,
            "{requirement}: tested_artifacts does not describe {commit}"
        );
        assert!(
            in_tested_history(root, commit),
            "{requirement}: tested_commit {commit} is not in HEAD's history"
        );
    }
}

/// Whether `commit` is in HEAD's history, directly or through distillation.
///
/// A distilled release branch (`local-distill compose`,
/// docs/release-distillation.md) re-applies each selected task's commits
/// onto an older release base, so the development commits evidence was
/// recorded on are not HEAD's ancestors there. Each composed commit names its
/// sources in `Source-Commits:` beside a `Distilled-Task:` line; evidence
/// recorded on (an ancestor of) one of those sources is in the release's
/// provenance. The digest checks above still require the evidence's files to
/// be byte-identical at the tested commit and at HEAD.
fn in_tested_history(root: &Path, commit: &str) -> bool {
    let is_ancestor = |descendant: &str| {
        Command::new("git")
            .args(["merge-base", "--is-ancestor", commit, descendant])
            .current_dir(root)
            .status()
            .is_ok_and(|status| status.success())
    };
    if is_ancestor("HEAD") {
        return true;
    }
    distilled_sources(root)
        .iter()
        .any(|source| is_ancestor(source))
}

/// Source commits named by the distillation commits in HEAD's history.
fn distilled_sources(root: &Path) -> Vec<String> {
    let Ok(output) = Command::new("git")
        .args(["log", "--format=%B%x00", "HEAD"])
        .current_dir(root)
        .output()
    else {
        return Vec::new();
    };
    let log = String::from_utf8_lossy(&output.stdout);
    let mut sources = std::collections::BTreeSet::new();
    for message in log.split('\0') {
        if !message
            .lines()
            .any(|line| line.starts_with("Distilled-Task: "))
        {
            continue;
        }
        for line in message.lines() {
            if let Some(list) = line.strip_prefix("Source-Commits: ") {
                sources.extend(
                    list.split(',')
                        .map(str::trim)
                        .filter(|oid| oid.len() == 40 && oid.chars().all(|c| c.is_ascii_hexdigit()))
                        .map(str::to_string),
                );
            }
        }
    }
    sources.into_iter().collect()
}

/// Whether this clone is shallow (the CI test shards): it cannot look up an
/// older commit, so a check that needs one rests on what HEAD holds.
fn is_shallow(root: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--is-shallow-repository"])
        .current_dir(root)
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == "true")
}

/// Mastery is current only when its source/artifact binding and the exact
/// requirement inputs it tested are current. New local records bind those
/// inputs by digest; legacy records retain their committed-input comparison.
#[test]
fn gmcp_mastery_records_are_retained_and_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let test_src = read_all_gmcp_requirements_sources(root);
    let files = gmcp_evidence_files(root);
    assert!(!files.is_empty(), "no GMCP evidence files");
    let shallow = is_shallow(root);
    for path in files {
        let vector = path
            .file_stem()
            .and_then(|name| name.to_str())
            .expect("vector filename");
        let number = vector
            .strip_prefix("VC-")
            .unwrap_or_else(|| panic!("{vector}: evidence files are named VC-<NNN>-<NNN>.jsonl"));
        let file = format!(".agents/gmcp/evidence/{vector}.jsonl");
        let evidence = std::fs::read_to_string(&path).expect("evidence must be readable");
        let id = format!("GMCP-{number}-M");
        let (mastery, requirements): (Vec<_>, Vec<_>) = newest_records(&evidence)
            .into_iter()
            .partition(|record| record["requirement"] == id.as_str());
        assert!(
            !requirements.is_empty(),
            "{vector}: no requirement evidence for its mastery record to stand on"
        );
        let [record] = mastery.as_slice() else {
            panic!(
                "{vector}: no retained mastery record {id}; run scripts/gmcp-record-evidence.sh --mastery {vector}"
            );
        };
        let test = format!("gmcp_vc_{}_mastery", number.replace('-', "_"));
        assert!(
            is_test_fn(&test_src, &test),
            "{vector}: no mastery test {test}"
        );
        assert_eq!(
            record["check"].as_str(),
            Some(format!("cargo test --locked --test gmcp_requirements {test}").as_str()),
            "{id}: the mastery record must name its vector's mastery test"
        );
        assert_eq!(record["exit"], 0, "{id}: a mastery record attests a pass");
        let artifacts = evidence_artifacts(root, vector);
        let head_digest = artifacts_digest(root, "HEAD", &artifacts)
            .expect("HEAD must list the evidence artifacts");
        assert_bound_to_tested_commit(root, record, &artifacts, &head_digest);
        let at = record["recorded_unix"].as_u64().unwrap_or(0);
        for requirement in &requirements {
            assert!(
                requirement["recorded_unix"].as_u64().unwrap_or(u64::MAX) <= at,
                "{id}: {} was re-recorded after the mastery record; run \
                 scripts/gmcp-record-evidence.sh --mastery {vector} on the commit that holds it",
                requirement["requirement"]
            );
        }
        if !gmcp_mastery_inputs_bound(root, &path, record, &requirements) && !shallow {
            let commit = record["tested_commit"].as_str().unwrap_or_default();
            let then = Command::new("git")
                .args(["show", &format!("{commit}:{file}")])
                .current_dir(root)
                .output()
                .expect("git show must run");
            assert!(then.status.success(), "{id}: {commit} holds no {file}");
            let proved: Vec<serde_json::Value> =
                newest_records(&String::from_utf8_lossy(&then.stdout))
                    .into_iter()
                    .filter(|earlier| earlier["requirement"] != id.as_str())
                    .collect();
            assert!(
                proved == requirements,
                "{id}: the mastery test ran at {commit} over other requirement evidence than \
                 the current gate judges by; run scripts/gmcp-record-evidence.sh --mastery {vector}"
            );
        }
    }
}

/// A record line for the `newest_records` tests.
fn evidence_line(requirement: &str, at: u64, who: &str, supersedes: Option<(u64, &str)>) -> String {
    let mut record = serde_json::json!({ "requirement": requirement, "recorded_unix": at, "identity": who, "artifact_sha256": format!("h{at}"),});
    if let Some((at, who)) = supersedes {
        record["supersedes"] = serde_json::json!({ "recorded_unix": at, "identity": who, "artifact_sha256": format!("h{at}"),});
    }
    record.to_string()
}

#[test]
fn is_test_fn_reads_the_attribute_not_a_comment() {
    let src = "#[test]\n/// docs\nfn real_one() {\n}\n\n\
               /// Not a test despite mentioning #[test].\nfn fabricated() {\n}\n\n\
               #[test]\n#[should_panic]\nfn attributed() {\n}\n";
    assert!(is_test_fn(src, "real_one"));
    assert!(is_test_fn(src, "attributed"));
    assert!(!is_test_fn(src, "fabricated"));
    assert!(!is_test_fn(src, "absent"));
}

#[test]
fn newest_records_follows_a_linear_supersession_chain() {
    let evidence = [
        evidence_line("R-1", 1, "a", None),
        evidence_line("R-2", 1, "a", None),
        evidence_line("R-1", 2, "b", Some((1, "a"))),
        evidence_line("R-1", 3, "c", Some((2, "b"))),
    ]
    .join("\n");
    let judged = newest_records(&evidence);
    assert_eq!(judged.len(), 2);
    assert_eq!(judged[0]["requirement"], "R-1");
    assert_eq!(judged[0]["identity"], "c");
    assert_eq!(judged[1]["identity"], "a");
}

/// Two branches superseded the same record and the merge kept both: the
/// file is A, B (supersedes A), C (supersedes A). The newest tip is judged,
/// and a later record may still extend either branch.
#[test]
fn newest_records_resolves_a_forked_chain_to_its_newest_tip() {
    let forked = [
        evidence_line("R-1", 1, "a", None),
        evidence_line("R-1", 3, "b", Some((1, "a"))),
        evidence_line("R-1", 2, "c", Some((1, "a"))),
    ];
    assert_eq!(newest_records(&forked.join("\n"))[0]["identity"], "b");
    let extended = [
        forked.to_vec(),
        vec![evidence_line("R-1", 4, "d", Some((2, "c")))],
    ]
    .concat()
    .join("\n");
    assert_eq!(newest_records(&extended)[0]["identity"], "d");
}

#[test]
#[should_panic(expected = "must name, in `supersedes`, an earlier record")]
fn newest_records_refuses_an_unlinked_later_record() {
    let evidence = [
        evidence_line("R-1", 1, "a", None),
        evidence_line("R-1", 2, "b", None),
    ]
    .join("\n");
    newest_records(&evidence);
}

/// Throwaway HOME for in-process `start_http_server` tests: NetGuard reads
/// `<config_dir>/api_token`, and without this swap a developer's seeded
/// host token turns every unauthenticated loopback call into 401 before
/// Origin / envelope checks run.
fn gmcp_http_home_guard(tag: &str) -> (std::sync::MutexGuard<'static, ()>, EnvGuard, PathBuf) {
    let lock = GMCP_HTTP_HOME.lock().unwrap_or_else(|e| e.into_inner());
    let home = std::env::temp_dir().join(format!(
        "gmcp-http-home-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::create_dir_all(home.join(".susi"));
    let mut env = EnvGuard::isolated();
    env.set("HOME", &home);
    (lock, env, home)
}

fn susi_command(root: &Path, home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_susi"));
    isolate_home(&mut command, home)
        .args(args)
        .current_dir(root)
        // This fixture owns its config location; bypass automatic dev-instance relocation.
        .env("SUSI_HOME", home.join(".susi"))
        .env("XDG_CONFIG_HOME", home.join("xdg"));
    command
}

fn susi_json(root: &Path, home: &Path, args: &[&str]) -> serde_json::Value {
    let output = susi_command(root, home, args)
        .output()
        .unwrap_or_else(|e| panic!("the susi binary must run {args:?}: {e}"));
    assert!(
        output.status.success(),
        "susi {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .unwrap_or_else(|e| panic!("susi {args:?} must print JSON: {e}"))
}

fn call_tool_report(result: &serde_json::Value) -> serde_json::Value {
    assert_eq!(
        result["isError"], false,
        "tool call returned an error: {result}"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("CallToolResult must carry a text block");
    serde_json::from_str(text).expect("ledger_verify must emit the canonical JSON report")
}

/// Canonical ledger report without spawning the `susi` binary. The CLI and
/// `ledger_verify` tool both call this same production verifier; the binary
/// packaging path is still proven by `gmcp_req_203_001_02_production`.
fn ledger_report(root: &Path) -> serde_json::Value {
    susi_gmcp::requirements_ledger::verify_report_json(root)
        .unwrap_or_else(|e| panic!("pinned ledger must verify: {e}"))
}

/// Direct tool surface without a process: same `ledger_verify` body the MCP
/// call path dispatches to.
fn ledger_tool_report(root: &Path) -> serde_json::Value {
    let raw = susi_gmcp::tools::CoreTools::ledger_verify(&serde_json::json!({}), root)
        .unwrap_or_else(|e| panic!("ledger_verify tool must succeed: {e}"));
    serde_json::from_str(&raw).expect("ledger_verify must emit JSON")
}

/// A newline-delimited JSON-RPC session against `susi mcp serve` — the served
/// MCP surface the daemon and gateway share. Responses are collected on a
/// reader thread; `request` handles correlation, `request_verbatim` replays
/// fixtures byte-for-byte (including malformed ids), and Drop kills the child.
struct StdioSession {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    rx: std::sync::mpsc::Receiver<serde_json::Value>,
    next_id: u64,
    /// Responses received while waiting for a different id — the service
    /// answers concurrent requests out of order, so a non-matching line
    /// is stashed for its caller rather than discarded.
    pending: Vec<serde_json::Value>,
}

impl StdioSession {
    /// Spawn `mcp serve` with no handshake — the era the session lands in
    /// is whatever the caller's first messages establish.
    fn spawn_raw(root: &Path, home: &Path) -> Self {
        let mut child = susi_command(root, home, &["mcp", "serve"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the susi binary must spawn `mcp serve`");
        let stdin = child.stdin.take().expect("stdio server must accept stdin");
        let stdout = child.stdout.take().expect("stdio server must yield stdout");
        let (tx, rx) = std::sync::mpsc::channel::<serde_json::Value>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(value) = serde_json::from_str(&line) {
                    if tx.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            child,
            stdin,
            rx,
            next_id: 2,
            pending: Vec::new(),
        }
    }

    /// A legacy-era session: `initialize` negotiates 2025-11-25, so
    /// `params.protocolVersion` applies and per-request `_meta` protocol
    /// fields are not required.
    fn spawn(root: &Path, home: &Path) -> Self {
        let mut session = Self::spawn_raw(root, home);
        session
            .stdin
            .write_all(
                br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"gmcp-it","version":"1.0"}}}"#
                    .as_slice(),
            )
            .and_then(|()| {
                session.stdin.write_all(
                    b"\n{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                )
            })
            .expect("handshake must write");
        session.stdin.flush().expect("requests must flush");
        // Consume the initialize response before callers drive requests.
        session
            .response_for(&serde_json::json!(1), std::time::Duration::from_secs(300))
            .expect("initialize must be answered");
        session
    }

    /// A modern-era session: no `initialize` handshake exists on the
    /// 2026-07-28 path — `server/discover` carries the per-request
    /// `_meta` fields instead and doubles as the warm-up so fixture
    /// replays never race the server's cold start.
    fn spawn_modern(root: &Path, home: &Path) -> Self {
        let mut session = Self::spawn_raw(root, home);
        session.send(&serde_json::json!({ "jsonrpc": "2.0", "id": "modern-handshake", "method": "server/discover", "params": {"_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {}}}}));
        session
            .response_for(
                &serde_json::json!("modern-handshake"),
                std::time::Duration::from_secs(300),
            )
            .expect("server/discover must be answered");
        session
    }

    fn send(&mut self, message: &serde_json::Value) {
        let line = serde_json::to_string(message).expect("request must serialize");
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .expect("request must write");
    }

    fn send_raw(&mut self, bytes: &[u8]) {
        self.stdin
            .write_all(bytes)
            .and_then(|()| self.stdin.flush())
            .expect("raw request must write");
    }

    fn response_for(
        &mut self,
        id: &serde_json::Value,
        timeout: std::time::Duration,
    ) -> Option<serde_json::Value> {
        if let Some(pos) = self.pending.iter().position(|v| &v["id"] == id) {
            return Some(self.pending.remove(pos));
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let value = self.rx.recv_timeout(remaining).ok()?;
            if &value["id"] == id {
                return Some(value);
            }
            self.pending.push(value);
        }
    }

    /// Correlate a request by id and return its `result`, asserting no
    /// JSON-RPC error arrived.
    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let response = self
            .response_for(&serde_json::json!(id), std::time::Duration::from_secs(300))
            .expect("served endpoint must answer within the bound");
        assert!(
            response.get("error").is_none_or(|e| e.is_null()),
            "JSON-RPC error: {response}"
        );
        response["result"].clone()
    }

    /// The whole served catalog. `tools/list` pages at 100 items with a
    /// revision-bound `nextCursor`; this follows the cursor until it is
    /// absent (at most `MAX_TOOLS_LIST_PAGES` pages) and returns the last
    /// page's result with `tools` replaced by every page's tools. `params`
    /// (for example a modern `_meta`) travel on every page; a cursor error
    /// fails through `request`'s JSON-RPC error assertion.
    fn list_all_tools(&mut self, params: serde_json::Value) -> serde_json::Value {
        const MAX_TOOLS_LIST_PAGES: usize = 50;
        let mut tools = Vec::new();
        let mut page_params = params.clone();
        for _ in 0..MAX_TOOLS_LIST_PAGES {
            let mut result = self.request("tools/list", page_params);
            tools.extend(
                result["tools"]
                    .as_array()
                    .unwrap_or_else(|| panic!("tools/list page without tools: {result}"))
                    .iter()
                    .cloned(),
            );
            let cursor = result
                .as_object_mut()
                .and_then(|object| object.remove("nextCursor"))
                .filter(|cursor| !cursor.is_null());
            match cursor {
                Some(serde_json::Value::String(cursor)) => {
                    page_params = params.clone();
                    page_params["cursor"] = serde_json::json!(cursor);
                }
                Some(other) => panic!("tools/list returned a non-string nextCursor: {other}"),
                None => {
                    result["tools"] = serde_json::Value::Array(tools);
                    return result;
                }
            }
        }
        panic!("tools/list still paging after {MAX_TOOLS_LIST_PAGES} pages");
    }

    /// Replay a fixture request verbatim and report what came back for it:
    /// `Some(response)` on an answer within the bound, `None` on a silent drop.
    fn request_verbatim(
        &mut self,
        message: &serde_json::Value,
        timeout: std::time::Duration,
    ) -> Option<serde_json::Value> {
        self.send(message);
        let deadline = std::time::Instant::now() + timeout;
        let Some(id) = message.get("id").filter(|id| !id.is_null()) else {
            return self.verdict_for_uncorrelated(deadline);
        };
        if let Some(pos) = self.pending.iter().position(|v| v["id"] == *id) {
            return Some(self.pending.remove(pos));
        }
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let value = self.rx.recv_timeout(remaining).ok()?;
            if &value["id"] == id {
                return Some(value);
            }
            self.pending.push(value);
        }
    }

    /// A null/absent id has no correlation key — the first response the
    /// endpoint emits after it (if any) is the verdict. The gate answers a
    /// line before it reads the next, so a ping sent right behind is a
    /// barrier: its answer arriving first proves a silent drop at once,
    /// where waiting for nothing used to cost the whole deadline (300 s per
    /// dropped fixture). The barrier's own answer is consumed here so it
    /// never reaches a later caller.
    fn verdict_for_uncorrelated(
        &mut self,
        deadline: std::time::Instant,
    ) -> Option<serde_json::Value> {
        if !self.pending.is_empty() {
            return Some(self.pending.remove(0));
        }
        let barrier = serde_json::json!(format!("verbatim-barrier-{}", self.next_id));
        self.next_id += 1;
        self.send(&serde_json::json!({"jsonrpc": "2.0", "id": barrier, "method": "ping"}));
        let mut verdict = None;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            let Ok(value) = self.rx.recv_timeout(remaining) else {
                return verdict;
            };
            if value["id"] == barrier {
                return verdict;
            }
            if verdict.is_none() {
                verdict = Some(value);
            } else {
                self.pending.push(value);
            }
        }
    }
}

fn stop_test_server(child: &mut std::process::Child) {
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        let _ = susi_core::bounded_cmd::output_within(
            Command::new("taskkill").args(["/PID", &pid, "/T", "/F"]),
            std::time::Duration::from_secs(10),
        );
    }
    // The stdio peer can still hold its input pipe open if tree cleanup failed.
    // Only wait after the exact child has exited or its own kill succeeded.
    let result = child.try_wait().and_then(|status| match status {
        Some(_) => Ok(()),
        None => child.kill().and_then(|()| child.wait().map(|_| ())),
    });
    if let Err(error) = result {
        eprintln!("failed to reap test server {}: {error}", child.id());
        assert!(
            std::thread::panicking(),
            "test server cleanup failed: {error}"
        );
    }
}

impl Drop for StdioSession {
    fn drop(&mut self) {
        stop_test_server(&mut self.child);
    }
}

/// `susi mcp serve` pages `tools/list` at 100 items with a revision-bound
/// `nextCursor` (spec-compliant). The first page holds exactly 100 tools and
/// a cursor; following every cursor yields each registered core tool exactly
/// once, so a client that paginates never misses a tool past page one.
#[test]
fn stdio_tools_list_pages_cover_every_registered_tool() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch =
        std::env::temp_dir().join(format!("gmcp-tools-list-pages-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut wire = StdioSession::spawn(root, &home);

    let first = wire.request("tools/list", serde_json::json!({}));
    let first_tools = first["tools"].as_array().expect("first page tools array");
    assert_eq!(
        first_tools.len(),
        100,
        "the first tools/list page holds exactly 100 tools"
    );
    assert!(
        first["nextCursor"].as_str().is_some_and(|c| !c.is_empty()),
        "a catalog past 100 tools must carry a nextCursor: {}",
        first["nextCursor"]
    );

    let all = wire.list_all_tools(serde_json::json!({}));
    let tools = all["tools"].as_array().expect("paginated tools array");
    assert!(
        all.get("nextCursor").is_none(),
        "the merged listing is complete"
    );
    assert_eq!(
        &tools[..100],
        first_tools.as_slice(),
        "the paginated listing starts with the first page"
    );
    let mut served = std::collections::BTreeSet::new();
    for tool in tools {
        let name = tool["name"].as_str().expect("every tool is named");
        assert!(
            served.insert(name.to_string()),
            "pages must not repeat a tool: {name}"
        );
    }
    let missing: Vec<String> = susi_gmcp::tools::args::core_tool_definitions()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .filter(|name| !served.contains(name))
        .collect();
    assert!(
        missing.is_empty(),
        "every registered tool must be listed across pages; missing {missing:?}"
    );
    assert!(served.contains("ledger_verify"), "ledger_verify is listed");

    drop(wire);
    let _ = std::fs::remove_dir_all(&scratch);
}

fn copy_tree(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            std::fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::create_dir_all(dst).unwrap();
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn mutate_json(path: &Path, mutate: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    mutate(&mut value);
    std::fs::write(path, serde_json::to_string_pretty(&value).unwrap()).unwrap();
}

fn restore_tree(pristine: &Path, work: &Path) {
    // Only .agents/gmcp is mutated. Restore the whole tree's contents while
    // leaving identical files in place: deleting and rewriting 106 files for
    // every corruption case dominated the Windows test.
    #[allow(clippy::permissions_set_readonly_false)] // Windows-only; the file is a scratch fixture.
    fn remove(path: &Path) {
        let metadata = std::fs::symlink_metadata(path).unwrap();
        let kind = metadata.file_type();
        if kind.is_dir() && !kind.is_symlink() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            #[cfg(windows)]
            if kind.is_file() && metadata.permissions().readonly() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                std::fs::set_permissions(path, permissions).unwrap();
            }
            std::fs::remove_file(path).unwrap();
        }
    }
    fn sync(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for entry in std::fs::read_dir(dst).unwrap() {
            let entry = entry.unwrap();
            match std::fs::symlink_metadata(src.join(entry.file_name())) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => remove(&entry.path()),
                Err(e) => panic!("cannot inspect pristine fixture: {e}"),
            }
        }
        for entry in std::fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let source = entry.path();
            let target = dst.join(entry.file_name());
            let source_kind = entry.file_type().unwrap();
            let current = std::fs::symlink_metadata(&target).ok();
            if source_kind.is_dir() {
                if current
                    .as_ref()
                    .is_some_and(|m| !m.file_type().is_dir() || m.file_type().is_symlink())
                {
                    remove(&target);
                }
                sync(&source, &target);
            } else {
                assert!(
                    source_kind.is_file(),
                    "unexpected fixture entry: {}",
                    source.display()
                );
                let source_metadata = entry.metadata().unwrap();
                let same = current.as_ref().is_some_and(|m| {
                    m.file_type().is_file()
                        && !m.file_type().is_symlink()
                        && m.len() == source_metadata.len()
                        && std::fs::read(&target).unwrap() == std::fs::read(&source).unwrap()
                });
                if !same {
                    if current.is_some() {
                        remove(&target);
                    }
                    std::fs::copy(&source, &target).unwrap();
                } else {
                    #[cfg(unix)]
                    let permissions_differ = {
                        use std::os::unix::fs::PermissionsExt;
                        current.as_ref().unwrap().permissions().mode()
                            != source_metadata.permissions().mode()
                    };
                    #[cfg(not(unix))]
                    let permissions_differ = current.as_ref().unwrap().permissions().readonly()
                        != source_metadata.permissions().readonly();
                    if permissions_differ {
                        std::fs::set_permissions(&target, source_metadata.permissions()).unwrap();
                    }
                }
            }
        }
    }
    sync(&pristine.join(".agents/gmcp"), &work.join(".agents/gmcp"));
}

#[test]
fn fixture_restore_recovers_changed_missing_and_extra_files() {
    let scratch = std::env::temp_dir().join(format!("gmcp-restore-{}", std::process::id()));
    let pristine = scratch.join("pristine/.agents/gmcp");
    let work = scratch.join("work/.agents/gmcp");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(pristine.join("nested")).unwrap();
    std::fs::create_dir_all(work.join("nested")).unwrap();
    std::fs::write(pristine.join("nested/changed"), "good").unwrap();
    std::fs::write(pristine.join("missing"), "restored").unwrap();
    std::fs::write(pristine.join("same"), "same").unwrap();
    std::fs::write(work.join("nested/changed"), "evil").unwrap();
    std::fs::write(work.join("extra"), "remove").unwrap();
    std::fs::write(work.join("same"), "same").unwrap();
    let mut permissions = std::fs::metadata(work.join("same")).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(work.join("same"), permissions).unwrap();
    restore_tree(&scratch.join("pristine"), &scratch.join("work"));
    assert_eq!(std::fs::read(work.join("nested/changed")).unwrap(), b"good");
    assert_eq!(std::fs::read(work.join("missing")).unwrap(), b"restored");
    assert!(!work.join("extra").exists());
    assert!(!std::fs::metadata(work.join("same"))
        .unwrap()
        .permissions()
        .readonly());
    std::fs::remove_dir_all(scratch).unwrap();
}

/// One adversarial case: apply the corruption, assert the verifier fails
/// closed with the expected reason, then prove recovery — the restored
/// tree must verify cleanly again.
fn adversarial_case(pristine: &Path, work: &Path, needle: &str, mutate: impl FnOnce(&Path)) {
    mutate(work);
    let err = susi_gmcp::requirements_ledger::verify_pinned_ledger(work)
        .err()
        .unwrap_or_else(|| panic!("corruption must fail closed (expecting '{needle}')"));
    assert!(
        err.to_string().contains(needle),
        "corruption must surface '{needle}', got: {err}"
    );
    restore_tree(pristine, work);
    susi_gmcp::requirements_ledger::verify_pinned_ledger(work)
        .expect("restored tree must verify after a corruption cycle");
}

fn md5ish(data: &[u8]) -> u128 {
    // Cheap deterministic content fingerprint for the read-only invariant —
    // correctness, not cryptographic strength (the pinned SHA-256 checks live
    // in the verifier itself).
    let mut a: u64 = 0x9e3779b97f4a7c15;
    let mut b: u64 = 0xcbf29ce484222325;
    for (i, byte) in data.iter().enumerate() {
        a = a.wrapping_mul(0x100000001b3) ^ (*byte as u64);
        b ^= (*byte as u64).wrapping_add(i as u64);
        b = b.wrapping_mul(0x9e3779b97f4a7c15);
    }
    ((a as u128) << 64) | b as u128
}

#[test]
fn gmcp_ledger_trace_finds_local_records_without_qualifying_implementation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-local-trace-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("docs")).unwrap();
    copy_tree(&root.join(".agents/gmcp"), &scratch.join(".agents/gmcp"));
    std::fs::copy(
        root.join("docs/gmcp-roadmap.md"),
        scratch.join("docs/gmcp-roadmap.md"),
    )
    .unwrap();
    std::fs::remove_dir_all(scratch.join(".agents/gmcp/evidence")).unwrap();
    let before = susi_gmcp::requirements_ledger::verify_report_json(&scratch).unwrap();
    assert_eq!(
        before["trace_references"]["rows_with_missing_evidence"],
        before["rows"]
    );
    let local = scratch.join(".susi/records/gmcp/evidence");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("VC-203-001.jsonl"), "not qualification proof").unwrap();
    let after = susi_gmcp::requirements_ledger::verify_report_json(&scratch).unwrap();
    assert!(
        after["trace_references"]["rows_with_missing_evidence"]
            .as_u64()
            .unwrap()
            < before["trace_references"]["rows_with_missing_evidence"]
                .as_u64()
                .unwrap()
    );
    assert_eq!(
        after["verified_scope"],
        "pinned corpus and requirement classification"
    );
    assert_eq!(after["implementation_verified"], false);
    assert_eq!(
        after["trace_references"]["tests_and_implementation"],
        "not verified"
    );
    std::fs::remove_dir_all(scratch).unwrap();
}

/// Classification verification reports unresolved trace references without
/// claiming that file existence proves implementation or test coverage.
#[test]
fn gmcp_ledger_report_discloses_unverified_implementation() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!("gmcp-trace-audit-{}", std::process::id()));
    let home = scratch.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let cli = ledger_report(root);
    assert_eq!(cli["verified"], true);
    assert_eq!(
        cli["verified_scope"],
        "pinned corpus and requirement classification"
    );
    assert_eq!(cli["implementation_verified"], false);
    assert_eq!(
        cli["trace_references"]["tests_and_implementation"],
        "not verified"
    );
    assert_eq!(ledger_tool_report(root), cli);

    let ledger: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join(".agents/gmcp/requirements-ledger.json")).unwrap(),
    )
    .unwrap();
    let rows = ledger["requirements"].as_array().unwrap();
    for (field, reported) in [
        ("production_path", "rows_with_missing_production_path"),
        ("evidence", "rows_with_missing_evidence"),
    ] {
        let missing = rows
            .iter()
            .filter(|row| {
                let path = row["trace"][field].as_str().unwrap();
                !root.join(path).exists() && !(field == "evidence" && gmcp_evidence_path(root, path).exists())
            })
            .count();
        assert_eq!(
            cli["trace_references"][reported].as_u64(),
            Some(missing as u64)
        );
    }

    // ── Automated drift detection fails closed: a changed upstream
    //    file must invalidate the pinned corpus on the shipped path ───
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join(".agents/gmcp/sources.json")).unwrap(),
    )
    .unwrap();
    let drifted_doc = manifest["files"][0]["document"]
        .as_str()
        .unwrap()
        .to_string();
    let refetched = scratch.join("refetched");
    copy_tree(&root.join(".agents/gmcp/spec"), &refetched);
    let victim = refetched.join(&drifted_doc);
    let mut bytes = std::fs::read(&victim).unwrap();
    bytes.push(b'!');
    std::fs::write(&victim, &bytes).unwrap();
    let output = susi_command(
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
    .expect("susi must run");
    assert!(
        !output.status.success(),
        "drift detection must fail closed on a changed source"
    );
    let findings: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).unwrap();
    assert!(
        findings["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["document"] == drifted_doc && f["kind"] == "hash_drift"),
        "the drift finding must name the changed document: {findings}"
    );

    // ── The contract itself is retained operational documentation ────
    let contract = std::fs::read_to_string(root.join("docs/gmcp/contract-203-001.md"))
        .expect("contract must exist");
    for token in [
        "Normative clause",
        "Pinned sources",
        "Threat model",
        "Operations and incident runbook",
        "Migration (re-pinning an upstream standard)",
        "Rollback",
    ] {
        assert!(contract.contains(token), "contract must retain {token}");
    }

    let _ = std::fs::remove_dir_all(&scratch);
}

/// Evidence recorded on development main stays valid on a distilled release
/// branch only through the composed commits' `Source-Commits:` provenance;
/// a bare `Source-Commits:` line without `Distilled-Task:` grants nothing.
#[test]
fn tested_commit_history_follows_distillation_provenance() {
    let dir = std::env::temp_dir().join(format!(
        "susi-gmcp-provenance-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| -> String {
        let out = Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@localhost"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", dir.join(".absent"))
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    git(&["commit", "-q", "--allow-empty", "-m", "dev work"]);
    let tested = git(&["rev-parse", "HEAD"]);
    git(&["commit", "-q", "--allow-empty", "-m", "unselected"]);
    let unrelated = git(&["rev-parse", "HEAD"]);

    // A forged provenance line without the distillation marker.
    git(&["checkout", "-q", "--detach", &base]);
    git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        &format!("not distilled\n\nSource-Commits: {tested}"),
    ]);
    assert!(!in_tested_history(&dir, &tested));
    assert!(in_tested_history(&dir, &base), "a direct ancestor counts");

    // The composed release commit names its source.
    git(&["checkout", "-q", "--detach", &base]);
    git(&[
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        &format!(
            "Distill task T-X-1: x\n\nDistilled-Task: T-X-1\nSource-Commits: {tested}\nDistill-Base: {base}"
        ),
    ]);
    assert!(in_tested_history(&dir, &tested));
    assert!(!in_tested_history(&dir, &unrelated));
    let _ = std::fs::remove_dir_all(&dir);
}
