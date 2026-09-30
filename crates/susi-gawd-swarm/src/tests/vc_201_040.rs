use crate::fault_matrix::{run_matrix, save_report, FaultKind};

#[test]
fn vc_201_040_matrix_covers_the_full_fault_catalogue() {
    let report = run_matrix();
    let kinds: Vec<FaultKind> = report.scenarios.iter().map(|s| s.kind.clone()).collect();
    for want in [
        FaultKind::Partition,
        FaultKind::ClockSkew,
        FaultKind::DuplicateDelivery,
        FaultKind::StaleKey,
        FaultKind::FailedRekey,
        FaultKind::MixedVersion,
        FaultKind::CrashRecovery,
    ] {
        assert!(kinds.contains(&want), "scenario {want:?} missing");
    }
}

#[test]
fn vc_201_040_every_guard_holds() {
    let report = run_matrix();
    for s in &report.scenarios {
        assert!(
            s.passed,
            "guard failed for {:?}: {:?} (trace: {:?})",
            s.kind, s.expected_guard, s.trace
        );
    }
    assert!(report.all_passed());
}

#[test]
fn vc_201_040_unsupported_guarantees_are_enumerated() {
    let report = run_matrix();
    assert!(!report.unsupported.is_empty());
    assert!(report
        .unsupported
        .iter()
        .any(|u| u.contains("compromised current cluster key")));
    assert!(report
        .unsupported
        .iter()
        .any(|u| u.contains("delayed-queue bound")));
}

#[test]
fn vc_201_040_trace_archive_roundtrips() {
    let report = run_matrix();
    let dir = std::env::temp_dir().join(format!("vc40-{}", std::process::id()));
    let path = dir.join("matrix.json");
    save_report(&path, &report).unwrap();
    let loaded: crate::fault_matrix::MatrixReport =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(loaded.scenarios.len(), report.scenarios.len());
    assert!(loaded.scenarios.iter().all(|s| !s.trace.is_empty()));
    let _ = std::fs::remove_dir_all(&dir);
}
