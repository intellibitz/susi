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
    if rerun_in_private_home() {
        return;
    }
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
    if rerun_in_private_home() {
        return;
    }
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
    if rerun_in_private_home() {
        return;
    }
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
    if rerun_in_private_home() {
        return;
    }
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

/// One remote, every directed-deletion case: the record is read from the
/// remote's own main, and only the listed ref at the listed object is accepted.
#[test]
fn a_directed_deletion_is_accepted_only_for_the_listed_ref_and_object() {
    if rerun_in_private_home() {
        return;
    }
    let repo = Repo::new();
    let one = repo.git(&["rev-parse", "HEAD"]);
    let tags = ["gone", "unlisted", "other", "back", "restored"];
    for tag in tags {
        repo.git(&["tag", tag]);
    }
    let mut push = vec!["push", "origin", "HEAD:refs/heads/wip/x"];
    push.extend(tags);
    repo.git(&push);
    assert_eq!(repo.audit().recorded, 6);
    let kept = |tag: &str| {
        let name = format!("refs/susi-guard/origin/tags/{tag}");
        repo.git(&["for-each-ref", "--format=%(objectname)", &name])
    };

    // The clean-up: five refs go, then the direction is written down on main.
    // `restored` stands for a tag an agent already pushed back from its copy.
    let mut delete = vec!["push", "origin", ":refs/heads/wip/x"];
    let gone: Vec<String> = tags[..4]
        .iter()
        .map(|t| format!(":refs/tags/{t}"))
        .collect();
    delete.extend(gone.iter().map(String::as_str));
    repo.git(&delete);
    let record = |body: &str| {
        let dir = repo.work.join(".agents").join("storage");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("directed-deletions.txt"), body).unwrap();
        repo.git(&["add", DIRECTED]);
        repo.git(&["commit", "-m", "record"]);
    };
    let wrong = "1".repeat(one.len());
    record(&format!(
        "# object ref date task\n\n{one} refs/tags/gone 2026-11-15 T-X-1\n{wrong} refs/tags/other 2026-11-15 T-X-1\n{one} refs/tags/back 2026-11-15 T-X-1\n{one} refs/tags/restored 2026-11-15 T-X-1\n{one} refs/heads/wip/x 2026-11-15 T-X-1\nnot a record line\n"
    ));
    repo.git(&["push", "origin", "main"]);

    let report = repo.audit();
    assert_eq!(report.accepted, 2, "{:?}", report.findings);
    assert!(report
        .summary("origin")
        .ends_with("2 directed deletion(s) accepted"));
    let starts = |report: &Report, prefix: &str| {
        report
            .findings
            .iter()
            .filter(|f| f.starts_with(prefix))
            .count()
    };
    assert_eq!(report.findings.len(), 4, "{:?}", report.findings);
    assert_eq!(starts(&report, "DELETED refs/tags/unlisted: "), 1);
    assert_eq!(starts(&report, "DELETED refs/tags/other: "), 1);
    assert_eq!(
        starts(
            &report,
            "UNREADABLE .agents/storage/directed-deletions.txt line 8"
        ),
        1
    );
    assert_eq!(starts(&report, "RECREATED refs/tags/restored: "), 1);
    let other = report
        .findings
        .iter()
        .find(|f| f.contains("tags/other"))
        .unwrap();
    assert!(
        other.contains("not at the object this clone saw"),
        "{other}"
    );
    // The accepted copies are dropped; a reported loss keeps its copy.
    assert_eq!((kept("gone"), kept("back")), (String::new(), String::new()));
    assert_eq!(
        (kept("unlisted"), kept("other")),
        (one.clone(), one.clone())
    );
    // An accepted deletion is silent from then on.
    let again = repo.audit();
    assert_eq!((again.accepted, again.findings.len()), (0, 4));

    // A listed ref that comes back is reported, never recorded, and its
    // message carries no command; that holds for an unguarded branch too.
    repo.git(&[
        "push",
        "origin",
        "HEAD:refs/tags/back",
        "HEAD:refs/heads/wip/x",
    ]);
    let back = repo.audit();
    assert_eq!(back.findings.len(), 6, "{:?}", back.findings);
    assert_eq!(starts(&back, "RECREATED refs/tags/back: "), 1);
    assert_eq!(starts(&back, "RECREATED refs/heads/wip/x: "), 1);
    assert!(back
        .findings
        .iter()
        .filter(|f| f.starts_with("RECREATED"))
        .all(|f| !f.contains("git push") && f.contains("2026-11-15, T-X-1")));
    assert_eq!(kept("back"), String::new());

    // A main that was rewound accepts nothing, whatever its record says.
    repo.git(&["checkout", "-q", "-b", "rewritten", &one]);
    record(&format!("{one} refs/tags/unlisted 2026-11-15 T-X-1\n"));
    repo.git(&["push", "--force", "origin", "rewritten:refs/heads/main"]);
    let rewound = repo.audit();
    assert_eq!(rewound.accepted, 0);
    assert_eq!(starts(&rewound, "REWOUND refs/heads/main: "), 1);
    assert_eq!(starts(&rewound, "DELETED refs/tags/unlisted: "), 1);
    assert_eq!(kept("unlisted"), one);
}

/// The committed record is one the guard reads completely, and it never
/// accepts the loss of main or release tags. The two superseded candidate
/// lines have separately directed deletions at their exact published objects.
#[test]
fn the_committed_record_spares_main_and_releases_except_directed_candidates() {
    let repo = Repo::new();
    let dir = repo.work.join(".agents").join("storage");
    fs::create_dir_all(&dir).unwrap();
    let committed = include_str!("../../../../.agents/storage/directed-deletions.txt");
    fs::write(dir.join("directed-deletions.txt"), committed).unwrap();
    repo.git(&["add", DIRECTED]);
    repo.git(&["commit", "-m", "record"]);
    let mut findings = Vec::new();
    let mut listed = directed(&repo.work, "HEAD", &mut findings);
    assert_eq!(findings, Vec::<String>::new());
    // The first record: 9 branches and 102 tags (2026-11-15).
    assert!(listed.len() >= 111, "{}", listed.len());
    for (reference, oid) in [
        (
            "refs/candidates/release/v0-21-65-distilled-rc1",
            "90daead4568eff5e471ddc2bf664b120a2570ed7",
        ),
        (
            "refs/candidates/release/v0.21.62",
            "5436c616c740086cf18190d0cbff188319b1791e",
        ),
    ] {
        assert_eq!(
            listed.remove(reference).unwrap(),
            [(
                oid.to_owned(),
                "2026-11-16, T-AGENTCLAUDEDIRECTEDDELETION-1".to_owned()
            )]
        );
    }
    for name in listed.keys() {
        let release = name
            .strip_prefix("refs/tags/v")
            .is_some_and(|version| version.bytes().all(|b| b.is_ascii_digit() || b == b'.'));
        assert!(
            name != MAIN && !name.starts_with("refs/candidates/") && !release,
            "{name}"
        );
    }
}

// Discovery in these tests must see a fresh home whatever providers the operator uses.
fn rerun_in_private_home() -> bool {
    const MARKER: &str = "SUSI_STORAGE_GUARD_TEST";
    let thread = std::thread::current();
    let name = thread.name().unwrap();
    if std::env::var(MARKER).as_deref() == Ok(name) {
        return false;
    }
    let scratch = std::env::temp_dir().join(format!(
        "susi-guard-home-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&scratch).unwrap();
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.env_clear();
    for key in [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "COMSPEC",
        "PATHEXT",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(key) {
            cmd.env(key, value);
        }
    }
    susi_paths::test_env::isolate_home(&mut cmd, &scratch);
    let result = cmd
        .current_dir(&scratch)
        .env(MARKER, name)
        .args(["--exact", name])
        .output()
        .unwrap();
    fs::remove_dir_all(&scratch).unwrap();
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        result.status.success() && stdout.contains("1 passed; 0 failed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    true
}

#[test]
fn a_provider_failure_does_not_create_a_local_witness() {
    let repo = Repo::new();
    let authorization = EaiError::authorization("denied by registered provider");
    let expected = authorization.to_string();
    let (result, calls) = crate::tool_backend::fixture::scripted(
        vec![
            json!({"isError":true,"structuredContent":crate::git_governor::workflow_error_value(authorization)}),
        ],
        || audit(&repo.work, "origin"),
    );
    assert_eq!(result.unwrap_err(), expected);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["arguments"]["operation"], "storage_audit");
    assert!(repo.git(&["for-each-ref", LEDGER]).is_empty());
}
