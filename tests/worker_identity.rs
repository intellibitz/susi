#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Every worker answers to an identity of its own — `<TOOL><WORKTREE-ID>` as
//! claim token and git author — never to the git login every agent shares
//! (`INTELLIBITZ`) or to `PRIMARY`. Two live failures drove this:
//!
//! * a worktree the desktop app creates inherits `PRIMARY` and commits as the
//!   login, so two sessions minted the same `T-PRIMARY-<n>`;
//! * `susi workflow start` named a new worker after `git config user.name`.

use std::path::{Path, PathBuf};
use std::process::Command;

struct World {
    root: PathBuf,
    primary: PathBuf,
}

/// The environment of a worker's shell: a private `$HOME`, no system git
/// config, and neither `SUSI_AGENT` nor an inherited git identity.
fn command(program: &str, dir: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(program);
    cmd.current_dir(dir)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("SUSI_HOME")
        .env_remove("SUSI_AGENT")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    cmd
}

fn finish(cmd: &mut Command) -> (i32, String, String) {
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

impl World {
    /// A bare server and a primary clone whose git login is `INTELLIBITZ` and
    /// whose worktree token is `PRIMARY`, as in the real clone.
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-wid-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("susi");
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&primary).unwrap();
        let w = Self { root, primary };
        w.git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        w.git(&w.primary, &["init", "--quiet", "-b", "main"]);
        w.git(&w.primary, &["config", "user.name", "INTELLIBITZ"]);
        w.git(&w.primary, &["config", "user.email", "login@localhost"]);
        w.git(
            &w.primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        w.git(
            &w.primary,
            &["commit", "--allow-empty", "--quiet", "-m", "init"],
        );
        w.git(&w.primary, &["push", "--quiet", "origin", "HEAD:main"]);
        w.git(&w.primary, &["config", "extensions.worktreeConfig", "true"]);
        w.git(
            &w.primary,
            &["config", "--worktree", "susi.agent", "PRIMARY"],
        );
        w
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn git(&self, dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = finish(command("git", dir, &self.home()).args(args));
        assert_eq!(code, 0, "git {args:?}: {err}");
        out
    }

    /// A linked worktree the way the desktop app makes one: it copies the
    /// creating tree's worktree config, so it answers to `PRIMARY`.
    fn inheriting(&self, branch: &str) -> PathBuf {
        let dir = self
            .root
            .join(branch.rsplit('/').next().unwrap())
            .to_path_buf();
        self.git(
            &self.primary,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                branch,
                dir.to_str().unwrap(),
            ],
        );
        self.git(&dir, &["config", "--worktree", "susi.agent", "PRIMARY"]);
        dir
    }

    fn susi(&self, dir: &Path, args: &[&str]) -> (i32, String, String) {
        finish(
            command(env!("CARGO_BIN_EXE_susi"), dir, &self.home())
                .args(args)
                .env("SUSI_PRIMARY_WATCH_DISABLE", "1"),
        )
    }

    fn susi_as(&self, agent: &str, dir: &Path, args: &[&str]) -> (i32, String, String) {
        finish(
            command(env!("CARGO_BIN_EXE_susi"), dir, &self.home())
                .args(args)
                .env("SUSI_AGENT", agent),
        )
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// `git config --worktree --get <key>` of a worktree.
fn own(w: &World, dir: &Path, key: &str) -> String {
    let (_, out, _) =
        finish(command("git", dir, &w.home()).args(["config", "--worktree", "--get", key]));
    out
}

#[test]
fn worker_identity_a_worktree_that_inherited_primary_is_given_its_own() {
    let w = World::new("inherit");
    let wt = w.inheriting("claude/sweet-khorana-5d3fc4");

    // The session-start hook runs this; it must fix the identity, not just
    // report it, so the first claim and the first commit are already the worker's.
    let (_, out, err) = w.susi(&wt, &["workflow", "check"]);
    assert!(
        out.contains("(agent CLAUDESWEETKHORANA5D3FC4)"),
        "out={out} err={err}"
    );
    assert!(
        out.contains("✅ own identity") && out.contains("assigned CLAUDESWEETKHORANA5D3FC4"),
        "{out}"
    );
    assert_eq!(own(&w, &wt, "susi.agent"), "CLAUDESWEETKHORANA5D3FC4");
    // The git author is the worker too, not the shared login.
    assert_eq!(own(&w, &wt, "user.name"), "CLAUDESWEETKHORANA5D3FC4");
    assert_eq!(
        own(&w, &wt, "user.email"),
        "claudesweetkhorana5d3fc4@localhost"
    );
    // The primary checkout keeps its own, and is not touched.
    assert_eq!(own(&w, &w.primary, "susi.agent"), "PRIMARY");

    // Asked again, it is already its own and says so.
    let (code, out, err) = w.susi(&wt, &["workflow", "identity"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("already this worktree's own"), "{out}");
}

#[test]
fn worker_identity_sibling_worktrees_mint_distinct_task_ids_and_commit_as_themselves() {
    let w = World::new("siblings");
    let a = w.inheriting("claude/charming-curie-cbbe7b");
    let b = w.inheriting("claude/sweet-khorana-5d3fc4");
    for wt in [&a, &b] {
        let (code, _, err) = w.susi(wt, &["workflow", "identity"]);
        assert_eq!(code, 0, "{err}");
    }

    let add = |wt: &Path| {
        let (code, out, err) = w.susi(
            wt,
            &["tasks", "add", "prove it", "--accept", "cargo --version"],
        );
        assert_eq!(code, 0, "{err}");
        out
    };
    // Both used to mint T-PRIMARY-1: one id, two tasks, one claim between them.
    assert!(add(&a).contains("added T-CLAUDECHARMINGCURIECBBE7B-1"));
    assert!(add(&b).contains("added T-CLAUDESWEETKHORANA5D3FC4-1"));

    // A commit made there is authored by the worker, not by `INTELLIBITZ`.
    let (code, _, err) = finish(command("git", &a, &w.home()).args([
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "work",
    ]));
    assert_eq!(code, 0, "{err}");
    assert_eq!(
        w.git(&a, &["log", "-1", "--format=%an <%ae>"]),
        "CLAUDECHARMINGCURIECBBE7B <claudecharmingcuriecbbe7b@localhost>"
    );
}

#[test]
fn worker_identity_is_not_swapped_under_a_live_claim() {
    let w = World::new("claim");
    let wt = w.inheriting("claude/held-1a2b3c");
    // The worker claimed under the borrowed token before anyone noticed.
    let (code, _, err) = w.susi_as(
        "PRIMARY",
        &wt,
        &["tasks", "add", "prove it", "--accept", "cargo --version"],
    );
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = w.susi_as(
        "PRIMARY",
        &wt,
        &["tasks", "claim", "T-PRIMARY-1", "--scope", "work"],
    );
    assert_eq!(code, 0, "{err}");

    // A claim travels with its token: swapping it would strand the task.
    let (code, _, err) = w.susi(&wt, &["workflow", "identity"]);
    assert_ne!(code, 0);
    assert!(
        err.contains("T-PRIMARY-1") && err.contains("susi tasks release T-PRIMARY-1"),
        "{err}"
    );
    assert_eq!(own(&w, &wt, "susi.agent"), "PRIMARY");
    let (_, out, _) = w.susi(&wt, &["workflow", "check"]);
    assert!(out.contains("❌ own identity"), "{out}");

    // The documented path: release, then the identity is replaced.
    let (code, _, err) = w.susi_as("PRIMARY", &wt, &["tasks", "release", "T-PRIMARY-1"]);
    assert_eq!(code, 0, "{err}");
    let (code, out, err) = w.susi(&wt, &["workflow", "identity"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("CLAUDEHELD1A2B3C"), "{out}");
}

#[test]
fn worker_identity_start_never_names_the_worker_after_the_git_login() {
    let w = World::new("start");
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts");
    std::fs::create_dir_all(w.primary.join("scripts")).unwrap();
    for s in ["susi-worktree.sh", "park-primary.sh", "setup-dev.sh"] {
        std::fs::copy(scripts.join(s), w.primary.join("scripts").join(s)).unwrap();
    }
    w.git(&w.primary, &["add", "-A"]);
    w.git(&w.primary, &["commit", "--quiet", "-m", "scripts"]);
    w.git(&w.primary, &["push", "--quiet", "origin", "HEAD:main"]);
    let script = w.primary.join("scripts/susi-worktree.sh");

    // Inside a tool's own process, the tool names the worker.
    let wrapper = w.root.join("codex");
    std::fs::write(
        &wrapper,
        format!("#!/bin/bash\n\"{}\" \"$@\"\n", script.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (code, out, err) = finish(
        command(wrapper.to_str().unwrap(), &w.primary, &w.home())
            .env("SUSI_PRIMARY_WATCH_DISABLE", "1"),
    );
    assert_eq!(code, 0, "{err}");
    let dest = PathBuf::from(out.lines().last().unwrap().strip_prefix("cd ").unwrap());
    let branch = w.git(&dest, &["symbolic-ref", "--short", "HEAD"]);
    assert!(branch.starts_with("codex-"), "{branch}");
    let token = own(&w, &dest, "susi.agent");
    assert!(token.starts_with("CODEX2"), "{token}");
    assert_eq!(own(&w, &dest, "user.name"), token);

    // Run by nothing that names a tool (under CI, a bare shell), with only the
    // git login to go on, the worker is still not the login.
    let (code, out, err) = finish(
        command(script.to_str().unwrap(), &w.primary, &w.home())
            .env("SUSI_PRIMARY_WATCH_DISABLE", "1"),
    );
    assert_eq!(code, 0, "{err}");
    let dest = PathBuf::from(out.lines().last().unwrap().strip_prefix("cd ").unwrap());
    let token = own(&w, &dest, "susi.agent");
    assert!(!token.contains("INTELLIBITZ"), "{token}");
    assert_ne!(own(&w, &dest, "user.name"), "INTELLIBITZ");
    let branch = w.git(&dest, &["symbolic-ref", "--short", "HEAD"]);
    assert!(!branch.starts_with("intellibitz"), "{branch}");
}

#[test]
fn worker_identity_commit_hook_rejects_a_login_token_and_login_authored_commits() {
    let w = World::new("hook");
    let wt = w.inheriting("claude/hooked-9f8e7d");
    let hook = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-worker-identity.sh");
    let run = || finish(&mut command(hook.to_str().unwrap(), &wt, &w.home()));

    // The inherited token is shared with the primary checkout.
    w.git(
        &wt,
        &["config", "--worktree", "susi.agent", "CLAUDEHOOKED9F8E7D"],
    );
    // Unique token, but commits would still be authored by the login.
    let (code, _, err) = run();
    assert_eq!(code, 1, "{err}");
    assert!(err.contains("authored by the shared git login"), "{err}");

    w.git(
        &wt,
        &["config", "--worktree", "user.name", "CLAUDEHOOKED9F8E7D"],
    );
    let (code, out, err) = run();
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("this worktree's token alone"), "{out}");

    // A token that *is* the login, or a role, is not a worker.
    for borrowed in ["IntelliBitz", "main"] {
        w.git(&wt, &["config", "--worktree", "susi.agent", borrowed]);
        w.git(&wt, &["config", "--worktree", "user.name", borrowed]);
        let (code, _, err) = run();
        assert_eq!(code, 1, "{borrowed}: {err}");
        assert!(
            err.contains("shared git login or a role") && err.contains("susi workflow identity"),
            "{err}"
        );
    }
}
