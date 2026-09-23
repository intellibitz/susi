use super::SusiAdmin;
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

impl SusiAdmin {
    /// The Cargo.toml files that share the engine's own release version -
    /// susi-error/core/native/sandbox/paths/tools/agents are independently
    /// versioned library crates (all pinned at 0.1.0) and are deliberately
    /// not part of this list.
    pub(crate) const ENGINE_VERSION_MANIFESTS: [&'static str; 6] = [
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
    pub(crate) fn cut_version(
        workspace: &Path,
        new_version: &str,
    ) -> EaiResult<Vec<(String, String)>> {
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
    pub(crate) fn rollback_version_cut(
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
    pub(crate) fn cuda_version_clamp_env() -> Option<(&'static str, &'static str)> {
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
            let new_content = updated.join("\n") + "\n";
            // Same hazard as the governance JSON path below: rewriting an
            // already-correct badge is usually a byte no-op (which is why
            // README did not show up dirtied), but trailing-newline /
            // line-ending drift would still dirty the tree on every audit.
            // Only write when the reconstructed file actually differs.
            if new_content != readme_content {
                fs::write(&readme_path, new_content)?;
            }
        }

        // 2. Sync agent-governance ledgers (.agents/*.json)
        let governance_files = ["identity.json", "roadmap.json", "evidence.json"];
        for file_name in governance_files {
            let path = workspace.join(".agents").join(file_name);
            if path.exists() {
                let content = fs::read_to_string(&path)?;
                let original: serde_json::Value = serde_json::from_str(&content)
                    .map_err(|e| EaiError::config(format!("{file_name}: invalid JSON: {e}")))?;
                let mut doc = original.clone();
                doc["version"] = serde_json::Value::String(version.to_string());
                // Skip the write when nothing semantically changed (the common
                // case: this runs on every compliance audit, not just an
                // actual version bump). `serde_json::to_string_pretty` writes
                // non-ASCII literally rather than as `\uXXXX` escapes, which
                // previously differed byte-for-byte from how these files were
                // hand-edited - rewriting unconditionally re-escaped every
                // em-dash on every audit, dirtying the tree even when the
                // version hadn't changed and tripping `execute_release`'s own
                // "working tree must be clean" gate before a real release
                // ever got to run.
                if doc == original {
                    continue;
                }
                let pretty = serde_json::to_string_pretty(&doc)
                    .map_err(|e| EaiError::config(format!("{file_name}: serialize: {e}")))?;
                fs::write(&path, pretty + "\n")?;
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

        // Check agent-governance ledgers (.agents/*.json)
        let governance_files = ["identity.json", "roadmap.json", "evidence.json"];
        for file_name in governance_files {
            let path = workspace.join(".agents").join(file_name);
            if path.exists() {
                let content = fs::read_to_string(&path)?;
                let doc: serde_json::Value = serde_json::from_str(&content)
                    .map_err(|e| EaiError::config(format!("{file_name}: invalid JSON: {e}")))?;
                let file_ver = doc.get("version").and_then(|v| v.as_str()).unwrap_or("");
                if file_ver != version {
                    return Err(EaiError::config(format!(
                        "{}: version is out of sync with Cargo.toml (v{}). Run 'susi admin sync'.",
                        file_name, version
                    )));
                }
            }
        }

        Ok(())
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

    /// Minimal workspace fixture for `enforce_version_consistency`: root
    /// Cargo.toml plus the README badge and one governance ledger. The
    /// ledger deliberately uses `\u2014` escapes so a naïve
    /// `to_string_pretty` rewrite would change bytes even when the version
    /// field is already correct — the bug that dirtied the tree on every
    /// compliance audit and blocked `--cut`.
    fn make_version_sync_fixture(dir: &Path, cargo_version: &str, ledger_version: &str) {
        fs::create_dir_all(dir.join(".agents")).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"demo\"\nversion = \"{cargo_version}\"\nedition = \"2021\"\n"
            ),
        )
        .unwrap();
        fs::write(
            dir.join("README.md"),
            format!(
                "# demo\n\n![SUSI Version](https://img.shields.io/badge/version-v{cargo_version}-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)\n"
            ),
        )
        .unwrap();
        // Keep `\u2014` as a literal escape sequence in the on-disk bytes.
        fs::write(
            dir.join(".agents/identity.json"),
            format!(
                "{{\n  \"version\": \"{ledger_version}\",\n  \"note\": \"before\\u2014after\"\n}}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn test_enforce_version_consistency_skips_write_when_already_aligned() {
        let dir =
            std::env::temp_dir().join(format!("susi_enforce_version_noop_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        make_version_sync_fixture(&dir, "0.2.3", "0.2.3");

        let before_readme = fs::read(dir.join("README.md")).unwrap();
        let before_identity = fs::read(dir.join(".agents/identity.json")).unwrap();
        assert!(
            before_identity.windows(6).any(|w| w == br"\u2014"),
            "fixture must contain a \\u2014 escape so a rewrite would be visible"
        );

        let version = SusiAdmin::enforce_version_consistency(&dir).unwrap();
        assert_eq!(version, "0.2.3");

        // Byte-identical: neither the badge rewrite nor the serde_json pretty
        // printer touched disk when nothing semantically changed. This is
        // what keeps `execute_release(..., Some(cut))`'s clean-tree gate
        // from failing after a prior compliance audit.
        assert_eq!(fs::read(dir.join("README.md")).unwrap(), before_readme);
        assert_eq!(
            fs::read(dir.join(".agents/identity.json")).unwrap(),
            before_identity
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_enforce_version_consistency_writes_when_version_differs() {
        let dir =
            std::env::temp_dir().join(format!("susi_enforce_version_bump_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        // Mimics the --cut path: Cargo.toml already bumped, docs still on
        // the previous version — enforce must bring them forward.
        make_version_sync_fixture(&dir, "0.3.0", "0.2.3");
        // Stale badge so README also needs a rewrite.
        fs::write(
            dir.join("README.md"),
            "# demo\n\n![SUSI Version](https://img.shields.io/badge/version-v0.2.3-blue.svg) ![License](https://img.shields.io/badge/license-Apache%202.0-green.svg)\n",
        )
        .unwrap();

        let version = SusiAdmin::enforce_version_consistency(&dir).unwrap();
        assert_eq!(version, "0.3.0");

        let readme = fs::read_to_string(dir.join("README.md")).unwrap();
        assert!(readme.contains("version-v0.3.0-blue.svg"));
        assert!(!readme.contains("version-v0.2.3-blue.svg"));

        let identity: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join(".agents/identity.json")).unwrap())
                .unwrap();
        assert_eq!(identity["version"], "0.3.0");

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
