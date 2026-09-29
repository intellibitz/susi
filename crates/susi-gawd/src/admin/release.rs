use super::{SusiAdmin, VersionBump};
use crate::susi_error::{EaiError, EaiResult};
use std::env;
use std::path::Path;
use std::process::Command;

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
            eprintln!("[Release Gatekeeper] 8. Tagging Release (v{})...", version);
            let tag_name = format!("v{}", version);
            let tag = Command::new("git")
                .args([
                    "tag",
                    "-a",
                    &tag_name,
                    "-m",
                    &format!("Release {}", tag_name),
                ])
                .current_dir(workspace)
                .output()?;
            if !tag.status.success() {
                return Err(EaiError::process(format!(
                    "Release, tests, sync, and push succeeded, but creating tag {} failed:\n{}",
                    tag_name,
                    String::from_utf8_lossy(&tag.stderr)
                )));
            }
            let push_tag = Command::new("git")
                .args(["push", "origin", &tag_name])
                .env("GIT_TERMINAL_PROMPT", "0")
                .current_dir(workspace)
                .output()?;
            if !push_tag.status.success() {
                return Err(EaiError::process(format!(
                    "Release, tests, sync, and push succeeded, and tag {} was created locally, \
                     but pushing it failed (release.yml only triggers on a pushed tag - push it \
                     manually with `git push origin {}`):\n{}",
                    tag_name,
                    tag_name,
                    String::from_utf8_lossy(&push_tag.stderr)
                )));
            }
            crate::susi_sandbox::manager::SusiAuditLogger::log(
                &global_dir,
                crate::susi_sandbox::manager::LogLevel::Axiomatic,
                "RELEASE_CUT",
                &format!("Cut and pushed release {}.", tag_name),
            );
            return Ok(format!(
                "Motion Rule complete: check, tests, audit, lints, smoke-tests, sync, push, and \
                 tag all succeeded. Release {} is live - release.yml will build and publish its \
                 binaries now.\n{}",
                tag_name,
                Self::promote_release_locally()
            ));
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
        assert!(E2E_CHECKS.len() >= 6);
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
