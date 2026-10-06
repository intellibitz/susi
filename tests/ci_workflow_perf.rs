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
    let (ok, said, log) = f.run("scripts/ci-system-deps.sh", &[("FAKE_MISSING", "")]);
    assert!(ok, "{said}");
    assert!(
        log.is_empty(),
        "apt-get must not run when nothing is missing, ran: {log}"
    );
    assert!(said.contains("already present"), "{said}");

    // One package missing: refresh, then install exactly that one.
    let (ok, said, log) = f.run("scripts/ci-system-deps.sh", &[("FAKE_MISSING", "cmake")]);
    assert!(ok, "{said}");
    assert_eq!(
        log.lines().collect::<Vec<_>>(),
        ["apt-get update", "apt-get install -y cmake"],
        "a job that does need a package must still get it"
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
