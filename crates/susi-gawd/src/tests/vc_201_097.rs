use susi_unsafe_ratchet::{ratchet_unsafe, GeigerVerdict, SbomEntry, UnsafeExposure};

#[test]
fn vc_201_097_flags_new_unsafe_for_review() {
    let baseline = vec![UnsafeExposure {
        crate_name: "foo".into(),
        unsafe_fns: 1,
        first_party_exception: false,
    }];
    let current = vec![
        UnsafeExposure {
            crate_name: "foo".into(),
            unsafe_fns: 1,
            first_party_exception: false,
        },
        UnsafeExposure {
            crate_name: "bar".into(),
            unsafe_fns: 2,
            first_party_exception: false,
        },
    ];
    let sbom = vec![SbomEntry {
        crate_name: "bar".into(),
        version: "1.0.0".into(),
        provenance: "crates.io".into(),
    }];
    match ratchet_unsafe(&baseline, &current, &[], &sbom) {
        GeigerVerdict::NeedsReview { new_crates } => {
            assert!(new_crates.contains(&"bar".into()));
        }
        other => panic!("expected review, got {other:?}"),
    }
}

#[test]
fn vc_201_097_allows_first_party_exceptions() {
    let baseline = vec![];
    let current = vec![UnsafeExposure {
        crate_name: "susi-tools".into(),
        unsafe_fns: 3,
        first_party_exception: true,
    }];
    assert_eq!(
        ratchet_unsafe(&baseline, &current, &[], &[]),
        GeigerVerdict::Clean
    );
}

#[test]
fn vc_201_097_flags_increased_unsafe_fns_on_baseline_crate() {
    let baseline = vec![UnsafeExposure {
        crate_name: "core-dep".into(),
        unsafe_fns: 2,
        first_party_exception: false,
    }];
    let current = vec![UnsafeExposure {
        crate_name: "core-dep".into(),
        unsafe_fns: 3,
        first_party_exception: false,
    }];
    assert_eq!(
        ratchet_unsafe(&baseline, &current, &[], &[]),
        GeigerVerdict::NeedsReview {
            new_crates: vec!["core-dep".into()],
        }
    );
}

#[test]
fn vc_201_097_unchanged_or_reduced_exposure_with_sbom_upgrade_passes_cleanly() {
    let baseline = vec![UnsafeExposure {
        crate_name: "pinned-crate".into(),
        unsafe_fns: 4,
        first_party_exception: false,
    }];
    let current = vec![UnsafeExposure {
        crate_name: "pinned-crate".into(),
        unsafe_fns: 2,
        first_party_exception: false,
    }];
    let base_sbom = vec![SbomEntry {
        crate_name: "pinned-crate".into(),
        version: "1.0.0".into(),
        provenance: "crates.io".into(),
    }];
    let cur_sbom = vec![SbomEntry {
        crate_name: "pinned-crate".into(),
        version: "1.1.0".into(),
        provenance: "crates.io".into(),
    }];
    assert_eq!(
        ratchet_unsafe(&baseline, &current, &base_sbom, &cur_sbom),
        GeigerVerdict::Clean
    );
}

#[test]
fn vc_201_097_removed_baseline_exposure_passes_cleanly() {
    let baseline = vec![UnsafeExposure {
        crate_name: "retired-unsafe-dep".into(),
        unsafe_fns: 5,
        first_party_exception: false,
    }];
    let current = vec![];
    assert_eq!(
        ratchet_unsafe(&baseline, &current, &[], &[]),
        GeigerVerdict::Clean
    );
}
