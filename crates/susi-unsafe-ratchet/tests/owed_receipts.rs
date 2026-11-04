#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

    fn owed(&self, agent: &str) -> Vec<String> {
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/owed-task-receipts.sh");
        let out = Command::new(if cfg!(windows) { "sh" } else { "bash" })
            .arg(script)
            .arg(agent)
            .current_dir(&self.0)
            .env("GIT_CONFIG_GLOBAL", self.0.join("empty-global-config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env_remove("GIT_CONFIG_COUNT")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
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
