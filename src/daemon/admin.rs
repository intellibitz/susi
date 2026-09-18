// SUSI Native Administrative Substrate
// 100% Rust implementation for Full Compliance Enforcement, Version Synchronization & Release Orchestration

use crate::error::{EaiError, EaiResult};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use rayon::prelude::*;

pub struct SusiAdmin;

impl SusiAdmin {
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
        let Some(major) = parts.next() else { return false };
        let Some(minor) = parts.next() else { return false };
        major == "13"
            && minor.chars().all(|c| c.is_ascii_digit())
            && minor.parse::<u32>().map(|m| m > 3).unwrap_or(false)
    }

    pub fn get_global_susi_dir() -> std::path::PathBuf {
        let home = env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".susi")
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
        Err(EaiError::config("Could not find version in Cargo.toml".to_string()))
    }

    /// Full Compliance Audit (Rule 15)
    pub fn audit_compliance(workspace: &Path, target: Option<&str>) -> EaiResult<String> {
        let mut report = "# SUSI Compliance Audit\n\n".to_string();
        if let Some(t) = target {
            report.push_str(&format!("Target: {}\n\n", t));
        }
        let mut overall_success = true;

        // 1. Audit Security Patterns (No hardcoded keys)
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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

        // 2. Enforce Workspace Purity (Rule 12)
        crate::sandbox::manager::SandboxManager::ensure_gitignore_purity(workspace);
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

        // 3. Model Integrity & Provenance (Rule 31)
        let model_verifications = crate::gemi::models::ModelManager::verify_local_models(workspace);
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

        // 4. Binary Integrity Check (Aspiration 4)
        if let Ok(current_exe) = env::current_exe() {
            let global_dir = Self::get_global_susi_dir();
            match crate::daemon::server::SusiDaemon::verify_binary_integrity(
                &current_exe,
                &global_dir,
            ) {
                Ok(true) => report.push_str(
                    "- [PASS] Binary Integrity: Executable hash matches trusted genome.\n",
                ),
                Ok(false) => {
                    report.push_str("- [FAIL] Binary Integrity: Executable hash MISMATCH. Potential tampering or build drift.\n");
                    overall_success = false;
                }
                Err(e) => report.push_str(&format!(
                    "- [WARNING] Binary Integrity: Could not verify ({})\n",
                    e
                )),
            }
        }

        // 5. Version Consistency (Rule 1)
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

        // 1. Sync Native Launcher Cargo.toml
        let launcher_cargo = workspace.join("src/native/susi/Cargo.toml");
        if launcher_cargo.exists() {
            let launcher_content = fs::read_to_string(&launcher_cargo)?;
            let mut updated = Vec::new();
            let mut in_package = false;
            for line in launcher_content.lines() {
                let trimmed = line.trim();
                if trimmed == "[package]" {
                    in_package = true;
                } else if trimmed.starts_with("[") {
                    in_package = false;
                }
                if in_package && trimmed.starts_with("version = \"") {
                    updated.push(format!("version = \"{}\"", version));
                } else {
                    updated.push(line.to_string());
                }
            }
            fs::write(&launcher_cargo, updated.join("\n") + "\n")?;
        }

        // 2. Sync README.md Badge
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

        // 3. Sync Governance Files (.agents/*.md)
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

        // 4. Update Binary Integrity Hash
        if let Ok(current_exe) = env::current_exe() {
            let global_dir = Self::get_global_susi_dir();
            fs::create_dir_all(&global_dir).map_err(|e| EaiError::filesystem(e.to_string()))?;
            let hash_file = global_dir.join("binary.hash");

            if let Ok(hash) = crate::daemon::server::SusiDaemon::calculate_binary_hash(&current_exe)
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

    /// Motion Rule (IDENTITY.md ENGINE-3): cargo check -> compliance audit ->
    /// cargo test -> clippy -> mission smoke tests -> susi admin sync -> git
    /// push, as one gated sequence. Each step must pass before the next runs;
    /// a git push failure (e.g. no configured upstream, diverged history) is
    /// reported as an error rather than silently swallowed, since the caller
    /// needs to know deployment did not complete.
    pub fn execute_release(workspace: &Path) -> EaiResult<String> {
        // `cuda`, `mkl`, and `metal` are mutually exclusive hardware backends
        // (e.g. metal pulls in macOS-only objc2 bindings) and cannot all build
        // together on any single host, so `--all-features` is never used here.
        // Instead, check default features plus the one GPU backend feature
        // that can actually compile on the host OS, mirroring install.sh's own
        // platform selection. `mkl` is intentionally excluded: it links against
        // the closed-source Intel MKL runtime, which this pipeline cannot
        // assume is installed, so it is left to be validated by whoever builds
        // with it explicitly.
        let host_gpu_feature: Option<&str> = if cfg!(target_os = "macos") {
            Some("metal")
        } else {
            Some("cuda")
        };
        let feature_sets: Vec<Option<&str>> = std::iter::once(None)
            .chain(host_gpu_feature.into_iter().map(Some))
            .collect();

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
                return Err(EaiError::process("Release aborted: cargo check failed.".to_string()));
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
            return Err(EaiError::process("Release aborted: Native tests failed.".to_string()));
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
                return Err(EaiError::process("Release aborted: Linting failed.".to_string()));
            }
        }

        eprintln!("[Release Gatekeeper] 5. Verifying Ephemeral Mission Protocols...");
        let missions = ["identity", "status", "models"];
        for mission in missions {
            let mut mission_out = Command::new("cargo")
                .args(["run", "--quiet", "--", mission])
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

        eprintln!(
            "[Release Gatekeeper] 6. Synchronizing Substrate Version Manifests (admin sync)..."
        );
        Self::enforce_version_consistency(workspace)?;

        eprintln!("[Release Gatekeeper] 7. Pushing to Remote (git push)...");
        let push = Command::new("git")
            .arg("push")
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(workspace)
            .output()?;

        let global_dir = Self::get_global_susi_dir();

        if !push.status.success() {
            let stderr = String::from_utf8_lossy(&push.stderr);
            crate::sandbox::manager::SusiAuditLogger::log(
                &global_dir,
                crate::sandbox::manager::LogLevel::Axiomatic,
                "MOTION_RULE_PUSH_FAILED",
                &format!("git push failed after a clean release/sync: {}", stderr),
            );
            return Err(EaiError::process(format!(
                "Release, tests, and sync succeeded, but git push failed:\n{}",
                stderr
            )));
        }

        crate::sandbox::manager::SusiAuditLogger::log(
            &global_dir,
            crate::sandbox::manager::LogLevel::Axiomatic,
            "MOTION_RULE_COMPLETE",
            "Full Motion Rule sequence (check -> test -> release -> sync -> push) completed successfully.",
        );

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
        if let Ok(reflex_action) = crate::gemi::pulse::SusiPulse::reason(trimmed, workspace) {
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
                fs::write(
                    &evidence_path,
                    crate::gawd::self_core::AlphaSelf::EVIDENCE_MD,
                )?;
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
            if line.contains("## 1. Sovereign Ledger (The Monotonic Proof)") || line.contains("## 1. Pending") {
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
        let res = crate::daemon::evolution::EvolutionManager::evolve_substrate(workspace)?;
        let _ = Self::execute_release(workspace)?;
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
