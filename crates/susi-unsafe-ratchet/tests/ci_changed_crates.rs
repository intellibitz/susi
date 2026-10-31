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
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ci-changed-crates.sh")
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
        self.change_many(&[rel]);
    }

    /// Commit a related set of paths together on the working branch.
    fn change_many(&self, rels: &[&str]) {
        for rel in rels {
            let path = self.root.join(rel);
            let mut text = std::fs::read_to_string(&path).unwrap_or_default();
            text.push_str("// changed\n");
            self.write(rel, &text);
        }
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-qm", "change"]);
    }

    fn cargo_metadata_file(&self) -> PathBuf {
        let out = Command::new("cargo")
            .args(["metadata", "--no-deps", "--format-version", "1"])
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "cargo metadata: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let path = self.root.join("cargo-metadata.json");
        std::fs::write(&path, out.stdout).unwrap();
        path
    }

    /// The script's output lines, and its exit code.
    fn run(&self, args: &[&str]) -> (i32, Vec<String>) {
        self.run_with_metadata_file(args, None)
    }

    fn run_with_metadata_file(
        &self,
        args: &[&str],
        metadata_file: Option<&Path>,
    ) -> (i32, Vec<String>) {
        let mut command = script_command(&script());
        command
            .args(args)
            .current_dir(&self.root)
            .env("BASE_REF", "main")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        if let Some(metadata_file) = metadata_file {
            command.env("SUSI_CARGO_METADATA_FILE", metadata_file);
        }
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
        self.pkgs_with_metadata_file(args, None)
    }

    fn pkgs_with_metadata_file(&self, args: &[&str], metadata_file: Option<&Path>) -> Vec<String> {
        let (code, lines) = self.run_with_metadata_file(args, metadata_file);
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

/// A changed integration test compiles with its owning crate's `--all-targets`
/// gate. Its reverse dependents cannot be affected, even when one consumes it
/// as a dev-dependency, so do not expand the clippy closure for this diff.
#[test]
fn test_only_changes_skip_reverse_dependents() {
    let ws = Workspace::new("test-only");
    ws.write("crates/base/tests/smoke.rs", "#[test] fn smoke() {}\n");
    ws.change("crates/base/tests/smoke.rs");
    assert_eq!(ws.pkgs(&["--dependents"]), ["base"]);
}

/// Inert workflow, hook, task, documentation, and script edits may accompany a
/// test-only change without making its reverse dependents relevant to clippy.
#[test]
fn inert_changes_do_not_expand_test_only_dependents() {
    let ws = Workspace::new("test-only-with-inert-files");
    ws.write("crates/base/tests/smoke.rs", "#[test] fn smoke() {}\n");
    ws.change_many(&[
        "crates/base/tests/smoke.rs",
        ".agents/tasks/T-1.json",
        ".github/workflows/test.yml",
        ".githooks/workflow-guard",
        "docs/workflow-speed.md",
        "scripts/ci-changed-crates.sh",
    ]);
    assert_eq!(ws.pkgs(&["--dependents"]), ["base"]);
}

/// A mixed test and source diff must still lint every reverse dependent.
#[test]
fn mixed_source_and_test_changes_keep_reverse_dependents() {
    let ws = Workspace::new("mixed-test-source");
    ws.write("crates/base/tests/smoke.rs", "#[test] fn smoke() {}\n");
    ws.change("crates/base/tests/smoke.rs");
    ws.change("crates/base/src/lib.rs");
    assert_eq!(
        ws.pkgs(&["--dependents"]),
        ["base", "devuser", "fixture-root", "mid", "top"]
    );
}

#[test]
fn root_test_targets_include_only_changed_root_test_files() {
    let ws = Workspace::new("root-test-targets");
    ws.change("tests/ci_changed_crates.rs");
    ws.change("scripts/ci-changed-crates.sh");
    ws.change(".agents/tasks/T-1.json");
    assert_eq!(ws.pkgs(&["--root-test-targets"]), ["ci_changed_crates"]);
}

#[test]
fn root_test_targets_fail_closed_when_production_sources_change() {
    let ws = Workspace::new("root-test-targets-source");
    ws.change("tests/ci_changed_crates.rs");
    ws.change("src/lib.rs");
    assert!(ws.pkgs(&["--root-test-targets"]).is_empty());
}

/// This package is small enough that restoring the workspace-wide target
/// archive costs more than compiling its affected clippy target from sccache.
/// Keep the exception scoped to a single-package gate in the branch workflow.
#[test]
fn ratchet_gate_skips_large_target_cache() {
    let workflow = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/test.yml");
    let workflow = std::fs::read_to_string(workflow).unwrap();
    assert!(
        workflow.contains(
            r#"if [ "$pkgs" = "xtask" ] || [ "$pkgs" = "susi-unsafe-ratchet" ]; then target_cache=false; fi"#
        ),
        "the lightweight package must skip the workspace target archive"
    );
    assert!(
        workflow.contains(
            "if: steps.affected.outputs.lintcmd != '' && steps.affected.outputs.target_cache != 'false'"
        ),
        "the target cache step must remain controlled by the package decision"
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
    revdeps_with_metadata_file(dir, pkg, None)
}

fn revdeps_with_metadata_file(dir: &Path, pkg: &str, metadata_file: Option<&Path>) -> Vec<String> {
    let out = script_command(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/ci-reverse-deps.sh"),
    )
    .arg(pkg)
    .current_dir(dir)
    .envs(
        metadata_file
            .map(|path| [("SUSI_CARGO_METADATA_FILE", path)])
            .into_iter()
            .flatten(),
    )
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

#[test]
fn selector_reads_shared_metadata_file() {
    let ws = Workspace::new("shared-metadata");
    let metadata_file = ws.cargo_metadata_file();
    ws.change("crates/base/src/lib.rs");
    let expected = ["base", "devuser", "fixture-root", "mid", "top"];
    assert_eq!(
        ws.pkgs_with_metadata_file(&["--dependents"], Some(&metadata_file)),
        expected
    );
    assert_eq!(
        revdeps_with_metadata_file(&ws.root, "base", Some(&metadata_file)),
        expected
    );
}

#[test]
fn compliance_uses_single_remote_ref_snapshot() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let helper = repo.join("scripts/fetch-workflow-refs.sh");
    let helper_text = std::fs::read_to_string(&helper).unwrap();
    assert_eq!(
        helper_text.matches("git fetch").count(),
        1,
        "the helper must make one fetch call"
    );
    for refspec in ["refs/heads/*", "refs/claims/*", "refs/closed/*"] {
        assert!(helper_text.contains(refspec), "missing refspec {refspec}");
    }

    let guard = std::fs::read_to_string(repo.join(".githooks/workflow-guard")).unwrap();
    assert!(guard.contains("fetch-workflow-refs.sh"));
    assert!(guard.contains("SUSI_WORKFLOW_REFS_FETCHED=1"));
    let compliance =
        std::fs::read_to_string(repo.join("scripts/check-workflow-compliance.sh")).unwrap();
    assert_eq!(
        compliance.matches("SUSI_WORKFLOW_REFS_FETCHED").count(),
        2,
        "claims and close receipts must both reuse the fetched snapshot"
    );
    let workflow = std::fs::read_to_string(repo.join(".github/workflows/test.yml")).unwrap();
    assert!(workflow.contains("run: scripts/fetch-workflow-refs.sh origin"));
    assert!(workflow.contains("SUSI_WORKFLOW_REFS_FETCHED: \"1\""));

    let ws = Workspace::new("workflow-refs");
    let bare = ws.root.join("remote.git");
    git(
        &ws.root,
        &["init", "--bare", "--quiet", bare.to_str().unwrap()],
    );
    git(
        &ws.root,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    git(
        &ws.root,
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    );
    git(
        &ws.root,
        &["push", "--quiet", "origin", "HEAD:refs/claims/T-TEST-1"],
    );
    git(
        &ws.root,
        &["push", "--quiet", "origin", "HEAD:refs/closed/T-TEST-1"],
    );

    let trace = ws.root.join("fetch-trace.log");
    let out = script_command(&helper)
        .current_dir(&ws.root)
        .env("GIT_TRACE", &trace)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "fetch helper failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let trace = std::fs::read_to_string(trace).unwrap();
    assert_eq!(
        trace
            .matches("built-in: git fetch --quiet --prune origin")
            .count(),
        1,
        "one fetch must update all three ref namespaces: {trace}"
    );
    for reference in [
        "refs/remotes/origin/main",
        "refs/claims/T-TEST-1",
        "refs/closed/T-TEST-1",
    ] {
        git(&ws.root, &["rev-parse", "--verify", reference]);
    }
}

#[test]
fn combined_gate_plan_matches_individual_modes() {
    let ws = Workspace::new("combined-plan");
    ws.change("crates/base/src/lib.rs");
    let plan = ws.pkgs(&["--plan"]);
    let entries = |plan: &[String], kind: &str| -> Vec<String> {
        let prefix = format!("{kind}:");
        plan.iter()
            .filter_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
            .collect()
    };
    assert_eq!(entries(&plan, "test"), ws.pkgs(&[]));
    assert_eq!(entries(&plan, "lint"), ws.pkgs(&["--dependents"]));
    assert_eq!(entries(&plan, "root-package"), ["fixture-root"]);
    assert!(entries(&plan, "root-test").is_empty());

    git(&ws.root, &["reset", "--hard", "main"]);
    ws.change("tests/focused.rs");
    let plan = ws.pkgs(&["--plan"]);
    assert_eq!(entries(&plan, "test"), ["fixture-root"]);
    assert_eq!(entries(&plan, "lint"), ["fixture-root"]);
    assert_eq!(
        entries(&plan, "root-test"),
        ws.pkgs(&["--root-test-targets"])
    );
    ws.change("crates/base/tests/focused.rs");
    let plan = ws.pkgs(&["--plan"]);
    assert_eq!(entries(&plan, "root-test"), ["focused"]);
    assert_eq!(entries(&plan, "test"), ["base", "fixture-root"]);
    assert_eq!(entries(&plan, "lint"), ["base", "fixture-root"]);
    ws.change("src/lib.rs");
    assert!(entries(&ws.pkgs(&["--plan"]), "root-test").is_empty());
    ws.change("rust-toolchain.toml");
    assert_eq!(ws.pkgs(&["--plan"]), ["ALL"]);
    git(&ws.root, &["reset", "--hard", "main"]);
    assert!(ws.pkgs(&["--plan"]).is_empty());
}

#[test]
fn finish_reuses_live_lease_and_fetched_refs() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let workflow = std::fs::read_to_string(repo.join("scripts/parallel-workflow.sh")).unwrap();
    let renew = workflow
        .split("renew_or_readopt() {")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    let ws = Workspace::new("finish-coordination");
    git(&ws.root, &["config", "susi.agent", "TESTAGENT"]);
    ws.write("lease-check.sh", &format!(
        "set -euo pipefail\n. \"$SUSI_TEST_COMPAT\"\nrenew_or_readopt() {{{renew}\n}}\n\
         stub() {{ echo \"$*\" >> calls; [ \"${{FAIL_RENEW:-0}}\" != 1 ] || [ \"$2\" != renew ]; }}\n\
         susi_bin=stub\ntask=T-TEST-1\nrenew_or_readopt\n"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for (owner, branch, lease, fail_renew, expected) in [
        ("TESTAGENT", "work", now + 3600, false, 0),
        ("TESTAGENT", "work", now + 30, false, 1),
        ("ANOTHER", "work", now + 3600, false, 1),
        ("TESTAGENT", "other-branch", now + 3600, false, 1),
        ("TESTAGENT", "work", now - 30, true, 2),
    ] {
        ws.write("claim.json", &format!("{{\"agent\":\"{owner}\",\"branch\":\"{branch}\",\"lease_until_unix\":{lease},\"scopes\":[\"src\"]}}"));
        let hash = Command::new("git")
            .args(["hash-object", "-w", "claim.json"])
            .current_dir(&ws.root)
            .output()
            .unwrap();
        assert!(hash.status.success());
        let object = String::from_utf8(hash.stdout).unwrap();
        git(
            &ws.root,
            &["update-ref", "refs/claims/T-TEST-1", object.trim()],
        );
        ws.write("calls", "");
        let output = script_command(&ws.root.join("lease-check.sh"))
            .current_dir(&ws.root)
            .env_remove("SUSI_AGENT")
            .env("SUSI_TEST_COMPAT", repo.join("scripts/posix-compat.sh"))
            .env("FAIL_RENEW", if fail_renew { "1" } else { "0" })
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let calls = std::fs::read_to_string(ws.root.join("calls")).unwrap();
        assert_eq!(
            calls.lines().count(),
            expected,
            "{owner}/{branch}/{lease}: {calls}"
        );
        if fail_renew {
            assert!(calls.contains("tasks claim T-TEST-1 --scope src"));
        }
    }

    // Timing must preserve failure both in a conditional (release) and under
    // errexit (the gate); a logging command must never turn red into green.
    let timing = workflow
        .split("timed_step() {")
        .nth(1)
        .unwrap()
        .split("\n}\n")
        .next()
        .unwrap();
    ws.write("timing-check.sh", &format!("set -e\ntimed_step() {{{timing}\n}}\nif timed_step conditional false; then exit 9; fi\ntimed_step direct false\necho escaped\n"));
    let timed = script_command(&ws.root.join("timing-check.sh"))
        .current_dir(&ws.root)
        .output()
        .unwrap();
    assert_eq!(timed.status.code(), Some(1));
    assert!(timed.stdout.is_empty());

    // A sync caller has just refreshed origin/main in the shared Git dir.
    // Parking must fast-forward from that snapshot without another fetch.
    ws.write("published.txt", "new main\n");
    git(&ws.root, &["add", "published.txt"]);
    git(&ws.root, &["commit", "-qm", "upstream advance"]);
    git(
        &ws.root,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(&ws.root, &["checkout", "-q", "main"]);
    let trace = ws.root.join("park-trace.log");
    let parked = script_command(&repo.join("scripts/park-primary.sh"))
        .arg("--sync-only")
        .current_dir(&ws.root)
        .env("SUSI_PARK_REFS_FETCHED", "1")
        .env("GIT_TRACE", &trace)
        .env_remove("SUSI_PRIMARY_SYNC_ACTIVE")
        .output()
        .unwrap();
    assert!(
        parked.status.success(),
        "{}",
        String::from_utf8_lossy(&parked.stderr)
    );
    assert!(ws.root.join("published.txt").exists());
    assert!(!std::fs::read_to_string(&trace)
        .unwrap()
        .contains("git fetch"));
    assert!(workflow.contains("SUSI_PARK_REFS_FETCHED=1 \"$root/scripts/park-primary.sh\""));
    let standalone = script_command(&repo.join("scripts/park-primary.sh"))
        .arg("--sync-only")
        .current_dir(&ws.root)
        .env_remove("SUSI_PARK_REFS_FETCHED")
        .env_remove("SUSI_PRIMARY_SYNC_ACTIVE")
        .env("GIT_TRACE", &trace)
        .output()
        .unwrap();
    assert!(standalone.status.success());
    assert!(std::fs::read_to_string(trace)
        .unwrap()
        .contains("git fetch --quiet origin"));
}

#[test]
fn compliance_batches_close_receipt_scan() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let compliance =
        std::fs::read_to_string(repo.join("scripts/check-workflow-compliance.sh")).unwrap();
    assert!(compliance.contains("declare -A owed_cache"));
    assert!(compliance.contains("unmerged_closes_of \"$agent\""));
    let helper = repo.join("scripts/owed-task-receipts.sh");
    let source = std::fs::read_to_string(&helper).unwrap();
    assert!(source.contains("git cat-file --batch"));
    assert!(!source.contains("git cat-file -p \"$name\""));

    let ws = Workspace::new("owed-receipts");
    git(&ws.root, &["init", "--quiet"]);
    git(&ws.root, &["config", "user.name", "Workflow Test"]);
    git(&ws.root, &["config", "user.email", "workflow@example.test"]);
    ws.write(".agents/tasks/done/T-PUBLISHED-1.json", "{}\n");
    git(&ws.root, &["add", "-A"]);
    git(&ws.root, &["commit", "-qm", "published close"]);
    git(
        &ws.root,
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    git(
        &ws.root,
        &[
            "cat-file",
            "-e",
            "refs/remotes/origin/main:.agents/tasks/done/T-PUBLISHED-1.json",
        ],
    );

    // Many unrelated receipts ensure the helper handles the same ref volume
    // that made the old per-receipt process loop slow.
    let mut receipt_paths = String::new();
    let mut receipt_ids = Vec::new();
    for index in 0..90 {
        let (id, agent) = match index {
            0 => ("T-PUBLISHED-1".to_owned(), "WORKFLOWINERTTESTGATES"),
            1 => ("T-OWED-1".to_owned(), "WORKFLOWINERTTESTGATES"),
            2 => ("T-OWED-2".to_owned(), "WORKFLOWINERTTESTGATES"),
            _ => (format!("T-OTHER-{index}"), "ANOTHERAGENT"),
        };
        // Cover both serializer forms; batch parsing cannot assume a separator
        // occupies an extra line after every blob.
        let newline = if index % 2 == 0 { "\n" } else { "" };
        let receipt = format!("{{\"agent\":\"{agent}\",\"task\":\"{id}\"}}{newline}");
        let path = format!("receipts/{index}.json");
        ws.write(&path, &receipt);
        receipt_paths.push_str(&format!("{path}\n"));
        receipt_ids.push(id);
    }
    // Two Git processes prepare all 90 refs. Per-ref hash-object/update-ref
    // launches dominated this fixture under the parallel Windows gate.
    let git_input = |args: &[&str], input: &str| {
        let mut child = Command::new("git")
            .args(args)
            .current_dir(&ws.root)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write as _;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    let objects = git_input(&["hash-object", "-w", "--stdin-paths"], &receipt_paths);
    let updates: String = receipt_ids
        .iter()
        .zip(objects.lines())
        .map(|(id, object)| format!("update refs/closed/{id} {object}\n"))
        .collect();
    assert_eq!(objects.lines().count(), receipt_ids.len());
    git_input(&["update-ref", "--stdin"], &updates);

    let output = script_command(&helper)
        .current_dir(&ws.root)
        .arg("WORKFLOWINERTTESTGATES")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "receipt scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let mut ids: Vec<_> = stdout.lines().collect();
    ids.sort_unstable();
    assert_eq!(
        ids,
        ["T-OWED-1", "T-OWED-2"],
        "receipt scan stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
