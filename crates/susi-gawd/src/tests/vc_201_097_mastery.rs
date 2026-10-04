use crate::unsafe_ratchet::{
    ratchet_unsafe, sbom_changes, GeigerVerdict, SbomEntry, UnsafeExposure,
};

#[test]
fn vc_201_097_mastery_records_transitive_changes_and_gates_new_exposure() {
    let baseline = vec![UnsafeExposure {
        crate_name: "serde".into(),
        unsafe_fns: 1,
        first_party_exception: false,
    }];
    let current = vec![
        baseline[0].clone(),
        UnsafeExposure {
            crate_name: "new-native-dependency".into(),
            unsafe_fns: 1,
            first_party_exception: false,
        },
    ];
    let old_sbom = vec![SbomEntry {
        crate_name: "serde".into(),
        version: "1.0.0".into(),
        provenance: "crates.io".into(),
    }];
    let new_sbom = vec![
        SbomEntry {
            crate_name: "serde".into(),
            version: "1.0.1".into(),
            provenance: "crates.io".into(),
        },
        SbomEntry {
            crate_name: "new-native-dependency".into(),
            version: "0.1.0".into(),
            provenance: "crates.io".into(),
        },
    ];

    let changes = sbom_changes(&old_sbom, &new_sbom);
    assert_eq!(changes.len(), 2);
    assert!(changes.iter().any(|change| change.crate_name == "serde"));
    assert_eq!(
        ratchet_unsafe(&baseline, &current, &old_sbom, &new_sbom),
        GeigerVerdict::NeedsReview {
            new_crates: vec!["new-native-dependency".into()]
        }
    );
}
