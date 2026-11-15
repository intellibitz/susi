#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

#[test]
fn accept_commands_keep_the_callers_toolchain_homes() {
    use std::ffi::OsString;
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |key: &str| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
        }
    };
    let defaults = toolchain_homes(env(&[("HOME", "/u")]));
    assert_eq!(
        defaults,
        vec![
            (
                "CARGO_HOME",
                Path::new("/u").join(".cargo").into_os_string()
            ),
            (
                "RUSTUP_HOME",
                Path::new("/u").join(".rustup").into_os_string()
            ),
        ]
    );
    let explicit = toolchain_homes(env(&[
        ("HOME", "/u"),
        ("CARGO_HOME", "/c"),
        ("RUSTUP_HOME", ""),
    ]));
    assert_eq!(explicit[0], ("CARGO_HOME", OsString::from("/c")));
    assert_eq!(
        explicit[1],
        (
            "RUSTUP_HOME",
            Path::new("/u").join(".rustup").into_os_string()
        )
    );
    let windows = toolchain_homes(env(&[("USERPROFILE", "C:/u")]));
    assert_eq!(
        windows[0],
        (
            "CARGO_HOME",
            Path::new("C:/u").join(".cargo").into_os_string()
        )
    );
    assert!(toolchain_homes(env(&[])).is_empty());
}

static FIXTURE_SEQ: AtomicUsize = AtomicUsize::new(0);

use crate::test_fixture_fs::{copy_dir_all, set_git_origin_url};

struct Fixture {
    dir: PathBuf,
    remote: PathBuf,
    work: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static FX_TPL: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let tpl = FX_TPL.get_or_init(|| {
            let tpl_dir =
                std::env::temp_dir().join(format!("susi-distill-fx-tpl-{}", std::process::id()));
            let _ = fs::remove_dir_all(&tpl_dir);
            let tpl_remote = tpl_dir.join("origin.git");
            let tpl_work = tpl_dir.join("work");
            fs::create_dir_all(&tpl_work).unwrap();

            let init_bare = Git { root: &tpl_dir };
            init_bare
                .text(&[
                    "init",
                    "--bare",
                    "--initial-branch=main",
                    tpl_remote.to_str().unwrap(),
                ])
                .unwrap();

            let git = Git { root: &tpl_work };
            git.text(&["init", "--initial-branch=main"]).unwrap();
            git.text(&["remote", "add", "origin", tpl_remote.to_str().unwrap()])
                .unwrap();
            tpl_dir
        });

        let id = FIXTURE_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("susi-distill-test-{}-{id}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        copy_dir_all(tpl, &dir);
        let remote = dir.join("origin.git");
        let work = dir.join("work");
        set_git_origin_url(&work, &remote);
        Self { dir, remote, work }
    }

    fn git(&self) -> Git<'_> {
        Git { root: &self.work }
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.work.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn write_task(&self, id: &str, deps: &[&str], scopes: &[&str]) {
        let task_path = format!(".agents/tasks/{id}.json");
        let mut full_scopes: Vec<String> = scopes.iter().map(|s| (*s).to_owned()).collect();
        if !full_scopes.contains(&task_path) {
            full_scopes.push(task_path.clone());
        }
        let doc = json!({
            "id": id,
            "title": format!("Title for {id}"),
            "goal": format!("Goal for {id}"),
            "created_by": "test-worker",
            "created_unix": 1_794_300_000u64,
            "deps": deps,
            "scopes": full_scopes,
            "accept": { "cmd": ["cargo", "test", "--offline", "--locked"] }
        });
        self.write(
            &task_path,
            &format!("{}\n", serde_json::to_string_pretty(&doc).unwrap()),
        );
    }

    fn commit_all(&self, message: &str) -> String {
        let git = self.git();
        git.text(&["add", "-A"]).unwrap();
        git.text(&[
            "-c",
            "user.name=Worker",
            "-c",
            "user.email=worker@localhost",
            "commit",
            "-m",
            message,
        ])
        .unwrap();
        git.resolve_commit("HEAD").unwrap()
    }

    fn run_args(&self, args: &[&str]) -> Result<String> {
        let vec: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        run(&self.work, &vec)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn transitive_dependency_closure_selects_prerequisites_without_unselected_leaks() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("initial base");

    // Unselected work lands first on main
    fx.write("crates/unselected/lib.rs", "pub fn unselected() {}\n");
    fx.write_task("T-UNSELECTED-1", &[], &["crates/unselected"]);
    fx.commit_all("feat: unselected change\n\nTask: T-UNSELECTED-1");

    // Prerequisite task T-DEP-1
    fx.write("crates/dep/lib.rs", "pub fn dep_v1() -> u32 { 42 }\n");
    fx.write_task("T-DEP-1", &[], &["crates/dep"]);
    fx.commit_all("feat: add dep\n\nTask: T-DEP-1");

    // Feature task T-FEAT-1 depending on T-DEP-1
    fx.write("crates/feat/lib.rs", "pub fn feat() -> u32 { 42 + 1 }\n");
    fx.write_task("T-FEAT-1", &["T-DEP-1"], &["crates/feat"]);
    fx.commit_all("feat: add feat\n\nTask: T-FEAT-1");

    let out = fx
        .run_args(&[
            "compose",
            "--name",
            "feat-slice-1",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-FEAT-1",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    let inc: Vec<&str> = receipt["included"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(inc, vec!["T-DEP-1", "T-FEAT-1"]);
    assert!(receipt["deferred"].as_array().unwrap().is_empty());
    assert!(receipt["rejected"].as_array().unwrap().is_empty());

    // Verify distilled branch contains dep + feat and ZERO unselected files!
    let files = fx
        .git()
        .tree_blobs(receipt["final_commit"].as_str().unwrap(), "crates")
        .unwrap();
    let paths: Vec<String> = files.into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths, vec!["crates/dep/lib.rs", "crates/feat/lib.rs"]);
}

#[test]
fn host_created_task_commit_is_refused_until_adopted_on_record() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("initial base");
    fx.write("crates/local/lib.rs", "pub fn local() {}\n");
    fx.write_task("T-LOCAL-1", &[], &["crates/local"]);
    fx.commit_all("feat: local\n\nTask: T-LOCAL-1");
    // The Git host writes this commit itself (an API commit or web edit).
    fx.write("crates/hosted/lib.rs", "pub fn hosted() {}\n");
    fx.write_task("T-HOSTED-1", &[], &["crates/hosted"]);
    // `Git` pins the distiller's own committer identity, so write this
    // commit with plain Git, the way the host would.
    let host_git = |args: &[&str]| {
        let out = Command::new("git")
            .current_dir(&fx.work)
            .args(args)
            .env("GIT_COMMITTER_NAME", "GitHub")
            .env("GIT_COMMITTER_EMAIL", "noreply@github.com")
            .env("GIT_AUTHOR_NAME", "Worker")
            .env("GIT_AUTHOR_EMAIL", "worker@localhost")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    host_git(&["add", "-A"]);
    host_git(&["commit", "-q", "-m", "feat: hosted\n\nTask: T-HOSTED-1"]);
    let hosted = fx.git().resolve_commit("HEAD").unwrap();
    let compose = |name: &str, extra: &[&str]| -> Value {
        let mut args = vec![
            "compose",
            "--name",
            name,
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-LOCAL-1,T-HOSTED-1",
            "--json",
        ];
        args.extend_from_slice(extra);
        serde_json::from_str(&fx.run_args(&args).unwrap()).unwrap()
    };

    let refused = compose("host-refused", &[]);
    assert_eq!(refused["included"][0]["task_id"], "T-LOCAL-1", "{refused}");
    assert_eq!(
        refused["included"].as_array().unwrap().len(),
        1,
        "{refused}"
    );
    assert_eq!(refused["rejected"][0]["task_id"], "T-HOSTED-1");
    assert_eq!(refused["rejected"][0]["reason_code"], "merged_outside_susi");
    assert_eq!(
        refused["git_host_created_in_stream"][0]["commit"],
        hosted.as_str()
    );
    assert!(refused["adopted_host_commits"]
        .as_object()
        .unwrap()
        .is_empty());

    // --strict refuses the candidate outright.
    let strict = fx.run_args(&[
        "compose",
        "--name",
        "host-strict",
        "--base",
        &base,
        "--from",
        "HEAD",
        "--select",
        "T-HOSTED-1",
        "--strict",
    ]);
    assert!(strict.unwrap_err().contains("strict distillation refused"));

    // An abbreviated id never adopts anything.
    assert!(fx
        .run_args(&[
            "compose",
            "--name",
            "host-short",
            "--base",
            &base,
            "--select",
            "T-HOSTED-1",
            "--adopt-host-commit",
            &hosted[..12],
        ])
        .is_err());

    // A Susi replay of the host commit (identical change, local committer,
    // `Relands:` trailer) re-lands it: the replay is composed instead.
    let parent = fx
        .git()
        .text(&["rev-parse", &format!("{hosted}^")])
        .unwrap();
    let tree = fx.git().tree(&hosted).unwrap();
    let replay_msg = format!("feat: hosted\n\nTask: T-HOSTED-1\nRelands: {hosted}\n");
    let replay = fx
        .git()
        .commit_tree(&tree, parent.trim(), &replay_msg)
        .unwrap();
    let joined = fx
        .git()
        .commit_tree_parents(&tree, &[hosted.as_str(), replay.as_str()], "join\n")
        .unwrap();
    let relanded: Value = serde_json::from_str(
        &fx.run_args(&[
            "compose",
            "--name",
            "host-relanded",
            "--base",
            &base,
            "--from",
            &joined,
            "--select",
            "T-LOCAL-1,T-HOSTED-1",
            "--json",
        ])
        .unwrap(),
    )
    .unwrap();
    assert!(
        relanded["rejected"].as_array().unwrap().is_empty(),
        "{relanded}"
    );
    assert_eq!(
        relanded["included"][1]["source_commits"][0],
        replay.as_str()
    );
    assert_eq!(
        relanded["relanded_host_commits_in_stream"][hosted.as_str()],
        replay.as_str()
    );
    assert!(relanded["git_host_created_in_stream"]
        .as_array()
        .unwrap()
        .is_empty());

    let adopted = compose("host-adopted", &["--adopt-host-commit", &hosted]);
    assert!(
        adopted["rejected"].as_array().unwrap().is_empty(),
        "{adopted}"
    );
    assert_eq!(adopted["included"].as_array().unwrap().len(), 2);
    assert!(adopted["adopted_host_commits"][hosted.as_str()]
        .as_str()
        .unwrap()
        .ends_with("for T-HOSTED-1"));
}

#[test]
fn conflicting_task_is_deferred_with_dependents_while_independent_task_lands() {
    let fx = Fixture::new();
    fx.write("shared/config.txt", "line1\nline2\nline3\n");
    let base = fx.commit_all("base");

    // Unselected commit rewrites shared/config.txt on main
    fx.write("shared/config.txt", "rewritten_by_unselected\n");
    fx.write_task("T-NOISE-1", &[], &["shared"]);
    fx.commit_all("noise\n\nTask: T-NOISE-1");

    // T-CONFLICT-1 edits on top of T-NOISE-1 without declaring dependency on T-NOISE-1
    fx.write(
        "shared/config.txt",
        "rewritten_by_unselected\nplus_conflict\n",
    );
    fx.write_task("T-CONFLICT-1", &[], &["shared"]);
    fx.commit_all("conflict\n\nTask: T-CONFLICT-1");

    // T-CHILD-1 depends on T-CONFLICT-1
    fx.write("child/mod.rs", "pub struct Child;\n");
    fx.write_task("T-CHILD-1", &["T-CONFLICT-1"], &["child"]);
    fx.commit_all("child\n\nTask: T-CHILD-1");

    // T-CLEAN-1 is completely independent
    fx.write("clean/ok.rs", "pub fn clean() {}\n");
    fx.write_task("T-CLEAN-1", &[], &["clean"]);
    fx.commit_all("clean\n\nTask: T-CLEAN-1");

    // Strict mode refuses because of deferred tasks
    let strict_err = fx
        .run_args(&[
            "plan",
            "--name",
            "conflict-check",
            "--base",
            &base,
            "--select",
            "T-CHILD-1,T-CLEAN-1",
            "--strict",
        ])
        .unwrap_err();
    assert!(strict_err.contains("strict distillation refused"));

    // Non-strict plan includes T-CLEAN-1 and defers T-CONFLICT-1 + T-CHILD-1 with exact reasons
    let out = fx
        .run_args(&[
            "plan",
            "--name",
            "conflict-check",
            "--base",
            &base,
            "--select",
            "T-CHILD-1,T-CLEAN-1",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(receipt["included"][0]["task_id"], "T-CLEAN-1");
    let def = receipt["deferred"].as_array().unwrap();
    assert_eq!(def.len(), 2);
    assert_eq!(def[0]["task_id"], "T-CONFLICT-1");
    assert_eq!(def[0]["reason_code"], "merge_conflict");
    assert_eq!(def[1]["task_id"], "T-CHILD-1");
    assert_eq!(def[1]["reason_code"], "dependency_deferred");
}

#[test]
fn cyclic_dependencies_are_rejected_and_propagate_to_dependents() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("base");

    fx.write("a/lib.rs", "// a\n");
    fx.write_task("T-CYC-A", &["T-CYC-B"], &["a"]);
    fx.commit_all("a\n\nTask: T-CYC-A");

    fx.write("b/lib.rs", "// b\n");
    fx.write_task("T-CYC-B", &["T-CYC-A"], &["b"]);
    fx.commit_all("b\n\nTask: T-CYC-B");

    fx.write("c/lib.rs", "// c\n");
    fx.write_task("T-TOP-1", &["T-CYC-A"], &["c"]);
    fx.commit_all("c\n\nTask: T-TOP-1");

    let out = fx
        .run_args(&[
            "plan",
            "--name",
            "cycle-plan",
            "--base",
            &base,
            "--select",
            "T-TOP-1",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    let rej = receipt["rejected"].as_array().unwrap();
    assert!(rej.iter().any(|r| r["reason_code"] == "cyclic_dependency"));
    assert!(rej
        .iter()
        .any(|r| r["task_id"] == "T-TOP-1" && r["reason_code"] == "dependency_rejected"));
}

#[test]
fn missing_manifest_out_of_scope_diff_and_confidential_files_are_rejected() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("base");

    // Out-of-scope modification
    fx.write("allowed/ok.rs", "// ok\n");
    fx.write("outside/sneaky.rs", "// sneaky\n");
    fx.write_task("T-ESCAPE-1", &[], &["allowed"]);
    fx.commit_all("escape\n\nTask: T-ESCAPE-1");

    // Confidential file inside scope
    fx.write("sec/server.pem", "PRIVATE KEY\n");
    fx.write_task("T-SECRET-1", &[], &["sec"]);
    fx.commit_all("secret\n\nTask: T-SECRET-1");

    let out = fx
        .run_args(&[
            "plan",
            "--name",
            "reject-audit",
            "--base",
            &base,
            "--select",
            "T-GHOST-1,T-ESCAPE-1,T-SECRET-1",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    let rej = receipt["rejected"].as_array().unwrap();
    assert_eq!(rej.len(), 3);
    assert!(rej
        .iter()
        .any(|r| r["task_id"] == "T-GHOST-1" && r["reason_code"] == "missing_manifest"));
    assert!(rej
        .iter()
        .any(|r| r["task_id"] == "T-ESCAPE-1" && r["reason_code"] == "out_of_scope_diff"));
    assert!(rej
        .iter()
        .any(|r| r["task_id"] == "T-SECRET-1" && r["reason_code"] == "confidentiality_violation"));
}

#[test]
fn prerequisite_already_in_base_satisfies_downstream_task() {
    let fx = Fixture::new();
    fx.write("foundation/core.rs", "pub const V: u8 = 1;\n");
    fx.write_task("T-FOUNDATION-1", &[], &["foundation"]);
    let base = fx.commit_all("foundation in base\n\nTask: T-FOUNDATION-1");

    fx.write("app/use_core.rs", "pub const X: u8 = 2;\n");
    fx.write_task("T-APP-1", &["T-FOUNDATION-1"], &["app"]);
    fx.commit_all("app on top\n\nTask: T-APP-1");

    let out = fx
        .run_args(&[
            "plan",
            "--name",
            "base-dep-test",
            "--base",
            &base,
            "--select",
            "T-APP-1",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(receipt["satisfied_in_base"][0]["task_id"], "T-FOUNDATION-1");
    assert_eq!(receipt["included"][0]["task_id"], "T-APP-1");
}

#[test]
fn task_trailer_names_every_comma_or_space_separated_id() {
    let mut ids = Vec::new();
    push_task_trailer_ids("Task: T-A-1, t-b-1", &mut ids);
    push_task_trailer_ids("  task:T-C-1 T-A-1", &mut ids);
    push_task_trailer_ids("Distilled-Task: T-X-1", &mut ids);
    push_task_trailer_ids("Tasks: T-Y-1", &mut ids);
    push_task_trailer_ids("Task:", &mut ids);
    assert_eq!(ids, ["T-A-1", "T-B-1", "T-C-1"]);
}

#[test]
fn distilling_onto_a_distilled_release_skips_what_it_already_carries() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("initial base");
    fx.write("crates/a/lib.rs", "pub const A: u8 = 1;\n");
    fx.write_task("T-A-1", &[], &["crates/a"]);
    fx.commit_all("feat: a\n\nTask: T-A-1");
    let out = fx
        .run_args(&[
            "compose",
            "--name",
            "rel-1",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-A-1",
            "--release-candidate",
            "--strict",
            "--json",
        ])
        .unwrap();
    let first: Value = serde_json::from_str(&out).unwrap();
    let released = first["final_commit"].as_str().unwrap().to_owned();

    // The release branch edits what T-A-1 added (as a version bump does),
    // so applying T-A-1's source commit again would be an add/add conflict.
    let main = fx
        .git()
        .text(&["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap();
    fx.git().text(&["checkout", "-q", &released]).unwrap();
    fx.write("crates/a/lib.rs", "pub const A: u8 = 1; // released\n");
    let release = fx.commit_all("chore: release-only edit");
    fx.git().text(&["checkout", "-q", &main]).unwrap();

    fx.write("crates/b/lib.rs", "pub const B: u8 = 2;\n");
    fx.write_task("T-B-1", &["T-A-1"], &["crates/b"]);
    fx.commit_all("feat: b\n\nTask: T-B-1");

    let out = fx
        .run_args(&[
            "plan",
            "--name",
            "rel-2",
            "--base",
            &release,
            "--from",
            "HEAD",
            "--tasks-from-range",
            &format!("{base}..HEAD"),
            "--strict",
            "--json",
        ])
        .unwrap();
    let second: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(second["satisfied_in_base"][0]["task_id"], "T-A-1");
    assert_eq!(
        second["satisfied_in_base"][0]["reason_code"],
        "already_distilled_in_base"
    );
    let included: Vec<&str> = second["included"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(included, ["T-B-1"]);
    assert!(second["deferred"].as_array().unwrap().is_empty());
    assert!(second["rejected"].as_array().unwrap().is_empty());
}

#[test]
fn feature_manifest_and_spdx_license_allowlist_are_enforced() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("base");

    fx.write("plug/mod.rs", "pub fn plug() {}\n");
    fx.write_task("T-PLUG-1", &[], &["plug"]);
    fx.commit_all("plug\n\nTask: T-PLUG-1");

    let bad_feature = json!({
        "schema": FEATURE_SCHEMA,
        "id": "F-GPL",
        "title": "Bad License Feature",
        "license": "GPL-3.0-only",
        "tasks": ["T-PLUG-1"]
    });
    fx.write(
        "bad-feature.json",
        &serde_json::to_string_pretty(&bad_feature).unwrap(),
    );
    let err = fx
        .run_args(&[
            "plan",
            "--name",
            "bad-lic",
            "--base",
            &base,
            "--feature",
            "bad-feature.json",
        ])
        .unwrap_err();
    assert!(err.contains("disallowed_license"));
    let _ = fs::remove_file(fx.work.join("bad-feature.json"));

    let good_feature = json!({
        "schema": FEATURE_SCHEMA,
        "id": "F-PLUG",
        "title": "Apache Plug Feature",
        "license": "Apache-2.0 AND MIT",
        "destination_branch": "feature/apache-plug",
        "enabled_capabilities": ["plug.v1"],
        "tasks": ["T-PLUG-1"]
    });
    fx.write(
        "good-feature.json",
        &serde_json::to_string_pretty(&good_feature).unwrap(),
    );
    let out = fx
        .run_args(&[
            "compose",
            "--name",
            "good-lic",
            "--base",
            &base,
            "--feature",
            "good-feature.json",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(receipt["branch_ref"], "refs/heads/feature/apache-plug");
    assert_eq!(receipt["feature"]["id"], "F-PLUG");
}

#[test]
fn compose_publish_and_status_enforce_cas_immutability_and_tamper_detection() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("base");
    fx.git().text(&["push", "origin", "main"]).unwrap();

    fx.write("mod_a/lib.rs", "pub fn step1() {}\n");
    fx.write_task("T-MULTI-1", &[], &["mod_a"]);
    fx.commit_all("step1\n\nTask: T-MULTI-1");

    fx.write("mod_a/lib.rs", "pub fn step1() {}\npub fn step2() {}\n");
    fx.commit_all("step2\n\nTask: T-MULTI-1");

    // Compose release candidate
    fx.run_args(&[
        "compose",
        "--name",
        "rc-2026-11-11",
        "--base",
        &base,
        "--select",
        "T-MULTI-1",
        "--release-candidate",
        "--strict",
    ])
    .unwrap();

    // Idempotent replay with identical inputs succeeds
    fx.run_args(&[
        "compose",
        "--name",
        "rc-2026-11-11",
        "--base",
        &base,
        "--select",
        "T-MULTI-1",
        "--release-candidate",
        "--strict",
    ])
    .unwrap();

    // Different flags under same name fail closed
    let dup_err = fx
        .run_args(&[
            "compose",
            "--name",
            "rc-2026-11-11",
            "--base",
            &base,
            "--select",
            "T-MULTI-1",
        ])
        .unwrap_err();
    assert!(dup_err.contains("different inputs/evidence"));

    // Status before publish
    let status_before: Value =
        serde_json::from_str(&fx.run_args(&["status", "rc-2026-11-11", "--json"]).unwrap())
            .unwrap();
    assert_eq!(
        status_before["branch_ref"],
        "refs/heads/release/rc-2026-11-11"
    );
    assert_eq!(status_before["branch_state"], "present_exact");
    assert_eq!(status_before["remote_state"], "absent");

    // Publish immutable snapshot tag to bare origin
    let pub_msg = fx.run_args(&["publish", "rc-2026-11-11"]).unwrap();
    assert!(pub_msg.contains("Published distilled snapshot verified"));

    let status_after: Value =
        serde_json::from_str(&fx.run_args(&["status", "rc-2026-11-11", "--json"]).unwrap())
            .unwrap();
    assert_eq!(status_after["remote_state"], "published_exact");

    // Verify bare remote tag cannot be silently replaced if it points elsewhere
    let bare = Git { root: &fx.remote };
    bare.text(&["update-ref", "refs/tags/susi-distill/rc-conflict", &base])
        .unwrap();
    fx.run_args(&[
        "compose",
        "--name",
        "rc-conflict",
        "--base",
        &base,
        "--select",
        "T-MULTI-1",
    ])
    .unwrap();
    let conflict_err = fx.run_args(&["publish", "rc-conflict"]).unwrap_err();
    assert!(conflict_err.contains("already points to"));

    // Grafted history fails closed
    let grafts = PathBuf::from(
        fx.git()
            .text(&[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "info/grafts",
            ])
            .unwrap(),
    );
    fs::write(&grafts, "").unwrap();
    let graft_err = fx.run_args(&["status", "rc-2026-11-11"]).unwrap_err();
    assert!(graft_err.contains("grafts"));
}

#[test]
fn plan_manifest_interop_and_batched_commit_stream_include_empty_and_multi_commit_tasks() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    // Seed 1,600 task records (>65 KB cat-file --batch stdin and >1 MiB stdout) to guard against OS pipe deadlocks
    let padding = "x".repeat(512);
    for n in 0..1600u32 {
        let id = format!("T-BULK-{n}");
        let doc = json!({
            "id": id,
            "title": format!("Bulk task {n} {padding}"),
            "goal": "Exercise large cat-file --batch pipe streaming",
            "created_by": "worker",
            "deps": [],
            "scopes": ["bulk"],
            "accept": { "cmd": ["git", "--version"] }
        });
        fx.write(
            &format!(".agents/tasks/{id}.json"),
            &serde_json::to_string(&doc).unwrap(),
        );
    }
    let base = fx.commit_all("base");

    // Empty marker commit in stream to verify batched git log parser handles zero-diff commits
    fx.git()
        .text(&[
            "-c",
            "user.name=Worker",
            "-c",
            "user.email=worker@localhost",
            "commit",
            "--allow-empty",
            "-m",
            "chore: empty checkpoint",
        ])
        .unwrap();

    // Task stored under .agents/tasks/in-progress/ with legacy dependencies/scope keys
    let in_prog_doc = json!({
        "id": "T-INPROG-1",
        "title": "In-progress layout task",
        "goal": "Verify in-progress dir and legacy field aliases",
        "created_by": "worker",
        "dependencies": [],
        "scope": ["alpha", ".agents/tasks/in-progress/T-INPROG-1.json"],
        "accept": { "cmd": ["git", "rev-parse", "--verify", "HEAD:alpha/one.rs"] }
    });
    fx.write("alpha/one.rs", "pub const ONE: u8 = 1;\n");
    fx.write(
        ".agents/tasks/in-progress/T-INPROG-1.json",
        &format!("{}\n", serde_json::to_string_pretty(&in_prog_doc).unwrap()),
    );
    fx.commit_all("feat: alpha step 1\n\ntask: t-inprog-1");

    fx.write("alpha/two.rs", "pub const TWO: u8 = 2;\n");
    let head = fx.commit_all("feat: alpha step 2\n\nTask: T-INPROG-1");

    let plan_doc = json!({
        "schema": PLAN_SCHEMA,
        "plan_only": true,
        "base": { "rev": "HEAD", "commit": head },
        "requested": ["T-INPROG-1"],
        "included": [{ "id": "T-INPROG-1" }]
    });
    fx.write(
        "slice1-plan.json",
        &serde_json::to_string_pretty(&plan_doc).unwrap(),
    );

    let out = fx
        .run_args(&[
            "compose",
            "--name",
            "from-plan-manifest",
            "--base",
            &base,
            "--plan-manifest",
            "slice1-plan.json",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    assert!(receipt["plan_manifest_sha256"].as_str().is_some());
    assert_eq!(receipt["included"][0]["task_id"], "T-INPROG-1");
    assert_eq!(
        receipt["included"][0]["source_commits"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn compose_verify_accept_runs_task_commands_in_isolated_worktree_and_fails_closed_on_error() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("base");

    let ok_doc = json!({
        "id": "T-ACCEPT-OK",
        "title": "Passing accept task",
        "goal": "Verify composed tree contains ready.txt",
        "created_by": "worker",
        "deps": [],
        "scopes": ["mod_ok", ".agents/tasks/T-ACCEPT-OK.json"],
        "accept": { "cmd": ["git", "rev-parse", "--verify", "HEAD:mod_ok/ready.txt"] }
    });
    fx.write("mod_ok/ready.txt", "verified\n");
    fx.write(
        ".agents/tasks/T-ACCEPT-OK.json",
        &format!("{}\n", serde_json::to_string_pretty(&ok_doc).unwrap()),
    );
    fx.commit_all("feat: mod_ok\n\nTask: T-ACCEPT-OK");

    let bad_doc = json!({
        "id": "T-ACCEPT-FAIL",
        "title": "Failing accept task",
        "goal": "Check missing file fails closed",
        "created_by": "worker",
        "deps": [],
        "scopes": ["mod_bad", ".agents/tasks/T-ACCEPT-FAIL.json"],
        "accept": { "cmd": ["git", "rev-parse", "--verify", "HEAD:mod_bad/absent.txt"] }
    });
    fx.write("mod_bad/present.txt", "present\n");
    fx.write(
        ".agents/tasks/T-ACCEPT-FAIL.json",
        &format!("{}\n", serde_json::to_string_pretty(&bad_doc).unwrap()),
    );
    fx.commit_all("feat: mod_bad\n\nTask: T-ACCEPT-FAIL");

    // Passing --verify-accept records accept_verified_candidate and creates branch ref
    let ok_out = fx
        .run_args(&[
            "compose",
            "--name",
            "verify-pass",
            "--base",
            &base,
            "--select",
            "T-ACCEPT-OK",
            "--verify-accept",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&ok_out).unwrap();
    assert_eq!(receipt["qualification_status"], "accept_verified_candidate");
    assert_eq!(receipt["accept_verification"][0]["passed"], true);
    assert!(fx
        .git()
        .text(&["show-ref", "--verify", "refs/heads/distill/verify-pass"])
        .is_ok());

    // Failing --verify-accept fails closed before creating refs/heads/distill/verify-fail
    let fail_err = fx
        .run_args(&[
            "compose",
            "--name",
            "verify-fail",
            "--base",
            &base,
            "--select",
            "T-ACCEPT-FAIL",
            "--verify-accept",
            "--strict",
        ])
        .unwrap_err();
    assert!(fail_err.contains("accept_verification_failed"));
    assert!(fx
        .git()
        .text(&["show-ref", "--verify", "refs/heads/distill/verify-fail"])
        .is_err());
}

#[test]
fn orchestrate_discovers_tasks_from_range_composes_and_publishes_snapshot() {
    let fx = Fixture::new();
    fx.write("core/base.txt", "base\n");
    fx.write(
        ".agents/tasks/done/T-LEGACY-BASE.json",
        "{\n  \"id\": \"T-LEGACY-BASE\",\n  \"title\": \"Closed without scopes by susi tasks close\",\n  \"closed\": { \"at_unix\": 1794000000 }\n}\n",
    );
    let base = fx.commit_all("chore: base");

    fx.write("mod_one/lib.rs", "pub fn one() -> usize { 1 }\n");
    fx.write_task("T-ORCH-1", &["T-LEGACY-BASE"], &["mod_one"]);
    let c1 = fx.commit_all("feat: mod_one\n\nTask: T-ORCH-1");

    fx.write("mod_two/lib.rs", "pub fn two() -> usize { 2 }\n");
    fx.write_task("T-ORCH-2", &["T-ORCH-1"], &["mod_two"]);
    let c2 = fx.commit_all("feat: mod_two\n\nTask: T-ORCH-2");

    let range = format!("{base}..{c2}");
    let out = fx
        .run_args(&[
            "orchestrate",
            "--name",
            "orch-wave-1",
            "--base",
            &base,
            "--from",
            &c2,
            "--tasks-from-range",
            &range,
            "--publish-snapshot",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(receipt["mode"], "orchestrate");
    assert_eq!(receipt["included"].as_array().unwrap().len(), 2);
    assert_eq!(receipt["included"][0]["task_id"], "T-ORCH-1");
    assert_eq!(receipt["included"][1]["task_id"], "T-ORCH-2");
    assert_eq!(receipt["included"][0]["source_commits"][0], c1);

    let status_json = fx.run_args(&["status", "orch-wave-1", "--json"]).unwrap();
    let st: Value = serde_json::from_str(&status_json).unwrap();
    assert_eq!(st["branch_state"], "present_exact");
    assert_eq!(st["remote_state"], "published_exact");
}

#[test]
fn submit_main_reconciles_concurrent_peer_commits_without_force_push_and_rejects_unlabelled() {
    let fx = Fixture::new();
    fx.write("core/root.txt", "v0\n");
    fx.write_task("T-BASE-1", &[], &["core"]);
    let base = fx.commit_all("feat: base\n\nTask: T-BASE-1");

    // Fast-forward initial main submission
    let ff_json = fx.run_args(&["submit-main", "--json"]).unwrap();
    let ff: Value = serde_json::from_str(&ff_json).unwrap();
    assert_eq!(ff["status"], "published_fast_forward");
    assert_eq!(ff["reconciled"], false);

    // Simulate a concurrent peer pushing disjoint work directly to origin/main
    let peer_dir = fx.dir.join("peer");
    Git { root: &fx.dir }
        .text(&[
            "clone",
            fx.remote.to_str().unwrap(),
            peer_dir.to_str().unwrap(),
        ])
        .unwrap();
    let peer_git = Git { root: &peer_dir };
    fs::create_dir_all(peer_dir.join("peer_mod")).unwrap();
    fs::write(peer_dir.join("peer_mod/peer.txt"), "peer work\n").unwrap();
    peer_git.text(&["add", "-A"]).unwrap();
    peer_git
        .text(&[
            "-c",
            "user.name=Peer",
            "-c",
            "user.email=peer@localhost",
            "commit",
            "-m",
            "feat: peer\n\nTask: T-PEER-1",
        ])
        .unwrap();
    peer_git
        .text(&["push", "origin", "HEAD:refs/heads/main"])
        .unwrap();
    let peer_head = peer_git.resolve_commit("HEAD").unwrap();
    assert_ne!(peer_head, base);

    // Meanwhile local worker commits isolated work on top of `base` (diverged from origin/main)
    fx.write("agent_mod/own.txt", "own isolated work\n");
    fx.write_task("T-OWN-1", &[], &["agent_mod"]);
    let own_commit = fx.commit_all("feat: own\n\nTask: T-OWN-1");

    // submit-main reconciles in-memory via 3-way merge-tree + commit-tree and fast-forwards origin/main!
    let rec_json = fx
        .run_args(&["submit-main", "--rev", &own_commit, "--json"])
        .unwrap();
    let rec: Value = serde_json::from_str(&rec_json).unwrap();
    assert_eq!(rec["status"], "published_reconciled");
    assert_eq!(rec["reconciled"], true);
    let published = rec["published_commit"].as_str().unwrap();

    // Both peer work and own work are preserved on origin/main!
    let bare = Git { root: &fx.remote };
    assert!(bare.ancestor(&peer_head, published).unwrap());
    assert!(bare.ancestor(&own_commit, published).unwrap());
    // The reconcile merge on shared main carries the time it was made,
    // not the fixed composition date.
    let dated: u64 = bare
        .text(&["log", "-1", "--format=%ct", published])
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(now.abs_diff(dated) < 600, "merge dated {dated}, now {now}");

    // Unlabelled commit is refused before touching origin/main
    fx.write("agent_mod/unlabelled.txt", "no trailer\n");
    let bad_commit = fx.commit_all("chore: forgot trailer");
    let err = fx
        .run_args(&["submit-main", "--rev", &bad_commit])
        .unwrap_err();
    assert!(err.contains("missing_task_trailer"));
}

#[test]
fn synthetic_base_merge_three_way_handles_clean_merge_and_conflict() {
    let fx = Fixture::new();
    fx.write("shared.txt", "alpha\nbeta\ngamma\n");
    fx.git().text(&["add", "-A"]).unwrap();
    fx.git().text(&["commit", "-m", "base"]).unwrap();
    let base = fx.git().resolve_commit("HEAD").unwrap();

    fx.write("ours_only.txt", "ours\n");
    fx.git().text(&["add", "-A"]).unwrap();
    fx.git().text(&["commit", "-m", "ours"]).unwrap();
    let ours = fx.git().resolve_commit("HEAD").unwrap();

    fx.git().text(&["checkout", "-q", &base]).unwrap();
    fx.write("shared.txt", "alpha\nbeta\ngamma\ndelta\n");
    fx.git().text(&["add", "-A"]).unwrap();
    fx.git().text(&["commit", "-m", "theirs-clean"]).unwrap();
    let theirs_clean = fx.git().resolve_commit("HEAD").unwrap();

    let clean = fx
        .git()
        .merge_three_way_synthetic_base(&base, &ours, &theirs_clean)
        .unwrap();
    let ThreeWayMerge::Clean(merged_tree) = clean else {
        panic!("expected clean 3-way merge, got {clean:?}");
    };
    assert_eq!(
        fx.git()
            .text(&["cat-file", "-p", &format!("{merged_tree}:shared.txt")])
            .unwrap(),
        "alpha\nbeta\ngamma\ndelta"
    );
    assert_eq!(
        fx.git()
            .text(&["cat-file", "-p", &format!("{merged_tree}:ours_only.txt")])
            .unwrap(),
        "ours"
    );

    fx.git().text(&["checkout", "-q", &base]).unwrap();
    fx.write("ours_only.txt", "conflicting-theirs\n");
    fx.git().text(&["add", "-A"]).unwrap();
    fx.git().text(&["commit", "-m", "theirs-conflict"]).unwrap();
    let theirs_conflict = fx.git().resolve_commit("HEAD").unwrap();

    let conflict = fx
        .git()
        .merge_three_way_synthetic_base(&base, &ours, &theirs_conflict)
        .unwrap();
    assert!(matches!(conflict, ThreeWayMerge::Conflict(_)));
}

#[test]
fn disjoint_lanes_and_stable_patch_ids_are_recorded_on_distill_receipts() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("initial base");

    fx.write("crates/alpha/lib.rs", "pub fn a1() {}\n");
    fx.write_task("T-ALPHA-1", &[], &["crates/alpha"]);
    fx.commit_all("feat: alpha 1\n\nTask: T-ALPHA-1");

    fx.write("crates/beta/lib.rs", "pub fn b1() {}\n");
    fx.write_task("T-BETA-1", &[], &["crates/beta"]);
    fx.commit_all("feat: beta 1\n\nTask: T-BETA-1");

    fx.write("crates/alpha/mod2.rs", "pub fn a2() {}\n");
    fx.write_task("T-ALPHA-2", &[], &["crates/alpha"]);
    fx.commit_all("feat: alpha 2\n\nTask: T-ALPHA-2");

    let out = fx
        .run_args(&[
            "compose",
            "--name",
            "lanes-check",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-ALPHA-1,T-BETA-1,T-ALPHA-2",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&out).unwrap();
    let lanes = receipt["lanes"].as_array().unwrap();
    assert_eq!(lanes.len(), 2, "{receipt}");
    assert_eq!(
        lanes[0]["tasks"],
        json!(["T-ALPHA-1", "T-ALPHA-2"]),
        "{lanes:?}"
    );
    assert_eq!(lanes[1]["tasks"], json!(["T-BETA-1"]), "{lanes:?}");
    for entry in receipt["included"].as_array().unwrap() {
        let pid = entry["patch_id"].as_str().unwrap();
        assert_eq!(pid.len(), 40);
    }
}

#[test]
fn harvest_audit_admits_attributed_apache2_and_refuses_unattributed_or_copyleft_commits() {
    let fx = Fixture::new();
    fx.write("LICENSE", "Apache License 2.0\n");
    fx.write(
        "THIRD_PARTY_NOTICES",
        "Portions Copyright 2026 Upstream Project.\n",
    );
    let base = fx.commit_all("initial base with license");

    // 1. Valid attributed Apache-2.0 harvest
    fx.write(
        "crates/harvested/lib.rs",
        "// Modified by Susi contributors, 2026-11-11: std port.\npub fn ported() {}\n",
    );
    let manifest = json!({
        "schema": "susi-harvest-manifest-v1",
        "id": "H-WORKER-1",
        "source_url": "https://gitlab.com/example/upstream.git",
        "source_commit": "0123456789abcdef0123456789abcdef01234567",
        "source_paths": ["src/lib.rs"],
        "destination_paths": ["crates/harvested/lib.rs"],
        "spdx_license": "Apache-2.0",
        "license_sha256": "c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4",
        "copyright_holders": ["Upstream Project"],
        "notice_text": "Portions Copyright 2026 Upstream Project.",
        "import_mode": "rust-port",
        "modified": true,
        "modification_summary": "Ported to std-only Rust.",
        "agent": "test-worker",
        "date": "2026-11-11",
        "task_id": "T-HARV-1"
    });
    fx.write(
        ".agents/harvest/H-WORKER-1.json",
        &format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()),
    );
    fx.write_task(
        "T-HARV-1",
        &[],
        &["crates/harvested", ".agents/harvest/H-WORKER-1.json"],
    );
    fx.commit_all(
        "feat: harvest upstream\n\nTask: T-HARV-1\nHarvest: H-WORKER-1\nOrigin: https://gitlab.com/example/upstream.git@0123456789abcdef0123456789abcdef01234567:src/lib.rs\nOrigin-License: Apache-2.0",
    );

    let ok_out = fx
        .run_args(&[
            "compose",
            "--name",
            "harvest-ok",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-HARV-1",
            "--strict",
            "--json",
        ])
        .unwrap();
    let ok_receipt: Value = serde_json::from_str(&ok_out).unwrap();
    assert_eq!(ok_receipt["harvest_audit"]["passed"], json!(true));
    assert_eq!(ok_receipt["harvest_audit"]["manifests_verified"], json!(1));
    assert_eq!(
        ok_receipt["harvest_audit"]["harvest_commits_checked"],
        json!(1)
    );

    let vh_out = fx
        .run_args(&[
            "verify-harvest",
            "--range",
            &format!("{base}..HEAD"),
            "--strict",
            "--json",
        ])
        .unwrap();
    let vh_json: Value = serde_json::from_str(&vh_out).unwrap();
    assert_eq!(vh_json["passed"], json!(true));

    // 2. Unattributed GPL Origin commit -> refused under --strict
    fx.write("crates/bad/lib.rs", "pub fn bad() {}\n");
    fx.write_task("T-BADHARV-1", &[], &["crates/bad"]);
    fx.commit_all(
        "feat: bad harvest\n\nTask: T-BADHARV-1\nOrigin: https://example.org/gpl.git@fedcba9876543210fedcba9876543210fedcba98\nOrigin-License: GPL-3.0-only",
    );
    let refused = fx.run_args(&[
        "compose",
        "--name",
        "harvest-bad",
        "--base",
        &base,
        "--from",
        "HEAD",
        "--select",
        "T-BADHARV-1",
        "--strict",
        "--json",
    ]);
    let err = refused.unwrap_err();
    assert!(err.contains("disallowed_or_unattributed_harvest"), "{err}");
}

#[test]
fn quarantine_fetch_isolates_upstream_commit_under_refs_susi_upstream_without_hooks() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let _base = fx.commit_all("initial");

    let upstream_dir = fx.dir.join("upstream-repo");
    fs::create_dir_all(&upstream_dir).unwrap();
    let up_git = Git {
        root: &upstream_dir,
    };
    up_git.text(&["init", "--initial-branch=main"]).unwrap();
    fs::write(upstream_dir.join("lib.rs"), "pub fn up() -> u32 { 7 }\n").unwrap();
    up_git.text(&["add", "-A"]).unwrap();
    up_git
        .text(&[
            "-c",
            "user.name=Upstream",
            "-c",
            "user.email=up@localhost",
            "commit",
            "-m",
            "upstream commit",
        ])
        .unwrap();
    let up_sha = up_git.resolve_commit("HEAD").unwrap();

    let q_out = fx
        .run_args(&[
            "quarantine-fetch",
            upstream_dir.to_str().unwrap(),
            &up_sha,
            "--slug",
            "upstream-demo",
            "--json",
        ])
        .unwrap();
    let q_json: Value = serde_json::from_str(&q_out).unwrap();
    assert_eq!(q_json["status"], json!("quarantined"));
    let expected_ref = format!("refs/susi/upstream/upstream-demo/{up_sha}");
    assert_eq!(q_json["quarantine_ref"], json!(expected_ref));
    assert_eq!(fx.git().resolve_commit(&expected_ref).unwrap(), up_sha);
}

#[test]
fn pull_fetches_remote_only_explicit_snapshot_refs_for_base_and_source() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("initial base");
    fx.git()
        .text(&["push", "origin", &format!("{base}:refs/heads/main")])
        .unwrap();

    fx.write(
        "crates/remote_tag/lib.rs",
        "pub fn remote_tag() -> u32 { 7 }\n",
    );
    fx.write_task("T-REMOTE-TAG-1", &[], &["crates/remote_tag"]);
    let feature = fx.commit_all("feat: remote tag input\n\nTask: T-REMOTE-TAG-1");

    let base_tag = "susi-distill/v0-21-64-remote-base";
    let source_tag = "susi-distill/v0-21-64-remote-source";
    let base_ref = format!("refs/tags/{base_tag}");
    let source_ref = format!("refs/tags/{source_tag}");
    fx.git()
        .text(&["tag", "-a", base_tag, "-m", "remote base", &base])
        .unwrap();
    fx.git()
        .text(&["tag", "-a", source_tag, "-m", "remote source", &feature])
        .unwrap();
    let base_spec = format!("{base_ref}:{base_ref}");
    let source_spec = format!("{source_ref}:{source_ref}");
    fx.git()
        .text(&["push", "origin", &base_spec, &source_spec])
        .unwrap();
    fx.git().text(&["tag", "-d", base_tag]).unwrap();
    fx.git().text(&["tag", "-d", source_tag]).unwrap();
    assert!(fx.git().local_ref_oid(&base_ref).unwrap().is_none());
    assert!(fx.git().local_ref_oid(&source_ref).unwrap().is_none());

    let output = fx
        .run_args(&[
            "compose",
            "--name",
            "remote-only-tags-1",
            "--base",
            &base_ref,
            "--from",
            &source_ref,
            "--select",
            "T-REMOTE-TAG-1",
            "--pull",
            "--strict",
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(receipt["included"][0]["task_id"], "T-REMOTE-TAG-1");
    assert_eq!(fx.git().resolve_commit(&base_ref).unwrap(), base);
    assert_eq!(fx.git().resolve_commit(&source_ref).unwrap(), feature);
}

#[test]
fn pull_refuses_to_overwrite_a_different_local_explicit_remote_ref() {
    let fx = Fixture::new();
    fx.write("README.md", "base\n");
    let base = fx.commit_all("initial base");
    fx.git()
        .text(&["push", "origin", &format!("{base}:refs/heads/main")])
        .unwrap();

    let tag = "susi-distill/v0-21-64-immutable-check";
    let reference = format!("refs/tags/{tag}");
    fx.git()
        .text(&["tag", "-a", tag, "-m", "remote tag", &base])
        .unwrap();
    let spec = format!("{reference}:{reference}");
    fx.git().text(&["push", "origin", &spec]).unwrap();
    fx.git().text(&["tag", "-d", tag]).unwrap();

    fx.write("local-only.txt", "local\n");
    let local = fx.commit_all("local ref target");
    fx.git()
        .text(&["tag", "-a", tag, "-m", "different local tag", &local])
        .unwrap();
    let error = fx.git().pull_explicit_remote_ref(&reference).unwrap_err();
    assert!(
        error.contains("refusing to overwrite an immutable ref"),
        "{error}"
    );
    assert_eq!(fx.git().resolve_commit(&reference).unwrap(), local);
}

#[test]
fn pull_reconciles_diverged_origin_main_and_emits_layout_valid_release_record() {
    let fx = Fixture::new();
    fx.write("rust-toolchain.toml", "[toolchain]\nchannel = \"1.99.0\"\n");
    let base = fx.commit_all("initial base");
    fx.git()
        .text(&["push", "origin", &format!("{base}:refs/heads/main")])
        .unwrap();

    // Peer clone pushes a Task-labelled feature onto origin/main while our local
    // clone stays un-fetched and independently commits another Task-labelled feature.
    let peer_dir = fx.dir.join("peer");
    fx.git()
        .text(&[
            "clone",
            fx.remote.to_str().unwrap(),
            peer_dir.to_str().unwrap(),
        ])
        .unwrap();
    let peer_git = Git { root: &peer_dir };
    fs::create_dir_all(peer_dir.join(".agents/tasks")).unwrap();
    fs::create_dir_all(peer_dir.join("crates/peer_mod")).unwrap();
    let peer_task = json!({
        "id": "T-PEER-1",
        "title": "Peer remote feature",
        "goal": "Ship peer feature on origin/main",
        "created_by": "peer-worker",
        "created_unix": 1_794_300_100u64,
        "status": "done",
        "deps": [],
        "scopes": ["crates/peer_mod", ".agents/tasks/T-PEER-1.json"],
        "accept": { "cmd": ["cargo", "test", "--offline", "--locked"] }
    });
    fs::write(
        peer_dir.join(".agents/tasks/T-PEER-1.json"),
        format!("{}\n", serde_json::to_string_pretty(&peer_task).unwrap()),
    )
    .unwrap();
    fs::write(
        peer_dir.join("crates/peer_mod/lib.rs"),
        "pub fn peer_ok() -> u32 { 11 }\n",
    )
    .unwrap();
    peer_git.text(&["add", "-A"]).unwrap();
    peer_git
        .text(&[
            "-c",
            "user.name=Peer",
            "-c",
            "user.email=peer@localhost",
            "commit",
            "-m",
            "feat: peer feature\n\nTask: T-PEER-1",
        ])
        .unwrap();
    peer_git
        .text(&["push", "origin", "HEAD:refs/heads/main"])
        .unwrap();

    // Local clone commits T-LOCAL-1 without having fetched T-PEER-1 yet.
    fx.write(
        "crates/local_mod/lib.rs",
        "pub fn local_ok() -> u32 { 22 }\n",
    );
    fx.write_task("T-LOCAL-1", &[], &["crates/local_mod"]);
    fx.commit_all("feat: local feature\n\nTask: T-LOCAL-1");

    let record_out = fx.dir.join("v0.21.61.json");
    let compose_json = fx
        .run_args(&[
            "compose",
            "--name",
            "rc-pull-1",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-PEER-1,T-LOCAL-1",
            "--release-candidate",
            "--pull",
            "--strict",
            "--release-tag",
            "v0.21.61",
            "--release-record-out",
            record_out.to_str().unwrap(),
            "--json",
        ])
        .unwrap();
    let receipt: Value = serde_json::from_str(&compose_json).unwrap();
    assert_eq!(receipt["included"].as_array().unwrap().len(), 2);

    // Both refs/heads/release/rc-pull-1 and refs/candidates/release/rc-pull-1 must exist.
    let rel_head = fx
        .git()
        .text(&[
            "show-ref",
            "--verify",
            "--hash",
            "refs/heads/release/rc-pull-1",
        ])
        .unwrap();
    let cand_head = fx
        .git()
        .text(&[
            "show-ref",
            "--verify",
            "--hash",
            "refs/candidates/release/rc-pull-1",
        ])
        .unwrap();
    assert_eq!(rel_head, cand_head);

    // Also test explicit `release-record` subcommand with reference binary sha256.
    let asset_sha = "a".repeat(64);
    let explicit_out = fx.dir.join("v0.21.61-qualified.json");
    fx.run_args(&[
        "release-record",
        "--name",
        "rc-pull-1",
        "--tag",
        "v0.21.61",
        "--asset",
        &format!("susi-linux-x86_64:x86_64-unknown-linux-gnu:susi:susi:{asset_sha}"),
        "--out",
        explicit_out.to_str().unwrap(),
    ])
    .unwrap();

    for check_path in [&record_out, &explicit_out] {
        let text = fs::read_to_string(check_path).unwrap();
        let val: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(val["schema"], "susi-release-record/v1");
        assert_eq!(val["product"], "susi");
        assert_eq!(val["version"], "0.21.61");
        assert_eq!(val["tag"], "v0.21.61");
        assert_eq!(val["commit"], rel_head);
        assert_eq!(val["build"]["toolchain"], "1.99.0");

        let lines: Vec<&str> = text.lines().collect();
        for field in [
            "version",
            "tag",
            "commit",
            "toolchain",
            "profile",
            "features",
        ] {
            let prefix = format!("\"{field}\": \"");
            let hits: Vec<&&str> = lines
                .iter()
                .filter(|l| l.trim_start().starts_with(&prefix))
                .collect();
            assert_eq!(hits.len(), 1, "field {field} must appear on one line");
        }
        let env_lines: Vec<&&str> = lines
            .iter()
            .filter(|l| l.trim_start().starts_with("\"env\": ["))
            .collect();
        assert_eq!(env_lines.len(), 1);
        assert!(env_lines[0].trim().trim_end_matches(',').ends_with(']'));
    }

    // Offline fallback: point origin at a missing path and verify --pull still works cleanly.
    let missing_remote = fx.dir.join("offline-missing.git");
    fx.git()
        .text(&[
            "remote",
            "set-url",
            "origin",
            missing_remote.to_str().unwrap(),
        ])
        .unwrap();
    let offline_plan = fx
        .run_args(&[
            "plan",
            "--name",
            "offline-plan",
            "--base",
            &base,
            "--from",
            "HEAD",
            "--select",
            "T-LOCAL-1",
            "--pull",
            "--strict",
            "--json",
        ])
        .unwrap();
    let offline_val: Value = serde_json::from_str(&offline_plan).unwrap();
    assert_eq!(offline_val["included"].as_array().unwrap().len(), 1);
}
