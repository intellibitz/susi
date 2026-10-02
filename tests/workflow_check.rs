#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `susi workflow check` against real git topologies: a primary checkout, a
//! worktree, a stale clone, missing hooks and missing claims.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("SUSI_HOME");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn git(dir: &Path, args: &[&str]) {
    let (code, _, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
}

struct World {
    root: PathBuf,
    primary: PathBuf,
    wt: PathBuf,
    home: PathBuf,
}

impl World {
    /// bare server + a primary clone with `.githooks` on main + a worktree.
    fn new(tag: &str) -> Self {
        // Unique per call, not just per process: libtest runs the tests of one
        // binary as threads, so two tests that happen to pick the same tag would
        // otherwise share a directory and delete each other's fixture.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root =
            std::env::temp_dir().join(format!("susi-wfc-{tag}-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("primary");
        let home = root.join("home");
        for d in [&bare, &primary, &home] {
            std::fs::create_dir_all(d).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        // The fixture owns its identity. A test that lets git fall back to the
        // *machine's* global config passes only where a developer has one: on a
        // CI runner `git merge` then fails with "Committer identity unknown"
        // before it ever writes MERGE_HEAD, which silently turns the
        // unresolved-merge case into an ordinary dirty tree. `wt` is a linked
        // worktree, so it shares this config.
        for repo in [&bare, &primary] {
            git(repo, &["config", "user.name", "t"]);
            git(repo, &["config", "user.email", "t@t"]);
        }
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        std::fs::create_dir_all(primary.join(".githooks")).unwrap();
        for hook in ["commit-msg", "pre-commit", "pre-push", "workflow-guard"] {
            let p = primary.join(".githooks").join(hook);
            std::fs::write(&p, "#!/bin/sh\nexit 0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        let park = primary.join("scripts/park-primary.sh");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/park-primary.sh"),
            &park,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&park, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "--quiet", "-m", "init"]);
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["fetch", "--quiet", "origin"]);
        let wt = root.join("wt");
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "work",
                wt.to_str().unwrap(),
            ],
        );
        git(&wt, &["config", "core.hooksPath", ".githooks"]);
        Self {
            root,
            primary,
            wt,
            home,
        }
    }

    fn check(&self, dir: &Path) -> (i32, String) {
        let (code, out, _) = run(
            dir,
            env!("CARGO_BIN_EXE_susi"),
            &["workflow", "check"],
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", self.home.join("xdg").to_str().unwrap()),
                ("SUSI_AGENT", "test"),
            ],
        );
        (code, out)
    }

    fn susi(&self, dir: &Path, args: &[&str]) -> (i32, String, String) {
        run(
            dir,
            env!("CARGO_BIN_EXE_susi"),
            args,
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", self.home.join("xdg").to_str().unwrap()),
                ("SUSI_AGENT", "test"),
            ],
        )
    }

    fn claim_a_task(&self) {
        let (code, _, err) = self.susi(
            &self.wt,
            &["tasks", "add", "job", "--accept", "cargo --version"],
        );
        assert_eq!(code, 0, "{err}");
        let (code, _, err) = self.susi(&self.wt, &["tasks", "claim", "T-TEST-1"]);
        assert_eq!(code, 0, "{err}");
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_ready_worktree_passes_and_says_so() {
    let w = World::new("ready");
    w.claim_a_task();
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "{out}");
    for needle in [
        "✅ own worktree",
        "✅ up to date",
        "✅ hooks installed",
        "✅ holds a claim",
        "ready:",
    ] {
        assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
    }
}

#[test]
fn the_primary_checkout_is_refused_with_the_fix() {
    let w = World::new("primary");
    w.claim_a_task();
    let (code, out) = w.check(&w.primary);
    assert_ne!(code, 0);
    assert!(out.contains("❌ own worktree"), "{out}");
    assert!(out.contains("scripts/susi-worktree.sh"), "{out}");
}

#[test]
fn a_stale_worktree_is_told_to_merge_origin_main() {
    let w = World::new("stale");
    w.claim_a_task();
    // Someone else advances origin/main.
    let other = w.root.join("other");
    git(
        &w.root,
        &[
            "clone",
            "--quiet",
            w.root.join("server.git").to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(
        &other,
        &["commit", "--allow-empty", "--quiet", "-m", "newer"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let (code, out) = w.check(&w.wt);
    assert_ne!(code, 0);
    assert!(out.contains("❌ up to date"), "{out}");
    assert!(out.contains("1 commit(s) behind origin/main"), "{out}");
    assert!(out.contains("git merge origin/main"), "{out}");
}

#[test]
fn missing_hooks_are_healed_and_missing_claim_is_reported() {
    let w = World::new("hooks");
    // No claim yet, and this worktree's hooks are not configured — check
    // heals hooksPath itself (hooks-on-clone), so the only ❌ left is
    // the missing claim.
    git(&w.wt, &["config", "--unset", "core.hooksPath"]);
    let (code, out) = w.check(&w.wt);
    assert_ne!(code, 0);
    assert!(out.contains("✅ hooks installed"), "{out}");
    assert_eq!(
        git_out(&w.wt, &["config", "--get", "core.hooksPath"]),
        ".githooks"
    );
    assert!(out.contains("❌ holds a claim"), "{out}");
    assert!(out.contains("susi tasks claim"), "{out}");
}

#[test]
fn hooks_missing_from_the_branch_are_named() {
    let w = World::new("nohooks");
    w.claim_a_task();
    std::fs::remove_file(w.wt.join(".githooks/commit-msg")).unwrap();
    let (code, out) = w.check(&w.wt);
    assert_ne!(code, 0);
    assert!(
        out.contains("missing or not executable: commit-msg"),
        "{out}"
    );
}

fn git_out(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out.trim().to_string()
}

#[test]
fn workflow_check_keeps_the_primary_checkouts_main_current() {
    let w = World::new("sync");
    w.claim_a_task();
    // origin/main moves on; the primary checkout (on main) is now behind.
    let other = w.root.join("other");
    git(
        &w.root,
        &[
            "clone",
            "--quiet",
            w.root.join("server.git").to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    git(
        &other,
        &["commit", "--allow-empty", "--quiet", "-m", "newer"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    let before = git_out(&w.primary, &["rev-parse", "HEAD"]);
    // Running the check from a worktree advances the primary checkout.
    let (_, out) = w.check(&w.wt);
    let after = git_out(&w.primary, &["rev-parse", "HEAD"]);
    assert_ne!(
        before, after,
        "primary main should have been fast-forwarded:\n{out}"
    );
    assert_eq!(after, git_out(&w.primary, &["rev-parse", "origin/main"]));
    assert_eq!(
        git_out(&w.primary, &["symbolic-ref", "--short", "HEAD"]),
        "main"
    );
}

fn advance_origin(w: &World) {
    let other = w.root.join("other");
    if !other.exists() {
        git(
            &w.root,
            &[
                "clone",
                "--quiet",
                w.root.join("server.git").to_str().unwrap(),
                other.to_str().unwrap(),
            ],
        );
    }
    git(&other, &["pull", "--quiet", "origin", "main"]);
    // A real, binary-affecting change: "main moved ahead" has to mean main
    // gained something the installed tool lacks, which the release check judges
    // by the paths the binary is built from (src, crates, Cargo.*, build.rs).
    std::fs::create_dir_all(other.join("src")).unwrap();
    std::fs::write(other.join("src/newer.rs"), "// newer\n").unwrap();
    git(&other, &["add", "-A"]);
    git(&other, &["commit", "--quiet", "-m", "newer"]);
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
}

fn add_task(w: &World) {
    let (code, _, err) = w.susi(
        &w.wt,
        &["tasks", "add", "job", "--accept", "cargo --version"],
    );
    assert_eq!(code, 0, "{err}");
}

#[test]
fn claim_requires_a_synced_worktree_and_says_how_to_sync() {
    let w = World::new("claimsync");
    add_task(&w);
    advance_origin(&w);
    let (code, _, err) = w.susi(&w.wt, &["tasks", "claim", "T-TEST-1"]);
    assert_ne!(code, 0, "a stale worktree must not claim");
    assert!(err.contains("1 commit(s) behind origin/main"), "{err}");
    assert!(err.contains("git merge origin/main"), "{err}");
    // No claim was taken.
    let (_, out, _) = w.susi(&w.wt, &["tasks", "list"]);
    assert!(!out.contains("\"claimed_by\": \"TEST\""), "{out}");
    // After syncing, the same claim goes through.
    git(&w.wt, &["merge", "--quiet", "origin/main"]);
    let (code, _, err) = w.susi(&w.wt, &["tasks", "claim", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn claim_requires_a_synced_worktree_but_offline_is_not_an_error() {
    let w = World::new("claimoffline");
    add_task(&w);
    // Server gone: freshness is unknowable, and the claim then fails on the
    // remote itself, not on the sync check.
    std::fs::remove_dir_all(w.root.join("server.git")).unwrap();
    let (_, _, err) = w.susi(&w.wt, &["tasks", "claim", "T-TEST-1"]);
    assert!(!err.contains("behind origin/main"), "{err}");
}

/// `sync` refuses a dirty tree and `finish`'s first gate step dies on one, but
/// `check` used to call that same tree ready — the first symptom was a failed
/// finish. A work in progress is now visible, and still not blocking.
#[test]
fn uncommitted_work_is_reported_without_blocking_the_loop() {
    let w = World::new("dirty");
    w.claim_a_task();
    std::fs::write(w.wt.join("scratch.txt"), "work in progress").unwrap();
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "a work in progress must not block: {out}");
    assert!(out.contains("⚠️  tree clean"), "{out}");
    assert!(out.contains("uncommitted change(s)"), "{out}");
    assert!(out.contains("commit or stash"), "{out}");
}

/// An unresolved merge is the state the review found agents stuck in: every
/// later sync and finish dies on it. It fails the check, with the way out.
#[test]
fn an_unresolved_merge_fails_the_check_and_says_how_to_get_out() {
    let w = World::new("merge");
    w.claim_a_task();
    std::fs::write(w.wt.join("shared.txt"), "branch side").unwrap();
    git(&w.wt, &["add", "shared.txt"]);
    git(&w.wt, &["commit", "--quiet", "-m", "branch side"]);
    std::fs::write(w.primary.join("shared.txt"), "main side").unwrap();
    git(&w.primary, &["add", "shared.txt"]);
    git(&w.primary, &["commit", "--quiet", "-m", "main side"]);
    git(&w.primary, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&w.wt, &["fetch", "--quiet", "origin"]);
    // Conflicts, and leaves MERGE_HEAD behind. Through the helper, so the
    // fixture's identity is used rather than whatever the machine happens to
    // have configured — a raw `git` here is what let this test depend on it.
    let _ = run(&w.wt, "git", &["merge", "--no-edit", "origin/main"], &[]);
    let (code, out) = w.check(&w.wt);
    assert_ne!(code, 0, "an unresolved merge must fail the check: {out}");
    assert!(out.contains("❌ tree clean"), "{out}");
    assert!(out.contains("git merge --abort"), "{out}");
}

/// The watcher keeps the primary checkout parked at origin/main and is required
/// while agents run, but nothing noticed when it died: its log stays empty
/// while things go well. A heartbeat makes a dead watcher visible.
#[test]
fn a_primary_watcher_without_a_heartbeat_is_reported() {
    let w = World::new("watcher");
    w.claim_a_task();
    let (_, out) = w.check(&w.wt);
    assert!(out.contains("⚠️  primary watcher"), "{out}");
    assert!(out.contains("no heartbeat"), "{out}");
    assert!(out.contains("susi workflow watch"), "{out}");

    // A fresh heartbeat passes.
    let common = git_out(
        &w.wt,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        std::path::Path::new(common.trim()).join("susi-primary-watch.stamp"),
        format!("{now}\n"),
    )
    .unwrap();
    let (_, out) = w.check(&w.wt);
    assert!(out.contains("✅ primary watcher"), "{out}");
}

/// A clone accumulates one worktree per task and nothing reclaims them (this
/// repository reached 14). The check counts the ones that are finished — clean
/// and already contained in `origin/main` — per agent, and never blocks on it.
#[test]
fn finished_worktrees_are_reported_by_the_check() {
    let w = World::new("stale-trees");
    w.claim_a_task();
    git(&w.primary, &["config", "extensions.worktreeConfig", "true"]);
    let spare = w.root.join("spare");
    git(
        &w.primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "spare",
            spare.to_str().unwrap(),
            "origin/main",
        ],
    );
    git(&spare, &["config", "--worktree", "susi.agent", "TEST"]);

    let (code, out) = w.check(&w.wt);
    assert!(out.contains("stale worktrees"), "{out}");
    assert!(
        out.contains("1 of your finished worktree(s) can be reclaimed"),
        "{out}"
    );
    assert!(out.contains("scripts/prune-worktrees.sh --apply"), "{out}");
    assert_eq!(code, 0, "housekeeping must never block the loop: {out}");

    // Uncommitted work in that worktree takes it off the list.
    std::fs::write(spare.join("scratch.txt"), "wip").unwrap();
    let (_, out) = w.check(&w.wt);
    assert!(out.contains("✅ stale worktrees"), "{out}");
}

/// Writes the marker `scripts/susi-release-sync.sh` installs beside the binary
/// (`${HOME}/.susi/bin/susi.build.json`), naming the commit it was built from.
fn install_release(w: &World, commit: &str) {
    let dir = w.home.join(".susi/bin");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("susi.build.json"),
        format!(
            "{{\"profile\":\"release\",\"accelerator\":\"none\",\"channel\":\"release\",\
             \"version\":\"0.0.0\",\"commit\":\"{commit}\",\"installed_at\":0}}\n"
        ),
    )
    .unwrap();
}

/// A release is the only thing agents run, and it only changes when someone
/// cuts and promotes one — so a fix can be merged and documented as active
/// while the tool every worker runs still enforces the old rules. `0.21.0` was
/// installed 92 commits behind main and released a claim taken on another
/// branch, which main refuses. The version string cannot show that; only the
/// release's own commit can. Advisory, because no agent can fix it alone.
#[test]
fn a_stale_installed_release_is_reported_without_blocking_the_loop() {
    let w = World::new("release-stale");
    w.claim_a_task();
    // The host is running the release built from what main is on right now.
    let installed = git_out(&w.wt, &["rev-parse", "HEAD"]);
    install_release(&w, &installed);
    // Then main moves ahead of it, and this worktree syncs as the loop requires.
    advance_origin(&w);
    git(&w.wt, &["fetch", "--quiet", "origin"]);
    git(&w.wt, &["merge", "--no-edit", "--quiet", "origin/main"]);

    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "a stale release must not block the loop: {out}");
    assert!(out.contains("⚠️  installed susi"), "{out}");
    assert!(out.contains("1 commit(s) behind origin/main"), "{out}");
    assert!(out.contains("susi-release-sync.sh"), "{out}");
}

/// Landing the release on main adds a merge commit and no content, so counting
/// merge commits made every release read as one commit stale the moment it
/// landed. A warning that is wrong the day it appears is one agents learn to
/// ignore, which is the opposite of the point.
#[test]
fn landing_the_release_on_main_is_not_drift() {
    let w = World::new("release-merge");
    w.claim_a_task();
    let other = w.root.join("other");
    git(
        &w.root,
        &[
            "clone",
            "--quiet",
            w.root.join("server.git").to_str().unwrap(),
            other.to_str().unwrap(),
        ],
    );
    // The release commit lives on its branch, and this host installed it...
    git(&other, &["switch", "--quiet", "-c", "release"]);
    git(
        &other,
        &[
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "chore: release v0.0.1",
        ],
    );
    let released = git_out(&other, &["rev-parse", "release"]);
    git(
        &other,
        &["push", "--quiet", "origin", "HEAD:refs/heads/release"],
    );
    install_release(&w, &released);

    // ...then its pull request landed: a merge commit and nothing else.
    git(&other, &["switch", "--quiet", "main"]);
    git(
        &other,
        &["merge", "--no-ff", "--no-edit", "--quiet", "release"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);

    git(&w.wt, &["fetch", "--quiet", "origin"]);
    git(&w.wt, &["merge", "--no-edit", "--quiet", "origin/main"]);
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("✅ installed susi"),
        "a merge that adds no content is not drift: {out}"
    );

    // Closing a task moves one record under .agents/tasks/ and adds no code,
    // so the very next task close must not resurrect the warning either.
    git(&other, &["switch", "--quiet", "-c", "close-task"]);
    std::fs::create_dir_all(other.join(".agents/tasks")).unwrap();
    std::fs::write(other.join(".agents/tasks/T-TEST-9.json"), "{}\n").unwrap();
    git(&other, &["add", "-A"]);
    git(
        &other,
        &["commit", "--quiet", "-m", "Close T-TEST-9 after acceptance"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&w.wt, &["fetch", "--quiet", "origin"]);
    git(&w.wt, &["merge", "--no-edit", "--quiet", "origin/main"]);
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("✅ installed susi"),
        "a task record is not a workflow fix: {out}"
    );

    // Nor is a test-only commit: it cannot change what the installed tool does,
    // and warning about it made every test fix look like a missing release.
    git(&other, &["switch", "--quiet", "-c", "worker-test"]);
    std::fs::create_dir_all(other.join("tests")).unwrap();
    std::fs::write(other.join("tests/newer_test.rs"), "// newer\n").unwrap();
    git(&other, &["add", "-A"]);
    git(
        &other,
        &["commit", "--quiet", "-m", "fix(test): a fixture detail"],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&w.wt, &["fetch", "--quiet", "origin"]);
    git(&w.wt, &["merge", "--no-edit", "--quiet", "origin/main"]);
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("✅ installed susi"),
        "a test-only commit is not a workflow fix: {out}"
    );

    // A change the binary is built from IS drift, so the rule still bites.
    git(&other, &["switch", "--quiet", "-c", "worker-code"]);
    std::fs::create_dir_all(other.join("src")).unwrap();
    std::fs::write(other.join("src/behavior.rs"), "// behavior\n").unwrap();
    git(&other, &["add", "-A"]);
    git(
        &other,
        &[
            "commit",
            "--quiet",
            "-m",
            "fix(workflow): a real behavior fix",
        ],
    );
    git(&other, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&w.wt, &["fetch", "--quiet", "origin"]);
    git(&w.wt, &["merge", "--no-edit", "--quiet", "origin/main"]);
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "a stale release warns, it does not block: {out}");
    assert!(
        out.contains("⚠️  installed susi"),
        "a src change is exactly what a release must carry: {out}"
    );
}

/// A release built from the tip of main is what the loop assumes, and a host
/// with no marker at all (a dev build, or the first run here) says so instead
/// of implying drift that cannot be measured.
#[test]
fn a_current_release_passes_and_a_dev_host_reports_no_marker() {
    let w = World::new("release-current");
    w.claim_a_task();
    let head = git_out(&w.wt, &["rev-parse", "HEAD"]);

    let (_, out) = w.check(&w.wt);
    assert!(out.contains("✅ installed susi"), "{out}");
    assert!(out.contains("no release marker"), "{out}");

    install_release(&w, &head);
    let (code, out) = w.check(&w.wt);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("✅ installed susi"), "{out}");
    assert!(out.contains("is on origin/main"), "{out}");
}
