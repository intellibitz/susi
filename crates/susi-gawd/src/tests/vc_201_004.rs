use crate::eval_receipt::EvalReceipt;

#[test]
fn vc_201_004_receipt_identifies_revision_digest_suite_env_exit() {
    let r = EvalReceipt::new("rev1", b"binary", "suite-a", "linux-ci", 0);
    assert_eq!(r.source_revision, "rev1");
    assert_eq!(r.suite_revision, "suite-a");
    assert_eq!(r.environment, "linux-ci");
    assert_eq!(r.exit_status, 0);
    assert_eq!(r.artifact_digest, EvalReceipt::digest_bytes(b"binary"));
}

#[test]
fn vc_201_004_changed_artifact_invalidates_comparison() {
    let r = EvalReceipt::new("rev1", b"binary", "suite-a", "env", 0);
    assert!(!r.validates_against(b"other", Some(&r)));
}

#[test]
fn vc_201_004_missing_receipt_invalidates() {
    let r = EvalReceipt::new("rev1", b"binary", "suite-a", "env", 0);
    assert!(!r.validates_against(b"binary", None));
    assert!(r.validates_against(b"binary", Some(&r)));
}
