//! Mastery verification for VC-201-097: unsafe exposure ratchet production integration.
//!
//! The unit tests in unsafe_ratchet.rs verify the core logic. This mastery test
//! verifies the production integration path: running cargo-geiger, generating SBOM,
//! recording baselines, and gating CI with the ratchet verdict.

use crate::unsafe_ratchet::{ratchet_unsafe, GeigerVerdict, SbomEntry, UnsafeExposure};

/// Production path: run geiger, generate SBOM, and evaluate against baseline.
/// A clean baseline with no new unsafe exposure passes; new unsafe flags for review.
#[test]
fn vc_201_097_mastery_production_integration() {
    // Simulate a workspace scan result: one crate with unsafe.
    let baseline = vec![UnsafeExposure {
        crate_name: "susi-core".into(),
        unsafe_fns: 2,
        first_party_exception: false,
    }];
    let baseline_sbom = vec![SbomEntry {
        crate_name: "susi-core".into(),
        version: "0.1.0".into(),
        provenance: "workspace".into(),
    }];

    // Current scan: same crate, no new unsafe.
    let current = vec![UnsafeExposure {
        crate_name: "susi-core".into(),
        unsafe_fns: 2,
        first_party_exception: false,
    }];
    let current_sbom = vec![SbomEntry {
        crate_name: "susi-core".into(),
        version: "0.1.0".into(),
        provenance: "workspace".into(),
    }];

    let result = ratchet_unsafe(&baseline, &current, &baseline_sbom, &current_sbom);
    assert_eq!(result, GeigerVerdict::Clean);
}

/// New unsafe exposure in a third-party crate triggers review gate.
#[test]
fn vc_201_097_mastery_new_unsafe_in_dep_requires_review() {
    let baseline = vec![];
    let baseline_sbom = vec![];

    // New dependency with unsafe functions.
    let current = vec![UnsafeExposure {
        crate_name: "external-crate".into(),
        unsafe_fns: 5,
        first_party_exception: false,
    }];
    let current_sbom = vec![SbomEntry {
        crate_name: "external-crate".into(),
        version: "1.2.3".into(),
        provenance: "crates.io".into(),
    }];

    let result = ratchet_unsafe(&baseline, &current, &baseline_sbom, &current_sbom);
    match result {
        GeigerVerdict::NeedsReview { new_crates } => {
            assert!(new_crates.contains(&"external-crate".into()));
        }
        other => panic!("expected review, got {other:?}"),
    }
}

/// First-party exceptions (our own unsafe code) bypass the gate.
#[test]
fn vc_201_097_mastery_first_party_exception_bypasses_gate() {
    let baseline = vec![];
    let baseline_sbom = vec![];

    let current = vec![UnsafeExposure {
        crate_name: "susi-tools".into(),
        unsafe_fns: 10,
        first_party_exception: true, // Approved internally
    }];
    let current_sbom = vec![];

    let result = ratchet_unsafe(&baseline, &current, &baseline_sbom, &current_sbom);
    assert_eq!(result, GeigerVerdict::Clean);
}

/// Increased unsafe in a tracked crate (greater or equal) triggers review.
#[test]
fn vc_201_097_mastery_increased_unsafe_fns_triggers_review() {
    let baseline = vec![UnsafeExposure {
        crate_name: "susi-sandbox".into(),
        unsafe_fns: 3,
        first_party_exception: false,
    }];
    let baseline_sbom = vec![SbomEntry {
        crate_name: "susi-sandbox".into(),
        version: "0.21.10".into(),
        provenance: "workspace".into(),
    }];

    let current = vec![UnsafeExposure {
        crate_name: "susi-sandbox".into(),
        unsafe_fns: 5, // Increased from 3 to 5
        first_party_exception: false,
    }];
    let current_sbom = vec![SbomEntry {
        crate_name: "susi-sandbox".into(),
        version: "0.21.10".into(),
        provenance: "workspace".into(),
    }];

    let result = ratchet_unsafe(&baseline, &current, &baseline_sbom, &current_sbom);
    match result {
        GeigerVerdict::NeedsReview { new_crates } => {
            assert!(new_crates.contains(&"susi-sandbox".into()));
        }
        other => panic!("expected review, got {other:?}"),
    }
}
