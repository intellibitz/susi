use crate::unsafe_ratchet::{ratchet_unsafe, GeigerVerdict, SbomEntry, UnsafeExposure};

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
