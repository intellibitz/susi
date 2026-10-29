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
//! time, and `top`'s build script reads `.agents/identity.json`.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;

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
            hostile: Cell::new(false),
        };
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
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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

/// vendor/ holds the `[patch.crates-io]` path crates linked into nearly every
/// workspace crate, and xtask/ is a member outside crates/; neither maps to a
/// package, and both fell through as "no crate": a change there reached main
/// with no compile and no test.
#[test]
fn a_vendored_patch_crate_or_xtask_is_workspace_wide() {
    let ws = Workspace::new("vendor");
    ws.change("vendor/patched/src/lib.rs");
    assert_eq!(ws.pkgs(&[]), ["ALL"]);
    assert_eq!(ws.pkgs(&["--dependents"]), ["ALL"]);
    let ws = Workspace::new("xtask");
    ws.change("xtask/src/main.rs");
    assert_eq!(ws.pkgs(&[]), ["ALL"]);
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

/// The changed-crates scan reads `git grep` as "<path>:<literal>". A user's
/// grep.lineNumber or grep.column put "<n>:" there and color.ui=always wrapped
/// it in escapes: a data file's readers were not found, and every diff
/// touching config/ or deploy/ widened to ALL.
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
