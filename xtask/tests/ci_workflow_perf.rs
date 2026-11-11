#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The local workflow's performance invariants (hooks, gate scripts, cargo
//! config and the publication path). GitHub Actions workflows were removed
//! (CEO decision 2026-11-06); `no_github_actions_workflow_returns` keeps them
//! out.

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

#[cfg(unix)]
fn script_command(script: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("bash");
    command.arg(script);
    command
}

#[cfg(unix)]
fn set_script_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    {
        let git_bin = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .find(|path| path.join("bash.exe").is_file())
            .map(|path| path.join("chmod.exe"));
        if let Some(chmod) = git_bin {
            let _ = std::process::Command::new(chmod)
                .arg("+x")
                .arg(path)
                .status();
        }
    }
}

#[cfg(unix)]
fn path_with_prefix(prefix: &Path, include_host: bool) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        let git_bin = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .find(|path| path.join("cygpath.exe").is_file());
        let cygpath = git_bin.as_ref().map(|path| path.join("cygpath.exe"));
        let to_posix = |path: &Path| -> String {
            if let Some(cygpath) = &cygpath {
                if let Ok(output) = std::process::Command::new(cygpath)
                    .arg("-u")
                    .arg(path)
                    .output()
                {
                    if output.status.success() {
                        return String::from_utf8_lossy(&output.stdout).trim().to_owned();
                    }
                }
            }
            path.to_string_lossy().replace('\\', "/")
        };
        let mut paths = vec![to_posix(prefix)];
        if include_host {
            if let Some(path) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&path).map(|part| to_posix(&part)));
            }
        } else if let Some(git_bin) = git_bin {
            paths.push(to_posix(&git_bin));
        }
        std::ffi::OsString::from(paths.join(":"))
    }
    #[cfg(not(windows))]
    {
        let mut paths = vec![prefix.to_path_buf()];
        if include_host {
            if let Some(path) = std::env::var_os("PATH") {
                paths.extend(std::env::split_paths(&path));
            }
        } else {
            // The fakes and the shell alone: /usr/bin would bring the host's
            // own tools along (a cloud container ships /usr/bin/cmake), and a
            // runner that lacks one could not be staged.
            let shell = prefix.join("shell");
            std::fs::create_dir_all(&shell).unwrap();
            let bash = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| dir.join("bash"))
                .find(|path| path.is_file())
                .expect("bash on the host's PATH");
            let link = shell.join("bash");
            if !link.exists() {
                std::os::unix::fs::symlink(bash, &link).unwrap();
            }
            paths.push(shell);
        }
        std::env::join_paths(paths).unwrap()
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Zero GitHub Actions (CEO decisions 2026-11-05 and 2026-11-06): the gate is
/// `cargo local-workflow check`/`ship` on the current host, so no workflow
/// definition may return to the repository.
#[test]
fn no_github_actions_workflow_returns() {
    let dir = root().join(".github/workflows");
    assert!(
        !dir.exists(),
        "{} exists: GitHub Actions workflows were removed; use cargo local-workflow",
        dir.display()
    );
}

/// The `[profile.<name>]` table of `Cargo.toml`: one `key = value` per row, comments
/// dropped, until the next table.
fn profile_table(manifest: &str, name: &str) -> Vec<(String, String)> {
    let header = format!("[profile.{name}]");
    let mut rows = Vec::new();
    let mut inside = false;
    for line in manifest.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            inside = line == header;
            continue;
        }
        if inside {
            if let Some((key, value)) = line.split_once('=') {
                rows.push((key.trim().to_string(), value.trim().to_string()));
            }
        }
    }
    assert!(!rows.is_empty(), "Cargo.toml has no {header} table");
    rows
}

fn profile_setting<'a>(rows: &'a [(String, String)], key: &str) -> Option<&'a str> {
    rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// Release builds are optimized builds: `cargo local-build production` and the
/// installer's `--release` build what ships, so the release profile optimizes,
/// strips, aborts on panic and carries no incremental cache, and `dist` keeps
/// the max-opt settings for published binaries. 58062432f had set the release
/// profile to opt-level 0 for faster iteration, which the dev and test profiles
/// provide instead; a release built that way is not a release.
#[test]
fn release_profiles_stay_optimized() {
    let manifest = read("Cargo.toml");
    let release = profile_table(&manifest, "release");
    assert_eq!(
        profile_setting(&release, "opt-level"),
        Some("3"),
        "{release:?}"
    );
    assert_eq!(profile_setting(&release, "panic"), Some("\"abort\""));
    assert_eq!(profile_setting(&release, "strip"), Some("true"));
    assert_eq!(profile_setting(&release, "incremental"), Some("false"));
    assert!(
        matches!(
            profile_setting(&release, "lto"),
            Some("\"thin\"" | "\"fat\"" | "true")
        ),
        "{release:?}"
    );
    let dist = profile_table(&manifest, "dist");
    assert_eq!(profile_setting(&dist, "inherits"), Some("\"release\""));
    assert_eq!(profile_setting(&dist, "lto"), Some("\"fat\""));
    assert_eq!(profile_setting(&dist, "codegen-units"), Some("1"));
}

/// A shell function's definition as `text` writes it: `name() {` through the
/// `}` that closes it at the start of a line.
fn shell_function(text: &str, name: &str) -> String {
    let start = text
        .find(&format!("\n{name}() {{\n"))
        .unwrap_or_else(|| panic!("no `{name}()` is defined"))
        + 1;
    let len = text[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("`{name}()` never closes"))
        + 3;
    text[start..start + len].to_string()
}

/// Worktrees shared one target, and cargo names a workspace crate's build by
/// its path relative to the workspace, so a crate another worktree built read
/// as fresh and was linked into this branch's build. `scripts/target-guard.sh`
/// narrowed that and could not close it; every worktree now builds into a
/// target of its own, seeded from a warm snapshot (`scripts/seed-target.sh`),
/// where plain cargo is correct. So no gate, hook or close depends on the
/// guard any more: it is a pass-through kept one release for the installed
/// susi and older branches, and the worktree paths seed instead of linking.
/// For that one release the new binary and hooks still meet branches cut
/// before the seeding, which carry the real guard and no seeder and build in
/// the shared target: `tasks close` and the pre-push hook run such a branch's
/// own guard, only when it has no `scripts/seed-target.sh` (pinned by
/// behaviour in tasks.rs and tests/worktree_target_dir.rs), and the staged
/// `finish` copy still drops an outer guard's hold. Nowhere else.
#[test]
fn no_gate_or_hook_depends_on_the_retired_target_guard() {
    let guard = "target-guard";
    // A file's code lines: no comment, and no Rust tests (which may name the
    // retired paths to prove them gone).
    let code_of = |path: &str| -> Vec<String> {
        let text = read(path).replace("\r\n", "\n");
        let text = text.split("#[cfg(test)]").next().unwrap_or_default();
        text.lines()
            .filter(|line| {
                let line = line.trim_start();
                !line.starts_with('#') && !line.starts_with("//")
            })
            .map(str::to_string)
            .collect()
    };
    for path in [
        "scripts/parallel-workflow.sh",
        ".githooks/pre-push",
        "crates/susi-harness/src/adapter.rs",
        "scripts/workflow-session-start.sh",
        "scripts/susi-worktree.sh",
        ".githooks/post-merge",
        ".githooks/post-commit",
        ".githooks/post-checkout",
    ] {
        let code = code_of(path);
        for (at, line) in code.iter().enumerate() {
            assert!(
                !line.contains("link-shared-target"),
                "{path} still links a shared target: {line}"
            );
            if !line.contains(guard) && !line.contains("SUSI_TARGET_GUARD_HELD") {
                continue;
            }
            // The one-release fallbacks for a branch that predates the
            // seeding, and nothing else.
            let allowed = match path {
                ".githooks/pre-push" => {
                    // Only inside the branch taken when there is no seeder.
                    let seeds = code.iter().position(|l| {
                        l.contains("if [ -f \"$top/scripts/seed-target.sh\" ]; then")
                    });
                    let fallback = code.iter().position(|l| {
                        l.contains("elif [ -x \"$top/scripts/target-guard.sh\" ]; then")
                    });
                    let end = fallback.and_then(|f| {
                        code[f..]
                            .iter()
                            .position(|l| l.trim() == "fi")
                            .map(|e| f + e)
                    });
                    matches!((seeds, fallback, end), (Some(s), Some(f), Some(e)) if s < f && f <= at && at < e)
                }
                _ => false,
            };
            assert!(allowed, "{path} still runs the retired guard: {line}");
        }
    }
    // The gates run plain cargo: the gate's clippy argv, run as `cargo clippy "$@"`.
    let gate = read("scripts/parallel-workflow.sh");
    assert!(gate.contains("clippy_args=(--locked --all-targets $lsel -- -D warnings)"));
    assert!(gate.contains("timed_step clippy cargo clippy \"$@\""));
    let hook = read(".githooks/pre-push");
    assert!(!hook.contains("cargo clippy"));
    // Worktrees are seeded, by the scripts and by `workflow start` alike;
    // scripts/susi-worktree.sh is the stub for `workflow start` (Susi
    // Commercial Apps' dev workflow since the lean-core move), which seeds in
    // process and is tested there.
    assert!(
        read("scripts/workflow-session-start.sh").contains("seed-target.sh"),
        "scripts/workflow-session-start.sh must seed the worktree's own target"
    );
    // The harness adapter gives a new worktree its own native target, never
    // a shell seeder; its unit tests pin that. They moved from the end of
    // `adapter.rs` to `src/tests/adapter.rs` (192b53d27, line ceiling), so
    // the guard reads the test, by name, where it lives.
    let adapter_tests = read("crates/susi-harness/src/tests/adapter.rs");
    assert!(
        adapter_tests
            .contains("fn start_worktree_uses_native_private_target_without_shell_seeding()")
            && adapter_tests.contains("scripts/seed-target.sh"),
        "crates/susi-harness/src/tests/adapter.rs must pin the adapter's own native target"
    );
    assert!(
        read("scripts/susi-worktree.sh").contains("exec \"$bin\" workflow start \"$@\""),
        "scripts/susi-worktree.sh is the stub for `susi workflow start`"
    );
    // The shim only passes through: no lock, no stamps, no fingerprints.
    let shim = read("scripts/target-guard.sh");
    for gone in ["mkdir", "rm -rf", ".fingerprint", "stamps"] {
        assert!(
            !shim
                .lines()
                .filter(|line| !line.trim_start().starts_with('#'))
                .any(|line| line.contains(gone)),
            "the retired guard must not `{gone}`"
        );
    }
    // Every path that runs cargo for an agent first gives a worktree still
    // linked to the old shared target one of its own: a worktree whose
    // session began before seeding keeps its link until something converts
    // it, and merging origin/main hands it these paths. A sibling's build in
    // the shared target could otherwise stand in for this branch's.
    let at = |text: &str, needle: &str, what: &str| {
        text.find(needle)
            .unwrap_or_else(|| panic!("{what}: no `{needle}`"))
    };
    let gate = gate.replace("\r\n", "\n");
    let gate_fn = shell_function(&gate, "gate");
    assert!(
        at(&gate_fn, "seed_own_target", "the gate")
            < at(&gate_fn, "gate_lint_and_test", "the gate"),
        "the local test and lint gate must seed before compiling"
    );
    assert!(
        at(&gate_fn, "SUSI_BRANCH_CI_HANDOFF=1", "the gate")
            < at(&gate_fn, "seed_own_target", "the gate"),
        "the remote handoff should not wait for a target copy it does not use"
    );
    let sync_fn = shell_function(&gate, "sync");
    assert!(
        at(&sync_fn, "git merge --no-edit origin/main", "sync")
            < at(&sync_fn, "seed_own_target", "sync"),
        "sync must seed after the merge that may bring the seeding"
    );
    assert!(gate.contains("\"${BASH:-bash}\" \"$root/scripts/seed-target.sh\" \"$root\""));

    let shim_code: Vec<&str> = shim
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect();
    assert!(
        shim_code
            .iter()
            .any(|l| l.contains("[ -L \"$1/target\" ] && return 0"))
            && shim_code
                .iter()
                .any(|l| l.contains("wants_seed \"$root\"") && l.contains("[ -f \"$seeder\" ]"))
            && shim_code
                .iter()
                .any(|l| l.contains("\"$shell\" \"$seeder\" \"$root\"")),
        "the retired guard converts a linked target before the cargo its callers run"
    );
    // The installed susi (v0.21.23) runs both shims on Windows by a path Rust
    // joined (`F:/w\scripts\target-guard.sh`): they find seed-target.sh beside
    // them only once $0's backslashes are slashes, and splitting $0 on `/`
    // alone found the wrong directory and seeded nothing.
    for path in ["scripts/target-guard.sh", "scripts/link-shared-target.sh"] {
        let text = read(path);
        let code: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .collect();
        assert!(
            code.iter()
                .any(|l| l.contains("self=$(printf '%s\\n' \"$0\" | tr '\\\\' /)")),
            "{path} must make $0's backslashes slashes before taking its directory"
        );
        assert!(
            !code.iter().any(|l| l.contains("${0%/*}")),
            "{path} splits $0 on `/` alone"
        );
    }
    // A merge that brings the seeding converts a linked target at once; so
    // does the commit that concludes one that stopped on conflicts (Git runs
    // no post-merge for it), and `git worktree add` (before v0.21.23's
    // `workflow start` can link a target/ that does not exist yet); in the
    // background, so none of them waits for the copy.
    for hook in [
        ".githooks/post-merge",
        ".githooks/post-commit",
        ".githooks/post-checkout",
    ] {
        let text = read(hook);
        assert!(
            text.contains("[ -L \"$top/target\" ]")
                && text.contains("scripts/seed-target.sh\" --background \"$top\""),
            "{hook} must seed a linked target in the background"
        );
    }
    assert!(read(".githooks/post-checkout").contains("[ ! -e \"$top/target\" ]"));
    // The cloud prebuild is plain cargo: the clone's target is its own.
    let session = read("scripts/workflow-session-start.sh");
    assert!(session.contains("exec cargo build -q"));
    // The seeder runs under the bash already running: on a Windows PATH a
    // bare `bash` can be WSL's.
    for (path, needle) in [
        (
            // The stub execs `workflow start` (susi-commercial), which seeds
            // in the background itself.
            "scripts/susi-worktree.sh",
            "exec \"$bin\" workflow start \"$@\"",
        ),
        (
            "scripts/workflow-session-start.sh",
            "\"${BASH:-bash}\" scripts/seed-target.sh --background",
        ),
    ] {
        assert!(read(path).contains(needle), "{path}: no `{needle}`");
    }
}

/// Routine publication is transport safety only; qualification is separate.
#[test]
fn pre_push_uses_only_native_transport_policy() {
    let hook = read(".githooks/pre-push");
    assert!(hook.contains("exec push-policy"));
    for forbidden in [
        "workflow-guard",
        "sync-primary",
        "clippy",
        "ci-branch-handoff",
        "SUSI_RELEASE_PUSH",
        "susi-commercial",
    ] {
        assert!(
            !hook.contains(forbidden),
            "legacy push prerequisite: {forbidden}"
        );
    }
}
/// Cargo resolves a relative `linker` in `.cargo/config.toml` to an absolute path
/// inside the current worktree and passes it as `-C linker=...`, and that path is
/// part of every unit's fingerprint: so no two worktrees could share a compiled
/// crate (registry crates included) and each overwrote the other's artifacts in
/// the shared target dir. Measured: a fresh worktree compiled 716 crates in 656 s;
/// with a path-independent config, one created after a warm build compiled 81 in
/// 176 s. Worktrees now get a target of their own seeded from a snapshot built in
/// another checkout (`scripts/seed-target.sh`), and the snapshot's registry
/// crates are reused only while no unit's fingerprint names a worktree path: a
/// repo-relative linker would make every seeded target rebuild cold.
#[test]
fn the_cargo_config_does_not_hash_a_worktree_path_into_every_unit() {
    let config = read(".cargo/config.toml");
    let mut section = String::new();
    for raw in config.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_string();
            continue;
        }
        // The x86_64 target is what developers build on; aarch64 Linux is a CI
        // release leg with one checkout, so a wrapper there shares nothing.
        if section != "target.x86_64-unknown-linux-gnu" {
            continue;
        }
        if let Some(value) = line.strip_prefix("linker") {
            let value = value
                .trim_start_matches([' ', '='])
                .trim()
                .trim_matches('"');
            assert!(
                value.starts_with('/'),
                "`{line}` is a repo-relative linker: cargo makes it an absolute path inside each \
                 worktree, hashes it into every unit, and no worktree can reuse another's \
                 compiled crates (a fresh one rebuilds ~716 crates, 11 min instead of 3)"
            );
        }
        assert!(
            !line.contains("linker-features=-lld"),
            "`{line}`: opting out of rustc's bundled lld only made sense to hand linking to the \
             wrapper; without one it silently falls back to GNU ld, which is slower than both"
        );
    }
}

/// A scratch directory of fake executables ahead of the real PATH, plus a log
/// every fake appends its argv to. The scripts under test run for real; only the
/// things that would touch the machine (`apt-get`, `curl`, `sudo`) are faked.
#[cfg(unix)]
struct Fakes {
    dir: PathBuf,
}

#[cfg(unix)]
impl Fakes {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("susi-ci-fakes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("log"), "").unwrap();
        Self { dir }
    }

    fn fake(&self, name: &str, body: &str) {
        let path = self.dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        set_script_executable(&path);
    }
}

#[cfg(unix)]
impl Drop for Fakes {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
#[cfg(unix)]
fn xtask_package_changes_are_scoped_to_xtask() {
    let dir = std::env::temp_dir().join(format!("susi-changed-crates-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let write = |rel: &str, body: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        set_script_executable(&path);
    };
    let commit = |message: &str| {
        git(&["add", "-A", "."]);
        git(&["commit", "--quiet", "-m", message]);
        git(&["rev-parse", "HEAD"])
    };
    git(&["init", "--quiet", "-b", "main", "."]);
    for script in ["ci-changed-crates.sh", "posix-compat.sh"] {
        write(
            &format!("scripts/{script}"),
            &read(&format!("scripts/{script}")),
        );
    }
    write(
        "Cargo.toml",
        "[workspace]\nmembers = [\".\", \"xtask\"]\nresolver = \"2\"\n\
         [package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write("src/lib.rs", "pub fn f() {}\n");
    write("tests/ci_workflow_perf.rs", "#[test] fn contract() {}\n");
    write(
        "xtask/Cargo.toml",
        "[package]\nname = \"xtask\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write("xtask/src/lib.rs", "pub fn f() {}\n");
    let base = commit("main");
    git(&["branch", "origin/main", &base]);
    std::fs::remove_file(dir.join("tests/ci_workflow_perf.rs")).unwrap();
    write(
        "xtask/tests/ci_workflow_perf.rs",
        "#[test] fn contract() {}\n",
    );
    write(".agents/tasks/done/T-X-1.json", "{}\n");
    commit("task and its lightweight acceptance test");

    let out = script_command(&dir.join("scripts/ci-changed-crates.sh"))
        .arg("--dependents")
        .current_dir(&dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ci-changed-crates.sh failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "xtask",
        "the task record is inert and the test file maps to the xtask package"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn local_workspace_lockfile_scope_preserves_full_gate_for_external_changes() {
    let dir = std::env::temp_dir().join(format!("susi-lockfile-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let write = |rel: &str, body: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    let commit = |message: &str| {
        git(&["add", "-A", "."]);
        git(&["commit", "--quiet", "-m", message]);
        git(&["rev-parse", "HEAD"])
    };
    git(&["init", "--quiet", "-b", "main", "."]);
    for script in ["ci-changed-crates.sh", "posix-compat.sh"] {
        let path = dir.join("scripts").join(script);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, read(&format!("scripts/{script}"))).unwrap();
        set_script_executable(&path);
    }
    write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/a\"]\nresolver = \"2\"\n",
    );
    write(
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write("crates/a/src/lib.rs", "pub fn f() {}\n");
    write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n\n\
         [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n\
         source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
         checksum = \"1111111111111111111111111111111111111111111111111111111111111111\"\n",
    );
    let base = commit("base lockfile");
    git(&["branch", "origin/main", &base]);

    write(
        "crates/a/Cargo.toml",
        "[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\
         [dependencies]\nserde = \"1.0.0\"\n",
    );
    write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n\
         dependencies = [\"serde\"]\n\n[[package]]\nname = \"serde\"\n\
         version = \"1.0.0\"\n\
         source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
         checksum = \"1111111111111111111111111111111111111111111111111111111111111111\"\n",
    );
    commit("add a local package dependency");

    let run = || {
        let out = script_command(&dir.join("scripts/ci-changed-crates.sh"))
            .arg("--dependents")
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ci-changed-crates.sh failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    assert_eq!(
        run(),
        "a",
        "a path-package lock row should be covered by its changed manifest"
    );

    write(
        "Cargo.lock",
        "version = 4\n\n[[package]]\nname = \"a\"\nversion = \"0.1.0\"\n\
         dependencies = [\"serde\"]\n\n[[package]]\nname = \"serde\"\n\
         version = \"1.0.1\"\n\
         source = \"registry+https://github.com/rust-lang/crates.io-index\"\n\
         checksum = \"2222222222222222222222222222222222222222222222222222222222222222\"\n",
    );
    commit("change an external package resolution");
    assert_eq!(
        run(),
        "ALL",
        "a registry package version or checksum change must retain the workspace-wide gate"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn roadmap_verdict_batch_searches_evidence_once() {
    let source = read("scripts/check-roadmap-verdict.sh");
    let check_all = source
        .split("def check_all():")
        .nth(1)
        .expect("roadmap verdict checker defines check_all");
    assert!(
        check_all.contains("batch_evidence_hits(identifiers)")
            && check_all.contains("--batch")
            && check_all.contains("(\"scripts/check-reachability.sh\", *item[\"check\"])")
            && check_all.contains("batched_checks=batched_checks"),
        "check_all must batch identifier evidence and reachability checks before validating records"
    );
    let out = script_command(&root().join("scripts/check-roadmap-verdict.sh"))
        .arg("--self-test")
        .current_dir(root())
        .output()
        .expect("shell runs the roadmap-verdict self-test");
    assert!(
        out.status.success(),
        "roadmap-verdict self-test failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("batch evidence index"),
        "the self-test exercises the batched evidence index"
    );
    let reachability = read("scripts/check-reachability.sh");
    assert!(
        reachability.contains("def call_sites_many(")
            && reachability.contains("indexed = call_sites_many("),
        "reachability checks must share a single production-source traversal"
    );
    let out = script_command(&root().join("scripts/check-reachability.sh"))
        .arg("--self-test")
        .current_dir(root())
        .output()
        .expect("shell runs the reachability self-test");
    assert!(
        out.status.success(),
        "reachability self-test failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("batch checks share one source traversal"),
        "the self-test proves the batched checker reads source once"
    );
}

#[test]
#[cfg(unix)]
fn finish_push_recovers_only_from_an_advanced_main() {
    let f = Fakes::new("finish-push-recovery");
    let workflow = read("scripts/parallel-workflow.sh").replace("\r\n", "\n");
    let mut script = String::from("set -euo pipefail\n");
    script.push_str(&shell_function(&workflow, "push_closed_branch"));
    script.push_str(r#"
git() {
    case "$1" in
    rev-parse) IFS= read -r head < head; printf '%s\n' "$head" ;;
    push)
        IFS= read -r count < count; count=$((count + 1)); printf '%s\n' "$count" > count
        echo push >> log
        if [ "$MODE" = success ] || { [ "$MODE" = advance ] && [ "$count" -gt 1 ]; }; then return 0; fi
        [ "$MODE" != changed ] || echo changed > head
        echo 'original push rejection' >&2; return 1 ;;
    merge-base) [ "$MODE" = network ] ;;
    merge) echo merge >> log; [ "$MODE" != conflict ] || return 1; echo merged > head ;;
    *) return 9 ;;
    esac
}
gate() { echo gate >> log; return 1; }
push_closed_branch work
"#);
    let path = f.dir.join("push-check.sh");
    std::fs::write(&path, script).unwrap();
    for (mode, regate, success, expected) in [
        ("success", "0", true, "push\n"),
        ("advance", "0", true, "push\nmerge\npush\n"),
        ("network", "0", false, "push\n"),
        ("changed", "0", false, "push\n"),
        ("conflict", "0", false, "push\nmerge\n"),
        ("busy", "0", false, "push\nmerge\npush\n"),
        ("advance", "1", false, "push\nmerge\ngate\n"),
    ] {
        std::fs::write(f.dir.join("head"), "original\n").unwrap();
        std::fs::write(f.dir.join("count"), "0\n").unwrap();
        std::fs::write(f.dir.join("log"), "").unwrap();
        let out = script_command(&path)
            .current_dir(&f.dir)
            .env("MODE", mode)
            .env("SUSI_FINISH_REGATE", regate)
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            success,
            "{mode}/{regate}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(f.dir.join("log")).unwrap(),
            expected,
            "{mode}/{regate}"
        );
        if !success && regate == "0" {
            assert!(String::from_utf8_lossy(&out.stderr).contains("original push rejection"));
        }
    }
}

#[test]
#[cfg(unix)]
fn finish_atomic_publication_falls_back_only_when_explicitly_requested() {
    let f = Fakes::new("finish-atomic-publication");
    let workflow = read("scripts/parallel-workflow.sh").replace("\r\n", "\n");
    std::fs::create_dir_all(f.dir.join("scripts")).unwrap();
    let mock_publish = f.dir.join("scripts/publish-task-branch.sh");
    std::fs::write(
        &mock_publish,
        "#!/bin/sh\necho atomic-push >> log\nexit \"$PUBLISH_STATUS\"\n",
    )
    .unwrap();
    set_script_executable(&mock_publish);
    let mut script = String::from("set -euo pipefail\n");
    script.push_str(&shell_function(&workflow, "push_closed_branch"));
    script.push_str(
        r#"
root=$PWD
task=T-TEST-1
wait=$WAIT_MODE
git() {
    case "$1" in
    rev-parse) printf '%s\n' original ;;
    push) echo separate-push >> log ;;
    merge-base) return 0 ;;
    *) return 9 ;;
    esac
}
push_closed_branch work
printf 'released=%s\n' "$released_with_push" >> log
"#,
    );
    let path = f.dir.join("publication-check.sh");
    std::fs::write(&path, script).unwrap();
    for (status, wait, success, expected) in [
        ("0", "0", true, "atomic-push\nreleased=1\n"),
        ("1", "0", false, "atomic-push\n"),
        ("75", "0", true, "atomic-push\nseparate-push\nreleased=0\n"),
        ("0", "1", true, "separate-push\nreleased=0\n"),
    ] {
        std::fs::write(f.dir.join("log"), "").unwrap();
        let out = script_command(&path)
            .current_dir(&f.dir)
            .env("SOURCE_ROOT", root())
            .env("PUBLISH_STATUS", status)
            .env("WAIT_MODE", wait)
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            success,
            "{status}/{wait}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(f.dir.join("log")).unwrap(),
            expected
        );
    }
}

#[cfg(unix)]
struct ProofRepo {
    _fakes: Fakes,
    repo: PathBuf,
    cargo_home: PathBuf,
    global: PathBuf,
    version: PathBuf,
    tool: PathBuf,
}

#[cfg(unix)]
impl ProofRepo {
    fn new(tag: &str) -> Self {
        let fakes = Fakes::new(tag);
        let repo = fakes.dir.join("repo");
        let cargo_home = fakes.dir.join("cargo-home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&cargo_home).unwrap();
        let global = fakes.dir.join("gitconfig");
        std::fs::write(&global, "").unwrap();
        let version = fakes.dir.join("formatter-version");
        std::fs::write(&version, "formatter 1\n").unwrap();
        let tool = {
            fakes.fake("rustfmt", "cat \"$VERSION_FILE\"");
            fakes.dir.join("rustfmt")
        };
        let this = Self {
            _fakes: fakes,
            repo,
            cargo_home,
            global,
            version,
            tool,
        };
        this.git(&["init", "--quiet", "--template=", "-b", "main"]);
        std::fs::create_dir_all(this.repo.join(".agents/tasks")).unwrap();
        for (path, data) in [
            (".agents/tasks/T-TEST-1.json", "{}\n"),
            (".agents/identity.json", "{}\n"),
            ("a.rs", "fn a() {}\n"),
            ("b.rs", "fn b() {}\n"),
        ] {
            std::fs::write(this.repo.join(path), data).unwrap();
        }
        this.git(&["add", "."]);
        this.git(&["commit", "--quiet", "-m", "base"]);
        let main_oid = std::fs::read_to_string(this.repo.join(".git/refs/heads/main")).unwrap();
        std::fs::create_dir_all(this.repo.join(".git/refs/remotes/origin")).unwrap();
        std::fs::write(this.repo.join(".git/refs/remotes/origin/main"), main_oid).unwrap();
        this
    }

    fn isolate(&self, command: &mut std::process::Command) {
        command
            .current_dir(&self.repo)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", &self.global);
    }

    fn git(&self, args: &[&str]) -> String {
        let mut command = std::process::Command::new("git");
        self.isolate(&mut command);
        let output = command
            .args([
                "-c",
                "user.name=fixture",
                "-c",
                "user.email=fixture@localhost",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "maintenance.auto=false",
                "-c",
                "gc.auto=0",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn stamp(&self, format: bool) -> String {
        let mut command = script_command(&root().join("scripts/gate-input-stamp.sh"));
        self.isolate(&mut command);
        command
            .env("CARGO_HOME", &self.cargo_home)
            .env("RUSTFMT", &self.tool)
            .env("RUSTC", &self.tool)
            .env("VERSION_FILE", &self.version);
        if format {
            command.arg("--format");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

#[cfg(unix)]
struct PublicationRepo {
    world: ProofRepo,
    server: PathBuf,
    claim: String,
    receipt: String,
    accepted_head: String,
    head: String,
}

#[cfg(unix)]
impl PublicationRepo {
    fn new(tag: &str) -> Self {
        let world = ProofRepo::new(tag);
        let server = world._fakes.dir.join("server.git");
        world.git(&[
            "init",
            "--bare",
            "--quiet",
            "--template=",
            server.to_str().unwrap(),
        ]);
        let config = server.join("config");
        let mut contents = std::fs::read_to_string(&config).unwrap();
        contents
            .push_str("\n[receive]\n\tautogc = false\n[gc]\n\tauto = 0\n[pack]\n\tthreads = 1\n");
        std::fs::write(config, contents).unwrap();
        let repo_config = world.repo.join(".git/config");
        let mut repo_cfg = std::fs::read_to_string(&repo_config).unwrap();
        repo_cfg.push_str(&format!(
            "\n[remote \"origin\"]\n\turl = {}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n[susi]\n\tagent = TEST\n[gc]\n\tauto = 0\n",
            server.to_string_lossy().replace('\\', "/")
        ));
        std::fs::write(repo_config, repo_cfg).unwrap();
        let main_oid = std::fs::read_to_string(world.repo.join(".git/refs/heads/main")).unwrap();
        std::fs::write(world.repo.join(".git/refs/heads/work"), &main_oid).unwrap();
        std::fs::write(world.repo.join(".git/HEAD"), "ref: refs/heads/work\n").unwrap();
        std::fs::write(world.repo.join("a.rs"), "fn accepted_change() {}\n").unwrap();
        world.git(&["commit", "--quiet", "-am", "accepted source"]);
        let accepted_head = std::fs::read_to_string(world.repo.join(".git/refs/heads/work"))
            .unwrap()
            .trim()
            .to_string();
        std::fs::create_dir_all(world.repo.join(".agents/tasks/done")).unwrap();
        std::fs::write(
            world.repo.join(".agents/tasks/done/T-TEST-1.json"),
            format!(r#"{{"id":"T-TEST-1","closed":{{"by":"TEST","commit":"{accepted_head}"}}}}"#),
        )
        .unwrap();
        std::fs::remove_file(world.repo.join(".agents/tasks/T-TEST-1.json")).unwrap();
        world.git(&["add", ".agents/tasks"]);
        world.git(&["commit", "--quiet", "-m", "completion"]);
        let head = std::fs::read_to_string(world.repo.join(".git/refs/heads/work"))
            .unwrap()
            .trim()
            .to_string();
        let claim_file = world._fakes.dir.join("claim.json");
        let receipt_file = world._fakes.dir.join("receipt.json");
        std::fs::write(
            &claim_file,
            r#"{"task":"T-TEST-1","agent":"TEST","branch":"work","scopes":["a.rs"]}"#,
        )
        .unwrap();
        std::fs::write(
            &receipt_file,
            format!(
                r#"{{"task":"T-TEST-1","agent":"TEST","head":"{accepted_head}","closed_unix":1,"scopes":["a.rs"]}}"#
            ),
        )
        .unwrap();
        let oids = world.git(&[
            "hash-object",
            "-w",
            claim_file.to_str().unwrap(),
            receipt_file.to_str().unwrap(),
        ]);
        let mut oid_lines = oids.lines();
        let claim = oid_lines.next().unwrap().trim().to_string();
        let receipt = oid_lines.next().unwrap().trim().to_string();
        let this = Self {
            world,
            server,
            claim,
            receipt,
            accepted_head,
            head,
        };
        this.refs(false, &format!("update refs/claims/T-TEST-1 {}\nupdate refs/claim-agents/TEST {}\nupdate refs/closed/T-TEST-1 {}\n", this.claim, this.claim, this.receipt));
        this.world.git(&[
            "push",
            "--quiet",
            "--atomic",
            "origin",
            "refs/heads/main:refs/heads/main",
            "refs/claims/T-TEST-1",
            "refs/claim-agents/TEST",
            "refs/closed/T-TEST-1",
        ]);
        this
    }

    fn remote(&self, args: &[&str]) -> String {
        let mut all = vec!["-C", self.server.to_str().unwrap()];
        all.extend_from_slice(args);
        self.world.git(&all)
    }

    fn blob(&self, remote: bool, body: &str) -> String {
        let file = self.world._fakes.dir.join("blob.json");
        std::fs::write(&file, body).unwrap();
        let args = ["hash-object", "-w", file.to_str().unwrap()];
        (if remote {
            self.remote(&args)
        } else {
            self.world.git(&args)
        })
        .trim()
        .to_string()
    }

    fn refs(&self, remote: bool, transaction: &str) {
        let file = self.world._fakes.dir.join("refs.in");
        std::fs::write(&file, transaction).unwrap();
        let mut command = std::process::Command::new("git");
        self.world.isolate(&mut command);
        if remote {
            command.args(["-C", self.server.to_str().unwrap()]);
        }
        let output = command
            .args(["update-ref", "--stdin"])
            .stdin(std::fs::File::open(file).unwrap())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn race(&self, transaction: &str) {
        let file = self.world._fakes.dir.join("race.in");
        std::fs::write(file, transaction).unwrap();
        let hooks = self.world.repo.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-push");
        std::fs::write(&hook, "#!/bin/sh\nwhile IFS= read -r line; do :; done\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR\ngit --git-dir \"$RACE_SERVER\" update-ref --stdin <\"$RACE_INPUT\"\n").unwrap();
        set_script_executable(&hook);
    }

    fn run(&self, cloud: &str) -> std::process::Output {
        let mut command = script_command(&root().join("scripts/publish-task-branch.sh"));
        self.world.isolate(&mut command);
        command
            .args(["T-TEST-1", "work"])
            .env_remove("SUSI_AGENT")
            .env("SUSI_CLOUD_SESSION", cloud)
            .env("SUSI_TASK_REMOTE", "origin")
            .env("SUSI_QUEUE_TRANSPORT", "direct")
            .env("RACE_SERVER", &self.server)
            .env("RACE_INPUT", self.world._fakes.dir.join("race.in"))
            .output()
            .unwrap()
    }

    fn remote_ref(&self, name: &str) -> String {
        self.remote(&["for-each-ref", "--format=%(objectname)", name])
            .trim()
            .to_string()
    }
}

#[test]
#[cfg(unix)]
fn publication_updates_packed_refs_and_republication_always_rewrites_the_receipt() {
    let f = PublicationRepo::new("atomic-publish-packed");
    f.world.git(&["pack-refs", "--all"]);
    f.remote(&["pack-refs", "--all"]);
    for revision in [1, 2] {
        if revision == 2 {
            let transaction = format!(
                "update refs/claims/T-TEST-1 {}\nupdate refs/claim-agents/TEST {}\n",
                f.claim, f.claim
            );
            f.refs(false, &transaction);
            f.refs(true, &transaction);
        }
        let before = f.remote_ref("refs/closed/T-TEST-1");
        let out = f.run("0");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(f.remote_ref("refs/heads/work"), f.head);
        assert!(f.remote_ref("refs/claims/T-TEST-1").is_empty());
        assert!(f.remote_ref("refs/claim-agents/TEST").is_empty());
        assert_ne!(f.remote_ref("refs/closed/T-TEST-1"), before);
        let receipt = f.remote(&["cat-file", "-p", "refs/closed/T-TEST-1"]);
        assert!(
            receipt.contains(&format!(r#""revision":{revision}"#)),
            "{receipt}"
        );
        assert!(receipt.contains(&f.accepted_head) && receipt.contains(&f.head));
        assert!(receipt.contains(r#""scopes":["a.rs"]"#));
    }
}

#[test]
#[cfg(unix)]
fn publication_claim_reassignment_rejects_the_whole_transaction() {
    let f = PublicationRepo::new("atomic-publish-owner-race");
    let new = f.blob(
        true,
        r#"{"task":"T-TEST-1","agent":"OTHER","branch":"other"}"#,
    );
    f.race(&format!("update refs/claims/T-TEST-1 {new}\ndelete refs/claim-agents/TEST\ncreate refs/claim-agents/OTHER {new}\n"));
    let out = f.run("0");
    assert!(!out.status.success());
    assert!(f.remote_ref("refs/heads/work").is_empty());
    assert_eq!(f.remote_ref("refs/claims/T-TEST-1"), new);
    assert_eq!(f.remote_ref("refs/claim-agents/OTHER"), new);
    assert_eq!(f.remote_ref("refs/closed/T-TEST-1"), f.receipt);
}

#[test]
#[cfg(unix)]
fn publication_agent_index_change_cannot_release_another_task() {
    let f = PublicationRepo::new("atomic-publish-index-race");
    let new = f.blob(
        true,
        r#"{"task":"T-TEST-2","agent":"TEST","branch":"other"}"#,
    );
    f.race(&format!("update refs/claim-agents/TEST {new}\n"));
    let out = f.run("0");
    assert!(!out.status.success());
    assert!(f.remote_ref("refs/heads/work").is_empty());
    assert_eq!(f.remote_ref("refs/claims/T-TEST-1"), f.claim);
    assert_eq!(f.remote_ref("refs/claim-agents/TEST"), new);
    assert_eq!(f.remote_ref("refs/closed/T-TEST-1"), f.receipt);
}

#[test]
#[cfg(unix)]
fn publication_changed_receipt_keeps_the_claim_and_branch_untouched() {
    let f = PublicationRepo::new("atomic-publish-receipt-race");
    let new = f.blob(
        true,
        &format!(
            r#"{{"task":"T-TEST-1","agent":"TEST","head":"{}","closed_unix":2}}"#,
            f.accepted_head
        ),
    );
    f.race(&format!("update refs/closed/T-TEST-1 {new}\n"));
    let out = f.run("0");
    assert!(!out.status.success());
    assert!(f.remote_ref("refs/heads/work").is_empty());
    assert_eq!(f.remote_ref("refs/claims/T-TEST-1"), f.claim);
    assert_eq!(f.remote_ref("refs/claim-agents/TEST"), f.claim);
    assert_eq!(f.remote_ref("refs/closed/T-TEST-1"), new);
}

#[test]
#[cfg(unix)]
fn publication_leases_never_force_the_code_branch_backwards() {
    let f = PublicationRepo::new("atomic-publish-history");
    let newer = f
        .world
        .git(&[
            "commit-tree",
            "HEAD^{tree}",
            "-p",
            "HEAD",
            "-m",
            "published later",
        ])
        .trim()
        .to_string();
    f.world.git(&[
        "push",
        "--quiet",
        "origin",
        &format!("{newer}:refs/heads/work"),
    ]);
    let out = f.run("0");
    assert!(!out.status.success());
    assert_eq!(f.remote_ref("refs/heads/work"), newer);
    assert_eq!(f.remote_ref("refs/claims/T-TEST-1"), f.claim);
    assert_eq!(f.remote_ref("refs/claim-agents/TEST"), f.claim);
    assert_eq!(f.remote_ref("refs/closed/T-TEST-1"), f.receipt);
}

#[test]
#[cfg(unix)]
fn publication_cloud_transport_selects_the_existing_path_without_writes() {
    let f = PublicationRepo::new("atomic-publish-cloud");
    let out = f.run("1");
    assert_eq!(out.status.code(), Some(75));
    assert!(f.remote_ref("refs/heads/work").is_empty());
    assert_eq!(f.remote_ref("refs/claims/T-TEST-1"), f.claim);
    assert_eq!(f.remote_ref("refs/closed/T-TEST-1"), f.receipt);
}

#[test]
#[cfg(unix)]
fn source_proof_changes_when_upstream_code_changes_but_the_branch_diff_does_not() {
    let f = ProofRepo::new("proof-upstream");
    f.git(&["checkout", "--quiet", "-b", "work"]);
    std::fs::write(f.repo.join("a.rs"), "fn branch_change() {}\n").unwrap();
    f.git(&["commit", "--quiet", "-am", "branch"]);
    let diff = f.git(&[
        "diff",
        "origin/main",
        "HEAD",
        "--",
        ".",
        ":(exclude).agents/tasks",
    ]);
    let before = f.stamp(false);
    std::fs::write(
        f.repo.join(".agents/tasks/T-TEST-1.json"),
        "{\"closed\":true}\n",
    )
    .unwrap();
    f.git(&["add", ".agents/tasks"]);
    assert_eq!(before, f.stamp(false));
    std::fs::write(f.repo.join(".agents/tasks/T-TEST-1.json"), "{}\n").unwrap();
    f.git(&["add", ".agents/tasks"]);
    f.git(&["checkout", "--quiet", "main"]);
    std::fs::write(f.repo.join("b.rs"), "fn upstream_change() {}\n").unwrap();
    f.git(&["commit", "--quiet", "-am", "upstream"]);
    f.git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
    f.git(&["checkout", "--quiet", "work"]);
    f.git(&["merge", "--quiet", "--no-edit", "main"]);
    assert_eq!(
        diff,
        f.git(&[
            "diff",
            "origin/main",
            "HEAD",
            "--",
            ".",
            ":(exclude).agents/tasks"
        ])
    );
    assert_ne!(before, f.stamp(false));
}

#[test]
#[cfg(unix)]
fn format_proof_tracks_formatter_and_external_cargo_config() {
    let f = ProofRepo::new("proof-tools");
    let before = f.stamp(true);
    std::fs::write(f.cargo_home.join("config.toml"), "[build]\njobs=2\n").unwrap();
    assert_ne!(before, f.stamp(true));
    std::fs::remove_file(f.cargo_home.join("config.toml")).unwrap();
    std::fs::write(&f.version, "formatter 2\n").unwrap();
    assert_ne!(before, f.stamp(true));
}

#[test]
#[cfg(unix)]
fn source_proof_tracks_untracked_names_contents_and_compiled_agent_data() {
    let f = ProofRepo::new("proof-untracked");
    let before = f.stamp(false);
    std::fs::write(f.repo.join("extra.rs"), "fn extra() {}\n").unwrap();
    let extra = f.stamp(false);
    assert_ne!(before, extra);
    std::fs::rename(f.repo.join("extra.rs"), f.repo.join("renamed.rs")).unwrap();
    let renamed = f.stamp(false);
    assert_ne!(extra, renamed);
    std::fs::write(f.repo.join("renamed.rs"), "fn different() {}\n").unwrap();
    assert_ne!(renamed, f.stamp(false));
    std::fs::remove_file(f.repo.join("renamed.rs")).unwrap();
    std::fs::write(f.repo.join(".agents/identity.json"), "{\"changed\":true}\n").unwrap();
    assert_ne!(before, f.stamp(false));
}

#[test]
#[cfg(unix)]
fn format_proof_reuse_never_turns_a_failed_formatter_into_a_success() {
    let f = Fakes::new("format-proof");
    std::fs::create_dir_all(f.dir.join("scripts")).unwrap();
    std::fs::create_dir_all(f.dir.join("cache")).unwrap();
    let helper = f.dir.join("scripts/gate-input-stamp.sh");
    std::fs::write(&helper, "#!/bin/sh\ncat \"$STAMP_FILE\"\n").unwrap();
    set_script_executable(&helper);
    let source = read("scripts/parallel-workflow.sh").replace("\r\n", "\n");
    let mut script = String::from("set -euo pipefail\n");
    script.push_str(&shell_function(&source, "gate_format"));
    script.push_str(
        r#"
root=$PWD
STAMP_FILE=$PWD/stamp
export STAMP_FILE
printf 'first\n' > stamp
git() { printf '%s\n' "$PWD/cache"; }
cargo() { printf 'format\n' >> calls; [ "${FAIL:-0}" = 0 ]; }
gate_format
gate_format
[ "$(wc -l < calls)" = 1 ]
printf 'second\n' > stamp
FAIL=1
if gate_format; then exit 9; fi
[ "$(cat cache/susi-fmt-ok)" = first ]
FAIL=0
gate_format
[ "$(cat cache/susi-fmt-ok)" = second ]
[ "$(wc -l < calls)" = 3 ]
[ ! -f cache/susi-gate-ok ]
"#,
    );
    let path = f.dir.join("format-check.sh");
    std::fs::write(&path, script).unwrap();
    let output = script_command(&path)
        .current_dir(&f.dir)
        .env_remove("SUSI_GATE_NOCACHE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn compliance_counts_live_claims_from_one_byte_counted_snapshot() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let f = Fakes::new("live-claim-snapshot");
    let run = |args: &[&str], input: &[u8]| {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&f.dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_COMMON_DIR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    };
    run(&["init", "--quiet", "--template="], b"");
    let mut files = String::new();
    for n in 0..94 {
        let body = match n {
            0 => "{\n  \"agent\": \"OWN\",\n  \"task\": \"wrong-body-id\",\n  \"lease_until_unix\": 2000\n}".to_string(),
            1 => r#"{"agent":"OWN","lease_until_unix":2000}"#.into(),
            2 => r#"{"agent":"OWN","lease_until_unix":1000}"#.into(),
            3 => r#"{"agent":"SINGLE","lease_until_unix":2000}"#.into(),
            4 => "not json\n".into(),
            5 => "[]".into(),
            _ => r#"{"agent":"OTHER","lease_until_unix":2000}"#.into(),
        };
        std::fs::write(f.dir.join(format!("claim{n}.json")), body).unwrap();
        files.push_str(&format!("claim{n}.json\n"));
    }
    let ids = run(&["hash-object", "-w", "--stdin-paths"], files.as_bytes());
    let refs: String = ids
        .lines()
        .enumerate()
        .map(|(n, id)| format!("create refs/claims/T-TEST-{n} {id}\n"))
        .collect();
    run(&["update-ref", "--stdin"], refs.as_bytes());
    let source = read("scripts/check-workflow-compliance.sh").replace("\r\n", "\n");
    let mut script = String::from("set -euo pipefail\n. \"$COMPAT\"\n");
    script.push_str(&shell_function(&source, "load_live_claim_index"));
    script.push_str(&shell_function(&source, "check_one_live_claim"));
    script.push_str(
        r#"
now=1000
live_claim_index=
agents_checked=' '
fail=0
err() { printf '%s\n' "$*" >> errors; fail=1; }
git() {
    printf '%s\n' "$*" >> calls
    command git "$@"
}
check_one_live_claim SINGLE abc T-TEST-3
[ "$fail" = 0 ]
check_one_live_claim ABSENT abc T-TEST-99
[ "$fail" = 0 ]
check_one_live_claim OWN abc T-TEST-0
[ "$fail" = 1 ]
check_one_live_claim OWN abc T-TEST-0
"#,
    );
    let path = f.dir.join("check.sh");
    std::fs::write(&path, script).unwrap();
    let output = script_command(&path)
        .current_dir(&f.dir)
        .env("COMPAT", root().join("scripts/posix-compat.sh"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let errors = std::fs::read_to_string(f.dir.join("errors")).unwrap();
    assert_eq!(errors.lines().count(), 1);
    assert!(
        errors.contains("holds 2 live claims:T-TEST-0 T-TEST-1"),
        "{errors}"
    );
    let calls = std::fs::read_to_string(f.dir.join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 2, "{calls}");
    assert!(calls.contains("cat-file --batch="));
}

#[test]
#[cfg(unix)]
fn scratch_cleanup_preserves_failure_and_stops_services_before_background_deletion() {
    let f = Fakes::new("async-scratch-cleanup");
    f.fake(
        "kill-leftover-service-runs.sh",
        "echo stopped > service-stopped",
    );
    let source = read("scripts/check-hermetic-tests.sh").replace("\r\n", "\n");
    let mut script = String::from(". \"$COMPAT\"\n");
    script.push_str(&shell_function(&source, "cleanup_hermetic_dirs"));
    script.push_str(
        r#"
git() { printf '%s\n' "$PWD"; }
remove_hermetic_dirs() {
    [ -f service-stopped ] || exit 9
    printf '%s\n' "$BASHPID" > deleter.pid
}
parent=$BASHPID
(exit 23)
cleanup_hermetic_dirs
status=$?
[ "$status" = 23 ] || exit 8
[ -f service-stopped ] || exit 7
wait
[ "$(cat deleter.pid)" != "$parent" ] || exit 6
"#,
    );
    let path = f.dir.join("cleanup.sh");
    std::fs::write(&path, script).unwrap();
    let output = script_command(&path)
        .current_dir(&f.dir)
        .env("COMPAT", root().join("scripts/posix-compat.sh"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn finish_hooks_leave_primary_maintenance_to_the_finish_cleanup() {
    let f = Fakes::new("finish-hook-cleanup");
    f.fake("git", "echo unexpected-git >&2; exit 91");
    let out = script_command(&root().join(".githooks/sync-primary"))
        .env("SUSI_SYNC_IN_FINISH", "1")
        .env("PATH", path_with_prefix(&f.dir, true))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[cfg(unix)]
fn finish_rechecks_the_remote_before_skipping_lease_renewal() {
    let f = Fakes::new("finish-remote-lease");
    let workflow = read("scripts/parallel-workflow.sh").replace("\r\n", "\n");
    let mut script = String::from("set -euo pipefail\n");
    script.push_str(&shell_function(&workflow, "renew_or_readopt"));
    script.push_str(r#"
task=T-TEST-1
SUSI_AGENT=TEST
susi_bin=susi
git() {
    case "$1" in
    cat-file)
        until=9999999999; [ "$MODE" != expired ] || until=1
        printf '{"task":"T-TEST-1","agent":"TEST","branch":"work","lease_until_unix":%s,"scopes":["src/owned"]}\n' "$until" ;;
    symbolic-ref) echo work ;;
    rev-parse) echo 1111111111111111111111111111111111111111 ;;
    ls-remote)
        echo remote >> calls
        case "$MODE" in
        changed) printf '%s\t%s\n' 2222222222222222222222222222222222222222 refs/claims/T-TEST-1 ;;
        missing) ;;
        unavailable) return 1 ;;
        *) printf '%s\t%s\n' 1111111111111111111111111111111111111111 refs/claims/T-TEST-1 ;;
        esac ;;
    *) return 9 ;;
    esac
}
susi() {
    printf '%s\n' "$*" >> calls
    [ "$2" != renew ]
}
renew_or_readopt
"#);
    let path = f.dir.join("lease-check.sh");
    std::fs::write(&path, script).unwrap();
    for (mode, expected) in [
        ("fresh", "remote\n"),
        (
            "changed",
            "remote\ntasks renew T-TEST-1\ntasks claim T-TEST-1 --scope src/owned\n",
        ),
        (
            "missing",
            "remote\ntasks renew T-TEST-1\ntasks claim T-TEST-1 --scope src/owned\n",
        ),
        (
            "unavailable",
            "remote\ntasks renew T-TEST-1\ntasks claim T-TEST-1 --scope src/owned\n",
        ),
        (
            "expired",
            "tasks renew T-TEST-1\ntasks claim T-TEST-1 --scope src/owned\n",
        ),
    ] {
        std::fs::write(f.dir.join("calls"), "").unwrap();
        let out = script_command(&path)
            .current_dir(&f.dir)
            .env("MODE", mode)
            .env_remove("SUSI_TASK_REMOTE")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(f.dir.join("calls")).unwrap(),
            expected,
            "{mode}"
        );
    }
}

#[test]
#[cfg(unix)]
fn normal_sync_finds_its_owned_claim_without_remote_renewal_lookup() {
    assert_eq!(
        read("scripts/parallel-workflow.sh")
            .matches("renew_owned_claims() {")
            .count(),
        1,
        "a later definition must not override the lease implementation exercised below"
    );
    let f = Fakes::new("sync-owned-claim");
    let workflow = read("scripts/parallel-workflow.sh").replace("\r\n", "\n");
    let mut script = String::from("set -euo pipefail\n. \"$COMPAT\"\n");
    script.push_str(&shell_function(&workflow, "renew_owned_claims"));
    script.push_str(r#"
SUSI_AGENT=TESTAGENT
receipt_json='{"agent":"TESTAGENT","task":"T-TEST-1","branch":"work"}'
[ "$MODE" != other ] || receipt_json='{"agent":"TESTAGENT","task":"T-TEST-1","branch":"other"}'
git() {
    case "$*" in
    'symbolic-ref -q --short HEAD') echo work ;;
    'cat-file -p refs/claim-agents/TESTAGENT') [ "$MODE" != legacy ] || return 1; printf '%s\n' "$receipt_json" ;;
    'cat-file -e origin/main:.agents/tasks/done/T-TEST-1.json') [ "$MODE" = published ] ;;
    'for-each-ref --format=%(objectname) refs/claims/') echo abc ;;
    'cat-file --batch') cat >/dev/null; printf 'abc blob 64\n%s\n\n' "$receipt_json" ;;
    *) echo "unexpected git $*" >&2; return 9 ;;
    esac
}
renew_or_readopt() { printf '%s\n' "$task" >> calls; }
renew_owned_claims
"#);
    let path = f.dir.join("sync-check.sh");
    std::fs::write(&path, script).unwrap();
    for (mode, expected) in [
        ("owned", "T-TEST-1\n"),
        ("other", ""),
        ("published", ""),
        ("legacy", "T-TEST-1\n"),
    ] {
        std::fs::write(f.dir.join("calls"), "").unwrap();
        let out = script_command(&path)
            .current_dir(&f.dir)
            .env("MODE", mode)
            .env("COMPAT", root().join("scripts/posix-compat.sh"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{mode}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(f.dir.join("calls")).unwrap(),
            expected,
            "{mode}"
        );
    }
}

#[test]
fn workflow_finish_and_merge_gate_skip_redundant_tests_compiles_and_network_trips() {
    let pw = read("scripts/parallel-workflow.sh").replace("\r\n", "\n");
    let sync_fn = shell_function(&pw, "sync");
    assert!(
        sync_fn.contains("SUSI_SYNC_IN_FINISH") && pw.contains("export SUSI_SYNC_IN_FINISH=1"),
        "`finish` must set SUSI_SYNC_IN_FINISH=1 so its two internal `sync` calls skip \
         the redundant `renew_owned_claims` remote round-trip"
    );
    let gate_fn = shell_function(&pw, "gate");
    let test_steps = shell_function(&pw, "gate_test_steps");
    assert!(
        gate_fn.contains("resolve_gate_plan")
            && shell_function(&pw, "resolve_gate_plan").contains("root-test) gate_root_tests")
            && test_steps.contains("gate_tests -p \"$rootpkg\" $(printf ' --test %s' $root_tests)"),
        "when the root package is gated only because top-level `tests/<name>.rs` files changed, \
         the local gate must run only those `--test <name>` binaries instead of all 102 root test binaries"
    );
    // Clippy and the test run overlap: the test steps start first (in the
    // background), clippy follows once the test build holds cargo's lock, and
    // the gate waits for both verdicts. The lint's wall time then disappears
    // into the test run's instead of being paid before it.
    let overlap = shell_function(&pw, "gate_lint_and_test");
    assert!(
        gate_fn.contains("gate_lint_and_test \"${clippy_args[@]}\"")
            && overlap.contains("gate_test_steps &")
            && overlap.contains("timed_step clippy cargo clippy \"$@\"")
            && overlap.contains("wait \"$tpid\" || test_status=$?")
            && overlap.contains("SUSI_GATE_SERIAL"),
        "the gate must run clippy alongside the test run (tests first, clippy after they take \
         the build lock), wait for both, and keep SUSI_GATE_SERIAL=1 as the one-after-the-other path"
    );
    let hermetic = read("scripts/check-hermetic-tests.sh");
    assert!(
        hermetic.contains("if [ \"$arg\" = --test ]; then has_test_filter=1; fi")
            && hermetic.contains("if [ \"$has_test_filter\" -eq 0 ]; then")
            && hermetic.contains("NEXTEST_TEST_THREADS=1 cargo nextest run --locked"),
        "`check-hermetic-tests.sh` must skip `cargo test --doc` when `--test` filters integration test binaries \
         and retry failed tests alone (`NEXTEST_TEST_THREADS=1`) when parallel load causes a flake"
    );

    let sweep = read("scripts/kill-leftover-service-runs.sh").replace("\r\n", "\n");
    assert!(
        sweep.contains("tasklist.exe /FI \"IMAGENAME eq susi.exe\""),
        "`kill-leftover-service-runs.sh` must fast-check `tasklist.exe` on Windows before spawning `powershell.exe` + WMI when no `susi.exe` process exists"
    );
    assert!(
        pw.contains("prefetch_finish_refs")
            && pw.contains("SUSI_CLOSE_QUEUE_SYNCED=\"$task@$root\"")
            && shell_function(&pw, "seed_own_target").contains("[ -f \"$root/target/.susi-seeded\" ]"),
        "`finish` must prefetch branch and queue refs once across `lease`, `initial-sync`, and `close`, and `seed_own_target` must short-circuit when `target/.susi-seeded` is already present"
    );
}

#[test]
#[cfg(unix)]
fn release_source_selects_root_workflow_test() {
    let dir = std::env::temp_dir().join(format!("susi-release-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let write = |rel: &str, body: &str| {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    };
    let commit = |message: &str| {
        git(&["add", "-A", "."]);
        git(&["commit", "-qm", message]);
        git(&["rev-parse", "HEAD"])
    };
    git(&["init", "-q", "-b", "main", "."]);
    for script in ["ci-changed-crates.sh", "posix-compat.sh"] {
        let path = dir.join("scripts").join(script);
        write(
            &format!("scripts/{script}"),
            &read(&format!("scripts/{script}")),
        );
        set_script_executable(&path);
    }
    write("Cargo.toml", "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\nsusi-gawd = { path = \"crates/susi-gawd\" }\n[workspace]\nmembers = [\"crates/susi-gawd\"]\nresolver = \"2\"\n");
    write("src/lib.rs", "pub fn root() {}\n");
    write(
        "tests/release_workflow.rs",
        "#[test] fn checks_release_source() {}\n",
    );
    write(
        "crates/susi-gawd/Cargo.toml",
        "[package]\nname = \"susi-gawd\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write("crates/susi-gawd/src/lib.rs", "pub mod admin;\n");
    write("crates/susi-gawd/src/admin.rs", "pub mod release;\n");
    write(
        "crates/susi-gawd/src/admin/release.rs",
        "pub fn release() {}\n",
    );
    let base = commit("base");
    git(&["branch", "origin/main", &base]);
    write(
        "crates/susi-gawd/src/admin/release.rs",
        "pub fn release_changed() {}\n",
    );
    commit("release source");
    let plan = || {
        let out = script_command(&dir.join("scripts/ci-changed-crates.sh"))
            .arg("--plan")
            .current_dir(&dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    let result = plan();
    for entry in [
        "test:susi-gawd",
        "test:fixture",
        "root-package:fixture",
        "root-test:release_workflow",
    ] {
        assert!(
            result.lines().any(|line| line == entry),
            "missing {entry}: {result}"
        );
    }
    write("src/lib.rs", "pub fn root_changed() {}\n");
    commit("also change root production");
    assert_eq!(
        plan(),
        "ALL",
        "mixed production changes must not narrow root coverage"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
