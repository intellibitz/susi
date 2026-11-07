//! Verified offline bundle import and airgapped bootstrap recovery (`VC-201-050`).
//!
//! Imports a checksummed offline bundle (binaries, local model weights, and
//! extension metadata) into an airgapped `SUSI_HOME`, verifying size and
//! SHA-256 digests *before* any artifact is installed, and runs deterministic
//! offline readiness drills that diagnose missing or corrupted artifacts
//! locally without attempting network or cloud fallback.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use susi_error::{EaiError, EaiResult};

/// Category of an artifact carried in an offline bootstrap bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Binary,
    Model,
    ExtensionMetadata,
}

/// One checksummed file entry in an [`OfflineBundleManifest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleArtifactEntry {
    pub name: String,
    pub kind: ArtifactKind,
    pub relative_path: String,
    pub sha256_hex: String,
    pub size_bytes: u64,
}

/// Manifest located at `<bundle_dir>/manifest.json` and installed to
/// `<target_home>/offline_manifest.json` after verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflineBundleManifest {
    pub bundle_id: String,
    pub version: String,
    pub artifacts: Vec<BundleArtifactEntry>,
}

/// Outcome of importing a verified offline bundle into `target_home`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflineImportReport {
    pub bundle_id: String,
    pub imported_artifacts: Vec<String>,
    pub target_home: PathBuf,
}

/// Actionable local diagnosis for a missing or corrupted offline artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingArtifactDiagnosis {
    pub name: String,
    pub kind: ArtifactKind,
    pub expected_path: String,
    pub reason: String,
}

/// Result of running an airgapped bootstrap readiness drill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfflineReadinessReport {
    pub ready_for_local_mission: bool,
    pub network_disabled: bool,
    pub cloud_fallback_attempted: bool,
    pub verified_artifacts: Vec<String>,
    pub missing_or_corrupt: Vec<MissingArtifactDiagnosis>,
    pub local_mission_output: Option<String>,
}

/// Compute lowercase hex SHA-256 digest of `bytes`.
#[must_use]
pub fn sha256_hex_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Validate that `rel` is a safe relative path within a bundle or home directory.
///
/// # Errors
///
/// Returns `EaiError::governance` if `rel` is empty, contains backslashes or
/// drive prefixes, is absolute, or includes `.` / `..` traversal components.
pub fn validate_relative_artifact_path(rel: &str) -> EaiResult<PathBuf> {
    let trimmed = rel.trim();
    if trimmed.is_empty() {
        return Err(EaiError::governance(
            "offline bundle artifact relative_path must not be empty",
        ));
    }
    if trimmed.contains('\\') || trimmed.contains(':') {
        return Err(EaiError::governance(format!(
            "offline bundle artifact path '{trimmed}' must use forward slashes without drive prefixes"
        )));
    }
    let path = Path::new(trimmed);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                return Err(EaiError::governance(format!(
                    "offline bundle artifact path '{trimmed}' contains forbidden traversal or root component"
                )));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(EaiError::governance(
            "offline bundle artifact path resolved to empty path",
        ));
    }
    Ok(clean)
}

/// Verify every artifact in `bundle_dir/manifest.json` (size + SHA-256) before
/// atomically installing them into `target_home`.
///
/// If any artifact fails validation, is missing, or mismatches its declared size
/// or SHA-256 checksum, `import_verified_bundle` aborts before writing any file
/// into `target_home`.
///
/// # Errors
///
/// Returns `EaiError::io`, `EaiError::config`, or `EaiError::governance` when
/// the manifest is invalid or any artifact fails checksum/size verification.
pub fn import_verified_bundle(
    bundle_dir: &Path,
    target_home: &Path,
) -> EaiResult<OfflineImportReport> {
    let manifest_path = bundle_dir.join("manifest.json");
    let raw_manifest = std::fs::read_to_string(&manifest_path).map_err(|e| {
        EaiError::io(format!(
            "read offline bundle manifest {}: {e}",
            manifest_path.display()
        ))
    })?;
    let manifest: OfflineBundleManifest = serde_json::from_str(&raw_manifest).map_err(|e| {
        EaiError::config(format!(
            "parse offline bundle manifest {}: {e}",
            manifest_path.display()
        ))
    })?;
    if manifest.bundle_id.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err(EaiError::config(
            "offline bundle manifest requires non-empty bundle_id and version",
        ));
    }
    if manifest.artifacts.is_empty() {
        return Err(EaiError::config(
            "offline bundle manifest must declare at least one artifact",
        ));
    }

    // Phase 1: Verify every artifact in memory before touching target_home.
    let mut staged: Vec<(String, PathBuf, Vec<u8>)> = Vec::with_capacity(manifest.artifacts.len());
    for entry in &manifest.artifacts {
        let rel_path = validate_relative_artifact_path(&entry.relative_path)?;
        let src_path = bundle_dir.join(&rel_path);
        let bytes = std::fs::read(&src_path).map_err(|e| {
            EaiError::io(format!(
                "read bundle artifact '{}' at {}: {e}",
                entry.name,
                src_path.display()
            ))
        })?;
        let actual_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual_size != entry.size_bytes {
            return Err(EaiError::governance(format!(
                "offline bundle artifact '{}' size mismatch: expected {} bytes, got {} bytes",
                entry.name, entry.size_bytes, actual_size
            )));
        }
        let actual_sha = sha256_hex_bytes(&bytes);
        if !actual_sha.eq_ignore_ascii_case(entry.sha256_hex.trim()) {
            return Err(EaiError::governance(format!(
                "offline bundle artifact '{}' sha256 mismatch: expected {}, got {}",
                entry.name, entry.sha256_hex, actual_sha
            )));
        }
        staged.push((entry.name.clone(), rel_path, bytes));
    }

    // Phase 2: All artifacts verified; install atomically into target_home.
    std::fs::create_dir_all(target_home).map_err(|e| {
        EaiError::io(format!(
            "create offline target home {}: {e}",
            target_home.display()
        ))
    })?;
    let mut imported_artifacts = Vec::with_capacity(staged.len());
    for (name, rel_path, bytes) in staged {
        let dest = target_home.join(&rel_path);
        crate::atomic_replace_file(&dest, &bytes)?;
        imported_artifacts.push(name);
    }
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| EaiError::config(format!("encode offline manifest: {e}")))?;
    crate::atomic_replace_file(&target_home.join("offline_manifest.json"), &manifest_bytes)?;

    Ok(OfflineImportReport {
        bundle_id: manifest.bundle_id,
        imported_artifacts,
        target_home: target_home.to_path_buf(),
    })
}

/// Run a fully airgapped bootstrap and local mission readiness drill against
/// `target_home`.
///
/// Never performs network I/O or cloud fallback. If any required artifact
/// (`Binary`, `Model`, or `ExtensionMetadata`) is missing or fails checksum
/// verification, reports actionable local diagnostics in `missing_or_corrupt`.
#[must_use]
pub fn run_offline_bootstrap_drill(target_home: &Path, goal: &str) -> OfflineReadinessReport {
    let manifest_path = target_home.join("offline_manifest.json");
    let raw_manifest = match std::fs::read_to_string(&manifest_path) {
        Ok(raw) => raw,
        Err(e) => {
            return OfflineReadinessReport {
                ready_for_local_mission: false,
                network_disabled: true,
                cloud_fallback_attempted: false,
                verified_artifacts: Vec::new(),
                missing_or_corrupt: vec![MissingArtifactDiagnosis {
                    name: "offline_manifest.json".to_string(),
                    kind: ArtifactKind::ExtensionMetadata,
                    expected_path: "offline_manifest.json".to_string(),
                    reason: format!("offline manifest missing or unreadable: {e}"),
                }],
                local_mission_output: None,
            };
        }
    };
    let manifest: OfflineBundleManifest = match serde_json::from_str(&raw_manifest) {
        Ok(m) => m,
        Err(e) => {
            return OfflineReadinessReport {
                ready_for_local_mission: false,
                network_disabled: true,
                cloud_fallback_attempted: false,
                verified_artifacts: Vec::new(),
                missing_or_corrupt: vec![MissingArtifactDiagnosis {
                    name: "offline_manifest.json".to_string(),
                    kind: ArtifactKind::ExtensionMetadata,
                    expected_path: "offline_manifest.json".to_string(),
                    reason: format!("offline manifest corrupt JSON: {e}"),
                }],
                local_mission_output: None,
            };
        }
    };

    let mut verified_artifacts = Vec::new();
    let mut missing_or_corrupt = Vec::new();
    let mut has_binary = false;
    let mut has_model = false;
    let mut has_extension_metadata = false;

    for entry in &manifest.artifacts {
        let Ok(rel_path) = validate_relative_artifact_path(&entry.relative_path) else {
            missing_or_corrupt.push(MissingArtifactDiagnosis {
                name: entry.name.clone(),
                kind: entry.kind,
                expected_path: entry.relative_path.clone(),
                reason: "invalid relative artifact path".to_string(),
            });
            continue;
        };
        let full_path = target_home.join(&rel_path);
        let Ok(bytes) = std::fs::read(&full_path) else {
            missing_or_corrupt.push(MissingArtifactDiagnosis {
                name: entry.name.clone(),
                kind: entry.kind,
                expected_path: entry.relative_path.clone(),
                reason: "artifact missing from offline home".to_string(),
            });
            continue;
        };
        let actual_size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if actual_size != entry.size_bytes {
            missing_or_corrupt.push(MissingArtifactDiagnosis {
                name: entry.name.clone(),
                kind: entry.kind,
                expected_path: entry.relative_path.clone(),
                reason: format!(
                    "size mismatch: expected {} bytes, found {} bytes",
                    entry.size_bytes, actual_size
                ),
            });
            continue;
        }
        let actual_sha = sha256_hex_bytes(&bytes);
        if !actual_sha.eq_ignore_ascii_case(entry.sha256_hex.trim()) {
            missing_or_corrupt.push(MissingArtifactDiagnosis {
                name: entry.name.clone(),
                kind: entry.kind,
                expected_path: entry.relative_path.clone(),
                reason: format!(
                    "sha256 mismatch: expected {}, found {}",
                    entry.sha256_hex, actual_sha
                ),
            });
            continue;
        }
        match entry.kind {
            ArtifactKind::Binary => has_binary = true,
            ArtifactKind::Model => has_model = true,
            ArtifactKind::ExtensionMetadata => has_extension_metadata = true,
        }
        verified_artifacts.push(entry.name.clone());
    }

    for (present, kind, label) in [
        (has_binary, ArtifactKind::Binary, "required-binary"),
        (has_model, ArtifactKind::Model, "required-model"),
        (
            has_extension_metadata,
            ArtifactKind::ExtensionMetadata,
            "required-extension-metadata",
        ),
    ] {
        if !present && !missing_or_corrupt.iter().any(|diag| diag.kind == kind) {
            missing_or_corrupt.push(MissingArtifactDiagnosis {
                name: label.to_string(),
                kind,
                expected_path: label.to_string(),
                reason: format!("no verified {kind:?} artifact present in offline home"),
            });
        }
    }

    let ready_for_local_mission = missing_or_corrupt.is_empty();
    let local_mission_output =
        ready_for_local_mission.then(|| format!("offline-local-mission-ok:{goal}"));

    OfflineReadinessReport {
        ready_for_local_mission,
        network_disabled: true,
        cloud_fallback_attempted: false,
        verified_artifacts,
        missing_or_corrupt,
        local_mission_output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sample_bundle(bundle_dir: &Path) -> OfflineBundleManifest {
        let bin_bytes = b"susi-local-runner-v1";
        let model_bytes = b"gguf-quantized-weights-v1";
        let ext_bytes = br#"{"extensions":["filesystem","local-shell"]}"#;

        std::fs::create_dir_all(bundle_dir.join("bin")).unwrap();
        std::fs::create_dir_all(bundle_dir.join("models")).unwrap();
        std::fs::create_dir_all(bundle_dir.join("extensions")).unwrap();

        std::fs::write(bundle_dir.join("bin/susi-runner"), bin_bytes).unwrap();
        std::fs::write(bundle_dir.join("models/local.gguf"), model_bytes).unwrap();
        std::fs::write(bundle_dir.join("extensions/catalog.json"), ext_bytes).unwrap();

        let manifest = OfflineBundleManifest {
            bundle_id: "airgap-bundle-2026-11".to_string(),
            version: "1.0.0".to_string(),
            artifacts: vec![
                BundleArtifactEntry {
                    name: "susi-runner".to_string(),
                    kind: ArtifactKind::Binary,
                    relative_path: "bin/susi-runner".to_string(),
                    sha256_hex: sha256_hex_bytes(bin_bytes),
                    size_bytes: bin_bytes.len() as u64,
                },
                BundleArtifactEntry {
                    name: "local-gguf".to_string(),
                    kind: ArtifactKind::Model,
                    relative_path: "models/local.gguf".to_string(),
                    sha256_hex: sha256_hex_bytes(model_bytes),
                    size_bytes: model_bytes.len() as u64,
                },
                BundleArtifactEntry {
                    name: "extension-catalog".to_string(),
                    kind: ArtifactKind::ExtensionMetadata,
                    relative_path: "extensions/catalog.json".to_string(),
                    sha256_hex: sha256_hex_bytes(ext_bytes),
                    size_bytes: ext_bytes.len() as u64,
                },
            ],
        };
        std::fs::write(
            bundle_dir.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        manifest
    }

    #[test]
    fn import_and_bootstrap_drill_verify_all_artifact_kinds_offline() {
        let root =
            std::env::temp_dir().join(format!("susi-offline-bundle-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bundle_dir = root.join("bundle");
        let target_home = root.join("home");
        write_sample_bundle(&bundle_dir);

        let report = import_verified_bundle(&bundle_dir, &target_home).unwrap();
        assert_eq!(report.bundle_id, "airgap-bundle-2026-11");
        assert_eq!(report.imported_artifacts.len(), 3);

        let readiness = run_offline_bootstrap_drill(&target_home, "summarize-incident");
        assert!(readiness.ready_for_local_mission);
        assert!(readiness.network_disabled);
        assert!(!readiness.cloud_fallback_attempted);
        assert!(readiness.missing_or_corrupt.is_empty());
        assert_eq!(
            readiness.local_mission_output.as_deref(),
            Some("offline-local-mission-ok:summarize-incident")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_validation_rejects_traversal_and_backslashes() {
        assert!(validate_relative_artifact_path("").is_err());
        assert!(validate_relative_artifact_path("../secret").is_err());
        assert!(validate_relative_artifact_path("models/../../etc/passwd").is_err());
        assert!(validate_relative_artifact_path("/abs/path").is_err());
        assert!(validate_relative_artifact_path("bin\\runner.exe").is_err());
        assert!(validate_relative_artifact_path("C:/models/m.bin").is_err());
        assert!(validate_relative_artifact_path("models/local.gguf").is_ok());
    }
}

// ---- Wave 14 (T-CLAUDE-416) structural invariant verifiers ----

/// A 64-character lowercase hex SHA-256 digest.
fn is_lower_sha256_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl BundleArtifactEntry {
    /// A manifest entry names an artifact, carries a confined relative path (no
    /// traversal, drive prefix or backslash), a 64-character lowercase hex SHA-256,
    /// and a non-zero size.
    pub fn verify_entry_invariants(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("BundleArtifactEntry name must not be empty".to_string());
        }
        validate_relative_artifact_path(&self.relative_path).map_err(|e| e.to_string())?;
        if !is_lower_sha256_hex(&self.sha256_hex) {
            return Err(format!(
                "BundleArtifactEntry `{}` sha256_hex must be 64 lowercase hex characters",
                self.name
            ));
        }
        if self.size_bytes == 0 {
            return Err(format!(
                "BundleArtifactEntry `{}` size_bytes must be non-zero",
                self.name
            ));
        }
        Ok(())
    }
}

impl OfflineBundleManifest {
    /// A manifest is identified, carries at least one artifact, every entry verifies,
    /// and no artifact name or relative path repeats.
    pub fn verify_manifest_invariants(&self) -> Result<(), String> {
        if self.bundle_id.trim().is_empty() || self.version.trim().is_empty() {
            return Err(
                "OfflineBundleManifest bundle_id and version must not be empty".to_string(),
            );
        }
        if self.artifacts.is_empty() {
            return Err("OfflineBundleManifest must carry at least one artifact".to_string());
        }
        let mut names = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        for entry in &self.artifacts {
            entry.verify_entry_invariants()?;
            if !names.insert(entry.name.as_str()) {
                return Err(format!("duplicate artifact name `{}`", entry.name));
            }
            if !paths.insert(entry.relative_path.as_str()) {
                return Err(format!("duplicate artifact path `{}`", entry.relative_path));
            }
        }
        Ok(())
    }
}

impl MissingArtifactDiagnosis {
    /// A diagnosis names the artifact, where it was expected, and why it failed.
    pub fn verify_diagnosis_invariants(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("MissingArtifactDiagnosis name must not be empty".to_string());
        }
        if self.expected_path.trim().is_empty() {
            return Err("MissingArtifactDiagnosis expected_path must not be empty".to_string());
        }
        if self.reason.trim().is_empty() {
            return Err("MissingArtifactDiagnosis reason must not be empty".to_string());
        }
        Ok(())
    }
}

impl OfflineReadinessReport {
    /// The drill's verdict is internally consistent: a report that declares readiness
    /// must have the network disabled, nothing missing or corrupt, and a captured local
    /// mission output; any missing/corrupt artifact means it is not ready; and every
    /// diagnosis verifies.
    pub fn verify_report_invariants(&self) -> Result<(), String> {
        if self.ready_for_local_mission {
            if !self.network_disabled {
                return Err("a ready offline report must have the network disabled".to_string());
            }
            if !self.missing_or_corrupt.is_empty() {
                return Err(
                    "a ready offline report must have nothing missing or corrupt".to_string(),
                );
            }
            if self.local_mission_output.is_none() {
                return Err(
                    "a ready offline report must capture its local mission output".to_string(),
                );
            }
        } else if self.missing_or_corrupt.is_empty()
            && self.network_disabled
            && self.local_mission_output.is_some()
        {
            return Err(
                "an offline report with the network disabled, nothing missing and an output must be ready"
                    .to_string(),
            );
        }
        for diag in &self.missing_or_corrupt {
            diag.verify_diagnosis_invariants()?;
        }
        if self.verified_artifacts.iter().any(|a| a.trim().is_empty()) {
            return Err("verified_artifacts must not contain a blank name".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod wave14_verifier_tests {
    use super::*;

    fn entry(name: &str, path: &str) -> BundleArtifactEntry {
        BundleArtifactEntry {
            name: name.to_string(),
            kind: ArtifactKind::Binary,
            relative_path: path.to_string(),
            sha256_hex: "a".repeat(64),
            size_bytes: 128,
        }
    }

    #[test]
    fn bundle_entry_and_manifest_verifiers_reject_corrupt_state() {
        assert_eq!(entry("susi", "bin/susi").verify_entry_invariants(), Ok(()));
        let mut traversal = entry("susi", "../bin/susi");
        assert!(traversal.verify_entry_invariants().is_err());
        traversal = entry("susi", "bin/susi");
        traversal.sha256_hex = "A".repeat(64);
        assert!(traversal.verify_entry_invariants().is_err());
        let mut zero = entry("susi", "bin/susi");
        zero.size_bytes = 0;
        assert!(zero.verify_entry_invariants().is_err());

        let manifest = OfflineBundleManifest {
            bundle_id: "b1".to_string(),
            version: "1.0.0".to_string(),
            artifacts: vec![entry("susi", "bin/susi"), entry("model", "models/m.gguf")],
        };
        assert_eq!(manifest.verify_manifest_invariants(), Ok(()));
        let dup = OfflineBundleManifest {
            bundle_id: "b1".to_string(),
            version: "1.0.0".to_string(),
            artifacts: vec![entry("susi", "bin/susi"), entry("susi", "bin/other")],
        };
        assert!(dup.verify_manifest_invariants().is_err());
        let empty = OfflineBundleManifest {
            bundle_id: "b1".to_string(),
            version: "1.0.0".to_string(),
            artifacts: vec![],
        };
        assert!(empty.verify_manifest_invariants().is_err());
    }

    #[test]
    fn readiness_report_verifier_enforces_consistent_verdict() {
        let diag = MissingArtifactDiagnosis {
            name: "model".to_string(),
            kind: ArtifactKind::Model,
            expected_path: "models/m.gguf".to_string(),
            reason: "sha256 mismatch".to_string(),
        };
        assert_eq!(diag.verify_diagnosis_invariants(), Ok(()));

        let ready = OfflineReadinessReport {
            ready_for_local_mission: true,
            network_disabled: true,
            cloud_fallback_attempted: false,
            verified_artifacts: vec!["susi".to_string()],
            missing_or_corrupt: vec![],
            local_mission_output: Some("42".to_string()),
        };
        assert_eq!(ready.verify_report_invariants(), Ok(()));

        let ready_but_online = OfflineReadinessReport {
            network_disabled: false,
            ..ready.clone()
        };
        assert!(ready_but_online.verify_report_invariants().is_err());

        let ready_but_missing = OfflineReadinessReport {
            missing_or_corrupt: vec![diag.clone()],
            ..ready.clone()
        };
        assert!(ready_but_missing.verify_report_invariants().is_err());

        let not_ready = OfflineReadinessReport {
            ready_for_local_mission: false,
            network_disabled: true,
            cloud_fallback_attempted: false,
            verified_artifacts: vec![],
            missing_or_corrupt: vec![diag],
            local_mission_output: None,
        };
        assert_eq!(not_ready.verify_report_invariants(), Ok(()));
    }
}
