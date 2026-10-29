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

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A path as the fixture's POSIX shell scripts take it.
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A user git config that changes what porcelain commands print: line and
/// column numbers before a `git grep` match, color around everything.
const HOSTILE_GIT_CONFIG: [(&str, &str); 5] = [
    ("grep.lineNumber", "true"),
    ("grep.column", "true"),
    ("color.ui", "always"),
    ("color.grep", "always"),
    ("color.diff", "always"),
];

fn with_git_config(command: &mut Command, config: &[(&str, &str)]) {
    command.env("GIT_CONFIG_COUNT", config.len().to_string());
    for (i, (key, value)) in config.iter().enumerate() {
        command
            .env(format!("GIT_CONFIG_KEY_{i}"), key)
            .env(format!("GIT_CONFIG_VALUE_{i}"), value);
    }
}

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
    /// Run the scripts under [`HOSTILE_GIT_CONFIG`].
    hostile: Cell<bool>,
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
            hostile: Cell::new(false),
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
        let mut command = script_command(&script());
        command
            .args(args)
            .current_dir(&self.root)
            .env("BASE_REF", "main")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        if self.hostile.get() {
            with_git_config(&mut command, &HOSTILE_GIT_CONFIG);
        }
        let out = command.output().unwrap();
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
    /// A lock goes stale in minutes here, not the six hours of a real gate:
    /// a guard that wrongly waits on a live lock fails its test in time
    /// rather than holding the suite (and every pipe it inherited) for hours.
    fn guard_in(&self, dir: &Path, args: &[&str]) -> Command {
        std::fs::create_dir_all(self.target()).unwrap();
        let mut command = script_command(&guard_script());
        command
            .args(args)
            .current_dir(dir)
            .env("CARGO_TARGET_DIR", self.target())
            .env_remove("SUSI_TARGET_GUARD_HELD")
            .env("SUSI_TARGET_LOCK_STALE", "300")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        if self.hostile.get() {
            with_git_config(&mut command, &HOSTILE_GIT_CONFIG);
        }
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

    /// `pkg`'s unit in the shared target, as cargo records one: a
    /// fingerprint directory per unit, named after the package.
    fn unit(&self, pkg: &str) -> PathBuf {
        self.target()
            .join("debug/.fingerprint")
            .join(format!("{pkg}-0123456789abcdef"))
    }

    /// A build of `pkg` that no guard held: a plain `cargo` in some
    /// worktree, or one whose content nothing records.
    fn build(&self, pkg: &str) {
        let unit = self.unit(pkg);
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(format!("lib-{pkg}")), "fingerprint").unwrap();
    }

    /// A guarded build of `pkgs` in `dir`: the guard, the build and letting
    /// go in one hold, as `target-guard.sh --run cargo build` does.
    fn held_build(&self, dir: &Path, pkgs: &[&str]) {
        let units: Vec<String> = pkgs.iter().map(|pkg| slash(&self.unit(pkg))).collect();
        let out = self
            .guard_in(
                dir,
                &[
                    "--run",
                    "sh",
                    "-c",
                    "for u in \"$@\"; do mkdir -p \"$u\" && echo fingerprint > \"$u/lib\" || exit 1; done",
                    "sh",
                ],
            )
            .args(&units)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "guarded build failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn built(&self, pkg: &str) -> bool {
        self.unit(pkg).exists()
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
    ws.held_build(&ws.root, &["base"]);
    ws.guard();
    assert!(
        ws.built("base"),
        "the build is this worktree's content: keep it"
    );
}

#[test]
fn a_crate_whose_content_changed_loses_its_shared_build_and_the_rest_keep_theirs() {
    let ws = Workspace::new("guard-change");
    ws.held_build(&ws.root, &["base", "mid", "lone"]);
    ws.change("crates/base/src/lib.rs");
    ws.guard();
    assert!(!ws.built("base"));
    assert!(ws.built("mid") && ws.built("lone"));
    // An uncommitted edit is this worktree's content too.
    ws.held_build(&ws.root, &["base"]);
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
    ws.held_build(&sibling, &["base"]); // the sibling's guarded build of its own base
    ws.guard(); // this worktree: other content for base, and no build after
    assert!(
        !ws.built("base"),
        "the sibling's base must not survive a guard that stamps this worktree's"
    );
    ws.held_build(&sibling, &["base"]); // the sibling builds again, from its content
    ws.guard_at(&sibling);
    assert!(ws.built("base"), "and its own build stays its own");
}

/// What a crate compiles in from outside its directory is part of its
/// content: two worktrees with one crate tree and different
/// deploy/harness/prices.json shared one price table.
#[test]
fn a_file_a_crate_compiles_in_from_outside_its_directory_is_part_of_its_content() {
    let ws = Workspace::new("guard-inputs");
    ws.held_build(&ws.root, &["base", "mid", "top"]);
    ws.change("config/data.json"); // mid's include_str!
    ws.guard();
    assert!(!ws.built("mid"));
    assert!(ws.built("base") && ws.built("top"));
    ws.held_build(&ws.root, &["mid"]);
    // A build script's input: a directory it names, joined with a file name.
    ws.change(".agents/identity.json");
    ws.guard();
    assert!(!ws.built("top"));
    assert!(ws.built("base") && ws.built("mid"));
    ws.held_build(&ws.root, &["top"]);
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
    ws.held_build(&ws.root, &["base", "mid", "top"]);
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
    /// Its Windows pid, which every MSYS runtime sees ("-" if unread).
    #[cfg(windows)]
    winpid: String,
    dead: bool,
}

impl Holder {
    fn start() -> Self {
        let shell = if cfg!(windows) { "sh" } else { "bash" };
        let mut child = Command::new(shell)
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
        let pid = line.trim().to_string();
        // Read after the exec: the Windows process the pid runs as now.
        #[cfg(windows)]
        let winpid = Command::new(shell)
            .args([
                "-c",
                &format!("cat /proc/{pid}/winpid 2>/dev/null || echo -"),
            ])
            .output()
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
            .ok()
            .filter(|w| !w.is_empty())
            .unwrap_or_else(|| "-".to_string());
        Self {
            child,
            pid,
            #[cfg(windows)]
            winpid,
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

/// Git for Windows' `<git>/bin/bash.exe`: a native launcher that runs the
/// MSYS shell in `usr/bin` and waits for it. No MSYS runtime's process table
/// lists it, so to every runtime it is what a holder from another runtime is:
/// a shell known only by its Windows pid.
#[cfg(windows)]
fn native_shell_launcher() -> Option<PathBuf> {
    let out = Command::new("git").arg("--exec-path").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let exec_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let launcher = Path::new(&exec_path)
        .ancestors()
        .nth(3)?
        .join("bin")
        .join("bash.exe");
    launcher.is_file().then_some(launcher)
}

/// A live shell no MSYS table lists (see [`native_shell_launcher`]): the
/// launcher, and under it a shell that ends when its stdin closes, so
/// killing the launcher leaves nothing behind.
#[cfg(windows)]
struct NativeShell {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
}

#[cfg(windows)]
impl NativeShell {
    fn start() -> Option<Self> {
        let mut child = Command::new(native_shell_launcher()?)
            .args(["-c", "read -r _"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        let stdin = child.stdin.take();
        std::thread::sleep(Duration::from_millis(500));
        if child.try_wait().ok()?.is_some() {
            return None; // it did not wait on its stdin
        }
        Some(Self { child, stdin })
    }

    fn winpid(&self) -> String {
        self.child.id().to_string()
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        drop(self.stdin.take());
    }
}

#[cfg(windows)]
impl Drop for NativeShell {
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
        .stdout(std::process::Stdio::null())
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
        .stdout(std::process::Stdio::null())
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

/// Each MSYS runtime (one per Git for Windows install: Program Files' and
/// scoop's on one host) keeps its own pid table, and both report
/// OSTYPE=cygwin. A holder from the other install read as dead here, its lock
/// was taken and the two gates built at once. A holder is named by its
/// runtime's install root and its Windows pid: from another runtime it is
/// alive while that Windows process is, and with no Windows pid to ask it is
/// presumed alive until the lock's stale age. Its pid number is another
/// table's, so it never makes this owner's --release (or --hold) its own.
///
/// On Windows the holder is a shell no MSYS table lists, as another
/// runtime's is: a shell of this runtime's own table is one whose pid the
/// guard reads as reused (the next test), and stood in for a foreign holder
/// that was then taken over within a second.
#[test]
fn a_holder_from_another_runtime_is_waited_for_and_never_released_as_ours() {
    let ws = Workspace::new("guard-runtime");
    // A live pid of this runtime with the number the line names: the line is
    // another table's, so this one says nothing about it.
    let holder = Holder::start();
    let lock = ws.target().join(".susi-guard/lock");
    std::fs::create_dir_all(&lock).unwrap();
    #[cfg(windows)]
    let mut foreign = NativeShell::start();
    #[cfg(windows)]
    let winpid = foreign
        .as_ref()
        .map_or_else(|| "-".to_string(), NativeShell::winpid);
    #[cfg(not(windows))]
    let winpid = "-".to_string();
    // A root with a space in it, last on the line.
    let planted = format!(
        "{} {} cygwin {winpid} C:/Another Git/Install",
        holder.pid,
        now_unix()
    );
    std::fs::write(lock.join("owner"), format!("{planted}\n")).unwrap();
    let owner_line = || {
        std::fs::read_to_string(lock.join("owner"))
            .unwrap_or_default()
            .trim_end()
            .to_string()
    };
    let released = ws
        .guard_in(&ws.root, &["--release", &holder.pid])
        .output()
        .unwrap();
    assert!(released.status.success());
    assert_eq!(
        owner_line(),
        planted,
        "another runtime's lock is not this pid's to release"
    );
    let mut waiting = ws
        .guard_in(&ws.root, &["--run", "sh", "-c", "echo ran > ran"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_secs(4));
    let still_waiting = waiting.try_wait().unwrap().is_none();
    let line_now = owner_line();
    if !still_waiting || line_now != planted {
        let _ = waiting.kill();
        let _ = waiting.wait();
    }
    assert!(
        !ws.root.join("ran").exists() && still_waiting,
        "a live holder from another runtime must be waited for"
    );
    assert_eq!(line_now, planted, "a live holder's lock is never taken");
    #[cfg(windows)]
    {
        if let Some(foreign) = foreign.as_mut() {
            // Gone from Windows, it is gone from every runtime, though this
            // runtime has a live pid of that number.
            foreign.kill();
            let status = wait_for(&mut waiting, 90);
            if status.is_none() {
                let _ = waiting.kill();
            }
            let status = status.expect("a dead holder from another runtime was never taken over");
            assert!(status.success() && ws.root.join("ran").exists());
            assert!(!lock.exists(), "the guard that took it over lets go");
            return;
        }
        eprintln!("no native Git launcher to stand for another runtime's holder: stale age only");
    }
    let _ = waiting.kill();
    let _ = waiting.wait();
    // Nothing to ask about another runtime's pid: only the stale age frees
    // its lock.
    let status = ws
        .guard_in(&ws.root, &["--run", "sh", "-c", "echo ran > ran"])
        .env("SUSI_TARGET_LOCK_STALE", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success() && ws.root.join("ran").exists());
    assert!(!lock.exists(), "the guard that took it over lets go");
}

/// A holder this runtime's table no longer shows is looked up by its Windows
/// pid, which Windows hands out again. A crashed holder's pid taken by a
/// program that is no shell (explorer.exe, an editor) read as the holder, and
/// every gate, close and push on the host waited for the lock's stale age,
/// six hours; one this runtime's own table lists is a reuse as well. Every
/// holder is a shell: the guard's bash, or the script that took `--hold`.
#[cfg(windows)]
#[test]
fn a_dead_holders_windows_pid_taken_by_another_process_frees_its_lock() {
    let ws = Workspace::new("guard-reuse");
    let lock = ws.target().join(".susi-guard/lock");
    let sh = |script: &str| -> String {
        let out = Command::new("sh").args(["-c", script]).output().unwrap();
        assert!(out.status.success(), "sh -c {script}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    // This runtime, as the guard names it, and a pid its table showed once.
    let runtime = sh("cd / && pwd -W");
    let runtime = runtime.trim_end_matches('/');
    let dead = sh("echo $$");
    // A program that is no shell, and a shell of this runtime's own table.
    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let program = Reaped(
        Command::new("ping")
            .args(["-n", "120", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let ours = Holder::start();
    assert!(ours.winpid.parse::<u32>().is_ok(), "{}", ours.winpid);
    // A native Git launcher: no MSYS table lists it, and every shell susi
    // starts runs under one, so a crashed holder's number can go to one next.
    // Asked about this runtime's holder, it read that holder as alive.
    let launcher = NativeShell::start();
    let mut cases = vec![
        (
            "this runtime's holder, its Windows pid now a program",
            format!("{dead} {} cygwin {} {runtime}", now_unix(), program.0.id()),
        ),
        (
            "another runtime's holder, its Windows pid now this runtime's",
            format!(
                "{dead} {} cygwin {} C:/Another Git/Install",
                now_unix(),
                ours.winpid
            ),
        ),
    ];
    if let Some(launcher) = &launcher {
        cases.push((
            "this runtime's holder, its Windows pid now a native Git shell",
            format!(
                "{dead} {} cygwin {} {runtime}",
                now_unix(),
                launcher.winpid()
            ),
        ));
    }
    for (case, line) in cases {
        std::fs::create_dir_all(&lock).unwrap();
        std::fs::write(lock.join("owner"), format!("{line}\n")).unwrap();
        let _ = std::fs::remove_file(ws.root.join("ran"));
        let mut next = ws
            .guard_in(&ws.root, &["--run", "sh", "-c", "echo ran > ran"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let status = wait_for(&mut next, 60);
        if status.is_none() {
            let _ = next.kill();
            let _ = next.wait();
        }
        let status = status.unwrap_or_else(|| panic!("{case}: its lock was never taken over"));
        assert!(
            status.success() && ws.root.join("ran").exists(),
            "{case}: the command never ran"
        );
        assert!(
            !lock.exists(),
            "{case}: the guard that took it over lets go"
        );
    }
}

/// A holder that died leaves its lock behind, and several waiters find that
/// out within the same second. Each used to read the dead owner and `rm -rf`
/// the lock: one's rm landed after another had already re-created it, and
/// both held the target. Only one waiter may take a dead holder's lock, and
/// the guarded commands never overlap.
#[test]
fn a_dead_holders_lock_goes_to_one_waiter_at_a_time() {
    let ws = Workspace::new("guard-takeover");
    let mut holder = Holder::start();
    let held = ws
        .guard_in(&ws.root, &["--hold", &holder.pid])
        .output()
        .unwrap();
    assert!(held.status.success());
    holder.kill();
    let inside = slash(&ws.target().join("inside"));
    let overlaps = ws.target().join("overlaps");
    let critical = format!(
        "if mkdir '{inside}' 2>/dev/null; then sleep 1; rmdir '{inside}'; else echo overlap >> '{}'; fi",
        slash(&overlaps)
    );
    let mut waiters: Vec<std::process::Child> = (0..5)
        .map(|_| {
            ws.guard_in(&ws.root, &["--run", "sh", "-c", &critical])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    for waiter in &mut waiters {
        let status = wait_for(waiter, 180).expect("a waiter never got the target");
        assert!(status.success());
    }
    assert!(
        !overlaps.exists(),
        "two guarded commands held the target at once: {}",
        std::fs::read_to_string(&overlaps).unwrap_or_default()
    );
    assert!(
        !ws.target().join(".susi-guard/lock").exists(),
        "the last holder lets go"
    );
}

/// Guards nest: a --run inside a --run inside a --run shares the outermost
/// hold. The middle one used to name itself to the innermost although it held
/// nothing, and the innermost waited on the outermost, which waited on it,
/// until the lock's stale age (six hours, every other worktree waiting too).
#[test]
fn guards_nested_three_deep_share_the_outermost_hold() {
    let ws = Workspace::new("guard-nest");
    let nest = ws.root.join(".nest").join("nest.sh");
    ws.write(
        ".nest/nest.sh",
        "d=$1\nif [ \"$d\" -gt 0 ]; then exec bash \"$G\" --run -- sh \"$NEST\" $((d - 1)); fi\necho deep > deep\n",
    );
    // Stale in a minute: a guard that waits on its ancestor ends then, and
    // says it waited, rather than holding the suite.
    let mut outer = ws
        .guard_in(&ws.root, &["--run", "sh", &slash(&nest), "2"])
        .env("G", slash(&guard_script()))
        .env("NEST", slash(&nest))
        .env("SUSI_TARGET_LOCK_STALE", "60")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let status = wait_for(&mut outer, 150);
    if status.is_none() {
        let _ = outer.kill();
    }
    let out = outer.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        status.is_some_and(|s| s.success()) && ws.root.join("deep").exists(),
        "three nested guards did not complete: {stderr}"
    );
    assert!(
        !stderr.contains("waiting for the shared target"),
        "a guard three deep waited on its own ancestor: {stderr}"
    );
}

/// `finish` holds the target for its gate (`hold_target`), lets go
/// (`release_target`), then closes and pushes, each with a guard of its own.
/// Run under an outer `target-guard.sh --run`, the gate replaced the
/// inherited holder with its own pid and then unset it, so the close and the
/// push waited on the outer guard, which waited on them. The gate exports
/// whoever `--hold` says holds the target and puts back what it inherited.
/// The functions are the real ones, read out of parallel-workflow.sh.
#[test]
fn a_finish_under_an_outer_guard_closes_and_pushes_without_waiting() {
    let ws = Workspace::new("guard-finish");
    let script = ws.root.join(".nest").join("finish.sh");
    ws.write(
        ".nest/finish.sh",
        r#"root=$PW_ROOT
eval "$(awk '/^target_held=/{p=1} p{print} p&&/^release_target\(\)/{r=1} r&&/^}/{exit}' "$root/scripts/parallel-workflow.sh")"
type hold_target >/dev/null 2>&1 && type release_target >/dev/null 2>&1 || { echo "no hold_target/release_target in parallel-workflow.sh" >&2; exit 9; }
before=${SUSI_TARGET_GUARD_HELD:-unset}
hold_target
during=${SUSI_TARGET_GUARD_HELD:-unset}
release_target
after=${SUSI_TARGET_GUARD_HELD:-unset}
echo "$$ $before $during $after" > "$1.env"
bash "$G" --run -- sh -c "echo closed > $1.closed"
bash "$G" --hold $$ > /dev/null && bash "$G" --release $$ && echo pushed > "$1.pushed"
"#,
    );
    let repo = slash(Path::new(env!("CARGO_MANIFEST_DIR")));
    std::fs::create_dir_all(ws.target()).unwrap();
    let finish = |outer: bool, tag: &str| -> Vec<String> {
        // finish.sh evals parallel-workflow.sh's functions: bash, as its
        // shebang says, wherever it runs (on Linux `sh` is dash).
        let mut command = if outer {
            ws.guard_in(&ws.root, &["--run", "bash", &slash(&script), tag])
        } else {
            let mut command = Command::new(if cfg!(windows) { "sh" } else { "bash" });
            command
                .arg(slash(&script))
                .arg(tag)
                .current_dir(&ws.root)
                .env("CARGO_TARGET_DIR", ws.target())
                .env_remove("SUSI_TARGET_GUARD_HELD")
                .env_remove("SUSI_TARGET_LOCK_STALE")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE");
            command
        };
        // Stale in a minute: a close or push that waits on the gate's own
        // hold ends then, and says it waited, rather than holding the suite.
        let mut child = command
            .env("G", slash(&guard_script()))
            .env("PW_ROOT", &repo)
            .env("SUSI_TARGET_LOCK_STALE", "60")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let status = wait_for(&mut child, 200);
        if status.is_none() {
            let _ = child.kill();
        }
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            status.is_some_and(|s| s.success()),
            "finish ({tag}) did not complete: {stderr}"
        );
        assert!(
            !stderr.contains("waiting for the shared target"),
            "finish ({tag}) waited on its own hold: {stderr}"
        );
        for step in ["closed", "pushed"] {
            assert!(
                ws.root.join(format!("{tag}.{step}")).exists(),
                "finish ({tag}) never {step}"
            );
        }
        std::fs::read_to_string(ws.root.join(format!("{tag}.env")))
            .unwrap()
            .split_whitespace()
            .map(str::to_string)
            .collect()
    };
    // Alone, the gate holds the target itself and names itself to its
    // commands; letting go leaves nothing behind.
    let alone = finish(false, "alone");
    let (pid, before, during, after) = (&alone[0], &alone[1], &alone[2], &alone[3]);
    assert_eq!(before, "unset");
    assert!(
        during.starts_with(&format!("{pid}:")),
        "the gate holds the target itself: {alone:?}"
    );
    assert_eq!(after, "unset");
    // Under an outer guard, it shares that hold and gives the name back.
    let nested = finish(true, "nested");
    let (before, during, after) = (&nested[1], &nested[2], &nested[3]);
    assert_ne!(before, "unset", "the outer guard names its hold");
    assert_eq!(
        during, before,
        "the gate's commands are told the real holder"
    );
    assert_eq!(after, before, "letting go puts back what was inherited");
    assert!(!ws.target().join(".susi-guard/lock").exists());
}

/// A plain cargo (`cargo run -q -- workflow check` in a fresh worktree, a
/// bare `cargo test`) builds into the shared target from its own worktree's
/// content and leaves the stamps alone. The next guarded run of a worktree
/// whose content matched the stamp kept that build and judged the other
/// branch's code. A hold seals the target when it lets go; a member whose
/// builds changed after the seal is unknown again, and only a guarded
/// build is trusted.
#[test]
fn a_build_no_guard_held_is_never_vouched_for() {
    let ws = Workspace::new("guard-unheld");
    ws.held_build(&ws.root, &["base", "mid"]);
    ws.guard();
    assert!(
        ws.built("base") && ws.built("mid"),
        "a guarded build is this worktree's"
    );
    // Over a second after the seal, so any file system's clock tells them
    // apart.
    std::thread::sleep(Duration::from_millis(1100));
    ws.build("base"); // a sibling's plain cargo, rebuilding base from its content
    ws.guard();
    assert!(
        !ws.built("base"),
        "a build made outside the guard must not ride on this worktree's stamp"
    );
    assert!(ws.built("mid"), "what nobody rebuilt stays shared");
    ws.held_build(&ws.root, &["base"]);
    ws.guard();
    assert!(ws.built("base"), "the guarded rebuild is trusted again");
    // Within the seal's second: a plain cargo blocked on cargo's build lock
    // starts as the hold's cargo exits. bash's `-nt` compared whole seconds
    // before 5.1 and trusted it; where the file system keeps sub-second
    // times, so does the guard.
    let sealed = ws.target().join(".susi-guard/sealed");
    let sub_second = std::fs::metadata(&sealed)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .is_some_and(|d| d.subsec_nanos() != 0);
    if sub_second {
        std::thread::sleep(Duration::from_millis(50));
        ws.build("base");
        ws.guard();
        assert!(
            !ws.built("base"),
            "a build made outside the guard in the seal's second must not be trusted"
        );
        ws.held_build(&ws.root, &["base"]);
        ws.guard();
        assert!(ws.built("base"));
    }
    let code = std::fs::read_to_string(guard_script()).unwrap();
    assert!(
        !code
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .any(|line| line.contains(" -nt ") || line.contains(" -ot ")),
        "the seal is compared by `find -newer`, never by bash's whole-second `-nt`"
    );
    // A guard nested in a hold trusts that hold's own builds.
    let fresh = slash(&ws.unit("lone"));
    let out = ws
        .guard_in(
            &ws.root,
            &[
                "--run",
                "sh",
                "-c",
                "mkdir -p \"$1\" && echo fingerprint > \"$1/lib\" && bash \"$G\" --run -- true",
                "sh",
                &fresh,
            ],
        )
        .env("G", slash(&guard_script()))
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(
        ws.built("lone"),
        "a nested guard dropped its own hold's build"
    );
    ws.guard();
    assert!(ws.built("lone") && ws.built("base") && ws.built("mid"));
    // A target no hold has sealed says nothing about who built what.
    std::fs::remove_file(ws.target().join(".susi-guard/sealed")).unwrap();
    ws.guard();
    assert!(!ws.built("base") && !ws.built("mid") && !ws.built("lone"));
}

/// `--hold` reports who holds the target, for its caller to name to the
/// commands it runs: itself when it took the lock, the ancestor whose lock
/// it found otherwise.
#[test]
fn hold_reports_who_holds_the_target() {
    let ws = Workspace::new("guard-holder");
    let holder = Holder::start();
    let out = ws
        .guard_in(&ws.root, &["--hold", &holder.pid])
        .output()
        .unwrap();
    assert!(out.status.success());
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(
        token.starts_with(&format!("{}:", holder.pid)),
        "--hold took the lock: {token:?}"
    );
    // A process under that holder shares the hold, and is told so.
    let other = Holder::start();
    let out = ws
        .guard_in(&ws.root, &["--hold", &other.pid])
        .env("SUSI_TARGET_GUARD_HELD", &token)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), token);
    let released = ws
        .guard_in(&ws.root, &["--release", &other.pid])
        .output()
        .unwrap();
    assert!(released.status.success());
    assert!(
        ws.target().join(".susi-guard/lock").exists(),
        "a guard that shared the hold has nothing to let go"
    );
    let released = ws
        .guard_in(&ws.root, &["--release", &holder.pid])
        .output()
        .unwrap();
    assert!(released.status.success());
    assert!(!ws.target().join(".susi-guard/lock").exists());
}

/// The guard reads `git grep` as "<path>:<literal>". A user's
/// grep.lineNumber or grep.column put "<n>:" there, and color.ui=always
/// wrapped everything in escapes: every build-script input (with color,
/// every input) dropped out, and two worktrees with different .agents
/// ledgers or config/ files shared one build.
#[test]
fn a_users_git_config_never_hides_a_crates_inputs() {
    let ws = Workspace::new("guard-gitconfig");
    ws.hostile.set(true);
    ws.held_build(&ws.root, &["base", "mid", "top"]);
    ws.change(".agents/identity.json"); // top's build script reads it
    ws.guard();
    assert!(!ws.built("top"), "a build script's input was lost");
    assert!(ws.built("base") && ws.built("mid"));
    ws.held_build(&ws.root, &["top"]);
    ws.change("config/data.json"); // mid compiles it in
    ws.guard();
    assert!(!ws.built("mid"), "an include_str! input was lost");
    assert!(ws.built("base") && ws.built("top"));
    // And uncommitted: the diff that names the change is the change itself.
    ws.held_build(&ws.root, &["mid"]);
    ws.write("config/data.json", "{\"edited\": true}\n");
    ws.guard();
    assert!(!ws.built("mid"));
}

/// The changed-crates scan reads `git grep` the same way: under that config
/// a data file's readers were not found, and every diff touching config/ or
/// deploy/ widened to ALL.
#[test]
fn a_users_git_config_never_hides_a_data_files_readers() {
    let ws = Workspace::new("data-gitconfig");
    ws.hostile.set(true);
    ws.change("config/data.json");
    assert_eq!(ws.pkgs(&[]), ["mid"]);
    assert_eq!(ws.pkgs(&["--dependents"]), ["fixture-root", "mid", "top"]);
}

/// A rename lists both paths. With git's rename detection only the new one
/// showed, and a crate that still compiles in the old path, which the
/// branch no longer has, was never gated: its build broke on main.
#[test]
fn a_renamed_data_file_still_gates_every_crate_that_names_its_old_path() {
    let ws = Workspace::new("data-rename");
    // Two readers of config/data.json on the base: mid, and now lone.
    ws.write(
        "crates/lone/src/lib.rs",
        "pub fn f() {}\npub fn assets() -> &'static str {\n    \"../../deploy/assets\"\n}\n\
         pub const DATA: &str = include_str!(\"../../../config/data.json\");\n",
    );
    git(&ws.root, &["add", "-A"]);
    git(&ws.root, &["commit", "-qm", "a second reader"]);
    git(&ws.root, &["branch", "-f", "main", "work"]);
    // The branch moves the file and only one of its readers with it.
    git(&ws.root, &["mv", "config/data.json", "config/moved.json"]);
    ws.write(
        "crates/mid/src/lib.rs",
        "pub fn f() {}\npub const DATA: &str = include_str!(\"../../../config/moved.json\");\n",
    );
    git(&ws.root, &["add", "-A"]);
    git(&ws.root, &["commit", "-qm", "move the data"]);
    assert_eq!(ws.pkgs(&[]), ["lone", "mid"]);
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
