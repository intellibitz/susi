use super::{SusiAdmin, VersionBump};
use crate::susi_error::{EaiError, EaiResult};
use std::env;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// What happened to the release tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagPublish {
    /// Pushed: its commit is on `origin/main`, so CI will build and publish it.
    Published,
    /// Created locally only: the merge has not landed, so publishing it now
    /// would be refused by CI and could never be repaired.
    Deferred,
}

/// Seconds to wait for the release branch's own pull request to merge before
/// the tag is published (`SUSI_RELEASE_MERGE_WAIT`, default 30 minutes).
fn merge_wait_budget() -> Duration {
    env::var("SUSI_RELEASE_MERGE_WAIT")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map_or(Duration::from_secs(1800), Duration::from_secs)
}

/// Is `sha` contained in `origin/main` yet? Fetches first: the branch is
/// pushed, not merged, at this point in the cut.
fn await_merged(workspace: &Path, sha: &str, budget: Duration) -> bool {
    let started = Instant::now();
    loop {
        let _ = Command::new("git")
            .args(["fetch", "--quiet", "origin"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(workspace)
            .output();
        let contained = Command::new("git")
            .args(["merge-base", "--is-ancestor", sha, "origin/main"])
            .current_dir(workspace)
            .output()
            .is_ok_and(|out| out.status.success());
        if contained {
            return true;
        }
        if started.elapsed() >= budget {
            return false;
        }
        std::thread::sleep(Duration::from_secs(15));
    }
}

/// Tag the cut release and publish it once its commit is on `origin/main`.
///
/// CI refuses — before any build job starts — a tag whose commit is not an
/// ancestor of `origin/main` (`scripts/check-release-tag.sh`), and the tag
/// ruleset forbids moving a published tag, so a tag pushed while its branch is
/// only *pushed* can never be repaired: the release would have to be cut again
/// under a new version. `--cut` pushes the branch, not the merge, so this waits
/// for the merge the loop's own automation performs, and leaves the tag local —
/// never published, never broken — when it does not arrive in time.
fn publish_release_tag(workspace: &Path, tag: &str, budget: Duration) -> EaiResult<TagPublish> {
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .ok_or_else(|| EaiError::process("cannot resolve HEAD to tag the release"))?;

    let exists = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/tags/{tag}"),
        ])
        .current_dir(workspace)
        .output()
        .is_ok_and(|out| out.status.success());
    if !exists {
        let tagged = Command::new("git")
            .args(["tag", "-a", tag, "-m", &format!("Release {tag}")])
            .current_dir(workspace)
            .output()?;
        if !tagged.status.success() {
            return Err(EaiError::process(format!(
                "creating tag {tag} failed: {}",
                String::from_utf8_lossy(&tagged.stderr)
            )));
        }
    }

    if !await_merged(workspace, &head, budget) {
        return Ok(TagPublish::Deferred);
    }

    let pushed = Command::new("git")
        .args(["push", "origin", tag])
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(workspace)
        .output()?;
    if !pushed.status.success() {
        return Err(EaiError::process(format!(
            "tag {tag} was created but pushing it failed (release.yml only triggers on a pushed \
             tag — push it manually with `git push origin {tag}`):\n{}",
            String::from_utf8_lossy(&pushed.stderr)
        )));
    }
    Ok(TagPublish::Published)
}

impl SusiAdmin {
    pub fn execute_release(workspace: &Path, cut: Option<VersionBump>) -> EaiResult<String> {
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

        eprintln!("[Release Gatekeeper] 1. Executing Static Type Check (cargo check)...");
        for features in &feature_sets {
            let mut args = vec!["check", "--all-targets"];
            if let Some(f) = features {
                eprintln!("[Release Gatekeeper]    -> cargo check --features {}", f);
                args.push("--features");
                args.push(f);
            } else {
                eprintln!("[Release Gatekeeper]    -> cargo check (default features)");
            }
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

        eprintln!("[Release Gatekeeper] 2. Executing Compliance Audit...");
        let _ = Self::audit_compliance(workspace, Some("release"))?;

        eprintln!("[Release Gatekeeper] 3. Executing Native Test Harness...");
        let mut test_cmd = Command::new("cargo");
        test_cmd.arg("test").current_dir(workspace);
        scrub_instance_env(&mut test_cmd);
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

        eprintln!("[Release Gatekeeper] 4. Executing Static Analysis (Clippy)...");
        for features in &feature_sets {
            let mut args = vec!["clippy", "--all-targets"];
            if let Some(f) = features {
                eprintln!("[Release Gatekeeper]    -> cargo clippy --features {}", f);
                args.push("--features");
                args.push(f);
            } else {
                eprintln!("[Release Gatekeeper]    -> cargo clippy (default features)");
            }
            args.extend(["--", "-D", "warnings"]);
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

        eprintln!("[Release Gatekeeper] 5. Verifying Ephemeral Mission Protocols...");
        // Build once, then invoke the binary directly for each mission —
        // 3x `cargo run` re-resolves/relinks per invocation for no benefit.
        eprintln!("[Release Gatekeeper]    -> cargo build (once for all smoke missions)");
        let mut build = Command::new("cargo")
            .args(["build", "--quiet"])
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
        // workspace. Run release smoke checks in an ignored scratch workspace
        // so validating a clean source tree cannot dirty `.agents/evidence.json`
        // and then fail the cut's clean-tree gate.
        let smoke_workspace = workspace.join("target").join("release-smoke-workspace");
        std::fs::create_dir_all(&smoke_workspace)?;
        let missions = ["identity", "status", "models"];
        for mission in missions {
            let mut mission_out = Command::new(&susi_bin)
                .arg(mission)
                .current_dir(&smoke_workspace)
                .stdout(std::process::Stdio::inherit())
                .stderr(std::process::Stdio::inherit())
                .spawn()?;
            let status = mission_out.wait()?;
            if !status.success() {
                return Err(EaiError::process(format!(
                    "Release aborted: Ephemeral mission '{}' failed.",
                    mission
                )));
            }
        }
        let _ = std::fs::remove_dir_all(&smoke_workspace);

        eprintln!("[Release Gatekeeper] 5b. Running the built binary on its own new behaviour (e2e checks)...");
        let e2e_scratch = workspace.join("target").join("release-e2e-scratch");
        let e2e = Self::run_e2e_checks(&susi_bin, &e2e_scratch);
        let _ = std::fs::remove_dir_all(&e2e_scratch);
        for line in e2e? {
            eprintln!("[Release Gatekeeper]    ok: {line}");
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
        let push = Command::new("git")
            .arg("push")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(workspace)
            .output()?;

        let global_dir = Self::get_global_susi_dir();

        if !push.status.success() {
            let stderr = String::from_utf8_lossy(&push.stderr);
            crate::susi_sandbox::manager::SusiAuditLogger::log(
                &global_dir,
                crate::susi_sandbox::manager::LogLevel::Axiomatic,
                "MOTION_RULE_PUSH_FAILED",
                &format!("git push failed after a clean release/sync: {}", stderr),
            );
            return Err(EaiError::process(format!(
                "Release, tests, and sync succeeded, but git push failed:\n{}",
                stderr
            )));
        }

        crate::susi_sandbox::manager::SusiAuditLogger::log(
            &global_dir,
            crate::susi_sandbox::manager::LogLevel::Axiomatic,
            "MOTION_RULE_COMPLETE",
            "Full Motion Rule sequence (check -> test -> release -> sync -> push) completed successfully.",
        );

        if let Some((version, _)) = new_version {
            let tag_name = format!("v{}", version);
            let budget = merge_wait_budget();
            eprintln!(
                "[Release Gatekeeper] 8. Tagging Release ({}) — publishing once its commit is on \
                 origin/main (waiting up to {}s)...",
                tag_name,
                budget.as_secs()
            );
            match publish_release_tag(workspace, &tag_name, budget)? {
                TagPublish::Published => {
                    crate::susi_sandbox::manager::SusiAuditLogger::log(
                        &global_dir,
                        crate::susi_sandbox::manager::LogLevel::Axiomatic,
                        "RELEASE_CUT",
                        &format!("Cut and pushed release {}.", tag_name),
                    );
                    return Ok(format!(
                        "Motion Rule complete: check, tests, audit, lints, smoke-tests, sync, push, \
                         and tag all succeeded. Release {} is live - release.yml will build and \
                         publish its binaries now.\n{}",
                        tag_name,
                        Self::promote_release_locally()
                    ));
                }
                TagPublish::Deferred => {
                    crate::susi_sandbox::manager::SusiAuditLogger::log(
                        &global_dir,
                        crate::susi_sandbox::manager::LogLevel::Axiomatic,
                        "RELEASE_CUT_DEFERRED",
                        &format!("Cut {tag_name}; tag not published (commit not on origin/main)."),
                    );
                    // Publishing here would hand CI a tag it refuses before
                    // building anything, and the tag ruleset forbids moving a
                    // published tag, so the release would need another version.
                    // Leaving it local keeps the cut recoverable with one push.
                    return Err(EaiError::process(format!(
                        "Release {tag_name} is cut, tested and pushed, and the tag exists locally, \
                         but it is NOT published: its commit did not reach origin/main within {}s, \
                         and CI refuses a tag whose commit main does not contain (it checks before \
                         building, and a published tag cannot be moved). Merge the release branch, \
                         then publish it: git push origin {tag_name}",
                        budget.as_secs()
                    )));
                }
            }
        }

        Ok("Motion Rule complete: check, tests, audit, lints, smoke-tests, sync, and push all succeeded. Substrate deployed.".into())
    }

    /// Kicks the host's release-sync unit so the release just cut replaces
    /// the local susi without waiting for the hourly timer. Non-blocking:
    /// the unit builds the tag in its own checkout and swaps the binary only
    /// once this `susi release` process (a busy client) has exited.
    fn promote_release_locally() -> String {
        const UNIT: &str = "susi-release-sync.service";
        let started = Command::new("systemctl")
            .args(["--user", "start", "--no-block", UNIT])
            .output()
            .is_ok_and(|out| out.status.success());
        if started {
            format!(
                "Local promotion queued ({UNIT}): the release is built for this host and \
                 installed once this command exits. Follow: journalctl --user -fu {UNIT}"
            )
        } else {
            "Local susi not updated: the release-sync timer is not installed. Run \
             scripts/susi-release-sync.sh (once) or --install-timer (always)."
                .to_string()
        }
    }
}

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
    },
    E2eCheck {
        name: "brain status reports ranking, budget and routing state",
        args: &["brain"],
        exit: 0,
        stdout_has: &[
            "ranking_by_task_class",
            "\"budget\"",
            "cooled_providers",
            "failure_streaks",
        ],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "ecosystem scan reports engines and accelerators",
        args: &["ecosystem", "scan"],
        exit: 0,
        stdout_has: &["\"engines\"", "\"accelerators\"", "\"hardware\""],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "tasks lists an empty queue even with no git remote",
        args: &["tasks"],
        exit: 0,
        stdout_has: &["\"open\": []", "claims_note"],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "workflow check refuses a non-worktree and prints the fixes",
        args: &["workflow", "check"],
        exit: 1,
        stdout_has: &["own worktree", "scripts/setup-dev.sh", "not ready"],
        stderr_has: &[],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "a flag-shaped typo is refused, not run as a mission",
        args: &["release", "--help"],
        exit: 2,
        stdout_has: &[],
        stderr_has: &["looks like a command", "susi admin release"],
        leaves_no_files: true,
    },
    E2eCheck {
        name: "a nested command word is refused with a path hint",
        args: &["scan"],
        exit: 2,
        stdout_has: &[],
        stderr_has: &["susi ecosystem scan"],
        leaves_no_files: true,
    },
];

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
        let mut passed = Vec::new();
        for (i, check) in E2E_CHECKS.iter().enumerate() {
            let root = scratch.join(format!("check-{i}"));
            let home = root.join("home");
            let cwd = root.join("cwd");
            std::fs::create_dir_all(&home)?;
            std::fs::create_dir_all(&cwd)?;
            let mut cmd = Command::new(susi_bin);
            cmd.args(check.args)
                .current_dir(&cwd)
                .env("HOME", &home)
                .env("USERPROFILE", &home)
                .env("XDG_CONFIG_HOME", home.join("xdg"))
                .env("GIT_CEILING_DIRECTORIES", &ceiling)
                .env_remove("SUSI_PORT_OFFSET");
            scrub_instance_env(&mut cmd);
            let out = cmd.output()?;
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
            passed.push(check.name.to_string());
        }
        Ok(passed)
    }
}

/// The gate's tests must run against their own hermetic HOME, not whichever
/// susi instance launched the release. A dev build isolates itself by setting
/// `SUSI_HOME=~/.susi-dev` (+ `SUSI_PORT_OFFSET`), and an inherited value
/// overrides the tests' HOME/XDG swap — `cluster_rekey` then rotated a key
/// under the wrong directory and blocked the cut.
fn scrub_instance_env(cmd: &mut Command) {
    cmd.env_remove("SUSI_HOME").env_remove("SUSI_PORT_OFFSET");
}

#[cfg(test)]
mod tests {
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
        run(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        run(&work, &["commit", "--allow-empty", "--quiet", "-m", "init"]);
        run(&work, &["push", "--quiet", "origin", "HEAD:main"]);
        work
    }

    fn remote_tags(work: &Path) -> String {
        let out = Command::new("git")
            .args(["ls-remote", "--tags", "origin"])
            .current_dir(work)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn commit_on_branch(work: &Path) -> String {
        let out = Command::new("git")
            .args(["checkout", "--quiet", "-b", "release"])
            .current_dir(work)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let out = Command::new("git")
            .args([
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                "chore: release v9.9.9",
            ])
            .current_dir(work)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let out = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(work)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The defect this guards: `--cut` pushed the tag immediately, while its
    /// commit was on a branch main did not contain yet, so CI refused it before
    /// building anything and — because a published tag may not be moved — the
    /// only repair was another version. A release that has not merged must
    /// therefore publish NO tag at all.
    #[test]
    fn an_unmerged_release_publishes_no_tag() {
        let work = repo("defer");
        let sha = commit_on_branch(&work);
        let outcome = publish_release_tag(&work, "v9.9.9", Duration::from_secs(0)).unwrap();
        assert_eq!(outcome, TagPublish::Deferred);
        assert_eq!(
            remote_tags(&work),
            "",
            "no tag may be published before its commit is on main"
        );
        // The tag exists locally, so the cut is recoverable with one push.
        // `^{commit}`: release tags are annotated, so the ref names a tag object.
        let local = Command::new("git")
            .args([
                "rev-parse",
                "--verify",
                "--quiet",
                "refs/tags/v9.9.9^{commit}",
            ])
            .current_dir(&work)
            .output()
            .unwrap();
        assert!(local.status.success(), "the tag must be created locally");
        assert_eq!(
            String::from_utf8_lossy(&local.stdout).trim(),
            sha,
            "the local tag must name the cut commit"
        );
    }

    #[test]
    fn a_merged_release_is_published() {
        let work = repo("publish");
        let sha = commit_on_branch(&work);
        let push = Command::new("git")
            .args(["push", "--quiet", "origin", "HEAD:main"])
            .current_dir(&work)
            .output()
            .unwrap();
        assert!(push.status.success(), "{push:?}");
        let outcome = publish_release_tag(&work, "v9.9.9", Duration::from_secs(0)).unwrap();
        assert_eq!(outcome, TagPublish::Published);
        assert!(
            remote_tags(&work).contains("refs/tags/v9.9.9"),
            "{}",
            remote_tags(&work)
        );
        let _ = sha;
    }

    #[test]
    fn await_merged_returns_true_once_the_commit_lands() {
        let work = repo("await");
        let sha = commit_on_branch(&work);
        // A merge arriving while the wait is in flight, as auto-merge does.
        let from_thread = work.clone();
        let merge = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let out = Command::new("git")
                .args(["push", "--quiet", "origin", "HEAD:main"])
                .current_dir(&from_thread)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        });
        assert!(
            await_merged(&work, &sha, Duration::from_secs(30)),
            "a merge that lands inside the budget must publish the tag"
        );
        merge.join().unwrap();
    }

    #[test]
    fn await_merged_gives_up_when_the_merge_never_comes() {
        let work = repo("timeout");
        let sha = commit_on_branch(&work);
        assert!(!await_merged(&work, &sha, Duration::from_secs(0)));
    }

    fn sample() -> E2eCheck {
        E2eCheck {
            name: "t",
            args: &[],
            exit: 0,
            stdout_has: &["ok"],
            stderr_has: &["warn"],
            leaves_no_files: true,
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
}
