use susi_unsafe_ratchet::{ratchet_unsafe, sbom_changes, GeigerVerdict, SbomEntry, UnsafeExposure};

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
    // The production gate must reach ratchet_unsafe: the release gate invokes
    // native complete measurement and admission.
    let bin = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-unsafe-ratchet/src/bin/unsafe_ratchet.rs"
    ))
    .expect("unsafe_ratchet binary present");
    assert!(
        bin.contains("inventory::receipt("),
        "the gate binary must admit a complete reviewed inventory"
    );
    assert!(
        bin.contains("--write-baseline"),
        "the binary must offer the auditable baseline-regeneration path"
    );

    // GitHub Actions workflows were removed (CEO decision 2026-11-06); the
    // release gate runs the native measurement and admission binaries.
    let gates = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../xtask/src/complete_release_gates.rs"
    ))
    .expect("release gate source present");
    assert!(gates.contains("Tool::read(&trust[\"unsafe_measure\"], root)?"));
    assert!(gates.contains("Tool::read(&trust[\"unsafe_ratchet\"], root)?"));
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
    let baseline: susi_unsafe_ratchet::UnsafeBaseline =
        serde_json::from_str(&text).expect("baseline parses as UnsafeBaseline");
    assert_eq!(baseline.features, "default");
    assert!(
        !baseline.exposure.is_empty() || !baseline.sbom.is_empty(),
        "baseline must record measured exposure and/or the SBOM — an empty \
         baseline is not a recorded measurement"
    );
}

#[test]
fn vc_201_097_mastery_baseline_roundtrips_json_and_verdict_is_clean_against_self() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../.agents/baseline/unsafe-default.json"
    ))
    .expect("pinned baseline recorded");
    let baseline: susi_unsafe_ratchet::UnsafeBaseline =
        serde_json::from_str(&text).expect("baseline parses as UnsafeBaseline");
    let reencoded = serde_json::to_string(&baseline).expect("serialize baseline");
    let decoded: susi_unsafe_ratchet::UnsafeBaseline =
        serde_json::from_str(&reencoded).expect("deserialize baseline");
    assert_eq!(decoded.features, baseline.features);
    assert_eq!(decoded.exposure, baseline.exposure);
    assert_eq!(decoded.sbom, baseline.sbom);
    assert_eq!(
        ratchet_unsafe(
            &baseline.exposure,
            &decoded.exposure,
            &baseline.sbom,
            &decoded.sbom
        ),
        GeigerVerdict::Clean
    );
}

#[test]
fn vc_201_097_mastery_sbom_changes_returns_empty_when_sbom_is_identical() {
    let sbom = vec![
        SbomEntry {
            crate_name: "alpha".into(),
            version: "1.0.0".into(),
            provenance: "crates.io".into(),
        },
        SbomEntry {
            crate_name: "beta".into(),
            version: "2.1.0".into(),
            provenance: "crates.io".into(),
        },
    ];
    assert!(sbom_changes(&sbom, &sbom).is_empty());
}
