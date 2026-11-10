#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

struct Repo(PathBuf);

impl Repo {
    fn new() -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!("susi-owed-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let repo = Self(root);
        repo.git(&["init", "-q", "-b", "main"], None);
        repo
    }

    fn git(&self, args: &[&str], input: Option<&str>) -> String {
        let mut command = Command::new("git");
        command
            .args([
                "-c",
                "user.name=Receipt Test",
                "-c",
                "user.email=receipt@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", self.0.join("empty-global-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if input.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().unwrap();
        if let Some(body) = input {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(body.as_bytes())
                .unwrap();
        }
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn receipt(&self, name: &str, body: &str) {
        let oid = self.git(&["hash-object", "-w", "--stdin"], Some(body));
        self.git(&["update-ref", &format!("refs/closed/{name}"), &oid], None);
    }

    fn query(&self, args: &[&str]) -> Output {
        let refs = self.git(&["for-each-ref", "--format=%(refname) %(objectname)"], None);
        let status = self.git(&["status", "--porcelain=v1", "--untracked-files=all"], None);
        let out = Command::new(env!("CARGO_BIN_EXE_owed-task-receipts"))
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", self.0.join("empty-global-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        assert_eq!(
            self.git(&["for-each-ref", "--format=%(refname) %(objectname)"], None),
            refs
        );
        assert_eq!(
            self.git(&["status", "--porcelain=v1", "--untracked-files=all"], None),
            status
        );
        out
    }

    fn owed(&self, agent: &str) -> Vec<String> {
        self.owed_on(&[agent])
    }

    fn owed_on(&self, args: &[&str]) -> Vec<String> {
        let out = self.query(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn outstanding_receipts_match_the_published_tree() {
    let repo = Repo::new();
    let done = repo.0.join(".agents/tasks/done");
    std::fs::create_dir_all(done.join("T-NESTED")).unwrap();
    std::fs::write(done.join("T-PUBLISHED.json"), "{}").unwrap();
    std::fs::write(done.join("T-NESTED/file.json"), "{}").unwrap();
    repo.git(&["add", "."], None);
    repo.git(&["commit", "-qm", "published"], None);
    let head = repo.git(&["rev-parse", "HEAD"], None);
    repo.git(&["update-ref", "refs/remotes/origin/main", &head], None);
    repo.receipt("T-PUBLISHED", r#"{"agent":"TEST"}"#);
    repo.receipt("T-NESTED/file", r#"{"agent":"TEST"}"#);
    repo.receipt("T-OWED", r#"{"agent":"TEST"}"#);
    repo.receipt("T-OTHER", r#"{"agent":"OTHER"}"#);
    repo.receipt("T-MALFORMED", "not-json");
    assert_eq!(repo.owed("TEST"), ["T-OWED"]);
    assert_eq!(repo.owed("OTHER"), ["T-OTHER"]);
    repo.git(&["update-ref", "-d", "refs/remotes/origin/main"], None);
    assert_eq!(
        repo.owed("TEST"),
        ["T-NESTED/file", "T-OWED", "T-PUBLISHED"]
    );
}

#[test]
fn empty_repository_without_closed_refs_reports_no_owed_receipts() {
    let repo = Repo::new();
    assert!(repo.owed("TEST").is_empty());
    assert!(repo.owed("OTHER").is_empty());
}

#[test]
fn owed_receipts_filter_by_agent_and_sort_unmerged_task_ids() {
    let repo = Repo::new();
    repo.receipt("T-ZETA-2", r#"{"agent":"ALPHA"}"#);
    repo.receipt("T-ALPHA-1", r#"{"agent":"ALPHA"}"#);
    repo.receipt("T-BETA-1", r#"{"agent":"BETA"}"#);
    assert_eq!(repo.owed("ALPHA"), ["T-ALPHA-1", "T-ZETA-2"]);
    assert_eq!(repo.owed("BETA"), ["T-BETA-1"]);
    assert!(repo.owed("GAMMA").is_empty());
}

#[test]
fn publishing_done_record_on_origin_main_clears_previously_owed_receipt() {
    let repo = Repo::new();
    repo.receipt("T-LATE-1", r#"{"agent":"LATEAGENT"}"#);
    assert_eq!(repo.owed("LATEAGENT"), ["T-LATE-1"]);

    let done = repo.0.join(".agents/tasks/done");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(done.join("T-LATE-1.json"), "{}").unwrap();
    repo.git(&["add", "."], None);
    repo.git(&["commit", "-qm", "publish T-LATE-1"], None);
    let head = repo.git(&["rev-parse", "HEAD"], None);
    repo.git(&["update-ref", "refs/remotes/origin/main", &head], None);

    assert!(repo.owed("LATEAGENT").is_empty());
}

#[test]
fn malformed_receipt_json_is_ignored_when_querying_valid_agent() {
    let repo = Repo::new();
    repo.receipt("T-CORRUPT-1", "not-a-json-object");
    repo.receipt("T-VALID-1", r#"{"agent":"CLEANAGENT"}"#);
    assert_eq!(repo.owed("CLEANAGENT"), ["T-VALID-1"]);
}

#[test]
fn multiline_unicode_empty_and_malformed_bodies_never_shift_following_receipts() {
    let repo = Repo::new();
    repo.receipt("T-0-EMPTY", "");
    repo.receipt("T-1-MALFORMED", "not json\nsecond line\n");
    repo.receipt("T-2-MULTILINE", "{\n  \"agent\": \"CLEAN\u{00e9}\"\n}\n");
    repo.receipt("T-3-NULL", "null");
    repo.receipt("T-4-NUMBER", "{\"agent\":42}");
    repo.receipt("T-5-VALID", "{\"agent\":\"CLEAN\u{00e9}\"}");
    repo.receipt("T-6-OTHER", "{\"agent\":\"OTHER\"}\n");
    assert_eq!(repo.owed("CLEAN\u{00e9}"), ["T-2-MULTILINE", "T-5-VALID"]);
    assert_eq!(repo.owed("OTHER"), ["T-6-OTHER"]);
}

#[test]
fn selected_remote_and_exact_nested_done_paths_control_publication() {
    let repo = Repo::new();
    let done = repo.0.join(".agents/tasks/done/T-NESTED");
    std::fs::create_dir_all(&done).unwrap();
    std::fs::write(done.join("exact.json"), "{}").unwrap();
    std::fs::write(done.join("prefix.json.extra"), "{}").unwrap();
    repo.git(&["add", "."], None);
    repo.git(&["commit", "-qm", "alternate remote done records"], None);
    repo.git(
        &["update-ref", "refs/remotes/upstream/team/main", "HEAD"],
        None,
    );
    repo.receipt("T-NESTED/exact", r#"{"agent":"TEST"}"#);
    repo.receipt("T-NESTED/prefix", r#"{"agent":"TEST"}"#);
    assert_eq!(repo.owed("TEST"), ["T-NESTED/exact", "T-NESTED/prefix"]);
    assert_eq!(
        repo.owed_on(&["TEST", "upstream/team"]),
        ["T-NESTED/prefix"]
    );
    assert_eq!(repo.owed_on(&["TEST", ""]), repo.owed("TEST"));
    repo.git(
        &["update-ref", "refs/remotes/descendant/main/child", "HEAD"],
        None,
    );
    assert_eq!(
        repo.owed_on(&["TEST", "descendant"]),
        ["T-NESTED/exact", "T-NESTED/prefix"]
    );
}

#[test]
fn native_scan_batches_many_receipts_with_mixed_newlines() {
    let repo = Repo::new();
    let inputs = repo.0.join("receipt-inputs");
    std::fs::create_dir_all(&inputs).unwrap();
    let mut paths = String::new();
    let mut expected = Vec::new();
    for index in 0..90 {
        let agent = if index % 7 == 0 { "TEST" } else { "OTHER" };
        let newline = if index % 2 == 0 { "\n" } else { "" };
        let body = format!("{{\"agent\":\"{agent}\"}}{newline}");
        let path = format!("receipt-inputs/{index}.json");
        std::fs::write(repo.0.join(&path), body).unwrap();
        paths.push_str(&format!("{path}\n"));
        if agent == "TEST" {
            expected.push(format!("T-BATCH-{index:03}"));
        }
    }
    let objects = repo.git(&["hash-object", "-w", "--stdin-paths"], Some(&paths));
    assert_eq!(objects.lines().count(), 90);
    let updates: String = objects
        .lines()
        .enumerate()
        .map(|(index, object)| format!("update refs/closed/T-BATCH-{index:03} {object}\n"))
        .collect();
    repo.git(&["update-ref", "--stdin"], Some(&updates));
    assert_eq!(repo.owed("TEST"), expected);
}

#[test]
fn invalid_arguments_and_non_blob_receipts_fail_without_partial_output() {
    let repo = Repo::new();
    repo.receipt("T-A-VALID", r#"{"agent":"TEST"}"#);
    let tree = repo.git(&["mktree"], Some(""));
    repo.git(&["update-ref", "refs/closed/T-Z-TREE", &tree], None);
    for args in [
        vec![],
        vec![""],
        vec!["TEST", "origin", "extra"],
        vec!["TEST", "bad..remote"],
        vec!["TEST"],
    ] {
        let out = repo.query(&args);
        assert!(!out.status.success(), "args {args:?}");
        assert!(out.stdout.is_empty(), "no partial success for {args:?}");
        assert!(!out.stderr.is_empty());
    }
}

#[test]
fn an_invalid_remote_main_object_is_not_treated_as_missing_publication() {
    let repo = Repo::new();
    repo.receipt("T-OWED", r#"{"agent":"TEST"}"#);
    let blob = repo.git(&["rev-parse", "refs/closed/T-OWED"], None);
    repo.git(&["update-ref", "refs/remotes/origin/main", &blob], None);
    let out = repo.query(&["TEST"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn missing_git_and_non_repository_errors_do_not_report_empty_success() {
    let repo = Repo::new();
    let out = Command::new(env!("CARGO_BIN_EXE_owed-task-receipts"))
        .arg("TEST")
        .current_dir(&repo.0)
        .env("PATH", repo.0.join("no-git"))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let outside = repo.0.join("not-a-repository");
    std::fs::create_dir(&outside).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_owed-task-receipts"))
        .arg("TEST")
        .current_dir(&outside)
        .env("GIT_CEILING_DIRECTORIES", &repo.0)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}

#[test]
fn a_broken_receipt_ref_is_not_silently_ignored_by_git() {
    let repo = Repo::new();
    let closed = repo.0.join(".git/refs/closed");
    std::fs::create_dir_all(&closed).unwrap();
    std::fs::write(closed.join("T-BROKEN"), "invalid-ref\n").unwrap();
    let out = repo.query(&["TEST"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&out.stderr);
    assert!(diagnostic.contains("owed-task-receipts: git failed"));
    assert!(diagnostic.contains("refs/closed/T-BROKEN"));
}

#[test]
fn a_broken_remote_main_ref_is_not_treated_as_an_absent_remote() {
    let repo = Repo::new();
    let remote = repo.0.join(".git/refs/remotes/origin");
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::write(remote.join("main"), "invalid-ref\n").unwrap();
    let out = repo.query(&["TEST"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
    let diagnostic = String::from_utf8_lossy(&out.stderr);
    assert!(diagnostic.contains("owed-task-receipts: git failed"));
    assert!(diagnostic.contains("refs/remotes/origin/main"));
}
