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
        git(&repo, &["commit", "--allow-empty", "-m", "init", "--quiet"]);
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
    let (code, out, err) = e.susi(&["tasks", "add", "first", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "second", "--accept", "cargo --version"]);
    assert_eq!(code, 0, "{err}");
    git(&e.repo, &["add", "-A"]);
    git(&e.repo, &["commit", "--quiet", "-m", "add second task"]);
    let before = e.head();
    e.commit("d", "feat: d", Some("T-TEST-2"));
    let (code, err) = e.check(&before);
    assert_eq!(code, 1);
    assert!(err.contains("nobody holds a claim"), "{err}");
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-1"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&[
        "tasks", "claim", "T-TEST-2", "--scope", "work", "--scope", "scripts",
    ]);
    assert_eq!(code, 0, "{err}");
    let (code, err) = e.check(&before);
    assert_eq!(code, 0, "{err}");

    // 5. Work then close on the same branch passes: the task was open at the
    //    work commit and the branch closed it (ownership remains until merge).
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-2"]);
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

    // 5b. Once closed, a task cannot be cited again (unrelated work must not
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "release", "T-TEST-1"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "t", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "target", "--accept", "cargo --version"]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = e.susi(&["tasks", "add", "rule", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
    let (code, _, err) = e.susi(&["tasks", "add", "one", "--accept", "cargo --version"]);
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
