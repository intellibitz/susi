#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! `scripts/check-workflow-compliance.sh` against real temp repos and the real
//! `susi tasks` binary: a commit passes only if its `Task:` trailer names a
//! task that is closed or held by a live claim.

use std::path::{Path, PathBuf};
use std::process::Command;

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "agent")
        .env("GIT_AUTHOR_EMAIL", "a@a")
        .env("GIT_COMMITTER_NAME", "agent")
        .env("GIT_COMMITTER_EMAIL", "a@a")
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

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out.trim().to_string()
}

struct Env {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Env {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-wf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let home = root.join("home");
        let bare = root.join("server.git");
        for d in [&repo, &home, &bare] {
            std::fs::create_dir_all(d).unwrap();
        }
        git(&bare, &["init", "--bare", "--quiet"]);
        git(&repo, &["init", "--quiet", "-b", "main"]);
        git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
        // A checker that already exists on main: the fixture acceptance these
        // tests declare, so their tasks are re-runnable (and `close`, which
        // executes it, succeeds in a repo with no Cargo.toml).
        let check = repo.join("scripts/fixture-check.sh");
        std::fs::create_dir_all(check.parent().unwrap()).unwrap();
        std::fs::write(&check, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&check).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&check, perms).unwrap();
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "--quiet", "-m", "init"]);
        git(&repo, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&repo, &["fetch", "--quiet", "origin"]);
        Self { root, repo, home }
    }

    fn susi(&self, args: &[&str]) -> (i32, String, String) {
        run(
            &self.repo,
            env!("CARGO_BIN_EXE_susi"),
            args,
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("XDG_CONFIG_HOME", self.home.join("xdg").to_str().unwrap()),
                ("SUSI_AGENT", "test"),
            ],
        )
    }

    fn commit(&self, file: &str, subject: &str, task: Option<&str>) -> String {
        let path = self.repo.join("work").join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, file).unwrap();
        git(&self.repo, &["add", "-A"]);
        match task {
            Some(t) => git(
                &self.repo,
                &[
                    "commit",
                    "--quiet",
                    "-m",
                    subject,
                    "-m",
                    &format!("Task: {t}"),
                ],
            ),
            None => git(&self.repo, &["commit", "--quiet", "-m", subject]),
        };
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn check(&self, base: &str) -> (i32, String) {
        self.check_on(base, "main")
    }

    /// The branch being pushed is what a live claim's ownership is judged
    /// against, so tests can push as a branch other than the checked-out one.
    fn check_on(&self, base: &str, branch: &str) -> (i32, String) {
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh");
        let (code, _, err) = run(
            &self.repo,
            script.to_str().unwrap(),
            &[base, "HEAD", "origin", branch],
            &[],
        );
        (code, err)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_commit_must_name_a_claimed_or_closed_task() {
    let e = Env::new("main");
    // Install the rule (the script's own commit is the cutover and must comply).
    let (code, out, err) = e.susi(&[
        "tasks",
        "add",
        "first",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("T-TEST-1"), "{out}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
                                                            // Fixtures reserve `work/`, where every `commit()` in this file writes:
                                                            // a claim that reserves no paths may no longer land code.
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "install the rule",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let origin_main = "origin/main";

    // 1. Trailer + live claim: passes.
    e.commit("a", "feat: a", Some("T-TEST-1"));
    let (code, err) = e.check(origin_main);
    assert_eq!(code, 0, "{err}");

    // 2. No trailer: refused, and the message says how to fix it.
    let before = e.head();
    e.commit("b", "feat: b", None);
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("no 'Task: T-<AGENT>-<n>' trailer"), "{err}");

    // 3. Trailer naming a task that does not exist: refused.
    let before = e.head();
    e.commit("c", "feat: c", Some("T-TEST-99"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("not a task in this branch"), "{err}");

    // 4. A real task nobody has claimed: refused; claiming it makes it pass.
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "second",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add second task"]);
    let before = e.head();
    e.commit("d", "feat: d", Some("T-TEST-2"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("nobody holds a claim"), "{err}");
    let (code, _, err) = e.susi(&[
        "tasks",
        "release",
        "T-TEST-1",
        "--abandon",
        "fixture: moving to the second task",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-2", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "{err}");

    // 5. Work then close on the same branch passes: the task was open at the
    //    work commit and the branch closed it (ownership remains until merge).
    let (code, _, err) = e.susi(&[
        "tasks",
        "release",
        "T-TEST-2",
        "--abandon",
        "fixture: returning to the first task",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    let before = e.head();
    e.commit("e", "fix: e", Some("T-TEST-1"));
    let (code, _, err) = e.susi(&["tasks", "close", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "close first task"]); // task-only: exempt
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "{err}");

    // 5b. The pipeline's handoff: `finish` may release the claim once the
    //     close receipt is on the remote, so a re-judged range finds no claim
    //     on T-TEST-1. The receipt is the stronger fact — writing it required
    //     the live owned claim — so the same commits still pass.
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-1", "--force"]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = e.check(&before);
    assert_eq!(
        code, 0,
        "a released claim on a receipted task passes: {err}"
    );

    // 5c. Once closed, a task cannot be cited again (unrelated work must not
    //     ride on it).
    let before = e.head();
    e.commit("e2", "fix: sneaky", Some("T-TEST-1"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("already closed"), "{err}");

    // 6. Release commits need no task.
    let before = e.head();
    e.commit("f", "chore: release v9.9.9", None);
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "{err}");
}

#[test]
fn a_closed_task_cannot_be_cited_again() {
    let e = Env::new("closed");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "install rule",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    e.commit("work", "feat: work", Some("T-TEST-1"));
    let (code, _, err) = e.susi(&["tasks", "close", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "close"]);
    let (code, err) = e.check("origin/main");
    assert_eq!(code, 0, "work then close on one branch is compliant: {err}");
    // A later commit citing the now-closed task is refused.
    let before = e.head();
    e.commit("more", "feat: more", Some("T-TEST-1"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("already closed"), "{err}");
}

impl Env {
    /// Copy the compliance script in and commit it, so the cutover exists and
    /// the commits that follow are judged.
    fn install_rule(&self, task: &str) {
        std::fs::create_dir_all(self.repo.join("scripts")).unwrap();
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
            self.repo.join("scripts/check-workflow-compliance.sh"),
        )
        .unwrap();
        git(&self.repo, &["add", "-A"]);
        git(
            &self.repo,
            &[
                "commit",
                "--quiet",
                "-m",
                "install the rule",
                "-m",
                &format!("Task: {task}"),
            ],
        );
    }
}

/// A claim owns ONE branch. Without this, a second agent can push work for a
/// task another agent already holds — the exact duplicate work the queue exists
/// to prevent, and something the liveness check alone cannot see.
#[test]
fn a_claim_held_on_another_branch_is_refused() {
    let e = Env::new("owner");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    e.install_rule("T-TEST-1"); // claim is live here, so this complies

    // Work on another branch for the task main holds: refused, and the message
    // says which branch owns it and how to take it over legitimately.
    git(&e.repo, &["switch", "--quiet", "-c", "feature"]);
    let before = e.head();
    e.commit("a", "feat: a", Some("T-TEST-1"));
    let (code, err) = e.check_on(&before, "feature");
    assert_eq!(code, 1, "another branch's claim must be refused: {err}");
    assert!(err.contains("holds on branch 'main'"), "{err}");
    assert!(err.contains("susi tasks release T-TEST-1 --force"), "{err}");

    // The documented remedy works, and it is deliberate: `release` refuses a
    // claim taken on another branch unless forced, because the agent token
    // alone can be shared or collide between workers.
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-1"]);
    assert_ne!(code, 0, "a cross-branch release must be refused: {err}");
    assert!(err.contains("was claimed on branch"), "{err}");
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-1", "--force"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = e.check_on(&before, "feature");
    assert_eq!(code, 0, "after re-claiming on this branch: {err}");
    // ...and the claim really did move: main no longer owns it.
    let (code, err) = e.check_on(&before, "main");
    assert_eq!(code, 1, "{err}");
}

/// Scopes are how two agents stay off each other's files. The local pre-commit
/// check can be skipped with --no-verify and only sees fetched claims, so CI
/// enforces the same rule on the pushed commits.
#[test]
fn files_outside_the_claims_scopes_are_refused() {
    let e = Env::new("scopes");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
    e.install_rule("T-TEST-1");
    let base = e.head();
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-1", "--scope", "work/allowed"]);
    assert_eq!(code, 0, "{err}");

    // Inside the reserved scope: compliant.
    e.commit("allowed/a", "feat: a", Some("T-TEST-1"));
    let (code, err) = e.check(&base);
    assert_eq!(code, 0, "in-scope work must pass: {err}");

    // Outside it: refused, naming the file and the fix.
    let before = e.head();
    e.commit("other/b", "feat: b", Some("T-TEST-1"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1, "out-of-scope work must be refused: {err}");
    assert!(
        err.contains("outside T-TEST-1's claimed scopes: work/other/b"),
        "{err}"
    );

    // Re-claiming with a scope that covers it makes the same commit compliant.
    // Re-scoping a claim that carries work: the explicit override, since a
    // plain release of unpublished work now needs a recorded reason.
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-1", "--force"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&[
        "tasks",
        "claim",
        "T-TEST-1",
        "--scope",
        "work/allowed",
        "--scope",
        "work/other",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "after widening the claim's scopes: {err}");
}

/// A claim that reserves no paths used to skip the scope test entirely, which
/// made the reservation optional in practice: claim without `--scope` and any
/// path was fair game, with neither the pre-commit hook nor CI able to tell two
/// agents off one file. 19 of the 20 claims on the real remote reserved
/// nothing. Task records stay exempt, so an unscoped claim can be closed.
#[test]
fn code_under_a_claim_that_reserves_no_paths_is_refused() {
    let e = Env::new("noscope");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
    e.install_rule("T-TEST-1");
    let base = e.head();
    // A scopeless claim now takes the deliberate override; what it buys is the
    // refusal below, because nothing about such a claim can be checked.
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-1", "--unscoped"]);
    assert_eq!(code, 0, "{err}");

    e.commit("work/a", "feat: a", Some("T-TEST-1"));
    let (code, err) = e.check(&base);
    assert_eq!(
        code, 1,
        "code under a claim that reserves no paths must be refused: {err}"
    );
    assert!(err.contains("reserves no paths"), "{err}");
    assert!(err.contains("work/a"), "{err}");
    assert!(err.contains("susi tasks claim T-TEST-1 --scope"), "{err}");

    // Closing it moves only .agents/tasks/: exempt, so the claim can be
    // finished rather than becoming unclosable.
    let before_close = e.head();
    let (code, _, err) = e.susi(&["tasks", "close", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "close first task",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let (code, err) = e.check(&before_close);
    assert_eq!(code, 0, "closing an unscoped task must still pass: {err}");
}

#[test]
fn history_before_the_rule_is_not_judged() {
    let e = Env::new("cutover");
    // A commit with no trailer exists before the script does.
    e.commit("old", "feat: old work", None);
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    let (code, _, err) = e.susi(&["tasks", "add", "t", "--accept", "scripts/fixture-check.sh"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "install rule and task",
            "-m",
            "Task: T-TEST-1", // the commit that installs the rule is itself bound by it
        ],
    );
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    e.commit("new", "feat: new", Some("T-TEST-1"));
    let (code, err) = e.check("origin/main");
    assert_eq!(
        code, 0,
        "old untrailered history must not fail the check: {err}"
    );
}

/// The claim requirement is not skipped just because the branch ends with
/// `done/<id>.json`. Before this, a branch could create a task, never claim it,
/// do the work, and hand-move the file into done/ — the closing commit touches
/// only `.agents/tasks/` (exempt from the trailer rule), so the whole thing
/// passed. `susi tasks close` keeps the lease until the closing commit reaches
/// main, so the real path through the CLI is unaffected.
#[test]
fn a_hand_moved_done_file_does_not_excuse_an_unclaimed_task() {
    let e = Env::new("handclosed");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "target",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "rule",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add tasks"]); // task-only: exempt
                                                             // The rule-install commit needs a task of its own, and that one is claimed.
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-2", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    e.install_rule("T-TEST-2");

    // Work citing T-TEST-1, which nobody ever claimed.
    e.commit("a", "feat: a", Some("T-TEST-1"));
    let (code, err) = e.check("origin/main");
    assert_eq!(code, 1, "unclaimed work must be refused: {err}");
    assert!(err.contains("nobody holds a claim on it"), "{err}");

    // Hand-moving the task into done/ must not launder it: the check used to
    // skip the claim requirement as soon as done/<id>.json existed at head.
    std::fs::create_dir_all(e.repo.join(".agents/tasks/done")).unwrap();
    git(
        &e.repo,
        &[
            "mv",
            ".agents/tasks/T-TEST-1.json",
            ".agents/tasks/done/T-TEST-1.json",
        ],
    );
    git(
        &e.repo,
        &["commit", "--quiet", "-m", "close the task by hand"],
    ); // task-only: exempt
    let (code, err) = e.check("origin/main");
    assert_eq!(code, 1, "a hand-moved done file must not excuse it: {err}");
    assert!(err.contains("nobody holds a claim on it"), "{err}");
}

/// The gate fails closed. An unresolvable base used to make the commit loop
/// iterate zero commits and print the ✅ line, certifying a range it never read.
#[test]
fn a_range_that_cannot_be_resolved_is_refused_instead_of_certified() {
    let e = Env::new("badbase");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
    e.install_rule("T-TEST-1");
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    e.commit("a", "feat: a", Some("T-TEST-1"));

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh");
    let (code, _, err) = run(
        &e.repo,
        script.to_str().unwrap(),
        &["no-such-ref", "HEAD"],
        &[],
    );
    assert_eq!(code, 1, "an unknown range must not be certified: {err}");
    assert!(err.contains("cannot resolve base"), "{err}");
    assert!(!err.contains('✅'), "{err}");
}

/// Deleting the gate does not reset the rule. The cutover is the commit that
/// *added* the script, so a branch cannot drop it and have its earlier commits
/// exempted as "history before the rule" — they are still judged.
#[test]
fn deleting_the_compliance_script_does_not_exempt_earlier_commits() {
    let e = Env::new("nogate");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
    e.install_rule("T-TEST-1");
    // Publish it, so the base really does carry the rule.
    git(&e.repo, &["push", "--quiet", "origin", "HEAD:main"]);

    // Work nobody claimed, then the gate is deleted to try to escape judgement.
    e.commit("a", "feat: unclaimed", Some("T-TEST-1"));
    git(
        &e.repo,
        &["rm", "--quiet", "scripts/check-workflow-compliance.sh"],
    );
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "drop the gate",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let (code, err) = e.check("origin/main");
    assert_eq!(code, 1, "the earlier commit is still judged: {err}");
    assert!(err.contains("nobody holds a claim on it"), "{err}");
}

/// A verification verdict is the record of the work, not the work: it lives in
/// `.agents/roadmap-verdicts/<VC-id>.json` and must commit under the very claim
/// that produced it, which reserves source paths, not that directory. Requiring
/// a reservation for it would make all 30 verification tasks uncommittable.
#[test]
fn a_verdict_record_commits_under_any_claim() {
    let e = Env::new("verdict");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add task"]); // task-only: exempt
    e.install_rule("T-TEST-1");
    let base = e.head();
    // A claim that reserves source, and nothing under .agents/.
    let (code, _, err) = e.susi(&["tasks", "claim", "T-TEST-1", "--scope", "work"]);
    assert_eq!(code, 0, "{err}");

    std::fs::create_dir_all(e.repo.join(".agents/roadmap-verdicts")).unwrap();
    std::fs::write(
        e.repo.join(".agents/roadmap-verdicts/VC-201-001.json"),
        "{\"vector\":\"VC-201-001\",\"verdict\":\"not-delivered\",\"missing\":\"x\",\
         \"tracked_by\":[\"T-TEST-2\"],\"recorded_by\":\"TEST\",\"recorded_unix\":1}\n",
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "record a verdict",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let (code, err) = e.check(&base);
    assert_eq!(
        code, 0,
        "a verdict record must not need its own reservation: {err}"
    );

    // It is still work: without the trailer, it is refused like anything else.
    std::fs::write(
        e.repo.join(".agents/roadmap-verdicts/VC-201-002.json"),
        "{\"vector\":\"VC-201-002\",\"verdict\":\"delivered\",\"evidence\":\"x\",\
         \"recorded_by\":\"TEST\",\"recorded_unix\":1}\n",
    )
    .unwrap();
    let before = e.head();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &["commit", "--quiet", "-m", "record another verdict"],
    );
    let (code, err) = e.check(&before);
    assert_eq!(
        code, 1,
        "a verdict commit is work and needs its task: {err}"
    );
    assert!(err.contains("no 'Task:"), "{err}");
}

/// A task that changes the CLI surface but not the README leaves the front door
/// describing a tool that no longer exists. It is an advisory, not a gate: the
/// README is prose, and a check that forces prose produces worse prose than a
/// task that lags.
#[test]
fn a_cli_change_without_the_readme_is_warned_about() {
    let e = Env::new("docs");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "one",
        "--accept",
        "scripts/fixture-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    e.install_rule("T-TEST-1");
    let base = e.head();
    let (code, _, err) = e.susi(&[
        "tasks",
        "claim",
        "T-TEST-1",
        "--scope",
        "src/cli",
        "--scope",
        "README.md",
    ]);
    assert_eq!(code, 0, "{err}");

    // A CLI file changes; the README does not.
    std::fs::create_dir_all(e.repo.join("src/cli")).unwrap();
    std::fs::write(e.repo.join("src/cli/new_command.rs"), "// a command\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "feat: a command",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let (code, err) = e.check(&base);
    assert_eq!(code, 0, "the docs warning must never fail the gate: {err}");
    assert!(
        err.contains("src/cli/ changed without README.md"),
        "the CLI change must be reported: {err}"
    );

    // Touching the README in the same range settles it.
    let after = e.head();
    std::fs::write(e.repo.join("README.md"), "# front door\n").unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "docs: describe it",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let (code, err) = e.check(&after);
    assert_eq!(code, 0, "{err}");
    assert!(
        !err.contains("changed without README.md"),
        "a documented CLI change must not warn: {err}"
    );
}

/// A claim blob as `susi tasks claim` writes it: live for an hour, owned by
/// `agent` on `branch`, reserving `scopes`.
fn claim_json(task: &str, agent: &str, branch: &str, scopes: &[&str]) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    serde_json::json!({
        "task": task, "agent": agent, "branch": branch, "scopes": scopes,
        "claimed_unix": now, "lease_until_unix": now + 3600
    })
    .to_string()
}

/// A claim whose lease has already lapsed: the takeover path, and never a
/// second live task for its agent.
fn expired_claim_json(task: &str, agent: &str, branch: &str, scopes: &[&str]) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    serde_json::json!({
        "task": task, "agent": agent, "branch": branch, "scopes": scopes,
        "claimed_unix": now - 7200, "lease_until_unix": now - 3600
    })
    .to_string()
}

/// A close receipt as `susi tasks close` pushes it.
fn receipt_json(task: &str, agent: &str) -> String {
    serde_json::json!({
        "task": task, "agent": agent, "head": "0".repeat(40), "closed_unix": 1
    })
    .to_string()
}

fn push_ref(repo: &Path, name: &str, body: &str) {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "hash-object: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let blob = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // Production writes these refs with a compare-and-swap lease (`put_record`);
    // a test wants the new value, so it forces.
    git(
        repo,
        &[
            "push",
            "--quiet",
            "--force",
            "origin",
            &format!("{blob}:{name}"),
        ],
    );
}

fn delete_ref(repo: &Path, name: &str) {
    git(repo, &["push", "--quiet", "origin", &format!(":{name}")]);
}

/// The debt is enforced by the agent's own binary, so a stale client or a
/// hand-pushed claim ref can skip it. The server must enforce the same bound:
/// ONE accepted-but-unmerged close is the pipeline — the merge queue lands it
/// while the agent works on the next task — and only the SECOND outstanding
/// close is refused. A receipt for the task in hand is the repair path,
/// another agent's receipt is not this branch's business, and a receipt whose
/// close already reached `origin/main` is published work.
#[test]
fn a_second_owed_merge_is_refused_but_one_pipelines_at_the_server() {
    let e = Env::new("owed");
    for t in ["first", "second", "third"] {
        assert_eq!(
            e.susi(&["tasks", "add", t, "--accept", "scripts/fixture-check.sh"])
                .0,
            0
        );
    }
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "install rule"]); // task-only: exempt
    let base = e.head();

    // TEST accepted T-TEST-1 and never published it; it is now working
    // T-TEST-2. One merge in flight is the pipeline — nobody idles on a merge
    // queue they cannot hurry — so the push is fine.
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-2",
        &claim_json("T-TEST-2", "TEST", "main", &["work"]),
    );
    push_ref(
        &e.repo,
        "refs/closed/T-TEST-1",
        &receipt_json("T-TEST-1", "TEST"),
    );
    e.commit("a", "feat: a", Some("T-TEST-2"));
    let after_a = e.head();
    let (code, err) = e.check(&base);
    assert_eq!(code, 0, "one merge in flight is the pipeline: {err}");

    // T-TEST-2 was also accepted unpublished, and TEST moved to a third task.
    // Two outstanding closes is the pileup the receipts exist to refuse.
    push_ref(
        &e.repo,
        "refs/closed/T-TEST-2",
        &receipt_json("T-TEST-2", "TEST"),
    );
    delete_ref(&e.repo, "refs/claims/T-TEST-2");
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-3",
        &claim_json("T-TEST-3", "TEST", "main", &["work"]),
    );
    e.commit("b", "feat: b", Some("T-TEST-3"));
    let after_b = e.head();
    let (code, err) = e.check(&after_a);
    assert_eq!(
        code, 1,
        "the second owed merge must refuse the third task: {err}"
    );
    assert!(err.contains("owes the merge"), "{err}");
    assert!(
        err.contains("T-TEST-1") && err.contains("T-TEST-2"),
        "{err}"
    );

    // Going back to an owed task is the repair path, not piling on: the
    // receipt for the task in hand is exempt, leaving one owed merge.
    delete_ref(&e.repo, "refs/claims/T-TEST-3");
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-1",
        &claim_json("T-TEST-1", "TEST", "main", &["work"]),
    );
    e.commit("c", "fix: c", Some("T-TEST-1"));
    let after_c = e.head();
    let (code, err) = e.check(&after_b);
    assert_eq!(code, 0, "repairing an owed task must pass: {err}");

    // Another agent's receipt is not this branch's business: with T-TEST-1
    // owed by OTHER, TEST owes only T-TEST-2 — inside the bound.
    delete_ref(&e.repo, "refs/claims/T-TEST-1");
    delete_ref(&e.repo, "refs/closed/T-TEST-1");
    push_ref(
        &e.repo,
        "refs/closed/T-TEST-1",
        &receipt_json("T-TEST-1", "OTHER"),
    );
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-3",
        &claim_json("T-TEST-3", "TEST", "main", &["work"]),
    );
    e.commit("d", "feat: d", Some("T-TEST-3"));
    let (code, err) = e.check(&after_c);
    assert_eq!(
        code, 0,
        "another agent's debt must not refuse this push: {err}"
    );
    // The same receipts, once their closes are on origin/main, are published
    // work — even several of them.
    delete_ref(&e.repo, "refs/closed/T-TEST-1");
    push_ref(
        &e.repo,
        "refs/closed/T-TEST-1",
        &receipt_json("T-TEST-1", "TEST"),
    );
    std::fs::create_dir_all(e.repo.join(".agents/tasks/done")).unwrap();
    std::fs::write(
        e.repo.join(".agents/tasks/done/T-TEST-1.json"),
        "{\"id\":\"T-TEST-1\"}\n",
    )
    .unwrap();
    std::fs::write(
        e.repo.join(".agents/tasks/done/T-TEST-2.json"),
        "{\"id\":\"T-TEST-2\"}\n",
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "publish both closes"]); // task-only: exempt
    git(&e.repo, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&e.repo, &["fetch", "--quiet", "origin"]);
    let before = e.head();
    e.commit("e", "feat: e", Some("T-TEST-3"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "a published close is not debt: {err}");
}

/// One task at a time: the client refuses a second live claim for one agent,
/// but a hand-pushed claim ref can create one, and then two branches share one
/// worker. An expired claim is a lease the agent let lapse and moved on from —
/// the takeover path — so it must never read as a second task.
#[test]
fn an_agent_holding_two_live_claims_is_refused_at_the_server() {
    let e = Env::new("oneclaim");
    assert_eq!(
        e.susi(&[
            "tasks",
            "add",
            "first",
            "--accept",
            "scripts/fixture-check.sh"
        ])
        .0,
        0
    );
    assert_eq!(
        e.susi(&[
            "tasks",
            "add",
            "second",
            "--accept",
            "scripts/fixture-check.sh"
        ])
        .0,
        0
    );
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "install rule"]); // task-only: exempt
    let base = e.head();

    // Two live claims for TEST, which `susi tasks claim` would never create.
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-1",
        &claim_json("T-TEST-1", "TEST", "main", &["work"]),
    );
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-2",
        &claim_json("T-TEST-2", "TEST", "main", &["work"]),
    );
    e.commit("a", "feat: a", Some("T-TEST-2"));
    let (code, err) = e.check(&base);
    assert_eq!(code, 1, "two live claims must be refused: {err}");
    assert!(err.contains("holds 2 live claims"), "{err}");

    // The first claim lapsing is not a second task: it is how a crashed agent's
    // work is taken over, and the agent may have moved on legitimately.
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-1",
        &expired_claim_json("T-TEST-1", "TEST", "main", &["work"]),
    );
    let (code, err) = e.check(&base);
    assert_eq!(code, 0, "an expired claim is not a second task: {err}");

    // It is the *agent* that may hold only one claim, not the branch: another
    // agent's live claim must not refuse this push.
    delete_ref(&e.repo, "refs/claims/T-TEST-1");
    push_ref(
        &e.repo,
        "refs/claims/T-TEST-1",
        &claim_json("T-TEST-1", "OTHER", "other-branch", &["work"]),
    );
    let (code, err) = e.check(&base);
    assert_eq!(code, 0, "another agent's claim is not this agent's: {err}");
}

/// A task's acceptance is its definition of done, so the acceptance a task
/// DECLARES must be one the merged tree can re-run: a test, or a checker that
/// already exists on `origin/main`. A task that writes the script declaring it
/// done, or that verifies the host instead of the change, is refused — while
/// the queue that already exists stays closable (only added task files are
/// judged).
#[test]
fn a_declared_acceptance_that_cannot_be_re_run_is_refused() {
    let e = Env::new("declare");

    // A checker that already exists on main is a legitimate declaration, and so
    // is a named test.
    std::fs::create_dir_all(e.repo.join("scripts")).unwrap();
    std::fs::write(
        e.repo.join("scripts/committed-check.sh"),
        "#!/bin/sh\nexit 0\n",
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &["commit", "--quiet", "-m", "add a committed checker"],
    );
    git(&e.repo, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&e.repo, &["fetch", "--quiet", "origin"]);

    let (code, out, err) = e.susi(&[
        "tasks",
        "add",
        "good",
        "--accept",
        "cargo nextest run --locked -p susi-gawd -E test(ids_and_acceptance_commands_are_validated)",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("T-TEST-1"), "{out}");
    let (code, _, err) = e.susi(&[
        "tasks",
        "add",
        "also good",
        "--accept",
        "scripts/committed-check.sh",
    ]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        e.susi(&["tasks", "claim", "T-TEST-1", "--scope", "work", "--scope", "scripts"])
            .0,
        0
    );
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-workflow-compliance.sh"),
        e.repo.join("scripts/check-workflow-compliance.sh"),
    )
    .unwrap();
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "install the rule",
            "-m",
            "Task: T-TEST-1",
        ],
    );
    let base = e.head();
    let (code, err) = e.check(&base);
    assert_eq!(code, 0, "re-runnable declarations must pass: {err}");

    // A task may not write the script that declares it done.
    assert_eq!(
        e.susi(&[
            "tasks",
            "add",
            "self-certifying",
            "--accept",
            "scripts/not-a-checker.sh"
        ])
        .0,
        0,
        "the client still lets it through; CI is the gate"
    );
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "declare a task that writes its own checker",
        ],
    );
    let (code, err) = e.check(&base);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("not a checker on origin/main"), "{err}");

    // Nor a command that verifies the host rather than the change.
    assert_eq!(
        e.susi(&["tasks", "add", "lazy", "--accept", "cargo --version"])
            .0,
        0
    );
    git(&e.repo, &["add", "-A"]);
    git(
        &e.repo,
        &[
            "commit",
            "--quiet",
            "-m",
            "declare an acceptance that proves nothing",
        ],
    );
    let (code, err) = e.check(&base);
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("test-shaped"), "{err}");
}
