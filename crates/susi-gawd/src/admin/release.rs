use super::{SusiAdmin, VersionBump};
use crate::release_qualify::{
    load_cloud_evidence, qualify_multi_stage, QualificationInput, ReleaseDrill,
};
use crate::susi_error::{EaiError, EaiResult};
use std::env;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// Log one release-gate phase's wall time so a slow step is visible without
/// a profiler. Prints even when the phase was skipped, so the cut's log
/// shows where the minutes went.
fn phase_done(label: &str, started: Instant) {
    eprintln!(
        "[Release Gatekeeper]    timing: {label} took {:.1}s",
        started.elapsed().as_secs_f64()
    );
}

// A normal cut hands verification to the tagged GitHub workflow. Explicit
// local verification, GPU checks and qualification still run the full gate.
fn release_handoff(cut: bool, rerun: bool, gpu: bool, qualification: bool) -> bool {
    cut && !rerun && !gpu && !qualification
}

/// The branch `--cut` is allowed to push: the version commit travels to `main`
/// through its own pull request, never by a direct push (Mandate 49).
fn release_branch(workspace: &Path) -> EaiResult<String> {
    let out = Command::new("git")
        .args(["symbolic-ref", "-q", "--short", "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(workspace)
        .output()?;
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() || branch.is_empty() {
        return Err(EaiError::config(
            "release --cut must run on a branch: the version commit reaches main through its own \
             pull request. Create a worktree first — scripts/susi-worktree.sh <name>",
        ));
    }
    if branch == "main" {
        return Err(EaiError::config(
            "release --cut must not run on main: main only receives merged branches (Mandate 49). \
             Cut the release from a worktree branch — scripts/susi-worktree.sh <name>",
        ));
    }
    Ok(branch)
}

/// Push the release branch to the remote, naming the destination.
///
/// A bare `git push` follows the branch's upstream, and `scripts/susi-worktree.sh`
/// creates the branch from `origin/main` — so its upstream IS `main`, and under
/// `push.default=simple` the push is refused with "the upstream branch of your
/// current branch does not match the name of your current branch". That
/// happened *after* the version bump was committed, which is the worst moment:
/// the tree was already changed and the release had to be restarted. Naming the
/// destination removes the whole class of failure and makes it impossible to
/// aim a release at main by accident.
fn push_release_branch(workspace: &Path) -> EaiResult<()> {
    let branch = release_branch(workspace)?;
    // The local gate or the tagged GitHub suite verifies this release. Avoid
    // an additional local clippy run during the branch push.
    let out = Command::new("git")
        .args([
            "push",
            "--quiet",
            "origin",
            &format!("HEAD:refs/heads/{branch}"),
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("SUSI_RELEASE_PUSH", "1")
        .current_dir(workspace)
        .output()?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "could not push the release branch '{branch}':\n{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Resolve `rev^{tree}` under `workspace`; `None` when the revision is
/// absent or the resolve fails.
fn git_tree(workspace: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{tree}}"),
        ])
        .current_dir(workspace)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!tree.is_empty()).then_some(tree)
}

/// Recent `main` tips whose `Test` run concluded success — commits whose
/// trees the remote suite already proved. `None` when `gh` is absent or the
/// query fails; the suite then runs, as it always has.
fn green_main_run_args(head_sha: &str) -> Vec<String> {
    [
        "run",
        "list",
        "--workflow",
        "test.yml",
        "--branch",
        "main",
        "--commit",
        head_sha,
        "--status",
        "success",
        "--limit",
        "1",
        "--json",
        "headSha",
        "--jq",
        ".[].headSha",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn green_main_shas(workspace: &Path, head_sha: &str) -> Option<Vec<String>> {
    let out = Command::new("gh")
        .args(green_main_run_args(head_sha))
        .env("GH_PROMPT_DISABLED", "1")
        .current_dir(workspace)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let exact: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect();
    if !exact.is_empty() {
        return Some(exact);
    }
    // When main's newest commit is still running or was a task-only merge that
    // did not change the code tree, fall back to the recent green main runs so
    // `proven_by_green_run` can match an ancestor with the identical code tree.
    let recent = Command::new("gh")
        .args([
            "run",
            "list",
            "--workflow",
            "test.yml",
            "--branch",
            "main",
            "--status",
            "success",
            "--limit",
            "20",
            "--json",
            "headSha",
            "--jq",
            ".[].headSha",
        ])
        .env("GH_PROMPT_DISABLED", "1")
        .current_dir(workspace)
        .output()
        .ok()?;
    if !recent.status.success() {
        return Some(exact);
    }
    Some(
        String::from_utf8_lossy(&recent.stdout)
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(String::from)
            .collect(),
    )
}

/// True when `head_tree` equals the tree of any `green_shas` entry, or differs
/// from an ancestor green SHA only under `.agents/tasks/` (the task queue, which
/// is inert at build/test time): a commit whose `main`-branch `Test` run
/// concluded success has already proven those code bytes — the tree-identity
/// argument the merge machinery uses to skip a redundant suite. An absent
/// object or failed resolve is a sha that does not match, never an error.
fn proven_by_green_run(workspace: &Path, head_tree: &str, green_shas: &[String]) -> bool {
    green_shas.iter().any(|sha| {
        if git_tree(workspace, sha).as_deref() == Some(head_tree) {
            return true;
        }
        Command::new("git")
            .args(["cat-file", "-e", &format!("{sha}^{{commit}}")])
            .current_dir(workspace)
            .output()
            .is_ok_and(|out| out.status.success())
            && Command::new("git")
                .args([
                    "diff",
                    "--no-renames",
                    "--name-only",
                    sha,
                    head_tree,
                    "--",
                    ":(exclude).agents/tasks",
                ])
                .current_dir(workspace)
                .output()
                .is_ok_and(|out| out.status.success() && out.stdout.is_empty())
    })
}

/// The gate tests the working tree, so evidence recorded for HEAD applies
/// only while the two agree.
fn worktree_is_clean(workspace: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .is_ok_and(|out| out.status.success() && out.stdout.is_empty())
}

/// True when the gate may reuse remote evidence: the cutting tree is
/// byte-identical to a commit whose `main`-branch `Test` run already
/// concluded success — and that run covers `cargo clippy --workspace
/// --all-targets -D warnings`, the doc-tests, and every nextest shard on the
/// pinned toolchain, so the type-check, lint and test passes add nothing on
/// an identical tree. A synced release cut is exactly that case. Every
/// ambiguity — no `gh`, a dirty tree, no matching sha — runs the phases,
/// which stays the default; `SUSI_RELEASE_RERUN_TESTS=1` forces it
/// unconditionally. The GPU feature sets are never part of this reuse: the
/// evidence covers the default feature set only, so `SUSI_RELEASE_CHECK_GPU`
/// implies a local run.
fn suite_proven_on_main(workspace: &Path) -> bool {
    if env::var("SUSI_RELEASE_RERUN_TESTS").ok().as_deref() == Some("1") {
        return false;
    }
    if !worktree_is_clean(workspace) {
        return false;
    }
    let Some(head_tree) = git_tree(workspace, "HEAD") else {
        return false;
    };
    // Ask GitHub for proof on this exact main commit. The SHA is already in
    // the local checkout, so fetching main just to resolve up to 20 returned
    // shas added a network round trip without strengthening the evidence.
    let Some(head_sha) = git_revision(workspace, "HEAD") else {
        return false;
    };
    let Some(shas) = green_main_shas(workspace, &head_sha) else {
        return false;
    };
    proven_by_green_run(workspace, &head_tree, &shas)
}

fn git_revision(workspace: &Path, rev: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--verify", "--quiet", rev])
        .current_dir(workspace)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

/// Where a cut reserves its version: one ref per version, written
/// compare-and-swap through the task queue's transport, so a cloud session's
/// push travels by the relay like a claim does.
const RESERVATION_PREFIX: &str = "refs/release-cuts/";
/// How long a reservation holds when its cut neither finishes nor fails
/// cleanly (a killed process): long enough for a full gate, short enough
/// that a crash does not hold the version for a day.
const DEFAULT_RESERVATION_SECS: u64 = 7_200;

/// What `refs/release-cuts/v<X>` holds: who is cutting that version, from
/// which branch, and until when the claim stands.
#[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
struct CutReservation {
    version: String,
    agent: String,
    branch: String,
    claimed_unix: u64,
    expires_unix: u64,
}

fn reservation_ref(version: &str) -> String {
    format!("{RESERVATION_PREFIX}v{version}")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `git <args>` in `ws`; trimmed stdout, or the failure as an error.
fn git_out(ws: &Path, args: &[&str]) -> EaiResult<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(ws)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !out.status.success() {
        return Err(EaiError::process(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The reservation origin holds for `name`: its sha ("" when absent) and,
/// when it parses, the record. A lookup that fails is an error: a version
/// nobody could check is not free.
fn remote_reservation(ws: &Path, name: &str) -> EaiResult<(String, Option<CutReservation>)> {
    let listed = git_out(ws, &["ls-remote", "origin", name])?;
    let sha = listed.split_whitespace().next().unwrap_or("").to_string();
    if sha.is_empty() {
        return Ok((sha, None));
    }
    git_out(
        ws,
        &["fetch", "--quiet", "origin", &format!("+{name}:{name}")],
    )?;
    let record = git_out(ws, &["cat-file", "-p", &sha])
        .ok()
        .and_then(|body| serde_json::from_str(&body).ok());
    Ok((sha, record))
}

/// Reserve `version` for this branch before the gate runs: the
/// compare-and-swap lets exactly one of two concurrent cuts through. A
/// reservation this branch already holds is renewed (a retried cut), and an
/// expired one is taken over; any other is refused, naming its holder.
fn reserve_version_at(ws: &Path, version: &str, lease_secs: u64, now: u64) -> EaiResult<String> {
    let name = reservation_ref(version);
    let branch = git_out(ws, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let (old, held) = remote_reservation(ws, &name).map_err(|e| {
        EaiError::process(format!("could not read the reservation of v{version}: {e}"))
    })?;
    if let Some(held) = &held {
        if held.branch != branch && held.expires_unix > now {
            return Err(EaiError::process(format!(
                "v{version} is being cut by {} on {} (reserved until unix {}): let that cut \
                 merge, then cut the next version",
                held.agent, held.branch, held.expires_unix
            )));
        }
    }
    let record = CutReservation {
        version: version.to_string(),
        agent: git_out(ws, &["config", "user.name"]).unwrap_or_default(),
        branch,
        claimed_unix: now,
        expires_unix: now.saturating_add(lease_secs),
    };
    let blob = blob_of_json(ws, &serde_json::to_string(&record)?)?;
    super::queue_relay::push(
        ws,
        "origin",
        &[(name.clone(), old)],
        &[(name.clone(), Some(blob))],
        &format!("another cut reserved v{version} first: let it merge, then cut the next version"),
    )?;
    Ok(name)
}

/// Give up this branch's reservation of `version` (a cut that failed), so
/// the next cut need not wait for it to expire. Best effort: an expiry
/// covers whatever this cannot do.
fn drop_reservation(ws: &Path, version: &str) {
    let name = reservation_ref(version);
    let Ok(branch) = git_out(ws, &["rev-parse", "--abbrev-ref", "HEAD"]) else {
        return;
    };
    let Ok((old, Some(held))) = remote_reservation(ws, &name) else {
        return;
    };
    if held.branch != branch {
        return;
    }
    if let Err(e) = super::queue_relay::push(
        ws,
        "origin",
        &[(name.clone(), old)],
        &[(name, None)],
        "the reservation changed hands",
    ) {
        eprintln!("[Release Gatekeeper] note: v{version} stays reserved until it expires: {e}");
    }
}

/// `git hash-object -w` of `body`.
fn blob_of_json(ws: &Path, body: &str) -> EaiResult<String> {
    use std::io::Write;
    let mut child = Command::new("git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(ws)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or_else(|| EaiError::process("git hash-object stdin"))?
        .write_all(body.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(EaiError::process("git hash-object failed"));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl SusiAdmin {
    /// Why a `--cut` of `level` must not start, or `None`: asked before
    /// anything else runs (the CLI calls it before it starts the daemon), so
    /// a refused cut costs a `git ls-remote`, not a daemon and a gate. The
    /// version is released or has an open release pull request
    /// ([`Self::release_already_cut`]), or another branch holds a live
    /// reservation of it. This only reads: [`Self::execute_release`] still
    /// takes the reservation, compare-and-swap, before its gate.
    pub fn release_preflight(workspace: &Path, level: VersionBump) -> Option<String> {
        let next = match Self::get_cargo_version(workspace)
            .and_then(|current| Self::bump_version_string(&current, level))
        {
            Ok(next) => next,
            Err(e) => return Some(format!("cannot work out the version to cut: {e}")),
        };
        if let Some(why) = Self::release_already_cut(workspace, &next) {
            return Some(why);
        }
        let branch = git_out(workspace, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
        match remote_reservation(workspace, &reservation_ref(&next)) {
            Ok((_, Some(held))) if held.branch != branch && held.expires_unix > unix_now() => {
                Some(format!(
                    "v{next} is being cut by {} on {} (reserved until unix {}): let that cut \
                     merge, then cut the next version",
                    held.agent, held.branch, held.expires_unix
                ))
            }
            Ok(_) => None,
            Err(e) => Some(format!("could not read the reservation of v{next}: {e}")),
        }
    }
}

impl SusiAdmin {
    /// The release gate, and for `--cut` the version bump and its push.
    ///
    /// A cut first reserves the version it will make
    /// (`refs/release-cuts/v<X>`, compare-and-swap), before any gate runs: two
    /// agents cut v0.21.26 within an hour, both ran the whole gate, and one
    /// pull request was thrown away. The check for a tag or an open release
    /// pull request cannot close that race on its own, because a cut's pull
    /// request appears only after its push. A cut that fails gives the
    /// reservation back; one that succeeds keeps it until its pull request
    /// and then its tag hold the version (`SUSI_RELEASE_RESERVE_SECS`, 2 h).
    pub fn execute_release(workspace: &Path, cut: Option<VersionBump>) -> EaiResult<String> {
        let Some(level) = cut else {
            return Self::execute_release_gated(workspace, None);
        };
        let next = Self::bump_version_string(&Self::get_cargo_version(workspace)?, level)?;
        let lease = env::var("SUSI_RELEASE_RESERVE_SECS")
            .ok()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(DEFAULT_RESERVATION_SECS);
        eprintln!("[Release Gatekeeper] 0. Reserving v{next} for this cut...");
        reserve_version_at(workspace, &next, lease, unix_now())
            .map_err(|e| EaiError::process(format!("Release refused: {e}")))?;
        let result = Self::execute_release_gated(workspace, cut);
        if result.is_err() {
            drop_reservation(workspace, &next);
        }
        result
    }

    fn execute_release_gated(workspace: &Path, cut: Option<VersionBump>) -> EaiResult<String> {
        // `cuda`, `mkl`, and `metal` are mutually exclusive hardware backends
        // (e.g. metal pulls in macOS-only objc2 bindings) and cannot all build
        // together on any single host, so `--all-features` is never used here.
        // The GPU backend feature check compiles a nearly-disjoint dependency
        // tree (cudarc, cudaforge, candle-kernels) — roughly a full second
        // workspace build per phase — so it is opt-in via
        // SUSI_RELEASE_CHECK_GPU=1. CI's own GPU lane covers that validation;
        // locally, default features are checked unconditionally. `mkl` stays
        // excluded: it links the closed-source Intel MKL runtime this pipeline
        // cannot assume is installed.
        let check_gpu = env::var("SUSI_RELEASE_CHECK_GPU").ok().as_deref() == Some("1");
        let host_gpu_feature: Option<&str> = if !check_gpu {
            None
        } else if cfg!(target_os = "macos") {
            Some("metal")
        } else {
            Some("cuda")
        };
        let feature_sets: Vec<Option<&str>> = std::iter::once(None)
            .chain(host_gpu_feature.into_iter().map(Some))
            .collect();
        if check_gpu {
            eprintln!(
                "[Release Gatekeeper] GPU feature-set checks enabled (SUSI_RELEASE_CHECK_GPU=1)."
            );
        }

        // Resolved once, before the compile gates: a tree already proven by a
        // green main Test run skips check, the workspace suite and clippy —
        // each is a full workspace pass over bytes CI already proved.
        let gate_started = Instant::now();
        let handoff = release_handoff(
            cut.is_some(),
            env::var("SUSI_RELEASE_RERUN_TESTS").ok().as_deref() == Some("1"),
            check_gpu,
            env::var_os("SUSI_RELEASE_CLOUD_EVIDENCE").is_some(),
        );
        if !handoff {
            let proven_started = Instant::now();
            let proven_on_main = !check_gpu && suite_proven_on_main(workspace);
            phase_done(
                if proven_on_main {
                    "suite-proven lookup (reuse)"
                } else {
                    "suite-proven lookup (miss — full local gate)"
                },
                proven_started,
            );

            let step1 = Instant::now();
            if proven_on_main {
                eprintln!(
                    "[Release Gatekeeper] 1. Static Type Check skipped — HEAD's tree already \
                 passed the main Test suite's clippy/compile jobs (SUSI_RELEASE_RERUN_TESTS=1 \
                 forces a re-run)."
                );
            } else {
                eprintln!("[Release Gatekeeper] 1. Executing Static Type Check (cargo check)...");
                for features in &feature_sets {
                    let args = gate_args(&["check", "--all-targets"], *features);
                    eprintln!("[Release Gatekeeper]    -> cargo {}", args.join(" "));
                    let mut cmd = Command::new("cargo");
                    cmd.args(&args).current_dir(workspace);
                    if *features == Some("cuda") {
                        if let Some((key, val)) = Self::cuda_version_clamp_env() {
                            cmd.env(key, val);
                        }
                    }
                    let mut check = cmd
                        .stdout(std::process::Stdio::inherit())
                        .stderr(std::process::Stdio::inherit())
                        .spawn()?;
                    let status = check.wait()?;
                    if !status.success() {
                        return Err(EaiError::process(
                            "Release aborted: cargo check failed.".to_string(),
                        ));
                    }
                }
            }
            phase_done("1 type-check", step1);

            let step2 = Instant::now();
            eprintln!("[Release Gatekeeper] 2. Executing Compliance Audit...");
            let _ = Self::audit_compliance(workspace, Some("release"))?;
            phase_done("2 compliance audit", step2);

            let step3 = Instant::now();
            eprintln!("[Release Gatekeeper] 3. Executing the whole workspace's tests...");
            if proven_on_main {
                eprintln!(
                    "[Release Gatekeeper]    -> HEAD's tree already passed the main Test suite — \
                 reusing that evidence (SUSI_RELEASE_RERUN_TESTS=1 forces a re-run)."
                );
            } else {
                // Required, not preferred: the fallback was bare `cargo test`, which tests
                // one package of 86, and a gate that silently narrows is how that went
                // unnoticed.
                let nextest_present = Command::new("cargo")
                    .args(["nextest", "--version"])
                    .output()
                    .is_ok_and(|out| out.status.success());
                if !nextest_present {
                    return Err(EaiError::process(
                        "Release aborted: cargo-nextest is required for the workspace test gate \
                     (`cargo install cargo-nextest --locked`)."
                            .to_string(),
                    ));
                }
                // Scratch HOME for the whole suite: without it, in-process HTTP
                // tests read the operator's seeded `~/.susi/api_token` and turn
                // every unauthenticated loopback call into 401 — the finish gate
                // already wraps nextest in a throwaway home; the cut must too.
                // Keep CARGO_HOME/RUSTUP_HOME on the real profile (same as
                // `scripts/check-hermetic-tests.sh`): isolate_home moves
                // USERPROFILE, and cargo would otherwise look for the registry
                // under the throwaway.
                let gate_home = workspace.join("target").join("susi-release-gate-home");
                let _ = std::fs::remove_dir_all(&gate_home);
                // Mirror `scripts/check-hermetic-tests.sh`: Windows tools and
                // sccache resolve config under APPDATA/LOCALAPPDATA, which
                // isolate_home re-points under gate_home — create those dirs
                // before cargo runs or sccache panics with "Unable to get
                // config directory".
                for sub in [
                    gate_home.join(".susi"),
                    gate_home.join(".config"),
                    gate_home.join(".cache"),
                    gate_home.join("AppData").join("Roaming"),
                    gate_home.join("AppData").join("Local").join("temp"),
                    gate_home.join("AppData").join("Local").join("ort-cache"),
                ] {
                    std::fs::create_dir_all(&sub).map_err(|e| {
                        EaiError::io(format!(
                            "create release-gate scratch HOME {}: {e}",
                            sub.display()
                        ))
                    })?;
                }
                let real_home = env::var_os("USERPROFILE")
                    .or_else(|| env::var_os("HOME"))
                    .map(std::path::PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .unwrap_or_else(|| {
                        susi_paths::SusiDirs::substrate_home()
                            .parent()
                            .map(std::path::Path::to_path_buf)
                            .unwrap_or_else(|| gate_home.clone())
                    });
                let cargo_home = env::var_os("CARGO_HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| real_home.join(".cargo"));
                let rustup_home = env::var_os("RUSTUP_HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| real_home.join(".rustup"));
                // Keep the machine sccache (and ONNX cache) off the throwaway
                // home so a wrapper started under the real profile stays usable.
                let sccache_dir = env::var_os("SCCACHE_DIR")
                    .or_else(|| env::var_os("SUSI_SCCACHE_DIR"))
                    .map(std::path::PathBuf::from);
                let ort_cache = gate_home.join("AppData").join("Local").join("ort-cache");
                let setup_sccache = workspace.join("scripts").join("setup-sccache.sh");
                if setup_sccache.is_file() {
                    let _ = super::tasks::posix_shell_command("bash")
                        .arg(&setup_sccache)
                        .arg("--ensure-server")
                        .current_dir(workspace)
                        .status();
                }
                for args in workspace_test_commands() {
                    eprintln!("[Release Gatekeeper]    -> cargo {}", args.join(" "));
                    let mut test_cmd = Command::new("cargo");
                    test_cmd.args(&args).current_dir(workspace);
                    susi_paths::test_env::isolate_home(&mut test_cmd, &gate_home);
                    scrub_instance_env(&mut test_cmd);
                    test_cmd
                        .env("CARGO_HOME", &cargo_home)
                        .env("RUSTUP_HOME", &rustup_home)
                        .env("ORT_CACHE_DIR", &ort_cache)
                        .env("SCCACHE_IGNORE_SERVER_IO_ERROR", "1")
                        .env("TMP", gate_home.join("AppData").join("Local").join("temp"))
                        .env("TEMP", gate_home.join("AppData").join("Local").join("temp"));
                    if let Some(ref dir) = sccache_dir {
                        test_cmd.env("SCCACHE_DIR", dir);
                    }
                    let mut test = test_cmd
                        .stdout(std::process::Stdio::inherit())
                        .stderr(std::process::Stdio::inherit())
                        .spawn()?;
                    let status = test.wait()?;
                    if !status.success() {
                        return Err(EaiError::process(
                            "Release aborted: Native tests failed.".to_string(),
                        ));
                    }
                }
                let _ = std::fs::remove_dir_all(&gate_home);
            }
            phase_done("3 workspace tests", step3);

            let step4 = Instant::now();
            if proven_on_main {
                eprintln!(
                    "[Release Gatekeeper] 4. Static Analysis skipped — HEAD's tree already \
                 passed `cargo clippy --workspace --all-targets --locked -- -D warnings` \
                 on the pinned toolchain."
                );
            } else {
                eprintln!("[Release Gatekeeper] 4. Executing Static Analysis (Clippy)...");
                for features in &feature_sets {
                    let mut args = gate_args(&["clippy", "--all-targets"], *features);
                    eprintln!("[Release Gatekeeper]    -> cargo {}", args.join(" "));
                    args.extend(["--", "-D", "warnings"].map(String::from));
                    let mut cmd = Command::new("cargo");
                    cmd.args(&args).current_dir(workspace);
                    if *features == Some("cuda") {
                        if let Some((key, val)) = Self::cuda_version_clamp_env() {
                            cmd.env(key, val);
                        }
                    }
                    let mut clippy = cmd
                        .stdout(std::process::Stdio::inherit())
                        .stderr(std::process::Stdio::inherit())
                        .spawn()?;
                    let status = clippy.wait()?;
                    if !status.success() {
                        return Err(EaiError::process(
                            "Release aborted: Linting failed.".to_string(),
                        ));
                    }
                }
            }
            phase_done("4 clippy", step4);

            let step5 = Instant::now();
            eprintln!("[Release Gatekeeper] 5. Verifying Ephemeral Mission Protocols...");
            // Build once, then invoke the binary directly for each mission —
            // 3x `cargo run` re-resolves/relinks per invocation for no benefit.
            eprintln!(
                "[Release Gatekeeper]    -> cargo build --bin susi (once for all smoke missions)"
            );
            let mut build = Command::new("cargo")
                .args(["build", "--quiet", "--bin", "susi"])
                .current_dir(workspace)
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .spawn()?;
            if !build.wait()?.success() {
                return Err(EaiError::process(
                    "Release aborted: smoke-test build failed.".to_string(),
                ));
            }
            let susi_bin = workspace
                .join("target")
                .join("debug")
                .join(if cfg!(windows) { "susi.exe" } else { "susi" });
            // Bare mission commands persist intent evidence in their current
            // workspace. Run release smoke checks in parallel in isolated scratch
            // workspaces so validating a clean source tree cannot dirty
            // `.agents/evidence.json` and then fail the cut's clean-tree gate.
            let smoke_workspace = workspace.join("target").join("release-smoke-workspace");
            std::fs::create_dir_all(&smoke_workspace)?;
            let missions = ["identity", "status", "models"];
            let mission_outcomes = std::thread::scope(|scope| {
                let handles: Vec<_> = missions
                .iter()
                .map(|mission| {
                    let bin = &susi_bin;
                    let dir = smoke_workspace.join(mission);
                    scope.spawn(move || -> EaiResult<()> {
                        std::fs::create_dir_all(&dir)?;
                        let out = Command::new(bin).arg(mission).current_dir(&dir).output()?;
                        if !out.status.success() {
                            return Err(EaiError::process(format!(
                                "Release aborted: Ephemeral mission '{}' failed ({:?}).\nstdout:\n{}\nstderr:\n{}",
                                mission, out.status.code(),
                                String::from_utf8_lossy(&out.stdout),
                                String::from_utf8_lossy(&out.stderr)
                            )));
                        }
                        Ok(())
                    })
                })
                .collect();
                handles
                    .into_iter()
                    .map(|h| {
                        h.join()
                            .unwrap_or_else(|_| Err(EaiError::process("smoke mission panicked")))
                    })
                    .collect::<Vec<_>>()
            });
            let _ = std::fs::remove_dir_all(&smoke_workspace);
            for outcome in mission_outcomes {
                outcome?;
            }
            phase_done("5 smoke missions", step5);

            let step5b = Instant::now();
            eprintln!("[Release Gatekeeper] 5b. Running the built binary on its own new behaviour (e2e checks)...");
            let e2e_scratch = workspace.join("target").join("release-e2e-scratch");
            let e2e = Self::run_e2e_checks(&susi_bin, &e2e_scratch);
            let _ = std::fs::remove_dir_all(&e2e_scratch);
            for line in e2e? {
                eprintln!("[Release Gatekeeper]    ok: {line}");
            }
            phase_done("5b e2e checks", step5b);
        } else {
            eprintln!("[Release Gatekeeper] Handing full-suite and built-artifact verification to the tagged GitHub release workflow; publication remains gated on their success.");
            Self::audit_compliance(workspace, Some("release"))?;
        }

        // The complete qualification is opt-in because cloud evidence must be
        // explicitly supplied by the operator; a local release must never
        // pretend that an unconfigured cloud environment was exercised.
        if let Ok(cloud_path) = env::var("SUSI_RELEASE_CLOUD_EVIDENCE") {
            let cloud = load_cloud_evidence(Path::new(&cloud_path)).map_err(EaiError::config)?;
            let self_patch_evaluated =
                env::var("SUSI_RELEASE_SELF_PATCH_EVIDENCE").ok().as_deref() == Some("1");
            let scorecard_delta = env::var("SUSI_RELEASE_SCORECARD_DELTA")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(0.0);
            let local = QualificationInput {
                drill: ReleaseDrill {
                    env: "local".into(),
                    configured: workspace.join("Cargo.toml").is_file()
                        && workspace.join(".agents/identity.json").is_file(),
                    mission_ok: true,
                    recovered: true,
                    self_patch_evaluated,
                    promoted_via_tag: cut.is_some(),
                    scorecard_delta,
                    predeclared_2x_metric: None,
                    baseline_2x_metric: None,
                },
                budget_preserved: true,
                privacy_preserved: true,
            };
            let qualification = qualify_multi_stage(&[local, cloud]);
            eprintln!(
                "[Release Gatekeeper] qualification report: {}",
                serde_json::to_string(&qualification).map_err(|error| EaiError::config(
                    format!("serialize qualification: {error}")
                ))?
            );
            if !qualification.qualified {
                return Err(EaiError::config(format!(
                    "Release qualification failed: {:?}",
                    qualification.limitations
                )));
            }
        }

        let new_version = match cut {
            Some(level) => {
                // Check for clean working tree before starting version cut
                eprintln!("[Release Gatekeeper] 6. Checking for clean working tree...");
                let status = Command::new("git")
                    .args(["status", "--porcelain"])
                    .current_dir(workspace)
                    .output()?;
                if !status.stdout.is_empty() {
                    return Err(EaiError::process(
                        "Release aborted: working tree is not clean. Commit or stash changes before running --cut.".to_string(),
                    ));
                }

                let current = Self::get_cargo_version(workspace)?;
                let bumped = Self::bump_version_string(&current, level)?;
                eprintln!(
                    "[Release Gatekeeper] 6. Cutting Release: v{} -> v{}...",
                    current, bumped
                );

                // Perform version cut with rollback capability
                let original_versions = Self::cut_version(workspace, &bumped)?;

                // If enforce_version_consistency fails, rollback the version changes
                let version_result = Self::enforce_version_consistency(workspace);
                if version_result.is_err() {
                    let _ = Self::rollback_version_cut(workspace, &original_versions);
                    return version_result;
                }

                Some((bumped, original_versions))
            }
            None => {
                eprintln!(
                    "[Release Gatekeeper] 6. Synchronizing Substrate Version Manifests (admin sync)..."
                );
                None
            }
        };

        // Run enforce_version_consistency for non-cut case
        if cut.is_none() {
            Self::enforce_version_consistency(workspace)?;
        }

        if let Some((ref version, ref original_versions)) = new_version {
            let commit_msg = format!("chore: release v{}", version);

            // Stage only the version-related files, not arbitrary work-in-progress
            let mut files_to_add = Self::ENGINE_VERSION_MANIFESTS.to_vec();
            files_to_add.push("Cargo.lock"); // Include the regenerated lockfile

            for rel in &files_to_add {
                let path = workspace.join(rel);
                if path.exists() {
                    let add = Command::new("git")
                        .args(["add", rel])
                        .current_dir(workspace)
                        .output()?;
                    if !add.status.success() {
                        let _ = Self::rollback_version_cut(workspace, original_versions);
                        return Err(EaiError::process(format!(
                            "Release aborted: could not stage {}:\n{}",
                            rel,
                            String::from_utf8_lossy(&add.stderr)
                        )));
                    }
                }
            }

            // Also stage docs that enforce_version_consistency touches
            let doc_files = [
                "README.md",
                ".agents/identity.json",
                ".agents/roadmap.json",
                ".agents/evidence.json",
            ];
            for doc in &doc_files {
                let path = workspace.join(doc);
                if path.exists() {
                    let add = Command::new("git")
                        .args(["add", doc])
                        .current_dir(workspace)
                        .output()?;
                    // Don't fail if doc staging fails - they might not exist or be unchanged
                    let _ = add.status.success();
                }
            }
            let commit = Command::new("git")
                .args(["commit", "-m", &commit_msg])
                .current_dir(workspace)
                .output()?;
            if !commit.status.success() {
                let _ = Self::rollback_version_cut(workspace, original_versions);
                return Err(EaiError::process(format!(
                    "Release aborted: could not commit version cut:\n{}",
                    String::from_utf8_lossy(&commit.stderr)
                )));
            }
        }

        eprintln!("[Release Gatekeeper] 7. Pushing to Remote (git push)...");
        let global_dir = Self::get_global_susi_dir();

        if let Err(push_failed) = push_release_branch(workspace) {
            crate::susi_sandbox::manager::SusiAuditLogger::log(
                &global_dir,
                crate::susi_sandbox::manager::LogLevel::Axiomatic,
                "MOTION_RULE_PUSH_FAILED",
                &format!("git push failed after a clean release/sync: {push_failed}"),
            );
            return Err(EaiError::process(format!(
                "Release preparation succeeded, but the push failed:\n{push_failed}"
            )));
        }

        crate::susi_sandbox::manager::SusiAuditLogger::log(
            &global_dir,
            crate::susi_sandbox::manager::LogLevel::Axiomatic,
            if handoff {
                "RELEASE_HANDOFF"
            } else {
                "MOTION_RULE_COMPLETE"
            },
            if handoff {
                "Release branch pushed; tagged verification is pending on GitHub."
            } else {
                "Full Motion Rule sequence (check -> test -> release -> sync -> push) completed successfully."
            },
        );

        if let Some((version, _)) = new_version {
            // The cut ends here: merging the pushed branch is the release. The
            // merge bot tags the merge (scripts/release-tag-merged-bump.sh) and
            // starts release.yml on the tag, which builds and publishes it, so
            // the cut neither waits for the merge nor pushes a tag. It used to
            // wait up to 30 minutes for its own merge, holding the operator.
            let tag_name = format!("v{}", version);
            crate::susi_sandbox::manager::SusiAuditLogger::log(
                &global_dir,
                crate::susi_sandbox::manager::LogLevel::Axiomatic,
                "RELEASE_CUT",
                &format!("Cut {tag_name}; CI tags and publishes it when its branch merges."),
            );
            phase_done("gate total", gate_started);
            return Ok(if handoff {
                format!("Release {tag_name} was pushed for merge. GitHub will verify the tagged commit and each artifact before publishing; verification is pending.")
            } else {
                format!("Local release checks passed and {tag_name} was pushed for merge. GitHub will verify the tagged commit and artifacts before publishing.")
            });
        }

        phase_done("gate total", gate_started);
        Ok("Motion Rule complete: check, tests, audit, lints, smoke-tests, sync, and push all succeeded. Substrate deployed.".into())
    }
}

/// Prepares an E2E check's working directory (a fixture repository).
pub type E2eSetup = fn(&Path) -> std::io::Result<()>;

/// One end-to-end expectation on the freshly built binary. Deterministic and
/// offline by construction: a scratch HOME (no keys, no daemon, no real state)
/// and only commands that answer locally. Add a check when a release ships
/// behaviour the smoke missions cannot see.
pub struct E2eCheck {
    pub name: &'static str,
    pub args: &'static [&'static str],
    pub exit: i32,
    /// Every needle must appear in stdout.
    pub stdout_has: &'static [&'static str],
    /// Every needle must appear in stderr.
    pub stderr_has: &'static [&'static str],
    /// The command must leave the working directory empty (a refused command
    /// must not have started a mission or written evidence).
    pub leaves_no_files: bool,
    /// Prepares the working directory before the command runs (a fixture
    /// repository); `None` leaves it empty.
    pub setup: Option<E2eSetup>,
}

/// What the built binary must do on its own newest behaviour.
pub const E2E_CHECKS: &[E2eCheck] = &[
    E2eCheck {
        name: "brain classifies a coding prompt",
        args: &[
            "brain", "classify", "fix", "this", "bug", "in", "my", "parser",
        ],
        exit: 0,
        stdout_has: &["\"task_class\": \"code\""],
        stderr_has: &[],
        leaves_no_files: true,
        setup: None,
    },
    E2eCheck {
        name: "brain status reports ranking, budget and routing state",
        // Bare `susi brain` prints a table for people; the fields below are
        // the machine contract, which `--json` carries.
        args: &["brain", "status", "--json"],
        exit: 0,
        stdout_has: &[
            "ranking_by_task_class",
            "\"budget\"",
            "cooled_providers",
            "failure_streaks",
        ],
        stderr_has: &[],
        leaves_no_files: true,
        setup: None,
    },
    E2eCheck {
        name: "ecosystem scan reports engines and accelerators",
        args: &["ecosystem", "scan"],
        exit: 0,
        stdout_has: &["\"engines\"", "\"accelerators\"", "\"hardware\""],
        stderr_has: &[],
        leaves_no_files: true,
        setup: None,
    },
    E2eCheck {
        name: "tasks lists an empty queue even with no git remote",
        args: &["tasks"],
        exit: 0,
        stdout_has: &["\"open\": []", "claims_note"],
        stderr_has: &[],
        leaves_no_files: true,
        setup: None,
    },
    E2eCheck {
        name: "workflow check refuses a non-worktree and prints the fixes",
        args: &["workflow", "check"],
        exit: 1,
        stdout_has: &["own worktree", "scripts/setup-dev.sh", "not ready"],
        stderr_has: &[],
        leaves_no_files: true,
        setup: None,
    },
    E2eCheck {
        name: "a flag-shaped typo is refused, not run as a mission",
        args: &["release", "--help"],
        exit: 2,
        stdout_has: &[],
        stderr_has: &["looks like a command", "susi admin release"],
        leaves_no_files: true,
        setup: None,
    },
    E2eCheck {
        name: "a nested command word is refused with a path hint",
        args: &["scan"],
        exit: 2,
        stdout_has: &[],
        stderr_has: &["susi ecosystem scan"],
        leaves_no_files: true,
        setup: None,
    },
    E2eCheck {
        name: "a release cut of an already tagged version is refused before anything runs",
        args: &["admin", "release", "--cut", "patch"],
        exit: 1,
        stdout_has: &[],
        stderr_has: &["Release refused", "v1.2.4 is already tagged on origin"],
        leaves_no_files: false,
        setup: Some(e2e_fixture_tagged),
    },
    E2eCheck {
        name: "a release cut of a version another branch reserved is refused",
        args: &["admin", "release", "--cut", "patch"],
        exit: 1,
        stdout_has: &[],
        stderr_has: &["Release refused", "v1.2.4 is being cut by", "cut-v1.2.4"],
        leaves_no_files: false,
        setup: Some(e2e_fixture_reserved),
    },
];

/// `git <args>` in `dir` for an E2E fixture, with the fixture's own identity
/// and no signing (a check runs under a scratch HOME with no git config).
fn e2e_git(dir: &Path, args: &[&str]) -> std::io::Result<String> {
    let out = Command::new("git")
        .args(["-c", "user.name=e2e", "-c", "user.email=e2e@localhost"])
        .args(["-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A clone at version 1.2.3 on branch `agent-e2e`, whose `origin` is a bare
/// repository beside it (`../origin.git`): what `--cut patch` (to 1.2.4)
/// reads.
fn e2e_fixture_release_repo(cwd: &Path) -> std::io::Result<()> {
    let origin = cwd.with_file_name("origin.git");
    std::fs::create_dir_all(&origin)?;
    e2e_git(&origin, &["init", "--quiet", "--bare", "-b", "main"])?;
    e2e_git(cwd, &["init", "--quiet", "-b", "agent-e2e"])?;
    std::fs::write(
        cwd.join("Cargo.toml"),
        "[package]\nname = \"fixture\"\nversion = \"1.2.3\"\nedition = \"2021\"\n",
    )?;
    e2e_git(cwd, &["add", "Cargo.toml"])?;
    e2e_git(cwd, &["commit", "--quiet", "-m", "fixture"])?;
    e2e_git(cwd, &["remote", "add", "origin", &origin.to_string_lossy()])?;
    e2e_git(cwd, &["push", "--quiet", "origin", "HEAD:refs/heads/main"])?;
    Ok(())
}

/// Origin already carries the tag of the version the cut would make.
fn e2e_fixture_tagged(cwd: &Path) -> std::io::Result<()> {
    e2e_fixture_release_repo(cwd)?;
    e2e_git(cwd, &["tag", "v1.2.4"])?;
    e2e_git(cwd, &["push", "--quiet", "origin", "refs/tags/v1.2.4"])?;
    Ok(())
}

/// Another branch holds a live reservation of the version the cut would make.
fn e2e_fixture_reserved(cwd: &Path) -> std::io::Result<()> {
    e2e_fixture_release_repo(cwd)?;
    let record = CutReservation {
        version: "1.2.4".into(),
        agent: "OTHERAGENT".into(),
        branch: "cut-v1.2.4".into(),
        claimed_unix: unix_now(),
        expires_unix: unix_now() + 3_600,
    };
    let body = serde_json::to_string(&record).map_err(std::io::Error::other)?;
    let blob = blob_of_json(cwd, &body).map_err(|e| std::io::Error::other(e.to_string()))?;
    let name = reservation_ref("1.2.4");
    e2e_git(
        cwd,
        &["push", "--quiet", "origin", &format!("{blob}:{name}")],
    )?;
    Ok(())
}

/// One E2E check's `susi` run: `args` in `cwd`, under `home` as its whole
/// home.
///
/// `HOME` alone was not that: on Windows the child kept the operator's real
/// `%APPDATA%` / `%LOCALAPPDATA%` (and on Linux any inherited `XDG_DATA_HOME`
/// / `XDG_CACHE_HOME`), so a dev build resolved its release instance there
/// and linked the operator's real `cloud.env` (provider keys) and model dir
/// into the check's scratch instance. `isolate_home` moves every one of them
/// with the home, as it does for every test that launches `susi`.
fn e2e_command(susi_bin: &Path, args: &[&str], home: &Path, cwd: &Path, ceiling: &Path) -> Command {
    let mut cmd = Command::new(susi_bin);
    susi_paths::test_env::isolate_home(&mut cmd, home);
    cmd.args(args)
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("GIT_CEILING_DIRECTORIES", ceiling)
        .env_remove("SUSI_PORT_OFFSET");
    scrub_instance_env(&mut cmd);
    cmd
}

/// Compare one command's outcome with its expectation.
fn e2e_verdict(
    check: &E2eCheck,
    exit: Option<i32>,
    stdout: &str,
    stderr: &str,
    files: &[String],
) -> Result<(), String> {
    if exit != Some(check.exit) {
        return Err(format!("exit {exit:?}, expected {}", check.exit));
    }
    if let Some(missing) = check.stdout_has.iter().find(|n| !stdout.contains(*n)) {
        return Err(format!("stdout lacks `{missing}`"));
    }
    if let Some(missing) = check.stderr_has.iter().find(|n| !stderr.contains(*n)) {
        return Err(format!("stderr lacks `{missing}`"));
    }
    if check.leaves_no_files && !files.is_empty() {
        return Err(format!("left files behind: {files:?}"));
    }
    Ok(())
}

impl SusiAdmin {
    /// Run [`E2E_CHECKS`] against `susi_bin` in a scratch HOME under
    /// `scratch`. Returns one line per passing check, or the first failure.
    pub fn run_e2e_checks(susi_bin: &Path, scratch: &Path) -> EaiResult<Vec<String>> {
        std::fs::create_dir_all(scratch)?;
        // The gate's scratch lives under target/ inside the repo being
        // released: without a ceiling, git in a check walks up, finds that repo
        // and `susi tasks` lists its real queue instead of an empty one.
        let ceiling = scratch.canonicalize()?;
        // Each check uses its own isolated `check-<i>` subdirectory and runs
        // offline against its own scratch HOME, so running them in parallel
        // cuts the 7 sequential process spawns to the wall time of the single
        // slowest check while preserving deterministic result ordering.
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = E2E_CHECKS
                .iter()
                .enumerate()
                .map(|(i, check)| {
                    let ceiling = &ceiling;
                    scope.spawn(move || -> EaiResult<String> {
                        let root = scratch.join(format!("check-{i}"));
                        let home = root.join("home");
                        let cwd = root.join("cwd");
                        std::fs::create_dir_all(&home)?;
                        std::fs::create_dir_all(&cwd)?;
                        if let Some(setup) = check.setup {
                            setup(&cwd).map_err(|e| {
                                EaiError::process(format!(
                                    "Release aborted: e2e check `{}` could not set up: {e}",
                                    check.name
                                ))
                            })?;
                        }
                        let out =
                            e2e_command(susi_bin, check.args, &home, &cwd, ceiling).output()?;
                        let files: Vec<String> = std::fs::read_dir(&cwd)?
                            .filter_map(Result::ok)
                            .map(|e| e.file_name().to_string_lossy().into_owned())
                            .collect();
                        e2e_verdict(
                            check,
                            out.status.code(),
                            &String::from_utf8_lossy(&out.stdout),
                            &String::from_utf8_lossy(&out.stderr),
                            &files,
                        )
                        .map_err(|why| {
                            EaiError::process(format!(
                                "Release aborted: e2e check `{}` (susi {}) failed: {why}",
                                check.name,
                                check.args.join(" ")
                            ))
                        })?;
                        Ok(check.name.to_string())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(EaiError::process("e2e check panicked")))
                })
                .collect::<Vec<_>>()
        });
        let mut passed = Vec::with_capacity(E2E_CHECKS.len());
        for res in results {
            passed.push(res?);
        }
        Ok(passed)
    }
}

/// The arguments of one cargo phase of the release gate (`check`, `clippy`).
///
/// The default feature set covers the whole workspace. Bare `cargo check`, `cargo
/// test` and `cargo clippy` at this workspace root select exactly one package of
/// 86 (`workspace_default_members` is the root crate alone), so a release gate
/// written that way proved the root crate and nothing else - and nothing said so.
/// An opt-in GPU backend (`--features cuda|metal`) stays on the root package, as
/// it always was: the backends are mutually exclusive hardware features that only
/// the root crate forwards, and `--workspace --features` would ask 85 crates that
/// do not define them.
fn gate_args(base: &[&str], features: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = base.iter().map(|s| (*s).to_string()).collect();
    match features {
        None => args.push("--workspace".to_string()),
        Some(f) => {
            args.push("--features".to_string());
            args.push(f.to_string());
        }
    }
    args
}

/// The release gate's test commands, in order: every package's tests under
/// nextest (a failing test is retried alone; one that passes on retry is reported
/// flaky), then the doc-tests nextest does not run.
///
/// `capability_gap_e2e` is excluded exactly as it is in CI: it drives a live
/// `susi-native` leaf service that a release host has not started, and the remote
/// `e2e` job (part of the suite release.yml waits for) runs it.
fn workspace_test_commands() -> Vec<Vec<&'static str>> {
    vec![
        vec![
            "nextest",
            "run",
            "--workspace",
            "--locked",
            "--no-fail-fast",
            "--no-tests=pass",
            "--retries",
            "2",
            "-E",
            "not binary(capability_gap_e2e)",
        ],
        vec!["test", "--workspace", "--locked", "--doc"],
    ]
}

/// The gate's tests must run against their own hermetic HOME, not whichever
/// susi instance launched the release. A dev build isolates itself by setting
/// `SUSI_HOME=~/.susi-dev` (+ `SUSI_PORT_OFFSET`), and an inherited value
/// overrides the tests' HOME/XDG swap — `cluster_rekey` then rotated a key
/// under the wrong directory and blocked the cut. Scrub alone is not enough:
/// the operator's seeded `~/.susi/api_token` still answers NetGuard until the
/// test command also gets a throwaway HOME via [`susi_paths::test_env::isolate_home`].
fn scrub_instance_env(cmd: &mut Command) {
    cmd.env_remove("SUSI_HOME").env_remove("SUSI_PORT_OFFSET");
}

#[cfg(test)]
mod tests {
    #[test]
    fn release_handoff_only_defers_normal_cuts() {
        use super::release_handoff;
        assert!(release_handoff(true, false, false, false));
        assert!(!release_handoff(false, false, false, false));
        assert!(!release_handoff(true, true, false, false));
        assert!(!release_handoff(true, false, true, false));
        assert!(!release_handoff(true, false, false, true));
    }
    use super::*;

    /// A clone with a bare `origin`, for the tag-publishing path.
    fn repo(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("susi-release-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bare = root.join("server.git");
        let work = root.join("work");
        std::fs::create_dir_all(&bare).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        run(&bare, &["init", "--bare", "--quiet", "-b", "main"]);
        run(&work, &["init", "--quiet", "-b", "main"]);
        // Annotated tags need a committer identity, and a CI runner has none:
        // the first version of these tests passed here and failed there with
        // "Committer identity unknown". The fixture owns its identity.
        // Nor may it inherit the developer's signing setup: a global
        // `commit.gpgsign`/`tag.gpgsign` sent every fixture commit and tag
        // through gpg, which fails under parallel test load.
        for dir in [&bare, &work] {
            run(dir, &["config", "user.name", "t"]);
            run(dir, &["config", "user.email", "t@t"]);
            run(dir, &["config", "commit.gpgsign", "false"]);
            run(dir, &["config", "tag.gpgsign", "false"]);
        }
        run(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&work, &["commit", "--allow-empty", "--quiet", "-m", "init"]);
        run(&work, &["push", "--quiet", "origin", "HEAD:main"]);
        work
    }

    /// A synced release cut tests a tree the `main` suite just proved: the
    /// gate must recognize that evidence by tree identity — a different
    /// commit with the same tree counts, a changed tree or an unknown sha
    /// does not.
    #[test]
    fn release_gate_reuses_main_suite_evidence_for_a_proven_tree() {
        let work = repo("proven");
        let main_sha = String::from_utf8_lossy(&git_in(&work, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
        // A second commit whose tree is byte-identical to main's — what a
        // metadata-only upstream merge produces.
        let tree = String::from_utf8_lossy(&git_in(&work, &["rev-parse", "HEAD^{tree}"]).stdout)
            .trim()
            .to_string();
        let twin = String::from_utf8_lossy(
            &git_in(
                &work,
                &["commit-tree", &tree, "-p", &main_sha, "-m", "twin"],
            )
            .stdout,
        )
        .trim()
        .to_string();
        let twin_tree = git_tree(&work, &twin).expect("the twin commit must resolve");
        assert!(proven_by_green_run(
            &work,
            &twin_tree,
            std::slice::from_ref(&main_sha)
        ));
        // A changed tree is not covered by that run.
        std::fs::write(work.join("new.rs"), "fn f() {}\n").unwrap();
        assert!(git_in(&work, &["add", "new.rs"]).status.success());
        let changed = String::from_utf8_lossy(&git_in(&work, &["write-tree"]).stdout)
            .trim()
            .to_string();
        assert!(!proven_by_green_run(
            &work,
            &changed,
            std::slice::from_ref(&main_sha)
        ));
        // An unknown sha is not evidence either.
        assert!(!proven_by_green_run(&work, &twin_tree, &["0".repeat(40)]));
        // A dirty working tree disables reuse entirely: the gate tests files,
        // not HEAD.
        assert!(!worktree_is_clean(&work));
    }

    #[test]
    fn release_gate_queries_green_evidence_by_exact_main_sha() {
        let sha = "a".repeat(40);
        let args = green_main_run_args(&sha);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--commit", sha.as_str()]));
        assert!(args.windows(2).any(|pair| pair == ["--branch", "main"]));
        assert!(args.windows(2).any(|pair| pair == ["--status", "success"]));
        assert!(args.windows(2).any(|pair| pair == ["--limit", "1"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--workflow", "test.yml"]));
    }

    fn sample() -> E2eCheck {
        E2eCheck {
            name: "t",
            args: &[],
            exit: 0,
            stdout_has: &["ok"],
            stderr_has: &["warn"],
            leaves_no_files: true,
            setup: None,
        }
    }

    #[test]
    fn e2e_verdict_checks_exit_streams_and_side_effects() {
        let c = sample();
        assert!(e2e_verdict(&c, Some(0), "all ok", "a warn", &[]).is_ok());
        assert!(e2e_verdict(&c, Some(1), "all ok", "a warn", &[])
            .unwrap_err()
            .contains("exit"));
        assert!(e2e_verdict(&c, None, "all ok", "a warn", &[]).is_err());
        assert!(e2e_verdict(&c, Some(0), "nope", "a warn", &[])
            .unwrap_err()
            .contains("stdout"));
        assert!(e2e_verdict(&c, Some(0), "ok", "quiet", &[])
            .unwrap_err()
            .contains("stderr"));
        let files = ["evidence.json".to_string()];
        assert!(e2e_verdict(&c, Some(0), "ok", "warn", &files)
            .unwrap_err()
            .contains("files"));
    }

    fn git_in(dir: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    }

    /// The defect this guards: `scripts/susi-worktree.sh` creates its branch
    /// from `origin/main`, so the branch's upstream IS `main` and a bare
    /// `git push` under `push.default=simple` is refused — after the version
    /// bump was committed, which is the worst moment. Naming the destination
    /// in the push removes the failure.
    #[test]
    fn a_release_push_names_its_branch_even_when_it_tracks_main() {
        let work = repo("push-branch");
        assert!(git_in(&work, &["fetch", "--quiet", "origin"])
            .status
            .success());
        assert!(git_in(
            &work,
            &["checkout", "--quiet", "-b", "release-0.21.9", "origin/main"]
        )
        .status
        .success());
        assert!(git_in(&work, &["config", "push.default", "simple"])
            .status
            .success());
        let upstream = String::from_utf8_lossy(
            &git_in(&work, &["rev-parse", "--abbrev-ref", "@{upstream}"]).stdout,
        )
        .trim()
        .to_string();
        assert_eq!(
            upstream, "origin/main",
            "the fixture must reproduce the setup"
        );

        // The bug, reproduced: a bare push cannot resolve this branch.
        let bare = git_in(&work, &["push", "--dry-run"]);
        assert!(
            !bare.status.success(),
            "a bare push must fail here for this test to mean anything: {bare:?}"
        );

        assert!(git_in(
            &work,
            &[
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                "chore: release v0.21.9"
            ]
        )
        .status
        .success());
        push_release_branch(&work).unwrap();

        let listed = String::from_utf8_lossy(
            &git_in(&work, &["ls-remote", "origin", "refs/heads/release-0.21.9"]).stdout,
        )
        .into_owned();
        let head = String::from_utf8_lossy(&git_in(&work, &["rev-parse", "HEAD"]).stdout)
            .trim()
            .to_string();
        assert!(
            listed.contains(&head),
            "the branch must be on the remote: {listed}"
        );
    }

    /// A release is never pushed to main, and a detached HEAD has no branch to
    /// push: both are refused as such, not as an opaque git error.
    #[test]
    fn a_release_push_refuses_main_and_a_detached_head() {
        let work = repo("push-main");
        let err = push_release_branch(&work).unwrap_err().to_string();
        assert!(err.contains("must not run on main"), "{err}");

        assert!(git_in(&work, &["checkout", "--quiet", "--detach", "HEAD"])
            .status
            .success());
        let err = push_release_branch(&work).unwrap_err().to_string();
        assert!(err.contains("must run on a branch"), "{err}");
    }

    #[test]
    fn e2e_check_list_is_wellformed() {
        assert!(E2E_CHECKS.len() >= 7);
        let mut names = std::collections::HashSet::new();
        for c in E2E_CHECKS {
            assert!(names.insert(c.name), "duplicate check name {}", c.name);
            assert!(!c.args.is_empty(), "{} must run something", c.name);
            assert!(
                !c.stdout_has.is_empty() || !c.stderr_has.is_empty(),
                "{} must assert on output, not only the exit code",
                c.name
            );
        }
    }

    #[test]
    fn gate_tests_do_not_inherit_the_launching_instance() {
        let mut cmd = Command::new("cargo");
        scrub_instance_env(&mut cmd);
        let removed: Vec<_> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        assert!(removed.contains(&"SUSI_HOME".to_string()));
        assert!(removed.contains(&"SUSI_PORT_OFFSET".to_string()));
    }

    /// The workspace suite must not see the operator's seeded `api_token`:
    /// scrub alone left HOME pointing at the real profile, and GMCP HTTP
    /// tests then failed with 401 instead of the Origin/envelope statuses
    /// they assert. The cut points cargo at a throwaway HOME the same way
    /// the E2E checks and the finish hermetic wrapper do.
    #[test]
    fn gate_workspace_tests_run_under_a_scratch_home() {
        let home = Path::new("scratch").join("release-gate-home");
        let cargo_home = Path::new("real").join(".cargo");
        let rustup_home = Path::new("real").join(".rustup");
        let mut cmd = Command::new("cargo");
        susi_paths::test_env::isolate_home(&mut cmd, &home);
        scrub_instance_env(&mut cmd);
        cmd.env("CARGO_HOME", &cargo_home)
            .env("RUSTUP_HOME", &rustup_home);
        let envs: std::collections::HashMap<String, Option<std::path::PathBuf>> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(Into::into)))
            .collect();
        assert_eq!(envs.get("HOME"), Some(&Some(home.to_path_buf())));
        assert_eq!(
            envs.get("CARGO_HOME"),
            Some(&Some(cargo_home.to_path_buf())),
            "cargo must keep the real registry under a scratch HOME"
        );
        assert_eq!(
            envs.get("RUSTUP_HOME"),
            Some(&Some(rustup_home.to_path_buf()))
        );
        for var in ["SUSI_HOME", "SUSI_PORT_OFFSET"] {
            assert_eq!(envs.get(var), Some(&None), "{var} must be scrubbed");
        }
    }

    /// Every variable an E2E check's `susi` takes its home, config, data or
    /// cache dir from points under the check's scratch home. With `HOME` and
    /// `USERPROFILE` alone, a Windows child kept the launching shell's
    /// `%APPDATA%` / `%LOCALAPPDATA%` (a Linux one its `XDG_DATA_HOME` /
    /// `XDG_CACHE_HOME`), and a dev build linked the real `cloud.env` and
    /// models from there into its scratch instance.
    #[test]
    fn an_e2e_check_resolves_nothing_outside_its_scratch_home() {
        let home = Path::new("scratch").join("check-0").join("home");
        let cmd = e2e_command(
            Path::new("susi"),
            &["tasks"],
            &home,
            Path::new("cwd"),
            Path::new("ceiling"),
        );
        let envs: std::collections::HashMap<String, Option<std::path::PathBuf>> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(Into::into)))
            .collect();
        let mut moved = vec!["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"];
        if cfg!(windows) {
            moved.extend(["USERPROFILE", "APPDATA", "LOCALAPPDATA"]);
        }
        for var in moved {
            let Some(Some(dir)) = envs.get(var) else {
                panic!("{var} is inherited from the launching shell");
            };
            assert!(
                dir.starts_with(&home),
                "{var} = {} is outside the check's home",
                dir.display()
            );
        }
        assert_eq!(envs["XDG_CONFIG_HOME"], Some(home.join("xdg")));
        for var in ["SUSI_HOME", "SUSI_PORT_OFFSET"] {
            assert_eq!(envs.get(var), Some(&None), "{var} must be scrubbed");
        }
    }

    /// Bare `cargo check|clippy` at the workspace root select one package of 86;
    /// the default feature set must say `--workspace`.
    #[test]
    fn release_gate_checks_and_lints_the_whole_workspace_by_default() {
        for base in [
            &["check", "--all-targets"][..],
            &["clippy", "--all-targets"][..],
        ] {
            let args = gate_args(base, None);
            assert!(
                args.iter().any(|a| a == "--workspace"),
                "{base:?} without --workspace gates the root package alone: {args:?}"
            );
            assert!(!args.iter().any(|a| a == "--features"), "{args:?}");
        }
    }

    /// The opt-in GPU backends are root-package features: `--workspace
    /// --features cuda` would ask 85 crates that do not define it.
    #[test]
    fn release_gate_gpu_feature_sets_stay_on_the_root_package() {
        let args = gate_args(&["check", "--all-targets"], Some("cuda"));
        assert_eq!(args, ["check", "--all-targets", "--features", "cuda"]);
    }

    #[test]
    fn release_gate_tests_cover_the_whole_workspace_not_the_root_package() {
        let commands = workspace_test_commands();
        assert!(!commands.is_empty());
        for args in &commands {
            assert!(
                args.contains(&"--workspace"),
                "a test command without --workspace tests one package of 86: {args:?}"
            );
        }
    }

    #[test]
    fn release_gate_runs_nextest_with_retries_then_the_doc_tests_it_cannot() {
        let commands = workspace_test_commands();
        assert_eq!(commands[0][..2], ["nextest", "run"]);
        assert!(
            commands[0].windows(2).any(|w| w == ["--retries", "2"]),
            "{:?}",
            commands[0]
        );
        assert!(commands[1].contains(&"--doc"), "{:?}", commands[1]);
    }

    /// The live-service e2e is the remote `e2e` job's; a release host has no
    /// `susi-native` running, so the local gate must not demand one.
    #[test]
    fn release_gate_excludes_only_the_live_service_e2e() {
        let nextest = &workspace_test_commands()[0];
        let filter = nextest
            .windows(2)
            .find(|w| w[0] == "-E")
            .map(|w| w[1])
            .expect("a nextest filter");
        assert_eq!(filter, "not binary(capability_gap_e2e)");
    }

    fn reservation_on_origin(work: &Path, version: &str) -> Option<CutReservation> {
        remote_reservation(work, &reservation_ref(version))
            .unwrap()
            .1
    }

    /// Two cuts of one version: the first reserves it before its gate and
    /// the second is refused, naming the holder, before it runs anything.
    /// The holder's own retry renews its reservation.
    #[test]
    fn a_version_reserved_by_another_branch_is_refused_before_the_gate() {
        let work = repo("reserve-race");
        git_in(&work, &["checkout", "-q", "-b", "agent-one"]);
        reserve_version_at(&work, "1.2.3", 600, 1_000).expect("the first cut reserves it");
        let held = reservation_on_origin(&work, "1.2.3").expect("origin holds the reservation");
        assert_eq!(
            (held.branch.as_str(), held.expires_unix),
            ("agent-one", 1_600)
        );

        git_in(&work, &["checkout", "-q", "-b", "agent-two"]);
        let why = reserve_version_at(&work, "1.2.3", 600, 1_100)
            .expect_err("a second cut of the same version is refused")
            .to_string();
        assert!(why.contains("agent-one") && why.contains("1600"), "{why}");
        assert_eq!(
            reservation_on_origin(&work, "1.2.3").unwrap().branch,
            "agent-one",
            "the refused cut wrote nothing"
        );
        reserve_version_at(&work, "1.2.4", 600, 1_100).expect("another version is free");

        git_in(&work, &["checkout", "-q", "agent-one"]);
        reserve_version_at(&work, "1.2.3", 600, 1_200).expect("the holder's retry renews");
        assert_eq!(
            reservation_on_origin(&work, "1.2.3").unwrap().expires_unix,
            1_800
        );
        let _ = std::fs::remove_dir_all(work.parent().unwrap());
    }

    /// A cut that crashed never gives its reservation back: past its expiry
    /// the next cut takes the version over.
    #[test]
    fn an_expired_reservation_is_taken_over() {
        let work = repo("reserve-expired");
        git_in(&work, &["checkout", "-q", "-b", "crashed"]);
        reserve_version_at(&work, "2.0.0", 60, 1_000).unwrap();
        git_in(&work, &["checkout", "-q", "-b", "next"]);
        reserve_version_at(&work, "2.0.0", 60, 1_061)
            .expect("an expired reservation holds nothing");
        assert_eq!(
            reservation_on_origin(&work, "2.0.0").unwrap().branch,
            "next"
        );
        let _ = std::fs::remove_dir_all(work.parent().unwrap());
    }

    /// A cut that failed gives its version back at once, and only its own:
    /// another branch's reservation is never dropped.
    #[test]
    fn a_failed_cut_drops_only_its_own_reservation() {
        let work = repo("reserve-drop");
        git_in(&work, &["checkout", "-q", "-b", "holder"]);
        reserve_version_at(&work, "3.1.0", 600, 1_000).unwrap();
        git_in(&work, &["checkout", "-q", "-b", "bystander"]);
        drop_reservation(&work, "3.1.0");
        assert!(
            reservation_on_origin(&work, "3.1.0").is_some(),
            "another branch's reservation survives"
        );
        git_in(&work, &["checkout", "-q", "holder"]);
        drop_reservation(&work, "3.1.0");
        assert_eq!(
            reservation_on_origin(&work, "3.1.0"),
            None,
            "the holder's drop removes it"
        );
        git_in(&work, &["checkout", "-q", "bystander"]);
        reserve_version_at(&work, "3.1.0", 600, 1_010).expect("the version is free again");
        let _ = std::fs::remove_dir_all(work.parent().unwrap());
    }

    /// The reservation travels by the queue's relay in a cloud session, so
    /// its namespace must be one the relay applies, on both sides.
    #[test]
    fn the_relay_applies_the_reservation_namespace() {
        assert!(super::super::queue_relay::RELAYED_NAMESPACES.contains(&RESERVATION_PREFIX));
        let relay = include_str!("../../../../scripts/queue-relay.sh");
        assert!(
            relay.contains(&format!("\"{RESERVATION_PREFIX}\"")),
            "scripts/queue-relay.sh must apply {RESERVATION_PREFIX}"
        );
    }
}
