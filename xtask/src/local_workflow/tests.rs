use super::*;
use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
#[test]
fn workflow_context_rejects_a_foreign_lock_directory_before_loading() {
    let (_scratch, repo, base) = ship_fixture("one");
    let foreign = Scratch::new();
    let other = foreign.repo();
    assert!(load_sources(&repo, &state(&other).unwrap(), &["one"], &base).is_err());
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
}

#[test]
fn workflow_context_rechecks_root_and_source_gitfile_after_the_gate() {
    for redirect_source in [false, true] {
        let (scratch, repo, base) = ship_fixture("one");
        let foreign = Scratch::new();
        let other = foreign.repo();
        let entry = scratch.0.join("entry");
        git(
            &repo,
            &[
                "-c",
                "core.hooksPath=",
                "worktree",
                "add",
                "--detach",
                text(&entry).unwrap(),
                &base,
            ],
        )
        .unwrap();
        let claim = read_claim(&state(&repo).unwrap().join("claims/one.claim")).unwrap();
        let redirected = if redirect_source {
            claim.worktree.join(".git")
        } else {
            entry.join(".git")
        };
        let original = fs::read(&redirected).unwrap();
        let mut remote = FakeRemote::single(base);
        let result = ship(&entry, &["one"], &mut remote, |_candidate, _attempt| {
            fs::write(
                &redirected,
                format!("gitdir: {}\n", other.join(".git").display()),
            )
            .unwrap();
            Ok(local_gate_evidence())
        });
        fs::write(&redirected, original).unwrap();
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains(if redirect_source {
                "claim repository"
            } else {
                "workflow directory changed"
            }),
            "{error}"
        );
        assert!(remote.pushes.is_empty());
        assert!(integration_head(&repo).unwrap().is_none());
        assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    }
}

#[test]
fn timing_record_preserves_nested_phases_without_calling_the_sum_elapsed() {
    let phases = vec![
        ("build".to_owned(), 100),
        ("binaries".to_owned(), 200),
        ("workspace".to_owned(), 300),
    ];
    let record = timing_record(
        7,
        "ship",
        "shipped",
        std::time::Duration::from_millis(450),
        &phases,
    );
    assert_eq!(record["schema"], "susi-local-workflow-timings-v2");
    assert_eq!(record["elapsed_ms"], 450);
    assert_eq!(record["inclusive_phase_sum_ms"], 600);
    assert_eq!(record["phases_ms"].as_array().unwrap().len(), 3);
    assert!(record.get("total_ms").is_none());
}

#[test]
fn timing_record_includes_unphased_time_and_keeps_failure_outcome() {
    let record = timing_record(
        8,
        "integrate",
        "failed",
        std::time::Duration::from_millis(125),
        &[],
    );
    assert_eq!(record["elapsed_ms"], 125);
    assert_eq!(record["inclusive_phase_sum_ms"], 0);
    assert_eq!(record["outcome"], "failed");
    assert_eq!(record["command"], "integrate");
    assert!(record["elapsed_scope"]
        .as_str()
        .unwrap()
        .contains("retries"));
}

#[test]
fn batched_git_paths_match_individual_queries_and_preserve_every_guard() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = start(&repo, "markers", &["owned.rs".into()]).unwrap();
    let names = [
        "MERGE_HEAD",
        "MERGE_AUTOSTASH",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "rebase-merge",
        "rebase-apply",
    ];
    for root in [&repo, &worktree] {
        let paths = git_paths(root, &names).unwrap();
        for (name, path) in names.iter().zip(paths) {
            let individual = git(
                root,
                &["rev-parse", "--path-format=absolute", "--git-path", name],
            )
            .unwrap();
            assert_eq!(path, PathBuf::from(individual));
        }
    }
    fs::write(worktree.join("owned.rs"), "change").unwrap();
    let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    for path in git_paths(&worktree, &names).unwrap() {
        fs::write(&path, "operation in progress").unwrap();
        assert!(commit(&worktree, "markers", "must refuse", None).is_err());
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
        fs::remove_file(path).unwrap();
    }
    commit(&worktree, "markers", "all guards absent", None).unwrap();
    assert_ne!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
}

#[test]
fn batched_git_paths_reject_incomplete_relative_and_extra_output() {
    let root = env::temp_dir();
    let absolute = root.display().to_string();
    assert!(parse_git_paths(&absolute, 1).is_ok());
    assert!(parse_git_paths("", 1).is_err());
    assert!(parse_git_paths("relative/path", 1).is_err());
    assert!(parse_git_paths(&absolute, 2).is_err());
    assert!(parse_git_paths(&format!("{absolute}\n{absolute}"), 1).is_err());
}

#[test]
fn workspace_proof_waits_for_every_planned_gate() {
    assert!(completed_workspace_proof("key", Some("key"), 0, 0).is_none());
    for completed in 0..6 {
        assert!(completed_workspace_proof("key", Some("key"), completed, 6).is_none());
    }
    assert!(completed_workspace_proof("key", Some("key"), 7, 6).is_none());
    assert_eq!(
        completed_workspace_proof("key", Some("key"), 6, 6),
        Some(workspace_test_proof::record("key"))
    );
}

#[test]
fn workspace_proof_rechecks_inputs_after_companion_gates() {
    assert!(completed_workspace_proof("before", Some("after"), 6, 6).is_none());
    assert!(completed_workspace_proof("before", None, 6, 6).is_none());
}
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let ty = entry.file_type().unwrap();
        let dst_path = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dst_path);
        } else {
            fs::copy(entry.path(), &dst_path).unwrap();
            if let Ok(meta) = entry.metadata() {
                let _ = fs::set_permissions(&dst_path, meta.permissions());
            }
        }
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = env::temp_dir().join(format!(
            "susi-local-workflow-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn repo_with_base(&self) -> (PathBuf, String) {
        static REPO_TPL: std::sync::OnceLock<(PathBuf, String)> = std::sync::OnceLock::new();
        let (tpl, base) = REPO_TPL.get_or_init(|| {
            let tpl = env::temp_dir().join(format!("susi-lw-repo-tpl-{}", std::process::id()));
            let _ = fs::remove_dir_all(&tpl);
            fs::create_dir_all(&tpl).unwrap();
            git(&tpl, &["init", "-q"]).unwrap();
            git(&tpl, &["config", "user.name", "Test"]).unwrap();
            git(&tpl, &["config", "user.email", "test@example.invalid"]).unwrap();
            git(&tpl, &["config", "commit.gpgsign", "false"]).unwrap();
            fs::write(tpl.join(".gitignore"), "/target/\n").unwrap();
            git(&tpl, &["add", ".gitignore"]).unwrap();
            git(
                &tpl,
                &[
                    "-c",
                    "core.hooksPath=",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "-qm",
                    "initial",
                ],
            )
            .unwrap();
            let head = git(&tpl, &["rev-parse", "HEAD"]).unwrap();
            (tpl, head)
        });
        let repo = self.0.join("repo");
        copy_dir_all(tpl, &repo);
        (repo, base.clone())
    }
    fn repo(&self) -> PathBuf {
        self.repo_with_base().0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn tracked_file_fixture(
    claimant: &str,
    path: &str,
    scope_path: &str,
    contents: &str,
) -> (Scratch, PathBuf, PathBuf) {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let file = repo.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, contents).unwrap();
    git(&repo, &["add", "--", path]).unwrap();
    git(
        &repo,
        &["-c", "core.hooksPath=", "commit", "-qm", "fixture file"],
    )
    .unwrap();
    let worktree = start(&repo, claimant, &[scope_path.to_owned()]).unwrap();
    (scratch, repo, worktree)
}

#[test]
fn commit_only_stages_an_already_staged_deletion_without_readding_its_path() {
    let (_scratch, repo, worktree) =
        tracked_file_fixture("staged-delete", "src/gone.txt", "src", "base\n");
    fs::remove_file(worktree.join("src/gone.txt")).unwrap();
    git(&worktree, &["add", "-u", "--", "src/gone.txt"]).unwrap();
    let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    commit(&worktree, "staged-delete", "delete tracked file", None).unwrap();
    assert_ne!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
    assert!(git(&worktree, &["cat-file", "-e", "HEAD:src/gone.txt"]).is_err());
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
    assert_eq!(integration_head(&repo).unwrap(), None);
}

#[test]
fn commit_only_stages_an_unstaged_deletion() {
    let path = "src/a [file].txt";
    let (_scratch, _repo, worktree) =
        tracked_file_fixture("unstaged-delete", path, "src", "base\n");
    fs::remove_file(worktree.join(path)).unwrap();
    commit(&worktree, "unstaged-delete", "delete tracked file", None).unwrap();
    assert!(git(&worktree, &["cat-file", "-e", "HEAD:src/a [file].txt"]).is_err());
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
}

#[test]
fn commit_only_stages_a_previously_staged_rename() {
    let (_scratch, _repo, worktree) =
        tracked_file_fixture("staged-rename", "src/old.txt", "src", "base\n");
    git(&worktree, &["mv", "--", "src/old.txt", "src/new.txt"]).unwrap();
    commit(&worktree, "staged-rename", "rename tracked file", None).unwrap();
    let names = git(&worktree, &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
    assert!(names.lines().any(|name| name == "src/new.txt"), "{names}");
    assert!(!names.lines().any(|name| name == "src/old.txt"), "{names}");
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
}

#[test]
fn commit_only_stages_the_unstaged_side_of_a_staged_revert() {
    let (_scratch, _repo, worktree) =
        tracked_file_fixture("staged-revert", "src/value.txt", "src", "base\n");
    fs::write(worktree.join("src/value.txt"), "staged\n").unwrap();
    git(&worktree, &["add", "--", "src/value.txt"]).unwrap();
    fs::write(worktree.join("src/value.txt"), "base\n").unwrap();
    let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    assert!(commit(&worktree, "staged-revert", "revert staged content", None).is_err());
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
}

#[test]
fn commit_only_stages_final_content_after_a_second_unstaged_edit() {
    let (_scratch, _repo, worktree) =
        tracked_file_fixture("staged-final", "src/value.txt", "src", "base\n");
    fs::write(worktree.join("src/value.txt"), "staged intermediate\n").unwrap();
    git(&worktree, &["add", "--", "src/value.txt"]).unwrap();
    fs::write(worktree.join("src/value.txt"), "final content\n").unwrap();
    commit(&worktree, "staged-final", "commit final content", None).unwrap();
    assert_eq!(
        git(&worktree, &["show", "HEAD:src/value.txt"]).unwrap(),
        "final content"
    );
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
}

#[test]
#[cfg(windows)]
fn commit_only_stages_a_scoped_windows_case_only_rename() {
    let (_scratch, _repo, worktree) =
        tracked_file_fixture("case-rename", "src/Task.json", "src", "base\n");
    git(
        &worktree,
        &["mv", "--force", "--", "src/Task.json", "src/task.json"],
    )
    .unwrap();
    commit(&worktree, "case-rename", "rename task casing", None).unwrap();
    let names = git(&worktree, &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
    assert!(names.lines().any(|name| name == "src/task.json"), "{names}");
    assert!(
        !names.lines().any(|name| name == "src/Task.json"),
        "{names}"
    );
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
}

#[test]
#[cfg(windows)]
fn commit_only_stages_exact_removal_from_a_duplicate_case_index() {
    let (_scratch, _repo, worktree) =
        tracked_file_fixture("case-index", "src/Task.json", "src", "base\n");
    let blob = git(&worktree, &["rev-parse", "HEAD:src/Task.json"]).unwrap();
    let duplicate = format!("100644,{blob},src/task.json");
    git(
        &worktree,
        &["update-index", "--add", "--cacheinfo", &duplicate],
    )
    .unwrap();
    git(
        &worktree,
        &[
            "-c",
            "core.hooksPath=",
            "commit",
            "-qm",
            "duplicate case fixture",
        ],
    )
    .unwrap();
    let index = git(&worktree, &["ls-files"]).unwrap();
    assert!(index.lines().any(|path| path == "src/Task.json"), "{index}");
    assert!(index.lines().any(|path| path == "src/task.json"), "{index}");

    git(
        &worktree,
        &["update-index", "--force-remove", "--", "src/task.json"],
    )
    .unwrap();
    let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    commit(
        &worktree,
        "case-index",
        "remove duplicate lowercase entry",
        None,
    )
    .unwrap();
    assert_ne!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
    let tree = git(&worktree, &["ls-tree", "-r", "--name-only", "HEAD"]).unwrap();
    assert!(tree.lines().any(|path| path == "src/Task.json"), "{tree}");
    assert!(!tree.lines().any(|path| path == "src/task.json"), "{tree}");
    assert!(worktree.join("src/Task.json").is_file());
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
}

#[test]
fn commit_only_stages_rejects_staged_out_of_scope_deletion() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    fs::write(repo.join("outside.txt"), "base\n").unwrap();
    git(&repo, &["add", "--", "outside.txt"]).unwrap();
    git(
        &repo,
        &["-c", "core.hooksPath=", "commit", "-qm", "outside fixture"],
    )
    .unwrap();
    let worktree = start(&repo, "outside-delete", &["src".into()]).unwrap();
    fs::remove_file(worktree.join("outside.txt")).unwrap();
    git(&worktree, &["add", "-u", "--", "outside.txt"]).unwrap();
    let before = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    let error = commit(&worktree, "outside-delete", "reject outside delete", None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("reserved scopes"), "{error}");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), before);
    assert_eq!(
        git(&worktree, &["diff", "--cached", "--name-status"]).unwrap(),
        "D\toutside.txt"
    );
}

#[test]
fn pinned_merge_requires_a_resolved_scoped_index_and_preserves_both_parents() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    git(&repo, &["config", "user.name", "Test"]).unwrap();
    git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
    let worker = start(&repo, "merge", &["docs".into()]).unwrap();
    fs::create_dir(worker.join("docs")).unwrap();
    fs::write(worker.join("docs/record"), "worker").unwrap();
    commit(&worker, "merge", "worker change", None).unwrap();
    let own = git(&worker, &["rev-parse", "HEAD"]).unwrap();
    fs::create_dir(repo.join("docs")).unwrap();
    fs::write(repo.join("docs/record"), "incoming").unwrap();
    git(&repo, &["add", "docs/record"]).unwrap();
    git(
        &repo,
        &["-c", "core.hooksPath=", "commit", "-qm", "incoming change"],
    )
    .unwrap();
    let incoming = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let refs = || {
        git(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        )
        .unwrap()
    };
    let before = refs();
    assert!(git(&worker, &["merge", "--no-ff", "--no-commit", &incoming]).is_err());
    let finish = |parent: &str| scoped_commit(&worker, "merge", "join", None, Some(parent));
    assert!(finish(&incoming).is_err()); // Unresolved add/add conflict.
    fs::write(worker.join("docs/record"), "worker and incoming").unwrap();
    git(&worker, &["add", "docs/record"]).unwrap();
    assert!(commit(&worker, "merge", "normal commit", None).is_err());
    assert!(finish(&own).is_err());
    assert!(scoped_commit(
        &worker,
        "merge",
        "join",
        Some(&"a".repeat(40)),
        Some(&incoming)
    )
    .is_err());

    fs::write(worker.join("outside"), "untracked").unwrap();
    assert!(finish(&incoming).is_err());
    git(&worker, &["add", "outside"]).unwrap();
    assert!(finish(&incoming).is_err());
    git(&worker, &["reset", "--", "outside"]).unwrap();
    fs::remove_file(worker.join("outside")).unwrap();
    fs::write(worker.join("docs/record"), "unstaged").unwrap();
    assert!(finish(&incoming).is_err());
    git(&worker, &["restore", "--worktree", "--", "docs/record"]).unwrap();
    let git_path = |name: &str| {
        PathBuf::from(
            git(
                &worker,
                &["rev-parse", "--path-format=absolute", "--git-path", name],
            )
            .unwrap(),
        )
    };
    let merge_head = git_path("MERGE_HEAD");
    fs::write(&merge_head, format!("{incoming}\n{own}\n")).unwrap();
    assert!(finish(&incoming).is_err());
    fs::write(&merge_head, format!("{incoming}\n")).unwrap();
    let sequencer = git_path("sequencer");
    fs::create_dir(&sequencer).unwrap();
    assert!(finish(&incoming).is_err());
    fs::remove_dir(sequencer).unwrap();
    assert_eq!(before, refs());

    let tree = git(&worker, &["write-tree"]).unwrap();
    finish(&incoming).unwrap();
    let merged = git(&worker, &["rev-parse", "HEAD"]).unwrap();
    assert_eq!(
        git(&worker, &["show", "-s", "--format=%T %P", "HEAD"]).unwrap(),
        format!("{tree} {own} {incoming}")
    );
    assert_eq!(
        refs(),
        before.replace(
            &format!("refs/heads/local-agent/merge {own}"),
            &format!("refs/heads/local-agent/merge {merged}")
        )
    );
    assert!(git(&worker, &["status", "--porcelain"]).unwrap().is_empty());
    assert!(!merge_head.exists());

    // Simulate interruption after the compare-and-swap, before Git-state cleanup.
    fs::write(&merge_head, format!("{incoming}\n")).unwrap();
    finish(&incoming).unwrap();
    assert_eq!(git(&worker, &["rev-parse", "HEAD"]).unwrap(), merged);
    assert!(!merge_head.exists());
}

#[test]
fn extension_preserves_base_and_rejects_conflicts_without_mutation() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let first = start(&repo, "first", &["src/first".into()]).unwrap();
    start(&repo, "second", &["src/second".into()]).unwrap();
    let claim_path = state(&repo).unwrap().join("claims/first.claim");
    let base = read_claim(&claim_path).unwrap().base;
    update_scopes(&first, "first", &["tests".into()], ScopeChange::Extend).unwrap();
    let claim = read_claim(&claim_path).unwrap();
    assert_eq!(claim.base, base);
    assert!(claim.scopes.contains(&"tests".into()));
    let before = fs::read(&claim_path).unwrap();
    assert!(update_scopes(&first, "first", &["src".into()], ScopeChange::Extend).is_err());
    assert!(update_scopes(&repo, "first", &["docs".into()], ScopeChange::Extend).is_err());
    fs::write(first.join("dirty"), "pending").unwrap();
    assert!(update_scopes(&first, "first", &["docs".into()], ScopeChange::Extend).is_err());
    assert_eq!(fs::read(claim_path).unwrap(), before);
}

#[test]
fn a_claim_recorded_on_another_os_is_listed_and_still_reserves_its_scopes() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let state = state(&repo).unwrap();
    let claims_dir = state.join("claims");
    fs::create_dir_all(&claims_dir).unwrap();
    let foreign = if cfg!(windows) {
        "/mnt/shared/src/repo-agent-far".to_owned()
    } else {
        "F:/src/github.com/intellibitz\\repo-agent-far".to_owned()
    };
    let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let recorded = format!("susi-local-claim-v2\nfar\n{foreign}\n{base}\nsrc/far\n");
    fs::write(claims_dir.join("far.claim"), &recorded).unwrap();

    let listed = claims(&state).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].is_local());
    assert_eq!(listed[0].worktree, PathBuf::from(&foreign));
    let status = workflow_status(&repo).unwrap();
    assert!(status_text(&status).contains("[other host]"));
    assert_eq!(status_json(&status)["claims"][0]["local"], false);

    let error = start(&repo, "near", &["src/far/x.rs".into()]).unwrap_err();
    assert!(error.to_string().contains("overlaps agent far"), "{error}");
    let worktree = start(&repo, "near", &["src/near".into()]).unwrap();
    assert!(claims(&state)
        .unwrap()
        .iter()
        .any(|c| c.name == "near" && c.is_local()));
    // Started from an agent worktree, the new one still sits beside the primary.
    let nested = start(&worktree, "nested", &["src/nested".into()]).unwrap();
    let primary_name = repo.file_name().unwrap().to_str().unwrap();
    // Compared as files, not strings: on a Windows host whose TEMP is an
    // 8.3 short path (C:\Users\ABCDEF~1\...), the scratch repo is named
    // that way while git reports the long form for the same directory.
    assert_eq!(
        fs::canonicalize(&nested).unwrap(),
        fs::canonicalize(
            repo.parent()
                .unwrap()
                .join(format!("{primary_name}-agent-nested"))
        )
        .unwrap()
    );

    let error = release(&repo, "far").unwrap_err();
    assert!(error.to_string().contains("another host"), "{error}");
    release(&worktree, "near").unwrap();
    release(&nested, "nested").unwrap();
    assert_eq!(
        fs::read_to_string(claims_dir.join("far.claim")).unwrap(),
        recorded
    );

    for relative in ["relative/path", "x:", "C:relative"] {
        fs::write(
            claims_dir.join("far.claim"),
            format!("susi-local-claim-v2\nfar\n{relative}\n{base}\nsrc/far\n"),
        )
        .unwrap();
        assert!(claims(&state).is_err(), "{relative}");
    }
}

#[test]
fn path_validation_blocks_windows_aliases_and_traversal() {
    for bad in [
        "",
        ".",
        "../src",
        "a/../b",
        "C:/src",
        "\\\\host\\share",
        "src/",
        "src//x",
        "NUL.rs",
        "src/COM1",
        ".git/config",
        "src/a.",
        "src/a ",
        "src/a:b",
        "src/\nx",
    ] {
        assert!(scope(bad).is_err(), "{bad}");
    }
    assert_eq!(scope("Src\\CLI").unwrap(), "src/cli");
    assert!(overlaps("src", "src/cli"));
    assert!(!overlaps("src", "src2"));
    for bad in ["con", "aux", "a/b", "-abc", "abc-", "A", ""] {
        assert!(name(bad).is_err());
    }
}

#[test]
fn unused_scope_can_transfer_without_orphaning_committed_changes() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = start(
        &repo,
        "builder",
        &["runtime.rs".into(), "Cargo.toml".into()],
    )
    .unwrap();
    fs::write(worktree.join("runtime.rs"), "changed").unwrap();
    commit(&worktree, "builder", "retain changed runtime", None).unwrap();
    let path = state(&repo).unwrap().join("claims/builder.claim");
    let original = fs::read(&path).unwrap();
    for scopes in [
        vec!["runtime.rs".into()],
        vec!["absent".into()],
        vec!["runtime.rs".into(), "Cargo.toml".into()],
    ] {
        assert!(update_scopes(&worktree, "builder", &scopes, ScopeChange::Shrink).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    fs::write(worktree.join("pending"), "keep dirty changes").unwrap();
    assert!(update_scopes(
        &worktree,
        "builder",
        &["Cargo.toml".into()],
        ScopeChange::Shrink
    )
    .is_err());
    assert_eq!(fs::read(&path).unwrap(), original);
    fs::remove_file(worktree.join("pending")).unwrap();
    update_scopes(
        &worktree,
        "builder",
        &["Cargo.toml".into()],
        ScopeChange::Shrink,
    )
    .unwrap();
    assert_eq!(read_claim(&path).unwrap().scopes, vec!["runtime.rs"]);
    start(&repo, "version", &["Cargo.toml".into()]).unwrap();
    assert_eq!(
        git(&worktree, &["show", "HEAD:runtime.rs"]).unwrap(),
        "changed"
    );
}
#[test]
fn reservation_is_exclusive_and_lock_is_released() {
    let scratch = Scratch::new();
    let lock = Lock::acquire(&scratch.0).unwrap();
    assert!(Lock::acquire(&scratch.0).is_err());
    drop(lock);
    assert!(Lock::acquire(&scratch.0).is_ok());
}
#[test]
fn local_worktrees_reject_overlap_and_preserve_dirty_work() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let first = start(&repo, "one", &["src".into()]).unwrap();
    assert!(first.join("target").is_dir());
    assert!(start(&repo, "two", &["SRC/cli".into()]).is_err());
    let second = start(&repo, "two", &["tests".into()]).unwrap();
    assert_ne!(first, second);
    fs::write(first.join("dirty.rs"), "preserve me").unwrap();
    assert!(release(&repo, "one").is_err());
    assert_eq!(
        fs::read_to_string(first.join("dirty.rs")).unwrap(),
        "preserve me"
    );
    release(&repo, "two").unwrap();
    assert!(second.exists());
    assert!(git(
        &repo,
        &["rev-parse", "--verify", "refs/heads/local-agent/two"]
    )
    .is_ok());
    // A clean, released, registered worktree is reused in place.
    assert_eq!(start(&repo, "two", &["tests".into()]).unwrap(), second);
    // The dirty, still-claimed one is never taken over.
    assert!(start(&repo, "one", &["src".into()]).is_err());
    assert_eq!(
        fs::read_to_string(first.join("dirty.rs")).unwrap(),
        "preserve me"
    );
}
#[test]
fn parallel_scope_claims_have_one_winner() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ["left", "right"]
        .into_iter()
        .map(|agent| {
            let repo = repo.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                start(&repo, agent, &["src".into()]).is_ok()
            })
        })
        .collect();
    let winners = handles
        .into_iter()
        .map(|handle| usize::from(handle.join().unwrap()))
        .sum::<usize>();
    assert_eq!(winners, 1);
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
}

#[test]
fn corrupt_claim_fails_closed() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let state = state(&repo).unwrap();
    fs::create_dir_all(state.join("claims")).unwrap();
    fs::write(state.join("claims/broken.claim"), "bad").unwrap();
    assert!(start(&repo, "one", &["src".into()]).is_err());
}

#[test]
fn commit_rejects_wrong_worktree_and_outside_scope_without_staging() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = start(&repo, "one", &["src".into()]).unwrap();
    fs::create_dir(worktree.join("src")).unwrap();
    fs::write(worktree.join("src/allowed.rs"), "allowed").unwrap();
    assert!(commit(&repo, "one", "wrong owner", None).is_err());
    fs::write(worktree.join("outside.rs"), "outside").unwrap();
    assert!(commit(&worktree, "one", "reject", None).is_err());
    assert!(git(&worktree, &["diff", "--cached", "--name-only"])
        .unwrap()
        .is_empty());
    fs::remove_file(worktree.join("outside.rs")).unwrap();
    // A staged delta cancelled by an unstaged edit still must be inspected.
    fs::write(worktree.join(".gitignore"), "changed\n").unwrap();
    git(&worktree, &["add", ".gitignore"]).unwrap();
    fs::write(worktree.join(".gitignore"), "/target/\n").unwrap();
    assert!(commit(&worktree, "one", "reject staged outside", None).is_err());
}

#[test]
fn local_commit_suppresses_only_its_own_hooks_and_preserves_shared_setting() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = start(&repo, "one", &["src".into()]).unwrap();
    git(&repo, &["config", "user.name", "Test"]).unwrap();
    git(&repo, &["config", "user.email", "test@example.invalid"]).unwrap();
    let hooks = scratch.0.join("failing-hooks");
    fs::create_dir(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(&repo, &["config", "core.hooksPath", text(&hooks).unwrap()]).unwrap();
    fs::create_dir(worktree.join("src")).unwrap();
    fs::write(worktree.join("src/a space.rs"), "allowed").unwrap();
    git(&worktree, &["add", "--", "src/a space.rs"]).unwrap();
    assert!(git(&worktree, &["commit", "-m", "blocked by hook"]).is_err());
    commit(&worktree, "one", "local change", None).unwrap();
    assert!(git(&worktree, &["status", "--porcelain"])
        .unwrap()
        .is_empty());
    assert_eq!(
        git(&repo, &["config", "--get", "core.hooksPath"]).unwrap(),
        text(&hooks).unwrap()
    );
    assert!(git(&worktree, &["log", "-1", "--format=%B"])
        .unwrap()
        .contains("Task: LOCAL-one"));
}

#[test]
fn signing_failure_preserves_the_previous_head_and_unsigned_retry() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = start(&repo, "signer", &["owned.rs".into()]).unwrap();
    let original = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    fs::write(worktree.join("owned.rs"), "scoped change").unwrap();
    git(
        &repo,
        &[
            "config",
            "gpg.program",
            text(&scratch.0.join("missing-signer")).unwrap(),
        ],
    )
    .unwrap();
    for key in ["ABC123", "0000000000000000000000000000000000000000"] {
        assert!(commit(&worktree, "signer", "sign", Some(key)).is_err());
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]).unwrap(), original);
    }
    commit(&worktree, "signer", "unsigned retry", None).unwrap();
    assert_eq!(
        git(&worktree, &["show", "HEAD:owned.rs"]).unwrap(),
        "scoped change"
    );
}

fn worker(repo: &Path, agent: &str, file: &str, content: &str) -> PathBuf {
    let worktree = start(repo, agent, &[file.into()]).unwrap();
    fs::write(worktree.join(file), content).unwrap();
    commit(&worktree, agent, "fixture change", None).unwrap();
    worktree
}

#[test]
fn batch_retries_all_claims_and_publishes_only_the_verified_combination() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let workers = ["left", "right"];
    for name in workers {
        worker(&repo, name, &format!("{name}.rs"), name);
    }
    let receipts = "refs/susi/integration/receipts/";
    assert!(integrate_with(&repo, &workers, |candidate, _| {
        assert!(candidate.join("left.rs").is_file());
        assert!(candidate.join("right.rs").is_file());
        Err("combined fixture rejected".into())
    })
    .is_err());
    assert!(integration_head(&repo).unwrap().is_none());
    assert!(git(&repo, &["for-each-ref", receipts]).unwrap().is_empty());
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
    let head = integrate_with(&repo, &workers, |candidate, _| {
        for name in workers {
            assert_eq!(
                fs::read_to_string(candidate.join(format!("{name}.rs")))?,
                name
            );
        }
        Ok(vec![
            "test-only combined fixture => exit code: 0; both files verified".into(),
        ])
    })
    .unwrap();
    let tree = git(&repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
    for name in workers {
        let source = git(&repo, &["rev-parse", &format!("local-agent/{name}")]).unwrap();
        let reference = format!("{receipts}{name}/{source}");
        let receipt = git(&repo, &["cat-file", "blob", &reference]).unwrap();
        assert!(receipt_matches(&repo, &reference, &receipt, &head, &tree).unwrap());
    }
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
}

#[test]
fn integration_tests_the_combined_tree_and_publishes_atomic_receipt() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let original = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let left = worker(&repo, "left", "left.rs", "left");
    let right = worker(&repo, "right", "right.rs", "right");
    let source = git(&right, &["rev-parse", "HEAD"]).unwrap();
    fs::write(repo.join("primary-dirty.txt"), "preserve primary").unwrap();
    let mut first_candidate = PathBuf::new();
    let first = integrate_with(&repo, &["left"], |candidate, _| {
        first_candidate = candidate.to_path_buf();
        let mut build_output = candidate.join("target");
        for _ in 0..6 {
            build_output.push("nested-windows-artifact-directory-with-a-long-name");
        }
        fs::create_dir_all(&build_output)?;
        fs::write(build_output.join("probe"), "build output")?;
        assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
        assert!(!candidate.join("right.rs").exists());
        Ok(vec![
            "test-only content verification passed => exit code: 0; fixture verifier completed"
                .into(),
        ])
    })
    .unwrap();
    assert!(
        !first_candidate.exists(),
        "accepted scratch tree was retained"
    );
    let second = integrate_with(&repo, &["right"], |candidate, _| {
        assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
        assert_eq!(fs::read_to_string(candidate.join("right.rs"))?, "right");
        Ok(vec!["test-only combined content verification passed => exit code: 0; fixture verifier completed".into()])
    })
    .unwrap();
    assert_ne!(first, second);
    assert_eq!(integration_head(&repo).unwrap(), Some(second.clone()));
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]).unwrap(), original);
    assert_eq!(
        fs::read_to_string(repo.join("primary-dirty.txt")).unwrap(),
        "preserve primary"
    );
    assert!(left.exists() && right.exists());
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    let receipt_ref = format!("refs/susi/integration/receipts/right/{source}");
    let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
    assert!(receipt.contains(&format!("commit={second}")));
    assert!(receipt.contains(&format!("previous={first}")));
    assert!(receipt.contains(&format!("source={source}")));
    let third = start(&repo, "third", &["third.rs".into()]).unwrap();
    assert_eq!(git(&third, &["rev-parse", "HEAD"]).unwrap(), second);
}

#[test]
fn combined_integration_keeps_all_claims_on_failure_and_publishes_all_receipts() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let left = worker(&repo, "left", "left.rs", "left");
    // A dependent worker starts from the first worker's private source.
    let right = worker(&left, "right", "right.rs", "right");
    let left_source = git(&left, &["rev-parse", "HEAD"]).unwrap();
    let right_source = git(&right, &["rev-parse", "HEAD"]).unwrap();
    assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
        Err("deliberate gate failure".into())
    })
    .is_err());
    assert!(integration_head(&repo).unwrap().is_none());
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
    assert!(integrate_many_with(&repo, &["left", "left"], |_, _| {
        panic!("duplicate sources reached verification")
    })
    .is_err());
    let head = integrate_many_with(&repo, &["left", "right"], |candidate, _| {
        assert_eq!(fs::read_to_string(candidate.join("left.rs"))?, "left");
        assert_eq!(fs::read_to_string(candidate.join("right.rs"))?, "right");
        Ok(vec!["fixture combined verification => exit code: 0".into()])
    })
    .unwrap();
    for (name, source) in [("left", left_source), ("right", right_source)] {
        let receipt = git(
            &repo,
            &[
                "cat-file",
                "blob",
                &format!("refs/susi/integration/receipts/{name}/{source}"),
            ],
        )
        .unwrap();
        assert!(receipt.contains(&format!("commit={head}")));
        assert!(receipt.contains(&format!("source={source}")));
    }
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
}

#[test]
fn combined_integration_cannot_mutually_admit_unaccepted_bases() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    worker(&repo, "anchor", "anchor.rs", "anchor");
    fs::write(repo.join("unowned.rs"), "not accepted").unwrap();
    git(&repo, &["add", "unowned.rs"]).unwrap();
    git(
        &repo,
        &["-c", "core.hooksPath=", "commit", "-qm", "unowned base"],
    )
    .unwrap();
    worker(&repo, "left", "left.rs", "left");
    worker(&repo, "right", "right.rs", "right");
    let accepted = integrate_with(&repo, &["anchor"], |_, _| {
        Ok(vec!["fixture verification => exit code: 0".into()])
    })
    .unwrap();
    assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
        panic!("unaccepted shared base reached verification")
    })
    .is_err());
    assert_eq!(integration_head(&repo).unwrap(), Some(accepted));
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
}

#[test]
fn combined_integration_rechecks_every_source_and_scope() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    worker(&repo, "left", "left.rs", "left");
    let right = worker(&repo, "right", "right.rs", "right");
    assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
        fs::write(right.join("right.rs"), "advanced")?;
        commit(&right, "right", "advance during gate", None)?;
        Ok(vec!["fixture verification => exit code: 0".into()])
    })
    .is_err());
    assert!(integration_head(&repo).unwrap().is_none());
    fs::write(right.join("outside.rs"), "unowned").unwrap();
    git(&right, &["add", "outside.rs"]).unwrap();
    git(
        &right,
        &["-c", "core.hooksPath=", "commit", "-qm", "outside"],
    )
    .unwrap();
    assert!(integrate_many_with(&repo, &["left", "right"], |_, _| {
        panic!("unowned source reached verification")
    })
    .is_err());
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 2);
}

#[test]
fn accepted_imports_do_not_expand_the_workers_reserved_scope() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let right = worker(&repo, "right", "right.rs", "right");
    worker(&repo, "left", "left.rs", "left");
    let accepted = integrate_with(&repo, &["left"], |_, _| {
        Ok(vec![
            "fixture verification => exit code: 0; fixture verifier completed".into(),
        ])
    })
    .unwrap();
    git(
        &right,
        &["-c", "core.hooksPath=", "merge", "--no-edit", &accepted],
    )
    .unwrap();
    let claim = read_claim(&state(&repo).unwrap().join("claims/right.claim")).unwrap();
    assert!(
        check_source(&repo, &claim).is_ok(),
        "previously accepted left.rs is not new worker work"
    );
    fs::write(right.join("outside.rs"), "new unauthorized work").unwrap();
    git(&right, &["add", "outside.rs"]).unwrap();
    git(
        &right,
        &["-c", "core.hooksPath=", "commit", "-qm", "outside"],
    )
    .unwrap();
    assert!(check_source(&repo, &claim)
        .unwrap_err()
        .to_string()
        .contains("outside reserved scope"));
    // Even an accepted import cannot authorize a later edit to its file.
    git(&right, &["revert", "--no-edit", "HEAD"]).unwrap();
    fs::write(right.join("left.rs"), "tampered accepted import").unwrap();
    git(&right, &["add", "left.rs"]).unwrap();
    git(
        &right,
        &["-c", "core.hooksPath=", "commit", "-qm", "tamper"],
    )
    .unwrap();
    assert!(check_source(&repo, &claim)
        .unwrap_err()
        .to_string()
        .contains("outside reserved scope"));
}

#[test]
fn scope_exemption_requires_related_accepted_history_and_receipt() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let right = worker(&repo, "right", "right.rs", "right");
    worker(&repo, "left", "left.rs", "left");
    let accepted = integrate_with(&repo, &["left"], |_, _| {
        Ok(vec![
            "fixture verification => exit code: 0; fixture verifier completed".into(),
        ])
    })
    .unwrap();
    git(
        &right,
        &["-c", "core.hooksPath=", "merge", "--no-edit", &accepted],
    )
    .unwrap();
    let claim = read_claim(&state(&repo).unwrap().join("claims/right.claim")).unwrap();
    let receipts = git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/susi/integration/receipts/",
        ],
    )
    .unwrap();
    for receipt in receipts.lines() {
        let raw = git(&repo, &["cat-file", "blob", receipt]).unwrap();
        let tree = git(&repo, &["rev-parse", &format!("{accepted}^{{tree}}")]).unwrap();
        assert!(receipt_matches(&repo, receipt, &raw, &accepted, &tree).unwrap());
        for malformed in [
            format!("susi-local-integration-v1\ncommit={accepted}\ntree={tree}\n"),
            format!("{raw}\ncommit={accepted}\n"),
            raw.replace(
                "fixture verification => exit code: 0; fixture verifier completed",
                "hello",
            ),
            raw.replace("agent=left", "agent=other"),
            raw.replace(
                "fixture verification",
                "cargo test => exit code: 1; elapsed-ms=1",
            ),
        ] {
            assert!(!receipt_matches(&repo, receipt, &malformed, &accepted, &tree).unwrap());
        }
        git(&repo, &["update-ref", "-d", receipt]).unwrap();
    }
    assert!(check_source(&repo, &claim)
        .unwrap_err()
        .to_string()
        .contains("matching durable"));
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated history"]).unwrap();
    git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
    assert!(check_source(&repo, &claim)
        .unwrap_err()
        .to_string()
        .contains("does not descend"));
}

#[test]
fn release_accepts_work_the_publisher_landed_on_fetched_main() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = worker(&repo, "pub", "pub.rs", "published");
    let head = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    // Not integrated and not on the fetched main: the reservation stays.
    let error = release(&repo, "pub").unwrap_err();
    assert!(
        error.to_string().contains("reservation retained"),
        "{error}"
    );
    // `local-snapshot submit` published it; a fetch mirrors that main.
    let main = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let tree = git(&worktree, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let merged = git(
        &repo,
        &[
            "commit-tree",
            &tree,
            "-p",
            &main,
            "-p",
            &head,
            "-m",
            "submission",
        ],
    )
    .unwrap();
    git(&repo, &["update-ref", FETCHED_MAIN_REF, &merged]).unwrap();
    release(&repo, "pub").unwrap();
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    // The next claim starts from that published main (it descends from
    // the stale integration ref) and reuses the worktree, whose branch
    // holds nothing the published main lacks.
    git(&repo, &["update-ref", INTEGRATION_REF, &main]).unwrap();
    let again = start(&repo, "pub", &["pub.rs".to_owned()]).unwrap();
    assert_eq!(again, worktree);
    assert_eq!(git(&again, &["rev-parse", "HEAD"]).unwrap(), merged);
    // Diverged refs keep the integration ref as the base.
    release(&repo, "pub").unwrap();
    let side = git(&repo, &["commit-tree", &tree, "-p", &main, "-m", "side"]).unwrap();
    git(&repo, &["update-ref", INTEGRATION_REF, &side]).unwrap();
    assert_eq!(claim_base(&repo).unwrap(), side);
}

#[test]
fn failed_gate_retains_source_claim_and_previous_integration() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = worker(&repo, "one", "one.rs", "one");
    assert!(release(&repo, "one").is_err());
    let result = integrate_with(&repo, &["one"], |_, attempt| {
        fs::write(attempt.join("gate-0.log"), "test failure evidence")?;
        Err("deliberate test-only gate failure".into())
    });
    assert!(result.is_err());
    assert_eq!(integration_head(&repo).unwrap(), None);
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    assert_eq!(fs::read_to_string(worktree.join("one.rs")).unwrap(), "one");
    let attempts = state(&repo).unwrap().join("attempts");
    assert!(fs::read_dir(attempts).unwrap().any(|entry| entry
        .unwrap()
        .path()
        .join("status")
        .is_file()));
}

#[test]
fn integration_rechecks_committed_scope_and_candidate_mutation() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = worker(&repo, "one", "one.rs", "one");
    assert!(integrate_with(&repo, &["one"], |candidate, _| {
        fs::write(candidate.join("one.rs"), "modified after verification")?;
        Ok(vec![
            "test-only check => exit code: 0; fixture verifier completed".into(),
        ])
    })
    .is_err());
    fs::write(worktree.join("outside.rs"), "escape").unwrap();
    git(&worktree, &["add", "outside.rs"]).unwrap();
    git(
        &worktree,
        &["-c", "core.hooksPath=", "commit", "-qm", "outside scope"],
    )
    .unwrap();
    assert!(integrate_with(&repo, &["one"], |_, _| panic!(
        "scope violation reached gate"
    ))
    .is_err());
    assert_eq!(integration_head(&repo).unwrap(), None);
}

#[test]
fn integration_cas_refuses_external_advancement_without_publishing_receipt() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = worker(&repo, "one", "one.rs", "one");
    let source = git(&worktree, &["rev-parse", "HEAD"]).unwrap();
    let external = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    assert!(integrate_with(&repo, &["one"], |_, _| {
        git(&repo, &["update-ref", INTEGRATION_REF, &external])?;
        Ok(vec![
            "test-only check => exit code: 0; fixture verifier completed".into(),
        ])
    })
    .is_err());
    assert_eq!(integration_head(&repo).unwrap(), Some(external));
    assert!(git(
        &repo,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/susi/integration/receipts/one/{source}")
        ]
    )
    .is_err());
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
}

#[test]
fn source_advancement_during_gate_cannot_be_promoted_as_tested() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let worktree = worker(&repo, "one", "one.rs", "one");
    assert!(integrate_with(&repo, &["one"], |_, _| {
        fs::write(worktree.join("one.rs"), "new untested source")?;
        commit(&worktree, "one", "advanced during verification", None)?;
        Ok(vec![
            "test-only prior-head check => exit code: 0; fixture verifier completed".into(),
        ])
    })
    .is_err());
    assert_eq!(integration_head(&repo).unwrap(), None);
}

#[test]
// Queue this long real qualification early; preserve every assertion below.
fn a_critical_path_real_native_cargo_gates_accept_a_passing_fixture() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let native_worker = start(
        &repo,
        "native",
        &[
            "Cargo.toml".into(),
            "Cargo.lock".into(),
            "deny.toml".into(),
            "src".into(),
            ".agents/tasks/T-FIXTURE-1.json".into(),
        ],
    )
    .unwrap();
    fs::create_dir(native_worker.join("src")).unwrap();
    fs::write(native_worker.join("Cargo.toml"), "[package]\nname = \"local-gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n") .unwrap();
    fs::write(
        native_worker.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"local-gate-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        native_worker.join("deny.toml"),
        "[licenses]\nallow = [\"MIT\"]\n",
    )
    .unwrap();
    fs::write(
        native_worker.join("src/lib.rs"),
        "#[test]\nfn actual_test() {\n    assert_eq!(2 + 2, 4);\n}\n",
    )
    .unwrap();
    fs::create_dir_all(native_worker.join(".agents/tasks")).unwrap();
    fs::write(native_worker.join(".agents/tasks/T-FIXTURE-1.json"),
        serde_json::to_vec(&serde_json::json!({"id":"T-FIXTURE-1","native_worker":"native",
            "status":"in_progress","scopes":["Cargo.toml","Cargo.lock","deny.toml","src",".agents/tasks/T-FIXTURE-1.json"],
            "accept":{"cmd":["cargo","test","--offline","--locked"]}})).unwrap()).unwrap();
    commit(&native_worker, "native", "native gate fixture", None).unwrap();
    let second = start(
        &repo,
        "second",
        &["tests".into(), ".agents/tasks/T-SECOND-1.json".into()],
    )
    .unwrap();
    fs::create_dir_all(second.join("tests")).unwrap();
    fs::create_dir_all(second.join(".agents/tasks")).unwrap();
    fs::write(
        second.join("tests/second.rs"),
        "#[test]\nfn second_actual_test() {\n    assert_eq!(3 + 3, 6);\n}\n",
    )
    .unwrap();
    fs::write(second.join(".agents/tasks/T-SECOND-1.json"),serde_json::to_vec(&serde_json::json!({
        "id":"T-SECOND-1","native_worker":"second","status":"in_progress",
        "scopes":["tests",".agents/tasks/T-SECOND-1.json"],"accept":{"cmd":["cargo","test","--offline","--locked"]}
    })).unwrap()).unwrap();
    commit(&second, "second", "second native gate fixture", None).unwrap();
    let head = integrate_many_with(&repo, &["native", "second"], verify_candidate).unwrap();
    assert_eq!(integration_head(&repo).unwrap(), Some(head.clone()));
    verify_real_native_provenance(&repo, &scratch.0, &head);
    let broken = worker(
        &repo,
        "broken",
        "src/lib.rs",
        "#[test]\nfn failing_test() {\n    let actual = std::hint::black_box(4);\n    assert_eq!(actual, 5);\n}\n",
    );
    extend(&broken, "broken", &[".agents/tasks/T-BROKEN-1.json".into()]).unwrap();
    fs::write(
        broken.join(".agents/tasks/T-BROKEN-1.json"),
        serde_json::to_vec(
            &serde_json::json!({"id":"T-BROKEN-1","native_worker":"broken",
            "status":"in_progress","scopes":["src/lib.rs",".agents/tasks/T-BROKEN-1.json"],
            "accept":{"cmd":["cargo","test","--offline","--locked"]}}),
        )
        .unwrap(),
    )
    .unwrap();
    commit(&broken, "broken", "bind failing fixture acceptance", None).unwrap();
    let failure = integrate_with(&repo, &["broken"], verify_candidate).unwrap_err();
    eprintln!("failing fixture gate: {failure}");
    assert_eq!(integration_head(&repo).unwrap(), Some(head));
    assert!(broken.exists());
    let attempts = state(&repo).unwrap().join("attempts");
    let failed_blob = fs::read_dir(attempts)
        .unwrap()
        .find_map(|entry| {
            let path = entry.unwrap().path();
            fs::read_to_string(path.join("gate-3.log"))
                .is_ok_and(|log| log.contains("test result: FAILED"))
                .then(|| {
                    fs::read_to_string(path.join("gate-3.log.oid"))
                        .unwrap()
                        .trim()
                        .to_owned()
                })
        })
        .expect("the genuine failing gate retains a content-addressed log");
    git(&repo, &["gc", "--prune=now"]).unwrap();
    assert!(git(&repo, &["cat-file", "blob", &failed_blob])
        .unwrap()
        .contains("test result: FAILED"));
}

fn verify_real_native_provenance(repo: &Path, scratch: &Path, head: &str) {
    let base = git(repo, &["rev-parse", "HEAD"]).unwrap();
    let base_tree = git(repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let accepted_tree = git(repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
    let diagnostics = b"test fixture: historical ownership incomplete; forward acceptance only\n";
    let receipts: Vec<_> = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/susi/provenance/receipts/",
        ],
    )
    .unwrap()
    .lines()
    .map(str::to_owned)
    .collect();
    assert_eq!(receipts.len(), 2);
    let verifier = native_provenance::file_digest(&env::current_exe().unwrap()).unwrap();
    let policy = native_provenance::policy_digest();
    let permit = serde_json::json!({"schema":native_provenance::PERMIT_SCHEMA,
        "verifier_sha256":verifier,"policy_sha256":policy,
        "source":{"commit":head,"tree":accepted_tree},
        "baseline":{"commit":base,"tree":base_tree,"historical_ownership":"incomplete",
            "diagnostics_blob":native_provenance::store_blob(repo,diagnostics).unwrap(),
            "diagnostics_sha256":native_provenance::digest(diagnostics)},"receipts":receipts});
    let path = scratch.join("independent-permit.json");
    let check = |value: &serde_json::Value, expected_head: &str| {
        let bytes = serde_json::to_vec(value).unwrap();
        fs::write(&path, &bytes).unwrap();
        native_provenance::verify(
            repo,
            &path,
            &native_provenance::TrustPins {
                permit_sha256: &native_provenance::digest(&bytes),
                verifier_sha256: &verifier,
                policy_sha256: &policy,
            },
            expected_head,
        )
    };
    let report = check(&permit, head).unwrap();
    assert_eq!(report["verified"], true);
    assert_eq!(report["historical_ownership"], "incomplete");
    let failed_bytes = b"failed-attempt evidence not present in any accepted receipt\n";
    let failed_blob = native_provenance::store_blob(repo, failed_bytes).unwrap();
    assert_eq!(
        native_provenance::store_blob(repo, failed_bytes).unwrap(),
        failed_blob
    );
    let failed_reference = format!("refs/susi/evidence/objects/{failed_blob}");
    let missing_target = "refs/susi/evidence/missing-fixture-target";
    git(repo, &["symbolic-ref", &failed_reference, missing_target]).unwrap();
    assert!(native_provenance::store_blob(repo, failed_bytes).is_err());
    assert_eq!(
        git(repo, &["symbolic-ref", "--quiet", &failed_reference]).unwrap(),
        missing_target
    );
    git(repo, &["symbolic-ref", "--delete", &failed_reference]).unwrap();
    assert_eq!(
        native_provenance::store_blob(repo, failed_bytes).unwrap(),
        failed_blob
    );
    git(repo, &["repack", "-ad"]).unwrap();
    git(repo, &["gc", "--prune=now"]).unwrap();
    assert_eq!(check(&permit, head).unwrap()["verified"], true);
    assert_eq!(
        git(repo, &["cat-file", "blob", &failed_blob]).unwrap(),
        String::from_utf8_lossy(failed_bytes).trim()
    );
    let anchored: serde_json::Value =
        serde_json::from_str(&git(repo, &["cat-file", "blob", &receipts[0]]).unwrap()).unwrap();
    let claim_oid = anchored["claim"]["blob"].as_str().unwrap();
    let claim_reference = format!("refs/susi/evidence/objects/{claim_oid}");
    let claim_bytes = git(repo, &["cat-file", "blob", claim_oid]).unwrap() + "\n";
    git(
        repo,
        &[
            "update-ref",
            "--no-deref",
            "-d",
            &claim_reference,
            claim_oid,
        ],
    )
    .unwrap();
    assert!(check(&permit, head).is_err());
    assert_eq!(
        native_provenance::store_blob(repo, claim_bytes.as_bytes()).unwrap(),
        claim_oid
    );
    git(
        repo,
        &[
            "update-ref",
            "--no-deref",
            &claim_reference,
            &failed_blob,
            claim_oid,
        ],
    )
    .unwrap();
    assert!(native_provenance::store_blob(repo, claim_bytes.as_bytes()).is_err());
    assert!(check(&permit, head).is_err());
    git(
        repo,
        &[
            "update-ref",
            "--no-deref",
            &claim_reference,
            claim_oid,
            &failed_blob,
        ],
    )
    .unwrap();
    assert_eq!(check(&permit, head).unwrap()["verified"], true);
    // Candidate replacement refs cannot reinterpret independently pinned IDs.
    git(repo, &["replace", head, &base]).unwrap();
    assert_eq!(
        check(&permit, head).unwrap()["source"]["tree"],
        accepted_tree
    );
    let mut replaced_tree = permit.clone();
    replaced_tree["source"]["tree"] = base_tree.clone().into();
    assert!(check(&replaced_tree, head).is_err());
    git(repo, &["replace", "-d", head]).unwrap();
    let graft = repo.join(git(repo, &["rev-parse", "--git-path", "info/grafts"]).unwrap());
    fs::create_dir_all(graft.parent().unwrap()).unwrap();
    fs::write(&graft, format!("{head} {base}\n")).unwrap();
    assert!(check(&permit, head).is_err());
    fs::remove_file(graft).unwrap();
    assert!(check(&permit, &base).is_err());
    let original: serde_json::Value =
        serde_json::from_str(&git(repo, &["cat-file", "blob", &receipts[0]]).unwrap()).unwrap();
    let reject_receipt = |changed: serde_json::Value| {
        let mut altered = permit.clone();
        altered["receipts"][0] =
            native_provenance::store_blob(repo, &serde_json::to_vec(&changed).unwrap())
                .unwrap()
                .into();
        assert!(check(&altered, head).is_err());
    };
    let mut changed = original.clone();
    changed["source"]["tree"] = base_tree.clone().into();
    reject_receipt(changed);
    let mut changed = original.clone();
    changed["claim"]["scopes"] = serde_json::json!(["outside"]);
    reject_receipt(changed);
    let mut changed = original.clone();
    changed["gates"][0]["exit_code"] = 1.into();
    reject_receipt(changed);
    let mut changed = original.clone();
    changed["gates"][3]["passed"] = 0.into();
    reject_receipt(changed);
    let mut changed = original.clone();
    changed["gates"][0]["log_sha256"] = "0".repeat(64).into();
    reject_receipt(changed);
    let mut changed = original.clone();
    changed["gates"][3]["argv"]
        .as_array_mut()
        .unwrap()
        .extend(["--exclude".into(), "susi-core".into()]);
    changed["gate_plan"][3]["argv"] = changed["gates"][3]["argv"].clone();
    reject_receipt(changed);
    let mut changed = original.clone();
    changed["gate_plan"].as_array_mut().unwrap().remove(1);
    changed["gates"].as_array_mut().unwrap().remove(1);
    reject_receipt(changed);
    let mut duplicate = permit.clone();
    duplicate["receipts"]
        .as_array_mut()
        .unwrap()
        .push(receipts[0].clone().into());
    assert!(check(&duplicate, head).is_err());
    // A same-tree merge can hide an unrelated second parent: ancestry, not
    // final net diff, must still reject its unowned source commit.
    let rogue = git_input(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &base_tree,
            "-p",
            &base,
        ],
        b"unowned source\n",
    )
    .unwrap();
    let hidden = git_input(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &accepted_tree,
            "-p",
            head,
            "-p",
            &rogue,
        ],
        b"same-tree uncovered second parent\n",
    )
    .unwrap();
    let mut hidden_permit = permit.clone();
    hidden_permit["source"]["commit"] = hidden.clone().into();
    for (index, oid) in receipts.iter().enumerate() {
        let mut record: serde_json::Value =
            serde_json::from_str(&git(repo, &["cat-file", "blob", oid]).unwrap()).unwrap();
        record["integration"]["commit"] = hidden.clone().into();
        hidden_permit["receipts"][index] =
            native_provenance::store_blob(repo, &serde_json::to_vec(&record).unwrap())
                .unwrap()
                .into();
    }
    assert!(check(&hidden_permit, &hidden).is_err());
    // A source merge may not claim unrelated pre-reservation history whose
    // out-of-scope edit was subsequently reverted to a clean net diff.
    let outside_blob = native_provenance::store_blob(repo, b"unowned historical edit\n").unwrap();
    let outside_tree = git_input(
        repo,
        &["mktree"],
        format!(
            "{}\n100644 blob {outside_blob}\toutside.txt\n",
            git(repo, &["ls-tree", &base]).unwrap()
        )
        .as_bytes(),
    )
    .unwrap();
    let outside_commit = git_input(
        repo,
        &["-c", "commit.gpgsign=false", "commit-tree", &outside_tree],
        b"unrelated scope bypass\n",
    )
    .unwrap();
    let reverted = git_input(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &base_tree,
            "-p",
            &outside_commit,
        ],
        b"hide unrelated edit\n",
    )
    .unwrap();
    let original_source = original["source"]["commit"].as_str().unwrap();
    let original_tree = original["source"]["tree"].as_str().unwrap();
    let mixed_source = git_input(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            original_tree,
            "-p",
            original_source,
            "-p",
            &reverted,
        ],
        b"source with hidden pre-scope history\n",
    )
    .unwrap();
    let mixed_head = git_input(
        repo,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &accepted_tree,
            "-p",
            head,
            "-p",
            &mixed_source,
        ],
        b"integrated hidden pre-scope history\n",
    )
    .unwrap();
    let mut mixed_permit = permit.clone();
    mixed_permit["source"]["commit"] = mixed_head.clone().into();
    for (index, oid) in receipts.iter().enumerate() {
        let mut record: serde_json::Value =
            serde_json::from_str(&git(repo, &["cat-file", "blob", oid]).unwrap()).unwrap();
        record["integration"]["commit"] = mixed_head.clone().into();
        if index == 0 {
            record["source"]["commit"] = mixed_source.clone().into();
        }
        mixed_permit["receipts"][index] =
            native_provenance::store_blob(repo, &serde_json::to_vec(&record).unwrap())
                .unwrap()
                .into();
    }
    assert!(check(&mixed_permit, &mixed_head).is_err());
    let original_bytes = serde_json::to_vec(&permit).unwrap();
    let original_digest = native_provenance::digest(&original_bytes);
    fs::write(&path, b"forged permit").unwrap();
    assert!(native_provenance::verify(
        repo,
        &path,
        &native_provenance::TrustPins {
            permit_sha256: &original_digest,
            verifier_sha256: &verifier,
            policy_sha256: &policy
        },
        head
    )
    .is_err());
    let candidate_permit = repo.join("self-authorized.json");
    fs::write(&candidate_permit, &original_bytes).unwrap();
    assert!(native_provenance::verify(
        repo,
        &candidate_permit,
        &native_provenance::TrustPins {
            permit_sha256: &original_digest,
            verifier_sha256: &verifier,
            policy_sha256: &policy
        },
        head
    )
    .is_err());
    fs::remove_file(candidate_permit).unwrap();
}

#[test]
fn merge_conflict_preserves_candidate_and_reservation() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    fs::write(repo.join("common.rs"), "original\n").unwrap();
    git(&repo, &["add", "common.rs"]).unwrap();
    git(&repo, &["-c", "core.hooksPath=", "commit", "-qm", "base"]).unwrap();
    worker(&repo, "one", "common.rs", "worker\n");
    fs::write(repo.join("common.rs"), "integrated\n").unwrap();
    git(&repo, &["add", "common.rs"]).unwrap();
    git(
        &repo,
        &["-c", "core.hooksPath=", "commit", "-qm", "other change"],
    )
    .unwrap();
    let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    git(&repo, &["update-ref", INTEGRATION_REF, &head]).unwrap();
    // This fixture models a previously accepted conflicting change. Supply
    // its matching receipt so the test reaches the actual merge conflict.
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let receipt = format!(
        "susi-local-integration-v1\nagent=fixture\nsource={head}\nclaim-base={head}\nprevious=none\ncommit={head}\ntree={tree}\ntest-only conflict fixture => exit code: 0; fixture verified\n"
    );
    let blob = git_input(&repo, &["hash-object", "-w", "--stdin"], receipt.as_bytes()).unwrap();
    git(
        &repo,
        &[
            "update-ref",
            &format!("refs/susi/integration/receipts/fixture/{head}"),
            &blob,
        ],
    )
    .unwrap();
    assert!(integrate_with(&repo, &["one"], |_, _| panic!(
        "conflict reached verification"
    ))
    .is_err());
    assert_eq!(integration_head(&repo).unwrap(), Some(head));
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
    let worktrees = git(&repo, &["worktree", "list", "--porcelain"]).unwrap();
    let candidate = worktrees
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .find(|path| path.ends_with("-verify-candidate"))
        .unwrap();
    assert!(!git(
        Path::new(candidate),
        &["diff", "--name-only", "--diff-filter=U"]
    )
    .unwrap()
    .is_empty());
}

#[test]
fn disk_full_signatures_are_recognized_in_gate_logs() {
    for log in [
        "error: failed to write: No space left on device (os error 28)",
        "LLVM ERROR: IO failure on output stream: No space left on device",
        "There is not enough space on the disk. (os error 112)",
    ] {
        assert!(gate_out_of_space(log.as_bytes()), "{log}");
    }
    for log in [
        "",
        "error[E0308]: mismatched types",
        "test result: FAILED. 1 passed; 1 failed",
        "No such file or directory (os error 2)",
    ] {
        assert!(!gate_out_of_space(log.as_bytes()), "{log}");
    }
}

#[test]
fn disk_floor_follows_the_workspace_the_gate_and_the_warm_target() {
    let susi = Some(RepoKind::Susi);
    let cold = gate_disk_floor(susi, true, false);
    let warm = gate_disk_floor(susi, true, true);
    let narrowed_cold = gate_disk_floor(susi, false, false);
    let narrowed_warm = gate_disk_floor(susi, false, true);
    assert!(cold > warm, "{cold} {warm}");
    assert!(warm > narrowed_cold, "{warm} {narrowed_cold}");
    assert_eq!(narrowed_cold, narrowed_warm);
    assert!(narrowed_warm >= GIB);
    let commercial = Some(RepoKind::Commercial);
    assert!(gate_disk_floor(commercial, true, false) < cold);
    assert!(gate_disk_floor(commercial, true, false) > gate_disk_floor(commercial, true, true));
    assert_eq!(
        gate_disk_floor(commercial, true, true),
        gate_disk_floor(commercial, false, false)
    );
    // A fixture workspace only needs room to compile at all.
    let fixture = gate_disk_floor(None, true, false);
    assert!(
        fixture < gate_disk_floor(commercial, true, true),
        "{fixture}"
    );
    assert!(fixture > 0);
}

#[test]
fn disk_free_space_is_measured_for_existing_and_future_paths() {
    let scratch = Scratch::new();
    assert!(free_space(&scratch.0).is_some_and(|free| free > 0));
    assert!(free_space(&scratch.0.join("not").join("yet")).is_some());
    assert_eq!(gib(3 * GIB / 2), "1.5 GiB");
}

#[test]
fn dropped_locks_release_while_a_descriptor_copy_is_alive() {
    let scratch = Scratch::new();
    let gate = acquire_host_gate(&scratch.0.join("host"), "test").unwrap();
    let gate_copy = gate._file.try_clone().unwrap();
    drop(gate);
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scratch.0.join("host").join("gate.lock"))
        .unwrap();
    assert!(probe.try_lock().is_ok(), "host gate stayed held by a copy");
    release_probe(probe);

    let state = scratch.0.join("state");
    let lock = Lock::acquire(&state).unwrap();
    let lock_copy = lock._file.try_clone().unwrap();
    drop(lock);
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(state.join("lock.file"))
        .unwrap();
    assert!(
        probe.try_lock().is_ok(),
        "workflow lock stayed held by a copy"
    );
    release_probe(probe);
    drop((gate_copy, lock_copy));
}

#[test]
fn disk_reclaim_frees_idle_caches_and_keeps_busy_and_current_ones() {
    let scratch = Scratch::new();
    let workflow = scratch.0.join("wf");
    let slots = workflow.join("slots");
    let seed = |dir: &Path| {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("unit.o"), vec![7u8; 2048]).unwrap();
    };
    seed(&workflow.join("integration-target"));
    seed(&slots.join("2").join("target"));
    seed(&slots.join("3").join("target"));
    seed(&slots.join("owner-notes"));
    let candidate = scratch.0.join("susi-verify-candidate-1");
    seed(&candidate.join("target"));
    seed(&scratch.0.join("susi-verify-candidate-2").join("target"));
    seed(&scratch.0.join("susi-verify-candidate-3").join("target"));
    // Slot 3 is a running ship's: its lock is held.
    let busy = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(slots.join("3.lock"))
        .unwrap();
    busy.try_lock().unwrap();
    let busy = HostGate { _file: busy };
    let freed = reclaim_gate_caches(&workflow, &candidate);
    let mut paths: Vec<_> = freed.iter().map(|(path, _)| path.clone()).collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            scratch.0.join("susi-verify-candidate-2").join("target"),
            workflow.join("integration-target"),
            slots.join("2").join("target"),
        ]
    );
    assert!(freed.iter().all(|(_, size)| *size == 2048), "{freed:?}");
    assert!(
        candidate.join("target").is_dir(),
        "the current candidate keeps its target"
    );
    assert!(
        slots.join("3").join("target").is_dir(),
        "a busy slot keeps its stash"
    );
    assert!(
        scratch
            .0
            .join("susi-verify-candidate-3")
            .join("target")
            .is_dir(),
        "a busy slot keeps its candidate target"
    );
    assert!(
        slots.join("owner-notes").is_dir(),
        "only numbered slots are caches"
    );
    // Once that ship is gone, its caches are idle too; then nothing is left.
    drop(busy);
    let mut released: Vec<_> = reclaim_gate_caches(&workflow, &candidate)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    released.sort();
    assert_eq!(
        released,
        vec![
            scratch.0.join("susi-verify-candidate-3").join("target"),
            slots.join("3").join("target"),
        ]
    );
    assert!(reclaim_gate_caches(&workflow, &candidate).is_empty());
}

struct FakeRemote {
    mains: Vec<String>,
    fetches: usize,
    pushes: Vec<(String, String)>,
    fail_push: bool,
}

impl FakeRemote {
    fn single(main: String) -> Self {
        Self {
            mains: vec![main],
            fetches: 0,
            pushes: Vec::new(),
            fail_push: false,
        }
    }
}

impl Remote for FakeRemote {
    fn fetch_main(&mut self, _root: &Path) -> Result<String> {
        let sha = self
            .mains
            .get(self.fetches)
            .or_else(|| self.mains.last())
            .cloned()
            .ok_or("no origin/main")?;
        self.fetches += 1;
        Ok(sha)
    }
    fn push_ref(&mut self, _root: &Path, sha: &str, git_ref: &str) -> Result<()> {
        require_ship_ref(git_ref)?;
        if self.fail_push {
            return Err("push refused".into());
        }
        self.pushes.push((sha.to_owned(), git_ref.to_owned()));
        Ok(())
    }
}

fn local_gate_evidence() -> Vec<String> {
    vec![
        "cargo fmt --all --check => exit code: 0; elapsed-ms=1".into(),
        "cargo deny --offline --locked check licenses => exit code: 0; elapsed-ms=1".into(),
        "cargo clippy --workspace --all-targets --offline --locked => exit code: 0; elapsed-ms=1"
            .into(),
        "cargo test --workspace --offline --locked => exit code: 0; elapsed-ms=1".into(),
    ]
}

fn ship_fixture(agent: &str) -> (Scratch, PathBuf, String) {
    let scratch = Scratch::new();
    let (repo, base) = scratch.repo_with_base();
    worker(&repo, agent, "owned.rs", "shipped");
    (scratch, repo, base)
}

#[test]
fn ship_publishes_when_local_integration_ref_is_unrelated() {
    let (_scratch, repo, base) = ship_fixture("one");
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated"]).unwrap();
    git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
    let mut remote = FakeRemote::single(base);
    ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert!(!remote.pushes.is_empty());
    assert_eq!(
        git(&repo, &["rev-parse", "refs/susi/integration/displaced"]).unwrap(),
        unrelated
    );
}

/// Regression (2026-11-06): the first ship after displacing an unrelated
/// integration ref wrote `previous=<displaced>`, so its own receipt never
/// verified and every later ship failed at load with "no matching durable
/// source/tree receipt". A second worker must ship on top of the first.
#[test]
fn ship_after_displacing_an_unrelated_ref_keeps_later_ships_working() {
    let (_scratch, repo, base) = ship_fixture("one");
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated"]).unwrap();
    git(&repo, &["update-ref", INTEGRATION_REF, &unrelated]).unwrap();
    let mut remote = FakeRemote::single(base.clone());
    let first = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap();
    // The receipts describe the verified merge; origin/main holds its
    // receipt commit.
    let first_head = first.merge.clone();
    let first_published = first.head.clone();
    let receipt_ref = git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/susi/integration/receipts/one/",
        ],
    )
    .unwrap();
    let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
    assert!(
        receipt.contains(&format!("previous={base}")),
        "a displaced ref must not be recorded as previous: {receipt}"
    );
    assert!(!receipt.contains(&format!("previous={unrelated}")));
    let first_tree = git(&repo, &["rev-parse", &format!("{first_head}^{{tree}}")]).unwrap();
    assert!(receipt_matches(&repo, &receipt_ref, &receipt, &first_head, &first_tree).unwrap());

    // Receipts written before the fix named the displaced ref; they must
    // still verify so repositories already in that state can ship again.
    let legacy = receipt.replace(
        &format!("previous={base}"),
        &format!("previous={unrelated}"),
    );
    assert!(receipt_matches(&repo, &receipt_ref, &legacy, &first_head, &first_tree).unwrap());
    let malformed = receipt.replace(&format!("previous={base}"), "previous=not-an-oid");
    assert!(!receipt_matches(&repo, &receipt_ref, &malformed, &first_head, &first_tree).unwrap());

    worker(&repo, "two", "second.rs", "second");
    let mut remote = FakeRemote::single(first_published.clone());
    let second = ship(&repo, &["two"], &mut remote, |candidate, _attempt| {
        assert!(candidate.join("owned.rs").is_file());
        assert!(candidate.join("second.rs").is_file());
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert!(is_ancestor(&repo, &first_published, &second.merge).unwrap());
    assert_eq!(remote.pushes.last().unwrap().0, second.head);
}

#[test]
fn host_gate_admits_one_holder_and_frees_on_drop() {
    let scratch = Scratch::new();
    let dir = scratch.repo().join("host-gate");
    let first = acquire_host_gate(&dir, "first").unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("owner")).unwrap().trim(),
        "first"
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter_dir = dir.clone();
    let waiter = std::thread::spawn(move || {
        let gate = acquire_host_gate(&waiter_dir, "second").unwrap();
        sender.send(()).unwrap();
        drop(gate);
    });
    // The second holder must still be waiting while the first holds it.
    assert!(receiver
        .recv_timeout(std::time::Duration::from_millis(800))
        .is_err());
    drop(first);
    receiver
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    waiter.join().unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("owner")).unwrap().trim(),
        "second"
    );
}

#[test]
fn a_leftover_host_gate_file_does_not_block() {
    // A crashed holder leaves the lock file and owner behind; only a live
    // OS lock blocks, so the next gate proceeds at once.
    let scratch = Scratch::new();
    let dir = scratch.repo().join("host-gate");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("gate.lock"), "").unwrap();
    fs::write(dir.join("owner"), "pid=1 crashed\n").unwrap();
    let started = std::time::Instant::now();
    drop(acquire_host_gate(&dir, "next").unwrap());
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn narrowed_xtask_gates_keep_release_tooling() {
    let mut gates = vec![
        vec!["fmt".to_owned(), "--all".to_owned()],
        ["clippy", "-p", "xtask", "--", "-D", "warnings"]
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
        ["test", "-p", "xtask", "-p", "susi"]
            .iter()
            .map(|arg| (*arg).to_owned())
            .collect(),
    ];
    let without: std::collections::BTreeSet<String> = ["susi".to_owned()].into();
    let mut untouched = gates.clone();
    include_release_tooling_for_xtask(&mut untouched, &without);
    assert_eq!(untouched, gates);
    let with: std::collections::BTreeSet<String> = ["susi".to_owned(), "xtask".to_owned()].into();
    include_release_tooling_for_xtask(&mut gates, &with);
    include_release_tooling_for_xtask(&mut gates, &with);
    assert_eq!(gates[0], ["fmt", "--all"]);
    assert_eq!(
        gates[1],
        [
            "clippy",
            "-p",
            "xtask",
            "--features",
            "xtask/release-tooling",
            "--",
            "-D",
            "warnings"
        ]
    );
    assert_eq!(
        gates[2],
        [
            "test",
            "-p",
            "xtask",
            "-p",
            "susi",
            "--features",
            "xtask/release-tooling"
        ]
    );
}

#[test]
fn doc_gate_follows_the_test_gate_and_names_only_libraries() {
    let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let whole = strings(&[
        "nextest",
        "run",
        "--workspace",
        "--offline",
        "--locked",
        "--target",
        "h",
        "--target-dir",
        "t",
        "--features",
        "xtask/release-tooling",
    ]);
    assert_eq!(
        doc_gate(&whole, None).unwrap(),
        strings(&[
            "test",
            "--doc",
            "--workspace",
            "--offline",
            "--locked",
            "--target",
            "h",
            "--target-dir",
            "t",
            "--features",
            "xtask/release-tooling",
        ])
    );
    let narrowed = strings(&[
        "nextest",
        "run",
        "-p",
        "susi",
        "-p",
        "xtask",
        "-p",
        "susi-core",
        "--offline",
        "--locked",
        "--target",
        "h",
        "--target-dir",
        "t",
    ]);
    let libraries: BTreeSet<String> = ["susi".to_owned(), "susi-core".to_owned()].into();
    assert_eq!(
        doc_gate(&narrowed, Some(&libraries)).unwrap(),
        strings(&[
            "test",
            "--doc",
            "-p",
            "susi",
            "-p",
            "susi-core",
            "--offline",
            "--locked",
            "--target",
            "h",
            "--target-dir",
            "t",
        ])
    );
    let narrowed_xtask = strings(&[
        "nextest",
        "run",
        "-p",
        "susi",
        "-p",
        "xtask",
        "--offline",
        "--locked",
        "--target",
        "h",
        "--target-dir",
        "t",
        "--features",
        "xtask/release-tooling",
    ]);
    assert_eq!(
        doc_gate(&narrowed_xtask, Some(&libraries)).unwrap(),
        strings(&[
            "test",
            "--doc",
            "-p",
            "susi",
            "--offline",
            "--locked",
            "--target",
            "h",
            "--target-dir",
            "t",
        ])
    );
    let only_bins = strings(&["nextest", "run", "-p", "xtask", "--offline", "--locked"]);
    assert!(doc_gate(&only_bins, Some(&libraries)).is_none());
}

#[test]
fn cargo_test_is_the_default_runner_and_nextest_is_opt_in() {
    let strings = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    let nextest_gate = strings(&["nextest", "run", "--workspace", "--offline", "--locked"]);
    let mut default = nextest_gate.clone();
    select_test_runner(&mut default, false);
    assert_eq!(
        default,
        strings(&["test", "--workspace", "--offline", "--locked"])
    );
    let mut opted_in = nextest_gate.clone();
    select_test_runner(&mut opted_in, true);
    assert_eq!(opted_in, nextest_gate);
    let mut already_test = strings(&["test", "-p", "x"]);
    select_test_runner(&mut already_test, false);
    assert_eq!(already_test, strings(&["test", "-p", "x"]));
}

#[test]
fn ship_does_not_push_when_the_gate_fails() {
    let (_scratch, repo, base) = ship_fixture("one");
    let mut remote = FakeRemote::single(base);
    let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Err("gate failed".into())
    })
    .unwrap_err();
    assert!(error.to_string().contains("gate failed"), "{error}");
    assert!(remote.pushes.is_empty());
}

#[test]
fn ship_pushes_only_main_fast_forward() {
    let (_scratch, repo, base) = ship_fixture("one");
    let mut remote = FakeRemote::single(base);
    ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert!(!remote.pushes.is_empty());
    for (_sha, git_ref) in &remote.pushes {
        assert_eq!(git_ref, "refs/heads/main");
    }
    assert!(require_ship_ref("refs/heads/other").is_err());
}

#[test]
fn ship_publishes_the_receipt_commit_and_nothing_else() {
    let (_scratch, repo, base) = ship_fixture("one");
    let mut remote = FakeRemote::single(base);
    let mut verified = String::new();
    let report = ship(&repo, &["one"], &mut remote, |candidate, _attempt| {
        verified = git(candidate, &["rev-parse", "HEAD"])?;
        Ok(local_gate_evidence())
    })
    .unwrap();
    // The verified merge is published under its receipt commit, and the
    // push of that commit is the only thing the remote receives.
    assert_eq!(report.merge, verified);
    assert_ne!(report.head, verified);
    assert_eq!(
        git(&repo, &["rev-parse", &format!("{}^1", report.head)]).unwrap(),
        verified
    );
    assert_eq!(
        remote.pushes,
        vec![(report.head.clone(), "refs/heads/main".to_owned())]
    );
}

/// Every landing publishes a receipt-only commit carrying
/// `.agents/receipts/<verified head>.json`, recorded at
/// `refs/susi/receipts/<verified head>`; without that record nothing is
/// published (`ship --retry` included).
/// The gate verifies the bytes its receipt names: a host whose Git
/// converts line endings on checkout (`core.autocrlf=true`, the Git for
/// Windows default) still gets the committed LF in its candidate.
#[test]
fn the_gate_candidate_holds_the_committed_bytes_under_autocrlf() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    worker(&repo, "one", "spec.mdx", "one\ntwo\n");
    git(&repo, &["config", "core.autocrlf", "true"]).unwrap();
    let mut remote = FakeRemote::single(base);
    let mut seen = Vec::new();
    ship(&repo, &["one"], &mut remote, |candidate, _attempt| {
        seen = fs::read(candidate.join("spec.mdx"))?;
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert_eq!(seen, b"one\ntwo\n");
}

#[test]
fn ship_batch_receipt_records_the_final_fetched_remote_not_an_intermediate_merge() {
    for moved_during_gate in [false, true] {
        let (_scratch, repo, base) = ship_fixture("one");
        worker(&repo, "two", "second.rs", "second");
        let expected = if moved_during_gate {
            commit_on(&repo, "remote-change.rs", "remote change")
        } else {
            base.clone()
        };
        let mut remote = FakeRemote::single(base.clone());
        if moved_during_gate {
            remote.mains = vec![base, expected.clone(), expected.clone()];
        }
        let runs = std::cell::Cell::new(0);
        let report = ship(
            &repo,
            &["one", "two"],
            &mut remote,
            |_candidate, _attempt| {
                runs.set(runs.get() + 1);
                Ok(local_gate_evidence())
            },
        )
        .unwrap();
        assert_eq!(runs.get(), if moved_during_gate { 2 } else { 1 });
        assert_eq!(remote.pushes.len(), 1);
        let parent = git(&repo, &["rev-parse", &format!("{}^1", report.merge)]).unwrap();
        assert_ne!(
            parent, expected,
            "the second source creates an intermediate merge"
        );
        let receipt: serde_json::Value = serde_json::from_str(
            &git(
                &repo,
                &[
                    "show",
                    &format!("{}:{}", report.head, gate_receipt::path_for(&report.merge)),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(receipt["integration"]["origin_main"], expected);
        assert_eq!(receipt["integration"]["merge_first_parent"], parent);
        assert!(is_ancestor(&repo, &expected, &report.merge).unwrap());
        gate_receipt::check_digest(&receipt).unwrap();
    }
}

#[test]
fn ship_publishes_the_gate_receipt_commit_and_refuses_without_one() {
    let (_scratch, repo, base) = ship_fixture("one");
    let mut remote = FakeRemote::single(base.clone());
    let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap();
    let verified = report.merge.clone();
    let published = report.head.clone();
    assert_eq!(
        git(&repo, &["rev-parse", &gate_receipt::ref_for(&verified)]).unwrap(),
        published
    );
    assert_eq!(
        git(&repo, &["rev-parse", INTEGRATION_REF]).unwrap(),
        verified
    );
    gate_receipt::check_receipt_only(&repo, &verified, &published).unwrap();
    let path = gate_receipt::path_for(&verified);
    let changed = git(
        &repo,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            &verified,
            &published,
        ],
    )
    .unwrap();
    assert_eq!(changed, path);
    let receipt: serde_json::Value =
        serde_json::from_str(&git(&repo, &["show", &format!("{published}:{path}")]).unwrap())
            .unwrap();
    gate_receipt::check_digest(&receipt).unwrap();
    assert_eq!(receipt["integration"]["commit"], verified);
    assert_eq!(receipt["integration"]["origin_main"], base);
    assert_eq!(receipt["sources"][0]["agent"], "one");
    assert_eq!(receipt["gates"].as_array().unwrap().len(), 4);
    assert!(receipt["gates"]
        .as_array()
        .unwrap()
        .iter()
        .all(|gate| gate["exit_code"] == 0));
    let message = git(&repo, &["log", "-1", "--format=%B", &published]).unwrap();
    assert!(message.contains("Task: LOCAL-one"), "{message}");
    // No operator path leaks: the slot, the scratch repository, the home.
    let text = git(&repo, &["show", &format!("{published}:{path}")]).unwrap();
    assert!(
        !text.contains(&repo.display().to_string().replace('\\', "/")),
        "{text}"
    );
    assert!(!text.contains("verify-candidate"), "{text}");
    // A verifier on this clone agrees with the receipt (static checks).
    git(&repo, &["reset", "-q", "--hard", &published]).unwrap();
    let verification = gate_receipt::verify(
        &repo,
        &verified,
        &gate_receipt::Options {
            static_only: true,
            ..gate_receipt::Options::default()
        },
    )
    .unwrap();
    assert!(verification.all_match(), "{}", verification.render());
    // Without the recorded receipt commit, retry publishes nothing.
    git(
        &repo,
        &["update-ref", "-d", &gate_receipt::ref_for(&verified)],
    )
    .unwrap();
    remote.pushes.clear();
    let refused = ship_retry(&repo, &mut remote).unwrap_err();
    assert!(
        refused.to_string().contains("no gate receipt commit"),
        "{refused}"
    );
    assert!(remote.pushes.is_empty());
}

fn commit_on(repo: &Path, file: &str, content: &str) -> String {
    fs::write(repo.join(file), content).unwrap();
    git(repo, &["add", "--", file]).unwrap();
    git(repo, &["-c", "core.hooksPath=", "commit", "-qm", file]).unwrap();
    git(repo, &["rev-parse", "HEAD"]).unwrap()
}

fn bare_claim(repo: &Path, base: &str) -> Claim {
    Claim {
        name: "probe".into(),
        worktree: repo.to_path_buf(),
        base: base.into(),
        scopes: vec!["probe.rs".into()],
    }
}

#[test]
fn scope_base_needs_no_receipt_when_the_claim_is_at_or_after_accepted() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let older = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let base = commit_on(&repo, "base.rs", "base");
    let source = commit_on(&repo, "probe.rs", "probe");
    // Integration ref equal to the claim base, with no receipt.
    git(&repo, &["update-ref", INTEGRATION_REF, &base]).unwrap();
    let claim = bare_claim(&repo, &base);
    assert_eq!(scope_base(&repo, &claim, &source).unwrap(), base);
    // Integration ref strictly behind the claim base, with no receipt.
    git(&repo, &["update-ref", INTEGRATION_REF, &older]).unwrap();
    assert_eq!(scope_base(&repo, &claim, &source).unwrap(), base);
}

#[test]
fn ship_scope_base_falls_back_to_the_claim_base_without_a_receipt() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let accepted = commit_on(&repo, "accepted.rs", "accepted");
    let source = commit_on(&repo, "probe.rs", "probe");
    // Accepted is ahead of the claim base but carries no receipt.
    git(&repo, &["update-ref", INTEGRATION_REF, &accepted]).unwrap();
    let claim = bare_claim(&repo, &base);
    let error = scope_base(&repo, &claim, &source).unwrap_err();
    assert!(error.to_string().contains("no matching durable"), "{error}");
    // The fallback is the claim base, raised to what the source shares
    // with the publish base: `accepted` is already published, so only
    // `probe.rs` on top of it is this worker's change.
    assert_eq!(
        ship_scope_base(&repo, &claim, &source, &accepted).unwrap(),
        accepted
    );
    // With a publish base behind the claim base nothing is raised.
    assert_eq!(
        ship_scope_base(&repo, &claim, &source, &base).unwrap(),
        base
    );
    // A claim base that is not on the publish base still fails.
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let unrelated = git(&repo, &["commit-tree", &tree, "-m", "unrelated"]).unwrap();
    assert!(ship_scope_base(&repo, &claim, &source, &unrelated).is_err());
}

#[test]
fn ship_succeeds_and_chains_when_accepted_is_ahead_without_a_receipt() {
    let (_scratch, repo, _base) = ship_fixture("one");
    // Main advanced past the claim base and the integration ref points at
    // it, but this clone never wrote a receipt for that commit.
    let accepted = commit_on(&repo, "elsewhere.rs", "elsewhere");
    git(&repo, &["update-ref", INTEGRATION_REF, &accepted]).unwrap();
    let mut remote = FakeRemote::single(accepted.clone());
    let first = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert!(is_ancestor(&repo, &accepted, &first.head).unwrap());
    assert_eq!(remote.pushes.last().unwrap().0, first.head);
    // A later worker ships against the receipt the first ship wrote.
    worker(&repo, "two", "second.rs", "second");
    let mut remote = FakeRemote::single(first.head.clone());
    let second = ship(&repo, &["two"], &mut remote, |candidate, _attempt| {
        assert!(candidate.join("owned.rs").is_file());
        assert!(candidate.join("second.rs").is_file());
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert!(is_ancestor(&repo, &first.head, &second.head).unwrap());
}

#[test]
fn workspace_proof_regate_retires_old_keys_for_partial_and_full_passes() {
    for full_regate in [false, true] {
        let (_scratch, repo, base) = ship_fixture("one");
        let moved = commit_on(&repo, "later.rs", "later");
        let mut remote = FakeRemote::single(base.clone());
        remote.mains = vec![base, moved.clone(), moved];
        let mut runs = 0;
        let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
            runs += 1;
            let mut evidence = local_gate_evidence();
            if runs == 1 || full_regate {
                evidence.push(format!("{}proof-{runs}", workspace_test_proof::PREFIX));
                evidence.push(format!("workspace-test-attestation-v1=attestation-{runs}"));
            }
            Ok(evidence)
        })
        .unwrap();
        assert_eq!(runs, 2);
        let tree = git(&repo, &["rev-parse", &format!("{}^{{tree}}", report.merge)]).unwrap();
        let receipt = matching_receipt(&repo, &report.merge, &tree)
            .unwrap()
            .unwrap();
        assert!(receipt.contains("historical workspace proof head="));
        let active: Vec<_> = receipt
            .lines()
            .filter(|line| {
                line.starts_with(workspace_test_proof::PREFIX)
                    || line.starts_with("workspace-test-attestation-v1=")
            })
            .collect();
        if full_regate {
            assert_eq!(
                active,
                [
                    "workspace-test-pass-v1=proof-2",
                    "workspace-test-attestation-v1=attestation-2"
                ]
            );
        } else {
            assert!(active.is_empty(), "{active:?}");
        }
    }
}

#[test]
fn workspace_proof_failed_regate_never_publishes() {
    let (_scratch, repo, base) = ship_fixture("one");
    let moved = commit_on(&repo, "later.rs", "later");
    let mut remote = FakeRemote::single(base.clone());
    remote.mains = vec![base, moved];
    let mut runs = 0;
    let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        runs += 1;
        if runs == 2 {
            return Err("failed re-gate".into());
        }
        let mut evidence = local_gate_evidence();
        evidence.push(format!("{}old", workspace_test_proof::PREFIX));
        Ok(evidence)
    })
    .unwrap_err();
    assert!(error.to_string().contains("failed re-gate"));
    assert!(remote.pushes.is_empty());
    assert!(integration_head(&repo).unwrap().is_none());
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
}

#[test]
fn workspace_proof_retirement_preserves_same_head_and_is_idempotent() {
    let original = vec![
        format!("{}key", workspace_test_proof::PREFIX),
        "cargo test => passed".into(),
    ];
    let mut evidence = original.clone();
    retire_workspace_proofs(&mut evidence, "same", "same");
    assert_eq!(evidence, original);
    retire_workspace_proofs(&mut evidence, "same", "new");
    let retired = evidence.clone();
    retire_workspace_proofs(&mut evidence, "new", "newer");
    assert_eq!(evidence, retired);
    assert_eq!(evidence[1], original[1]);
}
#[test]
fn ship_reverifies_once_when_main_moves() {
    let (_scratch, repo, base) = ship_fixture("one");
    let moved = commit_on(&repo, "later.rs", "later");
    let mut remote = FakeRemote::single(base.clone());
    remote.mains = vec![base, moved.clone(), moved];
    let runs = std::cell::Cell::new(0);
    let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        runs.set(runs.get() + 1);
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert_eq!(runs.get(), 2);
    assert_eq!(remote.pushes.len(), 1);
    assert_eq!(remote.pushes[0].0, report.head);
}

#[test]
fn ship_regates_each_time_main_moves_and_publishes_on_the_latest() {
    let (_scratch, repo, base) = ship_fixture("one");
    let moved = commit_on(&repo, "later.rs", "later");
    let third = commit_on(&repo, "third.rs", "third");
    let mut remote = FakeRemote::single(base.clone());
    remote.mains = vec![base, moved, third.clone()];
    let runs = std::cell::Cell::new(0);
    let report = ship(&repo, &["one"], &mut remote, |candidate, _attempt| {
        runs.set(runs.get() + 1);
        assert!(candidate.join("owned.rs").is_file());
        Ok(local_gate_evidence())
    })
    .unwrap();
    // No Cargo workspace in the fixture, so no selection: every re-gate runs.
    assert_eq!(runs.get(), 3);
    assert_eq!(remote.pushes.len(), 1);
    assert_eq!(remote.pushes[0].0, report.head);
    assert_eq!(
        git(&repo, &["rev-parse", &format!("{}^1", report.merge)]).unwrap(),
        third
    );
    assert_eq!(
        git(&repo, &["rev-parse", &format!("{}^1", report.head)]).unwrap(),
        report.merge
    );
    let tree = git(&repo, &["ls-tree", "--name-only", &report.head]).unwrap();
    for file in ["owned.rs", "later.rs", "third.rs"] {
        assert!(tree.lines().any(|line| line == file), "{file}: {tree}");
    }
    let receipt_ref = git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/susi/integration/receipts/one/",
        ],
    )
    .unwrap();
    let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
    assert_eq!(receipt.matches("\nregate base=").count(), 2, "{receipt}");
    assert!(
        receipt.contains(&format!("regate base={third} ")),
        "{receipt}"
    );
    assert_eq!(integration_head(&repo).unwrap(), Some(report.merge));
}

#[test]
fn ship_gives_up_without_publishing_when_main_moves_every_round() {
    let (_scratch, repo, base) = ship_fixture("one");
    let mut mains = vec![base];
    for round in 0..=SHIP_ROUNDS {
        mains.push(commit_on(
            &repo,
            &format!("later{round}.rs"),
            &format!("later {round}"),
        ));
    }
    let mut remote = FakeRemote::single(mains[0].clone());
    remote.mains = mains;
    let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("origin/main moved during each of"),
        "{error}"
    );
    assert!(remote.pushes.is_empty());
    assert!(integration_head(&repo).unwrap().is_none());
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 1);
}

#[test]
fn ship_refuses_a_rewritten_main() {
    let (_scratch, repo, base) = ship_fixture("one");
    let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]).unwrap();
    let unrelated = git(&repo, &["commit-tree", &tree, "-m", "rewritten"]).unwrap();
    let mut remote = FakeRemote::single(base.clone());
    remote.mains = vec![base, unrelated];
    let error = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap_err();
    assert!(error.to_string().contains("does not contain it"), "{error}");
    assert!(remote.pushes.is_empty());
}

/// One origin shared by concurrent ships: a push must fast-forward it.
#[derive(Clone)]
struct SharedRemote {
    main: std::sync::Arc<std::sync::Mutex<String>>,
    pushes: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Remote for SharedRemote {
    fn fetch_main(&mut self, _root: &Path) -> Result<String> {
        Ok(self.main.lock().unwrap().clone())
    }
    fn push_ref(&mut self, root: &Path, sha: &str, git_ref: &str) -> Result<()> {
        require_ship_ref(git_ref)?;
        let mut main = self.main.lock().unwrap();
        if !is_ancestor(root, &main, sha)? {
            return Err(format!("non-fast-forward push of {sha} over {main}").into());
        }
        sha.clone_into(&mut main);
        self.pushes.lock().unwrap().push(sha.to_owned());
        Ok(())
    }
}

/// The CEO's "no waiting" requirement: two ships' gates are inside
/// verification at the same moment (each gate blocks until the other has
/// started, so serialized gates fail here instead of passing slowly), in
/// two different candidate worktrees, and both land on one origin/main by
/// fast-forward with an intact receipt chain.
#[test]
fn concurrent_ships_gate_at_the_same_time_and_both_land() {
    use std::sync::{Arc, Condvar, Mutex};
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    worker(&repo, "one", "one.rs", "one");
    worker(&repo, "two", "two.rs", "two");
    let remote = SharedRemote {
        main: Arc::new(Mutex::new(base.clone())),
        pushes: Arc::new(Mutex::new(Vec::new())),
    };
    let inside = Arc::new((Mutex::new(0usize), Condvar::new()));
    let candidates = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for agent in ["one", "two"] {
        let repo = repo.clone();
        let mut remote = remote.clone();
        let inside = Arc::clone(&inside);
        let candidates = Arc::clone(&candidates);
        handles.push(std::thread::spawn(move || {
            ship(&repo, &[agent], &mut remote, |candidate, _attempt| {
                candidates.lock().unwrap().push(candidate.to_path_buf());
                let (count, entered) = &*inside;
                let mut count = count.lock().unwrap();
                *count += 1;
                entered.notify_all();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
                while *count < 2 {
                    let left = deadline.saturating_duration_since(std::time::Instant::now());
                    if left.is_zero() {
                        return Err("the other ship never gated concurrently".into());
                    }
                    count = entered.wait_timeout(count, left).unwrap().0;
                }
                Ok(local_gate_evidence())
            })
            .map(|report| report.head)
            .map_err(|error| error.to_string())
        }));
    }
    let heads: Vec<String> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap())
        .collect();
    let pushes = remote.pushes.lock().unwrap().clone();
    assert_eq!(pushes.len(), 2, "{pushes:?}");
    let last = remote.main.lock().unwrap().clone();
    assert_eq!(&last, pushes.last().unwrap());
    assert!(heads.contains(&last));
    assert!(is_ancestor(&repo, &pushes[0], &pushes[1]).unwrap());
    let tree = git(&repo, &["ls-tree", "--name-only", &last]).unwrap();
    assert!(tree.lines().any(|line| line == "one.rs"), "{tree}");
    assert!(tree.lines().any(|line| line == "two.rs"), "{tree}");
    let candidates = candidates.lock().unwrap();
    let first_two: BTreeSet<_> = candidates.iter().take(2).collect();
    assert_eq!(
        first_two.len(),
        2,
        "both gates used one candidate: {candidates:?}"
    );
    // origin/main holds the last receipt commit; the integration ref and
    // the receipts name the verified merge under it.
    let verified_last = git(&repo, &["rev-parse", &format!("{last}^1")]).unwrap();
    assert_eq!(
        integration_head(&repo).unwrap(),
        Some(verified_last.clone())
    );
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    let tree_oid = git(&repo, &["rev-parse", &format!("{verified_last}^{{tree}}")]).unwrap();
    let receipt = matching_receipt(&repo, &verified_last, &tree_oid)
        .unwrap()
        .unwrap();
    assert!(receipt.contains("\nregate base="), "{receipt}");
}

fn cargo_workspace_fixture(repo: &Path) {
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname = \"fixture-root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\nmembers = [\"alpha\", \"beta\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(repo.join("src/lib.rs"), "").unwrap();
    for member in ["alpha", "beta"] {
        fs::create_dir_all(repo.join(member).join("src")).unwrap();
        fs::write(
            repo.join(member).join("Cargo.toml"),
            format!("[package]\nname = \"{member}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        )
        .unwrap();
        fs::write(repo.join(member).join("src/lib.rs"), "").unwrap();
    }
    let status = Command::new("cargo")
        .current_dir(repo)
        .args(["generate-lockfile", "--offline"])
        .status()
        .unwrap();
    assert!(status.success());
    git(
        repo,
        &["add", "Cargo.toml", "Cargo.lock", "src", "alpha", "beta"],
    )
    .unwrap();
    git(
        repo,
        &["-c", "core.hooksPath=", "commit", "-qm", "workspace"],
    )
    .unwrap();
}

#[test]
fn regate_covers_only_what_the_newly_merged_commits_affect() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    cargo_workspace_fixture(&repo);
    let tested = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    fs::create_dir_all(repo.join("docs")).unwrap();
    commit_on(&repo, "docs/notes.md", "notes");
    // Docs: only the root package's tree-wide tests can observe them.
    let (needed, covers) = regate_needed(&repo, &tested);
    assert!(needed, "{covers}");
    assert!(
        covers.ends_with("select 1 packages: fixture-root"),
        "{covers}"
    );
    let docs = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    commit_on(&repo, "alpha/src/lib.rs", "pub fn alpha() -> u8 { 1 }\n");
    let (needed, covers) = regate_needed(&repo, &docs);
    assert!(needed, "{covers}");
    assert!(
        covers.ends_with("select 2 packages: alpha, fixture-root"),
        "{covers}"
    );
    // Nothing changed in the tree: the gates already run stand.
    let (needed, covers) = regate_needed(&repo, "HEAD");
    assert!(!needed, "{covers}");
}

#[test]
fn ship_skips_the_regate_when_the_merged_tree_is_unchanged() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    cargo_workspace_fixture(&repo);
    let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    worker(&repo, "one", "alpha/src/one.rs", "pub fn one() {}\n");
    // origin/main gains a commit that changes no file.
    git(
        &repo,
        &[
            "-c",
            "core.hooksPath=",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "empty",
        ],
    )
    .unwrap();
    let moved = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let mut remote = FakeRemote::single(base.clone());
    remote.mains = vec![base, moved.clone(), moved.clone()];
    let runs = std::cell::Cell::new(0);
    let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        runs.set(runs.get() + 1);
        let mut evidence = local_gate_evidence();
        evidence.push(format!("{}old", workspace_test_proof::PREFIX));
        evidence.push("workspace-test-attestation-v1=old".into());
        Ok(evidence)
    })
    .unwrap();
    assert_eq!(runs.get(), 1);
    assert_eq!(remote.pushes.len(), 1);
    assert_eq!(
        git(&repo, &["rev-parse", &format!("{}^1", report.merge)]).unwrap(),
        moved
    );
    let tree = git(&repo, &["rev-parse", &format!("{}^{{tree}}", report.merge)]).unwrap();
    let receipt = matching_receipt(&repo, &report.merge, &tree)
        .unwrap()
        .unwrap();
    assert!(!receipt
        .lines()
        .any(|line| line.starts_with(workspace_test_proof::PREFIX)
            || line.starts_with("workspace-test-attestation-v1=")));
    assert!(receipt.contains("historical workspace proof head="));
    assert!(
        receipt.contains(" skipped: the merged tree equals the tested tree"),
        "{receipt}"
    );
}

/// Regression (2026-11-06, commercial ships): the local integration ref
/// was behind origin/main (main advanced through ships made elsewhere),
/// and the receipt named that stale ref as `previous` although it is older
/// than the claim base; every later ship failed at load. New receipts
/// record the publish base; old ones verify by the published-lineage rule.
#[test]
fn a_stale_integration_ref_behind_main_keeps_receipts_verifiable() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    // A ship from this clone: the integration ref and its receipt.
    worker(&repo, "zero", "zero.rs", "zero");
    let mut remote = FakeRemote::single(base);
    let stale = ship(&repo, &["zero"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap()
    .merge;
    // origin/main then advanced through a ship made elsewhere, and the
    // next claim is reserved on top of it.
    git(
        &repo,
        &["-c", "core.hooksPath=", "merge", "--ff-only", "-q", &stale],
    )
    .unwrap();
    let advanced = commit_on(&repo, "elsewhere.rs", "elsewhere");
    assert_eq!(integration_head(&repo).unwrap(), Some(stale.clone()));
    worker(&repo, "one", "owned.rs", "owned");
    let mut remote = FakeRemote::single(advanced.clone());
    let report = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap();
    let head = report.merge;
    let tree = git(&repo, &["rev-parse", &format!("{head}^{{tree}}")]).unwrap();
    let receipt_ref = git(
        &repo,
        &[
            "for-each-ref",
            "--format=%(refname)",
            "refs/susi/integration/receipts/one/",
        ],
    )
    .unwrap();
    let receipt = git(&repo, &["cat-file", "blob", &receipt_ref]).unwrap();
    assert!(
        receipt.contains(&format!("previous={advanced}")),
        "{receipt}"
    );
    assert!(receipt_matches(&repo, &receipt_ref, &receipt, &head, &tree).unwrap());
    let legacy = receipt.replace(
        &format!("previous={advanced}"),
        &format!("previous={stale}"),
    );
    assert!(receipt_matches(&repo, &receipt_ref, &legacy, &head, &tree).unwrap());
    // A later ship loads against that receipt.
    worker(&repo, "two", "second.rs", "second");
    let mut remote = FakeRemote::single(head.clone());
    let second = ship(&repo, &["two"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert!(is_ancestor(&repo, &head, &second.head).unwrap());
}

#[test]
fn gate_share_divides_the_host_instead_of_queueing() {
    assert_eq!(
        gate_share(28, 1),
        GateShare {
            live: 1,
            jobs: 28,
            test_threads: 28,
            nextest_threads: 16
        }
    );
    assert_eq!(
        gate_share(28, 2),
        GateShare {
            live: 2,
            jobs: 14,
            test_threads: 14,
            nextest_threads: 8
        }
    );
    assert_eq!(
        gate_share(28, 3),
        GateShare {
            live: 3,
            jobs: 9,
            test_threads: 9,
            nextest_threads: 5
        }
    );
    assert_eq!(gate_share(28, 40).jobs, 2);
    assert_eq!(gate_share(28, 40).nextest_threads, 2);
    assert_eq!(gate_share(28, 0), gate_share(28, 1));
}

#[test]
fn commercial_process_heavy_share_caps_only_windows_test_fanout() {
    let base = gate_share(28, 1);
    for args in [
        vec!["test", "--workspace", "--locked"],
        vec!["test", "-p", "susi-dev-workflow", "--locked"],
        vec!["test", "--package", "susi-ecosystem-hub"],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let bounded = commercial_command_share(base, &args, true);
        assert_eq!(bounded.test_threads, 8);
        assert_eq!(bounded.jobs, base.jobs);
        assert_eq!(bounded.nextest_threads, base.nextest_threads);
        assert_eq!(bounded.live, base.live);
        assert_eq!(commercial_command_share(base, &args, false), base);
        let smaller = gate_share(4, 1);
        assert_eq!(commercial_command_share(smaller, &args, true), smaller);
        let mut command = Command::new("cargo");
        apply_gate_share(&mut command, bounded);
        for (key, expected) in [("CARGO_BUILD_JOBS", "28"), ("RUST_TEST_THREADS", "8")] {
            assert!(command.get_envs().any(|(name, value)| {
                name == key && value == Some(std::ffi::OsStr::new(expected))
            }));
        }
    }
    for args in [
        vec!["clippy", "--workspace"],
        vec!["test", "-p", "susi-reflex-modules"],
        vec!["test", "--package"],
        vec![],
    ] {
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        assert_eq!(commercial_command_share(base, &args, true), base);
    }
}

#[test]
fn integration_gate_disables_registry_environment() {
    let mut command = Command::new("unused");
    apply_gate_env(
        &mut command,
        &GateEnv {
            home: Path::new("private/home"),
            cargo_home: Path::new("toolchain/cargo"),
            rustup_home: Path::new("toolchain/rustup"),
            target: Path::new("private/target"),
            sysroot: None,
        },
    )
    .unwrap();
    assert!(command
        .get_envs()
        .any(|(key, value)| key == "SUSI_NO_REGISTRY_ENV"
            && value == Some(std::ffi::OsStr::new("1"))));
}
/// Regression (2026-11-06): `check` isolated HOME/USERPROFILE but not the
/// toolchain homes, so tests that run `cargo tree --offline` found an
/// empty cargo home and failed only under `check`.
#[test]
fn check_commands_keep_the_real_toolchain_homes_under_an_isolated_home() {
    let top = Path::new("worktree");
    let home = Path::new("worktree/target/susi-check-home");
    let command = check_command(
        top,
        &["test".to_owned()],
        Path::new("worktree/target"),
        home,
        (Path::new("real/.cargo"), Path::new("real/.rustup")),
    );
    let envs: std::collections::BTreeMap<String, Option<String>> = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();
    let get = |key: &str| envs.get(key).cloned().flatten();
    assert_eq!(get("SUSI_NO_REGISTRY_ENV").as_deref(), Some("1"));
    assert_eq!(get("CARGO_HOME").as_deref(), Some("real/.cargo"));
    assert_eq!(get("RUSTUP_HOME").as_deref(), Some("real/.rustup"));
    assert_eq!(get("CARGO_TARGET_DIR").as_deref(), Some("worktree/target"));
    for key in ["HOME", "USERPROFILE"] {
        assert_eq!(
            get(key).as_deref(),
            Some("worktree/target/susi-check-home"),
            "{key}"
        );
    }
}

/// A process that stays alive until killed: `git cat-file --batch` waits
/// on its open stdin.
fn live_child() -> std::process::Child {
    Command::new("git")
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

#[test]
fn locks_mark_their_owner_as_a_slots_launcher() {
    let scratch = Scratch::new();
    let state = scratch.0.join("state");
    let lock = Lock::acquire(&state).unwrap();
    let owner = fs::read_to_string(state.join("lock").join("owner")).unwrap();
    assert!(owner.lines().any(|line| line == SLOTS_LAUNCHER), "{owner}");
    drop(lock);
}

#[test]
fn legacy_owner_metadata_rejects_malformed_and_self_identity() {
    let scratch = Scratch::new();
    let dir = scratch.0.join("owner-probe");
    fs::create_dir_all(&dir).unwrap();
    assert!(!legacy_owner_alive(&dir));
    for record in [
        "pid=not-a-number\n".to_owned(),
        format!("pid={}\n", std::process::id()),
    ] {
        fs::write(dir.join("owner"), record).unwrap();
        assert!(!legacy_owner_alive(&dir));
    }
}
/// Transition (2026-11-07): a launcher from before per-ship slots holds
/// `integration` for its whole gate and its `integrate` moves the accepted
/// ref without `publish`; a slot ship's publication waits for it, but
/// never for a slots launcher's `integration` holder.
#[test]
fn legacy_integration_owner_is_only_a_live_pre_slots_launcher() {
    let scratch = Scratch::new();
    let workflow = scratch.0.join("workflow");
    let dir = workflow.join("integration").join("lock");
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(legacy_integration_owner(&workflow), None);
    let mut child = live_child();
    let pid = child.id();
    fs::write(dir.join("owner"), format!("pid={pid}\n")).unwrap();
    assert_eq!(legacy_integration_owner(&workflow), Some(pid));
    fs::write(dir.join("owner"), format!("pid={pid}\n{SLOTS_LAUNCHER}\n")).unwrap();
    assert_eq!(legacy_integration_owner(&workflow), None);
    fs::write(dir.join("owner"), format!("pid={pid}\n")).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(legacy_integration_owner(&workflow), None);
}

#[test]
fn ship_publishes_only_after_a_legacy_integration_holder_exits() {
    use std::sync::{Arc, Mutex};
    let (_scratch, repo, base) = ship_fixture("one");
    let dir = state(&repo).unwrap().join("integration").join("lock");
    fs::create_dir_all(&dir).unwrap();
    let mut child = live_child();
    fs::write(dir.join("owner"), format!("pid={}\n", child.id())).unwrap();
    let remote = SharedRemote {
        main: Arc::new(Mutex::new(base)),
        pushes: Arc::new(Mutex::new(Vec::new())),
    };
    let mut shipping = remote.clone();
    let (gated, gate_done) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        ship(&repo, &["one"], &mut shipping, |_candidate, _attempt| {
            let _ = gated.send(());
            Ok(local_gate_evidence())
        })
        .map(|report| report.head)
        .map_err(|error| error.to_string())
    });
    gate_done
        .recv_timeout(std::time::Duration::from_secs(120))
        .unwrap();
    // The gate has passed; publication must still be waiting.
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert!(remote.pushes.lock().unwrap().is_empty());
    child.kill().unwrap();
    child.wait().unwrap();
    let head = handle.join().unwrap().unwrap();
    assert_eq!(remote.pushes.lock().unwrap().as_slice(), [head]);
}

#[test]
fn live_gates_count_running_markers_and_drop_dead_ones() {
    let scratch = Scratch::new();
    let dir = scratch.0.join("live-gates");
    assert_eq!(live_gates(&dir), 1);
    let first = register_live_gate(&dir).unwrap();
    let second = register_live_gate(&dir).unwrap();
    assert_eq!(live_gates(&dir), 2);
    // A crashed gate leaves an unlocked marker behind.
    let dead = dir.join("4242-1-0.gate");
    fs::write(&dead, "").unwrap();
    assert_eq!(live_gates(&dir), 2);
    assert!(!dead.exists());
    drop(first);
    assert_eq!(live_gates(&dir), 1);
    drop(second);
    assert_eq!(live_gates(&dir), 1);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
}

#[test]
fn slots_are_private_never_waited_for_and_reused() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let first = acquire_slot(&repo).unwrap();
    let second = acquire_slot(&repo).unwrap();
    assert_ne!(first.candidate, second.candidate);
    assert_ne!(first.attempt, second.attempt);
    for slot in [&first, &second] {
        let name = slot.candidate.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("repo-verify-candidate-"), "{name}");
    }
    let freed = first.candidate.clone();
    drop(first);
    assert_eq!(acquire_slot(&repo).unwrap().candidate, freed);
    let workflow = state(&repo).unwrap();
    assert_eq!(
        warm_targets(&workflow, &second.candidate),
        vec![
            workflow.join("slots").join("2").join("target"),
            workflow.join("integration-target"),
        ]
    );
    assert_eq!(
        warm_targets(&workflow, &candidate_path(&repo).unwrap()),
        vec![workflow.join("integration-target")]
    );
}

/// Regression (2026-11-06, dsp4c1-1106): a leftover candidate that could
/// not be removed made the next ship fail at `worktree add` with a
/// misleading error, after the removal error had been ignored. A slot
/// whose leftover cannot be cleared is now skipped (no waiting), and the
/// removal error itself is what a direct merge reports.
#[test]
fn a_slot_whose_leftover_candidate_cannot_be_removed_is_skipped() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let mut blocked = candidate_path(&repo).unwrap().into_os_string();
    blocked.push("-1");
    let blocked = PathBuf::from(blocked);
    // A path that directory removal cannot clear stands in for a target
    // whose files another process still holds open.
    fs::write(&blocked, "held").unwrap();
    let error = clear_candidate(&repo, &blocked).unwrap_err().to_string();
    assert!(
        error.contains("cannot remove the previous candidate"),
        "{error}"
    );
    let slot = acquire_slot(&repo).unwrap();
    assert!(
        slot.candidate
            .to_string_lossy()
            .ends_with("-verify-candidate-2"),
        "{}",
        slot.candidate.display()
    );
    assert!(blocked.is_file());
}

#[test]
fn a_slot_adopts_the_legacy_warm_target_once_and_keeps_its_own() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let workflow = state(&repo).unwrap();
    let legacy = workflow.join("integration-target");
    fs::create_dir_all(legacy.join("debug")).unwrap();
    let slot = acquire_slot(&repo).unwrap();
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            text(&slot.candidate).unwrap(),
            "HEAD",
        ],
    )
    .unwrap();
    let target = restore_candidate_target(&slot.candidate);
    assert!(target.join("debug").is_dir());
    assert!(!legacy.exists());
    stash_candidate_target(&repo, &slot.candidate);
    assert!(workflow.join("slots/1/target/debug").is_dir());
    assert!(!legacy.exists());
}

#[test]
fn ship_retry_requires_a_matching_receipt() {
    let (_scratch, repo, base) = ship_fixture("one");
    let mut remote = FakeRemote::single(base.clone());
    let missing = ship_retry(&repo, &mut remote).unwrap_err();
    assert!(missing.to_string().contains("missing receipt"), "{missing}");
    assert!(remote.pushes.is_empty());
    remote.fail_push = true;
    let pushed = ship(&repo, &["one"], &mut remote, |_candidate, _attempt| {
        Ok(local_gate_evidence())
    })
    .unwrap_err();
    assert!(pushed.to_string().contains("push refused"), "{pushed}");
    remote.fail_push = false;
    remote.pushes.clear();
    let report = ship_retry(&repo, &mut remote).unwrap();
    assert_eq!(remote.pushes.len(), 1);
    assert_eq!(remote.pushes[0].0, report.head);
    assert_eq!(remote.pushes[0].1, "refs/heads/main");
}

#[test]
fn susi_checkout_is_susi_kind() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask parent");
    assert_eq!(repo_kind(root).unwrap(), RepoKind::Susi);
}

#[test]
fn commercial_package_is_commercial_kind() {
    let scratch = Scratch::new();
    fs::write(
        scratch.0.join("Cargo.toml"),
        "[package]\nname = \"susi-commercial\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    assert_eq!(repo_kind(&scratch.0).unwrap(), RepoKind::Commercial);
}

#[test]
fn unrecognized_checkout_is_refused() {
    let scratch = Scratch::new();
    let error = repo_kind(&scratch.0).unwrap_err();
    assert!(
        error.to_string().contains("unrecognized checkout"),
        "{error}"
    );
}

#[test]
fn commercial_gate_plan_does_not_call_susi_proofs() {
    let plan = commercial_gate_argv();
    let flat = plan
        .iter()
        .flat_map(|args| args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ");
    for needle in ["fmt", "clippy", "test", "deny"] {
        assert!(flat.contains(needle), "{flat}");
    }
    assert!(!flat.contains("trusted_sysroot"), "{flat}");
    assert!(!flat.contains("http://"), "{flat}");
    assert!(!flat.contains("https://"), "{flat}");
}

#[test]
fn usage_lists_ship_commands() {
    let error = run(&["nope".into()]).unwrap_err();
    let text = error.to_string();
    for needle in ["ship <name>", "ship --retry", "status [--json]"] {
        assert!(text.contains(needle), "{text}");
    }
    // GitHub is storage only: no command administers the host.
    for retired in ["ruleset", "workflows"] {
        assert!(!text.contains(retired), "{text}");
    }
    assert!(run(&["status".into(), "--yaml".into()]).is_err());
}

#[test]
fn start_reuses_a_clean_released_worktree_and_keeps_its_target() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let scopes = vec!["owned.rs".to_owned()];
    let worktree = start(&repo, "again", &scopes).unwrap();
    let sentinel = worktree.join("target").join("sentinel.txt");
    fs::write(&sentinel, "warm").unwrap();
    release(&repo, "again").unwrap();
    // main moves on while the worktree sits released.
    let moved = commit_on(&repo, "later.rs", "later");

    let reused = start(&repo, "again", &scopes).unwrap();
    assert_eq!(reused, worktree);
    assert_eq!(fs::read_to_string(&sentinel).unwrap(), "warm");
    assert_eq!(
        git(&reused, &["branch", "--show-current"]).unwrap(),
        "local-agent/again"
    );
    assert_eq!(git(&reused, &["rev-parse", "HEAD"]).unwrap(), moved);
    let claim = read_claim(&state(&repo).unwrap().join("claims").join("again.claim")).unwrap();
    assert_eq!(claim.base, moved);
    assert_eq!(claim.scopes, scopes);

    // Dirty: an untracked file blocks reuse and leaves no claim behind.
    release(&repo, "again").unwrap();
    fs::write(reused.join("stray.rs"), "stray").unwrap();
    let error = start(&repo, "again", &scopes).unwrap_err();
    assert!(error.to_string().contains("refusing to reuse"), "{error}");
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    fs::remove_file(reused.join("stray.rs")).unwrap();

    // Committed but unshipped work on the agent branch is never reset.
    fs::write(reused.join("owned.rs"), "unshipped").unwrap();
    git(&reused, &["add", "owned.rs"]).unwrap();
    git(
        &reused,
        &["-c", "core.hooksPath=", "commit", "-qm", "unshipped"],
    )
    .unwrap();
    let error = start(&repo, "again", &scopes).unwrap_err();
    assert!(error.to_string().contains("holds commits"), "{error}");
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
    assert!(sentinel.is_file());
}

#[test]
fn start_refuses_an_unregistered_destination() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let ghost = repo.parent().unwrap().join("repo-agent-ghost");
    fs::create_dir(&ghost).unwrap();
    let error = start(&repo, "ghost", &["owned.rs".to_owned()]).unwrap_err();
    assert!(error.to_string().contains("refusing to reuse"), "{error}");
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
}

#[test]
fn status_reports_claims_slots_and_integration_without_the_lock() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let empty = workflow_status(&repo).unwrap();
    assert!(empty.claims.is_empty() && empty.busy_slots.is_empty());
    assert_eq!(empty.integration_head, None);
    assert!(status_text(&empty).contains("integration: none"));

    start(&repo, "alpha", &["alpha.rs".to_owned()]).unwrap();
    let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    git(&repo, &["update-ref", INTEGRATION_REF, &head]).unwrap();
    let slot = acquire_slot(&repo).unwrap();
    // Holding the workflow lock proves status never asks for it: a
    // second acquire on this thread fails instead of waiting.
    let _lock = Lock::acquire(&state(&repo).unwrap()).unwrap();
    let status = workflow_status(&repo).unwrap();
    assert_eq!(status.integration_head.as_deref(), Some(head.as_str()));
    assert_eq!(status.claims.len(), 1);
    assert_eq!(status.busy_slots.len(), 1);
    assert_eq!(status.busy_slots[0].slot, 1);
    assert!(status.busy_slots[0].owner.starts_with("pid="));

    let text = status_text(&status);
    for needle in [
        format!("integration: {head}"),
        "claims: 1".to_owned(),
        "alpha".to_owned(),
        "alpha.rs".to_owned(),
        "verifying: 1".to_owned(),
        "slot 1".to_owned(),
    ] {
        assert!(text.contains(&needle), "{text}");
    }

    let json = status_json(&status);
    assert_eq!(json["schema"], "susi-local-workflow-status-v1");
    assert_eq!(json["integration_head"], head.as_str());
    assert_eq!(json["claims"][0]["name"], "alpha");
    assert_eq!(json["claims"][0]["scopes"][0], "alpha.rs");
    assert_eq!(json["busy_slots"][0]["slot"], 1);

    drop(slot);
    assert!(workflow_status(&repo).unwrap().busy_slots.is_empty());
}

#[test]
fn ten_parallel_agents_start_commit_extend_and_ship_without_conflict() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(10));
    let mut handles = Vec::new();
    for index in 0..10 {
        let repo = repo.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let agent = format!("agent{index:02}");
            let scope_a = format!("crates/mod{index:02}/a.rs");
            let scope_b = format!("crates/mod{index:02}/b.rs");
            let scope_c = format!("crates/mod{index:02}/c.rs");
            barrier.wait();
            let worktree = start(&repo, &agent, &[scope_a.clone(), scope_c.clone()]).unwrap();
            extend(&worktree, &agent, std::slice::from_ref(&scope_b)).unwrap();
            update_scopes(
                &worktree,
                &agent,
                std::slice::from_ref(&scope_c),
                ScopeChange::Shrink,
            )
            .unwrap();
            fs::create_dir_all(worktree.join(format!("crates/mod{index:02}"))).unwrap();
            fs::write(worktree.join(&scope_a), format!("pub fn a{index}() {{}}\n")).unwrap();
            fs::write(worktree.join(&scope_b), format!("pub fn b{index}() {{}}\n")).unwrap();
            commit(
                &worktree,
                &agent,
                &format!("feat(mod{index:02}): parallel agent {index}\n\nTask: LOCAL-{agent}"),
                None,
            )
            .unwrap();
            agent
        }));
    }
    let mut agents = Vec::new();
    for handle in handles {
        agents.push(handle.join().unwrap());
    }
    agents.sort();
    assert_eq!(claims(&state(&repo).unwrap()).unwrap().len(), 10);
    let agent_refs: Vec<&str> = agents.iter().map(String::as_str).collect();
    let mut remote = FakeRemote::single(base);
    let report = ship(&repo, &agent_refs, &mut remote, |candidate, _attempt| {
        for index in 0..10 {
            assert!(candidate
                .join(format!("crates/mod{index:02}/a.rs"))
                .is_file());
            assert!(candidate
                .join(format!("crates/mod{index:02}/b.rs"))
                .is_file());
        }
        Ok(local_gate_evidence())
    })
    .unwrap();
    assert_eq!(remote.pushes.len(), 1);
    assert_eq!(remote.pushes[0].0, report.head);
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());
}

#[test]
fn every_workflow_step_pulls_and_reconciles_bare_origin_with_offline_fallback() {
    let scratch = Scratch::new();
    let repo = scratch.repo();
    let origin_bare = repo.parent().unwrap().join("origin-bare.git");
    git(
        &repo,
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            text(&origin_bare).unwrap(),
        ],
    )
    .unwrap();
    git(
        &repo,
        &["remote", "add", "origin", text(&origin_bare).unwrap()],
    )
    .unwrap();
    let initial_base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
    git(
        &repo,
        &[
            "-c",
            "core.hooksPath=",
            "push",
            "--quiet",
            "origin",
            &format!("{initial_base}:refs/heads/main"),
        ],
    )
    .unwrap();

    // Create a peer clone simulating concurrent remote contributors on `origin/main`.
    let peer = repo.parent().unwrap().join("peer-clone");
    git(
        &repo,
        &[
            "clone",
            "--quiet",
            text(&origin_bare).unwrap(),
            text(&peer).unwrap(),
        ],
    )
    .unwrap();
    git(&peer, &["config", "user.name", "Peer"]).unwrap();
    git(&peer, &["config", "user.email", "peer@example.invalid"]).unwrap();

    let peer_push = |rel: &str, body: &str, task_id: &str| -> String {
        let full = peer.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&full, body).unwrap();
        let task_rel = format!(".agents/tasks/{task_id}.json");
        fs::create_dir_all(peer.join(".agents/tasks")).unwrap();
        let task_doc = serde_json::json!({
            "id": task_id,
            "title": format!("Task {task_id}"),
            "goal": format!("Goal {task_id}"),
            "created_by": "peer",
            "created_unix": 1_794_300_000u64,
            "status": "done",
            "deps": [],
            "scopes": [rel, task_rel],
            "accept": { "cmd": ["cargo", "test", "--offline", "--locked"] }
        });
        fs::write(
            peer.join(&task_rel),
            format!("{}\n", serde_json::to_string_pretty(&task_doc).unwrap()),
        )
        .unwrap();
        git(&peer, &["add", "-A"]).unwrap();
        git(
            &peer,
            &[
                "-c",
                "core.hooksPath=",
                "commit",
                "-qm",
                &format!("feat: {task_id}\n\nTask: {task_id}"),
            ],
        )
        .unwrap();
        git(
            &peer,
            &[
                "-c",
                "core.hooksPath=",
                "push",
                "--quiet",
                "origin",
                "HEAD:refs/heads/main",
            ],
        )
        .unwrap();
        git(&peer, &["rev-parse", "HEAD"]).unwrap()
    };

    // 1. `start` pulls peer commit P1 from `origin/main` before creating worktree.
    let p1 = peer_push("peer/step1.rs", "pub fn step1() {}\n", "T-STEP-1");
    let wt_alpha = start(&repo, "alpha", &["alpha/mod.rs".to_owned()]).unwrap();
    assert_eq!(git(&wt_alpha, &["rev-parse", "HEAD"]).unwrap(), p1);
    assert!(wt_alpha.join("peer/step1.rs").is_file());

    // 2. `sync` (`step`) fast-forwards `local-agent/alpha` when peer pushes P2 before alpha commits.
    let p2 = peer_push("peer/step2.rs", "pub fn step2() {}\n", "T-STEP-2");
    let ff_report = sync_claim(&wt_alpha, "alpha").unwrap();
    assert_eq!(ff_report.mode, AgentSyncMode::FastForwarded);
    assert_eq!(ff_report.head, p2);
    assert!(wt_alpha.join("peer/step2.rs").is_file());

    // 3. `commit` creates alpha's scoped commit AND auto-pulls + 3-way merges peer commit P3!
    let p3 = peer_push("peer/step3.rs", "pub fn step3() {}\n", "T-STEP-3");
    fs::create_dir_all(wt_alpha.join("alpha")).unwrap();
    fs::write(
        wt_alpha.join("alpha/mod.rs"),
        "pub fn alpha() -> u32 { 7 }\n",
    )
    .unwrap();
    commit(&wt_alpha, "alpha", "feat: alpha module", None).unwrap();
    let after_commit = git(&wt_alpha, &["rev-parse", "HEAD"]).unwrap();
    assert!(is_ancestor(&wt_alpha, &p3, &after_commit).unwrap());
    assert!(wt_alpha.join("peer/step3.rs").is_file());
    assert!(wt_alpha.join("alpha/mod.rs").is_file());
    // Scope guard still sees ONLY alpha's scoped file relative to updated claim.base!
    let wf = state(&repo).unwrap();
    let alpha_claim = read_local_claim(&wf.join("claims/alpha.claim")).unwrap();
    assert_eq!(alpha_claim.base, p3);
    check_source(&repo, &alpha_claim).unwrap();

    // 4. Offline fallback: point origin at a disconnected path and verify `sync` succeeds cleanly.
    let disconnected = repo.parent().unwrap().join("disconnected-origin.git");
    git(
        &repo,
        &["remote", "set-url", "origin", text(&disconnected).unwrap()],
    )
    .unwrap();
    let offline_sync = sync_claim(&wt_alpha, "alpha").unwrap();
    assert_eq!(offline_sync.mode, AgentSyncMode::OfflineFallback);
    git(
        &repo,
        &["remote", "set-url", "origin", text(&origin_bare).unwrap()],
    )
    .unwrap();

    // 5. `ship` reconciles with another peer commit P4 on real `GitRemote` and fast-forwards `origin/main`.
    let p4 = peer_push("peer/step4.rs", "pub fn step4() {}\n", "T-STEP-4");
    let mut real_remote = GitRemote;
    let shipped = ship(
        &repo,
        &["alpha"],
        &mut real_remote,
        |candidate, _attempt| {
            assert!(candidate.join("alpha/mod.rs").is_file());
            assert!(candidate.join("peer/step4.rs").is_file());
            Ok(local_gate_evidence())
        },
    )
    .unwrap();
    assert!(is_ancestor(&repo, &p4, &shipped.head).unwrap());

    // 6. `release`: start beta, commit + reconcile cleanly, push, and verify `release` pulls `origin/main`!
    let wt_beta = start(&repo, "beta", &["beta/mod.rs".to_owned()]).unwrap();
    fs::create_dir_all(wt_beta.join("beta")).unwrap();
    fs::write(wt_beta.join("beta/mod.rs"), "pub fn beta() {}\n").unwrap();
    commit(&wt_beta, "beta", "feat: beta module", None).unwrap();
    let beta_head = git(&wt_beta, &["rev-parse", "HEAD"]).unwrap();
    // Push beta_head directly to bare origin WITHOUT updating local refs/remotes/origin/main first.
    git(
        &wt_beta,
        &[
            "-c",
            "core.hooksPath=",
            "push",
            "--quiet",
            text(&origin_bare).unwrap(),
            &format!("{beta_head}:refs/heads/main"),
        ],
    )
    .unwrap();
    release(&repo, "beta").unwrap();
    assert!(claims(&state(&repo).unwrap()).unwrap().is_empty());

    // 7. `workflow_distill`: pulls `origin/main` and distills T-STEP-1..T-STEP-4 into a release snapshot + record!
    let record_out = repo.parent().unwrap().join("v0.21.61.json");
    let distill_out = workflow_distill(
        &repo,
        &[
            "compose".into(),
            "--name".into(),
            "wf-rc1".into(),
            "--base".into(),
            initial_base,
            "--from".into(),
            "origin/main".into(),
            "--select".into(),
            "T-STEP-1,T-STEP-2,T-STEP-3,T-STEP-4".into(),
            "--release-candidate".into(),
            "--strict".into(),
            "--release-tag".into(),
            "v0.21.61".into(),
            "--release-record-out".into(),
            text(&record_out).unwrap().to_owned(),
            "--json".into(),
        ],
    )
    .unwrap();
    let receipt: serde_json::Value = serde_json::from_str(&distill_out).unwrap();
    assert_eq!(receipt["included"].as_array().unwrap().len(), 4);
    assert!(record_out.is_file());
}
