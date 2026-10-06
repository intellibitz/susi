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

use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/ci-changed-crates.sh")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
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
}

impl Workspace {
    /// One workspace per test: libtest runs a binary's tests as threads of one
    /// process, so the tag, not the pid, keeps their directories apart.
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("susi-changed-crates-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        let ws = Self { root };
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
        let out = Command::new("bash")
            .arg(script())
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

#[test]
fn a_diff_that_touches_no_crate_prints_nothing_in_both_modes() {
    let ws = Workspace::new("docs");
    ws.change("README.md");
    assert!(ws.pkgs(&[]).is_empty());
    assert!(ws.pkgs(&["--dependents"]).is_empty());
}

#[test]
fn an_unknown_argument_is_refused_rather_than_silently_ignored() {
    let ws = Workspace::new("usage");
    ws.change("crates/base/src/lib.rs");
    let (code, lines) = ws.run(&["--everything"]);
    assert_eq!(code, 2, "{lines:?}");
    assert!(lines.is_empty(), "nothing may reach stdout: {lines:?}");
}
