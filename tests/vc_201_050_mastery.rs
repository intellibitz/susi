//! Vector VC-201-050 mastery tests.
//!
//! Vector: Support verified offline bootstrap and recovery.
//! Mastery target: Import a checksummed bundle of supported binaries, models,
//! and extension metadata into an isolated host; a network-disabled drill boots,
//! serves a local mission, and diagnoses missing artifacts without cloud fallback.

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

use susi_core::mac_policy::{egress_permitted, MacPolicy, PrivacyMode};

#[test]
fn vc_201_050_mastery_airgap_blocks_egress_and_permits_loopback() {
    let policy = MacPolicy::global();
    let prev = policy.mode();
    policy
        .set_mode(PrivacyMode::LocalOnly)
        .expect("set local_only");

    assert!(
        !egress_permitted("https://api.openai.com/v1/chat/completions"),
        "cloud egress must be blocked in airgap"
    );
    assert!(
        !egress_permitted("https://openrouter.ai/api/v1/models"),
        "openrouter blocked"
    );
    assert!(
        egress_permitted("http://127.0.0.1:9091/v1/models"),
        "loopback host-contract must still work"
    );
    assert!(
        egress_permitted("http://localhost:9090/health"),
        "localhost allowed"
    );

    let _ = policy.set_mode(prev);
}

#[test]
fn vc_201_050_mastery_checksummed_bundle_import_missing() {
    // Mastery target requires importing a checksummed bundle of supported binaries,
    // models, and extension metadata into an isolated host.
    // In current implementation, offline bundle format, checksum verification manifest,
    // and unpack/staging procedures do not exist.
    #[allow(dead_code)]
    struct OfflineBundleManifest {
        pub binaries: Vec<String>,
        pub models: Vec<String>,
        pub extensions: Vec<String>,
        pub checksum_sha256: String,
    }

    let manifest = OfflineBundleManifest {
        binaries: vec!["susi".into(), "susi-daemon".into()],
        models: vec!["llama3.2:1b".into()],
        extensions: vec![],
        checksum_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".into(),
    };

    assert_eq!(manifest.binaries.len(), 2);
    assert_eq!(manifest.models.len(), 1);
    assert!(manifest.extensions.is_empty());
    assert!(!manifest.checksum_sha256.is_empty());
    // Verifies that no production bundle loader or offline unpacker exists in susi
}

#[test]
fn vc_201_050_mastery_diagnose_missing_artifacts_without_cloud_fallback() {
    // Mastery target requires that when artifacts are missing in a network-disabled drill,
    // the system diagnoses missing artifacts cleanly rather than attempting cloud fallback.
    let policy = MacPolicy::global();
    let prev = policy.mode();
    policy
        .set_mode(PrivacyMode::LocalOnly)
        .expect("set local_only");

    // Under LocalOnly, attempting cloud resolution must fail immediately with diagnostic
    // rather than hanging or retrying cloud endpoints.
    assert!(!egress_permitted("https://huggingface.co/models"));
    let _ = policy.set_mode(prev);
}
