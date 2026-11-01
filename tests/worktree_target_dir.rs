// Mixed native seeding coverage and optional legacy POSIX-hook compatibility.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Every worktree has a cargo target of its own, copied from a warm snapshot
//! of origin/main (`susi workflow seed-target`, which `scripts/seed-target.sh`
//! execs). Worktrees used to link `target/` to the primary checkout's shared
//! one, and cargo names a workspace crate's build by its path relative to the
//! workspace: a crate another worktree had built last read as fresh, and its
//! code was linked into this branch's build. `scripts/target-guard.sh`
//! narrowed that and could not close it.
//!
//! These pin what seeding does: a link becomes a real directory (the link is
//! removed, never what it points to), a missing or empty target is copied from
//! the snapshot with its timestamps, every unit not built from a registry or
//! git source (workspace members, path dependencies, crates the lockfile no
//! longer names) is stripped so it rebuilds from this worktree, and the
//! registry crates' builds stay; a copy that cannot be stripped all the way is
//! never installed; a target holding a build is never replaced, `SUSI_NO_SEED=1`
//! skips the copy, and with no snapshot the target is a plain empty one. Also
//! the snapshot's one writer (`--refresh`): it builds only in a clone it made,
//! one run at a time across Git installs, publishes a stripped snapshot, and
//! keeps its build checkout's target bounded. And the paths that convert a
//! worktree still linked to the old shared target: the git hooks (a merge, the
//! commit that concludes a conflicted one, `git worktree add`), and the
//! retired guard and link script as the installed susi (v0.21.23) runs them,
//! which convert it before the command that follows.
//!
//! The seeding was a bash script; it is Rust now
//! (`crates/susi-gawd/src/admin/seed.rs`), and the script is a stub that finds
//! a susi with the verb and execs it, so every path here still runs the
//! script where the hooks and the older callers do, with `SUSI_WORKFLOW_BIN`
//! naming the binary under test. The races the stand-in `cp`, `rm`, `mkdir`
//! and `mv` shell functions used to inject are driven through the seeding's
//! own test seam: `SUSI_SEED_TEST_GATES` has a run record each phase it
//! reaches (`lock`, `copy`, `rename`, `done`) and hold at one until it is let
//! go. A stand-in cargo for `--refresh` goes through `SUSI_SEED_CARGO`, since
//! a script cannot be run by name on Windows.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use susi_gawd::admin::seed;

/// The snapshot fixture's packages, each with a hash of its own (cargo's
/// files in `deps/` and `build/` carry the unit's hash): the members
/// `fixture-root`, `cell` and `lone`, the path dependency `patched`, and the
/// registry crates `serde`, `cellar` and `cell-build` (whose names start like
/// a member's, and must not be taken for one).
const UNITS: [(&str, &str); 7] = [
    ("cell", "1000000000000001"),
    ("fixture-root", "1000000000000002"),
    ("lone", "1000000000000003"),
    ("patched", "1000000000000004"),
    ("serde", "2000000000000001"),
    ("cellar", "2000000000000002"),
    ("cell-build", "2000000000000003"),
];
const PATH_UNITS: [&str; 4] = ["cell", "fixture-root", "lone", "patched"];
/// A seeded target's marker, as the seeding writes it.
const SEEDED_MARK_NAME: &str = ".susi-seeded";
const REGISTRY_UNITS: [&str; 3] = ["serde", "cellar", "cell-build"];
const PROFILES: [&str; 2] = ["debug", "x86_64-unknown-linux-gnu/debug"];

fn hash(pkg: &str) -> &'static str {
    UNITS
        .iter()
        .find(|(name, _)| *name == pkg)
        .map(|(_, h)| *h)
        .unwrap_or_else(|| panic!("no unit {pkg}"))
}

fn script_command(script: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("sh");
        command.arg(script).env("MSYS_NO_PATHCONV", "1");
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("bash");
        command.arg(script);
        command
    }
}

/// The command for `program args` in `dir`, with nothing of the caller's own
/// git, cargo or snapshot settings.
fn command(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> Command {
    let mut cmd = if program.ends_with(".sh") {
        script_command(Path::new(program))
    } else {
        Command::new(program)
    };
    scrub(&mut cmd);
    cmd.args(args).current_dir(dir);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd
}

fn scrub(cmd: &mut Command) {
    cmd.env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        // The scripts and hooks exec `susi workflow seed-target`: the binary
        // under test, not whatever susi the host has installed.
        .env("SUSI_WORKFLOW_BIN", env!("CARGO_BIN_EXE_susi"))
        .env_remove("SUSI_HOME")
        .env_remove("SUSI_AGENT")
        // A developer's own snapshot settings must never reach a fixture:
        // seeding one from a real snapshot copies gigabytes.
        .env_remove("SUSI_NO_SEED")
        .env_remove("SUSI_TARGET_SEED")
        .env_remove("SUSI_TARGET_SEED_SRC")
        .env_remove("SUSI_SEED_REFRESH_SECS")
        .env_remove("SUSI_SEED_LOCK_STALE")
        .env_remove("SUSI_SEED_WAIT_SECS")
        .env_remove("SUSI_SEED_TEST_GATES")
        .env_remove("SUSI_SEED_CARGO")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_BUILD_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
}

/// `susi <args>` (the binary under test) in `dir`.
fn susi(dir: &Path, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    run(dir, env!("CARGO_BIN_EXE_susi"), args, envs)
}

/// The seeding's test seam (`SUSI_SEED_TEST_GATES`): a directory where every
/// run records the phases it reaches, one per line in `log`, and holds at a
/// phase while `<phase>.wait` exists until `<phase>.go` does.
struct Gates {
    dir: PathBuf,
}

impl Gates {
    fn new(root: &Path) -> Self {
        let dir = root.join("gates");
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }

    fn env(&self) -> (&'static str, String) {
        (
            "SUSI_SEED_TEST_GATES",
            self.dir.to_string_lossy().into_owned(),
        )
    }

    /// Have every run stop at `phase` until [`Gates::release`].
    fn hold(&self, phase: &str) {
        std::fs::write(self.dir.join(format!("{phase}.wait")), "").unwrap();
    }

    fn release(&self, phase: &str) {
        std::fs::write(self.dir.join(format!("{phase}.go")), "go").unwrap();
    }

    /// How many runs reached `phase`.
    fn count(&self, phase: &str) -> usize {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.trim() == phase)
            .count()
    }

    /// Wait until a run has reached `phase`.
    fn wait_for(&self, phase: &str, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        while self.count(phase) == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        self.count(phase) > 0
    }
}

fn finish(out: std::process::Output) -> (i32, String, String) {
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
        String::from_utf8_lossy(&out.stderr).trim().to_string(),
    )
}

fn run(dir: &Path, program: &str, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    finish(command(dir, program, args, envs).output().unwrap())
}

fn git(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = run(dir, "git", args, &[]);
    assert_eq!(code, 0, "git {args:?}: {err}");
    out
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn is_link(p: &Path) -> bool {
    p.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
}

fn is_real_dir(p: &Path) -> bool {
    p.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_dir() && !m.file_type().is_symlink())
}

fn is_empty_dir(p: &Path) -> bool {
    is_real_dir(p) && std::fs::read_dir(p).unwrap().next().is_none()
}

/// Every file (and link) under `dir`, sorted; links are not followed.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(at) = todo.pop() {
        for entry in std::fs::read_dir(&at).unwrap().filter_map(Result::ok) {
            let path = entry.path();
            if is_real_dir(&path) {
                todo.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The text of the shell function `name` in `text`, from its `name() {`
/// line to the `}` that closes it at the start of a line.
fn shell_function(text: &str, name: &str) -> String {
    let text = text.replace("\r\n", "\n");
    let start = text
        .find(&format!("\n{name}() {{\n"))
        .unwrap_or_else(|| panic!("no shell function {name}"))
        + 1;
    let len = text[start..]
        .find("\n}\n")
        .unwrap_or_else(|| panic!("{name} never closes"))
        + 3;
    text[start..start + len].to_string()
}

/// A script to run with `sh` (Git Bash on Windows): the given text, with
/// /usr/bin first on PATH on Windows, as the repository's scripts put it.
fn shell_script(path: &Path, body: &str) {
    write(
        path,
        &format!("case \"${{OSTYPE:-}}\" in msys* | cygwin*) PATH=/usr/bin:$PATH ;; esac\n{body}"),
    );
}

/// `path` with forward slashes, as a shell on Windows takes it best.
fn slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A link at `link` to the directory `to`: a junction on Windows (what the
/// old linker made, and what any user can create), a symlink elsewhere.
fn link_dir(to: &Path, link: &Path) {
    #[cfg(windows)]
    {
        // cmd reads a `/` inside a path as the start of a switch.
        let native = |p: &Path| p.to_string_lossy().replace('/', "\\");
        let out = Command::new("cmd")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(native(link))
            .arg(native(to))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "mklink /J: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[cfg(not(windows))]
    std::os::unix::fs::symlink(to, link).unwrap();
}

/// An old time, so a copy that kept it is told from one that did not.
fn old_time() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

fn mtime(p: &Path) -> SystemTime {
    std::fs::metadata(p).unwrap().modified().unwrap()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn wait_for(path: &Path, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !path.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    path.exists()
}

/// A directory of stand-in tools, and the PATH that puts it first. A bash
/// script named like the tool: Git Bash runs one by its `#!` line, whatever
/// its name, so `cargo` resolves to it too.
fn fake_tools(dir: &Path, tools: &[(&str, &str)]) -> String {
    std::fs::create_dir_all(dir).unwrap();
    for (name, body) in tools {
        let file = dir.join(name);
        std::fs::write(&file, format!("#!/usr/bin/env bash\n{body}")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let mut dirs = vec![dir.to_path_buf()];
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(dirs)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// The `SUSI_SEED_CARGO` value that runs the bash script `script` as cargo: the
/// script itself where a `#!` line runs it, Git's bash with the script on
/// Windows (a PATH-style list: the program, then its leading arguments).
/// `None` on a Windows host with no Git bash to be found.
fn cargo_seam(script: &Path) -> Option<String> {
    if cfg!(windows) {
        let bash = raw_msys_program(script.parent().unwrap(), "bash")?;
        Some(
            std::env::join_paths([bash, script.to_path_buf()])
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        Some(script.to_string_lossy().into_owned())
    }
}

/// Make the directory read-only where that is a mode bit (Unix): a file in
/// it can then be neither removed nor added. `false` where it cannot be, or
/// where the mode binds nothing (root, as in a container, writes through it;
/// the mode is then put back).
fn read_only(path: &Path, on: bool) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if on { 0o555 } else { 0o755 };
        if std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).is_err() {
            return false;
        }
        if !on {
            return true;
        }
        let probe = path.join(".read-only-probe");
        if std::fs::write(&probe, "").is_ok() {
            let _ = std::fs::remove_file(&probe);
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
            return false;
        }
        true
    }
    #[cfg(not(unix))]
    {
        let _ = (path, on);
        false
    }
}

/// Git for Windows' raw MSYS `<git>/usr/bin/<name>.exe`: no launcher puts
/// usr/bin on its PATH. `None` elsewhere, or when there is none.
fn raw_msys_program(dir: &Path, name: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let (code, exec, _) = run(dir, "git", &["--exec-path"], &[]);
    if code != 0 {
        return None;
    }
    // <git>/<mingw64|ucrt64>/libexec/git-core
    let program = Path::new(&exec)
        .ancestors()
        .nth(3)?
        .join("usr")
        .join("bin")
        .join(format!("{name}.exe"));
    program.is_file().then_some(program)
}

/// The program `name` on this process's own PATH, by its absolute path.
fn on_this_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|program| program.is_file())
}

/// Set the modification time of `path` (a directory too) to `secs` seconds
/// ago, with the shell's `touch`.
fn age(path: &Path, secs: u64) {
    let (code, _, err) = run(
        path.parent().unwrap(),
        "sh",
        &[
            "-c",
            "touch -d \"@$1\" \"$2\"",
            "age",
            &(now() - secs).to_string(),
            &path.to_string_lossy().replace('\\', "/"),
        ],
        &[("MSYS_NO_PATHCONV", "1")],
    );
    assert_eq!(code, 0, "touch: {err}");
}

/// The fixture's Cargo.lock: the members `fixture-root` and `cell`, the path
/// dependency `patched` (as the real one once listed vendor/reqwest) and the
/// registry crates `serde`, `cellar` and `cell-build`; the member `lone` is
/// left out, so only `cargo metadata` names it, and nothing keeps it.
fn fixture_lockfile() -> String {
    let registry = "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n";
    format!(
        "version = 4\n\n\
         [[package]]\nname = \"cell\"\nversion = \"0.0.0\"\n\n\
         [[package]]\nname = \"cell-build\"\nversion = \"1.0.0\"\n{registry}\n\
         [[package]]\nname = \"cellar\"\nversion = \"1.0.0\"\n{registry}\n\
         [[package]]\nname = \"fixture-root\"\nversion = \"0.0.0\"\ndependencies = [\n \"cell\",\n]\n\n\
         [[package]]\nname = \"patched\"\nversion = \"0.1.0\"\n\n\
         [[package]]\nname = \"serde\"\nversion = \"1.0.0\"\n{registry}"
    )
}

/// One profile of a full build tree at `at`, as cargo lays it out: every
/// unit's fingerprint and its files in deps/ and build/, what cargo copies up
/// into a profile for the workspace, examples, an incremental cache.
fn lay_out_profile(at: &Path) {
    for (pkg, h) in UNITS {
        write(
            &at.join(".fingerprint")
                .join(format!("{pkg}-{h}"))
                .join(format!("lib-{pkg}")),
            "fingerprint",
        );
        let krate = pkg.replace('-', "_");
        write(&at.join(format!("deps/lib{krate}-{h}.rlib")), "built");
        write(&at.join(format!("deps/{krate}-{h}.d")), "deps");
        write(&at.join(format!("build/{pkg}-{h}/out/generated.rs")), "out");
    }
    write(&at.join("fixture-root.exe"), "an uplifted binary");
    write(&at.join("libcell.rlib"), "an uplifted library");
    // On Windows cargo writes a binary into deps/ with no hash.
    for file in ["fixture-root.exe", "fixture-root.pdb", "fixture-root.d"] {
        write(&at.join("deps").join(file), "the workspace's binary");
    }
    write(&at.join("examples/demo-3000000000000001.exe"), "x");
    write(&at.join("incremental/cell-1abc/s-1/dep-graph.bin"), "x");
}

/// The fixture: a bare remote, a primary checkout holding a small cargo
/// workspace and the scripts, and a snapshot beside it where the default
/// `SUSI_TARGET_SEED` looks (`<primary>/../.susi-target-seed`).
///
/// Cargo.lock lists the members `fixture-root` and `cell`, the path
/// dependency `patched` (as it once listed vendor/reqwest) and the registry crates
/// `serde`, `cellar` and `cell-build`; it leaves the member `lone` out, so
/// only `cargo metadata` names it, and nothing keeps it.
struct World {
    root: PathBuf,
    primary: PathBuf,
}

impl World {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("susi-seed-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let primary = root.join("susi");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(primary.join("scripts")).unwrap();
        git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        git(&primary, &["init", "--quiet", "-b", "main"]);
        git(&primary, &["config", "commit.gpgsign", "false"]);
        git(
            &primary,
            &["remote", "add", "origin", bare.to_str().unwrap()],
        );
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        for s in [
            "susi-worktree.sh",
            "park-primary.sh",
            "setup-dev.sh",
            "seed-target.sh",
            "link-shared-target.sh",
            "target-guard.sh",
            "workflow-session-start.sh",
            "posix-compat.sh",
        ] {
            std::fs::copy(
                repo.join("scripts").join(s),
                primary.join("scripts").join(s),
            )
            .unwrap();
        }
        // `target` and the copy's staging directory (*.tmp) are ignored as
        // the real tree ignores them.
        std::fs::copy(repo.join(".gitignore"), primary.join(".gitignore")).unwrap();
        write(
            &primary.join("Cargo.toml"),
            "[package]\nname = \"fixture-root\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ncell = { path = \"crates/cell\" }\n\n\
             [workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        );
        write(&primary.join("src/lib.rs"), "pub fn fixture() {}\n");
        for name in ["cell", "lone"] {
            write(
                &primary.join(format!("crates/{name}/Cargo.toml")),
                &format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n"),
            );
            write(
                &primary.join(format!("crates/{name}/src/lib.rs")),
                "pub fn f() {}\n",
            );
        }
        write(&primary.join("Cargo.lock"), &fixture_lockfile());
        git(&primary, &["add", "-A"]);
        git(&primary, &["commit", "--quiet", "-m", "init"]);
        git(&primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&primary, &["fetch", "--quiet", "origin"]);
        Self { root, primary }
    }

    fn seed(&self) -> PathBuf {
        self.root.join(".susi-target-seed")
    }

    /// Where `--refresh` builds by default: `<seed>-src`.
    fn seed_src(&self) -> PathBuf {
        self.root.join(".susi-target-seed-src")
    }

    /// The refresh lock: `<seed>.lock`.
    fn seed_lock(&self) -> PathBuf {
        self.root.join(".susi-target-seed.lock")
    }

    /// A snapshot as a full build tree holds one, not yet stripped (a
    /// snapshot this version did not make): two profiles' fingerprints, every
    /// unit's files in deps/ and build/, what cargo copies up into a profile
    /// for the workspace, incremental caches, tmp/, the marker.
    fn make_seed(&self) {
        let seed = self.seed();
        for profile in PROFILES {
            lay_out_profile(&seed.join(profile));
        }
        write(&seed.join("tmp/test-scratch"), "x");
        let rlib = seed.join(format!("debug/deps/libserde-{}.rlib", hash("serde")));
        std::fs::File::options()
            .write(true)
            .open(&rlib)
            .unwrap()
            .set_modified(old_time())
            .unwrap();
        write(&seed.join(".susi-seed"), "feedfacefeedface 1\n");
    }

    fn worktree(&self, name: &str, envs: &[(&str, &str)]) -> (i32, String, String) {
        let mut envs = envs.to_vec();
        envs.push(("SUSI_AGENT", "claude"));
        envs.push(("SUSI_PRIMARY_WATCH_DISABLE", "1"));
        run(
            &self.primary,
            self.primary
                .join("scripts/susi-worktree.sh")
                .to_str()
                .unwrap(),
            &[name],
            &envs,
        )
    }

    /// A worktree made the way the desktop app makes one: nested under the
    /// primary, no script involved.
    fn app_worktree(&self, name: &str) -> PathBuf {
        let dest = self.primary.join(".claude/worktrees").join(name);
        git(
            &self.primary,
            &["worktree", "add", "-q", "-b", name, dest.to_str().unwrap()],
        );
        dest
    }

    fn script(&self) -> PathBuf {
        self.primary.join("scripts/seed-target.sh")
    }

    /// The repository's post-merge, post-commit and post-checkout hooks on
    /// origin/main, and the primary checkout running them (`core.hooksPath
    /// .githooks`, as setup-dev.sh sets it). Their sync-primary call is a
    /// no-op here.
    fn with_hooks(&self) {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        let hooks = self.primary.join(".githooks");
        std::fs::create_dir_all(&hooks).unwrap();
        for hook in ["post-merge", "post-commit", "post-checkout"] {
            std::fs::copy(repo.join(".githooks").join(hook), hooks.join(hook)).unwrap();
        }
        write(&hooks.join("sync-primary"), "#!/bin/sh\nexit 0\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for hook in ["post-merge", "post-commit", "post-checkout", "sync-primary"] {
                std::fs::set_permissions(hooks.join(hook), std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
        }
        git(&self.primary, &["add", "-A"]);
        git(&self.primary, &["commit", "--quiet", "-m", "hooks"]);
        git(&self.primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&self.primary, &["fetch", "--quiet", "origin"]);
        git(&self.primary, &["config", "core.hooksPath", ".githooks"]);
    }

    /// The seeding's gates ([`Gates`]), with a copy into a worktree held
    /// until the test lets it go: what the stand-in copy tool used to do.
    fn gated_copy(&self) -> Gates {
        let gates = Gates::new(&self.root);
        gates.hold("copy");
        gates
    }

    /// `scripts/seed-target.sh <args>` from the primary's scripts: the stub,
    /// which execs the binary under test.
    fn seed_target(&self, args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
        let mut native_args = vec!["workflow", "seed-target"];
        native_args.extend_from_slice(args);
        susi(&self.primary, &native_args, envs)
    }

    fn spawn_seed_target(&self, args: &[&str], envs: &[(&str, &str)]) -> Child {
        let mut native_args = vec!["workflow", "seed-target"];
        native_args.extend_from_slice(args);
        command(
            &self.primary,
            env!("CARGO_BIN_EXE_susi"),
            &native_args,
            envs,
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
    }

    /// A new origin/main, pushed and fetched.
    fn advance_main(&self, file: &str, text: &str) -> String {
        write(&self.primary.join(file), text);
        git(&self.primary, &["add", "-A"]);
        git(&self.primary, &["commit", "--quiet", "-m", "next"]);
        git(&self.primary, &["push", "--quiet", "origin", "HEAD:main"]);
        git(&self.primary, &["fetch", "--quiet", "origin"]);
        git(&self.primary, &["rev-parse", "origin/main"])
    }

    /// A stand-in cargo for `--refresh`: `build` lays out a target the way
    /// cargo does (registry and path units in two profiles, uplifted
    /// binaries, examples, tmp/, and `gone-crate`, which no lockfile names),
    /// after waiting for the file FAKE_CARGO_GATE when that is set; `clippy`
    /// does nothing; anything else (`metadata`) is the real cargo. Each build
    /// is logged in FAKE_CARGO_LOG. Returns the `SUSI_SEED_CARGO` value that
    /// runs it ([`cargo_seam`]); `None` where no bash can.
    fn fake_cargo(&self) -> Option<String> {
        let mut units: Vec<(&str, &str)> = UNITS.to_vec();
        units.push(("gone-crate", "4000000000000001"));
        let units: Vec<String> = units.iter().map(|(p, h)| format!("{p}:{h}")).collect();
        let body = format!(
            r#"case "$1" in
build)
    [ -z "${{FAKE_CARGO_LOG:-}}" ] || echo build >>"$FAKE_CARGO_LOG"
    [ -z "${{FAKE_CARGO_STARTED:-}}" ] || : >"$FAKE_CARGO_STARTED"
    i=0
    while [ -n "${{FAKE_CARGO_GATE:-}}" ] && [ ! -e "$FAKE_CARGO_GATE" ] && [ $i -lt 600 ]; do
        sleep 0.2
        i=$((i + 1))
    done
    for p in {profiles}; do
        for u in {units}; do
            n=${{u%%:*}} h=${{u#*:}} k=$(printf %s "${{u%%:*}}" | tr - _)
            mkdir -p "target/$p/.fingerprint/$n-$h" "target/$p/deps" "target/$p/build/$n-$h/out"
            echo fp >"target/$p/.fingerprint/$n-$h/lib-$n"
            echo rlib >"target/$p/deps/lib$k-$h.rlib"
            echo out >"target/$p/build/$n-$h/out/generated.rs"
        done
        mkdir -p "target/$p/examples"
        echo x >"target/$p/examples/demo-3000000000000001.exe"
        echo x >"target/$p/fixture-root.exe"
        echo x >"target/$p/deps/fixture-root.exe"
        echo x >"target/$p/deps/fixture-root.pdb"
    done
    mkdir -p target/tmp
    echo x >target/tmp/test-scratch
    ;;
clippy) ;;
*) exec "$FAKE_REAL_CARGO" "$@" ;;
esac
"#,
            profiles = PROFILES.join(" "),
            units = units.join(" "),
        );
        fake_tools(&self.root.join("fake-cargo"), &[("cargo", body.as_str())]);
        cargo_seam(&self.root.join("fake-cargo").join("cargo"))
    }
}

impl Drop for World {
    /// A background copy that is still exiting (letting go of its lock) can
    /// hold a file open for a moment on Windows: retry rather than leave the
    /// fixture behind.
    fn drop(&mut self) {
        for _ in 0..20 {
            if std::fs::remove_dir_all(&self.root).is_ok() || !self.root.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

/// Wait until no seeding of the worktree `dest` holds its lock: a background
/// copy has written its target and exited.
fn wait_for_seeders(dest: &Path) {
    let gitdir = PathBuf::from(git(
        dest,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let lock = gitdir.join("susi-seed-target.lockdir");
    let deadline = Instant::now() + Duration::from_secs(60);
    while lock.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        !lock.exists(),
        "a seeding of {dest:?} never let go of its lock"
    );
}

fn cd_target(stdout: &str) -> PathBuf {
    let last = stdout.lines().last().unwrap();
    PathBuf::from(
        last.strip_prefix("cd ")
            .unwrap_or_else(|| panic!("last line must be `cd <path>`: {stdout}")),
    )
}

fn fingerprint(target: &Path, profile: &str, pkg: &str) -> PathBuf {
    target
        .join(profile)
        .join(".fingerprint")
        .join(format!("{pkg}-{}", hash(pkg)))
}

/// Only registry and git units are left in the build tree `target`: in every
/// profile, no fingerprint, deps/ file or build/ directory of a unit built
/// from a path; none of what cargo copies up for the workspace (binaries,
/// libraries, examples); no incremental cache, no tmp/. The registry units
/// are all there.
fn assert_registry_only(target: &Path, path_units: &[&str]) {
    for profile in PROFILES {
        let at = target.join(profile);
        for pkg in path_units {
            let h = UNITS
                .iter()
                .find(|(n, _)| n == pkg)
                .map_or("4000000000000001", |(_, h)| *h);
            let krate = pkg.replace('-', "_");
            for gone in [
                at.join(format!(".fingerprint/{pkg}-{h}")),
                at.join(format!("deps/lib{krate}-{h}.rlib")),
                at.join(format!("build/{pkg}-{h}")),
            ] {
                assert!(
                    !gone.exists(),
                    "{gone:?} is built from a path, or named by no lockfile: it must go"
                );
            }
        }
        for pkg in REGISTRY_UNITS {
            let h = hash(pkg);
            let krate = pkg.replace('-', "_");
            for kept in [
                fingerprint(target, profile, pkg),
                at.join(format!("deps/lib{krate}-{h}.rlib")),
                at.join(format!("build/{pkg}-{h}/out/generated.rs")),
            ] {
                assert!(kept.exists(), "{kept:?} is a registry crate's: it stays");
            }
        }
        for gone in [
            "fixture-root.exe",
            "libcell.rlib",
            "deps/fixture-root.exe",
            "deps/fixture-root.pdb",
            "deps/fixture-root.d",
            "examples",
            "incremental",
        ] {
            assert!(
                !at.join(gone).exists(),
                "{profile}/{gone}: only the workspace's targets are copied up; a stale \
                 one would pass for this branch's"
            );
        }
    }
    assert!(
        !target.join("tmp").exists(),
        "tmp/ is the workspace's tests'"
    );
}

/// What a seeded target holds: the registry crates' builds, with their
/// times, and nothing built from a path; no snapshot marker, no staging left
/// behind.
fn assert_seeded(dest: &Path) {
    let target = dest.join("target");
    assert!(is_real_dir(&target), "{target:?} must be a real directory");
    assert_registry_only(&target, &PATH_UNITS);
    let rlib = target.join(format!("debug/deps/libserde-{}.rlib", hash("serde")));
    let kept = mtime(&rlib)
        .duration_since(old_time())
        .unwrap_or_else(|e| e.duration());
    assert!(
        kept < Duration::from_secs(2),
        "the copy keeps timestamps, or cargo rebuilds everything: {kept:?}"
    );
    assert!(
        !target.join(".susi-seed").exists(),
        "the marker is the snapshot's"
    );
    assert!(
        !target.join(".susi-seed-run").exists(),
        "the copying run's mark is gone"
    );
    assert!(
        target.join(".susi-seeded").is_file(),
        "the target says where it came from"
    );
    assert_no_staging(dest);
}

fn assert_no_staging(dest: &Path) {
    let leftovers: Vec<_> = std::fs::read_dir(dest)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("target.seed-"))
        .collect();
    assert!(leftovers.is_empty(), "staging left behind: {leftovers:?}");
}

/// The snapshot itself is read, never changed.
fn assert_seed_untouched(w: &World) {
    let seed = w.seed();
    for (pkg, _) in UNITS {
        assert!(
            fingerprint(&seed, "debug", pkg).exists(),
            "the snapshot lost {pkg}"
        );
    }
    assert!(seed.join("debug/incremental").exists());
    assert!(seed.join("debug/fixture-root.exe").exists());
    assert!(seed.join(".susi-seed").is_file());
}

#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_new_worktree_gets_its_own_target_seeded_from_the_snapshot() {
    let w = World::new("new");
    w.make_seed();
    let (code, out, err) = w.worktree("worker", &[]);
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert!(!is_link(&dest.join("target")), "no link to a shared target");
    // The start never waits for the copy: it runs in the background, and the
    // agent's first foreground seed-target.sh (AGENTS.md step 1) waits for it.
    assert!(
        err.contains("target: copying") && err.contains("in the background"),
        "it says the copy is under way: {err}"
    );
    let (code, _, seed_err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{seed_err}");
    assert_seeded(&dest);
    assert_seed_untouched(&w);
    // Neither the target nor its staging is ever swept into a commit.
    assert!(git(&dest, &["status", "--porcelain"]).is_empty());
}

/// The migration: a worktree whose target is a link (a junction on Windows)
/// to a shared target gets its own. Only the link goes: everything the shared
/// target holds, every other worktree's builds included, is still there.
#[test]
fn a_linked_target_becomes_a_real_one_and_what_it_pointed_at_survives() {
    let w = World::new("link");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    write(
        &fingerprint(&shared, "debug", "cell").join("lib-cell"),
        "theirs",
    );
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    assert!(is_link(&dest.join("target")));
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("replaced the link"), "{err}");
    assert_seeded(&dest);
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build",
        "the shared target must be left exactly as it was"
    );
    assert!(
        fingerprint(&shared, "debug", "cell")
            .join("lib-cell")
            .is_file(),
        "stripping must never reach through the link"
    );
    assert_seed_untouched(&w);
}

/// A target that holds anything is this worktree's own build: seeding again
/// changes nothing and copies nothing.
#[test]
fn seeding_twice_changes_nothing_and_a_build_is_never_replaced() {
    let w = World::new("twice");
    w.make_seed();
    let dest = w.app_worktree("app");
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert_seeded(&dest);
    // This worktree builds; the seed's copy of serde is replaced by its own.
    let rlib = dest.join(format!("target/debug/deps/libserde-{}.rlib", hash("serde")));
    std::fs::remove_file(&rlib).unwrap();
    write(&dest.join("target/debug/built-here"), "mine");
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "a second run has nothing to say: {err}");
    assert!(dest.join("target/debug/built-here").is_file());
    assert!(!rlib.exists(), "nothing was copied over the build");
    // An app worktree whose target already holds a build is never seeded.
    let other = w.app_worktree("other");
    write(&other.join("target/debug/built-here"), "mine");
    let (code, _, err) = w.seed_target(&[other.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!other.join("target/debug/.fingerprint").exists(), "{err}");
}

/// Tooling (rust-analyzer, an editor) can make an empty `target/`, or one
/// with only cargo's tag files, before anything builds: that is no build.
#[test]
fn an_empty_target_is_seeded() {
    let w = World::new("empty");
    w.make_seed();
    let dest = w.app_worktree("app");
    write(&dest.join("target/CACHEDIR.TAG"), "Signature");
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert_seeded(&dest);
}

#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn susi_no_seed_skips_the_copy_but_still_replaces_a_link() {
    let w = World::new("noseed");
    w.make_seed();
    let (code, out, err) = w.worktree("worker", &[("SUSI_NO_SEED", "1")]);
    assert_eq!(code, 0, "{err}");
    let dest = cd_target(&out);
    assert!(
        is_empty_dir(&dest.join("target")),
        "SUSI_NO_SEED=1: an empty target, built cold"
    );
    // A link is still replaced: the shared target is what is being retired.
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "theirs");
    let app = w.app_worktree("app");
    link_dir(&shared, &app.join("target"));
    let (code, _, err) = w.seed_target(&[app.to_str().unwrap()], &[("SUSI_NO_SEED", "1")]);
    assert_eq!(code, 0, "{err}");
    assert!(is_real_dir(&app.join("target")), "{err}");
    assert!(shared.join("sentinel").is_file());
}

#[test]
fn with_no_snapshot_the_target_is_a_plain_empty_one() {
    let w = World::new("noseedyet");
    let dest = w.app_worktree("app");
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(is_empty_dir(&dest.join("target")));
    assert!(err.contains("no snapshot"), "{err}");
    // SUSI_TARGET_SEED names another snapshot.
    let elsewhere = w.root.join("elsewhere");
    let seed = w.seed();
    w.make_seed();
    std::fs::rename(&seed, &elsewhere).unwrap();
    let other = w.app_worktree("other");
    let (code, _, err) = w.seed_target(
        &[other.to_str().unwrap()],
        &[("SUSI_TARGET_SEED", elsewhere.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{err}");
    assert_seeded(&other);
}

/// The primary checkout's target is nobody's worktree's, and older worktrees
/// may still link to it: it is never touched, nor used as a snapshot.
#[test]
fn the_primary_checkout_is_never_touched_nor_copied() {
    let w = World::new("primary");
    w.make_seed();
    let (code, _, err) = w.seed_target(&[w.primary.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!w.primary.join("target").exists(), "{err}");
    // The primary's (shared) target named as the snapshot is refused, even
    // carrying a snapshot's marker.
    write(
        &fingerprint(&w.primary.join("target"), "debug", "serde").join("lib-serde"),
        "x",
    );
    write(&w.primary.join("target/.susi-seed"), "feedfacefeedface 1\n");
    let dest = w.app_worktree("app");
    let (code, _, err) = w.seed_target(
        &[dest.to_str().unwrap()],
        &[(
            "SUSI_TARGET_SEED",
            w.primary.join("target").to_str().unwrap(),
        )],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        !dest.join("target/debug").exists(),
        "the shared target is no snapshot: {err}"
    );
}

/// A copy that cannot be stripped all the way must never become a target:
/// a member's fingerprint left in it reads as fresh wherever this checkout's
/// files are older than the snapshot, and this branch would build and test
/// origin/main's code. Here a `.fingerprint/` whose entries cannot be removed
/// (the directory is read-only, which the copy keeps), and then a lockfile
/// that names nothing: each time the copy is dropped and the target starts
/// empty, cold and correct.
#[test]
fn a_copy_that_cannot_be_stripped_is_never_installed() {
    let w = World::new("nostrip");
    w.make_seed();
    let fingerprints = w.seed().join("debug/.fingerprint");
    if read_only(&fingerprints, true) {
        let dest = w.app_worktree("app");
        let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
        read_only(&fingerprints, false);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("could not strip"), "{err}");
        assert!(
            is_empty_dir(&dest.join("target")),
            "the unstripped copy must not be installed: {err}"
        );
        // The copy's own read-only directory kept the seeding from removing
        // all of it; the staging directory is dropped as far as it can be,
        // and never renamed into place.
        for left in std::fs::read_dir(&dest).unwrap().filter_map(Result::ok) {
            let name = left.file_name().to_string_lossy().into_owned();
            if name.starts_with("target.seed-") {
                assert!(
                    !left.path().join(SEEDED_MARK_NAME).exists(),
                    "a refused copy is never marked seeded"
                );
                read_only(&left.path().join("debug/.fingerprint"), false);
                std::fs::remove_dir_all(left.path()).unwrap();
            }
        }
        assert_seed_untouched(&w);
    } else {
        eprintln!("skipped the read-only case: no mode bits here");
    }

    // A lockfile that names no package: nothing can be told safe to keep.
    let other = w.app_worktree("other");
    std::fs::write(other.join("Cargo.lock"), "# truncated\n").unwrap();
    let (code, _, err) = w.seed_target(&[other.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("could not strip"), "{err}");
    assert!(is_empty_dir(&other.join("target")), "{err}");
    assert_no_staging(&other);
}

/// Removing a link never recurses through it, and a link that cannot be
/// removed (here the worktree directory is read-only, so nothing in it can be
/// unlinked) is reported and left as it is: the seeding does not go on to
/// copy into a target that is still someone else's. Either way what the link
/// points at survives, down to its spaces.
#[test]
fn a_link_rm_cannot_remove_falls_back_and_never_reaches_through() {
    let w = World::new("rmlink");
    w.make_seed();
    let shared = w.root.join("shared target");
    write(&shared.join("sentinel"), "another worktree's build");
    write(
        &fingerprint(&shared, "debug", "cell").join("lib-cell"),
        "theirs",
    );
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    if read_only(&dest, true) {
        let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
        read_only(&dest, false);
        assert_eq!(code, 0, "{err}");
        assert!(err.contains("could not remove the link"), "{err}");
        assert!(is_link(&dest.join("target")), "{err}");
        assert_eq!(
            std::fs::read_to_string(shared.join("sentinel")).unwrap(),
            "another worktree's build"
        );
        assert_no_staging(&dest);
    }
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("replaced the link"), "{err}");
    assert!(!is_link(&dest.join("target")), "{err}");
    assert_seeded(&dest);
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    assert!(fingerprint(&shared, "debug", "cell")
        .join("lib-cell")
        .is_file());
}

/// The SessionStart hook runs at the start of every agent session and must
/// never make it wait: it replaces a link at once and copies the snapshot in
/// the background. The copy here waits until the test lets it go, so the hook
/// is shown to return while the copy has not finished. A worktree the
/// desktop app made gets its target this way.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_session_start_hook_replaces_a_link_now_and_seeds_in_the_background() {
    let w = World::new("hook");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "theirs");
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    // The copy into the staging directory waits at its gate.
    let gates = w.gated_copy();
    let (gate_var, gate_dir) = gates.env();
    let started = Instant::now();
    let (code, out, err) = run(
        &dest,
        dest.join("scripts/workflow-session-start.sh")
            .to_str()
            .unwrap(),
        &[],
        &[(gate_var, gate_dir.as_str())],
    );
    let returned_after = started.elapsed();
    let said = format!("{out}{err}");
    assert_eq!(code, 0, "{said}");
    assert!(said.contains("replaced the link"), "{said}");
    assert!(said.contains("in the background"), "{said}");
    assert!(!is_link(&dest.join("target")), "{said}");
    assert!(
        !dest.join("target/.susi-seeded").exists(),
        "the hook returned ({returned_after:?}) only once the copy was done: it must not wait for it"
    );
    assert!(shared.join("sentinel").is_file());
    gates.release("copy");
    assert!(
        wait_for(&dest.join("target/.susi-seeded"), 120),
        "the background copy never finished: {:?}",
        std::fs::read_to_string(
            git(&dest, &["rev-parse", "--path-format=absolute", "--git-dir"])
                + "/susi-seed-target.log"
        )
    );
    wait_for_seeders(&dest);
    assert_seeded(&dest);
}

/// A foreground seeding (the finish gate, sync, pre-push, `tasks close`) is
/// about to run cargo. When a background copy into the same worktree is
/// already running (the session-start or post-checkout hook started it), it
/// waits for that copy, neither building cold beside it nor copying a second
/// time, and returns once the target is seeded.
#[test]
fn a_foreground_seeding_waits_for_the_background_copy_already_running() {
    let w = World::new("waits");
    w.make_seed();
    let dest = w.app_worktree("app");
    let gates = w.gated_copy();
    let (gate_var, gate_dir) = gates.env();
    let envs = [(gate_var, gate_dir.as_str())];
    let (code, _, err) = w.seed_target(&["--background", dest.to_str().unwrap()], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("in the background"), "{err}");
    assert!(
        is_real_dir(&dest.join("target")),
        "a real target/ is there at once: {err}"
    );
    assert!(
        gates.wait_for("copy", 60),
        "the background copy never started"
    );
    let mut foreground = w.spawn_seed_target(&[dest.to_str().unwrap()], &envs);
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        foreground.try_wait().unwrap().is_none(),
        "the foreground seeding went ahead while a copy was still running"
    );
    assert!(!dest.join("target/.susi-seeded").exists());
    gates.release("copy");
    let (code, _, err) = finish(foreground.wait_with_output().unwrap());
    assert_eq!(code, 0, "{err}");
    wait_for_seeders(&dest);
    assert!(err.contains("waiting for the copy"), "{err}");
    assert!(
        dest.join("target/.susi-seeded").is_file(),
        "it returns once the target is seeded: {err}"
    );
    assert_seeded(&dest);
    assert_eq!(gates.count("copy"), 1, "one copy, not two");
    assert!(
        !err.contains("copying"),
        "the foreground run copied nothing itself: {err}"
    );
}

/// v0.21.23's `workflow start` linked target/ with `symlink_dir`: on Windows
/// a native directory symlink (Developer Mode lets any user make one), not
/// the junction every other test links with. It is converted the same way,
/// and what it points at stays as it was.
#[test]
fn a_native_directory_symlink_target_is_converted_too() {
    let w = World::new("symlink");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    write(
        &fingerprint(&shared, "debug", "cell").join("lib-cell"),
        "theirs",
    );
    let dest = w.app_worktree("app");
    #[cfg(windows)]
    if let Err(e) = std::os::windows::fs::symlink_dir(&shared, dest.join("target")) {
        eprintln!("skipped: this account cannot make a directory symlink ({e})");
        return;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&shared, dest.join("target")).unwrap();
    assert!(is_link(&dest.join("target")));
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("replaced the link"), "{err}");
    assert_seeded(&dest);
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    assert!(fingerprint(&shared, "debug", "cell")
        .join("lib-cell")
        .is_file());
}

/// On Windows the harness runs the script as `sh`, which can be Git's raw
/// usr/bin shell with System32 ahead of Git on PATH: there a bare `bash` is
/// WSL's and `find` is Windows' own, whose answer to `-mmin` is empty. The
/// stub needs neither (sh and git, then susi, which needs no userland at
/// all), and its POSIX-mode shell is left in place, with no second shell to
/// find. Here a `bash` (and on Windows a `find`) that only record that they
/// ran stand first on PATH. A two-hour-old unsigned lock in the worktree,
/// whose maker died before signing it, must still be taken over (the lock's
/// own age, read by susi, not by `find`), and the linked target converted
/// and seeded.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_raw_shell_with_system32_first_runs_neither_wsls_bash_nor_windows_find() {
    let w = World::new("rawsh");
    w.make_seed();
    // The shell by its absolute path, found on this process's PATH: on Unix a
    // bare name is looked up on the PATH the child is given, where the
    // impostor `bash` below stands first, and the shell would be the impostor.
    let (shell, shell_args): (PathBuf, &[&str]) =
        if let Some(sh) = raw_msys_program(&w.primary, "sh") {
            (sh, &[])
        } else if cfg!(unix) {
            let bash = on_this_path("bash").unwrap_or_else(|| PathBuf::from("/bin/bash"));
            (bash, &["--posix"])
        } else {
            return; // no raw Git shell here
        };
    assert!(shell.is_absolute(), "{shell:?}");
    let ran = w.root.join("impostors-ran");
    let impostors = w.root.join("impostors");
    std::fs::create_dir_all(&impostors).unwrap();
    let tools: &[&str] = if cfg!(windows) {
        &["bash", "find"]
    } else {
        &["bash"]
    };
    for tool in tools {
        let file = impostors.join(tool);
        std::fs::write(
            &file,
            format!(
                "#!/bin/sh\necho {tool} >>'{}'\nexit 127\n",
                ran.to_string_lossy().replace('\\', "/")
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    let mut dirs = vec![impostors.clone()];
    if let Some(root) = std::env::var_os("SystemRoot") {
        dirs.push(PathBuf::from(root).join("System32"));
    }
    dirs.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let path = std::env::join_paths(dirs).unwrap();

    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    let gitdir = PathBuf::from(git(
        &dest,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let lock = gitdir.join("susi-seed-target.lockdir");
    std::fs::create_dir_all(&lock).unwrap();
    age(&lock, 7200);

    let mut cmd = Command::new(&shell);
    scrub(&mut cmd);
    cmd.args(shell_args)
        .arg(w.script())
        .arg(&dest)
        .current_dir(&w.primary)
        .env("PATH", &path)
        .env("MSYS_NO_PATHCONV", "1")
        .env("SUSI_SEED_WAIT_SECS", "6");
    let (code, _, err) = finish(cmd.output().unwrap());
    assert_eq!(code, 0, "{err}");
    assert!(
        !ran.exists(),
        "an impostor ran: {}\n{err}",
        std::fs::read_to_string(&ran).unwrap_or_default()
    );
    assert!(err.contains("replaced the link"), "{err}");
    assert!(
        !err.contains("gave up waiting"),
        "the dead maker's lock must be taken over: {err}"
    );
    assert_seeded(&dest);
    assert!(!lock.exists(), "the lock is let go of after: {err}");
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
}

/// A worktree made before the seeding receives it by merging origin/main,
/// with its target/ still a link to the old shared one; the plain cargo it
/// runs next must not build there. The post-merge hook converts it as the
/// merge lands: the link is gone when `git merge` returns (and what it
/// pointed at stays), and the snapshot is copied in the background, which
/// the merge does not wait for.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_merge_into_a_linked_worktree_converts_its_target_at_once() {
    let w = World::new("postmerge");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    // Made, and linked, before the hooks reach main.
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    w.with_hooks();
    let gates = w.gated_copy();
    let (gate_var, gate_dir) = gates.env();
    let (code, _, err) = run(
        &dest,
        "git",
        &["merge", "--quiet", "--no-edit", "origin/main"],
        &[(gate_var, gate_dir.as_str())],
    );
    assert_eq!(code, 0, "{err}");
    assert!(dest.join(".githooks/post-merge").is_file());
    assert!(
        is_real_dir(&dest.join("target")),
        "the merge left target/ linked: {err}"
    );
    assert!(
        !dest.join("target/.susi-seeded").exists(),
        "the merge waited for the copy: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    gates.release("copy");
    assert!(
        wait_for(&dest.join("target/.susi-seeded"), 120),
        "the background copy never finished: {err}"
    );
    wait_for_seeders(&dest);
    assert_seeded(&dest);
    // A merge into a worktree whose target is its own changes nothing.
    write(&dest.join("target/debug/built-here"), "mine");
    w.advance_main("crates/cell/src/lib.rs", "pub fn g() {}\n");
    let (code, _, err) = run(
        &dest,
        "git",
        &["merge", "--quiet", "--no-edit", "origin/main"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "nothing to say: {err}");
    assert!(dest.join("target/debug/built-here").is_file());
}

/// The merge that brings the seeding may stop on conflicts (this change
/// rewrites files every branch touches), and Git runs no post-merge for it:
/// the commit that concludes it (`git commit`, `git merge --continue`) runs
/// only post-commit. That hook converts the link the same way, at once, with
/// the copy in the background, and what the link pointed at stays as it was.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_conflicted_merge_converts_a_linked_target_when_its_commit_concludes_it() {
    let w = World::new("conflict");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    // Made, and linked, before the hooks reach main; its branch and main
    // then change one line apart.
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    write(&dest.join("src/lib.rs"), "pub fn branch() {}\n");
    git(&dest, &["commit", "--quiet", "-am", "the branch's edit"]);
    w.with_hooks();
    w.advance_main("src/lib.rs", "pub fn main_side() {}\n");
    let gates = w.gated_copy();
    let (gate_var, gate_dir) = gates.env();
    let envs = [(gate_var, gate_dir.as_str())];
    let (code, out, err) = run(&dest, "git", &["merge", "--no-edit", "origin/main"], &envs);
    assert_ne!(code, 0, "the merge must stop on the conflict: {out}{err}");
    assert!(dest.join(".githooks/post-commit").is_file());
    assert!(
        is_link(&dest.join("target")),
        "Git ran a hook for a merge that stopped on conflicts; this test no longer \
         proves the commit that concludes one converts the link: {out}{err}"
    );
    write(&dest.join("src/lib.rs"), "pub fn resolved() {}\n");
    git(&dest, &["add", "src/lib.rs"]);
    let (code, out, err) = run(&dest, "git", &["commit", "--quiet", "--no-edit"], &envs);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        git(&dest, &["rev-parse", "--verify", "-q", "HEAD^2"]).len() >= 40,
        "the commit concluded the merge"
    );
    assert!(
        is_real_dir(&dest.join("target")),
        "the commit that concluded the merge left target/ linked: {err}"
    );
    assert!(err.contains("replaced the link"), "{err}");
    assert!(
        !dest.join("target/.susi-seeded").exists(),
        "the commit waited for the copy: {err}"
    );
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build",
        "the shared target must be left exactly as it was"
    );
    gates.release("copy");
    assert!(
        wait_for(&dest.join("target/.susi-seeded"), 120),
        "the background copy never finished: {err}"
    );
    wait_for_seeders(&dest);
    assert_seeded(&dest);
    assert_eq!(
        std::fs::read_dir(&shared).unwrap().count(),
        1,
        "nothing was added to the shared target"
    );
    // A commit in a worktree whose target is its own seeds nothing, and
    // says nothing about it.
    write(&dest.join("target/debug/built-here"), "mine");
    write(&dest.join("src/lib.rs"), "pub fn later() {}\n");
    let (code, _, err) = run(&dest, "git", &["commit", "--quiet", "-am", "later"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("target:"), "nothing to say: {err}");
    assert!(dest.join("target/debug/built-here").is_file());
}

/// v0.21.23's `susi workflow start` (the installed susi until the next
/// release) runs `git worktree add`, then links the primary checkout's
/// target/ into the new worktree when it has none. `git worktree add` runs
/// the post-checkout hook first, which gives the new worktree a real target/
/// at once (so there is nothing left to link) and seeds it in the
/// background, without making the add wait.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_new_worktree_has_a_real_target_before_an_old_workflow_start_can_link_one() {
    let w = World::new("postcheckout");
    w.make_seed();
    w.with_hooks();
    // The primary's shared target, which v0.21.23 linked.
    write(&w.primary.join("target/sentinel"), "the shared build");
    let gates = w.gated_copy();
    let (gate_var, gate_dir) = gates.env();
    let dest = w.root.join("susi-old-start");
    let (code, _, err) = run(
        &w.primary,
        "git",
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "old-start",
            dest.to_str().unwrap(),
            "origin/main",
        ],
        &[(gate_var, gate_dir.as_str())],
    );
    assert_eq!(code, 0, "{err}");
    // What v0.21.23 does next.
    let target = dest.join("target");
    if !target.exists() {
        link_dir(&w.primary.join("target"), &target);
    }
    assert!(
        is_real_dir(&target),
        "the new worktree's target/ must be its own before anything can link it: {err}"
    );
    assert!(
        !target.join(".susi-seeded").exists(),
        "`git worktree add` waited for the copy: {err}"
    );
    gates.release("copy");
    assert!(
        wait_for(&target.join(".susi-seeded"), 120),
        "the background copy never finished: {err}"
    );
    wait_for_seeders(&dest);
    assert_seeded(&dest);
    assert_eq!(
        std::fs::read_to_string(w.primary.join("target/sentinel")).unwrap(),
        "the shared build"
    );
    // A plain checkout in a worktree whose target is its own does nothing.
    let (code, _, err) = run(&dest, "git", &["checkout", "-q", "-b", "other"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "nothing to say: {err}");
}

/// Age never ends a lock holder whose liveness can be told. A refresh at
/// low priority on a busy host can build for hours; taken over, a second
/// refresh would check out, clean and build in the same checkout and target
/// under the first. A holder of this process table (a susi, by its pid, and
/// on Windows by its image) keeps the lock while it lives, however old its
/// line; once it is gone, the lock is taken at once, however young. A line
/// whose pid lives but names another table is not read as this table's.
#[test]
fn a_live_holder_keeps_the_lock_however_old_and_a_gone_one_loses_it_at_once() {
    let w = World::new("liveness");
    w.make_seed();
    let main = git(&w.primary, &["rev-parse", "origin/main"]);
    // A current snapshot: a refresh that gets the lock has nothing to do.
    write(&w.seed().join(".susi-seed"), &format!("{main} {}\n", now()));
    // A live susi: a seeding of a worktree, held at its first gate.
    let dest = w.app_worktree("app");
    let gates = Gates::new(&w.root);
    gates.hold("lock");
    let (gate_var, gate_dir) = gates.env();
    let mut holder = command(
        &w.primary,
        env!("CARGO_BIN_EXE_susi"),
        &["workflow", "seed-target", dest.to_str().unwrap()],
        &[(gate_var, gate_dir.as_str())],
    )
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    assert!(gates.wait_for("lock", 60), "the holder never started");
    let pid = holder.id().to_string();
    let wpid = if cfg!(windows) {
        pid.clone()
    } else {
        "-".to_string()
    };
    let runtime = seed::runtime_name();
    let lock = w.seed_lock();
    let hold = |pid: &str, since: u64, wpid: &str, runtime: &str| {
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(
            lock.join("owner"),
            format!("{pid} {since} {wpid} {runtime}\n"),
        )
        .unwrap();
    };
    let ancient = now() - 200_000;
    let stale = [("SUSI_SEED_LOCK_STALE", "60")];
    for recorded in ["-", wpid.as_str()] {
        hold(&pid, ancient, recorded, &runtime);
        let (code, out, err) = w.seed_target(&["--refresh"], &stale);
        assert_eq!(code, 0, "{err}");
        assert!(
            out.contains("a refresh is already running"),
            "a live holder (Windows pid {recorded}) keeps the lock, however old: {out}{err}"
        );
    }
    // The same pid in another install's table, with no Windows pid to ask
    // about: it cannot be told, so only age ends it.
    hold(&pid, ancient, "-", "C:/Program Files/Another Git");
    let (_, out, _) = w.seed_target(&["--refresh"], &stale);
    assert!(out.contains("already origin/main"), "{out}");
    assert!(!lock.exists());
    gates.release("lock");
    holder.wait().unwrap();
    wait_for_seeders(&dest);
    hold(&pid, now(), &wpid, &runtime);
    let (code, out, err) = w.seed_target(&["--refresh"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("already origin/main"),
        "a gone holder's lock is taken at once, however young: {out}{err}"
    );
    assert!(!lock.exists(), "and let go of after");
}

/// The ways the installed susi (v0.21.23) names a checkout before it joins
/// `scripts\<shim>` onto it with `Path::join`: as `git rev-parse` prints it
/// (forward slashes, so the joined path is mixed: `F:/w\scripts\x.sh`) and as
/// a native path (all backslashes). On Windows `$0` is then a path whose last
/// separators are backslashes. Elsewhere a path has one spelling.
fn as_v0_21_23_names(dir: &Path) -> Vec<PathBuf> {
    let forward = PathBuf::from(dir.to_string_lossy().replace('\\', "/"));
    if cfg!(windows) {
        let native = PathBuf::from(dir.to_string_lossy().replace('/', "\\"));
        vec![forward, native]
    } else {
        vec![forward]
    }
}

/// `<dir>/scripts/<script>` joined as Rust joins it, checked to end the way
/// v0.21.23's does on this platform.
fn joined_script(dir: &Path, script: &str) -> PathBuf {
    let path = dir.join("scripts").join(script);
    if cfg!(windows) {
        assert!(
            path.to_string_lossy()
                .ends_with(&format!("\\scripts\\{script}")),
            "{path:?} is not the path v0.21.23 runs"
        );
    }
    path
}

/// What v0.21.23's `tasks close` runs a cargo acceptance with in the
/// worktree `ws`: Git's bash ([`posix_shell_command`], unchanged since) with
/// `ws`'s own `scripts\target-guard.sh`, as Rust joins it, then `--run --
/// <argv>`, in `ws`. `None` where no Git shell is found: `tasks close` then
/// runs no guard.
///
/// [`posix_shell_command`]: susi_gawd::admin::tasks::posix_shell_command
fn v0_21_23_close(ws: &Path, argv: &[&str]) -> Option<Command> {
    let mut command = susi_gawd::admin::tasks::posix_shell_command("bash");
    if cfg!(windows) && !Path::new(command.get_program()).is_absolute() {
        return None;
    }
    scrub(&mut command);
    command
        .arg(joined_script(ws, "target-guard.sh"))
        .arg("--run")
        .arg("--")
        .args(argv)
        .current_dir(ws);
    Some(command)
}

/// The installed susi (v0.21.23) still runs link-shared-target.sh after its
/// `workflow start`, which still links: the retired script seeds instead. Its
/// harness runs it as `sh <repo>\scripts\link-shared-target.sh <worktree>`
/// in the primary checkout, the script's path joined by Rust: the script must
/// find seed-target.sh beside it whatever slashes that path has, and turn the
/// link into a real, seeded target, leaving what it pointed at alone.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_retired_link_script_seeds_instead_of_linking() {
    let w = World::new("oldlink");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    let dest = w.app_worktree("app");
    for repo in as_v0_21_23_names(&w.primary) {
        let linker = joined_script(&repo, "link-shared-target.sh");
        // What v0.21.23's `workflow start` leaves before it runs the script.
        link_dir(&shared, &dest.join("target"));
        let mut harness = Command::new("sh");
        scrub(&mut harness);
        let out = harness
            .arg(&linker)
            .arg(&dest)
            .current_dir(&repo)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let (code, out, err) = finish(out);
        assert_eq!(code, 0, "{linker:?}: {out}{err}");
        assert!(err.contains("replaced the link"), "{linker:?}: {err}");
        assert!(!is_link(&dest.join("target")), "{linker:?}: {err}");
        assert_seeded(&dest);
        assert_eq!(
            std::fs::read_to_string(shared.join("sentinel")).unwrap(),
            "another worktree's build",
            "{linker:?}: the shared target must be left exactly as it was"
        );
        std::fs::remove_dir_all(dest.join("target")).unwrap();
    }
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1);
}

/// `--background` runs this script again, by its own path, for the copy. A
/// caller may name it the way Rust joins a path, which on Windows is
/// `<dir>\scripts\seed-target.sh`: the copy once ran `<scripts dir>/<that
/// whole path>`, which does not exist, and the target was left empty.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_background_seeding_named_by_a_joined_path_still_copies() {
    let w = World::new("bgpath");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    for (n, repo) in as_v0_21_23_names(&w.primary).into_iter().enumerate() {
        let dest = w.app_worktree(&format!("app{n}"));
        link_dir(&shared, &dest.join("target"));
        let script = joined_script(&repo, "seed-target.sh");
        let mut cmd = Command::new("sh");
        scrub(&mut cmd);
        let out = cmd
            .arg(&script)
            .arg("--background")
            .arg(&dest)
            .current_dir(&dest)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let (code, out, err) = finish(out);
        assert_eq!(code, 0, "{script:?}: {out}{err}");
        assert!(err.contains("in the background"), "{script:?}: {err}");
        assert!(is_real_dir(&dest.join("target")), "{script:?}: {err}");
        let gitdir = git(&dest, &["rev-parse", "--path-format=absolute", "--git-dir"]);
        assert!(
            wait_for(&dest.join("target/.susi-seeded"), 120),
            "{script:?}: the background copy never finished: {:?}",
            std::fs::read_to_string(gitdir + "/susi-seed-target.log")
        );
        wait_for_seeders(&dest);
        assert_seeded(&dest);
    }
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1);
}

/// The guard is retired, kept one release as a pass-through for the
/// installed susi and older branches: `--run` runs its command and keeps its
/// status, with the caller's own PATH (on Windows the shim puts /usr/bin
/// first for itself), the rest hold nothing, and nothing is written into a
/// target that holds a build. Every caller is about to run cargo, so a
/// worktree still linked to the old shared target gets its own first, seeded,
/// before the command runs, and what the link pointed at is left alone. Run
/// as each caller runs it: v0.21.23's `tasks close` (a path Rust joined,
/// whatever its slashes), and an older branch's gate or pre-push
/// (`"$root/scripts/target-guard.sh" --hold $$`, a path its shell made).
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_retired_guard_runs_the_command_and_converts_a_linked_target_first() {
    let w = World::new("guard");
    w.make_seed();
    let dest = w.app_worktree("app");
    write(&dest.join("target/debug/built-here"), "mine");
    let Some(mut close) = v0_21_23_close(&dest, &["sh", "-c", "echo ran > ran; exit 4"]) else {
        eprintln!("skipped: no Git shell here, so v0.21.23's `tasks close` runs no guard");
        return;
    };
    let (code, out, err) = finish(close.output().unwrap());
    assert_eq!(code, 4, "--run keeps the command's status: {out}{err}");
    assert!(dest.join("ran").is_file());
    let root = dest.to_string_lossy().replace('\\', "/");
    let guard = format!("{root}/scripts/target-guard.sh");
    for args in [&["--hold", "1"][..], &["--release", "1"], &[]] {
        let (code, out, err) = run(&dest, &guard, args, &[]);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(out.is_empty(), "{args:?} holds nothing to report: {out}");
        assert!(err.is_empty(), "{args:?}: {err}");
    }
    // The command `--run` passes through gets the caller's PATH back,
    // unchanged: what the shim needs for itself is not the caller's.
    let (code, out, err) = run(
        &dest,
        "sh",
        &[
            "-c",
            "caller=$PATH\n\
             got=$(\"${BASH:-bash}\" \"$1\" --run sh -c 'printf %s \"$PATH\"') || exit 9\n\
             if [ \"$got\" = \"$caller\" ]; then echo same; else printf 'caller: %s\\ngot: %s\\n' \"$caller\" \"$got\"; fi",
            "path-probe",
            &guard,
        ],
        &[("MSYS_NO_PATHCONV", "1")],
    );
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(out, "same", "--run must restore the caller's PATH: {err}");
    let built: Vec<_> = walk(&dest.join("target"));
    assert_eq!(
        built,
        vec![dest.join("target/debug/built-here")],
        "the guard writes nothing into a target that holds a build"
    );
    std::fs::remove_dir_all(dest.join("target")).unwrap();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    let same = |what: &str| {
        assert_eq!(
            std::fs::read_to_string(shared.join("sentinel")).unwrap(),
            "another worktree's build",
            "{what}: the shared target must be left exactly as it was"
        );
        assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1, "{what}");
    };
    // v0.21.23's `tasks close`: the acceptance runs in a target of the
    // worktree's own, already seeded.
    let saw = dest.join("saw");
    for ws in as_v0_21_23_names(&dest) {
        link_dir(&shared, &dest.join("target"));
        let _ = std::fs::remove_file(&saw);
        let mut close = v0_21_23_close(
            &ws,
            &[
                "sh",
                "-c",
                "test -d target && ! test -L target && test -f target/.susi-seeded && echo own > saw",
            ],
        )
        .unwrap();
        let what = format!("{close:?}");
        let (code, out, err) = finish(close.output().unwrap());
        assert_eq!(code, 0, "{what}: {out}{err}");
        assert!(err.contains("replaced the link"), "{what}: {err}");
        assert!(!is_link(&dest.join("target")), "{what}: {err}");
        assert_eq!(
            std::fs::read_to_string(&saw).unwrap_or_default().trim(),
            "own",
            "{what}: the command must run in the worktree's own, seeded target: {err}"
        );
        assert_seeded(&dest);
        same(&what);
        std::fs::remove_dir_all(dest.join("target")).unwrap();
    }
    // An older branch's gate or pre-push still holds (`--hold`): converted
    // too, with nothing on stdout, which that caller reads.
    link_dir(&shared, &dest.join("target"));
    let (code, out, err) = run(&dest, &guard, &["--hold", "1"], &[]);
    assert_eq!(code, 0, "--hold: {out}{err}");
    assert!(out.is_empty(), "--hold: the report goes to stderr: {out}");
    assert!(err.contains("replaced the link"), "--hold: {err}");
    assert_seeded(&dest);
    same("--hold");
}

/// The docs tell people to run these scripts directly (`scripts/seed-target.sh
/// --refresh --force`, `scripts/susi-worktree.sh`), and so do older branches'
/// gates (`"$root/scripts/target-guard.sh" --hold $$`). A file Git records
/// without its executable bit is checked out without it on Linux and macOS,
/// and running it there fails with "Permission denied"; Git Bash on Windows
/// runs it anyway, which hides the mistake. So the index must record 100755.
#[test]
fn the_scripts_people_run_directly_are_executable() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    for script in [
        "scripts/seed-target.sh",
        "scripts/setup-sccache.sh",
        "scripts/susi-worktree.sh",
        "scripts/target-guard.sh",
        "scripts/link-shared-target.sh",
        "scripts/workflow-session-start.sh",
    ] {
        assert!(repo.join(script).is_file(), "{script} is missing");
        let (code, staged, err) = run(repo, "git", &["ls-files", "-s", "--", script], &[]);
        // A source tree that is no git checkout has no index to ask.
        if code == 0 && !staged.is_empty() {
            assert!(
                staged.starts_with("100755 "),
                "{script} is recorded as `{staged}`: run `git update-index --chmod=+x {script}`"
            );
        } else {
            eprintln!("{script}: no index entry to check ({err})");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(repo.join(script))
                .unwrap()
                .permissions()
                .mode();
            assert!(mode & 0o111 != 0, "{script} is not executable");
        }
    }
}

/// `--refresh` is the snapshot's one writer: it builds origin/main with the
/// real cargo in a clone of its own, never the primary, and publishes by
/// rename a snapshot stripped to registry and git crates (this fixture has
/// none, so nothing of the build stays), marked with the commit; a current
/// snapshot, or a young one, is left as it is, and a directory it did not
/// make is never replaced.
#[test]
fn refresh_builds_origin_main_apart_and_publishes_a_stripped_snapshot() {
    let w = World::new("refresh");
    // A lockfile `--locked` accepts, in place of the fixture's hand-made one.
    let (code, _, err) = run(
        &w.primary,
        env!("CARGO"),
        &["generate-lockfile", "--offline"],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    git(&w.primary, &["commit", "--quiet", "-am", "a real lockfile"]);
    git(&w.primary, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&w.primary, &["fetch", "--quiet", "origin"]);
    let primary_before = git(&w.primary, &["status", "--porcelain"]);
    // Caller output settings must not redirect fingerprints out of the owned
    // seed checkout. This directory is part of this fixture, not user state.
    let outer = w.root.join("hostile-outer-cargo-output");
    write(&outer.join("sentinel"), "keep outer output unchanged\n");
    let hostile_outputs = [
        ("CARGO_TARGET_DIR", outer.to_str().unwrap()),
        ("CARGO_BUILD_TARGET_DIR", outer.to_str().unwrap()),
        ("CARGO_BUILD_BUILD_DIR", outer.to_str().unwrap()),
    ];
    let (code, out, err) = w.seed_target(&["--refresh"], &hostile_outputs);
    assert_eq!(code, 0, "{out}{err}");
    let main = git(&w.primary, &["rev-parse", "origin/main"]);
    let marker = std::fs::read_to_string(w.seed().join(".susi-seed")).unwrap_or_default();
    assert!(
        marker.starts_with(&main),
        "the snapshot names origin/main ({main}): {marker:?}\n{out}{err}"
    );
    let src = w.seed_src();
    assert!(
        fingerprint_named(&src.join("target/debug"), "cell"),
        "it builds the workspace in a checkout of its own\n{out}"
    );
    assert!(
        src.join(".git/susi-seed-src").is_file(),
        "a checkout it marks as its own"
    );
    assert!(
        w.seed().join("debug/.fingerprint").is_dir(),
        "the snapshot keeps the profile's layout\n{out}"
    );
    for pkg in ["cell", "fixture-root", "lone"] {
        assert!(
            !fingerprint_named(&w.seed().join("debug"), pkg),
            "the snapshot holds no workspace build ({pkg})\n{out}"
        );
    }
    let top: Vec<_> = std::fs::read_dir(w.seed().join("debug"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name())
        .filter(|n| !n.to_string_lossy().starts_with('.'))
        .collect();
    assert!(
        top.is_empty(),
        "nothing copied up for the workspace: {top:?}"
    );
    assert!(!w.primary.join("target").exists(), "never in the primary");
    assert_eq!(
        std::fs::read_to_string(outer.join("sentinel")).unwrap(),
        "keep outer output unchanged\n"
    );
    assert_eq!(
        std::fs::read_dir(&outer).unwrap().count(),
        1,
        "no Cargo metadata or fingerprints may escape into caller output"
    );
    assert_eq!(git(&w.primary, &["status", "--porcelain"]), primary_before);
    assert_eq!(staging_dirs(&w), Vec::<String>::new());
    assert!(!w.seed_lock().exists(), "the lock is let go");
    // Current: nothing to do.
    let (code, out, _) = w.seed_target(&["--refresh"], &hostile_outputs);
    assert_eq!(code, 0);
    assert!(out.contains("already origin/main"), "{out}");
    // A newer origin/main, but a young snapshot: it waits, unless forced.
    let next = w.advance_main("crates/cell/src/lib.rs", "pub fn g() {}\n");
    let (_, out, _) = w.seed_target(&["--refresh"], &hostile_outputs);
    assert!(out.contains("refreshed after"), "{out}");
    let (code, out, err) = w.seed_target(&["--refresh", "--force"], &hostile_outputs);
    assert_eq!(code, 0, "{out}{err}");
    let marker = std::fs::read_to_string(w.seed().join(".susi-seed")).unwrap();
    assert!(marker.starts_with(&next), "{marker:?}\n{out}{err}");
    // A snapshot directory no refresh made is never replaced.
    std::fs::remove_dir_all(w.seed()).unwrap();
    write(&w.seed().join("debug/someone-elses"), "x");
    let (_, out, _) = w.seed_target(&["--refresh", "--force"], &hostile_outputs);
    assert!(out.contains("not a snapshot a refresh made"), "{out}");
    assert!(w.seed().join("debug/someone-elses").is_file());
    assert_eq!(
        std::fs::read_dir(&outer).unwrap().count(),
        1,
        "forced refresh also stays inside its owned checkout"
    );
}

/// `--refresh` checks out, cleans and builds only in a clone it made itself.
/// Named the primary checkout, a worktree of this clone, a path inside one,
/// or any directory it did not make, it refuses, says so, and changes
/// nothing there: no detached HEAD, no untracked file cleaned away, no build.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn refresh_never_builds_in_the_primary_or_a_checkout_it_did_not_make() {
    let w = World::new("refusesrc");
    let Some(fake) = w.fake_cargo() else {
        eprintln!("skipped: no bash to run the stand-in cargo");
        return;
    };
    let builds = w.root.join("builds");
    let envs = |src: &Path| -> Vec<(String, String)> {
        vec![
            ("SUSI_SEED_CARGO".into(), fake.clone()),
            ("FAKE_REAL_CARGO".into(), env!("CARGO").into()),
            ("FAKE_CARGO_LOG".into(), builds.to_str().unwrap().into()),
            ("SUSI_TARGET_SEED_SRC".into(), src.to_str().unwrap().into()),
        ]
    };
    let refresh = |src: &Path| {
        let envs = envs(src);
        let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        w.seed_target(&["--refresh"], &envs)
    };
    write(&w.primary.join("UNTRACKED-NOTES.txt"), "mine");
    let head = git(&w.primary, &["rev-parse", "HEAD"]);
    let (code, out, err) = refresh(&w.primary);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("refusing to build in") && out.contains("primary checkout"),
        "{out}"
    );
    assert!(w.primary.join("UNTRACKED-NOTES.txt").is_file(), "{out}");
    assert_eq!(git(&w.primary, &["branch", "--show-current"]), "main");
    assert_eq!(git(&w.primary, &["rev-parse", "HEAD"]), head);
    assert!(!w.primary.join("target").exists());

    let wt = w.app_worktree("busy");
    write(&wt.join("UNTRACKED-NOTES.txt"), "mine");
    let (_, out, _) = refresh(&wt);
    assert!(out.contains("inside a checkout of this clone"), "{out}");
    assert!(wt.join("UNTRACKED-NOTES.txt").is_file());
    assert_eq!(git(&wt, &["branch", "--show-current"]), "busy");

    let (_, out, _) = refresh(&w.primary.join("nested/src"));
    assert!(out.contains("inside a checkout of this clone"), "{out}");
    assert!(!w.primary.join("nested").exists());

    let other = w.root.join("someone-elses");
    std::fs::create_dir_all(&other).unwrap();
    git(&other, &["init", "--quiet", "-b", "theirs"]);
    write(&other.join("UNTRACKED-NOTES.txt"), "theirs");
    let (_, out, _) = refresh(&other);
    assert!(out.contains("did not make it"), "{out}");
    assert!(other.join("UNTRACKED-NOTES.txt").is_file());

    assert!(!builds.exists(), "nothing was built anywhere");
    assert!(!w.seed().exists(), "nothing was published");
}

/// Two refreshes at once (the watcher's and a hand-run `--refresh --force`):
/// one builds, the other sees the lock and leaves; one snapshot is
/// published, stripped to registry and git crates, and no staging directory
/// or lock is left behind.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn two_refreshes_at_once_build_once_and_publish_one_stripped_snapshot() {
    let w = World::new("together");
    let Some(fake) = w.fake_cargo() else {
        eprintln!("skipped: no bash to run the stand-in cargo");
        return;
    };
    let gate = w.root.join("build-gate");
    let started = w.root.join("build-started");
    let builds = w.root.join("builds");
    let envs = [
        ("SUSI_SEED_CARGO", fake.as_str()),
        ("FAKE_REAL_CARGO", env!("CARGO")),
        ("FAKE_CARGO_GATE", gate.to_str().unwrap()),
        ("FAKE_CARGO_STARTED", started.to_str().unwrap()),
        ("FAKE_CARGO_LOG", builds.to_str().unwrap()),
    ];
    let first = w.spawn_seed_target(&["--refresh"], &envs);
    assert!(wait_for(&started, 120), "the first refresh never built");
    let (code, out, err) = w.seed_target(&["--refresh", "--force"], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("a refresh is already running"), "{out}{err}");
    std::fs::write(&gate, "go").unwrap();
    let (code, out, err) = finish(first.wait_with_output().unwrap());
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("published"), "{out}{err}");
    assert_eq!(
        std::fs::read_to_string(&builds).unwrap().lines().count(),
        1,
        "one build"
    );
    let main = git(&w.primary, &["rev-parse", "origin/main"]);
    assert!(std::fs::read_to_string(w.seed().join(".susi-seed"))
        .unwrap()
        .starts_with(&main));
    let mut stripped: Vec<&str> = PATH_UNITS.to_vec();
    stripped.push("gone-crate");
    assert_registry_only(&w.seed(), &stripped);
    assert_eq!(staging_dirs(&w), Vec::<String>::new());
    assert!(!w.seed_lock().exists());
    // A worktree seeded from it gets exactly that.
    let dest = w.app_worktree("app");
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("target: seeded"), "{err}");
    assert_registry_only(&dest.join("target"), &stripped);
}

/// The refresh lock holds across Git installs. A holder another runtime
/// recorded (its pid means nothing to this one's process table, and here no
/// Windows pid was recorded to ask about) is presumed alive until the lock is
/// stale, then taken over; staging directories are swept only when the run
/// that owns them is gone, never a live one's.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_refresh_lock_and_staging_hold_across_git_installs() {
    let w = World::new("runtimes");
    let main = git(&w.primary, &["rev-parse", "origin/main"]);
    // A current snapshot: a refresh that gets the lock has nothing to do.
    write(&w.seed().join("debug/.fingerprint/.keep"), "");
    write(&w.seed().join(".susi-seed"), &format!("{main} {}\n", now()));
    let lock = w.seed_lock();
    std::fs::create_dir_all(&lock).unwrap();
    let other = "C:/Program Files/Another Git";
    std::fs::write(lock.join("owner"), format!("4242 {} - {other}\n", now())).unwrap();
    let (code, out, err) = w.seed_target(&["--refresh"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("a refresh is already running"),
        "a live holder from another runtime keeps the lock: {out}"
    );
    std::fs::write(
        lock.join("owner"),
        format!("4242 {} - {other}\n", now() - 30_000),
    )
    .unwrap();
    let (code, out, err) = w.seed_target(&["--refresh"], &[("SUSI_SEED_LOCK_STALE", "3600")]);
    assert_eq!(code, 0, "{err}");
    assert!(
        out.contains("already origin/main"),
        "a stale lock is taken: {out}"
    );
    assert!(!lock.exists(), "and let go of after");
    if cfg!(windows) {
        // A Windows pid that runs no shell (4 is the System process): the
        // holder is gone, whatever its age.
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(lock.join("owner"), format!("4242 {} 4 {other}\n", now())).unwrap();
        let (_, out, _) = w.seed_target(&["--refresh"], &[]);
        assert!(out.contains("already origin/main"), "{out}");
    }

    // Staging: one whose run is gone (stale), one whose run lives on in
    // another runtime. A refresh sweeps the first and keeps the second.
    let Some(fake) = w.fake_cargo() else {
        eprintln!("skipped the staging sweep: no bash to run the stand-in cargo");
        return;
    };
    let gone = w.root.join(".susi-target-seed.new.4242-1");
    let live = w.root.join(".susi-target-seed.new.4343-2");
    let old = w.root.join(".susi-target-seed.old.4444-3");
    write(&gone.join(".susi-seed-run"), &format!("4242 1 - {other}\n"));
    write(
        &live.join(".susi-seed-run"),
        &format!("4343 {} - {other}\n", now()),
    );
    write(&old.join(".susi-seed-run"), &format!("4444 1 - {other}\n"));
    w.advance_main("crates/cell/src/lib.rs", "pub fn h() {}\n");
    let (code, out, err) = w.seed_target(
        &["--refresh", "--force"],
        &[
            ("SUSI_SEED_CARGO", fake.as_str()),
            ("FAKE_REAL_CARGO", env!("CARGO")),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("published"), "{out}");
    assert!(!gone.exists(), "a gone run's staging is swept");
    assert!(!old.exists(), "a gone run's old snapshot is swept");
    assert!(live.exists(), "a live run's staging is never touched");
}

/// The build checkout's target only grows (every version bump renames most
/// workspace units), so it starts over when what every unit's hash depends
/// on changes (Cargo.lock here), or once it is twice its size after its last
/// fresh build; otherwise it is kept, and the next build is incremental.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_build_checkouts_target_starts_over_when_its_inputs_change_or_it_doubles() {
    let w = World::new("bounded");
    let Some(fake) = w.fake_cargo() else {
        eprintln!("skipped: no bash to run the stand-in cargo");
        return;
    };
    let envs = [
        ("SUSI_SEED_CARGO", fake.as_str()),
        ("FAKE_REAL_CARGO", env!("CARGO")),
    ];
    let (_, out, err) = w.seed_target(&["--refresh"], &envs);
    assert!(out.contains("published"), "{out}{err}");
    let src = w.seed_src();
    let mark = src.join("target/an-earlier-build");
    write(&mark, "x");
    // Same inputs, normal size: kept.
    w.advance_main("crates/cell/src/lib.rs", "pub fn g() {}\n");
    let (_, out, _) = w.seed_target(&["--refresh", "--force"], &envs);
    assert!(out.contains("published"), "{out}");
    assert!(mark.exists(), "an unchanged build checkout is kept: {out}");
    // Recorded as more than twice its fresh size: started over.
    let state = src.join(".git/susi-seed-src.state");
    let line = std::fs::read_to_string(&state).unwrap();
    let inputs = line.split_whitespace().next().unwrap().to_string();
    std::fs::write(&state, format!("{inputs} 100 201\n")).unwrap();
    w.advance_main("crates/cell/src/lib.rs", "pub fn h() {}\n");
    let (_, out, _) = w.seed_target(&["--refresh", "--force"], &envs);
    assert!(out.contains("over twice its fresh"), "{out}");
    assert!(!mark.exists(), "{out}");
    // A new lockfile: started over.
    write(&mark, "x");
    let lock = std::fs::read_to_string(w.primary.join("Cargo.lock")).unwrap();
    w.advance_main("Cargo.lock", &format!("{lock}\n"));
    let (_, out, _) = w.seed_target(&["--refresh", "--force"], &envs);
    assert!(
        out.contains("Cargo.lock, the toolchain or rustc changed"),
        "{out}"
    );
    assert!(!mark.exists(), "{out}");
    assert!(out.contains("published"), "{out}");
}

fn staging_dirs(w: &World) -> Vec<String> {
    std::fs::read_dir(&w.root)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.starts_with(".susi-target-seed.new") || n.starts_with(".susi-target-seed.old")
        })
        .collect()
}

fn fingerprint_named(profile: &Path, pkg: &str) -> bool {
    std::fs::read_dir(profile.join(".fingerprint")).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            name.strip_prefix(pkg)
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|hash| hash.len() == 16)
        })
    })
}

/// Make `path` executable where that is a mode bit.
fn executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// `git worktree remove` may run while a hook's background copy into that
/// worktree runs (here the copy waits at a gate until the worktree is gone).
/// The copy must bring nothing back: robocopy makes its destination's
/// parents, and the copy's failure paths once made the target with its
/// parents, which made the removed worktree's directory again; a later
/// `workflow start` or scripts/susi-worktree.sh of that name then refused it
/// as "an unrelated checkout". Nothing shared is touched either.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_worktree_removed_while_its_copy_runs_is_never_brought_back() {
    let w = World::new("removed");
    w.make_seed();
    w.with_hooks();
    write(&w.primary.join("target/sentinel"), "the shared build");
    let gates = w.gated_copy();
    let (gate_var, gate_dir) = gates.env();
    let envs = [(gate_var, gate_dir.as_str())];
    let dest = w.root.join("susi-removed");
    let (code, _, err) = run(
        &w.primary,
        "git",
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "removed",
            dest.to_str().unwrap(),
            "origin/main",
        ],
        &envs,
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        gates.wait_for("copy", 60),
        "the background copy never started: {err}"
    );
    let admin = PathBuf::from(git(
        &dest,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    assert!(admin.is_dir());
    let (code, _, err) = run(
        &w.primary,
        "git",
        &["worktree", "remove", "--force", dest.to_str().unwrap()],
        &[],
    );
    assert_eq!(code, 0, "{err}");
    assert!(
        !dest.exists() && !admin.exists(),
        "git removed the worktree"
    );
    gates.release("copy");
    assert!(gates.wait_for("done", 120), "the copy never ended");
    assert!(
        !dest.exists(),
        "the copy brought the removed worktree's directory back: {:?}",
        walk(&dest)
    );
    assert!(
        !admin.exists(),
        "the copy brought the removed worktree's git dir back: {:?}",
        walk(&admin)
    );
    assert_eq!(
        std::fs::read_to_string(w.primary.join("target/sentinel")).unwrap(),
        "the shared build"
    );
    assert_seed_untouched(&w);
    // The name is free again (SUSI_NO_SEED: nothing is copied behind the test).
    let (code, _, err) = run(
        &w.primary,
        "git",
        &["worktree", "add", "-q", dest.to_str().unwrap(), "removed"],
        &[("SUSI_NO_SEED", "1")],
    );
    assert_eq!(code, 0, "{err}");
}

/// A worktree removed after its seeding started and before that seeding took
/// its lock, which is in the worktree's git dir: the lock is made without its
/// parents, so `.git/worktrees/<name>` does not come back. The seeding is
/// held at its `lock` gate, right before it first makes anything under
/// `.git/worktrees/`, and the worktree is removed then, as `git worktree
/// remove` running at that moment would.
#[test]
fn a_worktree_removed_before_its_lock_is_taken_is_never_brought_back() {
    let w = World::new("lockgone");
    w.make_seed();
    let dest = w.app_worktree("app");
    let admin = PathBuf::from(git(
        &dest,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let gates = Gates::new(&w.root);
    gates.hold("lock");
    let (gate_var, gate_dir) = gates.env();
    let seeding = w.spawn_seed_target(&[dest.to_str().unwrap()], &[(gate_var, gate_dir.as_str())]);
    assert!(
        gates.wait_for("lock", 60),
        "the seeding never reached its lock"
    );
    git(
        &w.primary,
        &["worktree", "remove", "--force", dest.to_str().unwrap()],
    );
    assert!(
        !dest.exists() && !admin.exists(),
        "git removed the worktree"
    );
    gates.release("lock");
    let (code, _, err) = finish(seeding.wait_with_output().unwrap());
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("was removed"), "{err}");
    assert!(
        !admin.exists(),
        "the seeding made the removed worktree's git dir again: {err}"
    );
    assert!(!dest.exists(), "{err}");
    assert_seed_untouched(&w);
}

/// A snapshot whose profile is a link (here `debug`, linked to another
/// tree's) is never copied: it holds another tree's builds, and a copy that
/// keeps the link (`cp -a`) had its cleanup empty that tree's incremental/
/// through it. The other tree is left exactly as it was, and the worktree's
/// target starts empty (cold, and correct).
#[test]
fn a_snapshot_whose_profile_is_a_link_is_never_copied_nor_reached_through() {
    let w = World::new("linkedprofile");
    w.make_seed();
    let elsewhere = w.root.join("elsewhere");
    write(
        &elsewhere.join("debug/incremental/cell-1abc/s-1/dep-graph.bin"),
        "their cache",
    );
    write(
        &fingerprint(&elsewhere, "debug", "serde").join("lib-serde"),
        "theirs",
    );
    std::fs::remove_dir_all(w.seed().join("debug")).unwrap();
    link_dir(&elsewhere.join("debug"), &w.seed().join("debug"));
    let before = walk(&elsewhere);
    let dest = w.app_worktree("app");
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("no snapshot to copy"), "{err}");
    assert!(is_empty_dir(&dest.join("target")), "{err}");
    assert_no_staging(&dest);
    assert_eq!(
        walk(&elsewhere),
        before,
        "the linked tree must be left exactly as it was"
    );
}

/// The copy's cleanup ([`seed::drop_extras`]: what robocopy's /XD and /XF
/// leave out of a copy `cp -a` made) never removes anything through a link
/// the copy holds. Its old `rm -rf "$1"/*/incremental` followed a profile
/// that is a link (`debug` here) and emptied the linked tree's incremental/;
/// the real profiles' caches, the strip's and guard's state and the markers
/// still go.
#[test]
fn the_copys_cleanup_never_reaches_through_a_link() {
    let root = std::env::temp_dir().join(format!("susi-seed-cleanup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let elsewhere = root.join("elsewhere");
    let cache = elsewhere.join("debug/incremental/cell-1abc/s-1/dep-graph.bin");
    write(&cache, "their cache");
    let copy = root.join("copy");
    for file in [
        "x86_64-unknown-linux-gnu/debug/incremental/a/b",
        "release/incremental/a/b",
        "release/deps/libserde-2000000000000001.rlib",
        ".susi-seed",
        ".susi-seeded",
        ".susi-strip/remove",
        ".susi-guard/stamps",
    ] {
        write(&copy.join(file), "x");
    }
    link_dir(&elsewhere.join("debug"), &copy.join("debug"));
    seed::drop_extras(&copy);
    assert!(
        cache.is_file(),
        "the cleanup reached through the link and emptied the other tree's cache"
    );
    assert!(is_link(&copy.join("debug")), "the link itself is left");
    for gone in [
        "x86_64-unknown-linux-gnu/debug/incremental",
        "release/incremental",
        ".susi-seed",
        ".susi-seeded",
        ".susi-strip",
        ".susi-guard",
    ] {
        assert!(!copy.join(gone).exists(), "{gone} must go");
    }
    assert!(copy
        .join("release/deps/libserde-2000000000000001.rlib")
        .is_file());
    let _ = std::fs::remove_dir_all(&root);
}

/// Only a snapshot `--refresh` published is ever copied, and the snapshot
/// and its build clone sit outside the names worktrees get
/// (`<primary>-<name>`). A worktree named `target-seed` once sat exactly
/// where the snapshot was looked for: with a fingerprint in its own target,
/// it was copied, sources, target and `.git` file, into every new worktree's
/// target, and a refresh refused to replace it; one named `target-seed-src`
/// sat where the refresh builds, and kept any snapshot from being built.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn only_a_published_snapshot_is_copied_never_a_checkout() {
    let w = World::new("onlyseed");
    let (code, out, err) = w.worktree("target-seed", &[]);
    assert_eq!(code, 0, "{err}");
    let named = cd_target(&out);
    assert_eq!(
        named.file_name().unwrap().to_string_lossy(),
        "susi-target-seed"
    );
    write(
        &fingerprint(&named.join("target"), "debug", "serde").join("lib-serde"),
        "x",
    );
    let (code, out, err) = w.worktree("target-seed-src", &[]);
    assert_eq!(code, 0, "{err}");
    let named_src = cd_target(&out);
    let dest = w.app_worktree("app");
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("no snapshot at"), "{err}");
    assert!(
        is_empty_dir(&dest.join("target")),
        "a checkout was copied as the snapshot: {:?}",
        walk(&dest.join("target"))
    );
    // Named outright, a checkout is no snapshot, even with the marker and a
    // profile right at its top.
    write(&named.join(".susi-seed"), "feedfacefeedface 1\n");
    write(
        &fingerprint(&named, "debug", "serde").join("lib-serde"),
        "x",
    );
    let other = w.app_worktree("other");
    let (code, _, err) = w.seed_target(
        &[other.to_str().unwrap()],
        &[("SUSI_TARGET_SEED", named.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("no snapshot to copy"), "{err}");
    assert!(is_empty_dir(&other.join("target")), "{err}");
    // Nor is a build tree `--refresh` did not publish (no marker).
    w.make_seed();
    std::fs::remove_file(w.seed().join(".susi-seed")).unwrap();
    let third = w.app_worktree("third");
    let (code, _, err) = w.seed_target(&[third.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("no snapshot to copy"), "{err}");
    assert!(is_empty_dir(&third.join("target")), "{err}");
    std::fs::remove_dir_all(w.seed()).unwrap();
    // A refresh builds and publishes beside those worktrees, never in them.
    let Some(fake) = w.fake_cargo() else {
        eprintln!("skipped the refresh: no bash to run the stand-in cargo");
        return;
    };
    let (code, out, err) = w.seed_target(
        &["--refresh"],
        &[
            ("SUSI_SEED_CARGO", fake.as_str()),
            ("FAKE_REAL_CARGO", env!("CARGO")),
        ],
    );
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("published"), "{out}");
    assert!(w.seed().join(".susi-seed").is_file(), "{out}");
    assert!(w.seed_src().join(".git/susi-seed-src").is_file(), "{out}");
    assert_eq!(
        git(&named, &["branch", "--show-current"]),
        "target-seed",
        "{out}"
    );
    assert_eq!(
        git(&named_src, &["branch", "--show-current"]),
        "target-seed-src",
        "{out}"
    );
    assert!(!named_src.join("target/debug").exists(), "{out}");
}

/// The finish gate's own functions, run with a stand-in cargo that records
/// what target/ it finds: in a worktree still linked to the old shared
/// target, the gate's first cargo already finds a target of the worktree's
/// own, seeded from the snapshot, and what the link pointed at is left
/// alone. A text check (the seeding before `cargo fmt` in the function) would
/// pass with the call in a branch the gate does not take.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_finish_gate_seeds_a_linked_target_before_its_first_cargo() {
    let w = World::new("gate");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    // The plan the gate resolves (`ci-changed-crates.sh --plan`): one crate
    // to test and lint, so the gate takes its clippy-then-test branch.
    let changed = dest.join("scripts/ci-changed-crates.sh");
    write(&changed, "#!/bin/sh\nprintf 'test:cell\\nlint:cell\\n'\n");
    executable(&changed);
    let gate = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/parallel-workflow.sh"),
    )
    .unwrap();
    let log = w.root.join("cargo-log");
    let driver = w.root.join("gate.sh");
    shell_script(
        &driver,
        &format!(
            r#"set -euo pipefail
root=$1
cd "$root"
cargo() {{
    if [ -d target ] && [ ! -L target ] && [ -f target/.susi-seeded ]; then s=own; else s=shared; fi
    echo "cargo $1 $s" >>"$GATE_LOG"
}}
gate_stamp() {{ echo stamp; }}
gate_tests() {{ cargo test "$@"; }}
gate_tests_kept() {{ return 0; }}
gate_format() {{ cargo fmt --all --check; }}
{}{}{}{}{}{}gate
"#,
            shell_function(&gate, "timed_step"),
            shell_function(&gate, "seed_own_target"),
            shell_function(&gate, "resolve_gate_plan"),
            shell_function(&gate, "gate"),
            shell_function(&gate, "gate_test_steps"),
            shell_function(&gate, "gate_lint_and_test"),
        ),
    );
    let (code, out, err) = run(
        &w.root,
        driver.to_str().unwrap(),
        &[&slashes(&dest)],
        &[
            ("GATE_LOG", &slashes(&log)),
            ("SUSI_GATE_NOCACHE", "1"),
            ("MSYS_NO_PATHCONV", "1"),
        ],
    );
    assert_eq!(code, 0, "{out}{err}");
    let cargo = std::fs::read_to_string(&log).unwrap_or_default();
    // Clippy and the tests overlap in the gate, so their order in the log is
    // not fixed; what matters is that each cargo found the seeded target.
    let mut cargos: Vec<&str> = cargo.lines().collect();
    cargos.sort_unstable();
    assert_eq!(
        cargos,
        ["cargo clippy own", "cargo fmt own", "cargo test own"],
        "every cargo of the gate must find the worktree's own, seeded target: {err}"
    );
    assert!(err.contains("replaced the link"), "{err}");
    assert_seeded(&dest);
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1);
}

/// v0.21.23's `tasks close` run right after `workflow start`: the
/// post-checkout hook's background copy is still filling an empty target/
/// when the acceptance runs through the retired guard. The guard waits for
/// that copy, as the docs now tell an agent to before its first plain cargo
/// (`scripts/seed-target.sh`), so the acceptance finds the seeded target
/// instead of building cold beside the copy, which would then be thrown
/// away. One copy, not two.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_retired_guard_waits_for_the_copy_a_hook_started() {
    let w = World::new("guardwaits");
    w.make_seed();
    let dest = w.app_worktree("app");
    let gates = w.gated_copy();
    let (gate_var, gate_dir) = gates.env();
    let envs = [(gate_var, gate_dir.as_str())];
    let (code, _, err) = w.seed_target(&["--background", dest.to_str().unwrap()], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(is_empty_dir(&dest.join("target")), "{err}");
    assert!(
        gates.wait_for("copy", 60),
        "the background copy never started"
    );
    let Some(mut close) = v0_21_23_close(
        &dest,
        &["sh", "-c", "test -f target/.susi-seeded && echo own > saw"],
    ) else {
        gates.release("copy");
        wait_for_seeders(&dest);
        eprintln!("skipped: no Git shell here, so v0.21.23's `tasks close` runs no guard");
        return;
    };
    let mut child = close
        .env(gate_var, gate_dir.as_str())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        child.try_wait().unwrap().is_none(),
        "the acceptance went ahead while the copy into its target was still running"
    );
    gates.release("copy");
    let (code, out, err) = finish(child.wait_with_output().unwrap());
    assert_eq!(code, 0, "{out}{err}");
    assert!(err.contains("waiting for the copy"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dest.join("saw"))
            .unwrap_or_default()
            .trim(),
        "own",
        "the acceptance must find the seeded target: {err}"
    );
    wait_for_seeders(&dest);
    assert_seeded(&dest);
    assert_eq!(gates.count("copy"), 1, "one copy, not two");
}

/// For one release a branch cut before the seeding (no seed-target.sh until
/// it merges origin/main) may run the new pre-push hook through the clone's
/// hooksPath, with its target/ still the shared one: the hook holds that
/// branch's own guard through clippy, as the hook the branch carries does,
/// and lets go after. A branch with the seeder seeds instead, and never runs
/// its guard (there a pass-through).
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn the_pre_push_hook_holds_an_old_branchs_own_guard_only_without_a_seeder() {
    let root = std::env::temp_dir().join(format!("susi-seed-prepush-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let bare = root.join("server.git");
    let repo = root.join("susi");
    std::fs::create_dir_all(&bare).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
    git(&repo, &["init", "--quiet", "-b", "main"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]);
    write(&repo.join("README.md"), "fixture\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "init"]);
    git(&repo, &["push", "--quiet", "origin", "HEAD:main"]);
    git(&repo, &["fetch", "--quiet", "origin"]);
    git(&repo, &["switch", "--quiet", "-c", "work"]);
    write(&repo.join("src/lib.rs"), "pub fn f() {}\n");
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "--quiet", "-m", "rust"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    // The hook under test, beside stand-ins for what it calls first.
    let hooks = repo.join(".githooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-push");
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".githooks/pre-push"),
        &hook,
    )
    .unwrap();
    for (name, body) in [
        ("workflow-guard", "#!/bin/sh\ncat >/dev/null\nexit 0\n"),
        ("sync-primary", "#!/bin/sh\nexit 0\n"),
    ] {
        write(&hooks.join(name), body);
        executable(&hooks.join(name));
    }
    let scripts = repo.join("scripts");
    write(
        &scripts.join("ci-changed-crates.sh"),
        "#!/bin/sh\necho cell\n",
    );
    executable(&scripts.join("ci-changed-crates.sh"));
    let log = root.join("log");
    let state = root.join("guard-state");
    let guard = scripts.join("target-guard.sh");
    let write_guard = |body: &str| {
        write(&guard, body);
        executable(&guard);
    };
    write_guard(
        "#!/bin/sh\n\
         case \"$1\" in\n\
         --hold) echo \"hold $2\" >>\"$HOOK_LOG\"; echo held >\"$GUARD_STATE\"; echo \"$2\" ;;\n\
         --release) echo \"release $2\" >>\"$HOOK_LOG\"; echo free >\"$GUARD_STATE\" ;;\n\
         *) echo \"guard $*\" >>\"$HOOK_LOG\" ;;\n\
         esac\n",
    );
    let path = fake_tools(
        &root.join("fake"),
        &[(
            "cargo",
            "s=none\n[ ! -f \"$GUARD_STATE\" ] || read -r s <\"$GUARD_STATE\"\necho \"cargo $1 $s\" >>\"$HOOK_LOG\"\n",
        )],
    );
    let push = || {
        std::fs::write(&log, "").unwrap();
        std::fs::write(&state, "free\n").unwrap();
        let mut cmd = script_command(&hook);
        scrub(&mut cmd);
        let mut child = cmd
            .arg("origin")
            .arg(bare.to_str().unwrap())
            .current_dir(&repo)
            .env("PATH", &path)
            .env("HOOK_LOG", slashes(&log))
            .env("GUARD_STATE", slashes(&state))
            .env("MSYS_NO_PATHCONV", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().unwrap();
            writeln!(stdin, "refs/heads/work {head} refs/heads/work {head}").unwrap();
        }
        let (code, out, err) = finish(child.wait_with_output().unwrap());
        let log = std::fs::read_to_string(&log).unwrap();
        let state = std::fs::read_to_string(&state).unwrap();
        (code, format!("{out}\n{err}"), log, state.trim().to_string())
    };

    // An old branch: its own guard holds the target through clippy.
    let (code, said, log, state) = push();
    assert_eq!(code, 0, "{said}\n{log}");
    let lines: Vec<&str> = log.lines().collect();
    let holder = lines
        .first()
        .and_then(|l| l.strip_prefix("hold "))
        .unwrap_or_else(|| {
            panic!("the hook must hold the old branch's guard first:\n{log}\n{said}")
        });
    let clippy = lines
        .iter()
        .position(|l| l.starts_with("cargo clippy"))
        .unwrap_or_else(|| panic!("clippy must run:\n{log}"));
    assert_eq!(lines[clippy], "cargo clippy held", "{log}");
    let release = format!("release {holder}");
    assert!(
        lines[clippy + 1..].contains(&release.as_str()),
        "the hold is let go after clippy, by the pid that took it:\n{log}"
    );
    assert_eq!(state, "free", "the target is left free:\n{log}");

    // A branch that carries the seeder: it seeds, and the guard never runs.
    write(
        &scripts.join("seed-target.sh"),
        "echo \"seed $1\" >>\"$HOOK_LOG\"\n",
    );
    write_guard("#!/bin/sh\necho \"guard $*\" >>\"$HOOK_LOG\"\nexit 97\n");
    let (code, said, log, _) = push();
    assert_eq!(code, 0, "{said}\n{log}");
    let lines: Vec<&str> = log.lines().collect();
    assert!(
        lines.first().is_some_and(|l| l.starts_with("seed ")),
        "the hook seeds first:\n{log}"
    );
    assert!(lines.iter().any(|l| l.starts_with("cargo clippy")), "{log}");
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("guard") || l.starts_with("hold")),
        "a branch with the seeder never runs its guard:\n{log}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The strip ([`seed::strip_units`]) deletes nothing through a link anywhere
/// between the tree it strips and what it removes. It once checked only a
/// profile and its .fingerprint, and a target triple's directory that is a
/// link (to another tree) had that tree's path units, uplifted binaries,
/// examples and incremental cache deleted through it. A tree with a link
/// where cargo keeps builds (cargo makes none: a copy has one only from `cp
/// -a`, which `--refresh` uses on Linux and macOS, or from a snapshot changed
/// by hand) is refused before anything goes, and the caller drops it
/// ([`seed::discard`], which removes the link only). Also a link that leads
/// to a tree with no fingerprints, which nothing else saw: installed, cargo
/// would build into it.
#[test]
fn the_strip_never_deletes_through_a_link_in_the_tree() {
    let root = std::env::temp_dir().join(format!("susi-seed-striplink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let checkout = root.join("checkout");
    write(&checkout.join("Cargo.lock"), &fixture_lockfile());
    // Where the link is in the copy, and what it leads to in the other tree.
    for (n, (link, to)) in [
        ("x86_64-unknown-linux-gnu", ""),
        ("x86_64-unknown-linux-gnu/debug", "debug"),
        ("release", "loose"),
    ]
    .into_iter()
    .enumerate()
    {
        let theirs = root.join(format!("theirs-{n}"));
        lay_out_profile(&theirs.join("debug"));
        // Build output with no fingerprints: no strip looks at it.
        write(
            &theirs.join("loose/deps/libcell-1000000000000001.rlib"),
            "x",
        );
        write(
            &theirs.join("loose/incremental/cell-1abc/s-1/dep-graph.bin"),
            "x",
        );
        let copy = root.join(format!("copy-{n}"));
        lay_out_profile(&copy.join("debug"));
        std::fs::create_dir_all(copy.join(link).parent().unwrap()).unwrap();
        let to = if to.is_empty() {
            theirs.clone()
        } else {
            theirs.join(to)
        };
        link_dir(&to, &copy.join(link));
        let before = walk(&theirs);
        let stripped = seed::strip_units(&copy, &checkout);
        assert!(
            stripped.is_err(),
            "{link}: a tree with a link where cargo keeps builds must be refused: {stripped:?}"
        );
        seed::discard(&copy);
        assert_eq!(
            walk(&theirs),
            before,
            "{link}: the other tree must be left exactly as it was"
        );
        assert!(!copy.exists(), "{link}: the refused copy is dropped");
    }
    // A tree with no such link strips down to the registry units, and says
    // how many went: the three members and the path dependency, in each of
    // the two profiles, nothing else.
    let copy = root.join("copy-plain");
    for profile in PROFILES {
        lay_out_profile(&copy.join(profile));
    }
    write(&copy.join("tmp/test-scratch"), "x");
    assert_eq!(seed::strip_units(&copy, &checkout), Ok(8));
    assert_registry_only(&copy, &PATH_UNITS);
    let _ = std::fs::remove_dir_all(&root);
}

/// A snapshot with a link where cargo keeps builds is never copied: a
/// profile that leads to a tree with no fingerprints (which no fingerprint
/// check sees, and which a copy that keeps links installs, for cargo to build
/// into), or a target triple's directory. The worktree's target starts
/// empty, and the other tree is left as it was.
#[test]
fn a_snapshot_with_a_link_where_cargo_builds_is_never_copied() {
    for (n, (link, to)) in [("release", "loose"), ("x86_64-unknown-linux-gnu", "")]
        .into_iter()
        .enumerate()
    {
        let w = World::new(&format!("seedlink{n}"));
        w.make_seed();
        let theirs = w.root.join("theirs");
        lay_out_profile(&theirs.join("debug"));
        write(
            &theirs.join("loose/deps/libcell-1000000000000001.rlib"),
            "x",
        );
        write(
            &theirs.join("loose/incremental/cell-1abc/s-1/dep-graph.bin"),
            "x",
        );
        let at = w.seed().join(link);
        if at.exists() {
            std::fs::remove_dir_all(&at).unwrap();
        }
        let to = if to.is_empty() {
            theirs.clone()
        } else {
            theirs.join(to)
        };
        link_dir(&to, &at);
        let before = walk(&theirs);
        let dest = w.app_worktree("app");
        let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[]);
        assert_eq!(code, 0, "{link}: {err}");
        assert!(err.contains("no snapshot to copy"), "{link}: {err}");
        assert!(is_empty_dir(&dest.join("target")), "{link}: {err}");
        assert_no_staging(&dest);
        assert_eq!(walk(&theirs), before, "{link}: {err}");
    }
}

/// `git worktree remove` running as the finished copy is renamed into
/// place: its delete has listed the worktree's entries (and removed them,
/// the `.git` file with them, and the worktree's git dir) before the rename
/// adds `target/`, so the directory is left behind holding a full target, and
/// a later `workflow start` or scripts/susi-worktree.sh of that name refuses
/// it as "an unrelated checkout". The seeding is held at its `rename` gate
/// and that removal is done then. The seeding checks the worktree again after
/// the rename and takes the target back: nothing is left.
#[test]
fn a_worktree_removed_as_its_copy_is_renamed_into_place_is_left_removed() {
    let w = World::new("renamegone");
    w.make_seed();
    let dest = w.app_worktree("app");
    let admin = PathBuf::from(git(
        &dest,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let gates = Gates::new(&w.root);
    gates.hold("rename");
    let (gate_var, gate_dir) = gates.env();
    let seeding = w.spawn_seed_target(&[dest.to_str().unwrap()], &[(gate_var, gate_dir.as_str())]);
    assert!(
        gates.wait_for("rename", 120),
        "the copy was never about to be renamed into place"
    );
    // What `git worktree remove` leaves when it listed the directory before
    // the rename: everything but the finished copy, and the git dir, gone.
    for entry in std::fs::read_dir(&dest).unwrap().filter_map(Result::ok) {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("target.seed-")
        {
            continue;
        }
        let path = entry.path();
        if is_real_dir(&path) {
            std::fs::remove_dir_all(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
    }
    std::fs::remove_dir_all(&admin).unwrap();
    gates.release("rename");
    let (code, _, err) = finish(seeding.wait_with_output().unwrap());
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("was removed while the snapshot"), "{err}");
    assert!(
        !dest.exists(),
        "the removed worktree's directory was left holding a target: {:?}",
        walk(&dest)
    );
    assert!(!admin.exists(), "{err}");
    assert_seed_untouched(&w);
}

/// A copy into `<worktree>/target` is for the cargo the caller runs next,
/// with the caller's environment. With `CARGO_TARGET_DIR` (or
/// `CARGO_BUILD_TARGET_DIR`) naming another directory, that cargo builds
/// there: nothing is copied (about a minute, read by nothing), and nothing is
/// made where cargo builds; a link is still replaced, the link only. Named
/// as the worktree's own target, however spelled, the copy is made. `tasks
/// close` and the retired guard run the script with the environment their
/// cargo runs with.
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn a_cargo_target_dir_elsewhere_gets_no_copy() {
    let w = World::new("targetdir");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    let elsewhere = w.root.join("elsewhere-target");
    let dest = w.app_worktree("app");
    let target = dest.join("target");
    for var in ["CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR"] {
        link_dir(&shared, &target);
        let (code, _, err) = w.seed_target(
            &[dest.to_str().unwrap()],
            &[(var, elsewhere.to_str().unwrap())],
        );
        assert_eq!(code, 0, "{var}: {err}");
        assert!(err.contains("replaced the link"), "{var}: {err}");
        assert!(err.contains("nothing is copied"), "{var}: {err}");
        assert!(is_empty_dir(&target), "{var}: {err}");
        assert!(!elsewhere.exists(), "{var}: {err}");
        std::fs::remove_dir(&target).unwrap();
    }
    // v0.21.23's `tasks close`, through the retired guard: its acceptance
    // runs, and nothing is copied for it.
    if let Some(mut close) = v0_21_23_close(&dest, &["sh", "-c", "echo ran > ran"]) {
        close.env("CARGO_TARGET_DIR", &elsewhere);
        let (code, out, err) = finish(close.output().unwrap());
        assert_eq!(code, 0, "{out}{err}");
        assert!(dest.join("ran").is_file(), "{err}");
        assert!(err.contains("nothing is copied"), "{err}");
        assert!(is_empty_dir(&target), "{err}");
        std::fs::remove_dir(&target).unwrap();
    }
    // The worktree's own target, by its full path (on Windows as Rust joins
    // it) or relative to where cargo runs: copied.
    let own = target.to_string_lossy().into_owned();
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[("CARGO_TARGET_DIR", &own)]);
    assert_eq!(code, 0, "{err}");
    assert_seeded(&dest);
    std::fs::remove_dir_all(&target).unwrap();
    let (code, _, err) = run(
        &dest,
        w.script().to_str().unwrap(),
        &[dest.to_str().unwrap()],
        &[("CARGO_TARGET_DIR", "target")],
    );
    assert_eq!(code, 0, "{err}");
    assert_seeded(&dest);
    assert!(!elsewhere.exists());
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    assert_eq!(std::fs::read_dir(&shared).unwrap().count(), 1);
}

/// The primary checkout is never seeded, and its target/ is the old shared
/// one: worktrees made before the seeding still link it and build into it
/// until each next merges origin/main. A cargo in the primary (`workflow
/// check` with no installed susi) can then run a crate one of them built;
/// cargo's lock keeps the builds intact, not apart. The script the docs have
/// an agent run there first says so while any worktree links that target,
/// and changes nothing; it is silent once none does, and a link to anything
/// else is no concern of the primary's.
#[test]
fn the_primary_checkout_says_while_worktrees_still_share_its_target() {
    let w = World::new("primaryshared");
    w.make_seed();
    write(
        &w.primary.join("target/debug/sentinel"),
        "the primary's build",
    );
    let (code, _, err) = w.seed_target(&[], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "no worktree links it yet: {err}");
    let old = w.app_worktree("old");
    link_dir(&w.primary.join("target"), &old.join("target"));
    let other = w.app_worktree("other");
    write(&w.root.join("not-the-primary/sentinel"), "x");
    link_dir(&w.root.join("not-the-primary"), &other.join("target"));
    let own = w.app_worktree("own");
    write(&own.join("target/debug/built-here"), "mine");
    let (code, _, err) = w.seed_target(&[], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(
        err.contains("still the shared target of 1 worktree(s)") && err.contains("worktrees/old"),
        "{err}"
    );
    assert!(err.contains("installed susi"), "{err}");
    assert!(
        is_link(&old.join("target")),
        "the primary's run converts no worktree"
    );
    assert!(is_link(&other.join("target")));
    assert_eq!(
        walk(&w.primary.join("target")),
        vec![w.primary.join("target/debug/sentinel")],
        "the primary's target is left as it was"
    );
    // Once that worktree has a target of its own, there is nothing to say.
    let (code, _, err) = w.seed_target(&[old.to_str().unwrap()], &[]);
    assert_eq!(code, 0, "{err}");
    assert_seeded(&old);
    let (code, _, err) = w.seed_target(&[], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(
        std::fs::read_to_string(w.primary.join("target/debug/sentinel")).unwrap(),
        "the primary's build"
    );
}

/// A background copier is an orphan once its `--background` parent exits,
/// and where PID 1 or the subreaper never reaps (a Linux container with no
/// init) one that died ends a zombie, which `kill -0` still finds. A lock
/// such a holder left must not stand: a seeding that finds it must read the
/// holder as gone and take the lock at once, not wait out the two minutes
/// and build cold. Here `sleep 0` is the zombie: its parent becomes `sleep
/// 8` (`exec`), which never reaps it, and its pid signs the worktree's lock.
/// Run on Unix only: Git Bash's runtime keeps such a process findable in
/// some runs and not in others, so there the test would prove nothing;
/// orphans that nothing reaps are a Linux runner's problem.
#[test]
#[cfg_attr(
    not(unix),
    ignore = "Git Bash keeps an unreaped process findable only in some runs"
)]
fn the_copy_watcher_counts_an_unreaped_process_as_ended() {
    let w = World::new("zombie");
    w.make_seed();
    let dest = w.app_worktree("app");
    let pidfile = w.root.join("zombie");
    let mut parent = command(
        &w.root,
        "sh",
        &[
            "-c",
            "sleep 0 & echo $! >\"$1.tmp\" && mv \"$1.tmp\" \"$1\"; exec sleep 30",
            "zombie",
            pidfile.to_str().unwrap(),
        ],
        &[],
    )
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .unwrap();
    assert!(wait_for(&pidfile, 10), "the zombie's pid was never written");
    let zombie = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .to_string();
    assert!(zombie.parse::<u64>().is_ok(), "no pid: {zombie}");
    let gitdir = PathBuf::from(git(
        &dest,
        &["rev-parse", "--path-format=absolute", "--git-dir"],
    ));
    let lock = gitdir.join("susi-seed-target.lockdir");
    std::fs::create_dir_all(&lock).unwrap();
    std::fs::write(
        lock.join("owner"),
        format!("{zombie} {} - {}\n", now(), seed::runtime_name()),
    )
    .unwrap();
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &[("SUSI_SEED_WAIT_SECS", "6")]);
    let _ = parent.kill();
    let _ = parent.wait();
    assert_eq!(code, 0, "{err}");
    assert!(
        !err.contains("waiting for the copy") && !err.contains("gave up waiting"),
        "an exited holder nothing reaps must count as gone: {err}"
    );
    assert_seeded(&dest);
    assert!(!lock.exists(), "the lock is let go of after: {err}");
}

/// Until a release that carries the verb is installed, the stub can find no
/// susi to exec: the installed one is older (its `--help` probe fails), and a
/// worktree just made has no build of its own. It must still do what cannot
/// wait for a release: unlink a target/ that is still a link to the clone's
/// old shared target (the link only), and make a missing target/ as an empty
/// directory, so neither the caller's cargo nor v0.21.23's `workflow start`
/// (which links a target/ that does not exist) builds in the shared target.
/// Nothing is copied, the primary checkout is never touched, and `--refresh`
/// has nothing to fall back on.
#[test]
// Executes a shell-only compatibility fallback using a shebang fixture.
#[cfg(unix)]
fn the_stub_still_unlinks_and_makes_an_empty_target_without_a_capable_susi() {
    let w = World::new("stubfallback");
    w.make_seed();
    // An installed release from before the verb: it refuses `workflow
    // seed-target --help`, as clap does with a usage error.
    let old = w.root.join("old-susi");
    write(&old, "#!/bin/sh\nexit 2\n");
    executable(&old);
    // No capable susi anywhere on PATH either: cargo puts this checkout's
    // target/debug (the binary under test) on a test's PATH, and a developer
    // may have one installed, so every directory holding a susi is left out.
    let mut path = vec![w.root.join("nowhere")];
    path.extend(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter(|d| !d.join("susi").exists() && !d.join("susi.exe").exists()),
    );
    let path = std::env::join_paths(path).unwrap();
    let envs: Vec<(&str, &str)> = vec![
        ("SUSI_WORKFLOW_BIN", old.to_str().unwrap()),
        ("PATH", path.to_str().unwrap()),
    ];
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    let (code, _, err) = w.seed_target(&[dest.to_str().unwrap()], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("no susi with"), "{err}");
    assert!(err.contains("replaced the link"), "{err}");
    assert!(
        is_empty_dir(&dest.join("target")),
        "an empty target of the worktree's own, nothing copied: {err}"
    );
    assert_no_staging(&dest);
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    // A worktree with no target/ at all, through the hooks' `--background`.
    let other = w.app_worktree("other");
    let (code, _, err) = w.seed_target(&["--background", other.to_str().unwrap()], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(is_empty_dir(&other.join("target")), "{err}");
    // The primary checkout keeps the target it has, and gets none.
    let (code, _, err) = w.seed_target(&[], &envs);
    assert_eq!(code, 0, "{err}");
    assert!(!w.primary.join("target").exists(), "{err}");
    // A build is never touched, and --refresh does nothing.
    write(&other.join("target/debug/built-here"), "mine");
    let (code, _, _) = w.seed_target(&[other.to_str().unwrap()], &envs);
    assert_eq!(code, 0);
    assert!(other.join("target/debug/built-here").is_file());
    let (code, _, _) = w.seed_target(&["--refresh", "--force"], &envs);
    assert_eq!(code, 0);
    assert!(!w.seed_src().exists());
    assert_seed_untouched(&w);
}

/// The detached copy is this very binary, which has the verb, not whatever
/// `SUSI_WORKFLOW_BIN` names: the stub passes over a release older than the
/// verb and execs a checkout's build, but the environment reaches the binary
/// unchanged, and spawning that release would report a copy under way and
/// leave the target cold. And a relative `CARGO_TARGET_DIR` is the
/// worktree's, not this process's: `workflow start` seeds from the primary
/// checkout for a cargo that runs after its `cd`.
#[test]
fn the_background_copy_is_this_binary_and_a_relative_target_dir_is_the_worktrees() {
    let w = World::new("selfspawn");
    w.make_seed();
    let old = w.root.join("old-susi");
    write(&old, "#!/bin/sh\nexit 2\n");
    executable(&old);
    let dest = w.app_worktree("app");
    let (code, _, err) = susi(
        &w.primary,
        &[
            "workflow",
            "seed-target",
            "--background",
            dest.to_str().unwrap(),
        ],
        &[("SUSI_WORKFLOW_BIN", old.to_str().unwrap())],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("in the background"), "{err}");
    assert!(
        wait_for(&dest.join("target/.susi-seeded"), 120),
        "the detached copy never ran: {err}"
    );
    wait_for_seeders(&dest);
    assert_seeded(&dest);
    // From the primary checkout, `CARGO_TARGET_DIR=target` names the
    // worktree's own target: copied, not read as the primary's.
    let other = w.app_worktree("other");
    let (code, _, err) = susi(
        &w.primary,
        &["workflow", "seed-target", other.to_str().unwrap()],
        &[("CARGO_TARGET_DIR", "target")],
    );
    assert_eq!(code, 0, "{err}");
    assert!(!err.contains("nothing is copied"), "{err}");
    assert_seeded(&other);
    assert!(!w.primary.join("target").exists());
}

/// On Windows an installed `susi.exe` seeds a worktree with no Git userland
/// on PATH: only Git's `cmd` (git itself), System32 (robocopy) and the
/// Windows directory. The bash version needed `cp`, `find`, `awk`, `cygpath`
/// and `ps -W` from Git's `usr/bin`; this runs the binary where none of
/// those can be found, and the worktree is seeded, stripped and marked all
/// the same. Returns at once where Git for Windows is not installed (Linux,
/// macOS).
#[test]
fn the_seeding_needs_no_git_userland_on_windows() {
    let Ok(out) = Command::new("git").arg("--exec-path").output() else {
        return;
    };
    let exec_path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let Some(git_root) = exec_path.ancestors().nth(3).map(Path::to_path_buf) else {
        return;
    };
    if !git_root.join("cmd").join("git.exe").is_file() {
        return; // not Git for Windows' layout
    }
    let windows = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let path = std::env::join_paths([
        git_root.join("cmd"),
        windows.join("System32"),
        windows.clone(),
    ])
    .unwrap();
    let path = path.to_str().unwrap().to_string();
    assert!(
        !path.to_ascii_lowercase().contains("usr\\bin"),
        "the narrowed PATH must not carry Git's userland: {path}"
    );
    let w = World::new("nouserland");
    w.make_seed();
    let shared = w.root.join("shared-target");
    write(&shared.join("sentinel"), "another worktree's build");
    let dest = w.app_worktree("app");
    link_dir(&shared, &dest.join("target"));
    let envs: &[(&str, &str)] = &[("PATH", &path)];
    let (code, _, err) = susi(
        &w.primary,
        &["workflow", "seed-target", dest.to_str().unwrap()],
        envs,
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("replaced the link"), "{err}");
    assert!(err.contains("target: seeded"), "{err}");
    assert_seeded(&dest);
    assert_eq!(
        std::fs::read_to_string(shared.join("sentinel")).unwrap(),
        "another worktree's build"
    );
    // The background form too: the detached copy is this same binary.
    let other = w.app_worktree("other");
    let (code, _, err) = susi(
        &w.primary,
        &[
            "workflow",
            "seed-target",
            "--background",
            other.to_str().unwrap(),
        ],
        envs,
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("in the background"), "{err}");
    assert!(
        wait_for(&other.join("target/.susi-seeded"), 120),
        "the background copy never finished: {err}"
    );
    wait_for_seeders(&other);
    assert_seeded(&other);
}

/// The machine's compiler cache (`scripts/setup-sccache.sh`): what the
/// snapshot cannot hold compiles once per machine, not once per worktree. It
/// writes one marked block into the user's cargo config, writes it once, takes
/// it out again on `--off`, and never adds a second `[build]` or `[env]` table
/// to a config that has one (a TOML error that would break every cargo).
#[test]
// POSIX script/hook fixture; native seeding checks remain enabled.
#[cfg(unix)]
fn setup_sccache_writes_one_removable_block_and_never_doubles_a_table() {
    let root = std::env::temp_dir().join(format!("susi-sccache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("cargo-home");
    let path = fake_tools(
        &root.join("bin"),
        &[(
            "sccache",
            "case \"$1\" in --show-stats) echo 'Compile requests 0' ;; esac\nexit 0\n",
        )],
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/setup-sccache.sh");
    let cache = root.join("cache");
    let envs = |extra: &[(&str, &str)]| -> Vec<(String, String)> {
        let mut v = vec![
            ("PATH".to_string(), path.clone()),
            (
                "CARGO_HOME".to_string(),
                home.to_string_lossy().into_owned(),
            ),
            (
                "SUSI_SCCACHE_DIR".to_string(),
                cache.to_string_lossy().into_owned(),
            ),
        ];
        v.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        v
    };
    let run_setup = |args: &[&str], extra: &[(&str, &str)]| -> (i32, String, String) {
        let envs = envs(extra);
        let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        run(&root, script.to_str().unwrap(), args, &envs)
    };
    let config = home.join("config.toml");

    let (code, out, err) = run_setup(&[], &[]);
    assert_eq!(code, 0, "{out}\n{err}");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.contains("rustc-wrapper = \"sccache\""), "{text}");
    assert!(text.contains("SCCACHE_CACHE_SIZE = \"50G\""), "{text}");
    assert!(cache.is_dir(), "the cache directory is made");

    // Once: the same settings again change nothing and say nothing.
    let (code, out, _) = run_setup(&[], &[]);
    assert_eq!(code, 0);
    assert!(
        out.is_empty(),
        "a second run with the same settings is silent: {out}"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), text);

    // A new size rewrites the one block rather than adding another.
    let (code, _, err) = run_setup(&[], &[("SUSI_SCCACHE_SIZE", "20G")]);
    assert_eq!(code, 0, "{err}");
    let text = std::fs::read_to_string(&config).unwrap();
    assert_eq!(text.matches("[build]").count(), 1, "{text}");
    assert!(text.contains("SCCACHE_CACHE_SIZE = \"20G\""), "{text}");

    // --off takes the block out; a config that was only the block goes too.
    let (code, _, err) = run_setup(&["--off"], &[]);
    assert_eq!(code, 0, "{err}");
    assert!(!config.exists(), "nothing but the block: no config is left");

    // A [build] table this script did not write is never doubled.
    write(&config, "[build]\njobs = 4\n");
    let (code, _, err) = run_setup(&[], &[]);
    assert_eq!(code, 1, "a foreign [build] table must be refused: {err}");
    assert!(
        err.contains("already has a [build] or [env] table"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "[build]\njobs = 4\n"
    );

    // Other settings are kept, the block goes after them.
    write(&config, "[alias]\nb = \"build\"\n");
    let (code, _, err) = run_setup(&[], &[]);
    assert_eq!(code, 0, "{err}");
    let text = std::fs::read_to_string(&config).unwrap();
    assert!(text.starts_with("[alias]\nb = \"build\"\n"), "{text}");
    assert!(text.contains("rustc-wrapper = \"sccache\""), "{text}");

    // Without sccache on PATH nothing is written, and the way to install it is named.
    // Strip any real host sccache from PATH too so this test never stops or
    // restarts the machine's live sccache daemon against `root/cache`.
    let _ = std::fs::remove_file(&config);
    let bare = std::env::join_paths(std::env::split_paths(&path).filter(|p| {
        p != &root.join("bin") && !p.join("sccache").is_file() && !p.join("sccache.exe").is_file()
    }))
    .unwrap();
    let (code, _, err) = run_setup(&[], &[("PATH", bare.to_str().unwrap())]);
    assert_ne!(code, 0, "without sccache on PATH, setup-sccache must fail");
    assert!(err.contains("--install"), "{err}");
    assert!(!config.exists());
    let _ = std::fs::remove_dir_all(&root);
}
