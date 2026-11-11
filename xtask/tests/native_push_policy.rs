#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Test assertions.
#[path = "../src/push_policy.rs"]
mod push_policy;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
const ZERO: &str = "0000000000000000000000000000000000000000";
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "susi-native-push-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let r = Self(root);
        r.git(&["init", "-q", "-b", "main"]);
        r.commit("first");
        r
    }
    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(&self.0)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "fixture")
            .env("GIT_AUTHOR_EMAIL", "fixture@localhost")
            .env("GIT_COMMITTER_NAME", "fixture")
            .env("GIT_COMMITTER_EMAIL", "fixture@localhost")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }
    fn commit(&self, text: &str) -> String {
        std::fs::write(self.0.join("source"), text).unwrap();
        self.git(&["add", "source"]);
        self.git(&["commit", "-qm", text]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn check(&self, input: &str) -> Result<(), String> {
        push_policy::validate(&self.0, input)
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn main_create_and_fast_forward_use_supplied_refs_not_head_or_tracking_state() {
    let r = Repo::new();
    let old = r.git(&["rev-parse", "HEAD"]);
    let new = r.commit("second");
    assert!(r
        .check(&format!("{new} {new} refs/heads/main {ZERO}\n"))
        .is_ok());
    r.git(&["checkout", "--detach", &old]);
    assert!(r
        .check(&format!("refs/heads/main {new} refs/heads/main {old}\n"))
        .is_ok());
}
#[test]
fn branch_deletion_non_fast_forward_missing_old_and_stale_local_are_rejected() {
    let r = Repo::new();
    let old = r.git(&["rev-parse", "HEAD"]);
    let new = r.commit("second");
    for input in [
        format!("(delete) {ZERO} refs/heads/main {new}"),
        format!("{old} {old} refs/heads/main {new}"),
        format!("HEAD {old} refs/heads/main {ZERO}"),
        format!("HEAD {new} refs/heads/main {}", "1".repeat(40)),
        format!("(delete) {ZERO} refs/heads/topic {new}"),
    ] {
        assert!(r.check(&input).is_err(), "accepted {input}");
    }
}
#[test]
fn mixed_transaction_invalid_record_is_not_hidden_by_valid_main() {
    let r = Repo::new();
    let head = r.git(&["rev-parse", "HEAD"]);
    let valid = format!("HEAD {head} refs/heads/main {ZERO}\n");
    for bad in [
        format!("HEAD {head} refs/heads/main {ZERO}"),
        format!("HEAD {head} refs/unknown/value {ZERO}"),
        "bad input".into(),
        format!("HEAD {head} refs/heads/../bad {ZERO}"),
    ] {
        assert!(r.check(&(valid.clone() + &bad)).is_err());
    }
    assert!(r.check("").is_ok());
}
#[test]
fn snapshots_and_distill_candidates_are_commit_bound_immutable_and_release_creation_fails_closed() {
    let r = Repo::new();
    let head = r.git(&["rev-parse", "HEAD"]);
    let blob = r.git(&["rev-parse", "HEAD:source"]);
    assert!(r
        .check(&format!("HEAD {head} refs/tags/susi-snapshot/rc1 {ZERO}"))
        .is_ok());
    assert!(r
        .check(&format!(
            "HEAD {head} refs/tags/susi-distill/2026-11-11-rc1 {ZERO}"
        ))
        .is_ok());
    r.git(&["tag", "-a", "susi-snapshot/annotated", "-m", "snapshot"]);
    let tag = r.git(&["rev-parse", "refs/tags/susi-snapshot/annotated"]);
    assert!(r
        .check(&format!(
            "refs/tags/susi-snapshot/annotated {tag} refs/tags/susi-snapshot/annotated {ZERO}"
        ))
        .is_ok());
    r.git(&[
        "tag",
        "-a",
        "susi-distill/annotated",
        "-m",
        "distill candidate",
    ]);
    let distill_tag = r.git(&["rev-parse", "refs/tags/susi-distill/annotated"]);
    assert!(r
        .check(&format!(
            "refs/tags/susi-distill/annotated {distill_tag} refs/tags/susi-distill/annotated {ZERO}"
        ))
        .is_ok());
    for input in [
        format!("HEAD {head} refs/tags/susi-snapshot/rc1 {head}"),
        format!("(delete) {ZERO} refs/tags/susi-snapshot/rc1 {head}"),
        format!("HEAD {head} refs/tags/susi-distill/2026-11-11-rc1 {head}"),
        format!("(delete) {ZERO} refs/tags/susi-distill/2026-11-11-rc1 {head}"),
        format!("{blob} {blob} refs/tags/susi-distill/blob {ZERO}"),
        format!("HEAD {head} refs/tags/distill/2026-11-11-rc1 {ZERO}"),
        format!("HEAD {head} refs/tags/v1.0.0 {ZERO}"),
        format!("HEAD {head} refs/tags/release/latest {ZERO}"),
    ] {
        assert!(r.check(&input).is_err(), "accepted {input}");
    }
}
#[test]
fn metadata_blob_create_replace_delete_do_not_require_branch_builds() {
    let r = Repo::new();
    let head = r.git(&["rev-parse", "HEAD"]);
    let blob = r.git(&["rev-parse", "HEAD:source"]);
    for namespace in ["claims", "claim-agents", "claim-generation", "closed"] {
        let reference = format!("refs/{namespace}/T-FIXTURE-1");
        for old in [ZERO, &blob] {
            assert!(r.check(&format!("{blob} {blob} {reference} {old}")).is_ok());
        }
        assert!(r
            .check(&format!("(delete) {ZERO} {reference} {blob}"))
            .is_ok());
        assert!(r.check(&format!("HEAD {head} {reference} {ZERO}")).is_err());
    }
    assert!(r
        .check(&format!(
            "{blob} {blob} refs/tags/susi-snapshot/blob {ZERO}"
        ))
        .is_err());
}

#[test]
fn real_git_push_runs_native_hook_and_preserves_remote_on_rejection() {
    let binary = env!("CARGO_BIN_EXE_push-policy");
    let r = Repo::new();
    let remote = r.0.join("remote.git");
    r.git(&["init", "--bare", "-q", remote.to_str().unwrap()]);
    r.git(&["remote", "add", "fixture", remote.to_str().unwrap()]);
    let hook = r.0.join(".git/hooks/pre-push");
    std::fs::write(&hook, include_str!("../../.githooks/pre-push")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let push = |args: &[&str], legacy: &str| {
        let mut path = vec![std::path::Path::new(binary).parent().unwrap().to_owned()];
        path.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Command::new("git")
            .current_dir(&r.0)
            .env("SUSI_BRANCH_CI_HANDOFF", legacy)
            .env("SUSI_FINISH_REMOTE_GATE", legacy)
            .arg("push")
            .args(args)
            .env("PATH", std::env::join_paths(path).unwrap())
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .output()
            .unwrap()
    };
    let first = r.git(&["rev-parse", "HEAD"]);
    assert!(push(&["fixture", "HEAD:refs/heads/main"], "0")
        .status
        .success());
    for legacy in ["0", "1"] {
        let second = r.commit(&format!("second with legacy flags {legacy}"));
        assert!(push(&["fixture", "HEAD:refs/heads/main"], legacy)
            .status
            .success());
        let refusal = push(
            &["--force", "fixture", &format!("{first}:refs/heads/main")],
            legacy,
        );
        assert!(!refusal.status.success());
        assert!(String::from_utf8_lossy(&refusal.stderr).contains("pre-push:"));
        assert!(!push(&["fixture", ":refs/heads/main"], legacy)
            .status
            .success());
        assert!(!push(&["fixture", "HEAD:refs/tags/v1.0.0"], legacy)
            .status
            .success());
        let snapshot = format!("refs/tags/susi-snapshot/fixture-{legacy}");
        assert!(push(&["fixture", &format!("HEAD:{snapshot}")], legacy)
            .status
            .success());
        assert!(!push(&["fixture", &format!(":{snapshot}")], legacy)
            .status
            .success());
        assert_eq!(
            r.git(&[
                "--git-dir",
                remote.to_str().unwrap(),
                "rev-parse",
                "refs/heads/main"
            ]),
            second
        );
        assert!(!remote.join("refs/tags/v1.0.0").exists());
    }
}

#[test]
fn replacement_ancestry_cannot_authorize_unrelated_branch_push() {
    let r = Repo::new();
    let old = r.git(&["rev-parse", "HEAD"]);
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let unrelated = r.git(&["commit-tree", &tree, "-m", "unrelated root"]);
    let fake_descendant = r.git(&["commit-tree", &tree, "-p", &old, "-m", "fake ancestry"]);
    r.git(&["replace", &unrelated, &fake_descendant]);
    // Demonstrate the attack: ordinary Git sees ancestry that is not in the
    // real commit graph, but the policy must ignore that local replacement.
    r.git(&["merge-base", "--is-ancestor", &old, &unrelated]);
    assert!(r
        .check(&format!("{unrelated} {unrelated} refs/heads/main {old}"))
        .is_err());
    let genuine = r.commit("genuine descendant");
    assert!(r
        .check(&format!("HEAD {genuine} refs/heads/main {old}"))
        .is_ok());
}

#[test]
fn legacy_grafted_ancestry_fails_closed_in_primary_and_linked_worktree() {
    let r = Repo::new();
    let old = r.git(&["rev-parse", "HEAD"]);
    let tree = r.git(&["rev-parse", "HEAD^{tree}"]);
    let unrelated = r.git(&["commit-tree", &tree, "-m", "unrelated root"]);
    let linked = r.0.join("linked");
    r.git(&[
        "worktree",
        "add",
        "--detach",
        linked.to_str().unwrap(),
        &unrelated,
    ]);
    std::fs::write(r.0.join(".git/info/grafts"), format!("{unrelated} {old}\n")).unwrap();
    r.git(&["merge-base", "--is-ancestor", &old, &unrelated]);
    let input = format!("{unrelated} {unrelated} refs/heads/main {old}");
    assert!(r.check(&input).unwrap_err().contains("graft"));
    assert!(push_policy::validate(&linked, &input)
        .unwrap_err()
        .contains("graft"));
}

#[test]
fn graft_environment_override_fails_closed_even_when_file_is_missing() {
    use std::io::Write;
    use std::process::Stdio;
    let r = Repo::new();
    let head = r.git(&["rev-parse", "HEAD"]);
    let binary = env!("CARGO_BIN_EXE_push-policy");
    let mut child = Command::new(binary)
        .current_dir(&r.0)
        .env("GIT_GRAFT_FILE", r.0.join("nonexistent-graft"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(format!("HEAD {head} refs/heads/main {ZERO}\n").as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("GIT_GRAFT_FILE"));
}

#[test]
fn release_branches_are_local_only_and_legacy_refs_can_be_deleted() {
    let r = Repo::new();
    let old = r.git(&["rev-parse", "HEAD"]);
    let new = r.commit("release candidate");
    for reference in ["refs/heads/release", "refs/heads/release/1.0"] {
        for previous in [ZERO, &old] {
            let input = format!("HEAD {new} {reference} {previous}");
            let error = r.check(&input).unwrap_err();
            assert!(error.contains("is local-only"), "{error}");
        }
        assert!(r
            .check(&format!("(delete) {ZERO} {reference} {old}"))
            .is_ok());
    }
    for reference in ["refs/heads/main", "refs/heads/release-notes"] {
        assert!(r.check(&format!("HEAD {new} {reference} {old}")).is_ok());
    }
}

/// A release tree at `version` (package manifest, lockfile, toolchain pin),
/// committed; returns (commit, tree, lockfile sha256).
fn release_tree(r: &Repo, version: &str) -> (String, String, String) {
    use sha2::{Digest, Sha256};
    let lock = b"# lock\n";
    std::fs::write(
        r.0.join("Cargo.toml"),
        format!("[package]\nname = \"susi\"\nversion = \"{version}\"\n"),
    )
    .unwrap();
    std::fs::write(r.0.join("Cargo.lock"), lock).unwrap();
    std::fs::write(
        r.0.join("rust-toolchain.toml"),
        "[toolchain]\nchannel = \"1.99.0\"\n",
    )
    .unwrap();
    r.git(&["add", "-A"]);
    r.git(&["commit", "-qm", "release tree"]);
    let digest: String = Sha256::digest(lock)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    (
        r.git(&["rev-parse", "HEAD"]),
        r.git(&["rev-parse", "HEAD^{tree}"]),
        digest,
    )
}

fn release_record(
    tag: &str,
    version: &str,
    commit: &str,
    tree: &str,
    lock: &str,
) -> serde_json::Value {
    let gate =
        |name: &str| serde_json::json!({"name": name, "argv": ["cargo", name], "exit_code": 0});
    let mut gates: Vec<serde_json::Value> =
        ["format", "check", "clippy", "licenses", "build", "smoke"]
            .iter()
            .map(|name| gate(name))
            .collect();
    let mut tests = gate("tests");
    tests["tests"] = serde_json::json!({"binaries": 3, "passed": 40, "failed": 0, "ignored": 1});
    gates.push(tests);
    serde_json::json!({
        "schema": "susi-release-qualification/v1",
        "product": "susi",
        "tag": tag,
        "version": version,
        "qualified_commit": commit,
        "qualified_tree": tree,
        "cargo_lock_sha256": lock,
        "toolchain": {"channel": "1.99.0", "rustc": "rustc 1.99.0"},
        "host": "x86_64-unknown-linux-gnu",
        "gates": gates,
        "assets": [{"file": "susi", "bytes": 1, "sha256": "a".repeat(64), "version_output": format!("susi {version}")}],
    })
}

/// Qualify a fresh release tree with `edit` applied to its record and return
/// push-policy's verdict on creating the annotated release tag.
fn release_tag_verdict(
    edit: impl FnOnce(&mut serde_json::Value),
    extra_file: bool,
) -> Result<(), String> {
    let r = Repo::new();
    let (commit, tree, lock) = release_tree(&r, "1.2.3");
    let mut record = release_record("v1.2.3", "1.2.3", &commit, &tree, &lock);
    edit(&mut record);
    std::fs::create_dir_all(r.0.join("releases/qualification")).unwrap();
    std::fs::write(
        r.0.join("releases/qualification/v1.2.3.json"),
        serde_json::to_string_pretty(&record).unwrap(),
    )
    .unwrap();
    if extra_file {
        std::fs::write(r.0.join("source"), "changed after qualification").unwrap();
    }
    r.git(&["add", "-A"]);
    r.git(&[
        "commit",
        "-qm",
        "chore: release v1.2.3 qualification record",
    ]);
    r.git(&["tag", "-a", "v1.2.3", "-m", "release"]);
    let tag = r.git(&["rev-parse", "refs/tags/v1.2.3"]);
    r.check(&format!("refs/tags/v1.2.3 {tag} refs/tags/v1.2.3 {ZERO}"))
}

#[test]
fn a_release_tag_with_its_exact_tree_record_is_admitted() {
    assert_eq!(release_tag_verdict(|_| {}, false), Ok(()));
}

/// One way a release record can be wrong, named for the failure message.
type RecordEdit = (&'static str, Box<dyn FnOnce(&mut serde_json::Value)>);

#[test]
fn a_release_tag_without_a_valid_record_for_its_exact_tree_fails_closed() {
    let cases: Vec<RecordEdit> = vec![
        (
            "tree",
            Box::new(|r| r["qualified_tree"] = "0".repeat(40).into()),
        ),
        (
            "commit",
            Box::new(|r| r["qualified_commit"] = "0".repeat(40).into()),
        ),
        ("schema", Box::new(|r| r["schema"] = "other".into())),
        (
            "lock",
            Box::new(|r| r["cargo_lock_sha256"] = "b".repeat(64).into()),
        ),
        (
            "toolchain",
            Box::new(|r| r["toolchain"]["channel"] = "1.98.0".into()),
        ),
        (
            "failed gate",
            Box::new(|r| r["gates"][2]["exit_code"] = 1.into()),
        ),
        (
            "missing gate",
            Box::new(|r| {
                r["gates"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|g| g["name"] != "smoke");
            }),
        ),
        (
            "test failures",
            Box::new(|r| r["gates"][6]["tests"]["failed"] = 1.into()),
        ),
        (
            "no tests",
            Box::new(|r| r["gates"][6]["tests"]["passed"] = 0.into()),
        ),
        (
            "asset version",
            Box::new(|r| r["assets"][0]["version_output"] = "susi 1.2.2".into()),
        ),
        (
            "no assets",
            Box::new(|r| r["assets"] = serde_json::json!([])),
        ),
    ];
    for (name, edit) in cases {
        assert!(
            release_tag_verdict(edit, false).is_err(),
            "{name} must be refused"
        );
    }
    assert!(
        release_tag_verdict(|_| {}, true).is_err(),
        "a record commit that also changes the tree must be refused"
    );
}

#[test]
fn a_release_tag_must_point_at_the_record_commit_and_never_moves() {
    let r = Repo::new();
    let (commit, _, _) = release_tree(&r, "1.2.3");
    assert!(r
        .check(&format!("HEAD {commit} refs/tags/v1.2.3 {ZERO}"))
        .is_err());
    let other = r.commit("other");
    assert!(r
        .check(&format!("HEAD {other} refs/tags/v1.2.3 {commit}"))
        .unwrap_err()
        .contains("immutable"));
}
