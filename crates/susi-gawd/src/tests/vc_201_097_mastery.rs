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

#[test]
fn vc_201_097_mastery_gate_is_on_the_ci_path() {
    // The production gate must reach ratchet_unsafe: CI runs the workflow
    // script, the script invokes the susi-gawd binary, and the binary is
    // the comparator's production call site.
    let bin = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/bin/unsafe_ratchet.rs"
    ))
    .expect("unsafe_ratchet binary present");
    assert!(
        bin.contains("ratchet_unsafe("),
        "the gate binary must call ratchet_unsafe"
    );
    assert!(
        bin.contains("--write-baseline"),
        "the binary must offer the auditable baseline-regeneration path"
    );

    let script = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.github/workflows/unsafe-exposure-ratchet.sh"
    ))
    .expect("ratchet workflow script present");
    assert!(script.contains("cargo geiger --output-format Json"));
    assert!(script.contains("cargo metadata --format-version 1 --locked"));
    assert!(script.contains("--bin unsafe_ratchet"));

    let workflow = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.github/workflows/test.yml"
    ))
    .expect("test workflow present");
    assert!(
        workflow.contains("unsafe-exposure-ratchet.sh"),
        "CI must invoke the ratchet script"
    );
}

#[test]
fn vc_201_097_mastery_pinned_baseline_exists_for_the_default_feature_set() {
    // The gate only means something against a recorded baseline of the
    // pinned feature set. An empty baseline would flag every exposed crate.
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.agents/baseline/unsafe-default.json"
    ))
    .expect("pinned baseline recorded");
    let baseline: crate::unsafe_ratchet::UnsafeBaseline =
        serde_json::from_str(&text).expect("baseline parses as UnsafeBaseline");
    assert_eq!(baseline.features, "default");
    assert!(
        !baseline.exposure.is_empty() || !baseline.sbom.is_empty(),
        "baseline must record measured exposure and/or the SBOM — an empty \
         baseline is not a recorded measurement"
    );
}
