//! Storage guard against real Git: a bare remote and a clone per test.
// Test assertions intentionally panic on a failed invariant.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Repo {
    root: PathBuf,
    work: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "susi-storage-guard-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir(&root).unwrap();
        let work = root.join("work");
        fs::create_dir(&work).unwrap();
        text(&root, &["init", "--bare", "remote.git"]).unwrap();
        let repo = Self { root, work };
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "Guard Test"]);
        repo.git(&["config", "user.email", "guard@test.invalid"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        repo.git(&["config", "core.hooksPath", ".no-test-hooks"]);
        let remote = repo.root.join("remote.git");
        repo.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
        repo.commit("one");
        repo.git(&["push", "origin", "main"]);
        repo
    }
    fn git(&self, args: &[&str]) -> String {
        text(&self.work, args).unwrap()
    }
    fn commit(&self, body: &str) -> String {
        fs::write(self.work.join("source.txt"), body).unwrap();
        self.git(&["add", "source.txt"]);
        self.git(&["commit", "-m", body]);
        self.git(&["rev-parse", "HEAD"])
    }
    fn audit(&self) -> Report {
        audit(&self.work, "origin").unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn protected_namespaces_are_main_release_lines_and_tags_only() {
    assert_eq!(rule_for("refs/heads/main"), Some(Rule::AppendOnly));
    assert_eq!(
        rule_for("refs/candidates/release/v1"),
        Some(Rule::AppendOnly)
    );
    assert_eq!(rule_for("refs/tags/v1.0.0"), Some(Rule::Immutable));
    for free in [
        "refs/heads/other",
        "refs/agents/claude",
        "refs/claims/release-lanes/v1/a",
        "refs/tags/",
        "refs/candidates/",
    ] {
        assert_eq!(rule_for(free), None, "{free}");
    }
}

#[test]
fn a_first_run_records_and_forward_movement_advances_the_record() {
    let repo = Repo::new();
    repo.git(&["tag", "v1"]);
    repo.git(&["push", "origin", "v1", "HEAD:refs/candidates/release/r1"]);
    let first = repo.audit();
    assert_eq!((first.recorded, first.findings.len()), (3, 0));
    assert_eq!(repo.audit().unchanged, 3);

    let two = repo.commit("two");
    repo.git(&["push", "origin", "main", "HEAD:refs/candidates/release/r1"]);
    let moved = repo.audit();
    assert_eq!(
        (moved.advanced, moved.unchanged, moved.findings.len()),
        (2, 1, 0)
    );
    assert_eq!(
        repo.git(&["rev-parse", "refs/susi-guard/origin/heads/main"]),
        two
    );
    assert_eq!(witness(&repo.work, "origin"), None);
    // Nothing but the record is written: no incoming ref is left behind.
    assert_eq!(repo.git(&["for-each-ref", INCOMING]), "");
}

#[test]
fn a_rewound_main_is_reported_retained_and_recoverable_without_force() {
    let repo = Repo::new();
    let one = repo.git(&["rev-parse", "HEAD"]);
    let two = repo.commit("two");
    repo.git(&["push", "origin", "main"]);
    repo.audit();

    // Another client, with no hook, rewrites main and drops `two`.
    repo.git(&[
        "push",
        "--force",
        "origin",
        &format!("{one}:refs/heads/main"),
    ]);
    let report = repo.audit();
    assert_eq!(report.findings.len(), 1);
    assert!(report.findings[0].starts_with("REWOUND refs/heads/main: 1 published"));
    // The record is kept, so the loss stays reported until it is repaired.
    assert_eq!(
        repo.git(&["rev-parse", "refs/susi-guard/origin/heads/main"]),
        two
    );
    assert!(witness(&repo.work, "origin")
        .unwrap()
        .contains("did not block"));

    // Recovery is an ordinary fast-forward of the retained history.
    repo.git(&[
        "push",
        "origin",
        "refs/susi-guard/origin/heads/main:refs/heads/main",
    ]);
    let healed = repo.audit();
    assert_eq!((healed.findings.len(), healed.unchanged), (0, 1));
}

#[test]
fn a_moved_tag_and_a_deleted_tag_are_reported_and_the_deleted_one_restorable() {
    let repo = Repo::new();
    repo.git(&["tag", "v1"]);
    repo.git(&["tag", "v2"]);
    repo.git(&["push", "origin", "v1", "v2"]);
    let original = repo.git(&["rev-parse", "v1"]);
    repo.audit();

    repo.commit("two");
    repo.git(&["tag", "--force", "v1"]);
    repo.git(&["push", "--force", "origin", "v1"]);
    repo.git(&["push", "origin", ":refs/tags/v2"]);
    let report = repo.audit();
    assert_eq!(report.findings.len(), 2, "{:?}", report.findings);
    assert!(report
        .findings
        .iter()
        .any(|f| f.starts_with("MOVED refs/tags/v1")));
    assert!(report
        .findings
        .iter()
        .any(|f| f.starts_with("DELETED refs/tags/v2")));
    assert_eq!(
        repo.git(&["rev-parse", "refs/susi-guard/origin/tags/v1"]),
        original
    );

    // The printed restore is a plain create: no force is involved.
    repo.git(&[
        "push",
        "origin",
        "refs/susi-guard/origin/tags/v2:refs/tags/v2",
    ]);
    let after = repo.audit();
    assert_eq!(after.findings.len(), 1);
    assert!(after.findings[0].starts_with("MOVED refs/tags/v1"));
}

#[test]
fn unprotected_refs_and_bad_remote_names_are_not_guarded() {
    let repo = Repo::new();
    repo.git(&["push", "origin", "HEAD:refs/agents/claude"]);
    assert_eq!(repo.audit().recorded, 1);
    repo.git(&["push", "origin", ":refs/agents/claude"]);
    assert!(repo.audit().findings.is_empty());
    for bad in ["", "-upload-pack=x", "a b", "absent"] {
        assert!(audit(&repo.work, bad).is_err(), "{bad}");
    }
    assert!(witness(&repo.work, "absent")
        .unwrap()
        .contains("could not check"));
}
