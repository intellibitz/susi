#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The CI workflows' performance invariants.
//!
//! Each of these was measured to cost real wall time before it was fixed, and
//! each is invisible once broken: a job pinned to a retired runner image does
//! not fail, it sits queued; a shared cache key does not fail, it restores a
//! cache that holds the wrong dependencies; a cancelled `main` run does not
//! fail, it simply never reports. Nothing goes red, so nothing but a test
//! notices. The workflows are read as text - the root crate carries no YAML
//! parser and this does not justify one.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every `.github/workflows/*.yml`, as (file name, text).
fn workflows() -> Vec<(String, String)> {
    let dir = root().join(".github/workflows");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml"))
        .map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&p).unwrap();
            (name, text)
        })
        .collect();
    out.sort();
    out
}

/// A text with its comment lines removed, so prose explaining a fix cannot
/// trip the check that forbids it.
fn code(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

struct Job {
    id: String,
    body: String,
}

/// The jobs of one workflow: a job id is a key indented exactly two spaces
/// under `jobs:`; its body is every following non-comment line until the next.
fn jobs(workflow: &str) -> Vec<Job> {
    let mut out: Vec<Job> = Vec::new();
    let mut in_jobs = false;
    for line in code(workflow).lines() {
        if line.starts_with("jobs:") {
            in_jobs = true;
            continue;
        }
        if !in_jobs {
            continue;
        }
        let id = line
            .strip_prefix("  ")
            .and_then(|rest| rest.strip_suffix(':'))
            .filter(|rest| !rest.is_empty() && !rest.starts_with(' '));
        match id {
            Some(id) => out.push(Job {
                id: id.to_string(),
                body: String::new(),
            }),
            None => {
                if let Some(job) = out.last_mut() {
                    job.body.push_str(line);
                    job.body.push('\n');
                }
            }
        }
    }
    out
}

fn job<'a>(jobs: &'a [Job], id: &str) -> &'a Job {
    jobs.iter()
        .find(|j| j.id == id)
        .unwrap_or_else(|| panic!("no job `{id}` in test.yml"))
}

/// The value of the first `key:` line in `body`, unquoted.
fn value_of(body: &str, key: &str) -> Option<String> {
    body.lines().find_map(|l| {
        l.trim_start()
            .strip_prefix(&format!("{key}:"))
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
    })
}

/// GitHub-hosted runner images that no longer exist. A job asking for one is
/// never picked up: it has no runner and no steps, and waits in the queue.
const RETIRED_RUNNER_IMAGES: &[&str] = &[
    "ubuntu-16.04",
    "ubuntu-18.04",
    "ubuntu-20.04",
    "macos-10.15",
    "macos-11",
    "macos-12",
    "macos-13",
    "windows-2016",
    "windows-2019",
];

#[test]
fn no_workflow_targets_a_retired_runner_image() {
    for (name, text) in workflows() {
        for line in code(&text).lines() {
            for image in RETIRED_RUNNER_IMAGES {
                // Whole-token match: `macos-11` must not flag `macos-114`.
                let hit = line.match_indices(image).any(|(at, _)| {
                    let after = line[at + image.len()..].chars().next();
                    !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
                });
                assert!(
                    !hit,
                    "{name}: `{}` targets the retired runner image `{image}`. No runner \
                     carries that label, so the job never starts and holds the whole run \
                     open until it is cancelled (Kani sat queued 153 min on ubuntu-20.04)",
                    line.trim()
                );
            }
        }
    }
}

#[test]
fn a_main_run_is_never_cancelled_by_the_next_merge() {
    let text = read(".github/workflows/test.yml");
    let line = code(&text)
        .lines()
        .find(|l| l.trim_start().starts_with("cancel-in-progress:"))
        .expect("test.yml declares cancel-in-progress")
        .trim()
        .to_string();
    assert!(
        line.contains("refs/heads/main") && line.contains("!="),
        "`{line}`: cancelling in-progress runs must exempt main. Merges land every few \
         minutes and the suite takes ~15, so a main run that the next merge can cancel \
         never finishes: no gate verdict, no refs/verified records, and no cargo cache \
         saved (the save is a post-step a cancelled job never reaches)"
    );
}

#[test]
fn every_test_workflow_job_is_bounded() {
    let text = read(".github/workflows/test.yml");
    for j in jobs(&text) {
        assert!(
            j.body.contains("timeout-minutes:"),
            "job `{}` has no timeout-minutes: the default is six hours, so a hung job holds \
             the run (and the merge it gates) for that long",
            j.id
        );
    }
}

#[test]
fn matrix_jobs_keep_one_cargo_cache_per_shard() {
    for (name, text) in workflows() {
        for j in jobs(&text) {
            if !j.body.contains("matrix:") || !j.body.contains("Swatinem/rust-cache") {
                continue;
            }
            let key = value_of(&j.body, "shared-key").unwrap_or_default();
            assert!(
                key.contains("matrix."),
                "{name}: matrix job `{}` caches cargo under `{key}` for every shard. All \
                 shards share one job id, so without a per-shard shared-key they all use \
                 one cache entry; the first to finish saves it (a small one) and the rest \
                 restore dependencies they do not need and rebuild the ones they do - \
                 root-cli took 12 min to build tests that run in 40 s",
                j.id
            );
        }
    }
}

/// `cargo install` lines whose build is measured to be negligible, so a cache
/// would only add a step: kani-verifier is a thin driver, 2.6 s to build.
const QUICK_INSTALLS: &[&str] = &["kani-verifier"];

#[test]
fn source_built_tools_are_pinned_and_the_slow_ones_cached() {
    for (name, text) in workflows() {
        for j in jobs(&text) {
            for line in j.body.lines().filter(|l| l.contains("cargo install")) {
                assert!(
                    line.contains("--version"),
                    "{name}: `{}` is unpinned, so a cache key cannot name what it holds \
                     and a new release changes the tool under a recorded baseline",
                    line.trim()
                );
                if QUICK_INSTALLS.iter().any(|q| line.contains(q)) {
                    continue;
                }
                assert!(
                    j.body.contains("actions/cache@") || j.body.contains("Swatinem/rust-cache"),
                    "{name}: job `{}` runs `{}` with no cache: a release-profile source \
                     build (cargo-geiger: ~3 min) on every run. If this one is genuinely \
                     quick, measure it and add it to QUICK_INSTALLS",
                    j.id,
                    line.trim()
                );
            }
        }
    }
}

#[test]
fn kani_unstable_flags_travel_together() {
    // 0.68 refuses `--concrete-playback` without `-Z concrete-playback`. The job
    // sat on a dead runner for so long that nobody saw it fail the moment it ran.
    let text = read(".github/workflows/test.yml");
    let body = code(&text);
    if body.contains("--concrete-playback") {
        assert!(
            body.contains("-Z concrete-playback"),
            "`cargo kani --concrete-playback` is unstable: it needs `-Z concrete-playback`"
        );
    }
}

#[test]
fn a_branch_push_starts_from_the_cache_main_writes() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let check = job(&jobs, "check");
    let lint = job(&jobs, "lint");
    let key = value_of(&check.body, "shared-key");
    assert!(
        key.is_some() && key == value_of(&lint.body, "shared-key"),
        "`check` (branch pushes) and `lint` (main) must share one cargo cache key: a cache \
         is readable only from the ref that wrote it or from main, and `check` never runs on \
         main, so its own cache was unreadable by every fresh agent branch - each task's \
         first push compiled the workspace cold (7.5 min against 0.8 once warm)"
    );
    let save = value_of(&check.body, "save-if").unwrap_or_default();
    assert!(
        save.contains("refs/heads/main"),
        "`check` must save its cargo cache only on main (it reads main's); `{save}` writes \
         one cache entry per task branch and crowds the repository's cache quota"
    );
    assert!(
        !value_of(&lint.body, "save-if").is_some_and(|v| v.contains("false")),
        "`lint` is what writes the cache `check` reads on main; it must save"
    );
}

/// The merge gate is also the unit of the serialized merge queue: every merge
/// turns the next green head stale and each stale head costs one more run, so a
/// gate that executes tests multiplies the suite by the length of the queue.
/// Measured on a code-changing branch push: `cargo nextest` 173-267 s plus the
/// hermetic re-run 18-67 s of a 308-446 s job, against 96 s when no crate was
/// touched. It compiles and lints; the tests run in the agent's local gate, in
/// the sharded suite on main after every merge, and at release.
#[test]
fn the_branch_gate_compiles_and_lints_but_never_executes_tests() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let check = job(&jobs, "check");
    for executes_tests in ["nextest", "cargo test", "check-hermetic-tests"] {
        assert!(
            !check.body.contains(executes_tests),
            "the branch-push `check` job must not execute tests (`{executes_tests}`): the same \
             tests just ran in the agent's finish gate on this tree, and the job's length is \
             the latency of every stale-head retest in the merge queue"
        );
    }
    assert!(
        check.body.contains("--all-targets") && check.body.contains("cargo clippy"),
        "clippy over `--all-targets` is the compile gate: it covers the test targets, so a \
         test that stops compiling still fails before merge"
    );
}

/// With tests off the gate, what it must still catch is the compile break a
/// change causes in its callers: a signature change leaves the changed crate's
/// own targets green and breaks whichever crate calls it.
#[test]
fn the_branch_gate_lints_the_dependents_of_what_changed() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let check = job(&jobs, "check");
    assert!(
        check.body.contains("ci-changed-crates.sh --dependents"),
        "the branch gate must resolve `changed crates + their dependents`, not only the \
         changed crates: the crate that breaks is the one that calls the changed one"
    );
    assert!(
        !check.body.contains("cargo check"),
        "clippy over `--all-targets` is a superset of `cargo check`; a separate check step \
         only compiles the same crates twice"
    );
}

/// `Compile Check (branch pushes)` is a required status check
/// (`scripts/github-enforce.sh`): a renamed job never reports, and nothing
/// merges until someone edits the ruleset.
#[test]
fn the_branch_gate_keeps_the_name_branch_protection_requires() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let check = job(&jobs, "check");
    let name = value_of(&check.body, "name").unwrap_or_default();
    assert!(
        read("scripts/github-enforce.sh").contains(&format!("\"context\": \"{name}\"")),
        "job `check` is named `{name}`, which scripts/github-enforce.sh does not require"
    );
}

/// Taking tests off the branch gate is only sound while they still run
/// somewhere on every merge: the sharded suite on main.
#[test]
fn the_tests_still_run_on_main_where_the_branch_gate_does_not_run_them() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let test = job(&jobs, "test");
    assert!(
        test.body.contains("cargo nextest run"),
        "the sharded `test` job is where the tests run once the branch gate stops running them"
    );
    assert!(
        test.body.contains("refs/heads/main"),
        "the sharded `test` job must run on main"
    );
}

/// With the branch gate no longer executing tests, the sharded suite on main is
/// the first CI run that does, so a flaky test lands there. `Test (gemi)` failed
/// on main in a 0.6 s test on a commit that did not touch gemi, and Main Failure
/// Attribution blamed the pull request that merely preceded it. A test is retried
/// alone; one that passes on retry is reported FLAKY rather than failing the shard.
#[test]
fn the_main_shards_retry_a_flaky_test_alone() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let test = job(&jobs, "test");
    assert!(
        test.body.contains("cargo nextest run --locked --retries"),
        "the `test` shards must run nextest with `--retries`: a flake would otherwise redden \
         main and put a misattributed comment on an innocent pull request"
    );
}

/// The pre-push hook lints what the finish gate lints, with the same command
/// line, so its run is a cache hit. `--workspace` compiled every other crate's
/// test targets too - a cold build the gate never needed - on every Rust push.
#[test]
fn the_pre_push_hook_lints_the_gates_scoped_set_not_the_whole_workspace() {
    let hook = read(".githooks/pre-push");
    assert!(
        hook.contains("ci-changed-crates.sh\" --dependents"),
        "pre-push must resolve the changed crates and their dependents, as the gate does"
    );
    assert!(
        hook.contains(
            "cargo clippy --locked --all-targets $(printf ' -p %s' $LINT_PKGS) -- -D warnings"
        ),
        "the scoped lint must use the gate's exact command line, or it is a second compile"
    );
    let gate = read("scripts/parallel-workflow.sh");
    assert!(
        gate.contains("cargo clippy --locked --all-targets $lsel -- -D warnings"),
        "the gate's scoped lint changed; the hook must change with it so the run stays a cache hit"
    );
}

/// A release publishes binaries for half an hour of build time, and its gate was
/// not the proof it read as: bare `cargo test` selects one package of 86, and
/// release.yml ran no tests. Nothing builds until the full suite has passed on the
/// tagged commit, so a red commit costs one job instead of four builds.
#[test]
fn no_release_binary_is_built_before_the_suite_has_passed_on_the_tagged_commit() {
    let text = read(".github/workflows/release.yml");
    let jobs = jobs(&text);
    let tests = job(&jobs, "tests");
    assert!(
        tests.body.contains("scripts/release-await-tests.sh"),
        "the `tests` job must run the script that proves the suite on this sha"
    );
    assert!(
        tests.body.contains("actions: write"),
        "dispatching the Test workflow on the tag needs `actions: write`"
    );
    for builder in ["build", "build-cuda"] {
        let b = job(&jobs, builder);
        assert!(
            b.body.contains("needs: [verify-tag, tests]"),
            "`{builder}` must wait for `tests`: its binaries are published from a commit that \
             has not been shown to pass the suite otherwise"
        );
    }
}

/// release.yml dispatches the Test workflow on the tag. A run there restores the
/// caches main wrote and writes none (an entry saved under a tag ref is readable by
/// nothing else, and the repository is at its 10 GB quota), and skips the gates
/// that exist for merges to main.
#[test]
fn a_tag_run_of_the_suite_writes_no_cache_and_skips_the_main_only_gates() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    for gate in ["coverage", "kani"] {
        assert!(
            job(&jobs, gate).body.contains("refs/tags/"),
            "`{gate}` gates merges to main and must be skipped on a tag run: it is minutes a \
             release does not need, and the coverage target cache is 1.5 GB"
        );
    }
    for saver in ["test", "lint", "e2e"] {
        assert!(
            job(&jobs, saver)
                .body
                .contains("save-if: ${{ github.ref == 'refs/heads/main' }}"),
            "`{saver}` must save its cargo cache only on main, or a tag run fills the quota with \
             an entry nothing can read"
        );
    }
}

/// Cargo resolves a relative `linker` in `.cargo/config.toml` to an absolute path
/// inside the current worktree and passes it as `-C linker=...`, and that path is
/// part of every unit's fingerprint: so no two worktrees could share a compiled
/// crate (registry crates included) and each overwrote the other's artifacts in
/// the shared target dir. Measured: a fresh worktree compiled 716 crates in 656 s;
/// with a path-independent config, one created after a warm build compiled 81 in
/// 176 s. `scripts/susi-worktree.sh` shares one target dir *for this reason* and it
/// never worked until the linker path stopped differing.
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

#[test]
fn the_unsafe_ratchet_saves_its_caches_only_on_main() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let ratchet = job(&jobs, "unsafe-ratchet");
    let save = value_of(&ratchet.body, "save-if").unwrap_or_default();
    assert!(
        save.contains("refs/heads/main"),
        "`unsafe-ratchet` must save its cargo cache only on main (branches read main's); \
         `{save}` writes a 321 MB entry per task branch, which held the repository over its \
         10 GB cache quota until eviction took the 10 MB cargo-geiger entry and failed every \
         push of every branch"
    );
    // `actions/cache@` saves in a post-step on whatever ref it ran on; geiger's entry
    // is a restore plus a save that only main runs.
    assert!(
        !ratchet.body.contains("actions/cache@"),
        "cache cargo-geiger with `actions/cache/restore` and a main-only `actions/cache/save`: \
         `actions/cache` also saves, on every branch push"
    );
    let saves: Vec<String> = steps(&ratchet.body)
        .into_iter()
        .filter(|s| s.contains("actions/cache/save"))
        .collect();
    assert!(
        !saves.is_empty()
            && saves
                .iter()
                .all(|s| value_of(s, "if").is_some_and(|v| v.contains("refs/heads/main"))),
        "`unsafe-ratchet` needs a cargo-geiger `actions/cache/save` step gated on \
         `github.ref == 'refs/heads/main'`; without one the entry is never rewritten once \
         evicted, and a branch-side save adds ~10 MB per task branch:\n{saves:?}"
    );
}

#[test]
fn the_slow_all_features_build_is_not_a_serial_step_of_another_shard() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let test = job(&jobs, "test");
    assert!(
        test.body.contains("name: leaf-all-features"),
        "susi-leaf-services --all-features (wasmer's cranelift, ~7.5 min) runs as its own \
         parallel shard"
    );
    let serial = test
        .body
        .lines()
        .any(|l| l.contains("if: matrix.shard.name =="));
    assert!(
        !serial,
        "a shard-conditional step makes one shard run two builds back to back; add a shard \
         instead so they run in parallel (foundation was ~10 min for 2.5 min of work)"
    );
}

#[test]
fn ci_bootstrap_runs_through_scripts_not_per_job_boilerplate() {
    for script in ["scripts/ci-system-deps.sh", "scripts/ci-install-mold.sh"] {
        let path = root().join(script);
        assert!(path.is_file(), "{script} is missing");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = path.metadata().unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "{script} is not executable");
        }
    }
    let text = read(".github/workflows/test.yml");
    assert!(
        !code(&text).contains("apt-get"),
        "test.yml runs `apt-get update` inline again: the runner image already carries \
         pkg-config, libssl-dev and cmake, so scripts/ci-system-deps.sh installs only what \
         is missing instead of refreshing the package index in seven jobs of every run"
    );
    assert!(
        !code(&text).contains("rui314/mold"),
        "test.yml downloads mold inline again: use scripts/ci-install-mold.sh so every job \
         that links gets it, not just the one that happened to copy the block"
    );
    // Every job that compiles and links test binaries links them with mold.
    let jobs = jobs(&text);
    for id in ["check", "test", "lint", "verify", "e2e"] {
        assert!(
            job(&jobs, id).body.contains("scripts/ci-install-mold.sh"),
            "job `{id}` links test binaries without mold (.cargo/fast-linker falls back to \
             the slow system linker when it is absent)"
        );
    }
}

/// A scratch directory of fake executables ahead of the real PATH, plus a log
/// every fake appends its argv to. The scripts under test run for real; only the
/// things that would touch the machine (`apt-get`, `curl`, `sudo`) are faked.
struct Fakes {
    dir: PathBuf,
}

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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Like `run`, but PATH is the fakes directory alone, so a tool the script
    /// probes for (`cmake`) exists only when the test put a fake of it there -
    /// the developer's own machine cannot answer for the runner image.
    fn run_isolated(&self, script: &str, envs: &[(&str, &str)]) -> (bool, String, String) {
        let bash = std::env::var("PATH")
            .unwrap_or_default()
            .split(':')
            .map(|d| Path::new(d).join("bash"))
            .find(|p| p.is_file())
            .expect("bash on PATH");
        let out = std::process::Command::new(bash)
            .arg(root().join(script))
            .env("PATH", &self.dir)
            .env("FAKE_LOG", self.dir.join("log"))
            .envs(envs.iter().copied())
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(self.dir.join("log")).unwrap(),
        )
    }

    /// Run `script` with the fakes first on PATH; returns (success, stdout, log).
    fn run(&self, script: &str, envs: &[(&str, &str)]) -> (bool, String, String) {
        let real_path = std::env::var("PATH").unwrap_or_default();
        let out = std::process::Command::new("bash")
            .arg(root().join(script))
            .env("PATH", format!("{}:{real_path}", self.dir.display()))
            .env("FAKE_LOG", self.dir.join("log"))
            .envs(envs.iter().copied())
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(self.dir.join("log")).unwrap(),
        )
    }
}

impl Drop for Fakes {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn system_deps_are_installed_only_when_the_runner_image_lacks_them() {
    let f = Fakes::new("deps");
    // dpkg -s <pkg> succeeds unless the package is listed in $FAKE_MISSING.
    f.fake(
        "dpkg",
        r#"for m in $FAKE_MISSING; do [ "$2" = "$m" ] && exit 1; done; exit 0"#,
    );
    f.fake("sudo", r#"exec "$@""#);
    f.fake("apt-get", r#"echo "apt-get $*" >> "$FAKE_LOG""#);

    // Everything present: no package-index refresh at all.
    let (ok, said, log) = f.run_isolated("scripts/ci-system-deps.sh", &[("FAKE_MISSING", "")]);
    assert!(ok, "{said}");
    assert!(
        log.is_empty(),
        "apt-get must not run when nothing is missing, ran: {log}"
    );
    assert!(said.contains("already present"), "{said}");

    // One package missing: refresh, then install exactly that one.
    let (ok, said, log) = f.run_isolated("scripts/ci-system-deps.sh", &[("FAKE_MISSING", "cmake")]);
    assert!(ok, "{said}");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["apt-get update", "apt-get install -y cmake"],
        "a job that does need a package must still get it"
    );

    // The image's own cmake is on PATH but is no dpkg package (`dpkg -s cmake`
    // fails on every runner): that is not "missing" - installing it refreshed
    // the apt index in every job and added a cmake PATH never used.
    f.fake("cmake", r#"echo "cmake 3.31""#);
    std::fs::write(f.dir.join("log"), "").unwrap(); // the log accumulates across runs
    let (ok, said, log) = f.run_isolated("scripts/ci-system-deps.sh", &[("FAKE_MISSING", "cmake")]);
    assert!(ok, "{said}");
    assert!(
        log.is_empty(),
        "a cmake that resolves on PATH must not trigger apt-get, ran: {log}"
    );
}

#[test]
fn mold_is_installed_once_and_skipped_where_it_has_no_build() {
    let f = Fakes::new("mold");
    f.fake("sudo", r#"exec "$@""#);
    f.fake("curl", r#"echo "curl $*" >> "$FAKE_LOG""#);
    f.fake("uname", r#"echo "$FAKE_ARCH""#);

    // An architecture mold publishes no build for: skipped, nothing downloaded.
    let (ok, said, log) = f.run("scripts/ci-install-mold.sh", &[("FAKE_ARCH", "riscv64")]);
    assert!(ok, "{said}");
    assert!(said.contains("Skipping mold"), "{said}");
    assert!(log.is_empty(), "nothing is downloaded on riscv64: {log}");

    // The pinned version is already on PATH: nothing is downloaded again.
    f.fake("ld.mold", r#"echo "mold 2.40.4 (compatible with GNU ld)""#);
    let (ok, said, log) = f.run("scripts/ci-install-mold.sh", &[("FAKE_ARCH", "x86_64")]);
    assert!(ok, "{said}");
    assert!(said.contains("already installed"), "{said}");
    assert!(
        log.is_empty(),
        "the pinned mold must not be refetched: {log}"
    );

    // A different mold on PATH is not the pinned one: fetch the pinned release.
    f.fake("ld.mold", r#"echo "mold 1.0.0""#);
    f.fake("tar", r#"cat >/dev/null; echo "tar $*" >> "$FAKE_LOG""#);
    f.fake("mold", r#"echo "mold 2.40.4""#);
    let (_, said, log) = f.run("scripts/ci-install-mold.sh", &[("FAKE_ARCH", "x86_64")]);
    assert!(
        log.contains("mold-2.40.4-x86_64-linux.tar.gz"),
        "expected the pinned release to be fetched: {said} {log}"
    );
}

#[test]
fn post_merge_acceptance_starts_from_the_root_cli_cache() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let verify = job(&jobs, "verify");
    assert_eq!(
        value_of(&verify.body, "shared-key").as_deref(),
        Some("test-root-cli"),
        "`verify` builds the same dependency closure as the root-cli shard; without its \
         cache it compiled for 24 min cold"
    );
    assert_eq!(
        value_of(&verify.body, "save-if").as_deref(),
        Some("false"),
        "`verify` only reads root-cli's cache: two jobs saving one key just race"
    );
}

#[test]
fn ci_builds_without_debug_info_to_fit_the_cache_quota() {
    let text = read(".github/workflows/test.yml");
    let before_jobs = code(&text)
        .split("\njobs:")
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(
        before_jobs.contains("CARGO_PROFILE_DEV_DEBUG: 0"),
        "test.yml must build without debug info workflow-wide: it is ~40% of every cached \
         artifact and the repository's cache quota is 10 GB. Over it, GitHub's LRU eviction \
         cold-starts whichever jobs lose (e2e: 3.4 min -> 13.8 min)"
    );
    // The hand-keyed instrumented-target cache is immutable once saved, so a
    // build-setting change needs a new key or the old entry is restored forever.
    assert!(
        code(&text).contains("key: llvm-cov-v2-"),
        "the llvm-cov cache key must be versioned past the build that used debug info"
    );
}

#[test]
fn zc_matrix_cannot_stack_a_three_os_build_per_merge() {
    let text = read(".github/workflows/zc-matrix.yml");
    let body = code(&text);
    assert!(
        body.contains("concurrency:")
            && body.contains("cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}"),
        "zc-matrix.yml needs the same concurrency policy as test.yml: unbounded, it ran 34 \
         three-OS builds in six hours (513 runner-minutes)"
    );
    for j in jobs(&text) {
        assert!(
            j.body.contains("timeout-minutes:"),
            "zc-matrix job `{}` has no timeout",
            j.id
        );
    }
    assert!(
        !body.contains("Swatinem/rust-cache"),
        "a cargo cache on this 3-OS matrix would add ~3 GB to a 10 GB repository quota that \
         test.yml's caches only just fit; weigh that before adding one"
    );
}

#[test]
fn the_ratchet_script_is_unchanged_in_what_it_gates() {
    // The ratchet job was made faster by caching, not by weakening it: it must
    // still run the same script that regenerates and compares the baseline.
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    assert!(job(&jobs, "unsafe-ratchet")
        .body
        .contains(".github/workflows/unsafe-exposure-ratchet.sh"));
    assert!(Path::new(&root().join(".github/workflows/unsafe-exposure-ratchet.sh")).is_file());
}

/// The steps of one job body, each as its own text (a step starts at a line
/// indented six spaces that opens a list item).
fn steps(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        if line.starts_with("      - ") {
            out.push(String::new());
        }
        if let Some(step) = out.last_mut() {
            step.push_str(line);
            step.push('\n');
        }
    }
    out
}

#[test]
fn every_release_job_is_bounded() {
    let text = read(".github/workflows/release.yml");
    let all = jobs(&text);
    assert!(!all.is_empty(), "release.yml has no jobs?");
    for j in all {
        assert!(
            j.body.contains("timeout-minutes:"),
            "release job `{}` has no timeout-minutes: the default is six hours, so a hung \
             build holds the release (and its siblings' published assets) for that long. \
             The slowest leg takes ~31 min",
            j.id
        );
    }
}

#[test]
fn a_manual_dispatch_of_the_release_workflow_publishes_nothing() {
    // Without a tag, softprops/action-gh-release falls back to the ref name, so a
    // dispatch from a branch published a GitHub release named after the branch.
    // Every publishing step (and the retention prune that follows one) must be
    // confined to a pushed tag, which leaves dispatch a build-only dry run.
    let text = read(".github/workflows/release.yml");
    let mut publishing = 0;
    for j in jobs(&text) {
        let prunes = j.body.contains("prune-old-github-releases.sh");
        if prunes {
            publishing += 1;
            assert!(
                j.body.contains("github.event_name == 'push'"),
                "job `{}` prunes published releases on a dispatch that published none",
                j.id
            );
        }
        for step in steps(&j.body) {
            if step.contains("softprops/action-gh-release") {
                publishing += 1;
                assert!(
                    step.contains("if: github.event_name == 'push'"),
                    "job `{}` publishes on workflow_dispatch: a dispatch from a branch would \
                     create a release named after it. Gate the step on a pushed tag:\n{step}",
                    j.id
                );
            }
        }
    }
    assert!(
        publishing >= 3,
        "expected the three publish steps and the prune"
    );
}

#[test]
fn release_bootstrap_runs_through_the_same_scripts_as_test() {
    let text = read(".github/workflows/release.yml");
    let body = code(&text);
    assert!(
        !body.contains("rui314/mold"),
        "release.yml downloads mold inline again: use scripts/ci-install-mold.sh"
    );
    assert!(
        !body.contains("apt-get update && sudo apt-get install -y pkg-config"),
        "release.yml refreshes the apt index unconditionally again: the runner image already \
         carries pkg-config, libssl-dev and cmake; scripts/ci-system-deps.sh installs only \
         what is missing (15-25 s per Linux leg)"
    );
    let jobs = jobs(&text);
    for id in ["build", "build-cuda"] {
        let j = job(&jobs, id);
        assert!(
            j.body.contains("scripts/ci-system-deps.sh")
                && j.body.contains("scripts/ci-install-mold.sh"),
            "release job `{id}` must bootstrap through scripts/ci-system-deps.sh and \
             scripts/ci-install-mold.sh"
        );
    }
}

#[test]
fn the_cuda_toolkit_is_installed_without_its_recommends() {
    let text = read(".github/workflows/release.yml");
    let body = code(&text);
    let line = body
        .lines()
        .find(|l| l.contains("apt-get install") && l.contains("nvidia-cuda-toolkit"))
        .expect("release.yml installs nvidia-cuda-toolkit");
    assert!(
        line.contains("--no-install-recommends"),
        "`{}`: the recommends are Nsight, the profiler, cuda-gdb and Java - 295 of 375 \
         packages and ~1 GB of download (measured on ubuntu:24.04) that no build step uses",
        line.trim()
    );
}

/// The `codetree` stamp exists so a metadata-only merge (`.agents/` churn:
/// task adds, close receipts) skips the ~20-30 min of jobs that only ever
/// prove code. The invariant is the gating itself: every compile-heavy job
/// must `need` the stamp and skip when it is proven - and `verify`, whose
/// attestation queue is already starved by the main concurrency group, must
/// never be gated by it.
#[test]
fn code_proven_stamp_gates_the_expensive_main_jobs() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let stamp = job(&jobs, "codetree");
    assert!(
        stamp.body.contains("scripts/ci-code-proven.sh") && stamp.body.contains("proven=$proven"),
        "the stamp job must resolve proven through scripts/ci-code-proven.sh"
    );
    assert!(
        stamp.body.contains("--status success"),
        "only a concluded-green run is evidence: an in-flight one may still fail"
    );
    for id in ["test", "coverage", "lint", "e2e", "kani"] {
        let j = job(&jobs, id);
        assert!(
            j.body.contains("needs: codetree")
                && j.body.contains("needs.codetree.outputs.proven != 'true'"),
            "job `{id}` proves code only - it must skip when the stamp says the \
             code tree is already green"
        );
    }
    let verify = job(&jobs, "verify");
    assert!(
        !verify.body.contains("needs: codetree"),
        "`verify` attests refs/merged tasks that the queue already starves - \
         gating it on the stamp would let a pending attestation wait forever"
    );
}

/// The script itself: a head whose only difference from a green sha is under
/// .agents/ is proven; one that also touched code is not; an unresolvable sha
/// is not evidence.
#[test]
fn code_proven_script_judges_the_code_diff_not_the_commit() {
    let dir = std::env::temp_dir().join(format!("susi-code-proven-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "--quiet", "-b", "main", "."]);
    std::fs::write(dir.join("a.rs"), "fn f() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "code"]);
    let green = git(&["rev-parse", "HEAD"]);
    // A merge-shaped commit that only moves .agents/ on top of the green sha.
    std::fs::create_dir_all(dir.join(".agents/tasks")).unwrap();
    std::fs::write(dir.join(".agents/tasks/T-1.json"), "{}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "metadata"]);
    let proven_script = root().join("scripts/ci-code-proven.sh");
    let run = |args: &[&str]| {
        std::process::Command::new("bash")
            .arg(&proven_script)
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    let head = git(&["rev-parse", "HEAD"]);
    assert!(
        run(&[&head, &green]).status.success(),
        "a head that differs only in .agents/ is proven"
    );
    std::fs::write(dir.join("b.rs"), "fn g() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "--quiet", "-m", "real change"]);
    let changed = git(&["rev-parse", "HEAD"]);
    assert!(
        !run(&[&changed, &green]).status.success(),
        "a head that also changed code must run the suite"
    );
    assert!(
        !run(&[&head, &"0".repeat(40)]).status.success(),
        "a sha that does not resolve is not evidence"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `verify` builds susi to run `tasks verify-merged`, but most merges leave
/// the attestation queue drained — and the refs answer "anything owed?" in
/// seconds. The toolchain steps must be gated on that answer, never on the
/// codetree stamp (a pending attestation is debt, not code).
#[test]
fn pending_attestations_are_the_only_thing_verify_builds_for() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let verify = job(&jobs, "verify");
    let owed = steps(&verify.body)
        .into_iter()
        .find(|s| s.contains("scripts/ci-verified-pending.sh"))
        .expect("verify must ask the refs before building");
    assert!(
        owed.contains("id: owed"),
        "the refs check must publish `pending` for the steps that gate on it"
    );
    for name in [
        "Install Rust",
        "Cache Cargo",
        "Install Linux System Dependencies",
        "Install mold",
        "Install cargo-nextest",
        "Re-run the acceptance",
    ] {
        let step = steps(&verify.body)
            .into_iter()
            .find(|s| s.contains(&format!("name: {name}")))
            .unwrap_or_else(|| panic!("verify has no `{name}` step"));
        assert!(
            step.contains("steps.owed.outputs.pending == 'true'"),
            "`{name}` must not run when the attestation queue is drained"
        );
    }
}

/// The pending script: merged ids minus verified ids is the queue. A read
/// failure reports "pending" — the only safe direction, since the job then
/// builds and decides for real.
#[test]
fn pending_script_counts_merged_without_verified() {
    let f = Fakes::new("pending");
    f.fake(
        "git",
        r#"case "$*" in
  *"refs/merged/"*) printf 'a1b2 refs/merged/T-1\nc3d4 refs/merged/T-2\n' ;;
  *"refs/verified/"*) printf 'e5f6 refs/verified/T-1\n' ;;
esac"#,
    );
    let (ok, said, _) = f.run("scripts/ci-verified-pending.sh", &[]);
    assert!(ok, "T-2 is merged but unverified: {said}");

    f.fake(
        "git",
        r#"case "$*" in
  *"refs/merged/"*) printf 'a1b2 refs/merged/T-1\n' ;;
  *"refs/verified/"*) printf 'e5f6 refs/verified/T-1\n' ;;
esac"#,
    );
    let (ok, _, _) = f.run("scripts/ci-verified-pending.sh", &[]);
    assert!(!ok, "a drained queue must skip the build");

    f.fake("git", r#"exit 1"#);
    let (ok, _, _) = f.run("scripts/ci-verified-pending.sh", &[]);
    assert!(
        ok,
        "a remote read failure defaults to pending so verify decides for real"
    );
}

/// A .agents/-only push has an empty lint set, and paying for a toolchain
/// first was ~90 s of setup for nothing. Scope resolution must come first
/// and every install must be gated on it.
#[test]
fn pending_gate_resolves_scope_before_installing_a_toolchain() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let check = job(&jobs, "check");
    let steps = steps(&check.body);
    let at = |needle: &str| {
        steps
            .iter()
            .position(|s| s.contains(needle))
            .unwrap_or_else(|| panic!("check has no step containing `{needle}`"))
    };
    let scope = at("Resolve affected crates");
    for name in [
        "Install Rust",
        "Cache Cargo",
        "Install Linux System Dependencies",
        "Install mold",
        "cargo clippy",
    ] {
        let idx = at(&format!("name: {name}"));
        assert!(idx > scope, "`{name}` must come after scope resolution");
        assert!(
            steps[idx].contains("steps.affected.outputs.lintcmd != ''"),
            "`{name}` must be gated on a non-empty lint set"
        );
    }
}

/// The board hygiene check only ever ran in report mode, so the debris it
/// names — dead claim refs and merged remote branches — accumulated forever.
/// The nightly cleanup must actually apply it, with the write permission the
/// ref deletions need and a bound on the schedule.
#[test]
fn cleanup_runs_applies_board_hygiene() {
    let text = read(".github/workflows/cleanup-runs.yml");
    let jobs = jobs(&text);
    let hygiene = job(&jobs, "hygiene");
    assert!(
        hygiene.body.contains("check-board-hygiene.py --apply"),
        "report mode prints the debt; --apply is what removes it"
    );
    assert!(
        hygiene.body.contains("contents: write"),
        "deleting claim refs and merged branches is a push — it needs write"
    );
    assert!(
        hygiene.body.contains("timeout-minutes:"),
        "a hung janitor must not hold the schedule"
    );
}

/// `scripts/ci-test-shards.sh` is the shard -> package map the `codetree` scope
/// resolution filters merges through. If it drifts from the matrix in test.yml,
/// merges get scoped against crates the shard does not run — or a shard is
/// scoped against the wrong packages and skips the leg that would have failed.
#[test]
fn the_shard_map_the_scope_step_uses_matches_the_matrix() {
    let text = read(".github/workflows/test.yml");
    let test_jobs = jobs(&text);
    let test = job(&test_jobs, "test");
    let script = std::process::Command::new("bash")
        .arg(root().join("scripts/ci-test-shards.sh"))
        .output()
        .unwrap();
    assert!(
        script.status.success(),
        "ci-test-shards.sh: {}",
        String::from_utf8_lossy(&script.stderr)
    );
    let script_text = String::from_utf8_lossy(&script.stdout);
    let test_body = test.body.clone();
    let mut seen = 0;
    for line in script_text.lines() {
        let (name, pkgs) = line.split_once('\t').expect("name<TAB>pkgs");
        assert!(
            test_body.contains(&format!("- name: {name}\n            pkgs: {pkgs}")),
            "matrix has no `{name}` shard with exactly `{pkgs}`: {name} in \
             ci-test-shards.sh and the test.yml matrix must name the same \
             crates, or the scope resolution filters on a different set than \
             the shard runs"
        );
        seen += 1;
    }
    assert!(seen >= 6, "expected the real shard list, got {seen} lines");
}

/// `codetree` must publish the scoping verdicts the matrix legs and the e2e
/// gate on — and they must gate on them: a leg that skips late still paid for
/// the runner, and a leg that skips nothing re-runs the suite on every merge.
/// A job-level `if` cannot see matrix variables, which is why the gate is a
/// step every later step checks.
#[test]
fn the_main_suite_scopes_shards_and_e2e_to_the_merge() {
    let text = read(".github/workflows/test.yml");
    let jobs = jobs(&text);
    let stamp = job(&jobs, "codetree");
    for needle in ["affected", "skip_shards", "run_e2e"] {
        assert!(
            stamp.body.contains(&format!("{needle}=")) || stamp.body.contains(&needle),
            "codetree must emit `{needle}` for the scoping gates"
        );
    }
    for id in ["test", "e2e"] {
        let j = job(&jobs, id);
        let gated = steps(&j.body)
            .into_iter()
            .filter(|s| s.contains("if: steps.scope.outputs.run == 'true'"))
            .count();
        assert!(
            gated >= 5,
            "job `{id}`: only {gated} steps are gated on the scope verdict — \
             a skipped leg must exit before checkout, toolchain and cargo"
        );
    }
}

/// The ONNX prefetch builds susi-vendor-fastembed to warm a CDN download; on a
/// push whose affected set cannot reach that crate it is a pure wait. The gate
/// must resolve the intersection, not run it for every crate-touching push.
#[test]
fn the_branch_gate_prefetches_onnx_only_when_the_closure_can_reach_it() {
    let text = read(".github/workflows/test.yml");
    let test_jobs = jobs(&text);
    let check = job(&test_jobs, "check");
    assert!(
        check
            .body
            .contains("ci-reverse-deps.sh susi-vendor-fastembed"),
        "the check job must compute the fastembed reverse-dependency set"
    );
    let prefetch = steps(&check.body)
        .into_iter()
        .find(|s| s.contains("ci-prefetch-onnxruntime.sh"))
        .expect("check must still prefetch when the closure can reach ort");
    assert!(
        prefetch.contains("steps.affected.outputs.onnx == 'true'"),
        "the prefetch must be gated on the resolved intersection:\n{prefetch}"
    );
}
