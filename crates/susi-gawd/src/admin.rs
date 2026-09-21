// SUSI Native Administrative Substrate
// 100% Rust implementation for Full Compliance Enforcement, Version Synchronization & Release Orchestration

use rayon::prelude::*;
use std::env;
use std::fs;
use std::path::Path;
use std::process::Command;
use susi_error::{EaiError, EaiResult};

/// How much to bump the engine's own semver when cutting a release (see
/// `SusiAdmin::execute_release`'s `cut` parameter). Plain `FromStr`, not
/// `clap::ValueEnum` - this crate stays free of CLI-parsing dependencies;
/// clap's derive macro accepts any `FromStr` type for an `Option<T>` arg
/// without that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionBump {
    Patch,
    Minor,
    Major,
}

impl std::str::FromStr for VersionBump {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "patch" => Ok(Self::Patch),
            "minor" => Ok(Self::Minor),
            "major" => Ok(Self::Major),
            other => Err(format!(
                "'{other}' is not a valid version bump (expected patch, minor, or major)"
            )),
        }
    }
}

pub struct SusiAdmin;

impl SusiAdmin {
    /// The Cargo.toml files that share the engine's own release version -
    /// susi-error/core/native/sandbox/paths/tools/agents are independently
    /// versioned library crates (all pinned at 0.1.0) and are deliberately
    /// not part of this list.
    const ENGINE_VERSION_MANIFESTS: [&'static str; 6] = [
        "Cargo.toml",
        "crates/susi-gawd/Cargo.toml",
        "crates/susi-gemi/Cargo.toml",
        "crates/susi-gmcp/Cargo.toml",
        "crates/susi-daemon/Cargo.toml",
        "crates/susi-server/Cargo.toml",
    ];

    /// Bumps a clean `major.minor.patch` version string. Pure/no I/O so the
    /// arithmetic is directly unit-testable without a real Cargo.toml.
    pub fn bump_version_string(current: &str, level: VersionBump) -> EaiResult<String> {
        let parts: Vec<&str> = current.split('.').collect();
        let [maj, min, pat]: [&str; 3] = parts.try_into().map_err(|_| {
            EaiError::config(format!(
                "cannot bump '{current}': expected major.minor.patch"
            ))
        })?;
        let parse = |s: &str| {
            s.parse::<u64>()
                .map_err(|e| EaiError::config(format!("invalid version '{current}': {e}")))
        };
        let (major, minor, patch) = (parse(maj)?, parse(min)?, parse(pat)?);
        Ok(match level {
            VersionBump::Major => format!("{}.0.0", major + 1),
            VersionBump::Minor => format!("{}.{}.0", major, minor + 1),
            VersionBump::Patch => format!("{}.{}.{}", major, minor, patch + 1),
        })
    }

    /// Writes `new_version` into one Cargo.toml's `[package] version = "..."`
    /// line only, mirroring `get_cargo_version`'s own `[package]`-section
    /// tracking so a same-named `version = "..."` inside a dependency table
    /// (e.g. `reqwest = { version = "0.13" }`) is never touched. Returns
    /// whether the file actually changed.
    fn set_cargo_version(path: &Path, new_version: &str) -> EaiResult<bool> {
        let content = fs::read_to_string(path)?;
        let mut in_package = false;
        let mut changed = false;
        let mut updated = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[package]" {
                in_package = true;
                updated.push(line.to_string());
            } else if trimmed.starts_with('[') {
                in_package = false;
                updated.push(line.to_string());
            } else if in_package && trimmed.starts_with("version = \"") {
                let new_line = format!("version = \"{}\"", new_version);
                changed |= new_line != trimmed;
                updated.push(new_line);
            } else {
                updated.push(line.to_string());
            }
        }
        if changed {
            fs::write(path, updated.join("\n") + "\n")?;
        }
        Ok(changed)
    }

    /// Bumps every engine-version Cargo.toml to `new_version` together, so
    /// they can never drift out of sync with each other the way the root
    /// version alone used to drift from the (nonexistent) release tag.
    /// Also regenerates Cargo.lock to ensure the workspace builds with --locked.
    /// Returns the original versions for rollback in case of failure.
    fn cut_version(workspace: &Path, new_version: &str) -> EaiResult<Vec<(String, String)>> {
        let mut original_versions = Vec::new();

        for rel in Self::ENGINE_VERSION_MANIFESTS {
            let path = workspace.join(rel);
            if path.exists() {
                // Save original version for potential rollback
                if let Ok(orig) = Self::get_cargo_version_from_file(&path) {
                    original_versions.push((rel.to_string(), orig));
                }
                Self::set_cargo_version(&path, new_version)?;
            }
        }

        // Regenerate Cargo.lock after version bump to ensure --locked builds work
        eprintln!("[Release Gatekeeper]    -> Regenerating Cargo.lock for workspace...");
        let mut update = Command::new("cargo")
            .args(["update", "--workspace", "--offline"])
            .current_dir(workspace)
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()?;
        let status = update.wait()?;
        if !status.success() {
            return Err(EaiError::process(
                "Release aborted: cargo update failed to regenerate Cargo.lock.".to_string(),
            ));
        }

        Ok(original_versions)
    }

    /// Rolls back version changes to the original state if the release fails
    fn rollback_version_cut(
        workspace: &Path,
        original_versions: &[(String, String)],
    ) -> EaiResult<()> {
        eprintln!("[Release Gatekeeper] Rolling back version cut due to failure...");
        for (rel, orig_version) in original_versions {
            let path = workspace.join(rel);
            if path.exists() {
                if let Err(e) = Self::set_cargo_version(&path, orig_version) {
                    eprintln!("Warning: failed to rollback {}: {}", rel, e);
                }
            }
        }
        // Revert Cargo.lock to original state
        let _ = Command::new("git")
            .args(["checkout", "--", "Cargo.lock"])
            .current_dir(workspace)
            .output();
        Ok(())
    }

    /// Gets version from a specific Cargo.toml file
    fn get_cargo_version_from_file(path: &Path) -> EaiResult<String> {
        let content = fs::read_to_string(path)?;
        let mut in_package = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[package]" {
                in_package = true;
            } else if trimmed.starts_with("[") {
                in_package = false;
            } else if in_package && trimmed.starts_with("version = \"") {
                if let Some(v) = trimmed.split('"').nth(1) {
                    return Ok(v.to_string());
                }
            }
        }
        Err(EaiError::config(
            "Could not find version in Cargo.toml".to_string(),
        ))
    }
    /// Mirrors install.sh's `CUDARC_CUDA_VERSION` clamp: cudarc (candle's CUDA
    /// backend) pins an exact allowlist of CUDA toolkit versions and panics on
    /// any newer 13.x point release it hasn't added yet (as of cudarc 0.19.9,
    /// that ceiling is 13.3). install.sh exports this override before invoking
    /// `cargo build` from a shell, but the release gate spawns `cargo` directly
    /// as a child process — a fresh `Command` does not re-run install.sh, so
    /// without this it inherits none of that clamp and a `cargo check/clippy
    /// --features cuda` here panics on any host running CUDA 13.4+, exactly as
    /// install.sh's own comment already documented happening before its fix.
    fn cuda_version_clamp_env() -> Option<(&'static str, &'static str)> {
        let output = Command::new("nvcc").arg("--version").output().ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        Self::cuda_version_from_nvcc_output(&text).filter(|v| Self::cuda_version_needs_clamp(v))?;
        Some(("CUDARC_CUDA_VERSION", "13030"))
    }

    /// Extracts the `X.Y` release version from `nvcc --version` output, e.g.
    /// "Cuda compilation tools, release 13.4, V13.4.59" -> "13.4".
    fn cuda_version_from_nvcc_output(text: &str) -> Option<String> {
        text.lines()
            .find(|l| l.contains("release"))
            .and_then(|l| l.split("release ").nth(1))
            .map(|v| v.split(',').next().unwrap_or(v).trim().to_string())
    }

    /// True when `version` (e.g. "13.4") is a CUDA 13.x release past cudarc
    /// 0.19.9's hardcoded allowlist ceiling of 13.3, mirroring install.sh's
    /// bash `CUDA_MAJOR == 13 && CUDA_MINOR > 3` check.
    fn cuda_version_needs_clamp(version: &str) -> bool {
        let mut parts = version.splitn(2, '.');
        let Some(major) = parts.next() else {
            return false;
        };
        let Some(minor) = parts.next() else {
            return false;
        };
        major == "13"
            && minor.chars().all(|c| c.is_ascii_digit())
            && minor.parse::<u32>().map(|m| m > 3).unwrap_or(false)
    }

    pub fn get_global_susi_dir() -> std::path::PathBuf {
        susi_paths::SusiDirs::config_dir()
    }

    pub fn get_cargo_version(workspace: &Path) -> EaiResult<String> {
        let cargo_toml_path = workspace.join("Cargo.toml");
        let content = fs::read_to_string(&cargo_toml_path)?;
        let mut in_package = false;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed == "[package]" {
                in_package = true;
            } else if trimmed.starts_with("[") {
                in_package = false;
            } else if in_package && trimmed.starts_with("version = \"") {
                if let Some(v) = trimmed.split('"').nth(1) {
                    return Ok(v.to_string());
                }
            }
        }
        Err(EaiError::config(
            "Could not find version in Cargo.toml".to_string(),
        ))
    }

    /// Full Compliance Audit
    pub fn audit_compliance(workspace: &Path, target: Option<&str>) -> EaiResult<String> {
        let mut report = "# SUSI Compliance Audit\n\n".to_string();
        if let Some(t) = target {
            report.push_str(&format!("Target: {}\n\n", t));
        }
        let mut overall_success = true;

        // 1. Audit Security Patterns (No hardcoded keys)
        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let patterns = cfg.governance().secret_tokens();
        let src_dir = workspace.join("src");

        let mut files = Vec::new();
        let mut dirs = vec![src_dir];
        while let Some(dir) = dirs.pop() {
            if let Ok(entries) = fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(path);
                    } else if path.is_file() {
                        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
                        if !file_name.contains("security.rs")
                            && !file_name.contains("admin.rs")
                            && !file_name.contains("manager.rs")
                        {
                            files.push(path);
                        }
                    }
                }
            }
        }

        let hit_reports: Vec<String> = files
            .into_par_iter()
            .filter_map(|path| {
                if let Ok(content) = fs::read_to_string(&path) {
                    let mut local_hits = Vec::new();
                    for p in &patterns {
                        if content.contains(p) {
                            local_hits.push(format!("- [FAIL] Security: Potential secret matching '{}' detected in {}.\n", p, path.display()));
                        }
                    }
                    if !local_hits.is_empty() {
                        return Some(local_hits.join(""));
                    }
                }
                None
            })
            .collect();

        if hit_reports.is_empty() {
            report.push_str("- [PASS] Security: No hardcoded secrets detected.\n");
        } else {
            overall_success = false;
            for h in hit_reports {
                report.push_str(&h);
            }
        }

        // 2. Enforce Workspace Purity
        susi_sandbox::manager::SandboxManager::ensure_gitignore_purity(workspace);
        let gitignore = workspace.join(".gitignore");
        if gitignore.exists() {
            let content = fs::read_to_string(&gitignore)?;
            if content.contains(".susi") || content.contains(".susi/") {
                report.push_str("- [PASS] Workspace Purity: .susi is correctly git-ignored.\n");
            } else {
                report.push_str("- [FAIL] Workspace Purity: .susi is NOT git-ignored.\n");
                overall_success = false;
            }
        }

        // 3. Model Integrity & Provenance
        let model_verifications = susi_gemi::models::ModelManager::verify_local_models(workspace);
        if model_verifications.is_empty() {
            report.push_str("- [WARNING] Models: No local model substrates found.\n");
        } else {
            for v in model_verifications {
                let status = if v.checksum_verified { "PASS" } else { "FAIL" };
                report.push_str(&format!(
                    "- [{}] Model Integrity: {} (Verified: {})\n",
                    status, v.model_id, v.checksum_verified
                ));
                if !v.checksum_verified {
                    overall_success = false;
                }
            }
        }

        // 4. Binary Integrity Check
        if let Ok(current_exe) = env::current_exe() {
            let global_dir = Self::get_global_susi_dir();
            match susi_sandbox::daemon_state::SusiDaemonState::verify_binary_integrity(
                &current_exe,
                &global_dir,
            ) {
                Ok(true) => report.push_str(
                    "- [PASS] Binary Integrity: Executable hash matches trusted genome.\n",
                ),
                Ok(false) => {
                    // The Motion Rule's own `cargo check`/`cargo test` can relink
                    // `current_exe` before this audit runs; treat that rebuild drift
                    // as a warning on the release path (verify already refreshed
                    // `binary.hash`). For ad-hoc audits, mismatch stays a failure.
                    if target == Some("release") {
                        report.push_str(
                            "- [WARNING] Binary Integrity: Executable hash refreshed after gatekeeper rebuild.\n",
                        );
                    } else {
                        report.push_str("- [FAIL] Binary Integrity: Executable hash MISMATCH. Potential tampering or build drift.\n");
                        overall_success = false;
                    }
                }
                Err(e) => report.push_str(&format!(
                    "- [WARNING] Binary Integrity: Could not verify ({})\n",
                    e
                )),
            }
        }

        // 5. Version Consistency
        match Self::enforce_version_consistency(workspace) {
            Ok(v) => report.push_str(&format!(
                "- [PASS] Version Consistency: All manifests synchronized to v{}.\n",
                v
            )),
            Err(e) => {
                report.push_str(&format!("- [FAIL] Version Consistency: {}\n", e));
                overall_success = false;
            }
        }

        if overall_success {
            Ok(report)
        } else {
            Err(EaiError::governance(format!(
                "Compliance Audit Failed:\n{}",
                report
            )))
        }
    }

    /// Enforce Version Consistency across all files using Cargo.toml as the source of truth.
    pub fn enforce_version_consistency(workspace: &Path) -> EaiResult<String> {
        let version_string = Self::get_cargo_version(workspace)?;
        let version = version_string.as_str();

        // 1. Sync README.md Badge
        let readme_path = workspace.join("README.md");
        if readme_path.exists() {
            let readme_content = fs::read_to_string(&readme_path)?;
            let mut updated = Vec::new();
            for line in readme_content.lines() {
                if line.contains("https://img.shields.io/badge/version-v") {
                    let updated_line = format!("![SUSI Version](https://img.shields.io/badge/version-v{}-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)", version);
                    updated.push(updated_line);
                } else {
                    updated.push(line.to_string());
                }
            }
            fs::write(&readme_path, updated.join("\n") + "\n")?;
        }

        // 2. Sync Governance Files (.agents/*.md)
        let governance_files = ["IDENTITY.md", "ROADMAP.md", "EVIDENCE.md"];
        for file_name in governance_files {
            let path = workspace.join(".agents").join(file_name);
            if path.exists() {
                let content = fs::read_to_string(&path)?;
                let mut updated = Vec::new();
                let mut in_frontmatter = false;
                let mut frontmatter_count = 0;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed == "---" {
                        frontmatter_count += 1;
                        in_frontmatter = frontmatter_count == 1;
                        updated.push(line.to_string());
                        continue;
                    }

                    if in_frontmatter && trimmed.starts_with("version = \"") {
                        updated.push(format!("version = \"{}\"", version));
                    } else if !in_frontmatter
                        && trimmed.starts_with("* **Current Engine Version**: `v")
                    {
                        updated.push(format!("* **Current Engine Version**: `v{}`", version));
                    } else {
                        updated.push(line.to_string());
                    }

                    if frontmatter_count == 2 {
                        in_frontmatter = false;
                    }
                }
                fs::write(&path, updated.join("\n") + "\n")?;
            }
        }

        // 3. Update Binary Integrity Hash
        if let Ok(current_exe) = env::current_exe() {
            let global_dir = Self::get_global_susi_dir();
            fs::create_dir_all(&global_dir).map_err(|e| EaiError::filesystem(e.to_string()))?;
            let hash_file = global_dir.join("binary.hash");

            if let Ok(hash) =
                susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&current_exe)
            {
                fs::write(&hash_file, hash).map_err(|e| EaiError::filesystem(e.to_string()))?;
            }
        }

        // NOTE: config.default.json is intentionally NOT regenerated here.
        // SusiConfig::default() is deserialized from config.default.json via
        // include_str! at compile time, so writing it back out is a pure
        // round-trip that can only preserve or lose information — never add
        // any. If a binary older than the latest source (e.g. a stale
        // long-running daemon) runs this sync path, reserializing its
        // compiled-in defaults would silently delete any config keys added
        // to the source file since that binary was built. config.default.json
        // is hand/agent-maintained source, checked into version control; it
        // has no legitimate "sync" target to converge toward.

        Ok(version.to_string())
    }

    /// Checks if all files are in sync with the current Cargo.toml version.
    /// Does not modify files; returns an error if a mismatch is detected.
    pub fn verify_version_alignment(workspace: &Path) -> EaiResult<()> {
        let version_string = Self::get_cargo_version(workspace)?;
        let version = version_string.as_str();

        // Check README
        let readme_path = workspace.join("README.md");
        if readme_path.exists() {
            let readme_content = fs::read_to_string(&readme_path)?;
            let expected_badge = format!("version-v{}-blue.svg", version);
            if !readme_content.contains(&expected_badge) {
                return Err(EaiError::config(format!("README.md version badge is out of sync with Cargo.toml (v{}). Run 'susi admin sync'.", version)));
            }
        }

        // Check Governance Files (.agents/*.md)
        let governance_files = ["IDENTITY.md", "ROADMAP.md", "EVIDENCE.md"];
        for file_name in governance_files {
            let path = workspace.join(".agents").join(file_name);
            if path.exists() {
                let content = fs::read_to_string(&path)?;
                let expected_line = format!("version = \"{}\"", version);
                let expected_legacy = format!("* **Current Engine Version**: `v{}`", version);
                if !content.contains(&expected_line) && !content.contains(&expected_legacy) {
                    return Err(EaiError::config(format!(
                        "{}: version is out of sync with Cargo.toml (v{}). Run 'susi admin sync'.",
                        file_name, version
                    )));
                }
            }
        }

        Ok(())
    }

    /// Motion Rule (IDENTITY.md Pillar IV item 3): cargo check -> compliance audit ->
    /// cargo test -> clippy -> mission smoke tests -> susi admin sync -> git
    /// push, as one gated sequence. Each step must pass before the next runs;
    /// a git push failure (e.g. no configured upstream, diverged history) is
    /// reported as an error rather than silently swallowed, since the caller
    /// needs to know deployment did not complete.
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
        let mut test = Command::new("cargo")
            .arg("test")
            .current_dir(workspace)
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
        let missions = ["identity", "status", "models"];
        for mission in missions {
            let mut mission_out = Command::new(&susi_bin)
                .arg(mission)
                .current_dir(workspace)
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
                ".agents/IDENTITY.md",
                ".agents/ROADMAP.md",
                ".agents/EVIDENCE.md",
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
            susi_sandbox::manager::SusiAuditLogger::log(
                &global_dir,
                susi_sandbox::manager::LogLevel::Axiomatic,
                "MOTION_RULE_PUSH_FAILED",
                &format!("git push failed after a clean release/sync: {}", stderr),
            );
            return Err(EaiError::process(format!(
                "Release, tests, and sync succeeded, but git push failed:\n{}",
                stderr
            )));
        }

        susi_sandbox::manager::SusiAuditLogger::log(
            &global_dir,
            susi_sandbox::manager::LogLevel::Axiomatic,
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
            susi_sandbox::manager::SusiAuditLogger::log(
                &global_dir,
                susi_sandbox::manager::LogLevel::Axiomatic,
                "RELEASE_CUT",
                &format!("Cut and pushed release {}.", tag_name),
            );
            return Ok(format!(
                "Motion Rule complete: check, tests, audit, lints, smoke-tests, sync, push, and \
                 tag all succeeded. Release {} is live - release.yml will build and publish its \
                 binaries now.",
                tag_name
            ));
        }

        Ok("Motion Rule complete: check, tests, audit, lints, smoke-tests, sync, and push all succeeded. Substrate deployed.".into())
    }

    /// Dynamic Neural Cascade Classifier (Tier 0 Reflex -> Tier 2 GEMI -> Motion)
    pub fn classify_natural_intent(workspace: &Path, intent: &str) -> (&'static str, &'static str) {
        let trimmed = intent.trim();
        let lower = trimmed.to_lowercase();

        // Fast-Path Reflex for Standard Queries/Motions
        if lower == "identity"
            || lower == "status"
            || lower == "models"
            || lower == "version"
            || lower == "ls"
            || lower.starts_with("ls ")
            || lower == "dir"
            || lower.contains("who am i")
            || lower.contains("whoami")
            || lower.starts_with("susi status")
            || lower.starts_with("susi identity")
            || lower.starts_with("susi models")
        {
            return ("[QUERY]", "Zero-Mutation Interrogation");
        }

        if lower.contains("motion") || lower.contains("architecture") || lower.contains("hardcode")
        {
            return ("[MOTION]", "Architectural Evolution");
        }

        // Tier 0 Neural Reflex Attempt via Local Alpha Model
        if let Ok(reflex_action) = susi_gemi::pulse::SusiPulse::reason(trimmed, workspace) {
            let reflex_lower = reflex_action.to_lowercase();
            if reflex_lower.contains("query")
                || reflex_lower.contains("status")
                || reflex_lower.contains("identity")
            {
                return ("[QUERY]", "Zero-Mutation Interrogation");
            } else if reflex_lower.contains("motion") || reflex_lower.contains("recompile") {
                return ("[MOTION]", "Architectural Evolution");
            } else if reflex_lower.contains("mission") || reflex_lower.contains("solve") {
                return ("[MISSION]", "Dynamic Task Fulfillment");
            }
        }

        // Default to Dynamic Task Fulfillment Mission
        ("[MISSION]", "Dynamic Task Fulfillment")
    }

    /// Ingest a natural language intent and automatically inject it into EVIDENCE.md
    /// Supports both Genomic mode (.agents/EVIDENCE.md) and World mode (.susi/EVIDENCE.md).
    pub fn ingest_natural_intent(workspace: &Path, intent: &str) -> EaiResult<String> {
        let mut evidence_path = workspace.join(".agents/EVIDENCE.md");

        // World Fallback: If .agents/ is missing, use .susi/ sandbox
        if !evidence_path.exists() {
            evidence_path = workspace.join(".susi/EVIDENCE.md");
            if !evidence_path.exists() {
                // Synthesize a new local evidence from hard-compiled genome if missing
                fs::create_dir_all(workspace.join(".susi"))
                    .map_err(|e| EaiError::filesystem(e.to_string()))?;
                fs::write(&evidence_path, crate::self_core::AlphaSelf::EVIDENCE_MD)?;
            }
        }

        // 1. Dynamic Neural Cascade Classifier (Tier 0 Reflex -> Tier 2 GEMI -> Motion)
        let (prefix, _category) = Self::classify_natural_intent(workspace, intent);

        // 2. Read EVIDENCE.md and find the last index
        let content = fs::read_to_string(&evidence_path)?;
        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

        // 2.5 Prevent Duplicate Intent Ingestion
        let intent_trimmed = intent.trim().to_lowercase();
        let is_duplicate = lines
            .iter()
            .any(|l| l.to_lowercase().contains(&intent_trimmed));

        if is_duplicate {
            return Ok(format!(
                "Intent '{}' is already present in memory ({})",
                intent, prefix
            ));
        }

        let mut last_index = 0;
        let mut best_suffix = "2022920".to_string();

        for line in &lines {
            if line.contains("EV-") {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() > 1 {
                    let token = parts[1].trim();
                    if token.starts_with("EV-") {
                        let sub_parts: Vec<&str> = token.split('-').collect();
                        if sub_parts.len() >= 3 {
                            if sub_parts[1] > best_suffix.as_str() {
                                best_suffix = sub_parts[1].to_string();
                            }
                            if let Ok(idx) = sub_parts[2].parse::<usize>() {
                                if idx > last_index {
                                    last_index = idx;
                                }
                            }
                        }
                    }
                }
            }
        }

        let new_index = last_index + 1;
        let id = format!("EV-{}-{:03}", best_suffix, new_index);

        let entry = format!(
            "| {} | {} | {} | [manual](symbol://manual) | STAGED |",
            id, prefix, intent
        );

        // 3. Inject into Section 1 (Pending)
        let mut section1_start = None;
        for (i, line) in lines.iter().enumerate() {
            if line.contains("## 1. Sovereign Ledger (The Monotonic Proof)")
                || line.contains("## 1. Pending")
            {
                section1_start = Some(i);
                break;
            }
        }

        if let Some(start) = section1_start {
            let mut insert_pos = start + 1;
            while insert_pos < lines.len()
                && (lines[insert_pos].trim().is_empty()
                    || lines[insert_pos].trim().starts_with("---")
                    || lines[insert_pos].trim().starts_with("| ID")
                    || lines[insert_pos].trim().starts_with("| :---"))
            {
                insert_pos += 1;
            }
            lines.insert(insert_pos, entry);
        } else {
            lines.push(entry);
        }

        let mut final_content = lines.join("\n");
        if !final_content.ends_with('\n') {
            final_content.push('\n');
        }
        fs::write(&evidence_path, final_content)?;

        Ok(format!(
            "Intent ingested successfully as {} into sovereign memory",
            prefix
        ))
    }

    pub fn execute_autonomous_evolution_cycle(workspace: &Path) -> EaiResult<String> {
        let res = crate::evolution::EvolutionManager::evolve_substrate(workspace)?;
        // Never auto-cuts a version bump - that's a deliberate `--cut`-only
        // action (see execute_release's own doc comment on why), not
        // something an autonomous cycle should ever decide on its own.
        let _ = Self::execute_release(workspace, None)?;
        Ok(res)
    }

    pub fn run_lint(workspace: &Path) -> EaiResult<String> {
        let out = Command::new("cargo")
            .args(["clippy", "--all-targets", "--all-features"])
            .current_dir(workspace)
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    pub fn run_audit(workspace: &Path) -> EaiResult<String> {
        let out = Command::new("cargo")
            .arg("audit")
            .current_dir(workspace)
            .output()?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_alignment_enforcement() {
        let workspace = Path::new(".");
        // This test ensures that the build fails if developer forgot to run 'susi admin sync'
        let result = SusiAdmin::verify_version_alignment(workspace);
        assert!(result.is_ok(), "Version mismatch detected between Cargo.toml and documentation. Run 'cargo run -- admin sync' to fix.");
    }

    #[test]
    fn test_cuda_version_from_nvcc_output_extracts_release_version() {
        let text = "nvcc: NVIDIA (R) Cuda compiler driver\n\
                     Copyright (c) 2005-2026 NVIDIA Corporation\n\
                     Built on Mon_Aug_03_13:24:11_PDT_2026\n\
                     Cuda compilation tools, release 13.4, V13.4.59\n\
                     Build cuda_13.4.r13.4/compiler.38657139_0\n";
        assert_eq!(
            SusiAdmin::cuda_version_from_nvcc_output(text).as_deref(),
            Some("13.4")
        );
    }

    #[test]
    fn test_cuda_version_from_nvcc_output_none_without_release_line() {
        assert_eq!(SusiAdmin::cuda_version_from_nvcc_output("garbage\n"), None);
    }

    #[test]
    fn test_bump_version_string_patch_minor_major() {
        assert_eq!(
            SusiAdmin::bump_version_string("0.2.3", VersionBump::Patch).unwrap(),
            "0.2.4"
        );
        assert_eq!(
            SusiAdmin::bump_version_string("0.2.3", VersionBump::Minor).unwrap(),
            "0.3.0"
        );
        assert_eq!(
            SusiAdmin::bump_version_string("0.2.3", VersionBump::Major).unwrap(),
            "1.0.0"
        );
    }

    #[test]
    fn test_bump_version_string_rejects_wrong_shape() {
        // Wrong part count.
        assert!(SusiAdmin::bump_version_string("1.2", VersionBump::Patch).is_err());
        // A pre-release suffix makes the last segment non-numeric.
        assert!(SusiAdmin::bump_version_string("1.2.3-rc1", VersionBump::Patch).is_err());
    }

    #[test]
    fn test_version_bump_from_str_is_case_insensitive_and_rejects_unknown() {
        use std::str::FromStr;
        assert_eq!(VersionBump::from_str("patch").unwrap(), VersionBump::Patch);
        assert_eq!(VersionBump::from_str("MINOR").unwrap(), VersionBump::Minor);
        assert_eq!(VersionBump::from_str("Major").unwrap(), VersionBump::Major);
        assert!(VersionBump::from_str("banana").is_err());
    }

    #[test]
    fn test_set_cargo_version_only_touches_package_section() {
        let dir = std::env::temp_dir().join(format!(
            "susi_set_cargo_version_test_{}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("Cargo.toml");
        fs::write(
            &path,
            "[package]\n\
             name = \"demo\"\n\
             version = \"0.2.3\"\n\
             edition = \"2021\"\n\
             \n\
             [dependencies]\n\
             reqwest = { version = \"0.13\" }\n",
        )
        .unwrap();

        let changed = SusiAdmin::set_cargo_version(&path, "0.3.0").unwrap();
        assert!(changed);

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("version = \"0.3.0\""));
        // The dependency's own inline `version = "0.13"` must survive untouched.
        assert!(content.contains("reqwest = { version = \"0.13\" }"));

        // Re-applying the same version is a no-op (reported as unchanged).
        assert!(!SusiAdmin::set_cargo_version(&path, "0.3.0").unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cuda_version_needs_clamp_matches_install_sh_thresholds() {
        // Mirrors install.sh: only CUDA 13.x point releases past cudarc
        // 0.19.9's 13.3 allowlist ceiling need the override.
        assert!(!SusiAdmin::cuda_version_needs_clamp("13.3"));
        assert!(SusiAdmin::cuda_version_needs_clamp("13.4"));
        assert!(SusiAdmin::cuda_version_needs_clamp("13.9"));
        assert!(!SusiAdmin::cuda_version_needs_clamp("12.9"));
        assert!(!SusiAdmin::cuda_version_needs_clamp("14.0"));
        assert!(!SusiAdmin::cuda_version_needs_clamp("13"));
    }

    #[test]
    fn test_cuda_version_clamp_env_live_matches_needs_clamp() {
        // Live-exercises the real nvcc invocation this host has (verified
        // present: CUDA 13.4 toolkit), proving the end-to-end wiring —
        // not just the pure parser — produces the correct override.
        let Ok(output) = Command::new("nvcc").arg("--version").output() else {
            return; // honest skip: no nvcc on this host/CI runner
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let Some(version) = SusiAdmin::cuda_version_from_nvcc_output(&text) else {
            return;
        };
        let expected = if SusiAdmin::cuda_version_needs_clamp(&version) {
            Some(("CUDARC_CUDA_VERSION", "13030"))
        } else {
            None
        };
        assert_eq!(SusiAdmin::cuda_version_clamp_env(), expected);
    }
}
