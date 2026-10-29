#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! What the release workflow may publish.
//!
//! A release is the one artifact the world downloads, and nothing goes red
//! when it ships the wrong bytes: a binary that failed its own check, a binary
//! built by a cancelled run, a binary that names the wrong version. Each of
//! these was possible in `.github/workflows/release.yml`; the workflow is read
//! as text, as `ci_workflow_perf.rs` reads the others.

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
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
        .unwrap_or_else(|| panic!("no job `{id}` in release.yml"))
}

/// The value of the first `key:` line in `body`, unquoted.
fn value_of(body: &str, key: &str) -> Option<String> {
    body.lines().find_map(|l| {
        l.trim_start()
            .strip_prefix(&format!("{key}:"))
            .map(|v| v.trim().trim_matches(|c| c == '"' || c == '\'').to_string())
    })
}

/// The steps of one job body, each with its own lines.
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

/// The harness ships only what passed its own offline check. Upload Harness
/// used to gate on Build Harness alone, under `always()`, which drops the
/// implicit `success()`: a binary that failed the check was still attached to
/// the release. `always()` also holds after a cancel or the job timeout, so a
/// cancelled run still built the harness and could publish a release holding
/// nothing else. And the check, unlike the build, was required on the arm64 and
/// macOS legs, so an optional leg's harness could fail the release.
#[test]
fn the_harness_ships_only_a_binary_that_passed_its_own_check() {
    let text = read(".github/workflows/release.yml");
    let jobs = jobs(&text);
    let build = job(&jobs, "build");
    let steps = steps(&build.body);
    let step = |name: &str| -> &String {
        steps
            .iter()
            .find(|s| s.contains(&format!("- name: {name}\n")))
            .unwrap_or_else(|| panic!("release.yml `build` has no step `{name}`"))
    };
    let condition = |s: &str| -> String {
        s.lines()
            .find_map(|l| l.trim_start().strip_prefix("if: "))
            .unwrap_or_default()
            .to_string()
    };
    let built = step("Build Harness");
    let verify = step("Verify the harness artifact answers offline");
    let upload = step("Upload Harness");
    for (name, s) in [("Build", built), ("Verify", verify), ("Upload", upload)] {
        let gate = condition(s);
        assert!(
            !gate.contains("always()") && gate.contains("!cancelled()"),
            "{name} Harness must run despite a failed engine but not after a cancel: \
             `!cancelled()`, not `always()` (`{gate}`)"
        );
    }
    let soft = "continue-on-error: ${{ matrix.name != 'linux-x86_64' && matrix.name != 'windows-x86_64' }}";
    for (name, s) in [("Build Harness", built), ("the harness check", verify)] {
        assert!(
            s.contains(soft),
            "{name} must be required on linux-x86_64 and windows-x86_64 only, like the engine"
        );
    }
    let id = value_of(verify, "id").expect("the harness check needs an id for the upload to read");
    let gate = condition(upload);
    assert!(
        gate.starts_with("github.event_name == 'push' && "),
        "a dispatch is a dry run and must publish nothing: `{gate}`"
    );
    assert!(
        gate.contains(&format!("steps.{id}.outcome == 'success'")),
        "Upload Harness must publish only a binary that passed `{id}` - its outcome, which a \
         soft leg's continue-on-error does not turn green (`{gate}`)"
    );
    assert!(
        !verify
            .lines()
            .any(|l| l.contains("\"$bin\"") && l.contains("| grep")),
        "the check pipes the harness into grep: under pipefail a grep that matches and exits \
         first kills the binary's last write with EPIPE and fails the step"
    );
    assert!(
        verify.contains("reported=$(\"$bin\" version)")
            && verify.contains("[ \"$reported\" != \"susi-harness $release\" ]"),
        "the check must compare the version the harness reports with the release's"
    );
}

/// The Windows binary is checked from a scratch directory. Run from the
/// checkout, `susi tasks` read the repository's own queue where the check
/// expects an empty one, so every release from v0.21.21 to v0.21.23 failed
/// its required Windows leg and published no Windows binary.
#[test]
fn the_windows_binary_is_checked_outside_the_checkout() {
    let text = read(".github/workflows/release.yml");
    let jobs = jobs(&text);
    let build = job(&jobs, "build");
    let steps = steps(&build.body);
    let verify = steps
        .iter()
        .find(|s| s.contains("- name: Verify the Windows artifact answers offline\n"))
        .expect("release.yml `build` checks the Windows artifact");
    let moved = verify
        .find("Set-Location $home_")
        .expect("the check runs from its scratch home");
    let first_check = verify.find("Check '").expect("the step runs its checks");
    assert!(
        moved < first_check,
        "the check must leave the checkout before its first command"
    );
}

/// Every binary the release publishes is cut with the release's version. The
/// harness ships as its own asset, but the cut bumped only the engine's
/// manifests, so a harness built for v0.21.23 said `susi-harness 0.21.20` - in
/// `version`, the zero-config banner and the MCP clientInfo - and would have
/// said so in every release after. `susi admin release --cut` writes the new
/// version into every manifest on ENGINE_VERSION_MANIFESTS, whatever it held,
/// and the release workflow's own check above refuses a binary that reports
/// another version.
#[test]
fn every_binary_the_release_publishes_is_bumped_by_the_cut() {
    let release = read(".github/workflows/release.yml");
    let published: Vec<String> = code(&release)
        .lines()
        .filter_map(|l| {
            l.split_once("cargo build --profile dist -p ")
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .map(|krate| format!("crates/{krate}/Cargo.toml"))
        })
        .collect();
    assert!(
        published.contains(&"crates/susi-harness/Cargo.toml".to_string()),
        "release.yml no longer builds the harness by package? {published:?}"
    );
    let version_rs = read("crates/susi-gawd/src/admin/version.rs");
    let list = version_rs
        .split_once("ENGINE_VERSION_MANIFESTS")
        .and_then(|(_, rest)| rest.split_once("];"))
        .map(|(list, _)| list)
        .expect("version.rs declares ENGINE_VERSION_MANIFESTS");
    let package_version = |rel: &str| -> String {
        let mut in_package = false;
        for line in read(rel).lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_package = line == "[package]";
            } else if in_package {
                if let Some(v) = line.strip_prefix("version = \"") {
                    return v.trim_end_matches('"').to_string();
                }
            }
        }
        panic!("{rel} has no [package] version")
    };
    let root = package_version("Cargo.toml");
    for manifest in &published {
        assert!(
            list.contains(&format!("\"{manifest}\"")),
            "release.yml publishes the binary of {manifest}, but `susi admin release --cut` \
             does not bump it: add it to ENGINE_VERSION_MANIFESTS"
        );
        // Between cuts too: a dry run of the release workflow runs the same
        // version check, and a manifest one version behind failed both
        // required legs of every dry run until the next cut.
        assert_eq!(
            package_version(manifest),
            root,
            "{manifest} must carry the release version its binary is published under"
        );
    }
}
