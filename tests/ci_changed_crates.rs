#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test fixtures/assertions
#![allow(missing_docs)] // integration test crate
//! `scripts/ci-changed-crates.sh --dependents`: which packages a diff can break.
//!
//! The branch-push gate compiles and lints what the diff touched *and* whatever
//! depends on it, because a signature change leaves the changed crate's own
//! tests green and breaks its caller. The script is run against a real git
//! repository holding a real cargo workspace, so `cargo metadata` supplies the
//! dependency graph exactly as it does in CI:
//!
//! ```text
//! fixture-root -> top -> mid -> base <-dev- devuser        lone
//! ```
//!
//! `mid` compiles in `config/data.json`, `lone` reads `deploy/assets/` at run
//! time, and `top`'s build script reads `.agents/identity.json`. The same
//! fixture holds `scripts/target-guard.sh`, which decides whether the shared
//! cargo target's build of each of these crates is this worktree's.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn script_command(script: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut command = Command::new("sh");
        command.arg(script);
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("bash");
        command.arg(script);
        command
    }
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci-changed-crates.sh")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .args(args)
        .current_dir(dir)
        // An outer hook's repository must never be the one a fixture commits to.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

struct Workspace {
    root: PathBuf,
    /// Sibling worktrees and the shared target, removed with the workspace.
    extra: RefCell<Vec<PathBuf>>,
}

impl Workspace {
    /// One workspace per test: libtest runs a binary's tests as threads of one
    /// process, so the tag, not the pid, keeps their directories apart.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("susi-changed-crates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let ws = Self {
            root,
            extra: RefCell::new(Vec::new()),
        };
        let _ = std::fs::remove_dir_all(ws.target());
        ws.extra.borrow_mut().push(ws.target());
        ws.write(
            "Cargo.toml",
            "[package]\nname = \"fixture-root\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n\
             [dependencies]\ntop = { path = \"crates/top\" }\n\n\
             [workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
        );
        ws.write("src/lib.rs", "pub fn fixture() {}\n");
        ws.write("README.md", "fixture\n");
        ws.krate("base", "");
        ws.krate("mid", "base = { path = \"../base\" }");
        ws.krate("top", "mid = { path = \"../mid\" }");
        ws.krate_with_dev("devuser", "base = { path = \"../base\" }");
        ws.krate("lone", "");
        // Data outside every crate: compiled in, read at run time, read by a
        // build script, and named by nothing at all.
        ws.write(
            "crates/mid/src/lib.rs",
            "pub fn f() {}\npub const DATA: &str = include_str!(\"../../../config/data.json\");\n",
        );
        ws.write("config/data.json", "{}\n");
        ws.write(
            "crates/lone/src/lib.rs",
            "pub fn f() {}\npub fn assets() -> &'static str {\n    \"../../deploy/assets\"\n}\n",
        );
        ws.write("deploy/assets/a.json", "{}\n");
        ws.write("deploy/helm/values.yaml", "replicas: 1\n");
        ws.write(
            "crates/top/build.rs",
            "fn main() {\n    let ledgers = std::path::Path::new(\"../../.agents\");\n    \
             println!(\"cargo:rerun-if-changed={}\", ledgers.join(\"identity.json\").display());\n}\n",
        );
        ws.write(".agents/identity.json", "{}\n");
        ws.write(".agents/tasks/T-1.json", "{}\n");
        git(&ws.root, &["init", "-q", "-b", "main"]);
        git(&ws.root, &["config", "user.name", "test"]);
        git(&ws.root, &["config", "user.email", "test@example.test"]);
        git(&ws.root, &["add", "-A"]);
        git(&ws.root, &["commit", "-qm", "seed"]);
        git(&ws.root, &["checkout", "-q", "-b", "work"]);
        ws
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn krate(&self, name: &str, deps: &str) {
        self.manifest(name, &format!("[dependencies]\n{deps}\n"));
    }

    fn krate_with_dev(&self, name: &str, dev_deps: &str) {
        self.manifest(name, &format!("[dev-dependencies]\n{dev_deps}\n"));
    }

    fn manifest(&self, name: &str, tables: &str) {
        self.write(
            &format!("crates/{name}/Cargo.toml"),
            &format!(
                "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n{tables}"
            ),
        );
        self.write(&format!("crates/{name}/src/lib.rs"), "pub fn f() {}\n");
    }

    /// Commit a change to `rel` on the working branch.
    fn change(&self, rel: &str) {
        let path = self.root.join(rel);
        let mut text = std::fs::read_to_string(&path).unwrap_or_default();
        text.push_str("// changed\n");
        self.write(rel, &text);
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-qm", "change"]);
    }

    /// The script's output lines, and its exit code.
    fn run(&self, args: &[&str]) -> (i32, Vec<String>) {
        let out = script_command(&script())
            .args(args)
            .current_dir(&self.root)
            .env("BASE_REF", "main")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .unwrap();
        let lines = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_string)
            .collect();
        (out.status.code().unwrap_or(-1), lines)
    }

    fn pkgs(&self, args: &[&str]) -> Vec<String> {
        let (code, lines) = self.run(args);
        assert_eq!(code, 0, "script failed: {lines:?}");
        lines
    }

    /// Commit the deletion of `rel` on the working branch.
    fn remove(&self, rel: &str) {
        git(&self.root, &["rm", "-q", rel]);
        git(&self.root, &["commit", "-qm", "remove"]);
    }

    /// The shared cargo target, outside the worktree as the primary's is.
    fn target(&self) -> PathBuf {
        let mut name = self.root.file_name().unwrap().to_os_string();
        name.push("-target");
        self.root.with_file_name(name)
    }

    /// A sibling worktree of this repository on a new branch off `work`.
    fn worktree(&self, name: &str) -> PathBuf {
        let mut dir = self.root.file_name().unwrap().to_os_string();
        dir.push(format!("-{name}"));
        let path = self.root.with_file_name(dir);
        let _ = std::fs::remove_dir_all(&path);
        git(
            &self.root,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                name,
                path.to_str().unwrap(),
                "work",
            ],
        );
        self.extra.borrow_mut().push(path.clone());
        path
    }

    /// `scripts/target-guard.sh <args>` run in `dir`, against the shared target.
    fn guard_in(&self, dir: &Path, args: &[&str]) -> Command {
        std::fs::create_dir_all(self.target()).unwrap();
        let mut command = script_command(&guard_script());
        command
            .args(args)
            .current_dir(dir)
            .env("CARGO_TARGET_DIR", self.target())
            .env_remove("SUSI_TARGET_GUARD_HELD")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        command
    }

    /// The guard, run before a build in `dir`.
    fn guard_at(&self, dir: &Path) {
        let out = self.guard_in(dir, &[]).output().unwrap();
        assert!(
            out.status.success(),
            "target-guard failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn guard(&self) {
        self.guard_at(&self.root);
    }

    /// A build of `pkg` in the shared target, as cargo records one: a
    /// fingerprint directory per unit, named after the package.
    fn build(&self, pkg: &str) {
        let unit = self
            .target()
            .join("debug/.fingerprint")
            .join(format!("{pkg}-0123456789abcdef"));
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(format!("lib-{pkg}")), "fingerprint").unwrap();
    }

    fn built(&self, pkg: &str) -> bool {
        self.target()
            .join("debug/.fingerprint")
            .join(format!("{pkg}-0123456789abcdef"))
            .exists()
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        for dir in self.extra.borrow().iter() {
            let _ = std::fs::remove_dir_all(dir);
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn guard_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/target-guard.sh")
}

#[test]
fn without_the_flag_only_the_touched_crate_is_listed() {
    let ws = Workspace::new("bare");
    ws.change("crates/base/src/lib.rs");
    assert_eq!(ws.pkgs(&[]), ["base"]);
}

#[test]
fn dependents_are_listed_transitively_to_the_root_package() {
    let ws = Workspace::new("transitive");
    ws.change("crates/base/src/lib.rs");
    assert_eq!(
        ws.pkgs(&["--dependents"]),
        ["base", "devuser", "fixture-root", "mid", "top"]
    );
}

#[test]
fn a_dev_dependency_is_a_dependent_because_its_test_target_stops_compiling() {
    let ws = Workspace::new("dev");
    ws.change("crates/base/src/lib.rs");
    assert!(
        ws.pkgs(&["--dependents"]).contains(&"devuser".to_string()),
        "`devuser` only dev-depends on `base`"
    );
}

#[test]
fn a_leaf_lists_itself_and_the_crates_above_it_but_not_those_below() {
    let ws = Workspace::new("leaf");
    ws.change("crates/top/src/lib.rs");
    assert_eq!(ws.pkgs(&["--dependents"]), ["fixture-root", "top"]);
}

#[test]
fn an_unrelated_crate_stays_alone_in_both_modes() {
    let ws = Workspace::new("lone");
    ws.change("crates/lone/src/lib.rs");
    assert_eq!(ws.pkgs(&[]), ["lone"]);
    assert_eq!(ws.pkgs(&["--dependents"]), ["lone"]);
}

#[test]
fn the_root_packages_own_trees_map_to_the_root_package() {
    let ws = Workspace::new("root");
    ws.change("src/lib.rs");
    assert_eq!(ws.pkgs(&["--dependents"]), ["fixture-root"]);
}

#[test]
fn a_workspace_wide_input_is_all_in_both_modes() {
    let ws = Workspace::new("all");
    ws.change("Cargo.toml");
    assert_eq!(ws.pkgs(&[]), ["ALL"]);
    assert_eq!(ws.pkgs(&["--dependents"]), ["ALL"]);
}

#[test]
fn a_diff_that_touches_no_crate_prints_nothing_in_both_modes() {
    let ws = Workspace::new("docs");
    ws.change("README.md");
    assert!(ws.pkgs(&[]).is_empty());
    assert!(ws.pkgs(&["--dependents"]).is_empty());
}

/// A branch that holds nothing its base lacks (merges of main, or a done
/// record about to be committed) has nothing to prove. Reading that as ALL
/// sent `susi workflow finish` through the whole workspace suite.
#[test]
fn a_branch_with_no_diff_from_its_base_prints_nothing() {
    let ws = Workspace::new("nodiff");
    assert!(ws.pkgs(&[]).is_empty());
    assert!(ws.pkgs(&["--dependents"]).is_empty());
}

/// config/ and deploy/ are data the crates compile in or read: a change there
/// is a change to the crates whose sources name the file. Read as "no crate",
/// a broken deploy/harness/prices.json passed the local gate, the branch
/// Compile Check and main's affected run with no test run at all.
#[test]
fn a_data_file_a_crate_compiles_in_is_a_change_to_that_crate() {
    let ws = Workspace::new("data-include");
    ws.change("config/data.json");
    assert_eq!(ws.pkgs(&[]), ["mid"]);
    assert_eq!(ws.pkgs(&["--dependents"]), ["fixture-root", "mid", "top"]);
}

#[test]
fn a_data_file_read_through_its_directory_is_a_change_to_the_reader() {
    let ws = Workspace::new("data-dir");
    ws.change("deploy/assets/a.json");
    assert_eq!(ws.pkgs(&[]), ["lone"]);
}

/// A crate may reach a data file by a path assembled at run time, which no
/// text search sees: a file no source names gates everything.
#[test]
fn a_config_or_deploy_file_no_source_names_is_all() {
    let ws = Workspace::new("data-unnamed");
    ws.change("deploy/helm/values.yaml");
    assert_eq!(ws.pkgs(&[]), ["ALL"]);
    assert_eq!(ws.pkgs(&["--dependents"]), ["ALL"]);
}

#[test]
fn a_deleted_data_file_no_source_names_needs_nothing() {
    let ws = Workspace::new("data-gone");
    ws.remove("deploy/helm/values.yaml");
    assert!(ws.pkgs(&[]).is_empty());
}

#[test]
fn an_unknown_argument_is_refused_rather_than_silently_ignored() {
    let ws = Workspace::new("usage");
    ws.change("crates/base/src/lib.rs");
    let (code, lines) = ws.run(&["--everything"]);
    assert_eq!(code, 2, "{lines:?}");
    assert!(lines.is_empty(), "nothing may reach stdout: {lines:?}");
}

/// `scripts/ci-reverse-deps.sh <pkg>`: the packages whose build can pull in
/// `<pkg>` — the same transitive closure `--dependents` grows for a change to
/// it, plus the package itself. The branch gate intersects this set with the
/// affected crates to decide whether a push can reach the ONNX download.
fn revdeps(dir: &Path, pkg: &str) -> Vec<String> {
    let out =
        script_command(&Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci-reverse-deps.sh"))
            .arg(pkg)
            .current_dir(dir)
            .output()
            .unwrap();
    assert!(
        out.status.success(),
        "ci-reverse-deps {pkg}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn reverse_deps_of_a_base_crate_is_its_whole_dependent_closure() {
    let ws = Workspace::new("rev-base");
    assert_eq!(
        revdeps(&ws.root, "base"),
        ["base", "devuser", "fixture-root", "mid", "top"]
    );
}

#[test]
fn reverse_deps_of_a_leaf_is_itself_and_reverse_deps_of_root_is_just_root() {
    let ws = Workspace::new("rev-leaf");
    assert_eq!(revdeps(&ws.root, "lone"), ["lone"]);
    assert_eq!(revdeps(&ws.root, "fixture-root"), ["fixture-root"]);
}

// `scripts/target-guard.sh`: worktrees share one cargo target, and cargo names
// a workspace crate's build by its path relative to the workspace, so a build
// another worktree made of other content reads as fresh. A build here is a
// fingerprint directory, which is how cargo knows it has one.

/// With no stamp, nothing says whose content a build is, so it goes; once
/// this worktree has stamped and built, its build is kept and shared.
#[test]
fn the_guard_drops_a_build_no_stamp_vouches_for_and_keeps_its_own() {
    let ws = Workspace::new("guard-boot");
    ws.build("base");
    ws.build("serde"); // a registry crate: keyed by version, never this guard's
    ws.guard();
    assert!(
        !ws.built("base"),
        "a build no stamp describes may be a sibling's"
    );
    assert!(ws.built("serde"), "registry builds are never touched");
    ws.build("base");
    ws.guard();
    assert!(
        ws.built("base"),
        "the build is this worktree's content: keep it"
    );
}

#[test]
fn a_crate_whose_content_changed_loses_its_shared_build_and_the_rest_keep_theirs() {
    let ws = Workspace::new("guard-change");
    ws.guard();
    for pkg in ["base", "mid", "lone"] {
        ws.build(pkg);
    }
    ws.change("crates/base/src/lib.rs");
    ws.guard();
    assert!(!ws.built("base"));
    assert!(ws.built("mid") && ws.built("lone"));
    // An uncommitted edit is this worktree's content too.
    ws.build("base");
    ws.write("crates/lone/src/lib.rs", "pub fn edited() {}\n");
    ws.guard();
    assert!(!ws.built("lone"));
    assert!(ws.built("base") && ws.built("mid"));
}

/// A guard that builds nothing after it (`sync`, a gate scoped to other
/// crates) must not leave a sibling's build behind under this worktree's
/// stamp. Touching this worktree's files instead did: the stamp then named
/// content the target did not hold, and the next worktree with that content
/// linked the sibling's code.
#[test]
fn a_guard_that_builds_nothing_leaves_no_sibling_build_to_trust() {
    let ws = Workspace::new("guard-sibling");
    let sibling = ws.worktree("sibling");
    std::fs::write(
        sibling.join("crates/base/src/lib.rs"),
        "pub fn f() {}\npub fn sibling() {}\n",
    )
    .unwrap();
    git(&sibling, &["commit", "-qam", "sibling's base"]);
    ws.guard_at(&sibling);
    ws.build("base"); // the sibling's guarded build of its own base
    ws.guard(); // this worktree: other content for base, and no build after
    assert!(
        !ws.built("base"),
        "the sibling's base must not survive a guard that stamps this worktree's"
    );
    ws.guard_at(&sibling);
    ws.build("base"); // the sibling builds again, from its content
    ws.guard_at(&sibling);
    assert!(ws.built("base"), "and its own build stays its own");
}

/// What a crate compiles in from outside its directory is part of its
/// content: two worktrees with one crate tree and different
/// deploy/harness/prices.json shared one price table.
#[test]
fn a_file_a_crate_compiles_in_from_outside_its_directory_is_part_of_its_content() {
    let ws = Workspace::new("guard-inputs");
    ws.guard();
    for pkg in ["base", "mid", "top"] {
        ws.build(pkg);
    }
    ws.change("config/data.json"); // mid's include_str!
    ws.guard();
    assert!(!ws.built("mid"));
    assert!(ws.built("base") && ws.built("top"));
    ws.build("mid");
    // A build script's input: a directory it names, joined with a file name.
    ws.change(".agents/identity.json");
    ws.guard();
    assert!(!ws.built("top"));
    assert!(ws.built("base") && ws.built("mid"));
    ws.build("top");
    // The task queue is never a build input.
    ws.change(".agents/tasks/T-1.json");
    ws.guard();
    assert!(ws.built("base") && ws.built("mid") && ws.built("top"));
    // Nor is a compiled-in file only committed content: an edit counts.
    ws.write("config/data.json", "{\"edited\": true}\n");
    ws.guard();
    assert!(!ws.built("mid"));
    assert!(ws.built("base") && ws.built("top"));
}

/// A directory a build script names is where its inputs are, not an input:
/// read as one, `.agents` put every task-queue edit and untracked log under
/// it into the content of a crate with uncommitted changes, which then lost
/// its build to each of them.
#[test]
fn a_task_queue_edit_never_costs_a_crate_with_uncommitted_changes_its_build() {
    let ws = Workspace::new("guard-queue");
    ws.write("crates/top/src/lib.rs", "pub fn edited() {}\n");
    ws.guard();
    for pkg in ["base", "mid", "top"] {
        ws.build(pkg);
    }
    ws.write(".agents/tasks/T-1.json", "{\"claimed\": true}\n");
    ws.write(".agents/rsi-loop.jsonl", "{}\n");
    ws.guard();
    assert!(
        ws.built("base") && ws.built("mid") && ws.built("top"),
        "the task queue and a log are no crate's input"
    );
    // The file the build script reads still is, edited as well as committed.
    ws.write(".agents/identity.json", "{\"edited\": true}\n");
    ws.guard();
    assert!(!ws.built("top"));
    assert!(ws.built("base") && ws.built("mid"));
}

/// The guard needs bash 4.2 (associative arrays, `printf '%(%s)T'`), and
/// macOS's /bin/bash is 3.2. There the first `local -A` discarded the whole
/// mode dispatch, so `--run` exited 1 without running its command and `tasks
/// close` read every cargo acceptance as not done. No test host has a bash
/// that old, so this reads the script: an older bash runs the command
/// unguarded before any line that needs a newer one.
#[test]
fn an_older_bash_runs_the_command_unguarded_before_anything_needs_bash_4() {
    let text = std::fs::read_to_string(guard_script()).unwrap();
    let code: Vec<(usize, &str)> = text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with('#'))
        .collect();
    let gate = code
        .iter()
        .find(|(_, line)| line.contains("BASH_VERSINFO"))
        .map(|(at, _)| *at)
        .expect("the guard must check the bash it runs in");
    let unguarded = code
        .iter()
        .find(|(at, line)| *at > gate && line.contains("exec \"$@\""))
        .map(|(at, _)| *at)
        .expect("an older bash must still run the command");
    for (at, line) in &code {
        for needs_4 in ["local -A", "declare -A", "%(", "mapfile", "readarray"] {
            assert!(
                !line.contains(needs_4) || *at > unguarded,
                "line {} needs bash 4 but comes before the version check: {line}",
                at + 1
            );
        }
    }
}

/// The member list is read as cargo reads it: here it closes on its only
/// line, names its members by a glob, and the root package is a member
/// without being listed. Read to the end of the file instead, the guard
/// stamped "members" named 2021 and Muthu.
#[test]
fn the_guard_reads_the_member_list_as_cargo_does() {
    let ws = Workspace::new("guard-members");
    ws.guard();
    let mut stamps: Vec<String> = std::fs::read_dir(ws.target().join(".susi-guard/stamps"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    stamps.sort();
    assert_eq!(
        stamps,
        [
            "_",
            "crates_base",
            "crates_devuser",
            "crates_lone",
            "crates_mid",
            "crates_top"
        ]
    );
}

/// A shell whose pid the guard can see (on Windows an MSYS pid, not the
/// launcher's Windows one), standing for a gate that holds the target.
struct Holder {
    child: std::process::Child,
    pid: String,
    dead: bool,
}

impl Holder {
    fn start() -> Self {
        let mut child = Command::new(if cfg!(windows) { "sh" } else { "bash" })
            .args(["-c", "echo $$; exec sleep 120"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(child.stdout.take().unwrap()),
            &mut line,
        )
        .unwrap();
        Self {
            child,
            pid: line.trim().to_string(),
            dead: false,
        }
    }

    fn kill(&mut self) {
        if std::mem::replace(&mut self.dead, true) {
            return;
        }
        let _ = Command::new(if cfg!(windows) { "sh" } else { "bash" })
            .args(["-c", &format!("kill {}", self.pid)])
            .stderr(std::process::Stdio::null())
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        self.kill();
    }
}

fn wait_for(child: &mut std::process::Child, secs: u64) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// The guard and the build after it are one step: a second worktree's
/// guarded build waits while another holds the target (two gates
/// interleaving left one linking the other's code under its own stamp), and
/// a holder that died does not hold it forever.
#[test]
fn a_guarded_build_waits_for_one_in_progress_but_not_for_a_dead_one() {
    let ws = Workspace::new("guard-lock");
    let mut holder = Holder::start();
    let held = ws
        .guard_in(&ws.root, &["--hold", &holder.pid])
        .output()
        .unwrap();
    assert!(held.status.success());
    let mut waiting = ws
        .guard_in(&ws.root, &["--run", "sh", "-c", "echo ran > ran"])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        !ws.root.join("ran").exists() && waiting.try_wait().unwrap().is_none(),
        "a guarded build must wait while another holds the target"
    );
    let released = ws
        .guard_in(&ws.root, &["--release", &holder.pid])
        .output()
        .unwrap();
    assert!(released.status.success());
    let status = wait_for(&mut waiting, 60).expect("the waiting build never ran");
    assert!(status.success() && ws.root.join("ran").exists());

    // A holder that died leaves its lock behind; it is taken over.
    let held = ws
        .guard_in(&ws.root, &["--hold", &holder.pid])
        .output()
        .unwrap();
    assert!(held.status.success());
    holder.kill();
    let mut next = ws
        .guard_in(&ws.root, &["--run", "sh", "-c", "exit 3"])
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let status = wait_for(&mut next, 60).expect("a dead holder's lock was never taken over");
    assert_eq!(
        status.code(),
        Some(3),
        "--run exits with its command's status"
    );
}

/// The guard is an aid, never a gate: with no target to guard the command
/// still runs, and its status is the command's.
#[test]
fn a_guarded_command_runs_without_a_target_and_keeps_its_status() {
    let ws = Workspace::new("guard-notarget");
    let out = script_command(&guard_script())
        .args(["--run", "sh", "-c", "echo ran > ran; exit 4"])
        .current_dir(&ws.root)
        .env("CARGO_TARGET_DIR", ws.root.join("no-such-target"))
        .env_remove("SUSI_TARGET_GUARD_HELD")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert!(ws.root.join("ran").exists());
}
