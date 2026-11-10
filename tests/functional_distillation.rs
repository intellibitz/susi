#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! End-to-end integration proof for Wave 3 Functional-Only Release Distillation
//! and Confined In-Tree Symlinked-License Harvest Resolution:
//! 1. `distill plan --functional-only` and `local-distill plan|compose|orchestrate --functional-only`
//!    deterministically distill ONLY functional core commits from a mixed `base..main`
//!    stream (`feat(core): ...`, `receipt: gated landing ...`, `docs: ...`,
//!    `chore(release): ...`, `chore(tasks): ...`), skipping non-functional tasks
//!    without blocking functional tasks that declare `Depends:` on a skipped
//!    doc-only task, and stripping post-landing receipt commits without triggering
//!    `out_of_scope_diff`.
//! 2. The composed immutable release candidate passes `task_records` validation,
//!    `distill lint --strict`, and `release-qualify --from-distill --verify-only`.
//! 3. `local-harvest scan` and `local-harvest pickup` resolve Git `120000`
//!    symlinked `LICENSE` and `NOTICE` entries strictly confined inside the
//!    quarantined commit tree (e.g. `minijinja/LICENSE -> ../LICENSE`, <= 4 hops,
//!    regular blob target), while continuing to hard-reject escaping symlinks,
//!    absolute symlinks, > 4-hop symlink chains, directory-target symlinks, and
//!    symlinked `.rs` source files.

mod common;
use common::bins_are_fresh;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const APACHE_LICENSE_TEXT: &str = "\
                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, provided that You meet the following conditions:
      (a) You must give any other recipients of the Work a copy of this License;
      (b) You must cause any modified files to carry prominent notices;
      (c) You must retain all copyright, patent, trademark, and attribution notices;
      (d) If the Work includes a NOTICE text file, any Derivative Works must include it.
";

static SANDBOX_SEQ: AtomicUsize = AtomicUsize::new(0);
static XTASK_BINS: OnceLock<XtaskBins> = OnceLock::new();

struct XtaskBins {
    distill: PathBuf,
    local_distill: PathBuf,
    local_harvest: PathBuf,
    release_qualify: PathBuf,
}

fn xtask_bins() -> &'static XtaskBins {
    XTASK_BINS.get_or_init(|| {
        let exe = std::env::current_exe().expect("current test executable path");
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("target/<profile> directory");
        let suffix = std::env::consts::EXE_SUFFIX;
        let bins = XtaskBins {
            distill: profile_dir.join(format!("distill{suffix}")),
            local_distill: profile_dir.join(format!("local-distill{suffix}")),
            local_harvest: profile_dir.join(format!("local-harvest{suffix}")),
            release_qualify: profile_dir.join(format!("release-qualify{suffix}")),
        };
        let bin_paths = [
            bins.distill.as_path(),
            bins.local_distill.as_path(),
            bins.local_harvest.as_path(),
            bins.release_qualify.as_path(),
        ];
        let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        if !bins_are_fresh(&workspace_root, &bin_paths) {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let target_dir = profile_dir.parent().expect("target root directory");
            let mut build_cmd = Command::new(cargo);
            build_cmd
                .current_dir(&workspace_root)
                .env("CARGO_TARGET_DIR", target_dir)
                .args([
                    "build",
                    "--offline",
                    "--locked",
                    "-p",
                    "xtask",
                    "--bin",
                    "distill",
                    "--bin",
                    "local-distill",
                    "--bin",
                    "local-harvest",
                    "--bin",
                    "release-qualify",
                ]);
            if let Some(profile_name) = profile_dir.file_name().and_then(|n| n.to_str()) {
                if profile_name == "release" {
                    build_cmd.arg("--release");
                } else if profile_name != "debug" {
                    build_cmd.args(["--profile", profile_name]);
                }
            }
            let out = build_cmd.output().expect("build xtask binaries");
            assert!(
                out.status.success(),
                "building xtask binaries failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        for bin in bin_paths {
            assert!(bin.is_file(), "missing binary {}", bin.display());
        }
        bins
    })
}

fn null_config() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(label: &str) -> Self {
        let seq = SANDBOX_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "susi-func-distill-{label}-{}-{seq}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create sandbox dir");
        Self { dir }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn git_cmd(cwd: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .args(["-c", "gc.auto=0"])
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        .env("GIT_AUTHOR_NAME", "Susi Test")
        .env("GIT_AUTHOR_EMAIL", "test@intellibitz.com")
        .env("GIT_COMMITTER_NAME", "Susi Test")
        .env("GIT_COMMITTER_EMAIL", "test@intellibitz.com")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_NO_LAZY_FETCH", "1");
    cmd
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = git_cmd(cwd, args).output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn git_stdin(cwd: &Path, args: &[&str], stdin_bytes: &[u8]) -> String {
    let mut child = git_cmd(cwd, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git with stdin");
    child
        .stdin
        .take()
        .expect("stdin pipe")
        .write_all(stdin_bytes)
        .expect("write git stdin");
    let out = child.wait_with_output().expect("wait git");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        cwd.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn init_bare_tree_repo(
    parent: &Path,
    bare: &Path,
    message: &str,
    entries: &[(&str, &str, &str)],
) -> String {
    git(
        parent,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            bare.to_str().unwrap(),
        ],
    );
    let mut stream = Vec::with_capacity(4096);
    writeln!(stream, "commit refs/heads/main").unwrap();
    writeln!(
        stream,
        "author Susi Test <test@intellibitz.com> 1700000000 +0000"
    )
    .unwrap();
    writeln!(
        stream,
        "committer Susi Test <test@intellibitz.com> 1700000000 +0000"
    )
    .unwrap();
    writeln!(stream, "data {}", message.len()).unwrap();
    stream.extend_from_slice(message.as_bytes());
    stream.push(b'\n');
    for (mode, path, content) in entries {
        if *mode == "160000" {
            writeln!(stream, "M 160000 {content} {path}").unwrap();
        } else {
            writeln!(stream, "M {mode} inline {path}").unwrap();
            writeln!(stream, "data {}", content.len()).unwrap();
            stream.extend_from_slice(content.as_bytes());
            stream.push(b'\n');
        }
    }
    stream.push(b'\n');
    git_stdin(bare, &["fast-import", "--quiet"], &stream);
    fs::read_to_string(bare.join("refs/heads/main"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| s.len() == 40)
        .unwrap_or_else(|| git(bare, &["rev-parse", "refs/heads/main"]))
}

fn run_bin(bin: &Path, cwd: &Path, target_dir: &Path, args: &[&str]) -> Output {
    Command::new(bin)
        .current_dir(cwd)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env("CARGO_TARGET_DIR", target_dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_config())
        .env("GIT_AUTHOR_NAME", "Susi Distill")
        .env("GIT_AUTHOR_EMAIL", "distill@intellibitz.com")
        .env("GIT_COMMITTER_NAME", "Susi Distill")
        .env("GIT_COMMITTER_EMAIL", "distill@intellibitz.com")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .unwrap_or_else(|e| panic!("failed to run {}: {e}", bin.display()))
}

fn write_rel(root: &Path, rel: &str, content: &str) {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(full, content).unwrap();
}

struct TaskRecordSpec<'a> {
    id: &'a str,
    feature_id: &'a str,
    title: &'a str,
    deps: &'a [&'a str],
    scopes: &'a [&'a str],
    accept_cmd: &'a [&'a str],
    created_unix: u64,
}

fn write_task_record(root: &Path, spec: &TaskRecordSpec<'_>) {
    let payload = json!({
        "id": spec.id,
        "feature_id": spec.feature_id,
        "title": spec.title,
        "goal": spec.title,
        "status": "done",
        "state": "done",
        "created_by": "carbon",
        "created_unix": spec.created_unix,
        "deps": spec.deps,
        "scopes": spec.scopes,
        "accept": {
            "cmd": spec.accept_cmd,
        }
    });
    let mut text = serde_json::to_string_pretty(&payload).unwrap();
    text.push('\n');
    write_rel(root, &format!(".agents/tasks/{}.json", spec.id), &text);
}

/// Mirror the `tests/task_records.rs` runtime task-record contract on a Git tree.
#[derive(Deserialize)]
struct TaskRecordCheck {
    id: String,
    title: String,
    accept: TaskAcceptCheck,
    created_by: String,
    created_unix: u64,
}

#[derive(Deserialize)]
struct TaskAcceptCheck {
    cmd: Vec<String>,
}

fn assert_tree_task_records_valid(repo: &Path, rev: &str) -> Vec<String> {
    let listing = git(
        repo,
        &["ls-tree", "-r", "--name-only", rev, ".agents/tasks"],
    );
    let mut ids = Vec::new();
    for path in listing.lines().filter(|l| l.ends_with(".json")) {
        let raw = git(repo, &["cat-file", "blob", &format!("{rev}:{path}")]);
        let parsed: TaskRecordCheck = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("invalid task record {path} in {rev}: {e}"));
        assert!(!parsed.id.is_empty());
        assert!(!parsed.title.is_empty());
        assert!(!parsed.created_by.is_empty());
        assert!(parsed.created_unix > 0);
        assert_eq!(
            parsed.accept.cmd.first().map(String::as_str),
            Some("cargo"),
            "unexpected accept.cmd in {path}: {:?}",
            parsed.accept.cmd
        );
        ids.push(parsed.id);
    }
    ids.sort();
    ids
}

#[test]
fn functional_only_distills_core_commits_and_passes_lint_task_records_and_release_qualify() {
    let bins = xtask_bins();
    let sandbox = Sandbox::new("distill-core");
    let origin = sandbox.dir.join("origin.git");
    let work = sandbox.dir.join("work");
    let cargo_target = sandbox.dir.join("cargo-target");
    fs::create_dir_all(&work).unwrap();
    fs::create_dir_all(&cargo_target).unwrap();

    git(
        &sandbox.dir,
        &["init", "--quiet", "--bare", origin.to_str().unwrap()],
    );
    git(&work, &["init", "--quiet", "--initial-branch=main"]);
    git(
        &work,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );

    let workspace_toolchain =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("rust-toolchain.toml"))
            .expect("read workspace rust-toolchain.toml");
    write_rel(&work, "rust-toolchain.toml", &workspace_toolchain);
    write_rel(
        &work,
        "Cargo.toml",
        "[package]\nname = \"demo-core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"crates/core/src/lib.rs\"\n",
    );
    write_rel(&work, "README.md", "# Demo Core v0.1.0\n");
    write_rel(
        &work,
        "crates/core/src/lib.rs",
        "pub fn boot_banner() -> &'static str {\n    \"demo-core\"\n}\n",
    );

    // Generate deterministic Cargo.lock via offline lockfile generation on the zero-dep crate.
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let check_out = Command::new(&cargo)
        .current_dir(&work)
        .env("CARGO_TARGET_DIR", &cargo_target)
        .args(["generate-lockfile", "--offline"])
        .output()
        .expect("generate Cargo.lock via cargo generate-lockfile --offline");
    assert!(
        check_out.status.success(),
        "initial cargo generate-lockfile failed: {}",
        String::from_utf8_lossy(&check_out.stderr)
    );
    assert!(work.join("Cargo.lock").is_file());

    git(&work, &["add", "."]);
    git(&work, &["commit", "-qm", "chore: initial release 0.1.0"]);
    git(&work, &["tag", "-a", "v0.1.0", "-m", "Release v0.1.0"]);
    git(&work, &["push", "--quiet", "origin", "main", "v0.1.0"]);

    let base_readme = fs::read_to_string(work.join("README.md")).unwrap();
    let base_cargo_toml = fs::read_to_string(work.join("Cargo.toml")).unwrap();

    // 1. Functional feature commit: T-FUNC-1
    write_rel(
        &work,
        "crates/core/src/router.rs",
        "pub fn route(req: &str) -> &'static str {\n    if req == \"ping\" { \"pong\" } else { \"unknown\" }\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn routes_ping() {\n        assert_eq!(route(\"ping\"), \"pong\");\n    }\n}\n",
    );
    write_rel(
        &work,
        "crates/core/src/lib.rs",
        "pub mod router;\n\npub fn boot_banner() -> &'static str {\n    \"demo-core\"\n}\n",
    );
    write_task_record(
        &work,
        &TaskRecordSpec {
            id: "T-FUNC-1",
            feature_id: "F-FUNC-1",
            title: "Add deterministic core router",
            deps: &[],
            scopes: &[".agents/tasks/T-FUNC-1.json", "crates/core"],
            accept_cmd: &["cargo", "test", "--offline", "--locked"],
            created_unix: 1_794_429_001,
        },
    );
    git(&work, &["add", "."]);
    git(
        &work,
        &[
            "commit",
            "-qm",
            "feat(core): add deterministic router\n\nTask: T-FUNC-1\nFeature: F-FUNC-1",
        ],
    );

    // 2. Non-functional gate receipt commit carrying `Task: T-FUNC-1` (touches
    // `.agents/receipts/*` outside T-FUNC-1's declared scopes, mirroring local-gate).
    write_rel(
        &work,
        ".agents/receipts/1111111.json",
        "{\"schema\":\"susi-local-gate/v1\",\"task\":\"T-FUNC-1\",\"passed\":true}\n",
    );
    git(&work, &["add", "."]);
    git(
        &work,
        &[
            "commit",
            "-qm",
            "receipt: gated landing 1111111\n\nTask: T-FUNC-1",
        ],
    );

    // 3. Non-functional docs-only task: T-DOC-1
    write_rel(
        &work,
        "docs/router.md",
        "# Router Architecture\n\nDevelopment documentation.\n",
    );
    write_rel(
        &work,
        "README.md",
        "# Demo Core v0.1.0\n\nSee docs/router.md for development notes.\n",
    );
    write_task_record(
        &work,
        &TaskRecordSpec {
            id: "T-DOC-1",
            feature_id: "F-DOC-1",
            title: "Document router architecture",
            deps: &[],
            scopes: &[".agents/tasks/T-DOC-1.json", "docs", "README.md"],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_002,
        },
    );
    git(&work, &["add", "."]);
    git(
        &work,
        &[
            "commit",
            "-qm",
            "docs: document router architecture\n\nTask: T-DOC-1\nFeature: F-DOC-1",
        ],
    );

    // 4. Non-functional release-record housekeeping commit: T-RELREC-1
    write_rel(
        &work,
        "releases/v0.1.0.json",
        "{\"tag\":\"v0.1.0\",\"recorded\":true}\n",
    );
    write_rel(
        &work,
        "Cargo.toml",
        "# recorded release v0.1.0 on main\n[package]\nname = \"demo-core\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\npath = \"crates/core/src/lib.rs\"\n",
    );
    write_task_record(
        &work,
        &TaskRecordSpec {
            id: "T-RELREC-1",
            feature_id: "F-RELREC-1",
            title: "Record v0.1.0 release ledger",
            deps: &[],
            scopes: &[".agents/tasks/T-RELREC-1.json", "releases", "Cargo.toml"],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_003,
        },
    );
    git(&work, &["add", "."]);
    git(
        &work,
        &[
            "commit",
            "-qm",
            "chore(release): record v0.1.0\n\nTask: T-RELREC-1\nFeature: F-RELREC-1",
        ],
    );

    // 5. Non-functional task/evidence housekeeping commit: T-CHORE-1
    write_rel(
        &work,
        ".agents/evidence/core-health.json",
        "{\"healthy\":true}\n",
    );
    write_task_record(
        &work,
        &TaskRecordSpec {
            id: "T-CHORE-1",
            feature_id: "F-CHORE-1",
            title: "Record core health evidence",
            deps: &[],
            scopes: &[".agents/tasks/T-CHORE-1.json", ".agents/evidence"],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_004,
        },
    );
    git(&work, &["add", "."]);
    git(
        &work,
        &[
            "commit",
            "-qm",
            "chore(tasks): refresh core health evidence\n\nTask: T-CHORE-1\nFeature: F-CHORE-1",
        ],
    );

    // 6. Second functional commit: T-FUNC-2, which depends on BOTH functional
    // T-FUNC-1 AND non-functional doc-only T-DOC-1!
    write_rel(
        &work,
        "crates/core/src/replay.rs",
        "use crate::router::route;\n\npub fn replay_turn(turn: &str) -> String {\n    format!(\"replayed:{}\", route(turn))\n}\n\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn replays_router_turn() {\n        assert_eq!(replay_turn(\"ping\"), \"replayed:pong\");\n    }\n}\n",
    );
    write_rel(
        &work,
        "crates/core/src/lib.rs",
        "pub mod replay;\npub mod router;\n\npub fn boot_banner() -> &'static str {\n    \"demo-core\"\n}\n",
    );
    write_task_record(
        &work,
        &TaskRecordSpec {
            id: "T-FUNC-2",
            feature_id: "F-FUNC-2",
            title: "Add multi-turn session replay",
            deps: &["T-FUNC-1", "T-DOC-1"],
            scopes: &[".agents/tasks/T-FUNC-2.json", "crates/core"],
            accept_cmd: &["cargo", "test", "--offline", "--locked"],
            created_unix: 1_794_429_005,
        },
    );
    git(&work, &["add", "."]);
    git(
        &work,
        &[
            "commit",
            "-qm",
            "feat(core): add multi-turn session replay\n\nTask: T-FUNC-2\nFeature: F-FUNC-2",
        ],
    );
    git(&work, &["push", "--quiet", "origin", "main"]);

    // (A) Baseline WITHOUT `--functional-only`: receipt commit triggers
    // `out_of_scope_diff` on T-FUNC-1 and sweeps in housekeeping tasks.
    let raw_plan_out = run_bin(
        &bins.local_distill,
        &work,
        &cargo_target,
        &[
            "plan",
            "--name",
            "raw-baseline",
            "--base",
            "v0.1.0",
            "--from",
            "main",
            "--tasks-from-range",
            "v0.1.0..main",
            "--json",
        ],
    );
    assert!(
        raw_plan_out.status.success(),
        "non-strict raw plan failed: {}",
        String::from_utf8_lossy(&raw_plan_out.stderr)
    );
    let raw_plan: Value = serde_json::from_slice(&raw_plan_out.stdout).unwrap();
    assert_eq!(raw_plan["functional_only"], json!(false));
    assert_eq!(raw_plan["skipped_non_functional_tasks"], json!([]));
    assert!(
        raw_plan["rejected"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["task_id"] == "T-FUNC-1" && r["reason_code"] == "out_of_scope_diff"),
        "expected raw plan to reject T-FUNC-1 for receipt out_of_scope_diff: {raw_plan}"
    );
    let raw_included: Vec<&str> = raw_plan["included"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["task_id"].as_str())
        .collect();
    assert!(
        raw_included.contains(&"T-DOC-1") && raw_included.contains(&"T-CHORE-1"),
        "expected raw plan to sweep in non-functional tasks: {raw_included:?}"
    );

    // (B) `distill plan --tasks-from-range v0.1.0..main --functional-only --json`
    let distill_plan_out = run_bin(
        &bins.distill,
        &work,
        &cargo_target,
        &[
            "plan",
            "--tasks-from-range",
            "v0.1.0..main",
            "--functional-only",
            "--json",
        ],
    );
    assert!(
        distill_plan_out.status.success(),
        "distill plan --functional-only failed: {}",
        String::from_utf8_lossy(&distill_plan_out.stderr)
    );
    let distill_plan: Value = serde_json::from_slice(&distill_plan_out.stdout).unwrap();
    assert_eq!(distill_plan["functional_only"], json!(true));
    assert_eq!(
        distill_plan["skipped_non_functional_tasks"],
        json!(["T-CHORE-1", "T-DOC-1", "T-RELREC-1"])
    );
    let distill_included: Vec<&str> = distill_plan["included"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["id"].as_str())
        .collect();
    assert_eq!(distill_included, vec!["T-FUNC-1", "T-FUNC-2"]);
    assert_eq!(distill_plan["deferred"], json!([]));
    assert_eq!(distill_plan["rejected"], json!([]));

    // (C) `local-distill compose --release-candidate --functional-only --verify-accept --strict`
    let compose_out = run_bin(
        &bins.local_distill,
        &work,
        &cargo_target,
        &[
            "compose",
            "--name",
            "v0-1-1-distilled-rc1",
            "--base",
            "v0.1.0",
            "--from",
            "main",
            "--tasks-from-range",
            "v0.1.0..main",
            "--release-candidate",
            "--functional-only",
            "--verify-accept",
            "--strict",
            "--json",
        ],
    );
    assert!(
        compose_out.status.success(),
        "local-distill compose --functional-only failed: {}",
        String::from_utf8_lossy(&compose_out.stderr)
    );
    let receipt: Value = serde_json::from_slice(&compose_out.stdout).unwrap();
    assert_eq!(receipt["schema"], json!("susi-distillation-receipt-v1"));
    assert_eq!(
        receipt["qualification_status"],
        json!("accept_verified_candidate")
    );
    assert_eq!(receipt["functional_only"], json!(true));
    assert_eq!(
        receipt["skipped_non_functional_tasks"],
        json!(["T-CHORE-1", "T-DOC-1", "T-RELREC-1"])
    );
    assert_eq!(receipt["deferred"], json!([]));
    assert_eq!(receipt["rejected"], json!([]));
    let composed_included: Vec<&str> = receipt["included"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["task_id"].as_str())
        .collect();
    assert_eq!(composed_included, vec!["T-FUNC-1", "T-FUNC-2"]);

    let final_commit = receipt["final_commit"].as_str().unwrap();
    let tree_files: BTreeSet<String> = git(&work, &["ls-tree", "-r", "--name-only", final_commit])
        .lines()
        .map(str::to_owned)
        .collect();

    // Functional files and functional task records ARE in the composed candidate.
    for expected in [
        "crates/core/src/lib.rs",
        "crates/core/src/router.rs",
        "crates/core/src/replay.rs",
        ".agents/tasks/T-FUNC-1.json",
        ".agents/tasks/T-FUNC-2.json",
    ] {
        assert!(
            tree_files.contains(expected),
            "missing functional file {expected} in composed tree: {tree_files:?}"
        );
    }

    // Non-functional docs, receipts, release ledgers, evidence, and skipped task
    // records are completely ABSENT from the composed candidate!
    for forbidden in [
        ".agents/receipts/1111111.json",
        "docs/router.md",
        "releases/v0.1.0.json",
        ".agents/evidence/core-health.json",
        ".agents/tasks/T-DOC-1.json",
        ".agents/tasks/T-RELREC-1.json",
        ".agents/tasks/T-CHORE-1.json",
    ] {
        assert!(
            !tree_files.contains(forbidden),
            "non-functional path {forbidden} leaked into composed tree: {tree_files:?}"
        );
    }
    assert_eq!(
        git(
            &work,
            &["cat-file", "blob", &format!("{final_commit}:README.md")]
        ) + "\n",
        base_readme
    );
    assert_eq!(
        git(
            &work,
            &["cat-file", "blob", &format!("{final_commit}:Cargo.toml")]
        ) + "\n",
        base_cargo_toml
    );

    // (D) Task-record schema validation on composed tree (`task_records` invariant).
    let valid_task_ids = assert_tree_task_records_valid(&work, final_commit);
    assert_eq!(valid_task_ids, vec!["T-FUNC-1", "T-FUNC-2"]);

    // (E) `distill lint --strict` on the composed candidate commit range!
    let lint_out = run_bin(
        &bins.distill,
        &work,
        &cargo_target,
        &[
            "lint",
            "--range",
            &format!("v0.1.0..{final_commit}"),
            "--strict",
            "--no-claims",
            "--json",
        ],
    );
    assert!(
        lint_out.status.success(),
        "distill lint --strict failed on composed candidate: stdout={} stderr={}",
        String::from_utf8_lossy(&lint_out.stdout),
        String::from_utf8_lossy(&lint_out.stderr)
    );
    let lint_json: Value = serde_json::from_slice(&lint_out.stdout).unwrap();
    assert_eq!(lint_json["summary"]["findings"], json!(0));
    assert_eq!(lint_json["summary"]["commits"], json!(2));
    assert_eq!(lint_json["summary"]["commits_with_task_trailer"], json!(2));
    assert_eq!(
        lint_json["summary"]["commits_without_task_trailer"],
        json!(0)
    );

    // (F) `release-qualify v0.1.1 --from-distill v0-1-1-distilled-rc1 --verify-only`
    // on a clean checkout of the candidate's envelope branch + version bump!
    let release_wt = sandbox.dir.join("release-qualify-wt");
    git(
        &work,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "qualify-v0-1-1",
            release_wt.to_str().unwrap(),
            "refs/heads/release/v0-1-1-distilled-rc1",
        ],
    );
    for bump_file in ["Cargo.toml", "Cargo.lock", "README.md"] {
        let content = fs::read_to_string(release_wt.join(bump_file)).unwrap();
        fs::write(
            release_wt.join(bump_file),
            content.replace("0.1.0", "0.1.1"),
        )
        .unwrap();
    }
    write_task_record(
        &release_wt,
        &TaskRecordSpec {
            id: "T-REL-011",
            feature_id: "F-REL-011",
            title: "Bump release candidate to v0.1.1",
            deps: &[],
            scopes: &[
                ".agents/tasks/T-REL-011.json",
                "Cargo.toml",
                "Cargo.lock",
                "README.md",
            ],
            accept_cmd: &["cargo", "test", "--offline", "--locked"],
            created_unix: 1_794_429_006,
        },
    );
    git(&release_wt, &["add", "."]);
    git(
        &release_wt,
        &[
            "commit",
            "-qm",
            "chore: bump release to v0.1.1\n\nTask: T-REL-011\nFeature: F-REL-011",
        ],
    );
    let qualify_out = run_bin(
        &bins.release_qualify,
        &release_wt,
        &cargo_target,
        &[
            "v0.1.1",
            "--from-distill",
            "v0-1-1-distilled-rc1",
            "--verify-only",
        ],
    );
    assert!(
        qualify_out.status.success(),
        "release-qualify --from-distill --verify-only failed: stdout={} stderr={}",
        String::from_utf8_lossy(&qualify_out.stdout),
        String::from_utf8_lossy(&qualify_out.stderr)
    );
    let qualify_stdout = String::from_utf8_lossy(&qualify_out.stdout);
    assert!(
        qualify_stdout.contains("verify-only: v0.1.1 from distill")
            && qualify_stdout.contains("v0-1-1-distilled-rc1"),
        "unexpected release-qualify output: {qualify_stdout}"
    );

    // (G) `local-distill orchestrate --functional-only --publish-snapshot --strict --json`
    // auto-discovers tasks from `base..from`, verifies acceptances, and publishes ONLY
    // `refs/tags/susi-distill/<name>` to `origin` (never `refs/heads/release/*`).
    let orch_out = run_bin(
        &bins.local_distill,
        &work,
        &cargo_target,
        &[
            "orchestrate",
            "--name",
            "v0-1-1-orch-rc1",
            "--base",
            "v0.1.0",
            "--from",
            "main",
            "--functional-only",
            "--verify-accept",
            "--publish-snapshot",
            "--strict",
            "--json",
        ],
    );
    assert!(
        orch_out.status.success(),
        "local-distill orchestrate --functional-only failed: {}",
        String::from_utf8_lossy(&orch_out.stderr)
    );
    let orch_receipt: Value = serde_json::from_slice(&orch_out.stdout).unwrap();
    assert_eq!(orch_receipt["functional_only"], json!(true));
    assert_eq!(
        orch_receipt["skipped_non_functional_tasks"],
        json!(["T-CHORE-1", "T-DOC-1", "T-RELREC-1"])
    );
    assert_eq!(
        orch_receipt["qualification_status"],
        json!("accept_verified_candidate")
    );
    let orch_final = orch_receipt["final_commit"].as_str().unwrap();
    assert_eq!(
        git(
            &origin,
            &["rev-parse", "refs/tags/susi-distill/v0-1-1-orch-rc1"]
        ),
        orch_final
    );
    assert!(
        git(&origin, &["for-each-ref", "refs/heads/release"]).is_empty(),
        "remote origin must never receive refs/heads/release/* branches"
    );
}

#[test]
fn harvest_resolves_confined_in_tree_symlinked_licenses_and_rejects_unsafe_symlinks() {
    let bins = xtask_bins();
    let sandbox = Sandbox::new("harvest-symlink");
    let upstream = sandbox.dir.join("multi-crate-upstream.git");
    let target = sandbox.dir.join("susi-core-target");
    let cargo_target = sandbox.dir.join("cargo-target");
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&cargo_target).unwrap();

    // Build a bare upstream repository with regular (`100644`), symlink (`120000`),
    // and gitlink (`160000`) Git tree entries via `git fast-import` so the test is
    // 100% deterministic, fast, and OS-portable (Windows & Linux).
    let upstream_sha = init_bare_tree_repo(
        &sandbox.dir,
        &upstream,
        "feat: multi-crate workspace with symlinked licenses",
        &[
            // Root pure Apache-2.0 LICENSE and NOTICE.
            ("100644", "LICENSE", APACHE_LICENSE_TEXT),
            (
                "100644",
                "NOTICE",
                "MiniJinja Workspace Attribution Notice 2026\n",
            ),
            // 1. Valid subfolder crate (`minijinja/`) with `120000` Git symlinks to `../LICENSE` & `../NOTICE`.
            ("120000", "minijinja/LICENSE", "../LICENSE"),
            ("120000", "minijinja/NOTICE", "../NOTICE"),
            (
                "100644",
                "minijinja/src/template_parser.rs",
                "// Copyright 2026 Armin Ronacher and contributors\n\npub fn parse_placeholder(input: &str) -> Option<&str> {\n    input.strip_prefix(\"{{\")?.strip_suffix(\"}}\").map(str::trim)\n}\n",
            ),
            // 2. Valid nested crate (`nested/sub/crate_a/`) with 2-hop confined symlink chain (<= 4 hops).
            (
                "120000",
                "nested/sub/crate_a/LICENSE",
                "../../../minijinja/LICENSE",
            ),
            (
                "100644",
                "nested/sub/crate_a/src/lexer.rs",
                "// Copyright 2026 Armin Ronacher and contributors\n\npub fn is_delimiter(ch: char) -> bool {\n    matches!(ch, '{' | '}')\n}\n",
            ),
            // 3. Invalid crate: symlink escaping above repository root (`../../OUTSIDE_LICENSE`).
            ("120000", "escape_crate/LICENSE", "../../OUTSIDE_LICENSE"),
            (
                "100644",
                "escape_crate/src/bad_escape.rs",
                "pub fn bad_escape() -> bool { false }\n",
            ),
            // 4. Invalid crate: absolute symlink target (`/etc/passwd`).
            ("120000", "abs_crate/LICENSE", "/etc/passwd"),
            (
                "100644",
                "abs_crate/src/bad_abs.rs",
                "pub fn bad_abs() -> bool { false }\n",
            ),
            // 5. Invalid crate: 5-hop symlink chain (> 4-hop cap).
            ("120000", "hop5_crate/L4", "../LICENSE"),
            ("120000", "hop5_crate/L3", "L4"),
            ("120000", "hop5_crate/L2", "L3"),
            ("120000", "hop5_crate/L1", "L2"),
            ("120000", "hop5_crate/LICENSE", "L1"),
            (
                "100644",
                "hop5_crate/src/too_many_hops.rs",
                "pub fn too_many_hops() -> bool { false }\n",
            ),
            // 6. Invalid crate: symlink pointing to a directory (`../minijinja/src`) / missing blob.
            ("120000", "dir_target_crate/LICENSE", "../minijinja/src"),
            (
                "100644",
                "dir_target_crate/src/bad_dir.rs",
                "pub fn bad_dir() -> bool { false }\n",
            ),
            // 6b. Invalid crate: symlink pointing to a non-regular (`160000` submodule gitlink) entry.
            (
                "160000",
                "vendor_submod",
                "1111111111111111111111111111111111111111",
            ),
            ("120000", "submod_crate/LICENSE", "../vendor_submod"),
            (
                "100644",
                "submod_crate/src/bad_submod.rs",
                "pub fn bad_submod() -> bool { false }\n",
            ),
            // 7. Invalid crate: valid symlinked LICENSE, but `.rs` source file itself is a `120000` symlink!
            ("120000", "symlink_src_crate/LICENSE", "../LICENSE"),
            (
                "120000",
                "symlink_src_crate/src/linked_source.rs",
                "../../minijinja/src/template_parser.rs",
            ),
        ],
    );

    // Initialize clean Susi Core target repo.
    git(&target, &["init", "--quiet", "--initial-branch=main"]);
    write_rel(
        &target,
        "Cargo.toml",
        "[package]\nname = \"susi\"\nversion = \"0.21.64\"\nedition = \"2021\"\n",
    );
    write_rel(&target, "crates/susi-core/src/lib.rs", "// core root\n");
    git(&target, &["add", "."]);
    git(&target, &["commit", "-qm", "chore: init susi core"]);

    let upstream_url = upstream.to_str().unwrap();

    // Intake into quarantine refs (`refs/susi/upstream/...`).
    let intake_out = run_bin(
        &bins.local_harvest,
        &target,
        &cargo_target,
        &[
            "intake",
            "--slug",
            "minijinja-ws",
            "--url",
            upstream_url,
            "--rev",
            &upstream_sha,
            "--json",
        ],
    );
    assert!(
        intake_out.status.success(),
        "intake failed: {}",
        String::from_utf8_lossy(&intake_out.stderr)
    );

    // (A) `local-harvest scan` accepts BOTH 1-hop (`minijinja/src`) and 2-hop
    // (`nested/sub/crate_a/src`) confined in-tree symlinked Apache-2.0 licenses!
    let scan_ok_out = run_bin(
        &bins.local_harvest,
        &target,
        &cargo_target,
        &[
            "scan",
            "--slug",
            "minijinja-ws",
            "--rev",
            &upstream_sha,
            "--path",
            "minijinja/src",
            "--path",
            "nested/sub/crate_a/src",
            "--json",
        ],
    );
    assert!(
        scan_ok_out.status.success(),
        "scan on confined symlinked-license crates failed: stdout={} stderr={}",
        String::from_utf8_lossy(&scan_ok_out.stdout),
        String::from_utf8_lossy(&scan_ok_out.stderr)
    );
    let scan_ok: Value = serde_json::from_slice(&scan_ok_out.stdout).unwrap();
    assert_eq!(scan_ok["ok"], json!(true));
    assert_eq!(scan_ok["accepted"], json!(2));
    assert_eq!(scan_ok["rejected"], json!(0));
    let files = scan_ok["files"].as_array().unwrap();
    assert_eq!(files[0]["path"], json!("minijinja/src/template_parser.rs"));
    assert_eq!(files[0]["license"], json!("Apache-2.0"));
    assert!(
        files[0]["basis"]
            .as_str()
            .unwrap()
            .contains("minijinja/LICENSE -> LICENSE"),
        "expected basis to show resolved symlink path: {}",
        files[0]
    );
    assert_eq!(files[0]["license_files"], json!(["LICENSE"]));
    assert_eq!(files[0]["notice_files"], json!(["NOTICE"]));
    assert_eq!(files[1]["path"], json!("nested/sub/crate_a/src/lexer.rs"));
    assert_eq!(files[1]["license"], json!("Apache-2.0"));
    assert!(
        files[1]["basis"]
            .as_str()
            .unwrap()
            .contains("nested/sub/crate_a/LICENSE -> LICENSE"),
        "expected 2-hop basis to show resolved root LICENSE: {}",
        files[1]
    );

    // (B) `local-harvest pickup` imports `minijinja/src/template_parser.rs`, writes
    // `LICENSES/Apache-2.0.txt` + `THIRD_PARTY_NOTICES` (from resolved `../NOTICE`),
    // and passes `local-harvest audit` with 0 violations!
    let pickup_out = run_bin(
        &bins.local_harvest,
        &target,
        &cargo_target,
        &[
            "pickup",
            "--id",
            "H-SYMLINK-1",
            "--slug",
            "minijinja-ws",
            "--url",
            upstream_url,
            "--rev",
            &upstream_sha,
            "--task",
            "T-SYMLINK-1",
            "--feature",
            "F-ACP-TEMPL-1",
            "--map",
            "minijinja/src/template_parser.rs=crates/susi-core/src/template_parser.rs",
            "--summary",
            "Verify confined in-tree symlinked Apache-2.0 license and NOTICE pickup",
            "--json",
        ],
    );
    assert!(
        pickup_out.status.success(),
        "pickup failed on valid symlinked-license crate: stdout={} stderr={}",
        String::from_utf8_lossy(&pickup_out.stdout),
        String::from_utf8_lossy(&pickup_out.stderr)
    );
    let pickup_json: Value = serde_json::from_slice(&pickup_out.stdout).unwrap();
    assert_eq!(pickup_json["ok"], json!(true));
    assert_eq!(pickup_json["license"], json!("Apache-2.0"));
    assert_eq!(pickup_json["route"]["effective_target"], json!("core"));
    assert!(target
        .join("crates/susi-core/src/template_parser.rs")
        .is_file());
    assert!(target.join("LICENSES/Apache-2.0.txt").is_file());
    let notices = fs::read_to_string(target.join("THIRD_PARTY_NOTICES")).unwrap();
    assert!(
        notices.contains("MiniJinja Workspace Attribution Notice 2026"),
        "expected resolved symlinked NOTICE text in THIRD_PARTY_NOTICES: {notices}"
    );

    let audit_out = run_bin(
        &bins.local_harvest,
        &target,
        &cargo_target,
        &["audit", "--json"],
    );
    assert!(
        audit_out.status.success(),
        "local-harvest audit failed: stdout={} stderr={}",
        String::from_utf8_lossy(&audit_out.stdout),
        String::from_utf8_lossy(&audit_out.stderr)
    );
    let audit_json: Value = serde_json::from_slice(&audit_out.stdout).unwrap();
    assert_eq!(audit_json["ok"], json!(true));
    assert_eq!(audit_json["violations"], json!([]));

    // (C) Hard-reject escaping symlinks, absolute symlinks, > 4-hop symlink chains,
    // directory-target symlinks, AND symlinked `.rs` source files in both `scan` & `pickup`!
    let rejection_cases = [
        (
            "H-BADESC-1",
            "escape_crate/src",
            "escape_crate/src/bad_escape.rs",
            "crates/susi-core/src/bad_escape.rs",
            "escapes repository root",
        ),
        (
            "H-BADABS-1",
            "abs_crate/src",
            "abs_crate/src/bad_abs.rs",
            "crates/susi-core/src/bad_abs.rs",
            "absolute",
        ),
        (
            "H-BADHOPS-1",
            "hop5_crate/src",
            "hop5_crate/src/too_many_hops.rs",
            "crates/susi-core/src/too_many_hops.rs",
            "exceeds maximum hop limit (4)",
        ),
        (
            "H-BADDIR-1",
            "dir_target_crate/src",
            "dir_target_crate/src/bad_dir.rs",
            "crates/susi-core/src/bad_dir.rs",
            "does not exist in quarantined commit tree",
        ),
        (
            "H-BADSUBMOD-1",
            "submod_crate/src",
            "submod_crate/src/bad_submod.rs",
            "crates/susi-core/src/bad_submod.rs",
            "not a regular file blob",
        ),
        (
            "H-BADSYMSRC-1",
            "symlink_src_crate/src",
            "symlink_src_crate/src/linked_source.rs",
            "crates/susi-core/src/linked_source.rs",
            "symbolic link cannot be harvested",
        ),
    ];

    for (hid, scan_dir, src_file, dst_file, expected_reason) in rejection_cases {
        let bad_scan = run_bin(
            &bins.local_harvest,
            &target,
            &cargo_target,
            &[
                "scan",
                "--slug",
                "minijinja-ws",
                "--rev",
                &upstream_sha,
                "--path",
                scan_dir,
                "--json",
            ],
        );
        assert!(
            !bad_scan.status.success(),
            "expected scan to reject {scan_dir}, but it succeeded"
        );
        let bad_scan_json: Value = serde_json::from_slice(&bad_scan.stdout).unwrap();
        assert_eq!(bad_scan_json["ok"], json!(false));
        assert_eq!(bad_scan_json["rejected"], json!(1));
        let reasons = bad_scan_json["files"][0]["reasons"].to_string();
        assert!(
            reasons.contains(expected_reason),
            "expected scan rejection for {scan_dir} to contain {expected_reason:?}, got {reasons}"
        );

        let bad_pickup = run_bin(
            &bins.local_harvest,
            &target,
            &cargo_target,
            &[
                "pickup",
                "--id",
                hid,
                "--slug",
                "minijinja-ws",
                "--url",
                upstream_url,
                "--rev",
                &upstream_sha,
                "--task",
                "T-BAD-1",
                "--feature",
                "F-BAD-1",
                "--map",
                &format!("{src_file}={dst_file}"),
                "--summary",
                "Must be refused",
                "--json",
            ],
        );
        assert!(
            !bad_pickup.status.success(),
            "expected pickup to reject {src_file}, but it succeeded"
        );
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&bad_pickup.stdout),
            String::from_utf8_lossy(&bad_pickup.stderr)
        );
        assert!(
            combined.contains(expected_reason),
            "expected pickup output for {src_file} to contain {expected_reason:?}, got {combined}"
        );
        assert!(
            !target.join(dst_file).exists(),
            "rejected pickup must never write destination file {dst_file}"
        );
    }
}

#[test]
fn functional_select_pulls_functional_deps_and_dual_symlink_elects_apache2() {
    let bins = xtask_bins();
    let sandbox = Sandbox::new("select-and-dual-symlink");
    let cargo_target = sandbox.dir.join("cargo-target");
    fs::create_dir_all(&cargo_target).unwrap();

    // Part 1: `--select T-APP-1 --functional-only` pulls transitive functional
    // dependency `T-CORE-DEP-1` while skipping doc-only dependency `T-DOC-GUIDE-1`.
    let repo = sandbox.dir.join("select-repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    write_rel(&repo, "src/lib.rs", "pub fn version() -> u32 { 1 }\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "chore: base"]);
    git(&repo, &["tag", "v0.1.0"]);

    write_rel(&repo, "docs/guide.md", "# Guide\n");
    write_task_record(
        &repo,
        &TaskRecordSpec {
            id: "T-DOC-GUIDE-1",
            feature_id: "F-DOC-1",
            title: "Write architecture guide",
            deps: &[],
            scopes: &[".agents/tasks/T-DOC-GUIDE-1.json", "docs"],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_101,
        },
    );
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "commit",
            "-qm",
            "docs: add architecture guide\n\nTask: T-DOC-GUIDE-1",
        ],
    );

    write_rel(
        &repo,
        "src/dep_mod.rs",
        "pub fn base_unit() -> u32 { 40 }\n",
    );
    write_task_record(
        &repo,
        &TaskRecordSpec {
            id: "T-CORE-DEP-1",
            feature_id: "F-CORE-1",
            title: "Add functional base unit module",
            deps: &[],
            scopes: &[".agents/tasks/T-CORE-DEP-1.json", "src"],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_102,
        },
    );
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "commit",
            "-qm",
            "feat(core): add base unit module\n\nTask: T-CORE-DEP-1",
        ],
    );

    write_rel(
        &repo,
        "src/lib.rs",
        "pub mod dep_mod;\npub fn total() -> u32 { dep_mod::base_unit() + 2 }\n",
    );
    write_task_record(
        &repo,
        &TaskRecordSpec {
            id: "T-APP-1",
            feature_id: "F-CORE-1",
            title: "Wire total calculation",
            deps: &["T-CORE-DEP-1", "T-DOC-GUIDE-1"],
            scopes: &[".agents/tasks/T-APP-1.json", "src"],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_103,
        },
    );
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "commit",
            "-qm",
            "feat(core): wire total calculation\n\nTask: T-APP-1",
        ],
    );

    let sel_out = run_bin(
        &bins.local_distill,
        &repo,
        &cargo_target,
        &[
            "compose",
            "--name",
            "select-func-rc1",
            "--base",
            "v0.1.0",
            "--from",
            "main",
            "--select",
            "T-APP-1",
            "--functional-only",
            "--strict",
            "--json",
        ],
    );
    assert!(
        sel_out.status.success(),
        "compose --select T-APP-1 --functional-only failed: {}",
        String::from_utf8_lossy(&sel_out.stderr)
    );
    let sel_receipt: Value = serde_json::from_slice(&sel_out.stdout).unwrap();
    let sel_inc: Vec<&str> = sel_receipt["included"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["task_id"].as_str())
        .collect();
    assert_eq!(sel_inc, vec!["T-CORE-DEP-1", "T-APP-1"]);
    assert_eq!(
        sel_receipt["skipped_non_functional_tasks"],
        json!(["T-DOC-GUIDE-1"])
    );

    // Part 2: Dual-licensed subfolder crate with `120000` Git symlinks
    // (`LICENSE-APACHE -> ../LICENSE` and `LICENSE-MIT -> ../MIT.txt`) elects
    // `"Apache-2.0"` exclusively!
    let upstream = sandbox.dir.join("dual-symlink.git");
    let sha = init_bare_tree_repo(
        &sandbox.dir,
        &upstream,
        "feat: dual symlink",
        &[
            ("100644", "LICENSE", APACHE_LICENSE_TEXT),
            (
                "100644",
                "MIT.txt",
                "MIT License\n\nCopyright (c) 2026 Contributors\n\nPermission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the \"Software\"), to deal in the Software without restriction.\n",
            ),
            ("120000", "subcrate/LICENSE-APACHE", "../LICENSE"),
            ("120000", "subcrate/LICENSE-MIT", "../MIT.txt"),
            (
                "100644",
                "subcrate/src/codec.rs",
                "// Copyright (c) 2026 Contributors\npub fn frame_len(len: u32) -> u32 { len + 4 }\n",
            ),
        ],
    );

    let target = sandbox.dir.join("dual-target");
    fs::create_dir_all(&target).unwrap();
    git(&target, &["init", "--quiet", "--initial-branch=main"]);
    write_rel(
        &target,
        "Cargo.toml",
        "[package]\nname = \"susi\"\nversion = \"0.21.64\"\nedition = \"2021\"\n",
    );
    git(&target, &["add", "."]);
    git(&target, &["commit", "-qm", "init"]);

    let pickup = run_bin(
        &bins.local_harvest,
        &target,
        &cargo_target,
        &[
            "pickup",
            "--id",
            "H-DUALSYM-1",
            "--slug",
            "dual-sym",
            "--url",
            upstream.to_str().unwrap(),
            "--rev",
            &sha,
            "--task",
            "T-DUALSYM-1",
            "--feature",
            "F-GMCP-CODEC-1",
            "--map",
            "subcrate/src/codec.rs=crates/susi-gmcp/src/codec.rs",
            "--summary",
            "Verify dual symlinked Apache-2.0 OR MIT elects Apache-2.0",
            "--json",
        ],
    );
    assert!(
        pickup.status.success(),
        "dual symlink pickup failed: stdout={} stderr={}",
        String::from_utf8_lossy(&pickup.stdout),
        String::from_utf8_lossy(&pickup.stderr)
    );
    let pj: Value = serde_json::from_slice(&pickup.stdout).unwrap();
    assert_eq!(pj["ok"], json!(true));
    assert_eq!(pj["license"], json!("Apache-2.0"));
}

#[test]
fn already_in_base_leaf_boundary_and_cross_repo_commercial_deps_do_not_block() {
    let bins = xtask_bins();
    let sandbox = Sandbox::new("base-leaf-and-cross-repo-deps");
    let cargo_target = sandbox.dir.join("cargo-target");
    fs::create_dir_all(&cargo_target).unwrap();

    // -----------------------------------------------------------------------
    // Part 1 (Regression for @gate-audit #60):
    // `base` already carries `T-CODEXHISTORY-1` (whose historical `deps` list a
    // cross-repo commercial task `T-CLAUDECOMMCHAT-1` AND an archived historical
    // ID `T-ARCHIVED-0` that has no manifest in the repo) plus `T-CLAUDEGEMIFIX-2`
    // (`deps: ["T-CODEXHISTORY-1"]`), both with zero newer commits in `base..main`.
    // Downstream `T-CTOCORELEAN-1` in `base..main` depends on `T-CLAUDEGEMIFIX-2`
    // and `T-CODEXHISTORY-1`. `already_in_base` tasks MUST act as leaf boundaries
    // in `dfs_closure` + `deterministic_toposort` and resolve `SatisfiedInBase`
    // before `check_dep_outcomes` in BOTH `--functional-only` and default modes.
    // -----------------------------------------------------------------------
    let repo = sandbox.dir.join("base-leaf-repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--quiet", "--initial-branch=main"]);
    write_rel(
        &repo,
        "Cargo.toml",
        "[package]\nname = \"susi\"\nversion = \"0.21.64\"\nedition = \"2021\"\n",
    );
    write_rel(
        &repo,
        "crates/susi-gemi/src/history.rs",
        "pub fn preserve_history() -> bool { true }\n",
    );
    write_task_record(
        &repo,
        &TaskRecordSpec {
            id: "T-CODEXHISTORY-1",
            feature_id: "F-HIST-1",
            title: "Preserve full chat history for local engines",
            deps: &["T-CLAUDECOMMCHAT-1", "T-ARCHIVED-0"],
            scopes: &[
                ".agents/tasks/T-CODEXHISTORY-1.json",
                "crates/susi-gemi/src/history.rs",
            ],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_200,
        },
    );
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "commit",
            "-qm",
            "feat(gemi): preserve history\n\nTask: T-CODEXHISTORY-1",
        ],
    );

    write_rel(
        &repo,
        "crates/susi-gemi/src/tool_calls.rs",
        "pub fn return_tool_calls() -> usize { 1 }\n",
    );
    write_task_record(
        &repo,
        &TaskRecordSpec {
            id: "T-CLAUDEGEMIFIX-2",
            feature_id: "F-GEMI-2",
            title: "Caller tools return as OpenAI tool_calls",
            deps: &["T-CODEXHISTORY-1"],
            scopes: &[
                ".agents/tasks/T-CLAUDEGEMIFIX-2.json",
                "crates/susi-gemi/src/tool_calls.rs",
            ],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_210,
        },
    );
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "commit",
            "-qm",
            "fix(gemi): caller tools return as tool_calls\n\nTask: T-CLAUDEGEMIFIX-2",
        ],
    );
    let base_sha = git(&repo, &["rev-parse", "HEAD"]);

    // New functional Core task on top of `base_sha`:
    write_rel(
        &repo,
        "crates/susi-core/src/lean.rs",
        "pub fn lean_budget_ok() -> bool { true }\n",
    );
    write_task_record(
        &repo,
        &TaskRecordSpec {
            id: "T-CTOCORELEAN-1",
            feature_id: "F-LEAN-1",
            title: "Keep Susi core lean and fast",
            deps: &["T-CLAUDEGEMIFIX-2", "T-CODEXHISTORY-1"],
            scopes: &[
                ".agents/tasks/T-CTOCORELEAN-1.json",
                "crates/susi-core/src/lean.rs",
            ],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_220,
        },
    );
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "commit",
            "-qm",
            "refactor(core): trim pillar reach\n\nTask: T-CTOCORELEAN-1",
        ],
    );

    // Non-functional landing receipt commit in `base_sha..HEAD`:
    write_rel(
        &repo,
        ".agents/receipts/wave3.json",
        "{\"schema\":\"susi-gate-receipt-v1\"}\n",
    );
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "receipt: gated landing wave3"]);

    let range = format!("{base_sha}..HEAD");

    // Verify BOTH `--functional-only --strict` and default `--strict` succeed:
    for (name, extra_flag) in [
        ("rc-func-strict", Some("--functional-only")),
        ("rc-default-strict", None),
    ] {
        let mut args = vec![
            "compose",
            "--name",
            name,
            "--base",
            &base_sha,
            "--from",
            "HEAD",
            "--tasks-from-range",
            &range,
            "--strict",
            "--json",
        ];
        if let Some(flag) = extra_flag {
            args.push(flag);
        }
        let compose = run_bin(&bins.local_distill, &repo, &cargo_target, &args);
        assert!(
            compose.status.success(),
            "local-distill compose ({name}) failed: stdout={} stderr={}",
            String::from_utf8_lossy(&compose.stdout),
            String::from_utf8_lossy(&compose.stderr)
        );
        let receipt: Value = serde_json::from_slice(&compose.stdout).unwrap();
        let included: Vec<&str> = receipt["included"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["task_id"].as_str().unwrap())
            .collect();
        assert_eq!(included, vec!["T-CTOCORELEAN-1"]);
        assert_eq!(receipt["deferred"], json!([]));
        assert_eq!(receipt["rejected"], json!([]));

        let base_sat: Vec<&str> = receipt["satisfied_in_base"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["task_id"].as_str().unwrap())
            .collect();
        assert!(base_sat.contains(&"T-CLAUDEGEMIFIX-2"));
        assert!(base_sat.contains(&"T-CODEXHISTORY-1"));

        let closure: Vec<&str> = receipt["closure_order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            !closure.contains(&"T-CLAUDECOMMCHAT-1") && !closure.contains(&"T-ARCHIVED-0"),
            "already_in_base leaf boundary must not expand historical deps, got {closure:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Part 2: A NEW Core functional task in `base..HEAD` referencing a cross-repo
    // `susi-commercial-apps` task ID (`T-CLAUDECOMMCHAT-1`) skips that cross-repo
    // dep under `--functional-only --strict`, while a missing non-commercial
    // typo dependency (`T-TYPO-MISSING-1`) still fails closed with `missing_manifest`.
    // -----------------------------------------------------------------------
    let repo2 = sandbox.dir.join("cross-repo-dep");
    fs::create_dir_all(&repo2).unwrap();
    git(&repo2, &["init", "--quiet", "--initial-branch=main"]);
    write_rel(
        &repo2,
        "Cargo.toml",
        "[package]\nname = \"susi\"\nversion = \"0.21.64\"\nedition = \"2021\"\n",
    );
    git(&repo2, &["add", "."]);
    git(&repo2, &["commit", "-qm", "chore: base"]);
    let repo2_base = git(&repo2, &["rev-parse", "HEAD"]);

    write_rel(
        &repo2,
        "crates/susi-core/src/bridge.rs",
        "pub fn bridge_ready() -> bool { true }\n",
    );
    write_task_record(
        &repo2,
        &TaskRecordSpec {
            id: "T-COREBRIDGE-1",
            feature_id: "F-BRIDGE-1",
            title: "Core socket bridge for local chat",
            deps: &["T-CLAUDECOMMCHAT-1"],
            scopes: &[
                ".agents/tasks/T-COREBRIDGE-1.json",
                "crates/susi-core/src/bridge.rs",
            ],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_230,
        },
    );
    git(&repo2, &["add", "."]);
    git(
        &repo2,
        &[
            "commit",
            "-qm",
            "feat(core): add bridge\n\nTask: T-COREBRIDGE-1",
        ],
    );

    let cross_ok = run_bin(
        &bins.local_distill,
        &repo2,
        &cargo_target,
        &[
            "plan",
            "--name",
            "cross-comm-ok",
            "--base",
            &repo2_base,
            "--from",
            "HEAD",
            "--select",
            "T-COREBRIDGE-1",
            "--functional-only",
            "--strict",
            "--json",
        ],
    );
    assert!(
        cross_ok.status.success(),
        "cross-repo commercial dep should be skipped under --functional-only: {}",
        String::from_utf8_lossy(&cross_ok.stderr)
    );
    let cross_json: Value = serde_json::from_slice(&cross_ok.stdout).unwrap();
    assert_eq!(
        cross_json["included"][0]["task_id"],
        json!("T-COREBRIDGE-1")
    );
    assert_eq!(
        cross_json["skipped_non_functional_tasks"],
        json!(["T-CLAUDECOMMCHAT-1"])
    );

    // Explicitly selecting a non-existent commercial task still fails closed:
    let explicit_bad = run_bin(
        &bins.local_distill,
        &repo2,
        &cargo_target,
        &[
            "plan",
            "--name",
            "explicit-comm-fail",
            "--base",
            &repo2_base,
            "--from",
            "HEAD",
            "--select",
            "T-CLAUDECOMMCHAT-1",
            "--functional-only",
            "--json",
        ],
    );
    assert!(explicit_bad.status.success());
    let explicit_json: Value = serde_json::from_slice(&explicit_bad.stdout).unwrap();
    assert_eq!(
        explicit_json["rejected"][0]["reason_code"],
        json!("missing_manifest")
    );

    // A typo dependency on a missing local Core task still fails closed:
    write_rel(
        &repo2,
        "crates/susi-core/src/typo_dep.rs",
        "pub fn typo() {}\n",
    );
    write_task_record(
        &repo2,
        &TaskRecordSpec {
            id: "T-BADDEP-1",
            feature_id: "F-BADDEP-1",
            title: "Task with missing local dep",
            deps: &["T-TYPO-MISSING-1"],
            scopes: &[
                ".agents/tasks/T-BADDEP-1.json",
                "crates/susi-core/src/typo_dep.rs",
            ],
            accept_cmd: &["cargo", "--version"],
            created_unix: 1_794_429_240,
        },
    );
    git(&repo2, &["add", "."]);
    git(
        &repo2,
        &["commit", "-qm", "feat(core): bad dep\n\nTask: T-BADDEP-1"],
    );

    let bad_dep = run_bin(
        &bins.local_distill,
        &repo2,
        &cargo_target,
        &[
            "plan",
            "--name",
            "typo-dep-fail",
            "--base",
            &repo2_base,
            "--from",
            "HEAD",
            "--select",
            "T-BADDEP-1",
            "--functional-only",
            "--strict",
            "--json",
        ],
    );
    assert!(
        !bad_dep.status.success(),
        "missing non-commercial local dependency must fail strict --functional-only"
    );
}
