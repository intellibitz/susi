//! Native exact-tree release qualification: the record a release tag carries.
//!
//! `release-qualify vX.Y.Z` runs the release gates on the clean committed
//! release tree P, writes [`record_path`] and commits only that file as Q, whose
//! single parent is P. The release tag points at Q. `push-policy` accepts a new
//! `vX.Y.Z` tag only when [`verify_tag`] holds: Q changes exactly the record;
//! the record names P and P's tree; every gate in [`REQUIRED_GATES`] ran and
//! exited 0; tests ran and none failed; every built asset answered `--version`
//! with the release version; and the version, lockfile digest and toolchain it
//! names are P's own. The record is evidence bound to one tree, not a flag: a
//! change to the tree after qualification changes P and invalidates it.
//!
//! A release is reached from one of two sources, and the record says which
//! (`source`). `snapshot`: P is a snapshot of development main plus the
//! version bump (`local-snapshot cut`). `distill`: P is an accept-verified
//! distilled candidate (`local-distill`, receipt
//! `releases/distill/<name>.json` in its envelope commit E) plus the version
//! bump, and the record carries a `distillation` block naming the candidate,
//! the receipt digest, E, the composed commit and tree, the included tasks and
//! the adopted host commits. [`verify_distilled_source`] checks that block
//! against the tree itself, before any gate runs and again when the tag is
//! pushed: the receipt's digest, its verdict, no rejected or deferred task, no
//! unadopted host commit, every acceptance command passed, E an ancestor of P,
//! and nothing between E and P but the version bump ([`BUMP_FILES`]). Records
//! written before `source` existed (v0.21.54, v0.21.55) carry neither field and
//! stay valid as snapshot releases.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

pub const SCHEMA: &str = "susi-release-qualification/v1";
pub const REQUIRED_GATES: [&str; 7] = [
    "format", "check", "clippy", "licenses", "tests", "build", "smoke",
];
pub const DISTILL_RECEIPT_SCHEMA: &str = "susi-distillation-receipt-v1";
pub const SOURCE_SNAPSHOT: &str = "snapshot";
pub const SOURCE_DISTILL: &str = "distill";

/// The files a version bump modifies, exactly those of the v0.21.55 bump
/// `47339995d` (and of the v0.21.54 bump `2fe760fd7`): the engine manifests,
/// the lockfile, the README badge and the governance ledgers. In each of them
/// a bump may change a line only by replacing the old version with the new
/// one. The only other change a bump may make is to add its own task record
/// directly under [`BUMP_TASK_DIR`].
pub const BUMP_FILES: [&str; 12] = [
    ".agents/evidence.json",
    ".agents/identity.json",
    ".agents/roadmap.json",
    "Cargo.lock",
    "Cargo.toml",
    "README.md",
    "crates/susi-daemon/Cargo.toml",
    "crates/susi-gawd/Cargo.toml",
    "crates/susi-gemi/Cargo.toml",
    "crates/susi-gmcp/Cargo.toml",
    "crates/susi-harness/Cargo.toml",
    "crates/susi-server/Cargo.toml",
];
pub const BUMP_TASK_DIR: &str = ".agents/tasks/";

/// The committed record of `tag`, relative to the repository root.
pub fn record_path(tag: &str) -> String {
    format!("releases/qualification/{tag}.json")
}

/// `X.Y.Z` for a release tag `vX.Y.Z` (digits only), `None` for anything else.
pub fn release_version(tag: &str) -> Option<&str> {
    let version = tag.strip_prefix('v')?;
    let parts: Vec<&str> = version.split('.').collect();
    let numeric = |part: &&str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    (parts.len() == 3 && parts.iter().all(numeric)).then_some(version)
}

/// The `version` of the root `[package]` in a Cargo manifest.
pub fn package_version(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some(value) = line.strip_prefix("version") {
                let value = value.trim_start().strip_prefix('=')?.trim();
                return Some(value.trim_matches('"').to_owned());
            }
        }
    }
    None
}

/// The `channel` pinned by a `rust-toolchain.toml`.
pub fn toolchain_channel(pin: &str) -> Option<String> {
    pin.lines().find_map(|line| {
        let value = line.trim().strip_prefix("channel")?;
        let value = value.trim_start().strip_prefix('=')?.trim();
        Some(value.trim_matches('"').to_owned())
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn git_bytes(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("git")
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(args)
        .output()
        .map_err(|e| format!("cannot run Git: {e}"))?;
    if !out.status.success() {
        return Err(format!("Git failed: {}", args.join(" ")));
    }
    Ok(out.stdout)
}

fn git_text(root: &Path, args: &[&str]) -> Result<String, String> {
    String::from_utf8(git_bytes(root, args)?)
        .map(|s| s.trim().to_owned())
        .map_err(|e| e.to_string())
}

fn text<'a>(record: &'a Value, field: &str) -> Result<&'a str, String> {
    record[field]
        .as_str()
        .ok_or_else(|| format!("record field `{field}` missing"))
}

fn count(value: &Value, field: &str) -> Result<u64, String> {
    value[field]
        .as_u64()
        .ok_or_else(|| format!("test count `{field}` missing"))
}

/// The committed receipt of distilled candidate `name`.
pub fn distill_receipt_path(name: &str) -> String {
    format!("releases/distill/{name}.json")
}

/// A candidate name `local-distill` can publish as `release/<name>`.
pub fn valid_candidate_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && !name.starts_with(['-', '.'])
        && !name.ends_with(['-', '.'])
        && !name.contains("..")
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
}

/// The digest `local-distill` records as `receipt_sha256`: SHA-256 of the
/// compact JSON of the receipt without that field (`distill_compose.rs`).
pub fn distill_receipt_digest(receipt: &Value) -> Result<String, String> {
    let mut body = receipt.clone();
    body.as_object_mut()
        .ok_or("the distillation receipt is not a JSON object")?
        .remove("receipt_sha256");
    let canonical = serde_json::to_vec(&body).map_err(|e| e.to_string())?;
    Ok(sha256_hex(&canonical))
}

fn hex_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn git_ok(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .current_dir(root)
        .arg("--no-replace-objects")
        .args(args)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Check that the receipt of distilled candidate `name`, as committed in
/// `head`'s tree, makes `head` a promotable release source, and return the
/// `distillation` block a qualification record carries for it.
///
/// Refuses unless: the receipt parses and its `receipt_sha256` re-computes;
/// it names this candidate and is `accept_verified_candidate`; it rejected and
/// deferred nothing; every commit the Git host created in the distilled stream
/// was adopted on record; every acceptance command passed and every included
/// task has one; its envelope commit E (the commit that added the receipt) has
/// the composed commit as its only parent, changes only the receipt, and is an
/// ancestor of `head`; the composed tree is the one recorded; and `head`
/// differs from E only by a version bump ([`verify_bump_only`]).
pub fn verify_distilled_source(root: &Path, name: &str, head: &str) -> Result<Value, String> {
    if !valid_candidate_name(name) {
        return Err(format!("`{name}` is not a distilled candidate name"));
    }
    let head = git_text(
        root,
        &["rev-parse", "--verify", &format!("{head}^{{commit}}")],
    )?;
    let path = distill_receipt_path(name);
    let bytes = git_bytes(root, &["cat-file", "blob", &format!("{head}:{path}")])
        .map_err(|_| format!("{path} is not in the tree of {head}"))?;
    let receipt: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path} is not JSON: {e}"))?;
    let recorded = text(&receipt, "receipt_sha256")
        .map_err(|_| format!("{path} has no receipt_sha256"))?
        .to_owned();
    if distill_receipt_digest(&receipt)? != recorded {
        return Err(format!(
            "{path}: receipt_sha256 does not re-compute; the receipt was changed after distillation"
        ));
    }
    let expect = |field: &str, wanted: &str| -> Result<(), String> {
        let found = receipt[field]
            .as_str()
            .ok_or_else(|| format!("{path}: `{field}` missing"))?;
        if found == wanted {
            Ok(())
        } else {
            Err(format!("{path}: `{field}` is {found}, expected {wanted}"))
        }
    };
    expect("schema", DISTILL_RECEIPT_SCHEMA)?;
    expect("name", name)?;
    expect("branch_ref", &format!("refs/heads/release/{name}"))?;
    expect("qualification_status", "accept_verified_candidate")?;
    let array = |field: &str| -> Result<&Vec<Value>, String> {
        receipt[field]
            .as_array()
            .ok_or_else(|| format!("{path}: `{field}` is not a list"))
    };
    for field in ["rejected", "deferred"] {
        let tasks = array(field)?;
        if !tasks.is_empty() {
            let ids: Vec<&str> = tasks
                .iter()
                .map(|t| t["task_id"].as_str().unwrap_or("?"))
                .collect();
            return Err(format!(
                "{path}: the candidate {field} tasks ({}); only a complete candidate is promoted",
                ids.join(", ")
            ));
        }
    }
    let adopted = receipt["adopted_host_commits"].as_object().ok_or_else(|| {
        format!("{path}: no `adopted_host_commits`; the receipt predates host-commit accounting, distill the candidate again")
    })?;
    for commit in array("git_host_created_in_stream")? {
        let id = commit["commit"]
            .as_str()
            .ok_or_else(|| format!("{path}: a host-created commit names no id"))?;
        if !adopted.contains_key(id) {
            return Err(format!(
                "{path}: commit {id} was created by the Git host and not adopted; merges happen in Susi"
            ));
        }
    }
    let included: Vec<&str> = array("included")?
        .iter()
        .map(|task| {
            task["task_id"]
                .as_str()
                .ok_or_else(|| format!("{path}: an included task names no id"))
        })
        .collect::<Result<_, _>>()?;
    if included.is_empty() && array("satisfied_in_base")?.is_empty() {
        return Err(format!("{path}: the candidate includes no task"));
    }
    let checks = array("accept_verification")?;
    for check in checks {
        let task = check["task_id"].as_str().unwrap_or("?");
        if check["exit_code"].as_i64() != Some(0) || check["passed"].as_bool() != Some(true) {
            return Err(format!(
                "{path}: the acceptance command of {task} did not pass"
            ));
        }
    }
    for task in &included {
        if !checks.iter().any(|c| c["task_id"].as_str() == Some(task)) {
            return Err(format!(
                "{path}: included task {task} has no acceptance verification"
            ));
        }
    }
    let final_commit = text(&receipt, "final_commit")?.to_owned();
    let final_tree = text(&receipt, "final_tree")?.to_owned();
    let base_commit = text(&receipt, "base_commit")?.to_owned();
    for (field, id) in [
        ("final_commit", &final_commit),
        ("final_tree", &final_tree),
        ("base_commit", &base_commit),
    ] {
        if !hex_id(id) {
            return Err(format!("{path}: `{field}` is not an object id"));
        }
    }
    let envelope = git_text(root, &["rev-list", "-n", "1", &head, "--", &path])?;
    if envelope.is_empty() {
        return Err(format!("no commit in {head}'s history adds {path}"));
    }
    let lineage = git_text(root, &["rev-list", "--parents", "-n", "1", &envelope])?;
    let parents: Vec<&str> = lineage.split_whitespace().skip(1).collect();
    if parents != [final_commit.as_str()] {
        return Err(format!(
            "the envelope commit {envelope} of {path} must have the composed commit {final_commit} as its only parent"
        ));
    }
    let changed = git_text(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--no-renames",
            "--name-only",
            "-r",
            &final_commit,
            &envelope,
        ],
    )?;
    if changed != path {
        return Err(format!(
            "the envelope commit {envelope} must change only {path}, not: {changed}"
        ));
    }
    let blob = |rev: &str| git_text(root, &["rev-parse", &format!("{rev}:{path}")]);
    if blob(&envelope)? != blob(&head)? {
        return Err(format!(
            "{path} in {head} is not the receipt its envelope {envelope} committed"
        ));
    }
    if git_text(root, &["rev-parse", &format!("{final_commit}^{{tree}}")])? != final_tree {
        return Err(format!(
            "{path}: `final_tree` is not the tree of the composed commit {final_commit}"
        ));
    }
    if !git_ok(root, &["merge-base", "--is-ancestor", &envelope, &head]) {
        return Err(format!(
            "the envelope commit {envelope} is not an ancestor of {head}"
        ));
    }
    verify_bump_only(root, &envelope, &head)?;
    let mut adopted_ids: Vec<&str> = adopted.keys().map(String::as_str).collect();
    adopted_ids.sort_unstable();
    Ok(serde_json::json!({
        "candidate": name,
        "receipt_path": path,
        "receipt_sha256": recorded,
        "envelope_commit": envelope,
        "base_commit": base_commit,
        "final_commit": final_commit,
        "final_tree": final_tree,
        "included_tasks": included,
        "adopted_host_commits": adopted_ids,
    }))
}

/// Check that `head` differs from `from` only by a version bump: modified
/// files are in [`BUMP_FILES`] and each changed line differs only by the old
/// package version becoming the new one; at most one file is added, the
/// bump's task record directly under [`BUMP_TASK_DIR`]; nothing is deleted.
pub fn verify_bump_only(root: &Path, from: &str, head: &str) -> Result<(), String> {
    let version_at = |rev: &str| -> Result<String, String> {
        let manifest = git_text(root, &["cat-file", "blob", &format!("{rev}:Cargo.toml")])?;
        package_version(&manifest).ok_or_else(|| format!("Cargo.toml of {rev} names no version"))
    };
    let (old, new) = (version_at(from)?, version_at(head)?);
    let status = git_text(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--no-renames",
            "--name-status",
            "-r",
            from,
            head,
        ],
    )?;
    let mut task_records = 0;
    for line in status.lines() {
        let (kind, file) = line
            .split_once('\t')
            .ok_or_else(|| format!("unexpected diff line: {line}"))?;
        match kind {
            "M" if BUMP_FILES.contains(&file) => {
                let read =
                    |rev: &str| git_bytes(root, &["cat-file", "blob", &format!("{rev}:{file}")]);
                let (before, after) = (read(from)?, read(head)?);
                let before = String::from_utf8_lossy(&before);
                let after = String::from_utf8_lossy(&after);
                let (before, after): (Vec<&str>, Vec<&str>) =
                    (before.split('\n').collect(), after.split('\n').collect());
                let only_version = before.len() == after.len()
                    && before.iter().zip(&after).all(|(b, a)| {
                        b == a || (b.contains(old.as_str()) && b.replace(old.as_str(), &new) == *a)
                    });
                if !only_version {
                    return Err(format!(
                        "{file} changes more than the version {old} -> {new} after the candidate"
                    ));
                }
            }
            "A" if file
                .strip_prefix(BUMP_TASK_DIR)
                .is_some_and(|name| !name.contains('/') && name.ends_with(".json")) =>
            {
                task_records += 1;
                if task_records > 1 {
                    return Err(format!(
                        "the version bump adds more than one task record ({file})"
                    ));
                }
                let bytes = git_bytes(root, &["cat-file", "blob", &format!("{head}:{file}")])?;
                serde_json::from_slice::<Value>(&bytes)
                    .map_err(|e| format!("the bump's task record {file} is not JSON: {e}"))?;
            }
            _ => {
                return Err(format!(
                    "{file} ({kind}) is not part of a version bump; a distilled candidate takes nothing but the bump before qualification"
                ));
            }
        }
    }
    Ok(())
}

/// Check the record's `source` and, for a distilled release, that its
/// `distillation` block is exactly what the qualified tree `parent` proves.
fn verify_source(root: &Path, record: &Value, parent: &str) -> Result<(), String> {
    let distillation = record.get("distillation").filter(|v| !v.is_null());
    match (record.get("source").and_then(Value::as_str), distillation) {
        (None, None) if record.get("source").is_none() => Ok(()),
        (Some(SOURCE_SNAPSHOT), None) => Ok(()),
        (Some(SOURCE_DISTILL), Some(block)) => {
            let name = block["candidate"]
                .as_str()
                .ok_or("record `distillation` names no candidate")?;
            let proven = verify_distilled_source(root, name, parent)?;
            if &proven == block {
                Ok(())
            } else {
                Err(format!(
                    "record `distillation` does not match what the qualified tree proves for {name}"
                ))
            }
        }
        (Some(SOURCE_DISTILL), None) => {
            Err("record source is distill but has no `distillation`".into())
        }
        (_, Some(_)) => {
            Err("record has a `distillation` block but its source is not distill".into())
        }
        _ => Err("record `source` must be snapshot or distill".into()),
    }
}

/// Check that `new` (a tag object or commit) is a qualified release `tag`.
pub fn verify_tag(root: &Path, tag: &str, new: &str) -> Result<(), String> {
    let version = release_version(tag).ok_or("not a vX.Y.Z release tag")?;
    let commit = git_text(
        root,
        &["rev-parse", "--verify", &format!("{new}^{{commit}}")],
    )?;
    let lineage = git_text(root, &["rev-list", "--parents", "-n", "1", &commit])?;
    let parents: Vec<&str> = lineage.split_whitespace().skip(1).collect();
    let [parent] = parents.as_slice() else {
        return Err("the tagged commit must be the record commit, with one parent".into());
    };
    let path = record_path(tag);
    let changed = git_text(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--no-renames",
            "--name-only",
            "-r",
            parent,
            &commit,
        ],
    )?;
    if changed != path {
        return Err(format!(
            "the tagged commit must change only {path}, not: {changed}"
        ));
    }
    let bytes = git_bytes(root, &["cat-file", "blob", &format!("{commit}:{path}")])?;
    let record: Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{path} is not JSON: {e}"))?;
    let expect = |field: &str, wanted: &str| -> Result<(), String> {
        let found = text(&record, field)?;
        if found == wanted {
            Ok(())
        } else {
            Err(format!("record `{field}` is {found}, expected {wanted}"))
        }
    };
    expect("schema", SCHEMA)?;
    expect("product", "susi")?;
    expect("tag", tag)?;
    expect("version", version)?;
    expect("qualified_commit", parent)?;
    let tree = git_text(root, &["rev-parse", &format!("{parent}^{{tree}}")])?;
    expect("qualified_tree", &tree)?;
    let manifest = git_text(root, &["cat-file", "blob", &format!("{parent}:Cargo.toml")])?;
    if package_version(&manifest).as_deref() != Some(version) {
        return Err(format!(
            "Cargo.toml of the qualified tree is not version {version}"
        ));
    }
    let lock = git_bytes(root, &["cat-file", "blob", &format!("{parent}:Cargo.lock")])?;
    expect("cargo_lock_sha256", &sha256_hex(&lock))?;
    let pin = git_text(
        root,
        &["cat-file", "blob", &format!("{parent}:rust-toolchain.toml")],
    )?;
    let channel = toolchain_channel(&pin).ok_or("rust-toolchain.toml names no channel")?;
    if record["toolchain"]["channel"].as_str() != Some(channel.as_str()) {
        return Err(format!("record toolchain is not the pinned {channel}"));
    }
    verify_source(root, &record, parent)?;
    if text(&record, "host")?.is_empty() {
        return Err("record names no host".into());
    }
    let gates = record["gates"].as_array().ok_or("record has no gates")?;
    for name in REQUIRED_GATES {
        let gate = gates
            .iter()
            .find(|gate| gate["name"].as_str() == Some(name))
            .ok_or_else(|| format!("gate `{name}` did not run"))?;
        let argv = gate["argv"].as_array().filter(|argv| !argv.is_empty());
        if argv.is_none() {
            return Err(format!("gate `{name}` records no command"));
        }
        if gate["exit_code"].as_i64() != Some(0) {
            return Err(format!("gate `{name}` did not exit 0"));
        }
        if name == "tests" {
            let tests = &gate["tests"];
            if count(tests, "binaries")? == 0 || count(tests, "passed")? == 0 {
                return Err("the tests gate ran no tests".into());
            }
            if count(tests, "failed")? != 0 {
                return Err("the tests gate recorded failures".into());
            }
        }
    }
    let assets = record["assets"]
        .as_array()
        .filter(|assets| !assets.is_empty())
        .ok_or("record has no built assets")?;
    for asset in assets {
        let file = asset["file"].as_str().unwrap_or("?");
        let digest = asset["sha256"].as_str().unwrap_or("");
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("asset {file} has no sha256"));
        }
        if !asset["version_output"]
            .as_str()
            .is_some_and(|out| out.contains(version))
        {
            return Err(format!("asset {file} did not report version {version}"));
        }
    }
    Ok(())
}
