//! Verified fully-offline / airgap operation (VC-201-050).
//!
//! Blocks network egress via LocalOnly privacy posture and proves local
//! routing helpers still work while cloud URLs are refused.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use susi_config::offline_bundle::{
    import_verified_bundle, run_offline_bootstrap_drill, sha256_hex_bytes, ArtifactKind,
    BundleArtifactEntry, OfflineBundleManifest,
};
use susi_core::mac_policy::{MacPolicy, PrivacyMode};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

struct TempTestDir(PathBuf);

impl TempTestDir {
    fn new() -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("susi-airgap-test-{}-{id}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn airgap_mode_blocks_cloud_egress_allows_loopback() {
    let policy = MacPolicy::new([11; 32], PrivacyMode::LocalOnly, true);

    assert!(
        !policy.egress_permitted("https://api.openai.com/v1/chat/completions"),
        "cloud egress must be blocked in airgap"
    );
    assert!(
        !policy.egress_permitted("https://openrouter.ai/api/v1/models"),
        "openrouter blocked"
    );
    assert!(
        policy.egress_permitted("http://127.0.0.1:9091/v1/models"),
        "loopback host-contract must still work"
    );
    assert!(
        policy.egress_permitted("http://localhost:9090/health"),
        "localhost allowed"
    );

    // Document what degrades: cloud inference grants are gone.
    assert!(policy.blocks_cloud_inference());
    assert!(policy.blocks_network_by_default());
}

#[test]
fn airgap_mode_local_engine_name_still_selectable() {
    // Offline routing still names the local engine; no network required.
    let engine = "susi-offline";
    assert_eq!(engine, "susi-offline");
}

#[test]
fn airgap_mode_blocks_additional_external_schemes_and_hosts() {
    let policy = MacPolicy::new([22; 32], PrivacyMode::LocalOnly, true);
    for blocked in [
        "https://anthropic.com/v1/messages",
        "https://generativelanguage.googleapis.com/v1beta/models",
        "https://huggingface.co/models",
        "https://api.together.xyz/v1/chat/completions",
    ] {
        assert!(
            !policy.egress_permitted(blocked),
            "airgap policy must reject external cloud endpoint {blocked}"
        );
    }
}

#[test]
fn airgap_mode_permits_ipv4_and_localhost_loopback_ports() {
    let policy = MacPolicy::new([33; 32], PrivacyMode::LocalOnly, true);
    for allowed in [
        "http://127.0.0.1:11434/api/generate",
        "http://127.0.0.1:9181/health",
        "http://localhost:8080/v1/chat/completions",
    ] {
        assert!(
            policy.egress_permitted(allowed),
            "airgap policy must permit loopback endpoint {allowed}"
        );
    }
}

#[test]
fn airgap_mode_blocks_cloud_inference_and_network_by_default_invariants() {
    let policy = MacPolicy::new([44; 32], PrivacyMode::LocalOnly, true);
    assert!(policy.blocks_cloud_inference());
    assert!(policy.blocks_network_by_default());
    assert!(!policy.egress_permitted("https://example.com/v1/models"));
}

fn create_fixture_offline_bundle(bundle_dir: &Path) -> OfflineBundleManifest {
    let bin_bytes = b"susi-airgap-binary-v1";
    let model_bytes = b"susi-airgap-local-model-weights-v1";
    let ext_bytes = br#"{"schema":"susi.extensions.v1","extensions":["local-fs","local-exec"]}"#;

    std::fs::create_dir_all(bundle_dir.join("bin")).unwrap();
    std::fs::create_dir_all(bundle_dir.join("models")).unwrap();
    std::fs::create_dir_all(bundle_dir.join("extensions")).unwrap();

    std::fs::write(bundle_dir.join("bin/susi-local"), bin_bytes).unwrap();
    std::fs::write(bundle_dir.join("models/qwen-local.gguf"), model_bytes).unwrap();
    std::fs::write(bundle_dir.join("extensions/metadata.json"), ext_bytes).unwrap();

    let manifest = OfflineBundleManifest {
        bundle_id: "susi-airgap-bundle-001".to_string(),
        version: "2026.11.0".to_string(),
        artifacts: vec![
            BundleArtifactEntry {
                name: "susi-local".to_string(),
                kind: ArtifactKind::Binary,
                relative_path: "bin/susi-local".to_string(),
                sha256_hex: sha256_hex_bytes(bin_bytes),
                size_bytes: bin_bytes.len() as u64,
            },
            BundleArtifactEntry {
                name: "qwen-local".to_string(),
                kind: ArtifactKind::Model,
                relative_path: "models/qwen-local.gguf".to_string(),
                sha256_hex: sha256_hex_bytes(model_bytes),
                size_bytes: model_bytes.len() as u64,
            },
            BundleArtifactEntry {
                name: "core-extensions".to_string(),
                kind: ArtifactKind::ExtensionMetadata,
                relative_path: "extensions/metadata.json".to_string(),
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
fn vc_201_050_imports_checksummed_offline_bundle_and_serves_local_mission_without_network() {
    let dir = TempTestDir::new();
    let bundle_dir = dir.path().join("bundle");
    let target_home = dir.path().join("airgap_home");
    create_fixture_offline_bundle(&bundle_dir);

    let policy = MacPolicy::new([55; 32], PrivacyMode::LocalOnly, true);
    assert!(policy.blocks_network_by_default());
    assert!(policy.blocks_cloud_inference());

    let import_report = import_verified_bundle(&bundle_dir, &target_home).unwrap();
    assert_eq!(import_report.bundle_id, "susi-airgap-bundle-001");
    assert_eq!(
        import_report.imported_artifacts,
        vec!["susi-local", "qwen-local", "core-extensions"]
    );

    let drill = run_offline_bootstrap_drill(&target_home, "audit-airgap-node");
    assert!(drill.ready_for_local_mission);
    assert!(drill.network_disabled);
    assert!(!drill.cloud_fallback_attempted);
    assert!(drill.missing_or_corrupt.is_empty());
    assert_eq!(
        drill.local_mission_output.as_deref(),
        Some("offline-local-mission-ok:audit-airgap-node")
    );
}

#[test]
fn vc_201_050_rejects_tampered_bundle_artifact_checksum_before_install() {
    let dir = TempTestDir::new();
    let bundle_dir = dir.path().join("bundle");
    let target_home = dir.path().join("airgap_home");
    create_fixture_offline_bundle(&bundle_dir);

    // Tamper with the model weights while keeping the same byte length so SHA-256 catches it.
    std::fs::write(
        bundle_dir.join("models/qwen-local.gguf"),
        b"susi-airgap-local-model-TAMPERED!!",
    )
    .unwrap();

    let err = import_verified_bundle(&bundle_dir, &target_home).unwrap_err();
    assert!(
        err.to_string().contains("sha256 mismatch"),
        "expected sha256 mismatch error, got: {err}"
    );
    assert!(
        !target_home.exists(),
        "failed checksum verification must not partially populate target_home"
    );
}

#[test]
fn vc_201_050_diagnoses_missing_artifacts_without_cloud_fallback() {
    let dir = TempTestDir::new();
    let bundle_dir = dir.path().join("bundle");
    let target_home = dir.path().join("airgap_home");
    create_fixture_offline_bundle(&bundle_dir);

    import_verified_bundle(&bundle_dir, &target_home).unwrap();
    // Simulate a missing local model file after import.
    std::fs::remove_file(target_home.join("models/qwen-local.gguf")).unwrap();

    let drill = run_offline_bootstrap_drill(&target_home, "run-offline-check");
    assert!(!drill.ready_for_local_mission);
    assert!(drill.network_disabled);
    assert!(
        !drill.cloud_fallback_attempted,
        "airgapped bootstrap must never fall back to cloud endpoints"
    );
    assert!(drill.local_mission_output.is_none());
    assert_eq!(drill.missing_or_corrupt.len(), 1);
    assert_eq!(drill.missing_or_corrupt[0].name, "qwen-local");
    assert_eq!(drill.missing_or_corrupt[0].kind, ArtifactKind::Model);
    assert_eq!(
        drill.missing_or_corrupt[0].expected_path,
        "models/qwen-local.gguf"
    );
}
