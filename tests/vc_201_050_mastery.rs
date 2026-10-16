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

use std::collections::BTreeSet;
use susi_core::mac_policy::{MacPolicy, PrivacyMode};
use susi_vendor_models::offline_bundle::{import_bundle, pack_bundle};

fn available_set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn vc_201_050_mastery_airgap_blocks_egress_and_permits_loopback() {
    let policy = MacPolicy::new([7; 32], PrivacyMode::LocalOnly, true);

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
}

#[test]
fn vc_201_050_mastery_checksummed_bundle_import_and_verification() {
    // 1. Pack deterministic checksummed bundle
    let bundle = pack_bundle(
        vec!["susi".into(), "susi-daemon".into()],
        vec!["qwen2.5-coder:7b".into(), "llama3.2:1b".into()],
        vec!["extensions.json".into(), "registry.sig".into()],
    );

    // Permuted inputs yield identical deterministic checksum
    let reordered = pack_bundle(
        vec!["susi-daemon".into(), "susi".into()],
        vec!["llama3.2:1b".into(), "qwen2.5-coder:7b".into()],
        vec!["registry.sig".into(), "extensions.json".into()],
    );
    assert_eq!(
        bundle.checksum, reordered.checksum,
        "Bundle checksum must be deterministic regardless of input ordering"
    );
    assert!(!bundle.checksum.is_empty());

    // 2. Complete isolated host imports cleanly
    let available = available_set(&[
        "susi",
        "susi-daemon",
        "qwen2.5-coder:7b",
        "llama3.2:1b",
        "extensions.json",
        "registry.sig",
    ]);
    let report = import_bundle(&bundle, &available);
    assert!(report.checksum_ok, "Valid bundle passes checksum");
    assert!(
        report.is_complete(),
        "Complete host has no missing artifacts"
    );

    // 3. Tampered bundle is detected and rejected by checksum
    let mut tampered = bundle.clone();
    tampered.binaries.push("malicious-binary".into());
    let tampered_report = import_bundle(&tampered, &available);
    assert!(
        !tampered_report.checksum_ok,
        "Tampered content fails verification"
    );
}

#[test]
fn vc_201_050_mastery_diagnose_missing_artifacts_without_cloud_fallback() {
    let bundle = pack_bundle(
        vec!["susi".into(), "susi-daemon".into()],
        vec!["llama3.2:1b".into()],
        vec!["extensions.json".into()],
    );

    // Host has only partial artifacts locally
    let partial_available = available_set(&["susi", "extensions.json"]);
    let report = import_bundle(&bundle, &partial_available);

    assert!(report.checksum_ok);
    assert!(!report.is_complete());
    assert_eq!(
        report.missing_binaries,
        vec!["susi-daemon".to_string()],
        "Missing daemon diagnosed cleanly"
    );
    assert_eq!(
        report.missing_models,
        vec!["llama3.2:1b".to_string()],
        "Missing model diagnosed cleanly"
    );
    assert!(
        report.missing_extension_metadata.is_empty(),
        "Present extension correctly identified"
    );

    // Under airgap / LocalOnly, cloud resolution must be blocked rather than falling back
    let policy = MacPolicy::new([7; 32], PrivacyMode::LocalOnly, true);
    assert!(
        !policy.egress_permitted("https://huggingface.co/models"),
        "No cloud fallback allowed under network-disabled drill"
    );
}
